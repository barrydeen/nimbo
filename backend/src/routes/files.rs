use crate::{
    auth::AuthUser,
    error::ApiError,
    files::{self, extract, lock, storage, Node, Scope, ROOT_ID},
    routes::shares,
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Extension, Multipart, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

#[derive(Deserialize)]
pub struct ListQuery {
    pub folder_id: Option<String>,
    /// Optional bounded listing pagination. Existing clients may omit it; the
    /// default still caps a single response so a huge folder cannot force an
    /// unbounded JSON allocation.
    pub offset: Option<u32>,
    pub limit: Option<u32>,
}

const DEFAULT_LIST_LIMIT: u32 = 500;
const MAX_LIST_LIMIT: u32 = 1_000;

#[derive(Debug, sqlx::FromRow)]
struct ListRow {
    id: String,
    parent_id: String,
    kind: String,
    name: String,
    mime: Option<String>,
    size: i64,
    created_at: i64,
    updated_at: i64,
    thumb_sha: Option<String>,
    blob_sha: Option<String>,
    index_status: String,
    vision_status: String,
}

impl ListRow {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "parent_id": self.parent_id,
            "kind": self.kind,
            "name": self.name,
            "mime": self.mime,
            "size": self.size,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "has_thumb": self.thumb_sha.is_some(),
            "index_status": self.index_status,
            "vision_status": self.vision_status,
            "sha256": self.blob_sha,
        })
    }
}

const LIST_COLUMNS: &str = "id, parent_id, kind, name, mime, size, created_at, updated_at, \
                            thumb_sha, blob_sha, index_status, vision_status";

fn folder_or_root(folder_id: &Option<String>) -> String {
    folder_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| ROOT_ID.to_string())
}

fn display_folder_name(id: &str, name: &str) -> String {
    if id == ROOT_ID {
        "My Drive".to_string()
    } else {
        name.to_string()
    }
}

