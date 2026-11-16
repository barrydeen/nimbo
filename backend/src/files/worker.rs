use crate::{
    error::ApiError,
    files::{
        extract::{self, Extraction},
        lock, storage, upsert_fts_conn, vision, vision_eligible, Node,
    },
    state::AppState,
};
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Semaphore;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_millis(500);
/// Best-effort wall-clock ceiling for one claimed job. The async job is aborted
/// on timeout; the parser subprocess is killed on drop. Native blocking work in
/// the in-process test fallback is not preemptible.
const JOB_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_ATTEMPTS: i64 = 3;
/// Shared between the index and vision loops so image/parser work cannot
/// multiply memory use by running every stage at once.
const WORK_PERMITS: usize = 2;
/// Expected parser output is bounded by 2 MiB of text plus metadata and a small
/// thumbnail; 16 MiB leaves generous JSON-escaping headroom while preventing a
/// buggy child from making the parent buffer unbounded.
const MAX_EXTRACT_RESULT_BYTES: usize = 16 * 1024 * 1024;
/// Hard limits applied to the parser child on Unix. The address-space limit is
/// conservative relative to the 256 MiB image-decode ceiling and a 2 GiB
/// container, leaving room for the runtime and parser working copies.
#[cfg(unix)]
const PARSER_RLIMIT_CPU_SECONDS: libc::rlim_t = 300;
#[cfg(unix)]
const PARSER_RLIMIT_ADDRESS_SPACE: libc::rlim_t = 1024 * 1024 * 1024;
#[cfg(unix)]
const PARSER_RLIMIT_CORE: libc::rlim_t = 0;

fn work_permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(Semaphore::new(WORK_PERMITS)))
}

pub fn spawn(state: Arc<AppState>) {
    let init_state = state.clone();
    tokio::spawn(async move {
        if let Err(err) = storage::init(&init_state).await {
            tracing::warn!(error = %err, "storage reconciliation failed");
        }
        if let Err(err) = crate::files::reconcile_fts(&init_state).await {
            tracing::warn!(error = %err, "FTS reconciliation failed");
        }
        supervise("index", init_state, index_loop).await;
    });
    tokio::spawn(async move {
        if let Err(err) = storage::init(&state).await {
            tracing::warn!(error = %err, "storage reconciliation failed");
        }
        supervise("vision", state, vision_loop).await;
    });
}

