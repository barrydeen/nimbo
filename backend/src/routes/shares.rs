use crate::{
    auth::{self, AuthUser},
    error::ApiError,
    files::{self, Node, ROOT_ID},
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
pub struct ShareRow {
    pub id: String,
    pub target_type: String,
    pub target_id: String,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub last_accessed_at: Option<i64>,
    pub access_count: i64,
}

impl ShareRow {
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

pub fn share_json(share: &ShareRow, target_name: Option<&str>, target_path: Option<&str>) -> Value {
    json!({
        "id": share.id,
        "token": share.id,
        "url": format!("/s/{}", share.id),
        "target_type": share.target_type,
        "target_id": share.target_id,
        "target_name": target_name,
        "target_path": target_path,
        "created_by": share.created_by,
        "created_at": share.created_at,
        "expires_at": share.expires_at,
        "revoked_at": share.revoked_at,
        "last_accessed_at": share.last_accessed_at,
        "access_count": share.access_count,
        "status": share.status(),
    })
}

pub async fn list_for_node(state: &AppState, node_id: &str) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query_as::<_, ShareRow>(
        "SELECT * FROM shares WHERE target_id = ? ORDER BY created_at DESC",
    )
    .bind(node_id)
    .fetch_all(&state.pool)
    .await?;
    let name = files::get_node(&state.pool, node_id)
        .await?
        .map(|node| node.name);
    Ok(rows
        .iter()
        .map(|share| share_json(share, name.as_deref(), None))
        .collect())
}

#[derive(Deserialize)]
pub struct CreateShareRequest {
    pub target_type: String,
    pub target_id: String,
    pub expires_at: Option<i64>,
}

pub async fn create_share(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<CreateShareRequest>,
) -> Result<Json<Value>, ApiError> {
    let node = files::require_node(&state.pool, &req.target_id).await?;
    if (req.target_type == "file" && !node.is_file())
        || (req.target_type == "folder" && !node.is_folder())
    {
        return Err(ApiError::bad_request("share type does not match the item"));
    }
    if req.target_type != "file" && req.target_type != "folder" {
        return Err(ApiError::bad_request("target_type must be file or folder"));
    }
    if let Some(expires) = req.expires_at {
        if expires <= crate::now() {
            return Err(ApiError::bad_request("expiry must be in the future"));
        }
    }

    let token = auth::new_token().0;
    sqlx::query(
        "INSERT INTO shares (id, target_type, target_id, created_by, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(&req.target_type)
    .bind(&req.target_id)
    .bind(&user.id)
    .bind(crate::now())
    .bind(req.expires_at)
    .execute(&state.pool)
    .await?;

    audit(&state, &user.id, "shares.create", &token).await;
    let share = load_share(&state, &token).await?;
    Ok(Json(
        json!({ "share": share_json(&share, Some(&node.name), None) }),
    ))
}

type ShareListRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<String>,
    Option<String>,
);

pub async fn list_shares(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<ShareListRow> = sqlx::query_as(
        "WITH RECURSIVE paths(id, path) AS ( \
           SELECT id, '' FROM nodes WHERE id = 'root' \
           UNION ALL \
           SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END \
           FROM nodes n JOIN paths p ON n.parent_id = p.id \
         ) \
         SELECT s.id, s.target_type, s.target_id, s.created_by, s.created_at, s.expires_at, \
                s.revoked_at, s.last_accessed_at, s.access_count, n.name, p.path \
         FROM shares s LEFT JOIN nodes n ON n.id = s.target_id \
         LEFT JOIN paths p ON p.id = s.target_id \
         ORDER BY s.created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?;

    let shares: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                id,
                target_type,
                target_id,
                created_by,
                created_at,
                expires_at,
                revoked_at,
                last_accessed_at,
                access_count,
                target_name,
                target_path,
            )| {
                let share = ShareRow {
                    id,
                    target_type,
                    target_id,
                    created_by,
                    created_at,
                    expires_at,
                    revoked_at,
                    last_accessed_at,
                    access_count,
                };
                share_json(&share, target_name.as_deref(), target_path.as_deref())
            },
        )
        .collect();
    Ok(Json(json!({ "shares": shares })))
}