pub fn summary(node: &Node) -> Value {
    json!({
        "id": node.id,
        "parent_id": node.parent_id,
        "kind": node.kind,
        "name": node.name,
        "mime": node.mime,
        "size": node.size,
        "created_at": node.created_at,
        "updated_at": node.updated_at,
        "has_thumb": node.thumb_sha.is_some(),
        "index_status": node.index_status,
        "vision_status": node.vision_status,
        "sha256": node.blob_sha,
    })
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

/// Loads a node and rejects everything outside a restricted token's backup
/// subtree. Full-scope callers are unchanged.
async fn authorize_node(state: &AppState, user: &AuthUser, id: &str) -> Result<Node, ApiError> {
    let scope = Scope::for_user(user)?;
    let node = files::require_node(&state.pool, id).await?;
    if scope.restricted && !files::is_within(&state.pool, &scope.root, &node.id).await? {
        return Err(ApiError::not_found("item not found"));
    }
    Ok(node)
}

fn ensure_writable_folder(scope: &Scope, folder_id: &str) -> Result<(), ApiError> {
    if scope.restricted && folder_id == ROOT_ID {
        // Writes only ever land below the assigned backup root.
        return Err(ApiError::forbidden(
            "this token may only write inside its backup folder",
        ));
    }
    Ok(())
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let folder_id = folder_or_root(&params.folder_id);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let offset = params.offset.unwrap_or(0);

    if scope.restricted && folder_id == ROOT_ID {
        // A backup token sees exactly one entry at the drive root: its assigned
        // backup folder.
        let root = files::require_node(&state.pool, &scope.root)
            .await
            .map_err(|_| ApiError::forbidden("the assigned backup folder is unavailable"))?;
        if !root.is_folder() {
            return Err(ApiError::forbidden(
                "the assigned backup folder is unavailable",
            ));
        }
        return Ok(Json(json!({
            "folder": { "id": ROOT_ID, "name": "My Drive", "parent_id": "" },
            "breadcrumbs": [{ "id": ROOT_ID, "name": "My Drive" }],
            "items": if offset == 0 { vec![summary(&root)] } else { Vec::new() },
            "next_offset": null,
        })));
    }

    let mut tx = state.pool.begin().await?;
    let folder = files::require_folder_conn(&mut tx, &folder_id).await?;
    if scope.restricted && !files::is_within_conn(&mut tx, &scope.root, &folder.id).await? {
        return Err(ApiError::not_found("item not found"));
    }
    let sql = format!(
        "SELECT {LIST_COLUMNS} FROM nodes WHERE parent_id = ? \
         ORDER BY CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name COLLATE NOCASE, id \
         LIMIT ? OFFSET ?"
    );
    let mut rows = sqlx::query_as::<_, ListRow>(sqlx::AssertSqlSafe(sql))
        .bind(&folder_id)
        .bind(i64::from(limit) + 1)
        .bind(i64::from(offset))
        .fetch_all(&mut *tx)
        .await?;
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_offset = if has_more {
        Some(offset.saturating_add(limit))
    } else {
        None
    };
    let breadcrumbs: Vec<Value> = files::breadcrumbs_conn(&mut tx, &folder_id)
        .await?
        .into_iter()
        .map(|crumb| {
            json!({
                "id": crumb.id,
                "name": display_folder_name(&crumb.id, &crumb.name),
            })
        })
        .collect();
    tx.commit().await?;

    Ok(Json(json!({
        "folder": {
            "id": folder.id,
            "name": display_folder_name(&folder.id, &folder.name),
            "parent_id": folder.parent_id,
        },
        "breadcrumbs": breadcrumbs,
        "items": rows.iter().map(ListRow::json).collect::<Vec<_>>(),
        "next_offset": next_offset,
    })))
}

#[derive(Debug, sqlx::FromRow)]
struct RecentRow {
    id: String,
    name: String,
    parent_id: String,
    mime: Option<String>,
    size: i64,
    updated_at: i64,
    path: String,
    blob_sha: Option<String>,
}

pub async fn recent(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let sql = format!(
        "{SCOPE_CTE} ) , paths(id, path, visited) AS ( \
           SELECT id, '', ',' || id || ',' FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END, \
                  p.visited || n.id || ',' \
           FROM nodes n JOIN paths p ON n.parent_id = p.id \
           WHERE instr(p.visited, ',' || n.id || ',') = 0 AND length(p.visited) < 100000 \
         ) \
         SELECT n.id, n.name, n.parent_id, n.mime, n.size, n.updated_at, p.path, n.blob_sha \
         FROM nodes n JOIN paths p ON p.id = n.id \
         WHERE n.kind = 'file' ORDER BY n.updated_at DESC, n.name LIMIT 50",
        SCOPE_CTE = files::SCOPE_CTE
    );
    let rows: Vec<RecentRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&scope.root)
        .bind(&scope.root)
        .fetch_all(&state.pool)
        .await?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "name": row.name,
                "parent_id": row.parent_id,
                "mime": row.mime,
                "size": row.size,
                "updated_at": row.updated_at,
                "path": row.path,
                "kind": "file",
                "sha256": row.blob_sha,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct CreateFolderRequest {
    pub parent_id: Option<String>,
    pub name: String,
}

pub async fn create_folder(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<CreateFolderRequest>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let parent_id = folder_or_root(&req.parent_id);
    let name = files::validate_name(&req.name)?;

    let _guard = lock::mutation().await;
    let mut tx = files::begin_write(&state.pool).await?;
    files::require_folder_conn(&mut tx, &parent_id).await?;
    ensure_writable_folder(&scope, &parent_id)?;
    if scope.restricted && !files::is_within_conn(&mut tx, &scope.root, &parent_id).await? {
        return Err(ApiError::forbidden(
            "this token may only create items inside its backup folder",
        ));
    }
    let unique = files::unique_child_name_conn(&mut tx, &parent_id, &name, None).await?;
    if unique != name {
        return Err(ApiError::conflict(
            "a folder or file with that name already exists",
        ));
    }
    let node =
        files::insert_node_conn(&mut tx, &parent_id, "folder", &name, Some(&user.id)).await?;
    tx.commit().await?;
    drop(_guard);

    audit(&state, &user.id, "files.folder.create", &node.id).await;
    Ok(Json(json!({ "item": summary(&node) })))
}

#[derive(Deserialize)]
pub struct UpdateNodeRequest {
    pub name: Option<String>,
    pub parent_id: Option<String>,
}

pub async fn update_node(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNodeRequest>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let _guard = lock::mutation().await;
    let mut tx = files::begin_write(&state.pool).await?;
    let node = files::require_node_conn(&mut tx, &id).await?;
    if node.id == ROOT_ID {
        return Err(ApiError::bad_request(
            "the drive root cannot be renamed or moved",
        ));
    }
    if scope.restricted && !files::is_within_conn(&mut tx, &scope.root, &node.id).await? {
        return Err(ApiError::not_found("item not found"));
    }

    let new_parent = req
        .parent_id
        .clone()
        .unwrap_or_else(|| node.parent_id.clone());
    if req.parent_id.is_some() {
        files::require_folder_conn(&mut tx, &new_parent).await?;
        ensure_writable_folder(&scope, &new_parent)?;
        if scope.restricted && !files::is_within_conn(&mut tx, &scope.root, &new_parent).await? {
            return Err(ApiError::forbidden(
                "this token may only move items inside its backup folder",
            ));
        }
        if node.is_folder() && files::is_within_conn(&mut tx, &node.id, &new_parent).await? {
            return Err(ApiError::bad_request(
                "a folder cannot be moved inside itself",
            ));
        }
    }

    let new_name = match req.name.as_deref() {
        Some(raw) => files::validate_name(raw)?,
        None => node.name.clone(),
    };
    let unique =
        files::unique_child_name_conn(&mut tx, &new_parent, &new_name, Some(&node.id)).await?;
    if unique != new_name {
        return Err(ApiError::conflict(
            "a folder or file with that name already exists",
        ));
    }

    let moved = new_parent != node.parent_id;
    let renamed = new_name != node.name;
    if moved {
        sqlx::query("UPDATE nodes SET parent_id = ?, updated_at = ? WHERE id = ?")
            .bind(&new_parent)
            .bind(crate::now())
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
    }
    if renamed {
        sqlx::query("UPDATE nodes SET name = ?, updated_at = ? WHERE id = ?")
            .bind(&new_name)
            .bind(crate::now())
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
    }
    if !moved && !renamed {
        sqlx::query("UPDATE nodes SET updated_at = ? WHERE id = ?")
            .bind(crate::now())
            .bind(&node.id)
            .execute(&mut *tx)
            .await?;
    }

    let updated = files::require_node_conn(&mut tx, &node.id).await?;
    if updated.is_file() {
        let metadata = crate::files::vision::metadata_search_text(&updated);
        files::upsert_fts_conn(
            &mut tx,
            &updated.id,
            &updated.name,
            updated.text_content.as_deref(),
            updated.vision_description.as_deref(),
            Some(&metadata),
        )
        .await?;
    }
    tx.commit().await?;
    drop(_guard);
    Ok(Json(json!({ "item": summary(&updated) })))
}

pub async fn delete_node(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if id == ROOT_ID {
        return Err(ApiError::bad_request("the drive root cannot be deleted"));
    }
    let scope = Scope::for_user(&user)?;
    let guard = lock::mutation().await;
    let mut tx = files::begin_write(&state.pool).await?;
    let node = files::require_node_conn(&mut tx, &id).await?;
    if scope.restricted {
        if node.id == scope.root {
            return Err(ApiError::forbidden(
                "the assigned backup folder cannot be deleted",
            ));
        }
        if !files::is_within_conn(&mut tx, &scope.root, &node.id).await? {
            return Err(ApiError::not_found("item not found"));
        }
    }
    let ids = files::subtree_ids_conn(&mut tx, &node.id).await?;

    // One stable subtree snapshot inside one transaction for shares, FTS and
    // nodes, so a concurrent move cannot leave stale dependants behind.
    let delete_sql = |table: &str, column: &str| {
        format!(
            "{SUBTREE_CTE} ) DELETE FROM {table} WHERE {column} IN (SELECT id FROM subtree)",
            SUBTREE_CTE = files::SUBTREE_CTE
        )
    };
    sqlx::query(sqlx::AssertSqlSafe(delete_sql("shares", "target_id")))
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(delete_sql("files_fts", "file_id")))
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(delete_sql("nodes", "id")))
        .bind(&node.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    drop(guard);

    // Reclaiming the unreferenced blobs is best-effort: the delete itself has
    // committed and must not be reported as failed if cleanup is retried later.
    if let Err(err) = storage::gc_unreferenced(&state).await {
        tracing::warn!(error = %err, "blob garbage collection after delete failed");
    }
    audit(&state, &user.id, "files.delete", &id).await;
    Ok(Json(json!({ "deleted": id, "count": ids.len() })))
}

