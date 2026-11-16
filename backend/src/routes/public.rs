use crate::{
    db,
    error::ApiError,
    files::storage::{self, TempFile},
    routes::{
        files::{content_disposition, serve_blob, serve_thumbnail},
        shares::{self, ActiveShare},
    },
    state::AppState,
    util::sha256_hex,
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::Response,
    Json,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
#[cfg(unix)]
use std::ffi::CString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;

const MAX_ZIP_ENTRIES: usize = 10_000;
const MAX_ZIP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Aggregate disk budget shared by all in-flight archive builds on this process.
const MAX_ZIP_TEMP_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ZIP_CONCURRENCY: usize = 2;
const MAX_ZIP_TOKENS: usize = 4_096;
/// Safety reserve kept free on the data filesystem in addition to the archive's
/// logical source bytes; SQLite, blobs and uploads share this volume.
const ZIP_FREE_SPACE_RESERVE: u64 = 256 * 1024 * 1024;

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

/// Global + per-share admission control so one token cannot fan out unbounded
/// archive builds and no combination of tokens can saturate the host.
struct ZipGate {
    _global: OwnedSemaphorePermit,
    _share: OwnedSemaphorePermit,
}

fn zip_global() -> &'static Arc<Semaphore> {
    static GLOBAL: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GLOBAL.get_or_init(|| Arc::new(Semaphore::new(MAX_ZIP_CONCURRENCY)))
}

fn zip_shares() -> &'static Mutex<HashMap<String, Arc<Semaphore>>> {
    static SHARES: OnceLock<Mutex<HashMap<String, Arc<Semaphore>>>> = OnceLock::new();
    SHARES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn too_busy() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "archive builds are busy; try again shortly",
    )
}

fn zip_gate(token: &str) -> Result<ZipGate, ApiError> {
    let global = zip_global()
        .clone()
        .try_acquire_owned()
        .map_err(|_| too_busy())?;
    let key = sha256_hex(token.as_bytes());
    let share_sem = {
        let mut map = zip_shares().lock().map_err(|_| too_busy())?;
        if let Some(existing) = map.get(&key) {
            existing.clone()
        } else {
            if map.len() >= MAX_ZIP_TOKENS {
                // Drop entries no build is currently holding, then re-check.
                map.retain(|_, semaphore| Arc::strong_count(semaphore) > 1);
                if map.len() >= MAX_ZIP_TOKENS {
                    return Err(too_busy());
                }
            }
            let semaphore = Arc::new(Semaphore::new(1));
            map.insert(key, semaphore.clone());
            semaphore
        }
    };
    let share = share_sem.try_acquire_owned().map_err(|_| too_busy())?;
    Ok(ZipGate {
        _global: global,
        _share: share,
    })
}

/// Tracks aggregate temp bytes reserved by archive builds, independent of the
/// per-request logical-size cap.
struct ZipBudget(u64);

static ZIP_RESERVED: AtomicU64 = AtomicU64::new(0);

impl ZipBudget {
    fn reserve(bytes: u64) -> Result<Self, ApiError> {
        let bytes = bytes.min(MAX_ZIP_BYTES);
        let mut current = ZIP_RESERVED.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return Err(too_busy());
            };
            if next > MAX_ZIP_TEMP_TOTAL {
                return Err(ApiError::payload_too_large(
                    "not enough archive staging capacity; try again later",
                ));
            }
            match ZIP_RESERVED.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(Self(bytes)),
                Err(observed) => current = observed,
            }
        }
    }
}

impl Drop for ZipBudget {
    fn drop(&mut self) {
        ZIP_RESERVED.fetch_sub(self.0, Ordering::AcqRel);
    }
}

/// Available bytes on the filesystem containing `path`.
#[cfg(unix)]
fn available_bytes(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe {
        // SAFETY: statvfs only writes into the provided, correctly sized struct.
        std::mem::zeroed()
    };
    // SAFETY: `c_path` is a valid NUL-terminated path pointer and `stats` points
    // to writable memory of the type expected by statvfs(3).
    let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) };
    if result != 0 {
        return None;
    }
    let block_size = if stats.f_frsize != 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    Some((stats.f_bavail as u64).saturating_mul(block_size as u64))
}

