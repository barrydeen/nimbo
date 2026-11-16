use crate::{error::ApiError, state::AppState, util::sha256_hex};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

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

pub async fn ensure_dirs(state: &AppState) -> std::io::Result<()> {
    tokio::fs::create_dir_all(state.cfg.blobs_dir()).await?;
    tokio::fs::create_dir_all(state.cfg.upload_tmp_dir()).await
}

/// Moves a fully-written temp file into the content-addressed store. Returns
/// true when the blob was new and false when an identical blob already existed.
pub async fn promote_temp(
    state: &AppState,
    temp: &Path,
    sha: &str,
    size: i64,
) -> Result<bool, ApiError> {
    let destination = blob_path(state, sha)?;
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let is_new = !destination.exists();
    if is_new {
        tokio::fs::rename(temp, &destination).await?;
    } else {
        let _ = tokio::fs::remove_file(temp).await;
    }
    sqlx::query(
        "INSERT INTO blobs (sha256, size, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(sha256) DO UPDATE SET size = excluded.size",
    )
    .bind(sha)
    .bind(size)
    .bind(crate::now())
    .execute(&state.pool)
    .await?;
    Ok(is_new)
}

/// Stores an in-memory blob (used for generated thumbnails).
pub async fn store_bytes(state: &AppState, bytes: &[u8]) -> Result<String, ApiError> {
    let sha = sha256_hex(bytes);
    let destination = blob_path(state, &sha)?;
    if !destination.exists() {
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temp = state
            .cfg
            .upload_tmp_dir()
            .join(format!("thumb-{}.tmp", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(state.cfg.upload_tmp_dir()).await?;
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&temp, &destination).await?;
    }
    sqlx::query(
        "INSERT INTO blobs (sha256, size, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(sha256) DO UPDATE SET size = excluded.size",
    )
    .bind(&sha)
    .bind(bytes.len() as i64)
    .bind(crate::now())
    .execute(&state.pool)
    .await?;
    Ok(sha)
}

/// Deletes blob rows and files that are no longer referenced by any node.
pub async fn gc_unreferenced(pool: &SqlitePool, state: &AppState) -> Result<(), ApiError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT b.sha256 FROM blobs b \
         WHERE NOT EXISTS ( \
           SELECT 1 FROM nodes n WHERE n.blob_sha = b.sha256 OR n.thumb_sha = b.sha256 \
         )",
    )
    .fetch_all(pool)
    .await?;

    for (sha,) in rows {
        sqlx::query("DELETE FROM blobs WHERE sha256 = ?")
            .bind(&sha)
            .execute(pool)
            .await?;
        if let Ok(path) = blob_path(state, &sha) {
            let _ = tokio::fs::remove_file(path).await;
        }
    }
    Ok(())
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
}