pub async fn get_file(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let node = authorize_node(&state, &user, &id).await?;
    let breadcrumbs: Vec<Value> = files::breadcrumbs(&state.pool, &node.parent_id)
        .await?
        .into_iter()
        .map(|crumb| {
            json!({
                "id": crumb.id,
                "name": display_folder_name(&crumb.id, &crumb.name),
            })
        })
        .collect();
    let metadata: Value = serde_json::from_str(&node.metadata_json).unwrap_or_else(|_| json!({}));
    let text_preview = node
        .text_content
        .as_deref()
        .map(|text| text.chars().take(4000).collect::<String>());
    let tags: Value = node
        .vision_tags
        .as_deref()
        .and_then(|tags| serde_json::from_str(tags).ok())
        .unwrap_or_else(|| json!([]));
    let node_shares = if scope.restricted {
        Vec::new()
    } else {
        shares::list_for_node(&state, &node.id).await?
    };

    Ok(Json(json!({
        "item": {
            "id": node.id,
            "parent_id": node.parent_id,
            "kind": node.kind,
            "name": node.name,
            "mime": node.mime,
            "size": node.size,
            "sha256": node.blob_sha,
            "created_at": node.created_at,
            "updated_at": node.updated_at,
            "has_thumb": node.thumb_sha.is_some(),
            "index_status": node.index_status,
            "index_error": node.index_error,
            "metadata": metadata,
            "text_preview": text_preview,
            "vision": {
                "status": node.vision_status,
                "description": node.vision_description,
                "text": node.vision_text,
                "tags": tags,
                "model": node.vision_model,
                "at": node.vision_at,
                "error": node.vision_error,
            },
        },
        "path": files::path_for(&state.pool, &node.id).await?,
        "breadcrumbs": breadcrumbs,
        "shares": node_shares,
    })))
}

// ------------------------------------------------------------------ uploads

const MAX_UPLOAD_FILES: usize = 100;
const SNIFF_BYTES: usize = 8192;

