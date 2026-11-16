use crate::{
    auth::AuthUser,
    error::ApiError,
    files::{self, extract, storage, Node, ROOT_ID},
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
}

type RecentRow = (String, String, String, Option<String>, i64, i64, String);
type FileSearchRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
);

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

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let folder_id = folder_or_root(&params.folder_id);
    let folder = files::require_folder(&state.pool, &folder_id).await?;
    let rows = sqlx::query_as::<_, Node>(
        "SELECT * FROM nodes WHERE parent_id = ? \
         ORDER BY CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name COLLATE NOCASE",
    )
    .bind(&folder_id)
    .fetch_all(&state.pool)
    .await?;

    let breadcrumbs: Vec<Value> = files::breadcrumbs(&state.pool, &folder_id)
        .await?
        .into_iter()
        .map(|crumb| {
            json!({
                "id": crumb.id,
                "name": display_folder_name(&crumb.id, &crumb.name),
            })
        })
        .collect();

    Ok(Json(json!({
        "folder": {
            "id": folder.id,
            "name": display_folder_name(&folder.id, &folder.name),
            "parent_id": folder.parent_id,
        },
        "breadcrumbs": breadcrumbs,
        "items": rows.iter().map(summary).collect::<Vec<_>>(),
    })))
}

pub async fn recent(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<RecentRow> = sqlx::query_as(
        "WITH RECURSIVE paths(id, path) AS ( \
           SELECT id, '' FROM nodes WHERE id = 'root' \
           UNION ALL \
           SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END \
           FROM nodes n JOIN paths p ON n.parent_id = p.id \
         ) \
         SELECT n.id, n.name, n.parent_id, n.mime, n.size, n.updated_at, p.path \
         FROM nodes n JOIN paths p ON p.id = n.id \
         WHERE n.kind = 'file' ORDER BY n.updated_at DESC, n.name LIMIT 50",
    )
    .fetch_all(&state.pool)
    .await?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, parent_id, mime, size, updated_at, path)| {
            json!({
                "id": id,
                "name": name,
                "parent_id": parent_id,
                "mime": mime,
                "size": size,
                "updated_at": updated_at,
                "path": path,
                "kind": "file",
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
    let parent_id = folder_or_root(&req.parent_id);
    files::require_folder(&state.pool, &parent_id).await?;
    let name = files::validate_name(&req.name)?;
    let unique = files::unique_child_name(&state.pool, &parent_id, &name, None).await?;
    if unique != name {
        return Err(ApiError::conflict(
            "a folder or file with that name already exists",
        ));
    }
    let node = files::insert_node(&state.pool, &parent_id, "folder", &name, Some(&user.id)).await?;
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
    Path(id): Path<String>,
    Json(req): Json<UpdateNodeRequest>,
) -> Result<Json<Value>, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
    if node.id == ROOT_ID {
        return Err(ApiError::bad_request(
            "the drive root cannot be renamed or moved",
        ));
    }

    let new_parent = req
        .parent_id
        .clone()
        .unwrap_or_else(|| node.parent_id.clone());
    if req.parent_id.is_some() {
        files::require_folder(&state.pool, &new_parent).await?;
        if node.is_folder() && files::is_within(&state.pool, &node.id, &new_parent).await? {
            return Err(ApiError::bad_request(
                "a folder cannot be moved inside itself",
            ));
        }
        if new_parent == node.parent_id {
            // no-op
        }
    }

    let new_name = match req.name.as_deref() {
        Some(raw) => files::validate_name(raw)?,
        None => node.name.clone(),
    };
    let unique =
        files::unique_child_name(&state.pool, &new_parent, &new_name, Some(&node.id)).await?;
    if unique != new_name {
        return Err(ApiError::conflict(
            "a folder or file with that name already exists",
        ));
    }

    sqlx::query("UPDATE nodes SET name = ?, parent_id = ?, updated_at = ? WHERE id = ?")
        .bind(&new_name)
        .bind(&new_parent)
        .bind(crate::now())
        .bind(&node.id)
        .execute(&state.pool)
        .await?;

    let updated = files::require_node(&state.pool, &node.id).await?;
    if updated.is_file() {
        let description = updated.vision_description.as_deref();
        let metadata = crate::files::vision::metadata_search_text(&updated);
        files::upsert_fts(
            &state.pool,
            &updated,
            updated.text_content.as_deref(),
            description,
            Some(&metadata),
        )
        .await?;
    }
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
    let node = files::require_node(&state.pool, &id).await?;
    let ids = files::subtree_ids(&state.pool, &node.id).await?;

    sqlx::query(
        "WITH RECURSIVE subtree(id) AS ( \
           SELECT id FROM nodes WHERE id = ? \
           UNION ALL SELECT n.id FROM nodes n JOIN subtree s ON n.parent_id = s.id \
         ) \
         DELETE FROM shares WHERE target_id IN (SELECT id FROM subtree)",
    )
    .bind(&node.id)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "WITH RECURSIVE subtree(id) AS ( \
           SELECT id FROM nodes WHERE id = ? \
           UNION ALL SELECT n.id FROM nodes n JOIN subtree s ON n.parent_id = s.id \
         ) \
         DELETE FROM files_fts WHERE file_id IN (SELECT id FROM subtree)",
    )
    .bind(&node.id)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "WITH RECURSIVE subtree(id) AS ( \
           SELECT id FROM nodes WHERE id = ? \
           UNION ALL SELECT n.id FROM nodes n JOIN subtree s ON n.parent_id = s.id \
         ) \
         DELETE FROM nodes WHERE id IN (SELECT id FROM subtree)",
    )
    .bind(&node.id)
    .execute(&state.pool)
    .await?;

    storage::gc_unreferenced(&state.pool, &state).await?;
    audit(&state, &user.id, "files.delete", &id).await;
    Ok(Json(json!({ "deleted": id, "count": ids.len() })))
}