/// Restarts a worker loop after an unexpected return or panic instead of
/// silently losing the only indexing/vision task.
async fn supervise<F, Fut>(name: &'static str, state: Arc<AppState>, run: F)
where
    F: Fn(Arc<AppState>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        if let Err(err) = reset_running(&state).await {
            tracing::warn!(worker = name, error = %err, "could not reset running jobs");
        }
        let handle = tokio::spawn(run(state.clone()));
        match handle.await {
            Ok(()) => tracing::error!(worker = name, "worker exited; restarting"),
            Err(err) => tracing::error!(worker = name, error = %err, "worker panicked; restarting"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn reset_running(state: &AppState) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET index_status = 'pending' \
         WHERE index_status = 'running' AND index_attempts < ?",
    )
    .bind(MAX_ATTEMPTS)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "UPDATE nodes SET vision_status = 'pending' \
         WHERE vision_status = 'running' AND vision_attempts < ?",
    )
    .bind(MAX_ATTEMPTS)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "UPDATE nodes SET index_status = 'failed', \
           index_error = COALESCE(index_error, 'indexing attempts exhausted') \
         WHERE index_status = 'running' AND index_attempts >= ?",
    )
    .bind(MAX_ATTEMPTS)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        "UPDATE nodes SET vision_status = 'failed', \
           vision_error = COALESCE(vision_error, 'vision attempts exhausted') \
         WHERE vision_status = 'running' AND vision_attempts >= ?",
    )
    .bind(MAX_ATTEMPTS)
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn index_loop(state: Arc<AppState>) {
    loop {
        match process_next_index(&state).await {
            Ok(true) => {}
            Ok(false) => tokio::time::sleep(POLL_INTERVAL).await,
            Err(err) => {
                tracing::warn!(error = %err, "file index loop error");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

async fn vision_loop(state: Arc<AppState>) {
    loop {
        match process_next_vision(&state).await {
            Ok(true) => {}
            Ok(false) => tokio::time::sleep(POLL_INTERVAL).await,
            Err(err) => {
                tracing::warn!(error = %err, "vision loop error");
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

async fn claim_index(state: &AppState) -> Result<Option<Node>, ApiError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nodes WHERE kind = 'file' AND index_status = 'pending' \
         ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?;
    let Some((id,)) = row else {
        return Ok(None);
    };
    let updated = sqlx::query(
        "UPDATE nodes SET index_status = 'running', index_error = NULL, \
           index_attempts = index_attempts + 1 \
         WHERE id = ? AND index_status = 'pending'",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(
        sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?,
    ))
}

async fn process_next_index(state: &Arc<AppState>) -> Result<bool, ApiError> {
    let Some(node) = claim_index(state).await? else {
        return Ok(false);
    };
    let permit = work_permits()
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::internal("worker semaphore closed"))?;
    let job_state = state.clone();
    let job_node = node.clone();
    let mut handle = tokio::spawn(async move { run_index(&job_state, job_node).await });
    let outcome = tokio::time::timeout(JOB_TIMEOUT, &mut handle).await;
    if outcome.is_err() {
        // The async task is aborted so it cannot apply stale results later.
        // Native blocking parser work inside `spawn_blocking` is not
        // preemptible; it will finish in the background, but its result is
        // discarded and can no longer write to the database.
        handle.abort();
        let _ = handle.await;
    }
    let failure = match outcome {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(err))) => Some(err.message),
        Ok(Err(err)) if err.is_panic() => Some("indexing task panicked".to_string()),
        Ok(Err(err)) => Some(format!("indexing task failed: {err}")),
        Err(_) => Some("indexing exceeded its time budget".to_string()),
    };
    drop(permit);
    if let Some(message) = failure {
        tracing::warn!(file = %node.name, error = %message, "indexing failed");
        fail_index(state, &node, &message).await?;
        tokio::time::sleep(RETRY_DELAY).await;
    }
    Ok(true)
}

async fn run_index(state: &AppState, node: Node) -> Result<(), ApiError> {
    let path = node.blob_path(state)?;
    let name = node.name.clone();
    let mime = node
        .mime
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let extraction = extract_blob(path, name, mime).await?;
    finish_index(state, node, extraction).await
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ExtractRequest {
    path: String,
    name: String,
    mime: String,
}

/// Hidden entry point for the parser subprocess (`nimbo --extract-worker`).
/// Reads one JSON request on stdin and writes one JSON `Extraction` on stdout.
/// The child must not initialize tracing, configuration, state or workers; main
/// dispatches this before all of those.
pub fn run_extract_worker_cli() -> anyhow::Result<()> {
    use std::io::{Read, Write};

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: ExtractRequest = serde_json::from_str(&input)?;
    let extraction = extract::extract(
        std::path::Path::new(&request.path),
        &request.name,
        &request.mime,
    );
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &extraction)?;
    stdout.flush()?;
    Ok(())
}

/// Runs extraction in the isolated child process. There is deliberately no
/// production in-process fallback: if the child cannot be spawned or does not
/// return a valid bounded result, the claimed job fails and is terminalised.
/// Only `cfg(test)` uses the in-process path.
async fn extract_blob(
    path: std::path::PathBuf,
    name: String,
    mime: String,
) -> Result<Extraction, ApiError> {
    #[cfg(test)]
    {
        in_process_extract(path, name, mime).await
    }
    #[cfg(not(test))]
    {
        extract_subprocess(&path, &name, &mime).await
    }
}

fn extraction_payload_too_large(length: usize) -> bool {
    length > MAX_EXTRACT_RESULT_BYTES
}

#[cfg_attr(test, allow(dead_code))]
async fn extract_subprocess(
    path: &std::path::Path,
    name: &str,
    mime: &str,
) -> Result<Extraction, ApiError> {
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let executable = std::env::current_exe()
        .map_err(|err| ApiError::internal(format!("cannot locate parser executable: {err}")))?;
    let mut command = tokio::process::Command::new(executable);
    command
        .arg("--extract-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        // SAFETY: `pre_exec` runs in the forked child before `exec`. The closure
        // only invokes async-signal-safe `setrlimit(2)` calls, performs no
        // allocation and takes no locks. All values are compile-time constants.
        unsafe {
            command.pre_exec(|| {
                let core = libc::rlimit {
                    rlim_cur: PARSER_RLIMIT_CORE,
                    rlim_max: PARSER_RLIMIT_CORE,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &core) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let cpu = libc::rlimit {
                    rlim_cur: PARSER_RLIMIT_CPU_SECONDS,
                    rlim_max: PARSER_RLIMIT_CPU_SECONDS,
                };
                if libc::setrlimit(libc::RLIMIT_CPU, &cpu) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let address_space = libc::rlimit {
                    rlim_cur: PARSER_RLIMIT_ADDRESS_SPACE,
                    rlim_max: PARSER_RLIMIT_ADDRESS_SPACE,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &address_space) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Non-Unix keeps `kill_on_drop(true)` (process killability) but cannot apply
    // the POSIX address-space/CPU limits through this interface.

    let mut child = command
        .spawn()
        .map_err(|err| ApiError::internal(format!("cannot start parser worker: {err}")))?;

    let request = ExtractRequest {
        path: path.to_string_lossy().into_owned(),
        name: name.to_string(),
        mime: mime.to_string(),
    };
    let payload = serde_json::to_vec(&request)
        .map_err(|err| ApiError::internal(format!("cannot encode parser request: {err}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .map_err(|err| ApiError::internal(format!("cannot send parser request: {err}")))?;
    }

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::internal("parser worker produced no stdout pipe"))?;
    let mut result = Vec::with_capacity(64 * 1024);
    let mut too_large = false;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = stdout
            .read(&mut buffer)
            .await
            .map_err(|err| ApiError::internal(format!("cannot read parser result: {err}")))?;
        if read == 0 {
            break;
        }
        // Continue draining after overflow so the child never blocks on a full
        // stdout pipe; the oversized payload is discarded and reported.
        if too_large {
            continue;
        }
        if extraction_payload_too_large(result.len() + read) {
            too_large = true;
            result.clear();
            continue;
        }
        result.extend_from_slice(&buffer[..read]);
    }

    let status = child
        .wait()
        .await
        .map_err(|err| ApiError::internal(format!("parser worker failed: {err}")))?;
    if too_large {
        return Err(ApiError::internal(format!(
            "parser result exceeded the {} MiB limit",
            MAX_EXTRACT_RESULT_BYTES / (1024 * 1024)
        )));
    }
    if !status.success() {
        return Err(ApiError::internal(format!(
            "parser worker exited with {status}"
        )));
    }
    serde_json::from_slice(&result)
        .map_err(|err| ApiError::internal(format!("invalid parser result: {err}")))
}

#[cfg(test)]
async fn in_process_extract(
    path: std::path::PathBuf,
    name: String,
    mime: String,
) -> Result<Extraction, ApiError> {
    tokio::task::spawn_blocking(move || extract::extract(&path, &name, &mime))
        .await
        .map_err(|err| ApiError::internal(format!("extraction task failed: {err}")))
}

/// Terminalises a claimed job. Transient failures return to `pending` until the
/// attempt budget is exhausted; nothing is left `running` forever. The update is
/// conditional on the claimed content identity so a replacement's newer pending
/// state is never overwritten by a stale failure.
async fn fail_index(state: &AppState, claimed: &Node, message: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET \
           index_status = CASE WHEN index_attempts >= ? THEN 'failed' ELSE 'pending' END, \
           index_error = ? \
         WHERE id = ? AND blob_sha IS ? AND index_status = 'running'",
    )
    .bind(MAX_ATTEMPTS)
    .bind(truncate(message, 500))
    .bind(&claimed.id)
    .bind(claimed.blob_sha.as_deref())
    .execute(&state.pool)
    .await?;
    Ok(())
}

async fn finish_index(
    state: &AppState,
    node: Node,
    extraction: Extraction,
) -> Result<(), ApiError> {
    let metadata_json = extraction.metadata_json();
    let status = if extraction.error.is_some() || extraction.partial {
        "partial"
    } else {
        "done"
    };
    let text = extraction
        .text
        .as_deref()
        .map(|text| truncate(text, 2 * 1024 * 1024));

    let _guard = lock::mutation().await;

    // Cheap early identity check while the mutation lock blocks replacement:
    // avoid promoting a thumbnail for a claim that is already stale.
    let current = crate::files::get_node(&state.pool, &node.id).await?;
    match current {
        None => return Ok(()),
        Some(current) if current.index_status != "running" || !same_claim(&current, &node) => {
            let mut tx = crate::files::begin_write(&state.pool).await?;
            requeue_stale_index(&mut tx, &current).await?;
            tx.commit().await?;
            return Ok(());
        }
        Some(_) => {}
    }

    let thumbnail = match extraction.thumbnail.as_deref() {
        Some(bytes) => Some(storage::store_bytes(state, bytes).await?),
        None => None,
    };
    let thumbnail_size = extraction
        .thumbnail
        .as_ref()
        .map(|bytes| bytes.len() as i64)
        .unwrap_or(0);

    let mut tx = crate::files::begin_write(&state.pool).await?;
    let result = apply_index_result(
        &mut tx,
        &node,
        &metadata_json,
        status,
        text.as_deref(),
        extraction.error.as_deref(),
        thumbnail.as_ref().map(|blob| blob.sha.as_str()),
        thumbnail_size,
    )
    .await;
    match result {
        Ok(IndexOutcome::Applied) => {
            tx.commit().await?;
            Ok(())
        }
        Ok(IndexOutcome::Cancelled) | Ok(IndexOutcome::Missing) => {
            // Persist the stale-claim requeue/no-op, but never commit this
            // claim's extraction, thumbnail reference or FTS row.
            tx.commit().await?;
            if let Some(thumbnail) = thumbnail.as_ref() {
                storage::rollback_promoted(state, thumbnail).await;
            }
            Ok(())
        }
        Err(err) => {
            let _ = tx.rollback().await;
            if let Some(thumbnail) = thumbnail.as_ref() {
                storage::rollback_promoted(state, thumbnail).await;
            }
            Err(err)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexOutcome {
    /// The claimed content is still current; extraction was written.
    Applied,
    /// The node exists but its content identity changed while extraction ran.
    /// The current content's queue state is preserved/requeued; nothing from
    /// this claim may be written.
    Cancelled,
    /// The node was deleted while extraction ran.
    Missing,
}

/// Identity of the content a claim was issued for. `blob_sha` catches content
/// replacement; name/updated_at/mime catch renames/moves/updates that invalidate
/// the snapshot used for extraction and FTS naming.
fn same_claim(current: &Node, claimed: &Node) -> bool {
    current.blob_sha == claimed.blob_sha
        && current.updated_at == claimed.updated_at
        && current.name == claimed.name
        && current.mime == claimed.mime
        && current.size == claimed.size
}

/// Only requeue a stale claim when it still owns the `running` slot for the
/// current row identity. A replacement that already set `pending` is untouched.
async fn requeue_stale_index(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    current: &Node,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET index_status = 'pending', index_error = NULL \
         WHERE id = ? AND blob_sha IS ? AND updated_at = ? AND name = ? \
           AND index_status = 'running'",
    )
    .bind(&current.id)
    .bind(current.blob_sha.as_deref())
    .bind(current.updated_at)
    .bind(&current.name)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_index_result(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    node: &Node,
    metadata_json: &str,
    status: &str,
    text: Option<&str>,
    error: Option<&str>,
    thumbnail_sha: Option<&str>,
    thumbnail_size: i64,
) -> Result<IndexOutcome, ApiError> {
    let current: Option<Node> = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&node.id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(current) = current else {
        return Ok(IndexOutcome::Missing);
    };

    if current.index_status != "running" || !same_claim(&current, node) {
        // The replacement/retry already persisted its own state. Only requeue
        // when this stale claim still owns the `running` slot; never clear a
        // replacement's `pending` status or write its content fields.
        requeue_stale_index(tx, &current).await?;
        return Ok(IndexOutcome::Cancelled);
    }

    sqlx::query(
        "UPDATE nodes SET index_status = ?, index_error = ?, text_content = ?, \
         metadata_json = ?, thumb_sha = COALESCE(?, thumb_sha) WHERE id = ?",
    )
    .bind(status)
    .bind(error.map(|error| truncate(error, 500)))
    .bind(text)
    .bind(metadata_json)
    .bind(thumbnail_sha)
    .bind(&current.id)
    .execute(&mut **tx)
    .await?;

    let mime = current.mime.clone().unwrap_or_default();
    if vision_eligible(&mime) {
        sqlx::query(
            "UPDATE nodes SET vision_status = 'pending' \
             WHERE id = ? AND vision_status = 'n/a'",
        )
        .bind(&current.id)
        .execute(&mut **tx)
        .await?;
    } else if mime.starts_with("image/") {
        sqlx::query(
            "UPDATE nodes SET vision_status = 'skipped', vision_error = ? \
             WHERE id = ? AND vision_status IN ('n/a', 'pending')",
        )
        .bind("this image format cannot be described automatically")
        .bind(&current.id)
        .execute(&mut **tx)
        .await?;
    }

    let updated: Option<Node> = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&current.id)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(updated) = updated {
        let metadata_text = vision::metadata_search_text(&updated);
        upsert_fts_conn(
            tx,
            &updated.id,
            &updated.name,
            updated.text_content.as_deref(),
            updated.vision_description.as_deref(),
            Some(&metadata_text),
        )
        .await?;
    }
    if let Some(sha) = thumbnail_sha {
        storage::register_blob_tx(tx, sha, thumbnail_size).await?;
    }
    Ok(IndexOutcome::Applied)
}

async fn claim_vision(state: &AppState) -> Result<Option<Node>, ApiError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM nodes WHERE kind = 'file' AND vision_status = 'pending' \
         AND mime LIKE 'image/%' AND mime <> 'image/svg+xml' \
         ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?;
    let Some((id,)) = row else {
        return Ok(None);
    };
    let updated = sqlx::query(
        "UPDATE nodes SET vision_status = 'running', vision_error = NULL, \
           vision_attempts = vision_attempts + 1 \
         WHERE id = ? AND vision_status = 'pending'",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Ok(None);
    }
    Ok(Some(
        sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await?,
    ))
}

async fn process_next_vision(state: &Arc<AppState>) -> Result<bool, ApiError> {
    let Some(node) = claim_vision(state).await? else {
        return Ok(false);
    };
    let permit = work_permits()
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ApiError::internal("worker semaphore closed"))?;
    let job_state = state.clone();
    let job_node = node.clone();
    let mut handle = tokio::spawn(async move { vision::process(&job_state, job_node).await });
    let outcome = tokio::time::timeout(JOB_TIMEOUT, &mut handle).await;
    if outcome.is_err() {
        // Prevent a timed-out task from applying a stale description later.
        // The native image decode/LLM work itself cannot be preempted.
        handle.abort();
        let _ = handle.await;
    }
    let failure = match outcome {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(err))) => Some(err.message),
        Ok(Err(err)) if err.is_panic() => Some("vision task panicked".to_string()),
        Ok(Err(err)) => Some(format!("vision task failed: {err}")),
        Err(_) => Some("vision exceeded its time budget".to_string()),
    };
    drop(permit);
    if let Some(message) = failure {
        tracing::warn!(file = %node.name, error = %message, "image description failed");
        fail_vision(state, &node, &message).await?;
        tokio::time::sleep(RETRY_DELAY).await;
    }
    Ok(true)
}

async fn fail_vision(state: &AppState, claimed: &Node, message: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET \
           vision_status = CASE WHEN vision_attempts >= ? THEN 'failed' ELSE 'pending' END, \
           vision_error = ?, vision_at = ? \
         WHERE id = ? AND blob_sha IS ? AND vision_status = 'running'",
    )
    .bind(MAX_ATTEMPTS)
    .bind(truncate(message, 500))
    .bind(crate::now())
    .bind(&claimed.id)
    .bind(claimed.blob_sha.as_deref())
    .execute(&state.pool)
    .await?;
    Ok(())
}

#[allow(dead_code)]
fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations");
        pool
    }

    async fn insert_claimed(pool: &SqlitePool, blob: &str, name: &str) -> Node {
        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at, blob_sha, \
             mime, size, index_status, text_content, metadata_json, vision_status, thumb_sha) \
             VALUES ('n1', 'root', 'file', ?, 1000, 1000, ?, 'image/jpeg', 4, 'running', \
             NULL, '{}', 'n/a', NULL)",
        )
        .bind(name)
        .bind(blob)
        .execute(pool)
        .await
        .expect("insert claimed node");
        sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = 'n1'")
            .fetch_one(pool)
            .await
            .expect("claimed node")
    }

    #[tokio::test]
    async fn stale_index_result_does_not_overwrite_replaced_content() {
        let pool = test_pool().await;
        let old_blob = "a".repeat(64);
        let new_blob = "c".repeat(64);
        let claimed = insert_claimed(&pool, &old_blob, "photo.jpg").await;

        // Replacement commits while extraction is running.
        sqlx::query(
            "UPDATE nodes SET blob_sha = ?, updated_at = 2000, index_status = 'pending', \
             index_error = NULL, text_content = NULL, metadata_json = '{}', thumb_sha = NULL, \
             vision_status = 'pending', vision_description = NULL, vision_text = NULL \
             WHERE id = 'n1'",
        )
        .bind(&new_blob)
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let outcome = apply_index_result(
            &mut tx,
            &claimed,
            r#"{"old":true}"#,
            "done",
            Some("old extracted text"),
            None,
            Some(&"b".repeat(64)),
            4,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(outcome, IndexOutcome::Cancelled);
        let (status, text, metadata, thumb, vision, description): (
            String,
            Option<String>,
            String,
            Option<String>,
            String,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT index_status, text_content, metadata_json, thumb_sha, vision_status, \
             vision_description FROM nodes WHERE id = 'n1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "pending");
        assert!(text.is_none());
        assert_eq!(metadata, "{}");
        assert!(thumb.is_none());
        assert_eq!(vision, "pending");
        assert!(description.is_none());
        let (fts, blobs): (i64, i64) =
            sqlx::query_as("SELECT (SELECT COUNT(*) FROM files_fts), (SELECT COUNT(*) FROM blobs)")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(fts, 0, "stale extraction must not be indexed");
        assert_eq!(blobs, 0, "stale thumbnail reference must not be registered");
    }

    #[tokio::test]
    async fn stale_index_result_is_requeued_after_rename() {
        let pool = test_pool().await;
        let blob = "a".repeat(64);
        let claimed = insert_claimed(&pool, &blob, "photo.jpg").await;

        sqlx::query("UPDATE nodes SET name = 'renamed.jpg', updated_at = 1500 WHERE id = 'n1'")
            .execute(&pool)
            .await
            .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let outcome = apply_index_result(
            &mut tx,
            &claimed,
            r#"{"old":true}"#,
            "done",
            Some("old extracted text"),
            None,
            None,
            0,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(outcome, IndexOutcome::Cancelled);
        let (name, status, text, metadata): (String, String, Option<String>, String) =
            sqlx::query_as(
                "SELECT name, index_status, text_content, metadata_json FROM nodes WHERE id = 'n1'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "renamed.jpg");
        assert_eq!(status, "pending", "stale claim requeues the current row");
        assert!(text.is_none());
        assert_eq!(metadata, "{}");
        let (fts,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files_fts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[tokio::test]
    async fn pending_retry_is_not_clobbered_by_stale_index_result() {
        let pool = test_pool().await;
        let claimed = insert_claimed(&pool, &"a".repeat(64), "photo.jpg").await;

        // Same content, but another actor (retry/replacement) owns the queue.
        sqlx::query(
            "UPDATE nodes SET index_status = 'pending', index_error = NULL WHERE id = 'n1'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let outcome = apply_index_result(
            &mut tx,
            &claimed,
            r#"{"old":true}"#,
            "done",
            Some("old extracted text"),
            None,
            None,
            0,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(outcome, IndexOutcome::Cancelled);
        let (status, text): (String, Option<String>) =
            sqlx::query_as("SELECT index_status, text_content FROM nodes WHERE id = 'n1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "pending");
        assert!(text.is_none());
        let (fts,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files_fts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[tokio::test]
    async fn current_claim_applies_extraction() {
        let pool = test_pool().await;
        let blob = "a".repeat(64);
        let claimed = insert_claimed(&pool, &blob, "photo.jpg").await;

        let mut tx = pool.begin().await.unwrap();
        let outcome = apply_index_result(
            &mut tx,
            &claimed,
            r#"{"image":{"width":4}}"#,
            "done",
            Some("hello extracted"),
            None,
            None,
            0,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(outcome, IndexOutcome::Applied);
        let (status, text): (String, Option<String>) =
            sqlx::query_as("SELECT index_status, text_content FROM nodes WHERE id = 'n1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "done");
        assert_eq!(text.as_deref(), Some("hello extracted"));
        let (indexed,): (String,) =
            sqlx::query_as("SELECT text FROM files_fts WHERE file_id = 'n1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed, "hello extracted");
    }

    #[test]
    fn parser_result_cap_rejects_only_oversized_payloads() {
        let limit = std::hint::black_box(MAX_EXTRACT_RESULT_BYTES);
        assert!(!extraction_payload_too_large(0));
        assert!(!extraction_payload_too_large(limit));
        assert!(extraction_payload_too_large(limit + 1));
    }

    #[cfg(unix)]
    #[test]
    fn parser_resource_limits_are_conservative() {
        let cpu = std::hint::black_box(PARSER_RLIMIT_CPU_SECONDS);
        let address_space = std::hint::black_box(PARSER_RLIMIT_ADDRESS_SPACE);
        let core = std::hint::black_box(PARSER_RLIMIT_CORE);
        assert!(cpu > 0 && cpu <= 600);
        assert!(address_space >= 512 * 1024 * 1024);
        assert!(address_space <= 2 * 1024 * 1024 * 1024);
        assert_eq!(core, 0);
    }
}