struct StagedUpload {
    temp: storage::TempFile,
    sha: String,
    size: u64,
    head: Vec<u8>,
}

/// Streams one multipart file field to a private temp file, enforcing the
/// per-file limit. The RAII temp guard removes the artifact on every error path.
async fn stage_field(
    state: &AppState,
    mut field: axum::extract::multipart::Field<'_>,
) -> Result<StagedUpload, ApiError> {
    let (temp, mut file) =
        storage::TempFile::create_with_file(&state.cfg.upload_tmp_dir(), "upload-", ".tmp").await?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|err| ApiError::bad_request(format!("upload stream failed: {err}")))?
    {
        size += chunk.len() as u64;
        if size > state.cfg.max_upload_bytes {
            return Err(ApiError::payload_too_large(format!(
                "file exceeds the {} MiB upload limit",
                state.cfg.max_upload_bytes / (1024 * 1024)
            )));
        }
        if head.len() < SNIFF_BYTES {
            let take = (SNIFF_BYTES - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    drop(file);
    if size == 0 {
        return Err(ApiError::bad_request("file is empty"));
    }
    let sha = hex::encode(hasher.finalize());
    Ok(StagedUpload {
        temp,
        sha,
        size,
        head,
    })
}

async fn commit_upload(
    state: &AppState,
    user: &AuthUser,
    scope: &Scope,
    folder_id: &str,
    name: &str,
    mut staged: StagedUpload,
) -> Result<Node, ApiError> {
    let mime = extract::detect_mime(name, &staged.head);
    let guard = lock::mutation().await;

    // Move the blob before taking SQLite's write lock so file I/O never holds
    // the database writer hostage. The recheck below is authoritative and rolls
    // the new file back if the parent/scope changed.
    let promoted =
        storage::promote_temp(state, &mut staged.temp, &staged.sha, staged.size as i64).await?;
    let outcome: Result<(String, String), ApiError> = async {
        let mut tx = files::begin_write(&state.pool).await?;
        // Recheck parent existence, kind and scope *inside* the serialized
        // critical section so a folder deleted during the upload cannot orphan
        // this node.
        let folder = files::require_folder_conn(&mut tx, folder_id).await?;
        if scope.restricted
            && (folder.id == ROOT_ID
                || !files::is_within_conn(&mut tx, &scope.root, &folder.id).await?)
        {
            return Err(ApiError::forbidden(
                "this token may only upload inside its backup folder",
            ));
        }
        let unique_name = files::unique_child_name_conn(&mut tx, folder_id, name, None).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let ts = crate::now();
        let vision_status = if files::vision_eligible(&mime) {
            "pending"
        } else {
            "n/a"
        };
        storage::register_blob_tx(&mut tx, &staged.sha, staged.size as i64).await?;
        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, owner_id, created_at, updated_at, \
             blob_sha, mime, size, index_status, vision_status, metadata_json) \
             VALUES (?, ?, 'file', ?, ?, ?, ?, ?, ?, ?, 'pending', ?, '{}')",
        )
        .bind(&id)
        .bind(folder_id)
        .bind(&unique_name)
        .bind(&user.id)
        .bind(ts)
        .bind(ts)
        .bind(&staged.sha)
        .bind(&mime)
        .bind(staged.size as i64)
        .bind(vision_status)
        .execute(&mut *tx)
        .await?;
        files::upsert_fts_conn(&mut tx, &id, &unique_name, None, None, Some("")).await?;
        tx.commit().await?;
        Ok((id, unique_name))
    }
    .await;
    match outcome {
        Ok((id, _)) => {
            drop(guard);
            files::require_node(&state.pool, &id).await
        }
        Err(err) => {
            storage::rollback_promoted(state, &promoted).await;
            drop(guard);
            Err(err)
        }
    }
}

