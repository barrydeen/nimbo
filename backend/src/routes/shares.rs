use crate::{
    auth::{self, AuthUser},
    error::ApiError,
    files::{self, lock, ROOT_ID},
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

/// Public folder listings are bounded; the response carries a `truncated` flag
/// instead of materialising an unbounded number of rows.
pub const MAX_PUBLIC_ITEMS: usize = 2_000;

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

/// The small field set needed for public listings/navigation. Extracted text,
/// metadata, vision output and hashes are never loaded for public requests.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SummaryNode {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub mime: Option<String>,
    pub size: i64,
    pub thumb_sha: Option<String>,
}

impl SummaryNode {
    pub fn is_file(&self) -> bool {
        self.kind == "file"
    }

    pub fn is_folder(&self) -> bool {
        self.kind == "folder"
    }

    /// Minimal public representation. Parent ids and timestamps are omitted:
    /// items always live in the folder being listed, and the share root's parent
    /// must not leak an otherwise unshared identifier.
    pub fn public_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "name": self.name,
            "mime": self.mime,
            "size": self.size,
            "has_thumb": self.thumb_sha.is_some(),
        })
    }
}

/// Blob reference for a public download/content request.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PublicFile {
    pub kind: String,
    pub name: String,
    pub mime: Option<String>,
    pub blob_sha: Option<String>,
    pub thumb_sha: Option<String>,
}

impl PublicFile {
    pub fn is_file(&self) -> bool {
        self.kind == "file"
    }

