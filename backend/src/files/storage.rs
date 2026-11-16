use crate::{error::ApiError, files::lock, state::AppState, util::sha256_hex};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

const MAX_ORPHAN_SCAN: usize = 200_000;
const TEMP_PREFIXES: [&str; 3] = ["upload-", "thumb-", "share-"];

/// Validates a content hash before it is used in a filesystem path.
fn valid_sha(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn blob_path(state: &AppState, sha: &str) -> Result<PathBuf, ApiError> {
    blob_path_under(&state.cfg.blobs_dir(), sha)
}

pub fn blob_path_under(root: &Path, sha: &str) -> Result<PathBuf, ApiError> {
    if !valid_sha(sha) {
        return Err(ApiError::internal("invalid blob hash"));
    }
    Ok(root.join(&sha[0..2]).join(&sha[2..4]).join(sha))
}

fn temp_name(prefix: &str, suffix: &str) -> String {
    format!("{prefix}{}{suffix}", uuid::Uuid::new_v4())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Creates a directory (and parents) and forces its mode to 0700 on Unix,
/// independently of the process umask.
pub async fn ensure_dir(path: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(path).await?;
    set_mode(path, 0o700);
    Ok(())
}

async fn open_private(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    options.open(path).await
}

/// Best-effort durability barrier for a directory entry (rename/create/remove).
pub async fn sync_dir(path: &Path) {
    #[cfg(unix)]
    {
        if let Ok(dir) = tokio::fs::File::open(path).await {
            let _ = dir.sync_all().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// A temporary file that deletes itself on drop unless explicitly disarmed.
/// Used from the moment a path is created so every early return, parse error,
/// panic or client disconnect removes the partial artifact.
pub struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl TempFile {
    pub async fn create(dir: &Path, prefix: &str, suffix: &str) -> std::io::Result<Self> {
        let (temp, file) = Self::create_with_file(dir, prefix, suffix).await?;
        drop(file);
        Ok(temp)
    }

    /// Creates the guard and an open private writer in one step. The caller must
    /// flush/sync the file before promoting it.
    pub async fn create_with_file(
        dir: &Path,
        prefix: &str,
        suffix: &str,
    ) -> std::io::Result<(Self, tokio::fs::File)> {
        ensure_dir(dir).await?;
        let path = dir.join(temp_name(prefix, suffix));
        let file = open_private(&path).await?;
        Ok((Self { path, armed: true }, file))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Creates the storage directories with private permissions.
pub async fn ensure_dirs(state: &AppState) -> std::io::Result<()> {
    ensure_dir(&state.cfg.files_dir()).await?;
    ensure_dir(&state.cfg.blobs_dir()).await?;
    ensure_dir(&state.cfg.upload_tmp_dir()).await
}

/// A blob file that was written into the content-addressed store.
pub struct Promoted {
    pub sha: String,
    /// True when this call created the destination file. Only new files may be
    /// removed when the surrounding reference transaction fails.
    pub is_new: bool,
}

/// Moves a fully-written and synced temp file into the content-addressed store.
/// The caller must hold the mutation lock so no garbage collection can observe
/// the file before its blob row/reference commits.
pub async fn promote_temp(
    state: &AppState,
    temp: &mut TempFile,
    sha: &str,
    size: i64,
) -> Result<Promoted, ApiError> {
    let destination = blob_path(state, sha)?;
    if let Some(parent) = destination.parent() {
        ensure_dir(parent).await?;
    }
    let existing = tokio::fs::metadata(&destination).await.ok();
    let is_new = match existing {
        Some(meta) if meta.is_file() && meta.len() == size as u64 => {
            set_mode(&destination, 0o600);
            false
        }
        _ => {
            if tokio::fs::rename(temp.path(), &destination).await.is_err() {
                // Cross-device or an unexpected failure: copy then remove.
                tokio::fs::copy(temp.path(), &destination).await?;
                if let Ok(file) = tokio::fs::File::open(&destination).await {
                    let _ = file.sync_all().await;
                }
                tokio::fs::remove_file(temp.path()).await.ok();
            }
            set_mode(&destination, 0o600);
            if let Some(parent) = destination.parent() {
                sync_dir(parent).await;
            }
            true
        }
    };
    temp.disarm();
    Ok(Promoted {
        sha: sha.to_string(),
        is_new,
    })
}

/// Removes a just-promoted blob when the reference transaction failed. Safe
/// because the caller still holds the mutation lock and no row references it.
pub async fn rollback_promoted(state: &AppState, promoted: &Promoted) {
    if !promoted.is_new {
        return;
    }
    if let Ok(path) = blob_path(state, &promoted.sha) {
        let _ = tokio::fs::remove_file(path).await;
    }
}

/// Writes bytes durably to the content-addressed store. Returns the hash and
/// whether a new file was created. Caller holds the mutation lock.
pub async fn store_bytes(state: &AppState, bytes: &[u8]) -> Result<Promoted, ApiError> {
    let sha = sha256_hex(bytes);
    let destination = blob_path(state, &sha)?;
    let existing = tokio::fs::metadata(&destination).await.ok();
    let is_new = match existing {
        Some(meta) if meta.is_file() && meta.len() == bytes.len() as u64 => {
            set_mode(&destination, 0o600);
            false
        }
        _ => {
            if let Some(parent) = destination.parent() {
                ensure_dir(parent).await?;
            }
            let mut temp = TempFile::create(&state.cfg.upload_tmp_dir(), "thumb-", ".tmp").await?;
            let mut file = open_private(temp.path()).await?;
            file.write_all(bytes).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(temp.path(), &destination).await?;
            set_mode(&destination, 0o600);
            if let Some(parent) = destination.parent() {
                sync_dir(parent).await;
            }
            temp.disarm();
            true
        }
    };
    Ok(Promoted { sha, is_new })
}

/// Registers a blob row. Must run in the same transaction that creates the
/// reference so a committed row is never unreferenced.
pub async fn register_blob_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    sha: &str,
    size: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO blobs (sha256, size, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(sha256) DO UPDATE SET size = excluded.size",
    )
    .bind(sha)
    .bind(size)
    .bind(crate::now())
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM blob_tombstones WHERE sha256 = ?")
        .bind(sha)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Deletes unreferenced blob rows/files and keeps a retryable tombstone when the
/// filesystem unlink fails. The reference check is repeated immediately before
/// the row is deleted while the mutation lock is held.
pub async fn gc_unreferenced(state: &AppState) -> Result<(), ApiError> {
    let guard = lock::mutation().await;
    let candidates: Vec<(String,)> = sqlx::query_as(
        "SELECT b.sha256 FROM blobs b \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM nodes n WHERE n.blob_sha = b.sha256 OR n.thumb_sha = b.sha256 \
         )",
    )
    .fetch_all(&state.pool)
    .await?;

    for (sha,) in candidates {
        let mut tx = super::begin_write(&state.pool).await?;
        let live: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM nodes WHERE blob_sha = ? OR thumb_sha = ? LIMIT 1")
                .bind(&sha)
                .bind(&sha)
                .fetch_optional(&mut *tx)
                .await?;
        if live.is_some() {
            tx.rollback().await?;
            continue;
        }
        let removed = sqlx::query("DELETE FROM blobs WHERE sha256 = ?")
            .bind(&sha)
            .execute(&mut *tx)
            .await?;
        if removed.rows_affected() == 0 {
            tx.rollback().await?;
            continue;
        }
        sqlx::query(
            "INSERT INTO blob_tombstones (sha256, queued_at) VALUES (?, ?) \
             ON CONFLICT(sha256) DO UPDATE SET queued_at = excluded.queued_at",
        )
        .bind(&sha)
        .bind(crate::now())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if let Err(err) = remove_blob_file(state, &sha).await {
            tracing::warn!(sha = %sha, error = %err, "blob unlink failed; keeping retryable tombstone");
        }
    }
    drop(guard);
    retry_tombstones(state).await
}

async fn remove_blob_file(state: &AppState, sha: &str) -> std::io::Result<()> {
    let path = blob_path(state, sha).map_err(std::io::Error::other)?;
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Retries tombstoned unlinks. A blob row that reappeared (deduplicated upload)
/// cancels the deletion because the file is live again.
pub async fn retry_tombstones(state: &AppState) -> Result<(), ApiError> {
    let guard = lock::mutation().await;
    let rows: Vec<(String,)> = sqlx::query_as("SELECT sha256 FROM blob_tombstones")
        .fetch_all(&state.pool)
        .await?;
    for (sha,) in rows {
        let live: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM blobs WHERE sha256 = ?")
            .bind(&sha)
            .fetch_optional(&state.pool)
            .await?;
        if live.is_some() {
            sqlx::query("DELETE FROM blob_tombstones WHERE sha256 = ?")
                .bind(&sha)
                .execute(&state.pool)
                .await?;
            continue;
        }
        match remove_blob_file(state, &sha).await {
            Ok(()) => {
                sqlx::query("DELETE FROM blob_tombstones WHERE sha256 = ?")
                    .bind(&sha)
                    .execute(&state.pool)
                    .await?;
            }
            Err(err) => {
                tracing::warn!(sha = %sha, error = %err, "blob unlink retry failed");
            }
        }
    }
    drop(guard);
    Ok(())
}

static INIT: OnceCell<()> = OnceCell::const_new();

/// Idempotent storage initialisation: creates private directories, removes
/// abandoned temp artifacts and reconciles blob files that have no row.
///
/// Temp writers await this before creating a file, so startup reconciliation
/// can never delete an in-flight upload.
pub async fn init(state: &AppState) -> Result<(), ApiError> {
    INIT.get_or_try_init(|| async {
        ensure_dirs(state).await?;
        let guard = lock::mutation().await;
        cleanup_stale_temps(state).await;
        reconcile_orphan_blobs(state).await;
        drop(guard);
        retry_tombstones(state).await?;
        Ok::<(), ApiError>(())
    })
    .await
    .map(|_| ())
}

async fn cleanup_stale_temps(state: &AppState) {
    let dir = state.cfg.upload_tmp_dir();
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return;
    };
    // Initialisation completes before any writer creates a temp file (writers
    // await `init`), so every recognised artifact here is abandoned.
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !TEMP_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let _ = tokio::fs::remove_file(entry.path()).await;
    }
}

/// Tightens permissions on existing blob shards/files and removes blob files
/// that no row references. Such files can only be left by a crash between the
/// durable rename and the metadata commit; anything live has a `blobs` row (or
/// a tombstone while its unlink is retried).
async fn reconcile_orphan_blobs(state: &AppState) {
    use std::collections::HashSet;

    let live: HashSet<String> = sqlx::query_as::<_, (String,)>("SELECT sha256 FROM blobs")
        .fetch_all(&state.pool)
        .await
        .map(|rows| rows.into_iter().map(|row| row.0).collect())
        .unwrap_or_default();
    let tombstoned: HashSet<String> =
        sqlx::query_as::<_, (String,)>("SELECT sha256 FROM blob_tombstones")
            .fetch_all(&state.pool)
            .await
            .map(|rows| rows.into_iter().map(|row| row.0).collect())
            .unwrap_or_default();

    let blobs = state.cfg.blobs_dir();
    let Ok(mut shards) = tokio::fs::read_dir(&blobs).await else {
        return;
    };
    let mut seen = 0usize;
    while let Ok(Some(shard)) = shards.next_entry().await {
        if seen >= MAX_ORPHAN_SCAN {
            tracing::warn!("blob orphan scan stopped at its safety bound");
            break;
        }
        let shard_name = shard.file_name().to_string_lossy().to_string();
        if shard_name.len() != 2 || !shard_name.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        set_mode(&shard.path(), 0o700);
        let Ok(mut inner) = tokio::fs::read_dir(shard.path()).await else {
            continue;
        };
        while let Ok(Some(entry)) = inner.next_entry().await {
            if seen >= MAX_ORPHAN_SCAN {
                break;
            }
            seen += 1;
            let name = entry.file_name().to_string_lossy().to_string();
            if !valid_sha(&name) {
                continue;
            }
            // Existing stores created under a permissive umask are tightened
            // here without touching live content.
            set_mode(&entry.path(), 0o600);
            if live.contains(&name) || tombstoned.contains(&name) {
                continue;
            }
            match tokio::fs::remove_file(entry.path()).await {
                Ok(()) => tracing::info!(sha = %name, "removed orphaned blob file"),
                Err(err) => {
                    tracing::warn!(sha = %name, error = %err, "could not remove orphaned blob file")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_hashes() {
        assert!(!valid_sha("../etc/passwd"));
        assert!(!valid_sha("abc"));
        assert!(valid_sha(&"a".repeat(64)));
    }

    #[test]
    fn temp_names_are_unique() {
        assert_ne!(temp_name("upload-", ".tmp"), temp_name("upload-", ".tmp"));
    }

    #[test]
    fn blob_paths_are_sharded() {
        let sha = "ab".to_string() + &"c".repeat(62);
        let path = blob_path_under(Path::new("/data"), &sha).unwrap();
        assert!(path.ends_with(Path::new("ab").join("cc").join(&sha)));
    }

    #[tokio::test]
    async fn temp_directories_and_files_are_private_and_cleaned_up() {
        let dir = std::env::temp_dir().join(format!("nimbo-temp-{}", uuid::Uuid::new_v4()));
        let (temp, mut file) = TempFile::create_with_file(&dir, "upload-", ".tmp")
            .await
            .expect("temp file");
        file.write_all(b"payload").await.unwrap();
        file.sync_all().await.unwrap();
        drop(file);
        let path = temp.path().to_path_buf();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }
        assert!(path.exists());
        drop(temp);
        assert!(!path.exists(), "RAII guard removes the staged temp file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