pub async fn upload(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<ListQuery>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let folder_id = folder_or_root(&params.folder_id);
    ensure_writable_folder(&scope, &folder_id)?;
    let folder = files::require_folder(&state.pool, &folder_id).await?;
    if scope.restricted && !files::is_within(&state.pool, &scope.root, &folder.id).await? {
        return Err(ApiError::forbidden(
            "this token may only upload inside its backup folder",
        ));
    }
    storage::init(&state).await?;

    let mut uploaded = Vec::new();
    let mut failures = Vec::new();
    let mut file_count = 0usize;

    loop {
        let next = multipart.next_field().await;
        let field = match next {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                // The multipart body itself is broken. Report already committed
                // members explicitly instead of hiding them behind a 400.
                if uploaded.is_empty() {
                    return Err(ApiError::bad_request(format!("invalid upload: {err}")));
                }
                failures
                    .push(json!({ "name": "", "error": format!("upload stream ended: {err}") }));
                break;
            }
        };
        if field.name() != Some("file") {
            continue;
        }
        file_count += 1;
        let raw_name = field.file_name().unwrap_or("").to_string();
        if file_count > MAX_UPLOAD_FILES {
            failures.push(json!({
                "name": raw_name,
                "error": format!("at most {MAX_UPLOAD_FILES} files per upload"),
            }));
            continue;
        }
        let name = match files::validate_name(&raw_name) {
            Ok(name) => name,
            Err(err) => {
                failures.push(json!({ "name": raw_name, "error": err.message }));
                continue;
            }
        };
        let staged = match stage_field(&state, field).await {
            Ok(staged) => staged,
            Err(err) if err.status == StatusCode::PAYLOAD_TOO_LARGE => {
                failures.push(json!({ "name": name, "error": err.message }));
                continue;
            }
            Err(err) => {
                failures.push(json!({ "name": name, "error": err.message }));
                continue;
            }
        };
        match commit_upload(&state, &user, &scope, &folder_id, &name, staged).await {
            Ok(node) => {
                audit(
                    &state,
                    &user.id,
                    "files.upload",
                    &format!("{} ({})", node.name, node.blob_sha.as_deref().unwrap_or("")),
                )
                .await;
                uploaded.push(summary(&node));
            }
            Err(err) => {
                failures.push(json!({ "name": name, "error": err.message }));
            }
        }
    }

    if uploaded.is_empty() && failures.is_empty() {
        return Err(ApiError::bad_request("no files were uploaded"));
    }
    Ok(Json(json!({ "uploaded": uploaded, "failed": failures })))
}

/// Replaces the content of an existing file node in place, preserving its id
/// and name. The previous blob stays referenced until the new blob and its
/// database references commit; only then is garbage collection allowed to
/// reclaim it.
pub async fn replace_content(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    let node = authorize_node(&state, &user, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request("folders do not have file content"));
    }
    storage::init(&state).await?;

    let mut staged: Option<StagedUpload> = None;
    loop {
        let field = multipart
            .next_field()
            .await
            .map_err(|err| ApiError::bad_request(format!("invalid upload: {err}")))?;
        let Some(field) = field else { break };
        if field.name() != Some("file") {
            continue;
        }
        if staged.is_some() {
            return Err(ApiError::bad_request(
                "replace accepts exactly one file field",
            ));
        }
        staged = Some(stage_field(&state, field).await?);
    }
    let mut staged = staged.ok_or_else(|| ApiError::bad_request("no file was uploaded"))?;

    // Preserve the node name but derive the MIME type from the new bytes.
    let mime = extract::detect_mime(&node.name, &staged.head);
    let scope = Scope::for_user(&user)?;
    let guard = lock::mutation().await;
    let promoted =
        storage::promote_temp(&state, &mut staged.temp, &staged.sha, staged.size as i64).await?;
    let vision_status = if files::vision_eligible(&mime) {
        "pending"
    } else {
        "n/a"
    };
    let outcome: Result<(), ApiError> = async {
        let mut tx = files::begin_write(&state.pool).await?;
        let current = files::require_node_conn(&mut tx, &id).await?;
        if !current.is_file() {
            return Err(ApiError::bad_request("folders do not have file content"));
        }
        if scope.restricted && !files::is_within_conn(&mut tx, &scope.root, &current.id).await? {
            return Err(ApiError::not_found("item not found"));
        }

        storage::register_blob_tx(&mut tx, &staged.sha, staged.size as i64).await?;
        sqlx::query(
            "UPDATE nodes SET blob_sha = ?, mime = ?, size = ?, updated_at = ?, \
             index_status = 'pending', index_error = NULL, index_attempts = 0, \
             text_content = NULL, metadata_json = '{}', thumb_sha = NULL, \
             vision_status = ?, vision_attempts = 0, vision_description = NULL, \
             vision_text = NULL, vision_tags = NULL, vision_model = NULL, vision_at = NULL, \
             vision_error = NULL \
             WHERE id = ?",
        )
        .bind(&staged.sha)
        .bind(&mime)
        .bind(staged.size as i64)
        .bind(crate::now())
        .bind(vision_status)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        files::upsert_fts_conn(&mut tx, &id, &current.name, None, None, Some("")).await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    match outcome {
        Ok(()) => drop(guard),
        Err(err) => {
            storage::rollback_promoted(&state, &promoted).await;
            drop(guard);
            return Err(err);
        }
    }

    // The old blob and old thumbnail are unreferenced only after the commit.
    if let Err(err) = storage::gc_unreferenced(&state).await {
        tracing::warn!(error = %err, "blob garbage collection after replace failed");
    }
    let updated = files::require_node(&state.pool, &id).await?;
    audit(&state, &user.id, "files.replace", &id).await;
    Ok(Json(json!({ "item": summary(&updated) })))
}

// ---------------------------------------------------------------- downloads

pub(crate) const DISPOSITION_ATTR_CHARS: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC
        .remove(b'!')
        .remove(b'#')
        .remove(b'$')
        .remove(b'&')
        .remove(b'+')
        .remove(b'-')
        .remove(b'.')
        .remove(b'^')
        .remove(b'_')
        .remove(b'`')
        .remove(b'|')
        .remove(b'~');