pub async fn get_file(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
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
    let node_shares = shares::list_for_node(&state, &node.id).await?;

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

pub async fn upload(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Query(params): Query<ListQuery>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    let folder_id = folder_or_root(&params.folder_id);
    files::require_folder(&state.pool, &folder_id).await?;
    storage::ensure_dirs(&state).await?;

    let mut uploaded = Vec::new();
    let mut failures = Vec::new();
    let mut file_count = 0usize;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|err| ApiError::bad_request(format!("invalid upload: {err}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        file_count += 1;
        if file_count > 100 {
            return Err(ApiError::payload_too_large("at most 100 files per upload"));
        }

        let raw_name = field
            .file_name()
            .ok_or_else(|| ApiError::bad_request("uploaded file has no filename"))?;
        let name = match files::validate_name(raw_name) {
            Ok(name) => name,
            Err(err) => {
                failures.push(json!({ "name": raw_name, "error": err.message }));
                continue;
            }
        };

        let temp_path = state
            .cfg
            .upload_tmp_dir()
            .join(format!("upload-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut head = Vec::with_capacity(8192);
        let mut failure: Option<ApiError> = None;

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|err| ApiError::bad_request(format!("upload stream failed: {err}")))?
        {
            size += chunk.len() as u64;
            if size > state.cfg.max_upload_bytes {
                failure = Some(ApiError::payload_too_large(format!(
                    "file exceeds the {} MiB upload limit",
                    state.cfg.max_upload_bytes / (1024 * 1024)
                )));
                break;
            }
            if head.len() < 8192 {
                let take = (8192 - head.len()).min(chunk.len());
                head.extend_from_slice(&chunk[..take]);
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        if let Some(err) = failure {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
        file.flush().await?;
        drop(file);

        if size == 0 {
            let _ = tokio::fs::remove_file(&temp_path).await;
            failures.push(json!({ "name": name, "error": "file is empty" }));
            continue;
        }

        let sha = hex::encode(hasher.finalize());
        let mime = extract::detect_mime(&name, &head);
        storage::promote_temp(&state, &temp_path, &sha, size as i64).await?;

        let unique_name = files::unique_child_name(&state.pool, &folder_id, &name, None).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let ts = crate::now();
        let vision_status = if mime.starts_with("image/") && mime != "image/svg+xml" {
            "pending"
        } else {
            "n/a"
        };
        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, owner_id, created_at, updated_at, \
             blob_sha, mime, size, index_status, vision_status, metadata_json) \
             VALUES (?, ?, 'file', ?, ?, ?, ?, ?, ?, ?, 'pending', ?, '{}')",
        )
        .bind(&id)
        .bind(&folder_id)
        .bind(&unique_name)
        .bind(&user.id)
        .bind(ts)
        .bind(ts)
        .bind(&sha)
        .bind(&mime)
        .bind(size as i64)
        .bind(vision_status)
        .execute(&state.pool)
        .await?;

        let node = files::require_node(&state.pool, &id).await?;
        files::upsert_fts(&state.pool, &node, None, None, Some("")).await?;
        audit(
            &state,
            &user.id,
            "files.upload",
            &format!("{unique_name} ({sha})"),
        )
        .await;
        uploaded.push(summary(&node));
    }

    if uploaded.is_empty() && failures.is_empty() {
        return Err(ApiError::bad_request("no files were uploaded"));
    }
    Ok(Json(json!({ "uploaded": uploaded, "failed": failures })))
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

struct ByteRange {
    start: u64,
    end: u64,
}

fn parse_range(value: &str, len: u64) -> Option<ByteRange> {
    let value = value.trim().strip_prefix("bytes=")?;
    if value.contains(',') {
        return None;
    }
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.trim().parse().ok()?;
        if suffix == 0 || len == 0 {
            return None;
        }
        let start = len.saturating_sub(suffix);
        return Some(ByteRange {
            start,
            end: len - 1,
        });
    }
    let start: u64 = start.trim().parse().ok()?;
    if start >= len {
        return None;
    }
    let end = if end.trim().is_empty() {
        len - 1
    } else {
        end.trim().parse::<u64>().ok()?.min(len - 1)
    };
    if end < start {
        return None;
    }
    Some(ByteRange { start, end })
}

pub(crate) async fn serve_node(
    state: &AppState,
    node: &Node,
    headers: &HeaderMap,
    force_attachment: bool,
) -> Result<Response, ApiError> {
    let path = node.blob_path(state)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("stored file is missing"))?;
    let len = file.metadata().await?.len();
    let mime = node
        .mime
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let disposition = if force_attachment || !safe_inline(&mime) {
        "attachment"
    } else {
        "inline"
    };

    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| parse_range(value, len));

    let (status, start, length) = match range {
        Some(range) => (
            StatusCode::PARTIAL_CONTENT,
            range.start,
            range.end - range.start + 1,
        ),
        None => (StatusCode::OK, 0, len),
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
            content_disposition(disposition, &node.name),
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

pub async fn content(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request("folders do not have file content"));
    }
    serve_node(&state, &node, &headers, false).await
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request(
            "folders cannot be downloaded this way",
        ));
    }
    serve_node(&state, &node, &headers, true).await
}