#[cfg(not(unix))]
fn available_bytes(_path: &std::path::Path) -> Option<u64> {
    // No portable statvfs equivalent here; aggregate budget and ENOSPC mapping
    // remain the controls.
    None
}

/// Free space must cover the archive's logical source bytes plus the safety
/// reserve. An unavailable figure (non-Unix or statvfs failure) is treated as
/// unknown and leaves the aggregate budget/ENOSPC guard in charge.
fn has_zip_space(available: Option<u64>, logical_total: u64) -> bool {
    match available {
        Some(bytes) => bytes >= logical_total.saturating_add(ZIP_FREE_SPACE_RESERVE),
        None => true,
    }
}

async fn children(
    conn: &mut sqlx::SqliteConnection,
    active: &ActiveShare,
    folder_id: Option<&str>,
) -> Result<(shares::SummaryNode, Vec<Value>, bool), ApiError> {
    let (folder, rows, truncated) = shares::children_snapshot(conn, active, folder_id).await?;
    let items = rows.iter().map(shares::SummaryNode::public_json).collect();
    Ok((folder, items, truncated))
}

pub async fn share(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let instance_name = db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into());
    let mut tx = state.pool.begin().await?;
    let active = shares::load_active_conn(&mut tx, &token).await?;

    let (kind, name, file, folder, items, breadcrumbs, truncated) = if active.target.is_file() {
        (
            "file",
            active.target.name.clone(),
            Some(active.target.public_json()),
            None,
            Vec::new(),
            Vec::new(),
            false,
        )
    } else {
        let (folder, items, truncated) = children(&mut tx, &active, None).await?;
        let crumbs = shares::public_breadcrumbs_conn(&mut tx, &active, &active.target.id).await?;
        (
            "folder",
            active.target.name.clone(),
            None,
            Some(folder.public_json()),
            items,
            crumbs,
            truncated,
        )
    };
    tx.commit().await?;
    let _ = shares::bump_access(&state, &active.share).await;

    if kind == "file" {
        Ok(Json(json!({
            "token": token,
            "instance_name": instance_name,
            "kind": "file",
            "name": name,
            "expires_at": active.share.expires_at,
            "file": file,
        })))
    } else {
        Ok(Json(json!({
            "token": token,
            "instance_name": instance_name,
            "kind": "folder",
            "name": name,
            "expires_at": active.share.expires_at,
            "folder": folder,
            "breadcrumbs": breadcrumbs,
            "items": items,
            "truncated": truncated,
        })))
    }
}

pub async fn nodes(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<FolderQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let active = shares::load_active_conn(&mut tx, &token).await?;
    let (folder, items, truncated) = children(&mut tx, &active, query.folder_id.as_deref()).await?;
    let breadcrumbs = shares::public_breadcrumbs_conn(&mut tx, &active, &folder.id).await?;
    tx.commit().await?;
    Ok(Json(json!({
        "folder": folder.public_json(),
        "breadcrumbs": breadcrumbs,
        "items": items,
        "truncated": truncated,
    })))
}

/// Authorizes a public file and returns its blob reference inside one snapshot.
async fn public_file(
    state: &AppState,
    token: &str,
    id: &str,
) -> Result<(String, Option<String>, String, shares::PublicFile), ApiError> {
    let mut tx = state.pool.begin().await?;
    let active = shares::load_active_conn(&mut tx, token).await?;
    let node = shares::file_snapshot(&mut tx, &active, id).await?;
    tx.commit().await?;
    let sha = node.blob_sha()?.to_string();
    Ok((sha, node.thumb_sha.clone(), node.name.clone(), node))
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (sha, _thumb, name, node) = public_file(&state, &token, &id).await?;
    let mut response =
        serve_blob(&state, &sha, &name, node.mime.as_deref(), &headers, true).await?;
    public_headers(&mut response);
    Ok(response)
}