pub(crate) fn content_disposition(kind: &str, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_graphic() && ch != '"' && ch != '\\' {
                ch
            } else {
                '_'
            }
        })
        .take(180)
        .collect();
    let encoded = percent_encoding::utf8_percent_encode(name, DISPOSITION_ATTR_CHARS);
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

fn safe_inline(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/svg+xml"
        || mime == "application/pdf"
        || mime.starts_with("audio/")
        || mime.starts_with("video/")
        || matches!(
            mime,
            "text/plain" | "text/markdown" | "application/json" | "text/csv"
        )
}

#[derive(Debug, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum RangeRequest {
    /// No usable single range header; serve the full representation.
    Ignore,
    Partial(ByteRange),
    /// Syntactically valid but unsatisfiable; answer 416.
    Unsatisfiable,
}

fn parse_range(value: &str, len: u64) -> RangeRequest {
    let Some(value) = value.trim().strip_prefix("bytes=") else {
        return RangeRequest::Ignore;
    };
    if value.contains(',') {
        return RangeRequest::Ignore;
    }
    let Some((start, end)) = value.split_once('-') else {
        return RangeRequest::Ignore;
    };
    if start.is_empty() {
        let Ok(suffix) = end.trim().parse::<u64>() else {
            return RangeRequest::Ignore;
        };
        if suffix == 0 || len == 0 {
            return RangeRequest::Unsatisfiable;
        }
        let start = len.saturating_sub(suffix);
        return RangeRequest::Partial(ByteRange {
            start,
            end: len - 1,
        });
    }
    let Ok(start) = start.trim().parse::<u64>() else {
        return RangeRequest::Ignore;
    };
    if start >= len {
        return RangeRequest::Unsatisfiable;
    }
    let end = if end.trim().is_empty() {
        len - 1
    } else {
        match end.trim().parse::<u64>() {
            Ok(end) => end.min(len - 1),
            Err(_) => return RangeRequest::Ignore,
        }
    };
    if end < start {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Partial(ByteRange { start, end })
}

pub(crate) async fn serve_blob(
    state: &AppState,
    blob_sha: &str,
    name: &str,
    mime: Option<&str>,
    headers: &HeaderMap,
    force_attachment: bool,
) -> Result<Response, ApiError> {
    let path = storage::blob_path(state, blob_sha)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("stored file is missing"))?;
    let len = file.metadata().await?.len();
    let mime = mime.unwrap_or("application/octet-stream").to_string();
    let disposition = if force_attachment || !safe_inline(&mime) {
        "attachment"
    } else {
        "inline"
    };

    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| parse_range(value, len))
        .unwrap_or(RangeRequest::Ignore);

    let (status, start, length) = match range {
        RangeRequest::Partial(range) => (
            StatusCode::PARTIAL_CONTENT,
            range.start,
            range.end - range.start + 1,
        ),
        RangeRequest::Unsatisfiable => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .header(header::ACCEPT_RANGES, "bytes")
                .header("x-content-type-options", "nosniff")
                .header(header::CACHE_CONTROL, "private, max-age=0, must-revalidate")
                .body(Body::empty())
                .map_err(|err| ApiError::internal(format!("could not build response: {err}")));
        }
        RangeRequest::Ignore => (StatusCode::OK, 0, len),
    };

    let mut file = file;
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }
    let stream = ReaderStream::new(file.take(length));

    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(disposition, name),
        )
        .header(header::ACCEPT_RANGES, "bytes")
        .header("x-content-type-options", "nosniff")
        .header(header::CACHE_CONTROL, "private, max-age=0, must-revalidate");
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{len}", start + length - 1),
        );
    }
    builder
        .body(Body::from_stream(stream))
        .map_err(|err| ApiError::internal(format!("could not build response: {err}")))
}

pub(crate) async fn serve_thumbnail(
    state: &AppState,
    sha: &str,
    cache_control: &'static str,
) -> Result<Response, ApiError> {
    let path = storage::blob_path(state, sha)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("thumbnail is missing"))?;
    let len = file.metadata().await?.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, cache_control)
        .header("x-content-type-options", "nosniff")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|err| ApiError::internal(format!("could not build response: {err}")))
}

pub async fn content(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node = authorize_node(&state, &user, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request("folders do not have file content"));
    }
    let sha = node
        .blob_sha
        .as_deref()
        .ok_or_else(|| ApiError::internal("file has no stored content"))?;
    serve_blob(
        &state,
        sha,
        &node.name,
        node.mime.as_deref(),
        &headers,
        false,
    )
    .await
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node = authorize_node(&state, &user, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request(
            "folders cannot be downloaded this way",
        ));
    }
    let sha = node
        .blob_sha
        .as_deref()
        .ok_or_else(|| ApiError::internal("file has no stored content"))?;
    serve_blob(
        &state,
        sha,
        &node.name,
        node.mime.as_deref(),
        &headers,
        true,
    )
    .await
}

