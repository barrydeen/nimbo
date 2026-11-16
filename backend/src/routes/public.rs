use crate::{
    db,
    error::ApiError,
    files::{storage, Node},
    routes::{
        files::{content_disposition, serve_node},
        shares::{self, ActiveShare},
    },
    state::AppState,
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue},
    response::Response,
    Json,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

const MAX_ZIP_ENTRIES: usize = 10_000;
const MAX_ZIP_BYTES: u64 = 4 * 1024 * 1024 * 1024;

type ZipRow = (String, String, Option<String>, i64, Option<String>);

#[derive(Deserialize)]
pub struct FolderQuery {
    pub folder_id: Option<String>,
}

fn public_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

async fn children(
    state: &AppState,
    share: &ActiveShare,
    folder: &Node,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query_as::<_, Node>(
        "SELECT * FROM nodes WHERE parent_id = ? \
         ORDER BY CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name COLLATE NOCASE",
    )
    .bind(&folder.id)
    .fetch_all(&state.pool)
    .await?;
    let _ = share;
    Ok(rows.iter().map(shares::public_summary).collect())
}

pub async fn share(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let active = shares::load_active(&state, &token).await?;
    let instance_name = db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into());
    let _ = shares::bump_access(&state, &active.share).await;

    if active.target.is_file() {
        return Ok(Json(json!({
            "token": token,
            "instance_name": instance_name,
            "kind": "file",
            "name": active.target.name,
            "expires_at": active.share.expires_at,
            "file": shares::public_summary(&active.target),
        })));
    }

    let items = children(&state, &active, &active.target).await?;
    let breadcrumbs = shares::public_breadcrumbs(&state, &active, &active.target.id).await?;
    Ok(Json(json!({
        "token": token,
        "instance_name": instance_name,
        "kind": "folder",
        "name": active.target.name,
        "expires_at": active.share.expires_at,
        "folder": {
            "id": active.target.id,
            "name": active.target.name,
            "parent_id": active.target.parent_id,
        },
        "breadcrumbs": breadcrumbs,
        "items": items,
    })))
}

pub async fn nodes(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<FolderQuery>,
) -> Result<Json<Value>, ApiError> {
    let active = shares::load_active(&state, &token).await?;
    let folder = shares::contained_folder(&state, &active, query.folder_id.as_deref()).await?;
    let items = children(&state, &active, &folder).await?;
    let breadcrumbs = shares::public_breadcrumbs(&state, &active, &folder.id).await?;
    Ok(Json(json!({
        "folder": {
            "id": folder.id,
            "name": folder.name,
            "parent_id": folder.parent_id,
        },
        "breadcrumbs": breadcrumbs,
        "items": items,
    })))
}

async fn public_file(
    state: &AppState,
    token: &str,
    id: &str,
) -> Result<(ActiveShare, Node), ApiError> {
    let active = shares::load_active(state, token).await?;
    let node = shares::contained_node(state, &active, id).await?;
    if !node.is_file() {
        return Err(ApiError::not_found("file not found"));
    }
    Ok((active, node))
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (_active, node) = public_file(&state, &token, &id).await?;
    let mut response = serve_node(&state, &node, &headers, true).await?;
    public_headers(&mut response);
    Ok(response)
}

pub async fn content(
    State(state): State<Arc<AppState>>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (_active, node) = public_file(&state, &token, &id).await?;
    let mut response = serve_node(&state, &node, &headers, false).await?;
    public_headers(&mut response);
    Ok(response)
}

