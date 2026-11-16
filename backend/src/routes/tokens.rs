use crate::{
    auth::{self, AuthUser},
    error::ApiError,
    state::AppState,
};
use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::sync::Arc;

const BACKUP_ROOT_NAME: &str = "Phone Backups";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeviceTokenRow {
    pub id: String,
    pub name: String,
    // The hash is the lookup key; user_id scopes listing and revocation.
    #[allow(dead_code)]
    pub token_hash: String,
    #[allow(dead_code)]
    pub user_id: String,
    pub scope: String,
    pub backup_root_id: Option<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl DeviceTokenRow {
    pub fn status(&self) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self
            .expires_at
            .map(|expires| expires <= crate::now())
            .unwrap_or(false)
        {
            "expired"
        } else {
            "active"
        }
    }
}

/// Listing shape — never exposes the raw token (it is not stored anyway).
pub fn token_json(row: &DeviceTokenRow) -> Value {
    json!({
        "id": row.id,
        "name": row.name,
        "scope": row.scope,
        "backup_root_id": row.backup_root_id,
        "created_at": row.created_at,
        "last_used_at": row.last_used_at,
        "expires_at": row.expires_at,
        "revoked_at": row.revoked_at,
        "status": row.status(),
    })
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query_as::<_, DeviceTokenRow>(
        "SELECT * FROM device_tokens WHERE user_id = ? AND revoked_at IS NULL \
         ORDER BY created_at DESC",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        json!({ "items": rows.iter().map(token_json).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
pub struct CreateTokenRequest {
    pub name: String,
    /// `full` (default) or `backup`. Backup tokens are bound to a root child
    /// folder named `Phone Backups`, matching the Android backup path scheme.
    #[serde(default)]
    pub scope: Option<String>,
}

pub fn validate_scope(raw: Option<&str>) -> Result<&'static str, ApiError> {
    match raw.map(str::trim).filter(|scope| !scope.is_empty()) {
        None | Some("full") => Ok("full"),
        Some("backup") => Ok("backup"),
        Some(other) => Err(ApiError::bad_request(format!(
            "unknown token scope {other:?}; use full or backup"
        ))),
    }
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Json<Value>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("token name is required"));
    }
    if name.chars().count() > 100 {
        return Err(ApiError::bad_request(
            "token name is too long (max 100 characters)",
        ));
    }
    let scope = validate_scope(req.scope.as_deref())?;

    let (raw, token_hash) = auth::new_device_token();
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::now();

    // BEGIN IMMEDIATE serializes folder discovery/creation with token insertion
    // so concurrent backup-token creations agree on one root folder.
    let mut conn = state.pool.acquire().await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    let created = async {
        let backup_root_id = if scope == "backup" {
            Some(ensure_backup_root(&mut conn, &user.id).await?)
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO device_tokens \
             (id, name, token_hash, user_id, scope, backup_root_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(&token_hash)
        .bind(&user.id)
        .bind(scope)
        .bind(&backup_root_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        Ok::<Option<String>, ApiError>(backup_root_id)
    }
    .await;

    let backup_root_id = match created {
        Ok(backup_root_id) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            backup_root_id
        }
        Err(err) => {
            let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
            return Err(err);
        }
    };

    audit(&state, &user.id, "tokens.create", &format!("{name} ({id})")).await;

    let row = DeviceTokenRow {
        id,
        name: name.to_string(),
        token_hash,
        user_id: user.id.clone(),
        scope: scope.to_string(),
        backup_root_id,
        created_at: now,
        last_used_at: None,
        expires_at: None,
        revoked_at: None,
    };
    // The raw token is returned exactly once and never persisted.
    Ok(Json(json!({ "token": raw, "item": token_json(&row) })))
}

/// Finds or creates the instance-wide `Phone Backups` root folder. `INSERT OR
/// IGNORE` plus the sibling-name unique index makes this idempotent even if an
/// earlier transaction raced us.
async fn ensure_backup_root(
    conn: &mut SqliteConnection,
    owner_id: &str,
) -> Result<String, ApiError> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nodes WHERE parent_id = 'root' AND kind = 'folder' \
         AND name = ? COLLATE NOCASE LIMIT 1",
    )
    .bind(BACKUP_ROOT_NAME)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::now();
    sqlx::query(
        "INSERT OR IGNORE INTO nodes \
         (id, parent_id, kind, name, owner_id, created_at, updated_at) \
         VALUES (?, 'root', 'folder', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(BACKUP_ROOT_NAME)
    .bind(owner_id)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nodes WHERE parent_id = 'root' AND kind = 'folder' \
         AND name = ? COLLATE NOCASE LIMIT 1",
    )
    .bind(BACKUP_ROOT_NAME)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((id,)) = row {
        return Ok(id);
    }

    let occupied: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nodes WHERE parent_id = 'root' AND name = ? COLLATE NOCASE LIMIT 1",
    )
    .bind(BACKUP_ROOT_NAME)
    .fetch_optional(&mut *conn)
    .await?;
    if occupied.is_some() {
        return Err(ApiError::conflict(format!(
            "a non-folder item named {BACKUP_ROOT_NAME} exists at the drive root; \
             rename it before creating a backup token"
        )));
    }
    Err(ApiError::internal("could not create backup folder"))
}

pub async fn revoke(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let result = sqlx::query(
        "UPDATE device_tokens SET revoked_at = ? \
         WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(crate::now())
    .bind(&id)
    .bind(&user.id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("token not found or already revoked"));
    }
    audit(&state, &user.id, "tokens.revoke", &id).await;
    Ok(Json(json!({ "ok": true })))
}

async fn audit(state: &AppState, user_id: &str, action: &str, target: &str) {
    let _ = sqlx::query("INSERT INTO audit_log (user_id, action, target, at) VALUES (?, ?, ?, ?)")
        .bind(user_id)
        .bind(action)
        .bind(target)
        .bind(crate::now())
        .execute(&state.pool)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sha256_hex;

    #[test]
    fn device_tokens_are_prefixed_and_hashed() {
        let (raw, hash) = auth::new_device_token();
        assert!(raw.starts_with("nmb_"));
        assert_eq!(hash, sha256_hex(raw.as_bytes()));
        assert_ne!(raw, auth::new_device_token().0);
    }

    #[test]
    fn token_json_never_leaks_hashes() {
        let row = DeviceTokenRow {
            id: "t1".into(),
            name: "phone".into(),
            token_hash: "deadbeef".into(),
            user_id: "u1".into(),
            scope: "backup".into(),
            backup_root_id: Some("root-folder".into()),
            created_at: 1,
            last_used_at: Some(2),
            expires_at: None,
            revoked_at: None,
        };
        let value = token_json(&row);
        assert!(value.get("token_hash").is_none());
        assert_eq!(value["status"], "active");
        assert_eq!(value["scope"], "backup");
        assert_eq!(value["backup_root_id"], "root-folder");
    }

    #[test]
    fn scopes_are_explicit() {
        assert_eq!(validate_scope(None).unwrap(), "full");
        assert_eq!(validate_scope(Some("full")).unwrap(), "full");
        assert_eq!(validate_scope(Some("backup")).unwrap(), "backup");
        assert!(validate_scope(Some("admin")).is_err());
        assert!(validate_scope(Some("")).is_ok());
    }
}