async fn load_share(state: &AppState, id: &str) -> Result<ShareRow, ApiError> {
    sqlx::query_as::<_, ShareRow>("SELECT * FROM shares WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("share link not found"))
}

#[derive(Deserialize)]
pub struct UpdateShareRequest {
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub clear_expiry: bool,
}

pub async fn update_share(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateShareRequest>,
) -> Result<Json<Value>, ApiError> {
    let share = load_share(&state, &id).await?;
    if share.revoked_at.is_some() {
        return Err(ApiError::bad_request("revoked links cannot be changed"));
    }
    let expires = if req.clear_expiry {
        None
    } else {
        match req.expires_at {
            Some(expires) if expires > crate::now() => Some(expires),
            Some(_) => return Err(ApiError::bad_request("expiry must be in the future")),
            None => return Err(ApiError::bad_request("provide expires_at or clear_expiry")),
        }
    };
    sqlx::query("UPDATE shares SET expires_at = ? WHERE id = ?")
        .bind(expires)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    let share = load_share(&state, &id).await?;
    Ok(Json(json!({ "share": share_json(&share, None, None) })))
}

pub async fn revoke_share(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let share = load_share(&state, &id).await?;
    if share.revoked_at.is_none() {
        sqlx::query("UPDATE shares SET revoked_at = ? WHERE id = ?")
            .bind(crate::now())
            .bind(&id)
            .execute(&state.pool)
            .await?;
        audit(&state, &user.id, "shares.revoke", &id).await;
    }
    let share = load_share(&state, &id).await?;
    Ok(Json(json!({ "share": share_json(&share, None, None) })))
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

// --------------------------------------------------------------- public side

pub struct ActiveShare {
    pub share: ShareRow,
    pub target: Node,
}

pub async fn load_active(state: &AppState, token: &str) -> Result<ActiveShare, ApiError> {
    let share = load_share(state, token).await?;
    if share.revoked_at.is_some() {
        return Err(ApiError::gone("this link has been revoked"));
    }
    if share
        .expires_at
        .map(|expires| expires <= crate::now())
        .unwrap_or(false)
    {
        return Err(ApiError::gone("this link has expired"));
    }
    let target = files::get_node(&state.pool, &share.target_id)
        .await?
        .ok_or_else(|| ApiError::gone("the shared item no longer exists"))?;
    Ok(ActiveShare { share, target })
}

pub async fn bump_access(state: &AppState, share: &ShareRow) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE shares SET last_accessed_at = ?, access_count = access_count + 1 WHERE id = ?",
    )
    .bind(crate::now())
    .bind(&share.id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Loads a file or folder and verifies it is reachable through the share.
pub async fn contained_node(
    state: &AppState,
    share: &ActiveShare,
    node_id: &str,
) -> Result<Node, ApiError> {
    let node = files::require_node(&state.pool, node_id).await?;
    let allowed = if share.share.target_type == "file" {
        node.id == share.target.id
    } else {
        files::is_within(&state.pool, &share.target.id, &node.id).await?
    };
    if !allowed {
        return Err(ApiError::not_found("item is not part of this share"));
    }
    Ok(node)
}

pub async fn contained_folder(
    state: &AppState,
    share: &ActiveShare,
    folder_id: Option<&str>,
) -> Result<Node, ApiError> {
    if share.share.target_type == "file" {
        return Err(ApiError::bad_request("this link shares a single file"));
    }
    let id = folder_id.unwrap_or(&share.target.id);
    let node = contained_node(state, share, id).await?;
    if !node.is_folder() {
        return Err(ApiError::bad_request("not a folder"));
    }
    Ok(node)
}

pub fn public_summary(node: &Node) -> Value {
    json!({
        "id": node.id,
        "parent_id": node.parent_id,
        "kind": node.kind,
        "name": node.name,
        "mime": node.mime,
        "size": node.size,
        "updated_at": node.updated_at,
        "has_thumb": node.thumb_sha.is_some(),
    })
}

pub async fn public_breadcrumbs(
    state: &AppState,
    share: &ActiveShare,
    folder_id: &str,
) -> Result<Vec<Value>, ApiError> {
    let all = files::breadcrumbs(&state.pool, folder_id).await?;
    let mut output = Vec::new();
    let mut inside = false;
    for crumb in all {
        if crumb.id == share.target.id {
            inside = true;
            continue;
        }
        if inside && crumb.id != ROOT_ID {
            output.push(json!({ "id": crumb.id, "name": crumb.name }));
        }
    }
    Ok(output)
}