pub async fn thumbnail(
    State(state): State<Arc<AppState>>,
    Path((token, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (_active, node) = public_file(&state, &token, &id).await?;
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
        .status(200)
        .header(header::CONTENT_TYPE, "image/jpeg")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|err| ApiError::internal(format!("could not build response: {err}")))
}

struct ZipFileEntry {
    zip_name: String,
    blob_sha: String,
    mime: String,
    size: i64,
}

/// Deletes a staged download file when the response stream is dropped,
/// including when the client disconnects mid-transfer.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Builds a zip in a temp file. Returns the file size.
fn build_zip(
    blobs_dir: PathBuf,
    temp_path: PathBuf,
    root_name: String,
    entries: Vec<ZipFileEntry>,
) -> Result<u64, String> {
    let file = std::fs::File::create(&temp_path).map_err(|err| err.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let root = root_name.replace(['/', '\\', '\0'], "_").trim().to_string();
    let root = if root.is_empty() {
        "shared".to_string()
    } else {
        root
    };

    for entry in entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(
                if entry.mime.starts_with("image/")
                    || entry.mime.starts_with("video/")
                    || entry.mime.starts_with("audio/")
                    || entry.mime == "application/pdf"
                {
                    zip::CompressionMethod::Stored
                } else {
                    zip::CompressionMethod::Deflated
                },
            )
            .large_file(true);
        let name = format!("{}/{}", root, entry.zip_name);
        zip.start_file(name, options)
            .map_err(|err| err.to_string())?;
        let path =
            storage::blob_path_under(&blobs_dir, &entry.blob_sha).map_err(|err| err.message)?;
        let mut source = std::fs::File::open(&path).map_err(|err| err.to_string())?;
        let mut wrote = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            use std::io::Read;
            let read = source.read(&mut buffer).map_err(|err| err.to_string())?;
            if read == 0 {
                break;
            }
            zip.write_all(&buffer[..read])
                .map_err(|err| err.to_string())?;
            wrote += read as u64;
            if wrote > entry.size as u64 + 1 {
                return Err("stored file size does not match metadata".into());
            }
        }
    }

    let file = zip.finish().map_err(|err| err.to_string())?;
    file.metadata()
        .map(|meta| meta.len())
        .map_err(|err| err.to_string())
}

pub async fn download_zip(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<FolderQuery>,
) -> Result<Response, ApiError> {
    let active = shares::load_active(&state, &token).await?;
    let folder = shares::contained_folder(&state, &active, query.folder_id.as_deref()).await?;

    let rows: Vec<ZipRow> = sqlx::query_as(
        "WITH RECURSIVE subtree(id, rel) AS ( \
           SELECT id, '' FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, CASE WHEN s.rel = '' THEN n.name ELSE s.rel || '/' || n.name END \
           FROM nodes n JOIN subtree s ON n.parent_id = s.id \
         ) \
         SELECT n.id, s.rel, n.blob_sha, n.size, n.mime \
         FROM nodes n JOIN subtree s ON s.id = n.id WHERE n.kind = 'file'",
    )
    .bind(&folder.id)
    .fetch_all(&state.pool)
    .await?;

    if rows.len() > MAX_ZIP_ENTRIES {
        return Err(ApiError::payload_too_large(
            "folder has too many files for a single zip",
        ));
    }
    let total: u64 = rows.iter().map(|row| row.3.max(0) as u64).sum();
    if total > MAX_ZIP_BYTES {
        return Err(ApiError::payload_too_large(
            "folder is too large for a single zip download",
        ));
    }

    let mut entries = Vec::with_capacity(rows.len());
    for (id, zip_name, blob_sha, size, mime) in rows {
        let blob_sha =
            blob_sha.ok_or_else(|| ApiError::internal(format!("file {id} has no blob")))?;
        entries.push(ZipFileEntry {
            zip_name,
            blob_sha,
            mime: mime.unwrap_or_else(|| "application/octet-stream".into()),
            size,
        });
    }

    tokio::fs::create_dir_all(state.cfg.upload_tmp_dir()).await?;
    let temp_path = state
        .cfg
        .upload_tmp_dir()
        .join(format!("share-{}.zip", uuid::Uuid::new_v4()));
    let blobs_dir = state.cfg.blobs_dir();
    let root_name = folder.name.clone();
    let build_path = temp_path.clone();
    let size =
        tokio::task::spawn_blocking(move || build_zip(blobs_dir, build_path, root_name, entries))
            .await
            .map_err(|err| ApiError::internal(format!("zip task failed: {err}")))?
            .map_err(|err| {
                let _ = std::fs::remove_file(&temp_path);
                ApiError::internal(format!("could not build zip: {err}"))
            })?;

    let file = tokio::fs::File::open(&temp_path).await?;
    let reader = ReaderStream::new(file);
    let cleanup = RemoveOnDrop(temp_path);
    let stream = async_stream::stream! {
        let _cleanup = cleanup;
        let mut reader = reader;
        while let Some(chunk) = reader.next().await {
            yield chunk;
        }
    };
    let download_name = format!("{}.zip", folder.name);
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_LENGTH, size.to_string())
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition("attachment", &download_name),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Body::from_stream(stream))
        .map_err(|err| ApiError::internal(format!("could not build response: {err}")))
}