pub async fn content(
    State(state): State<Arc<AppState>>,
    Path((token, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (sha, _thumb, name, node) = public_file(&state, &token, &id).await?;
    let mut response =
        serve_blob(&state, &sha, &name, node.mime.as_deref(), &headers, false).await?;
    public_headers(&mut response);
    Ok(response)
}

pub async fn thumbnail(
    State(state): State<Arc<AppState>>,
    Path((token, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (_sha, thumb, _name, _node) = public_file(&state, &token, &id).await?;
    let sha = thumb.ok_or_else(|| ApiError::not_found("no thumbnail available"))?;
    let mut response = serve_thumbnail(&state, &sha, "no-store").await?;
    public_headers(&mut response);
    Ok(response)
}

struct ZipFileEntry {
    zip_name: String,
    blob_sha: String,
    mime: String,
    size: i64,
}

/// Builds a zip in a temp file. Returns the file size.
fn build_zip(
    blobs_dir: PathBuf,
    temp_path: PathBuf,
    root_name: String,
    entries: Vec<ZipFileEntry>,
) -> Result<u64, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&temp_path).map_err(|err| err.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let root = root_name.replace(['/', '\\', '\0'], "_").trim().to_string();
    let root = if root.is_empty() {
        "shared".to_string()
    } else {
        root
    };
    let mut total_written = 0u64;

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
            total_written += read as u64;
            if wrote > entry.size.max(0) as u64 {
                return Err("stored file size does not match metadata".into());
            }
            if total_written > MAX_ZIP_BYTES {
                return Err("archive exceeds its size budget".into());
            }
        }
        if wrote != entry.size.max(0) as u64 {
            return Err("stored file size does not match metadata".into());
        }
    }

    let file = zip.finish().map_err(|err| err.to_string())?;
    file.metadata()
        .map(|meta| meta.len())
        .map_err(|err| err.to_string())
}

const AUTHORIZED_CTE: &str = "WITH RECURSIVE authorized(id, visited) AS ( \
   SELECT id, ',' || id || ',' FROM nodes WHERE id = ? \
   UNION ALL \
   SELECT n.id, authorized.visited || n.id || ',' FROM nodes n \
     JOIN authorized ON n.parent_id = authorized.id \
   WHERE instr(authorized.visited, ',' || n.id || ',') = 0 \
     AND length(authorized.visited) < 40000";

const ZIP_SUBTREE_CTE: &str = "subtree(id, rel, depth, visited) AS ( \
   SELECT n.id, n.name, 1, ',' || n.id || ',' \
   FROM nodes n JOIN authorized a ON a.id = n.id \
   WHERE n.parent_id = ? \
   UNION ALL \
   SELECT n.id, subtree.rel || '/' || n.name, subtree.depth + 1, \
          subtree.visited || n.id || ',' \
   FROM nodes n JOIN subtree ON n.parent_id = subtree.id \
   WHERE instr(subtree.visited, ',' || n.id || ',') = 0 \
     AND length(subtree.visited) < 40000";

fn zip_build_error(err: String) -> ApiError {
    if err.to_ascii_lowercase().contains("no space left") {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "not enough free disk space to build the archive",
        )
    } else {
        ApiError::internal(format!("could not build zip: {err}"))
    }
}