pub async fn thumbnail(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let node = authorize_node(&state, &user, &id).await?;
    let sha = node
        .thumb_sha
        .as_deref()
        .ok_or_else(|| ApiError::not_found("no thumbnail available"))?;
    serve_thumbnail(&state, sha, "private, max-age=86400").await
}

// ------------------------------------------------------------------- search

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn safe_snippet(raw: &str) -> String {
    escape_html(raw)
        .replace('\u{1}', "<mark>")
        .replace('\u{2}', "</mark>")
}

fn like_escape(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[derive(Debug, sqlx::FromRow)]
struct FileSearchRow {
    id: String,
    name: String,
    parent_id: String,
    mime: Option<String>,
    size: i64,
    updated_at: i64,
    path: String,
    blob_sha: Option<String>,
    text_snip: Option<String>,
    desc_snip: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct LikeSearchRow {
    id: String,
    name: String,
    parent_id: String,
    mime: Option<String>,
    size: i64,
    updated_at: i64,
    blob_sha: Option<String>,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(json!({ "results": [] })));
    }
    let fts_query = crate::routes::docs::build_fts_query(query);
    let mut results: Vec<Value> = Vec::new();

    if !fts_query.is_empty() {
        let sql = format!(
            "{SCOPE_CTE} ) , paths(id, path, visited) AS ( \
               SELECT id, '', ',' || id || ',' FROM nodes WHERE id = ? \
               UNION ALL \
               SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END, \
                      p.visited || n.id || ',' \
               FROM nodes n JOIN paths p ON n.parent_id = p.id \
               WHERE instr(p.visited, ',' || n.id || ',') = 0 AND length(p.visited) < 100000 \
             ) \
             SELECT n.id, n.name, n.parent_id, n.mime, n.size, n.updated_at, p.path, n.blob_sha, \
                    snippet(files_fts, 2, char(1), char(2), '…', 18) AS text_snip, \
                    snippet(files_fts, 3, char(1), char(2), '…', 18) AS desc_snip \
             FROM files_fts f JOIN nodes n ON n.id = f.file_id JOIN paths p ON p.id = n.id \
             WHERE files_fts MATCH ? AND n.id IN (SELECT id FROM scope) \
             ORDER BY bm25(files_fts, 0.0, 10.0, 1.0, 5.0, 2.0) LIMIT 50",
            SCOPE_CTE = files::SCOPE_CTE
        );
        let rows: Result<Vec<FileSearchRow>, sqlx::Error> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql))
                .bind(&scope.root)
                .bind(&scope.root)
                .bind(&fts_query)
                .fetch_all(&state.pool)
                .await;

        match rows {
            Ok(rows) => {
                for row in rows {
                    let snippet = row
                        .text_snip
                        .filter(|snippet| !snippet.trim().is_empty())
                        .or(row.desc_snip)
                        .map(|snippet| safe_snippet(&snippet))
                        .unwrap_or_default();
                    results.push(json!({
                        "id": row.id,
                        "kind": "file",
                        "name": row.name,
                        "parent_id": row.parent_id,
                        "mime": row.mime,
                        "size": row.size,
                        "updated_at": row.updated_at,
                        "path": row.path,
                        "snippet": snippet,
                        "sha256": row.blob_sha,
                    }));
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "file fts query failed, falling back to LIKE");
                let like = format!("%{}%", like_escape(query));
                let sql = format!(
                    "{} ) SELECT n.id, n.name, n.parent_id, n.mime, n.size, n.updated_at, n.blob_sha \
                     FROM nodes n \
                     WHERE n.kind = 'file' AND n.id IN (SELECT id FROM scope) \
                       AND (n.name LIKE ? ESCAPE '\\' OR n.vision_description LIKE ? ESCAPE '\\') \
                     ORDER BY n.updated_at DESC LIMIT 50",
                    files::SCOPE_CTE
                );
                let rows: Vec<LikeSearchRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
                    .bind(&scope.root)
                    .bind(&like)
                    .bind(&like)
                    .fetch_all(&state.pool)
                    .await?;
                for row in rows {
                    results.push(json!({
                        "id": row.id,
                        "kind": "file",
                        "name": row.name,
                        "parent_id": row.parent_id,
                        "mime": row.mime,
                        "size": row.size,
                        "updated_at": row.updated_at,
                        "path": "",
                        "snippet": "",
                        "sha256": row.blob_sha,
                    }));
                }
            }
        }
    }

    let folder_like = format!("%{}%", like_escape(query));
    let sql = format!(
        "{} ) , paths(id, path, visited) AS ( \
           SELECT id, '', ',' || id || ',' FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END, \
                  p.visited || n.id || ',' \
           FROM nodes n JOIN paths p ON n.parent_id = p.id \
           WHERE instr(p.visited, ',' || n.id || ',') = 0 AND length(p.visited) < 100000 \
         ) \
         SELECT n.id, n.name, n.parent_id, p.path FROM nodes n JOIN paths p ON p.id = n.id \
         WHERE n.kind = 'folder' AND n.id <> 'root' AND n.id <> ? \
           AND n.id IN (SELECT id FROM scope) AND n.name LIKE ? ESCAPE '\\' \
         ORDER BY n.name COLLATE NOCASE LIMIT 20",
        files::SCOPE_CTE
    );
    let folders: Vec<(String, String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&scope.root)
        .bind(&scope.root)
        .bind(&scope.root)
        .bind(&folder_like)
        .fetch_all(&state.pool)
        .await?;
    let folder_results: Vec<Value> = folders
        .into_iter()
        .map(|(id, name, parent_id, path)| {
            json!({
                "id": id,
                "kind": "folder",
                "name": name,
                "parent_id": parent_id,
                "mime": null,
                "size": 0,
                "updated_at": 0,
                "path": path,
                "snippet": "",
            })
        })
        .collect();
    let mut combined = folder_results;
    combined.append(&mut results);
    Ok(Json(json!({ "results": combined })))
}