    pub fn blob_sha(&self) -> Result<&str, ApiError> {
        self.blob_sha
            .as_deref()
            .ok_or_else(|| ApiError::internal("file has no stored content"))
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
    files::require_full_scope(&user)?;
    if req.target_type != "file" && req.target_type != "folder" {
        return Err(ApiError::bad_request("target_type must be file or folder"));
    }
    if let Some(expires) = req.expires_at {
        if expires <= crate::now() {
            return Err(ApiError::bad_request("expiry must be in the future"));
        }
    }

    // Hold the tree mutation lock so a concurrent delete cannot remove the
    // target between validation and the share insert.
    let _guard = lock::mutation().await;
    let mut tx = files::begin_write(&state.pool).await?;
    let node = files::require_node_conn(&mut tx, &req.target_id).await?;
    if (req.target_type == "file" && !node.is_file())
        || (req.target_type == "folder" && !node.is_folder())
    {
        return Err(ApiError::bad_request("share type does not match the item"));
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
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

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

pub async fn list_shares(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    files::require_full_scope(&user)?;
    let rows: Vec<ShareListRow> = sqlx::query_as(
        "WITH RECURSIVE paths(id, path, visited) AS ( \
           SELECT id, '', ',' || id || ',' FROM nodes WHERE id = 'root' \
           UNION ALL \
           SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END, \
                  p.visited || n.id || ',' \
           FROM nodes n JOIN paths p ON n.parent_id = p.id \
           WHERE instr(p.visited, ',' || n.id || ',') = 0 \
             AND length(p.visited) < 100000 \
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

async fn load_share_conn(conn: &mut SqliteConnection, id: &str) -> Result<ShareRow, ApiError> {
    sqlx::query_as::<_, ShareRow>("SELECT * FROM shares WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
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
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<UpdateShareRequest>,
) -> Result<Json<Value>, ApiError> {
    files::require_full_scope(&user)?;
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
    files::require_full_scope(&user)?;
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
    pub target: SummaryNode,
}

/// Snapshot the active share and its target root. Public handlers run this and
/// every subsequent selection inside one read transaction so authorization and
/// data selection share a single SQLite snapshot.
pub async fn load_active_conn(
    conn: &mut SqliteConnection,
    token: &str,
) -> Result<ActiveShare, ApiError> {
    let share = load_share_conn(conn, token).await?;
    validate_active(&share)?;
    let target: Option<SummaryNode> =
        sqlx::query_as("SELECT id, kind, name, mime, size, thumb_sha FROM nodes WHERE id = ?")
            .bind(&share.target_id)
            .fetch_optional(&mut *conn)
            .await?;
    let target = target.ok_or_else(|| ApiError::gone("the shared item no longer exists"))?;
    Ok(ActiveShare { share, target })
}

fn validate_active(share: &ShareRow) -> Result<(), ApiError> {
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
    Ok(())
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

fn subtree_select(columns: &str) -> String {
    format!(
        "{SUBTREE_CTE} ) SELECT {columns} FROM nodes n \
         WHERE n.id = ? AND n.id IN (SELECT id FROM subtree)",
        SUBTREE_CTE = files::SUBTREE_CTE
    )
}

/// Loads a contained folder and its children in statements anchored at the
/// share root. A subtree moved outside the share between requests yields no
/// rows, because every selected id must occur in the same share-rooted CTE.
pub async fn children_snapshot(
    conn: &mut SqliteConnection,
    active: &ActiveShare,
    folder_id: Option<&str>,
) -> Result<(SummaryNode, Vec<SummaryNode>, bool), ApiError> {
    let folder = folder_snapshot(conn, active, folder_id).await?;
    let folder_id = folder.id.clone();

    let children_sql = format!(
        "{} ) SELECT n.id, n.kind, n.name, n.mime, n.size, n.thumb_sha \
         FROM nodes n \
         WHERE n.parent_id = ? AND n.id IN (SELECT id FROM subtree) \
         ORDER BY CASE n.kind WHEN 'folder' THEN 0 ELSE 1 END, n.name COLLATE NOCASE \
         LIMIT ?",
        files::SUBTREE_CTE
    );
    let mut rows: Vec<SummaryNode> = sqlx::query_as(sqlx::AssertSqlSafe(children_sql))
        .bind(&active.target.id)
        .bind(&folder_id)
        .bind((MAX_PUBLIC_ITEMS + 1) as i64)
        .fetch_all(&mut *conn)
        .await?;
    let truncated = rows.len() > MAX_PUBLIC_ITEMS;
    rows.truncate(MAX_PUBLIC_ITEMS);
    Ok((folder, rows, truncated))
}

/// Loads only the requested folder, anchored at the share root.
pub async fn folder_snapshot(
    conn: &mut SqliteConnection,
    active: &ActiveShare,
    folder_id: Option<&str>,
) -> Result<SummaryNode, ApiError> {
    if active.share.target_type == "file" || active.target.is_file() {
        return Err(ApiError::bad_request("this link shares a single file"));
    }
    let folder_id = folder_id.unwrap_or(&active.target.id);
    let folder_sql = subtree_select("n.id, n.kind, n.name, n.mime, n.size, n.thumb_sha");
    let folder: Option<SummaryNode> = sqlx::query_as(sqlx::AssertSqlSafe(folder_sql))
        .bind(&active.target.id)
        .bind(folder_id)
        .fetch_optional(&mut *conn)
        .await?;
    let folder = folder.ok_or_else(|| ApiError::not_found("item is not part of this share"))?;
    if !folder.is_folder() {
        return Err(ApiError::bad_request("not a folder"));
    }
    Ok(folder)
}

/// Loads a contained file with only the columns needed to stream it.
pub async fn file_snapshot(
    conn: &mut SqliteConnection,
    active: &ActiveShare,
    id: &str,
) -> Result<PublicFile, ApiError> {
    let sql = subtree_select("n.id, n.kind, n.name, n.mime, n.blob_sha, n.thumb_sha");
    let node: Option<PublicFile> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&active.target.id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let node = node.ok_or_else(|| ApiError::not_found("item is not part of this share"))?;
    if !node.is_file() {
        return Err(ApiError::not_found("file not found"));
    }
    Ok(node)
}

pub async fn public_breadcrumbs_conn(
    conn: &mut SqliteConnection,
    share: &ActiveShare,
    folder_id: &str,
) -> Result<Vec<Value>, ApiError> {
    let all = files::breadcrumbs_conn(conn, folder_id).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_summary_omits_parent_and_timestamps() {
        let node = SummaryNode {
            id: "n1".into(),
            kind: "file".into(),
            name: "photo.jpg".into(),
            mime: Some("image/jpeg".into()),
            size: 10,
            thumb_sha: None,
        };
        let value = node.public_json();
        assert!(value.get("parent_id").is_none());
        assert!(value.get("updated_at").is_none());
        assert_eq!(value["name"], "photo.jpg");
    }

    #[test]
    fn active_share_validation_rejects_revoked_and_expired() {
        let mut share = ShareRow {
            id: "t".into(),
            target_type: "folder".into(),
            target_id: "n".into(),
            created_by: None,
            created_at: 0,
            expires_at: None,
            revoked_at: None,
            last_accessed_at: None,
            access_count: 0,
        };
        assert!(validate_active(&share).is_ok());
        share.revoked_at = Some(1);
        assert!(validate_active(&share).is_err());
        share.revoked_at = None;
        share.expires_at = Some(crate::now() - 1);
        assert!(validate_active(&share).is_err());
    }

    #[tokio::test]
    async fn public_selection_is_anchored_to_the_share_root() {
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE nodes (id TEXT PRIMARY KEY, parent_id TEXT NOT NULL, \
             kind TEXT NOT NULL, name TEXT NOT NULL, mime TEXT, size INTEGER NOT NULL, \
             thumb_sha TEXT, blob_sha TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, size, blob_sha) VALUES \
             ('shared', 'root', 'folder', 'shared', 0, NULL), \
             ('inside', 'shared', 'file', 'inside.txt', 5, 'abc'), \
             ('outside', 'root', 'folder', 'outside', 0, NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let active = ActiveShare {
            share: ShareRow {
                id: "token".into(),
                target_type: "folder".into(),
                target_id: "shared".into(),
                created_by: None,
                created_at: 0,
                expires_at: None,
                revoked_at: None,
                last_accessed_at: None,
                access_count: 0,
            },
            target: SummaryNode {
                id: "shared".into(),
                kind: "folder".into(),
                name: "shared".into(),
                mime: None,
                size: 0,
                thumb_sha: None,
            },
        };
        let mut conn = pool.acquire().await.unwrap();
        let (folder, items, truncated) = children_snapshot(&mut conn, &active, None).await.unwrap();
        assert_eq!(folder.id, "shared");
        assert_eq!(items.len(), 1);
        assert!(!truncated);

        // Moving the child out of the shared subtree must make it invisible even
        // though its id is still requested directly.
        sqlx::query("UPDATE nodes SET parent_id = 'outside' WHERE id = 'inside'")
            .execute(&mut *conn)
            .await
            .unwrap();
        let (_, items, _) = children_snapshot(&mut conn, &active, None).await.unwrap();
        assert!(items.is_empty());
        assert!(file_snapshot(&mut conn, &active, "inside").await.is_err());
        assert!(folder_snapshot(&mut conn, &active, Some("outside"))
            .await
            .is_err());
    }
}