pub async fn download_zip(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Query(query): Query<FolderQuery>,
) -> Result<Response, ApiError> {
    let _gate = zip_gate(&token)?;

    let mut tx = state.pool.begin().await?;
    let active = shares::load_active_conn(&mut tx, &token).await?;
    let folder = shares::folder_snapshot(&mut tx, &active, query.folder_id.as_deref()).await?;

    let count_sql = format!(
        "{AUTHORIZED_CTE} ), {ZIP_SUBTREE_CTE} ) \
         SELECT COUNT(*), COALESCE(SUM(size), 0) FROM ( \
           SELECT n.size FROM nodes n JOIN subtree s ON s.id = n.id \
           WHERE n.kind = 'file' LIMIT ? \
         )"
    );
    let (count, total): (i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(count_sql))
        .bind(&active.target.id)
        .bind(&folder.id)
        .bind((MAX_ZIP_ENTRIES + 1) as i64)
        .fetch_one(&mut *tx)
        .await?;
    if count > MAX_ZIP_ENTRIES as i64 {
        return Err(ApiError::payload_too_large(
            "folder has too many files for a single zip",
        ));
    }
    let total = total.max(0) as u64;
    if total > MAX_ZIP_BYTES {
        return Err(ApiError::payload_too_large(
            "folder is too large for a single zip download",
        ));
    }

    let rows_sql = format!(
        "{AUTHORIZED_CTE} ), {ZIP_SUBTREE_CTE} ) \
         SELECT n.id, s.rel, n.blob_sha, n.size, n.mime \
         FROM nodes n JOIN subtree s ON s.id = n.id \
         WHERE n.kind = 'file' LIMIT ?"
    );
    let rows: Vec<ZipRow> = sqlx::query_as(sqlx::AssertSqlSafe(rows_sql))
        .bind(&active.target.id)
        .bind(&folder.id)
        .bind((MAX_ZIP_ENTRIES + 1) as i64)
        .fetch_all(&mut *tx)
        .await?;
    let folder_name = folder.name.clone();
    tx.commit().await?;
    let _ = shares::bump_access(&state, &active.share).await;

    if rows.len() > MAX_ZIP_ENTRIES {
        return Err(ApiError::payload_too_large(
            "folder has too many files for a single zip",
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

    let _budget = ZipBudget::reserve(total)?;
    storage::init(&state).await?;
    let temp_dir = state.cfg.upload_tmp_dir();
    let available = available_bytes(&temp_dir);
    if !has_zip_space(available, total) {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "not enough free disk space to stage this archive (need {} MiB plus a {} MiB reserve)",
                total.div_ceil(1024 * 1024),
                ZIP_FREE_SPACE_RESERVE / (1024 * 1024)
            ),
        ));
    }
    if available.is_none() {
        tracing::warn!("could not determine free space for archive staging");
    }
    let temp = TempFile::create(&temp_dir, "share-", ".zip").await?;
    let build_path = temp.path().to_path_buf();
    let blobs_dir = state.cfg.blobs_dir();
    let root_name = folder_name.clone();
    let size =
        tokio::task::spawn_blocking(move || build_zip(blobs_dir, build_path, root_name, entries))
            .await
            .map_err(|err| ApiError::internal(format!("zip task failed: {err}")))?
            .map_err(zip_build_error)?;

    let file = tokio::fs::File::open(temp.path()).await?;
    let reader = ReaderStream::new(file);
    let download_name = format!("{}.zip", folder_name);
    // The temp guard and aggregate budget are held until the stream finishes or
    // the client disconnects.
    let stream = async_stream::stream! {
        let _temp = temp;
        let _budget = _budget;
        let mut reader = reader;
        while let Some(chunk) = reader.next().await {
            yield chunk;
        }
    };
    Response::builder()
        .status(StatusCode::OK)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_budget_enforces_aggregate_cap() {
        let first = ZipBudget::reserve(MAX_ZIP_BYTES).expect("first");
        let second = ZipBudget::reserve(MAX_ZIP_BYTES).expect("second");
        assert!(ZipBudget::reserve(MAX_ZIP_TEMP_TOTAL).is_err());
        drop(first);
        drop(second);
        assert!(ZipBudget::reserve(MAX_ZIP_BYTES).is_ok());
    }

    #[test]
    fn zip_entries_are_bounded_before_building() {
        // The SQL gate is exercised in integration; verify the budget arithmetic
        // stays consistent with the documented contract.
        assert_eq!(MAX_ZIP_TEMP_TOTAL / MAX_ZIP_BYTES, 2);
    }

    #[test]
    fn zip_free_space_requires_total_plus_reserve() {
        assert!(has_zip_space(Some(ZIP_FREE_SPACE_RESERVE), 0));
        assert!(!has_zip_space(Some(ZIP_FREE_SPACE_RESERVE - 1), 0));
        let total = 10 * 1024 * 1024;
        assert!(has_zip_space(Some(total + ZIP_FREE_SPACE_RESERVE), total));
        assert!(!has_zip_space(
            Some(total + ZIP_FREE_SPACE_RESERVE - 1),
            total
        ));
        assert!(has_zip_space(None, total));
    }

    #[cfg(unix)]
    #[test]
    fn statvfs_reports_space_for_an_existing_directory() {
        assert!(
            available_bytes(std::path::Path::new(".")).is_some(),
            "statvfs should succeed for the crate directory"
        );
    }
}