pub async fn thumbnail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
    let sha = node
        .thumb_sha
        .as_deref()
        .ok_or_else(|| ApiError::not_found("no thumbnail available"))?;
    let path = storage::blob_path(&state, sha)?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::not_found("thumbnail is missing"))?;
    let len = file.metadata().await?.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, "private, max-age=86400")
        .header("x-content-type-options", "nosniff")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|err| ApiError::internal(format!("could not build response: {err}")))
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

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(json!({ "results": [] })));
    }
    let fts_query = crate::routes::docs::build_fts_query(query);
    let mut results: Vec<Value> = Vec::new();

    if !fts_query.is_empty() {
        let rows: Result<Vec<FileSearchRow>, sqlx::Error> = sqlx::query_as(
            "WITH RECURSIVE paths(id, path) AS ( \
               SELECT id, '' FROM nodes WHERE id = 'root' \
               UNION ALL \
               SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END \
               FROM nodes n JOIN paths p ON n.parent_id = p.id \
             ) \
             SELECT n.id, n.name, n.parent_id, n.mime, n.size, n.updated_at, p.path, \
                    snippet(files_fts, 2, char(1), char(2), '…', 18), \
                    snippet(files_fts, 3, char(1), char(2), '…', 18) \
             FROM files_fts f JOIN nodes n ON n.id = f.file_id JOIN paths p ON p.id = n.id \
             WHERE files_fts MATCH ? \
             ORDER BY bm25(files_fts, 0.0, 10.0, 1.0, 5.0, 2.0) LIMIT 50",
        )
        .bind(&fts_query)
        .fetch_all(&state.pool)
        .await;

        match rows {
            Ok(rows) => {
                for (id, name, parent_id, mime, size, updated_at, path, text_snip, desc_snip) in
                    rows
                {
                    let snippet = text_snip
                        .filter(|s| !s.trim().is_empty())
                        .or(desc_snip)
                        .map(|s| safe_snippet(&s))
                        .unwrap_or_default();
                    results.push(json!({
                        "id": id,
                        "kind": "file",
                        "name": name,
                        "parent_id": parent_id,
                        "mime": mime,
                        "size": size,
                        "updated_at": updated_at,
                        "path": path,
                        "snippet": snippet,
                    }));
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "file fts query failed, falling back to LIKE");
                let like = format!("%{}%", like_escape(query));
                let rows: Vec<(String, String, String, Option<String>, i64, i64)> =
                    sqlx::query_as(
                        "SELECT id, name, parent_id, mime, size, updated_at FROM nodes \
                         WHERE kind = 'file' AND (name LIKE ? ESCAPE '\\' OR vision_description LIKE ? ESCAPE '\\') \
                         ORDER BY updated_at DESC LIMIT 50",
                    )
                    .bind(&like)
                    .bind(&like)
                    .fetch_all(&state.pool)
                    .await?;
                for (id, name, parent_id, mime, size, updated_at) in rows {
                    results.push(json!({
                        "id": id,
                        "kind": "file",
                        "name": name,
                        "parent_id": parent_id,
                        "mime": mime,
                        "size": size,
                        "updated_at": updated_at,
                        "path": "",
                        "snippet": "",
                    }));
                }
            }
        }
    }

    let folder_like = format!("%{}%", like_escape(query));
    let folders: Vec<(String, String, String, String)> = sqlx::query_as(
        "WITH RECURSIVE paths(id, path) AS ( \
           SELECT id, '' FROM nodes WHERE id = 'root' \
           UNION ALL \
           SELECT n.id, CASE WHEN p.path = '' THEN n.name ELSE p.path || '/' || n.name END \
           FROM nodes n JOIN paths p ON n.parent_id = p.id \
         ) \
         SELECT n.id, n.name, n.parent_id, p.path FROM nodes n JOIN paths p ON p.id = n.id \
         WHERE n.kind = 'folder' AND n.name LIKE ? ESCAPE '\\' AND n.id <> 'root' \
         ORDER BY n.name COLLATE NOCASE LIMIT 20",
    )
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