// -------------------------------------------------------------------- stats

pub async fn stats(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    let scope = Scope::for_user(&user)?;
    let counts_sql = format!(
        "{} ) SELECT \
           (SELECT COUNT(*) FROM nodes n JOIN scope s ON s.id = n.id WHERE n.kind = 'file'), \
           (SELECT COUNT(*) FROM nodes n JOIN scope s ON s.id = n.id \
              WHERE n.kind = 'folder' AND n.id <> 'root'), \
           (SELECT COUNT(*) FROM nodes n JOIN scope s ON s.id = n.id \
              WHERE n.kind = 'file' AND n.mime LIKE 'image/%')",
        files::SCOPE_CTE
    );
    let (files, folders, images): (i64, i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(counts_sql))
        .bind(&scope.root)
        .fetch_one(&state.pool)
        .await?;
    let bytes_sql = format!(
        "{} ) SELECT COALESCE(SUM(b.size), 0) FROM blobs b \
         WHERE EXISTS ( \
           SELECT 1 FROM nodes n JOIN scope s ON s.id = n.id WHERE n.blob_sha = b.sha256 \
         )",
        files::SCOPE_CTE
    );
    let (bytes,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(bytes_sql))
        .bind(&scope.root)
        .fetch_one(&state.pool)
        .await?;
    let shares_sql = format!(
        "{} ) SELECT COUNT(*) FROM shares \
         WHERE revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?) \
           AND target_id IN (SELECT id FROM scope)",
        files::SCOPE_CTE
    );
    let (active_shares,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(shares_sql))
        .bind(&scope.root)
        .bind(crate::now())
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(json!({
        "files": files,
        "folders": folders,
        "images": images,
        "bytes": bytes,
        "active_shares": active_shares,
    })))
}

// ------------------------------------------------------------------- retries

pub async fn retry_vision(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let node = authorize_node(&state, &user, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request("only images can be described"));
    }
    let mime = node.mime.as_deref().unwrap_or("");
    if !mime.starts_with("image/") {
        return Err(ApiError::bad_request("only images can be described"));
    }
    if !files::vision_eligible(mime) {
        return Err(ApiError::bad_request(
            "this image format cannot be described automatically",
        ));
    }
    sqlx::query(
        "UPDATE nodes SET vision_status = 'pending', vision_error = NULL, vision_attempts = 0 \
         WHERE id = ?",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn retry_index(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let node = authorize_node(&state, &user, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request("only files can be reindexed"));
    }
    sqlx::query(
        "UPDATE nodes SET index_status = 'pending', index_error = NULL, index_attempts = 0 \
         WHERE id = ?",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ranges() {
        let range = parse_range("bytes=0-99", 1000);
        assert_eq!(
            range,
            RangeRequest::Partial(ByteRange { start: 0, end: 99 })
        );
        let range = parse_range("bytes=900-", 1000);
        assert_eq!(
            range,
            RangeRequest::Partial(ByteRange {
                start: 900,
                end: 999
            })
        );
        let range = parse_range("bytes=-100", 1000);
        assert_eq!(
            range,
            RangeRequest::Partial(ByteRange {
                start: 900,
                end: 999
            })
        );
        assert_eq!(
            parse_range("bytes=1000-", 1000),
            RangeRequest::Unsatisfiable
        );
        assert_eq!(parse_range("bytes=-0", 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=5-2", 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range("nonsense", 1000), RangeRequest::Ignore);
        assert_eq!(parse_range("bytes=0-1,4-5", 1000), RangeRequest::Ignore);
    }

    #[test]
    fn content_disposition_encodes_unicode() {
        let value = content_disposition("attachment", "résumé.pdf");
        assert!(value.contains("filename*=UTF-8''"));
        assert!(value.contains("r%C3%A9sum%C3%A9.pdf"));
    }

    #[test]
    fn snippets_escape_html_but_keep_marks() {
        let value = safe_snippet("<script>\u{1}hello\u{2}</script>");
        assert!(value.contains("&lt;script&gt;"));
        assert!(value.contains("<mark>hello</mark>"));
    }
}
