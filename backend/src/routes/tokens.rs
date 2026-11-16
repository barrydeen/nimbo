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
use std::sync::Arc;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeviceTokenRow {
    pub id: String,
    pub name: String,
    // The hash is the lookup key; user_id scopes listing and revocation.
    #[allow(dead_code)]
    pub token_hash: String,
    #[allow(dead_code)]
    pub user_id: String,
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

    let (raw, token_hash) = auth::new_device_token();
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::now();
    sqlx::query(
        "INSERT INTO device_tokens (id, name, token_hash, user_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(&token_hash)
    .bind(&user.id)
    .bind(now)
    .execute(&state.pool)
    .await?;

    audit(&state, &user.id, "tokens.create", &format!("{name} ({id})")).await;

    let row = DeviceTokenRow {
        id,
        name: name.to_string(),
        token_hash,
        user_id: user.id.clone(),
        created_at: now,
        last_used_at: None,
        expires_at: None,
        revoked_at: None,
    };
    // The raw token is returned exactly once and never persisted.
    Ok(Json(json!({ "token": raw, "item": token_json(&row) })))
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
            created_at: 1,
            last_used_at: Some(2),
            expires_at: None,
            revoked_at: None,
        };
        let value = token_json(&row);
        assert!(value.get("token_hash").is_none());
        assert_eq!(value["status"], "active");
    }
}