pub async fn stats(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let (files, folders, images): (i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM nodes WHERE kind = 'file'), \
           (SELECT COUNT(*) FROM nodes WHERE kind = 'folder' AND id <> 'root'), \
           (SELECT COUNT(*) FROM nodes WHERE kind = 'file' AND mime LIKE 'image/%')",
    )
    .fetch_one(&state.pool)
    .await?;
    let (bytes,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(b.size), 0) FROM blobs b \
         WHERE EXISTS (SELECT 1 FROM nodes n WHERE n.blob_sha = b.sha256)",
    )
    .fetch_one(&state.pool)
    .await?;
    let (active_shares,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM shares WHERE revoked_at IS NULL \
         AND (expires_at IS NULL OR expires_at > ?)",
    )
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
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
    if !node.is_file() || !node.mime.as_deref().unwrap_or("").starts_with("image/") {
        return Err(ApiError::bad_request("only images can be described"));
    }
    sqlx::query("UPDATE nodes SET vision_status = 'pending', vision_error = NULL WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn retry_index(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let node = files::require_node(&state.pool, &id).await?;
    if !node.is_file() {
        return Err(ApiError::bad_request("only files can be reindexed"));
    }
    sqlx::query("UPDATE nodes SET index_status = 'pending', index_error = NULL WHERE id = ?")
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
        let range = parse_range("bytes=0-99", 1000).unwrap();
        assert_eq!((range.start, range.end), (0, 99));
        let range = parse_range("bytes=900-", 1000).unwrap();
        assert_eq!((range.start, range.end), (900, 999));
        let range = parse_range("bytes=-100", 1000).unwrap();
        assert_eq!((range.start, range.end), (900, 999));
        assert!(parse_range("bytes=1000-", 1000).is_none());
        assert!(parse_range("nonsense", 1000).is_none());
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
