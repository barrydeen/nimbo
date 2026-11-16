use crate::{auth::AuthUser, error::ApiError, state::AppState, util::sha256_hex};
use axum::{
    extract::{Extension, Query, State},
    Json,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{QueryBuilder, Sqlite, SqlitePool, Transaction};
use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

/// The advertised maximum document size (markdown bytes, not JSON bytes).
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound on indexed wikilink occurrences per document. This bounds the
/// work a single hostile document can impose on a save.
const MAX_LINKS_PER_DOCUMENT: usize = 2_000;
/// Upper bound on dangling-link rows examined per save.
const MAX_DANGLING_CANDIDATES: i64 = 5_000;
/// Rows per generated `UPDATE ... WHERE id IN (...)` statement.
const UPDATE_CHUNK: usize = 400;
/// Link rows per multi-row `INSERT` statement (keeps bound parameters well
/// below SQLite's conservative variable limit).
const LINK_INSERT_CHUNK: usize = 250;

/// Serializes cooperating vault mutations across validation, filesystem
/// mutation and the index hand-off. This is what makes two API writers behave
/// deterministically: the loser of a save comparison receives 409 instead of
/// silently overwriting the winner.
static VAULT_MUTATION: Mutex<()> = Mutex::const_new(());

#[cfg(all(unix, target_os = "linux"))]
const O_NOFOLLOW_FLAG: i32 = 0x20000;

fn link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([^\]\n]{1,300})\]\]").expect("valid regex"))
}

fn word_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^\p{L}\p{N}_]+").expect("valid regex"))
}

/// Validates and canonicalizes a vault-relative document path.
///
/// This is lexical only; callers must additionally use the containment helpers
/// below before touching the filesystem.
pub fn normalize_path(raw: &str) -> Result<String, ApiError> {
    let raw = raw.trim().replace('\\', "/");
    if raw.is_empty() {
        return Err(ApiError::bad_request("path is required"));
    }
    if raw.len() > 512 {
        return Err(ApiError::bad_request("path is too long"));
    }
    if raw.starts_with('/') {
        return Err(ApiError::bad_request("path must be relative"));
    }

    let mut segments: Vec<&str> = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return Err(ApiError::bad_request("path may not contain '..'"));
        }
        if segment.len() > 128 {
            return Err(ApiError::bad_request("path segment is too long"));
        }
        if segment.contains('\0') {
            return Err(ApiError::bad_request("path contains a null byte"));
        }
        segments.push(segment);
    }

    if segments.is_empty() {
        return Err(ApiError::bad_request("path is required"));
    }

    let joined = segments.join("/");
    if !joined.ends_with(".md") {
        return Err(ApiError::bad_request("path must end with .md"));
    }
    Ok(joined)
}

// ------------------------------------------------------- path containment

fn canonical_vault(vault: &Path) -> Result<PathBuf, ApiError> {
    vault
        .canonicalize()
        .map_err(|_| ApiError::internal("vault directory is unavailable"))
}

fn reject_symlink(path: &Path, meta: &Metadata) -> Result<(), ApiError> {
    if meta.file_type().is_symlink() {
        tracing::warn!(path = %path.display(), "rejected vault path traversing a symbolic link");
        return Err(ApiError::forbidden("path may not traverse symbolic links"));
    }
    Ok(())
}

fn ensure_beneath(root: &Path, path: &Path) -> Result<(), ApiError> {
    let canonical = path
        .canonicalize()
        .map_err(|_| ApiError::internal("vault path could not be resolved"))?;
    if !canonical.starts_with(root) {
        tracing::warn!(path = %path.display(), "rejected vault path outside the vault root");
        return Err(ApiError::forbidden("path escapes the vault"));
    }
    Ok(())
}

/// Resolves an existing document, rejecting symlinked final components and
/// symlinked parents. The canonical parent must remain beneath the canonical
/// vault root.
fn resolve_read_path(vault: &Path, rel: &str) -> Result<PathBuf, ApiError> {
    let root = canonical_vault(vault)?;
    let parts: Vec<&str> = rel.split('/').collect();
    let mut current = root.clone();

    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = std::fs::symlink_metadata(&current).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ApiError::not_found("document not found")
            } else {
                ApiError::from(err)
            }
        })?;
        reject_symlink(&current, &meta)?;
        let last = index + 1 == parts.len();
        if last {
            if !meta.is_file() {
                return Err(ApiError::not_found("document not found"));
            }
        } else if !meta.is_dir() {
            return Err(ApiError::not_found("document not found"));
        }
    }

    if let Some(parent) = current.parent() {
        ensure_beneath(&root, parent)?;
    }
    Ok(current)
}

/// Resolves a write destination, rejecting symlinked final components and
/// symlinked parents. Missing trailing components are allowed (the caller
/// creates them later through `prepare_parent_dirs`).
fn resolve_write_path(vault: &Path, rel: &str) -> Result<PathBuf, ApiError> {
    let root = canonical_vault(vault)?;
    let parts: Vec<&str> = rel.split('/').collect();
    let mut current = root.clone();
    let mut deepest_existing = root.clone();

    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let last = index + 1 == parts.len();
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                reject_symlink(&current, &meta)?;
                if last {
                    if !meta.is_file() {
                        return Err(ApiError::conflict("path exists and is not a document"));
                    }
                } else if !meta.is_dir() {
                    return Err(ApiError::bad_request("parent path is not a directory"));
                }
                deepest_existing = current.clone();
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => return Err(err.into()),
        }
    }

    ensure_beneath(&root, &deepest_existing)?;
    Ok(current)
}

/// Creates missing parent directories one component at a time, rejecting
/// symlinks introduced at any point, and verifies every ancestor canonicalizes
/// beneath the vault root. Returns the final file path.
fn prepare_parent_dirs(vault: &Path, rel: &str) -> Result<PathBuf, ApiError> {
    let root = canonical_vault(vault)?;
    let parts: Vec<&str> = rel.split('/').collect();
    let mut current = root.clone();

    for part in &parts[..parts.len() - 1] {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                reject_symlink(&current, &meta)?;
                if !meta.is_dir() {
                    return Err(ApiError::bad_request("parent path is not a directory"));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                let meta = std::fs::symlink_metadata(&current)?;
                reject_symlink(&current, &meta)?;
                if !meta.is_dir() {
                    return Err(ApiError::internal("could not create vault directory"));
                }
            }
            Err(err) => return Err(err.into()),
        }
        ensure_beneath(&root, &current)?;
    }

    let target = current.join(parts[parts.len() - 1]);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) => {
            reject_symlink(&target, &meta)?;
            if !meta.is_file() {
                return Err(ApiError::conflict("path exists and is not a document"));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(target)
}

// ------------------------------------------------------------ filesystem IO

fn open_read_only(path: &Path) -> Result<File, ApiError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(O_NOFOLLOW_FLAG);
    }
    options.open(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found("document not found")
        } else {
            ApiError::from(err)
        }
    })
}

fn read_checked_bytes(path: &Path) -> Result<Vec<u8>, ApiError> {
    let mut file = open_read_only(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>, ApiError> {
    match open_read_only(path) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Err(err) if err.status == axum::http::StatusCode::NOT_FOUND => Ok(None),
        Err(err) => Err(err),
    }
}

fn current_unix_id() -> u128 {
    // UUID v4 provides collision-resistant names without new dependencies.
    uuid::Uuid::new_v4().as_u128()
}

/// Creates a unique temporary file in `parent` with `create_new` semantics.
fn create_temp_file(parent: &Path) -> Result<(File, PathBuf), ApiError> {
    let mut last_error = None;
    for _ in 0..16 {
        let candidate = parent.join(format!(".nimbo-{:032x}.tmp", current_unix_id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                last_error = Some(err);
                continue;
            }
            Err(err) => return Err(err.into()),
        }
    }
    Err(ApiError::from(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "temporary file collision",
        )
    })))
}

fn write_and_sync(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn sync_dir_best_effort(path: &Path) {
    if let Err(err) = sync_dir(path) {
        tracing::warn!(error = %err, path = %path.display(), "could not sync vault directory");
    }
}

fn is_cross_device(err: &std::io::Error) -> bool {
    // EXDEV (18) means the filesystems differ; links cannot cross devices.
    matches!(err.kind(), std::io::ErrorKind::Unsupported) || err.raw_os_error() == Some(18)
}

fn copy_no_replace(from: &Path, to: &Path) -> Result<(), ApiError> {
    let mut source = File::open(from)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut destination = options.open(to).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            ApiError::conflict("destination already exists")
        } else {
            ApiError::from(err)
        }
    })?;
    if let Err(err) = (|| -> std::io::Result<()> {
        std::io::copy(&mut source, &mut destination)?;
        destination.sync_all()
    })() {
        let _ = std::fs::remove_file(to);
        return Err(err.into());
    }
    drop(destination);
    if let Err(err) = std::fs::remove_file(from) {
        let _ = std::fs::remove_file(to);
        return Err(err.into());
    }
    Ok(())
}

/// Renames `from` to `to` without ever replacing an existing destination.
/// `hard_link` gives atomic no-replace semantics on the same filesystem; a
/// creating copy is used when links are unsupported.
fn rename_no_replace(from: &Path, to: &Path) -> Result<(), ApiError> {
    match std::fs::hard_link(from, to) {
        Ok(()) => match std::fs::remove_file(from) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(to);
                Err(err.into())
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(ApiError::conflict("destination already exists"))
        }
        Err(err) if is_cross_device(&err) => copy_no_replace(from, to),
        Err(err) => Err(err.into()),
    }
}

/// Writes `bytes` to a unique temp file, syncs it, then replaces (or creates)
/// `target` atomically and syncs the parent directory. When `expected` is set,
/// the current target bytes are re-read immediately before the atomic rename,
/// so an external writer that changed the file since validation wins with 409
/// instead of being overwritten.
fn write_atomic(
    target: &Path,
    bytes: &[u8],
    replace: bool,
    expected: Option<&[u8]>,
) -> Result<(), ApiError> {
    let parent = target
        .parent()
        .ok_or_else(|| ApiError::bad_request("invalid document path"))?;
    let (mut temp_file, temp_path) = create_temp_file(parent)?;
    if let Err(err) = write_and_sync(&mut temp_file, bytes) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err.into());
    }
    drop(temp_file);

    if let Some(expected) = expected {
        let current = match read_optional_bytes(target) {
            Ok(current) => current,
            Err(err) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(err);
            }
        };
        if current.as_deref() != Some(expected) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(ApiError::conflict("document changed since you opened it"));
        }
    }

    let outcome = if replace {
        std::fs::rename(&temp_path, target).map_err(ApiError::from)
    } else {
        rename_no_replace(&temp_path, target)
    };
    if let Err(err) = outcome {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }
    sync_dir_best_effort(parent);
    Ok(())
}

/// Attempts to return the filesystem to its pre-mutation state after the index
/// transaction failed. Returns whether the compensating action succeeded; when
/// it did not, the caller must report that persisted files and the index may
/// temporarily disagree until the startup reconciliation runs.
fn restore_after_index_failure(target: &Path, previous: Option<&[u8]>) -> bool {
    match previous {
        Some(bytes) => match write_atomic(target, bytes, true, None) {
            Ok(()) => true,
            Err(err) => {
                tracing::error!(error = %err, path = %target.display(), "could not restore document after index failure");
                false
            }
        },
        None => {
            let removed = match std::fs::remove_file(target) {
                Ok(()) => true,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
                Err(err) => {
                    tracing::error!(error = %err, path = %target.display(), "could not remove document after index failure");
                    false
                }
            };
            if let Some(parent) = target.parent() {
                sync_dir_best_effort(parent);
            }
            removed
        }
    }
}

// ------------------------------------------------------------ document model

fn extract_title(path: &str, content: &str) -> String {
    for line in content.lines() {
        if let Some(title) = line.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_string()
}

fn normalize_target(raw: &str) -> Option<String> {
    let target = raw.split('|').next().unwrap_or("").trim();
    let target = target.split('#').next().unwrap_or("").trim();
    if target.is_empty() {
        return None;
    }
    let mut normalized = target.replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized = normalized.trim_start_matches('/').to_string();
    if !normalized.ends_with(".md") {
        normalized.push_str(".md");
    }
    Some(normalized)
}

/// Extracts (raw, normalized-target) wikilink pairs from markdown content.
/// Duplicate occurrences are preserved so the UI/API can keep link counts.
fn parse_links(content: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    for capture in link_regex().captures_iter(content) {
        if links.len() >= MAX_LINKS_PER_DOCUMENT {
            tracing::warn!(
                limit = MAX_LINKS_PER_DOCUMENT,
                "document link count capped while indexing"
            );
            break;
        }
        let raw = capture[1].trim().to_string();
        if let Some(normalized) = normalize_target(&raw) {
            links.push((raw, normalized));
        }
    }
    links
}

/// Deterministic in-memory resolver for wikilinks. Exact (case-insensitive)
/// path matches win; otherwise the basename is matched, preferring the
/// shortest path and then the lexicographically smallest one.
#[derive(Default)]
struct PathMap {
    exact: HashMap<String, String>,
    by_basename: HashMap<String, String>,
}

impl PathMap {
    fn from_paths<I: IntoIterator<Item = String>>(paths: I) -> Self {
        let mut map = Self::default();
        for path in paths {
            map.insert(&path);
        }
        map
    }

    fn insert(&mut self, path: &str) {
        let key = path.to_ascii_lowercase();
        self.exact
            .entry(key.clone())
            .or_insert_with(|| path.to_string());
        let Some(basename) = key.rsplit('/').next() else {
            return;
        };
        match self.by_basename.get(basename) {
            Some(existing) => {
                if (path.len(), path) < (existing.len(), existing.as_str()) {
                    self.by_basename
                        .insert(basename.to_string(), path.to_string());
                }
            }
            None => {
                self.by_basename
                    .insert(basename.to_string(), path.to_string());
            }
        }
    }

    fn resolve(&self, target: &str) -> Option<String> {
        let key = target.to_ascii_lowercase();
        if let Some(path) = self.exact.get(&key) {
            return Some(path.clone());
        }
        let basename = key.rsplit('/').next()?;
        self.by_basename.get(basename).cloned()
    }
}

fn link_may_target_stem(stem: &str, raw: &str) -> bool {
    !stem.is_empty()
        && raw
            .to_ascii_lowercase()
            .contains(&stem.to_ascii_lowercase())
}

fn document_stem(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
}

async fn load_path_map(tx: &mut Transaction<'_, Sqlite>) -> Result<PathMap, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT path FROM documents ORDER BY path")
        .fetch_all(&mut **tx)
        .await?;
    Ok(PathMap::from_paths(rows.into_iter().map(|row| row.0)))
}

async fn upsert_document(
    tx: &mut Transaction<'_, Sqlite>,
    path: &str,
    title: &str,
    hash: &str,
    size: i64,
    owner: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO documents (path, title, content_hash, size, updated_at, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(path) DO UPDATE SET \
           title = excluded.title, \
           content_hash = excluded.content_hash, \
           size = excluded.size, \
           updated_at = excluded.updated_at, \
           owner_id = COALESCE(documents.owner_id, excluded.owner_id)",
    )
    .bind(path)
    .bind(title)
    .bind(hash)
    .bind(size)
    .bind(crate::now())
    .bind(owner)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn write_links_and_fts(
    tx: &mut Transaction<'_, Sqlite>,
    path: &str,
    content: &str,
    title: &str,
    map: &PathMap,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM links WHERE source_path = ?")
        .bind(path)
        .execute(&mut **tx)
        .await?;

    let links = parse_links(content);
    for chunk in links.chunks(LINK_INSERT_CHUNK) {
        let mut builder =
            QueryBuilder::new("INSERT INTO links (source_path, target_raw, target_path) ");
        builder.push_values(chunk.iter(), |mut row, (raw, target)| {
            row.push_bind(path)
                .push_bind(raw.as_str())
                .push_bind(map.resolve(target));
        });
        builder.build().execute(&mut **tx).await?;
    }

    sqlx::query("DELETE FROM docs_fts WHERE path = ?")
        .bind(path)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO docs_fts (path, title, content) VALUES (?, ?, ?)")
        .bind(path)
        .bind(title)
        .bind(content)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Indexes one document against a freshly loaded path map inside `tx`.
async fn index_document_tx(
    tx: &mut Transaction<'_, Sqlite>,
    path: &str,
    content: &str,
    owner: Option<&str>,
) -> Result<PathMap, sqlx::Error> {
    let title = extract_title(path, content);
    let hash = sha256_hex(content.as_bytes());
    upsert_document(tx, path, &title, &hash, content.len() as i64, owner).await?;
    let map = load_path_map(tx).await?;
    write_links_and_fts(tx, path, content, &title, &map).await?;
    Ok(map)
}

/// Indexes one document against a precomputed map (used by the staged startup
/// rebuild, where the map is built from the filesystem listing).
async fn index_document_with_map(
    tx: &mut Transaction<'_, Sqlite>,
    path: &str,
    content: &str,
    owner: Option<&str>,
    map: &PathMap,
) -> Result<(), sqlx::Error> {
    let title = extract_title(path, content);
    upsert_document(
        tx,
        path,
        &title,
        &sha256_hex(content.as_bytes()),
        content.len() as i64,
        owner,
    )
    .await?;
    write_links_and_fts(tx, path, content, &title, map).await
}

/// Resolves only dangling links that mention the stem of `affected` and now
/// resolve to it. All updates are batched inside the caller's transaction.
async fn resolve_dangling_tx(
    tx: &mut Transaction<'_, Sqlite>,
    affected: &str,
    map: &PathMap,
) -> Result<(), sqlx::Error> {
    let stem = document_stem(affected).to_ascii_lowercase();
    if stem.is_empty() {
        return Ok(());
    }

    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, target_raw FROM links \
         WHERE target_path IS NULL AND instr(lower(target_raw), lower(?)) > 0 \
         ORDER BY id LIMIT ?",
    )
    .bind(&stem)
    .bind(MAX_DANGLING_CANDIDATES)
    .fetch_all(&mut **tx)
    .await?;

    let mut ids = Vec::new();
    for (id, raw) in rows {
        if !link_may_target_stem(&stem, &raw) {
            continue;
        }
        let Some(target) = normalize_target(&raw) else {
            continue;
        };
        if map.resolve(&target).as_deref() == Some(affected) {
            ids.push(id);
        }
    }

    for chunk in ids.chunks(UPDATE_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!("UPDATE links SET target_path = ? WHERE id IN ({placeholders})");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(affected);
        for id in chunk {
            query = query.bind(*id);
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}

async fn index_transaction(
    pool: &SqlitePool,
    path: &str,
    content: &str,
    owner: Option<&str>,
) -> Result<(), ApiError> {
    let mut tx = pool.begin().await?;
    let map = index_document_tx(&mut tx, path, content, owner).await?;
    resolve_dangling_tx(&mut tx, path, &map).await?;
    tx.commit().await?;
    Ok(())
}

async fn delete_document_rows(
    tx: &mut Transaction<'_, Sqlite>,
    path: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM documents WHERE path = ?")
        .bind(path)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM links WHERE source_path = ?")
        .bind(path)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE links SET target_path = NULL WHERE target_path = ?")
        .bind(path)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM docs_fts WHERE path = ?")
        .bind(path)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn rename_document_rows(
    tx: &mut Transaction<'_, Sqlite>,
    from: &str,
    to: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM documents WHERE path = ?")
        .bind(from)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM links WHERE source_path = ?")
        .bind(from)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE links SET target_path = ? WHERE target_path = ?")
        .bind(to)
        .bind(from)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM docs_fts WHERE path = ?")
        .bind(from)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn collect_md(dir: &Path, base: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_md(&path, base, out)?;
        } else if file_type.is_file() && path.extension().map(|e| e == "md").unwrap_or(false) {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

/// Rebuilds the document index from the vault inside one transaction. The
/// previous index survives untouched if anything fails, and unreadable files
/// are reported without aborting the whole rebuild.
pub async fn reindex_all(state: &AppState) -> anyhow::Result<()> {
    let _guard = VAULT_MUTATION.lock().await;
    let vault = state.cfg.vault_dir();
    let mut files = Vec::new();
    collect_md(&vault, &vault, &mut files)?;
    files.sort();

    let mut readable: Vec<(String, String)> = Vec::new();
    let mut unreadable: Vec<(String, String)> = Vec::new();
    for rel in files.iter() {
        let rel = rel.as_str();
        let result = resolve_read_path(&vault, rel).and_then(|path| {
            let bytes = read_checked_bytes(&path)?;
            String::from_utf8(bytes).map_err(|_| ApiError::internal("document is not valid UTF-8"))
        });
        match result {
            Ok(content) => readable.push((rel.to_string(), content)),
            Err(err) => {
                tracing::warn!(path = %rel, error = %err, "skipping unreadable vault document");
                unreadable.push((rel.to_string(), err.message));
            }
        }
    }

    if !files.is_empty() && readable.is_empty() {
        anyhow::bail!(
            "none of {} vault documents could be read; keeping the previous index",
            files.len()
        );
    }

    let map = PathMap::from_paths(readable.iter().map(|(path, _)| path.clone()));
    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM documents")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM links").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM docs_fts")
        .execute(&mut *tx)
        .await?;
    for (rel, content) in &readable {
        index_document_with_map(&mut tx, rel, content, None, &map).await?;
    }
    tx.commit().await?;

    if !unreadable.is_empty() {
        tracing::warn!(
            count = unreadable.len(),
            "vault index rebuilt with unreadable documents omitted"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- handlers

pub async fn tree(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT path, title, updated_at FROM documents ORDER BY lower(path)")
            .fetch_all(&state.pool)
            .await?;
    let docs: Vec<Value> = rows
        .into_iter()
        .map(|(path, title, updated_at)| {
            json!({ "path": path, "title": title, "updated_at": updated_at })
        })
        .collect();
    Ok(Json(json!({ "documents": docs })))
}

/// Reads a document with the same containment rules as the HTTP handlers. This
/// is also the intended entry point for agent context reads.
pub fn read_document(state: &AppState, raw_path: &str) -> Result<String, ApiError> {
    let rel = normalize_path(raw_path)?;
    let path = resolve_read_path(&state.cfg.vault_dir(), &rel)?;
    let bytes = read_checked_bytes(&path)?;
    String::from_utf8(bytes).map_err(|_| ApiError::internal("document is not valid UTF-8"))
}

/// Reports whether a vault document exists without exposing its contents.
/// Used by agent draft tools to reject a create that would collide.
pub fn document_exists(state: &AppState, raw_path: &str) -> Result<bool, ApiError> {
    let rel = normalize_path(raw_path)?;
    match resolve_read_path(&state.cfg.vault_dir(), &rel) {
        Ok(_) => Ok(true),
        Err(err) if err.status == axum::http::StatusCode::NOT_FOUND => Ok(false),
        Err(err) => Err(err),
    }
}

pub async fn get_content(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = normalize_path(&params.path)?;
    let content = read_document(&state, &rel)?;
    let hash = sha256_hex(content.as_bytes());
    Ok(Json(json!({
        "path": rel,
        "content": content,
        "hash": hash,
        "title": extract_title(&rel, &content),
    })))
}

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
}

#[derive(Deserialize)]
pub struct SaveRequest {
    pub path: String,
    pub content: String,
    pub base_hash: Option<String>,
}

#[derive(Serialize)]
pub struct SaveResponse {
    pub path: String,
    pub hash: String,
}

pub async fn put_content(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<SaveRequest>,
) -> Result<Json<SaveResponse>, ApiError> {
    let rel = normalize_path(&req.path)?;
    if req.content.len() > MAX_DOCUMENT_BYTES {
        return Err(ApiError::bad_request("document exceeds 8 MiB"));
    }

    let _guard = VAULT_MUTATION.lock().await;
    let vault = state.cfg.vault_dir();
    let path = resolve_write_path(&vault, &rel)?;

    let existing = match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            if !meta.is_file() {
                return Err(ApiError::conflict("path exists and is not a document"));
            }
            Some(read_checked_bytes(&path)?)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    let current_hash = existing.as_ref().map(|bytes| sha256_hex(bytes));

    match (&req.base_hash, &current_hash) {
        (Some(base), Some(current)) if base != current => {
            return Err(ApiError::conflict("document changed since you opened it"));
        }
        (Some(_), None) => {
            return Err(ApiError::conflict("document was deleted"));
        }
        (None, Some(_)) => {
            return Err(ApiError::conflict("document already exists"));
        }
        _ => {}
    }

    let target = prepare_parent_dirs(&vault, &rel)?;
    let replace = existing.is_some();
    write_atomic(
        &target,
        req.content.as_bytes(),
        replace,
        existing.as_deref(),
    )?;

    // Index the bytes that actually persisted. An external editor can still
    // win the commit after our validation; in that case the index follows disk
    // and the API reports a conflict instead of pretending the save applied.
    let persisted = match read_checked_bytes(&target) {
        Ok(persisted) => persisted,
        Err(err) => {
            restore_after_index_failure(&target, existing.as_deref());
            return Err(err);
        }
    };
    let requested_hash = sha256_hex(req.content.as_bytes());
    let persisted_hash = sha256_hex(&persisted);
    let mut concurrent_change = false;
    let content_for_index = if persisted_hash == requested_hash {
        req.content.clone()
    } else {
        concurrent_change = true;
        String::from_utf8(persisted)
            .map_err(|_| ApiError::conflict("document changed during save"))?
    };

    if let Err(err) = index_transaction(&state.pool, &rel, &content_for_index, Some(&user.id)).await
    {
        if !restore_after_index_failure(&target, existing.as_deref()) {
            return Err(ApiError::internal(
                "document was saved but the index could not be updated; reload before retrying",
            ));
        }
        return Err(err);
    }

    if concurrent_change {
        return Err(ApiError::conflict("document changed during save"));
    }

    Ok(Json(SaveResponse {
        path: rel,
        hash: persisted_hash,
    }))
}

pub async fn delete_content(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = normalize_path(&params.path)?;

    let _guard = VAULT_MUTATION.lock().await;
    let vault = state.cfg.vault_dir();
    let path = resolve_read_path(&vault, &rel)?;
    let previous = read_checked_bytes(&path)?;
    let parent = path.parent().map(Path::to_path_buf);

    // Recheck the bytes at the commit point so an external editor's newer
    // content is not deleted out from under it.
    let current = read_checked_bytes(&path)?;
    if current != previous {
        return Err(ApiError::conflict("document changed since you opened it"));
    }
    std::fs::remove_file(&path)?;
    if let Some(parent) = parent.as_deref() {
        sync_dir_best_effort(parent);
    }

    let result = async {
        let mut tx = state.pool.begin().await?;
        delete_document_rows(&mut tx, &rel).await?;
        tx.commit().await?;
        Ok::<(), ApiError>(())
    }
    .await;

    if let Err(err) = result {
        let restored = match write_atomic(&path, &previous, false, None) {
            Ok(()) => true,
            Err(restore_err) => {
                tracing::error!(
                    error = %restore_err,
                    path = %path.display(),
                    "could not restore deleted document after index failure"
                );
                false
            }
        };
        if !restored {
            return Err(ApiError::internal(
                "document was deleted but the index could not be updated; it will be reconciled on restart",
            ));
        }
        return Err(err);
    }

    Ok(Json(json!({ "deleted": rel })))
}

#[derive(Deserialize)]
pub struct RenameRequest {
    pub from: String,
    pub to: String,
}

pub async fn rename(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RenameRequest>,
) -> Result<Json<Value>, ApiError> {
    let from = normalize_path(&req.from)?;
    let to = normalize_path(&req.to)?;
    if from == to {
        return Ok(Json(json!({ "path": to })));
    }

    let _guard = VAULT_MUTATION.lock().await;
    let vault = state.cfg.vault_dir();
    let from_path = resolve_read_path(&vault, &from)?;
    let previous = read_checked_bytes(&from_path)?;

    let to_path = resolve_write_path(&vault, &to)?;
    if std::fs::symlink_metadata(&to_path).is_ok() {
        return Err(ApiError::conflict("destination already exists"));
    }
    let to_path = prepare_parent_dirs(&vault, &to)?;

    // Recheck the source at the commit point; an external writer's newer bytes
    // must not be moved while the index still describes the old content.
    let current = read_checked_bytes(&from_path)?;
    if current != previous {
        return Err(ApiError::conflict("document changed since you opened it"));
    }
    rename_no_replace(&from_path, &to_path)?;
    if let Some(parent) = from_path.parent() {
        sync_dir_best_effort(parent);
    }
    if let Some(parent) = to_path.parent() {
        sync_dir_best_effort(parent);
    }

    // Index the bytes that were actually moved. A change landing in the tiny
    // window after the recheck follows disk and is reported as a conflict.
    let persisted = match read_checked_bytes(&to_path) {
        Ok(persisted) => persisted,
        Err(err) => {
            if let Err(restore_err) = rename_no_replace(&to_path, &from_path) {
                tracing::error!(
                    error = %restore_err,
                    from = %to_path.display(),
                    to = %from_path.display(),
                    "could not restore document after read failure"
                );
            }
            return Err(err);
        }
    };
    let concurrent_change = persisted != previous;
    let content = match String::from_utf8(persisted) {
        Ok(content) => content,
        Err(_) => {
            if let Err(restore_err) = rename_no_replace(&to_path, &from_path) {
                tracing::error!(
                    error = %restore_err,
                    from = %to_path.display(),
                    to = %from_path.display(),
                    "could not restore unreadable document after failed rename"
                );
            }
            return Err(ApiError::conflict("document changed during rename"));
        }
    };

    let result = async {
        let mut tx = state.pool.begin().await?;
        rename_document_rows(&mut tx, &from, &to).await?;
        let map = index_document_tx(&mut tx, &to, &content, None).await?;
        resolve_dangling_tx(&mut tx, &to, &map).await?;
        tx.commit().await?;
        Ok::<(), ApiError>(())
    }
    .await;

    if let Err(err) = result {
        let restored = match rename_no_replace(&to_path, &from_path) {
            Ok(()) => true,
            Err(restore_err) => {
                tracing::error!(
                    error = %restore_err,
                    from = %to_path.display(),
                    to = %from_path.display(),
                    "could not restore document rename after index failure"
                );
                false
            }
        };
        if !restored {
            return Err(ApiError::internal(
                "document was renamed but the index could not be updated; it will be reconciled on restart",
            ));
        }
        return Err(err);
    }

    if concurrent_change {
        return Err(ApiError::conflict("document changed during rename"));
    }

    Ok(Json(json!({ "path": to })))
}

pub async fn backlinks(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = normalize_path(&params.path)?;

    let backlink_rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT l.source_path, d.title FROM links l \
         LEFT JOIN documents d ON d.path = l.source_path \
         WHERE l.target_path = ? ORDER BY lower(l.source_path)",
    )
    .bind(&rel)
    .fetch_all(&state.pool)
    .await?;

    let link_rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, target_raw, target_path FROM links WHERE source_path = ? ORDER BY id",
    )
    .bind(&rel)
    .fetch_all(&state.pool)
    .await?;

    let backlinks: Vec<Value> = backlink_rows
        .into_iter()
        .map(|(path, title)| json!({ "path": path, "title": title.unwrap_or_default() }))
        .collect();
    // Duplicate occurrences are returned independently (each with its row id)
    // so consumers never have to collapse them into one keyed entry.
    let links: Vec<Value> = link_rows
        .into_iter()
        .map(|(id, raw, resolved)| {
            json!({
                "id": id,
                "target": raw,
                "path": resolved,
                "exists": resolved.is_some(),
            })
        })
        .collect();

    Ok(Json(json!({ "backlinks": backlinks, "links": links })))
}

pub(crate) fn build_fts_query(input: &str) -> String {
    let cleaned = word_regex().replace_all(input, " ");
    let terms: Vec<String> = cleaned
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}*"))
        .collect();
    terms.join(" AND ")
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Renders an FTS snippet as escaped text with only the intended `<mark>`
/// elements. Source text can never contribute markup.
fn render_snippet(raw: &str) -> String {
    escape_html(raw)
        .replace('\u{1}', "<mark>")
        .replace('\u{2}', "</mark>")
}

fn escape_like(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

type FtsRow = (String, String, Option<String>, f64);

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let query = params.q.trim().to_string();
    if query.is_empty() {
        return Ok(Json(json!({ "results": [] })));
    }

    let fts_query = build_fts_query(&query);
    if fts_query.is_empty() {
        return Ok(Json(json!({ "results": [] })));
    }

    let fts_result: Result<Vec<FtsRow>, sqlx::Error> = sqlx::query_as(
        "SELECT path, title, \
                snippet(docs_fts, 2, char(1), char(2), '…', 18) AS snip, \
                bm25(docs_fts) AS score \
         FROM docs_fts WHERE docs_fts MATCH ? ORDER BY score LIMIT 50",
    )
    .bind(&fts_query)
    .fetch_all(&state.pool)
    .await;

    let results: Vec<Value> = match fts_result {
        Ok(rows) => rows
            .into_iter()
            .map(|(path, title, snippet, score)| {
                json!({
                    "path": path,
                    "title": title,
                    "snippet": render_snippet(snippet.as_deref().unwrap_or_default()),
                    "score": score,
                })
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "fts query failed, falling back to LIKE");
            let like = format!("%{}%", escape_like(&query));
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT path, title FROM documents \
                 WHERE lower(title) LIKE lower(?) ESCAPE '\\' \
                    OR lower(path) LIKE lower(?) ESCAPE '\\' LIMIT 50",
            )
            .bind(&like)
            .bind(&like)
            .fetch_all(&state.pool)
            .await?;
            rows.into_iter()
                .map(|(path, title)| {
                    json!({ "path": path, "title": title, "snippet": "", "score": 0.0 })
                })
                .collect()
        }
    };

    Ok(Json(json!({ "results": results })))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    struct TempVault(PathBuf);

    impl TempVault {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("nimbo-docs-test-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create temp vault");
            Self(path)
        }
    }

    impl Drop for TempVault {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn memory_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("memory sqlite");
        for statement in [
            "CREATE TABLE documents (path TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', \
             content_hash TEXT NOT NULL DEFAULT '', size INTEGER NOT NULL DEFAULT 0, \
             updated_at INTEGER NOT NULL DEFAULT 0, owner_id TEXT)",
            "CREATE TABLE links (id INTEGER PRIMARY KEY AUTOINCREMENT, \
             source_path TEXT NOT NULL, target_raw TEXT NOT NULL, target_path TEXT)",
            "CREATE VIRTUAL TABLE docs_fts USING fts5(path UNINDEXED, title, content, \
             tokenize = 'porter unicode61')",
        ] {
            sqlx::query(statement).execute(&pool).await.expect("schema");
        }
        pool
    }

    #[test]
    fn rejects_traversal() {
        assert!(normalize_path("../etc/passwd").is_err());
        assert!(normalize_path("notes/../../x.md").is_err());
        assert!(normalize_path("/abs.md").is_err());
        assert!(normalize_path("notes.txt").is_err());
    }

    #[test]
    fn normalizes_valid_paths() {
        assert_eq!(normalize_path("notes/idea.md").unwrap(), "notes/idea.md");
        assert_eq!(normalize_path("./notes//idea.md").unwrap(), "notes/idea.md");
        assert_eq!(normalize_path("a\\b\\c.md").unwrap(), "a/b/c.md");
    }

    #[test]
    fn parses_wikilinks() {
        let links = parse_links("See [[Other Note]] and [[dir/file.md|alias]] and [[#heading]].");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].1, "Other Note.md");
        assert_eq!(links[1].1, "dir/file.md");
    }

    #[test]
    fn duplicate_links_are_preserved() {
        let links = parse_links("[[Target]] then [[target]] and [[dir/Target|alias]]");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].1, "Target.md");
        assert_eq!(links[1].1, "target.md");
        assert_eq!(links[2].1, "dir/Target.md");
    }

    #[test]
    fn link_count_is_bounded() {
        let mut content = String::new();
        for _ in 0..(MAX_LINKS_PER_DOCUMENT + 25) {
            content.push_str("[[A]] ");
        }
        assert_eq!(parse_links(&content).len(), MAX_LINKS_PER_DOCUMENT);
    }

    #[test]
    fn fts_query_is_sanitized() {
        assert_eq!(build_fts_query("rust + axum"), "rust* AND axum*");
        assert_eq!(build_fts_query("!!!"), "");
    }

    #[test]
    fn snippets_escape_source_html() {
        let raw = "<img src=x onerror=alert(1)> \u{1}hit\u{2} <script>bad()</script>";
        let rendered = render_snippet(raw);
        assert!(rendered.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(rendered.contains("<mark>hit</mark>"));
        assert!(rendered.contains("&lt;script&gt;bad()&lt;/script&gt;"));
        assert!(!rendered.contains("<img"));
        assert!(!rendered.contains("<script"));
        assert_eq!(
            rendered.matches('<').count(),
            rendered.matches("<mark>").count() * 2
        );
    }

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("100%_foo\\"), "100\\%\\_foo\\\\");
    }

    #[test]
    fn wikilink_resolution_does_not_treat_wildcards() {
        let map = PathMap::from_paths(vec![
            "notes/foo_bar.md".to_string(),
            "notes/fooXbar.md".to_string(),
            "notes/foo%bar.md".to_string(),
        ]);
        assert_eq!(
            map.resolve("foo_bar.md").as_deref(),
            Some("notes/foo_bar.md")
        );
        assert_eq!(
            map.resolve("FOO_BAR.md").as_deref(),
            Some("notes/foo_bar.md")
        );
        assert_eq!(
            map.resolve("foo%bar.md").as_deref(),
            Some("notes/foo%bar.md")
        );
        assert_eq!(map.resolve("fooQbar.md"), None);
        assert_eq!(map.resolve("zzz%zzz.md"), None);
    }

    #[test]
    fn basename_fallback_is_deterministic() {
        let map = PathMap::from_paths(vec![
            "z/Note.md".to_string(),
            "deep/dir/Note.md".to_string(),
            "a/Note.md".to_string(),
        ]);
        assert_eq!(map.resolve("Note.md").as_deref(), Some("a/Note.md"));
        assert_eq!(map.resolve("unknown/Note.md").as_deref(), Some("a/Note.md"));
        assert_eq!(map.resolve("missing.md"), None);
    }

    #[test]
    fn dangling_filter_only_matches_relevant_links() {
        assert!(link_may_target_stem("note", "[[Note|alias]]"));
        assert!(link_may_target_stem("note", "dir/Note.md"));
        assert!(!link_may_target_stem("note", "[[Other]]"));
        assert!(!link_may_target_stem("", "[[Note]]"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_final_component_is_rejected() {
        let vault = TempVault::new("symlink-final");
        let outside = TempVault::new("symlink-target");
        std::fs::write(outside.0.join("secret.txt"), "host secret").unwrap();
        std::os::unix::fs::symlink(outside.0.join("secret.txt"), vault.0.join("leak.md")).unwrap();

        assert!(resolve_read_path(&vault.0, "leak.md").is_err());
        assert!(resolve_write_path(&vault.0, "leak.md").is_err());
        assert!(prepare_parent_dirs(&vault.0, "leak.md").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_directory_is_rejected() {
        let vault = TempVault::new("symlink-parent");
        let outside = TempVault::new("symlink-parent-target");
        std::fs::create_dir_all(outside.0.join("nested")).unwrap();
        std::fs::write(outside.0.join("nested/doc.md"), "outside").unwrap();
        std::os::unix::fs::symlink(outside.0.join("nested"), vault.0.join("linked")).unwrap();

        assert!(resolve_read_path(&vault.0, "linked/doc.md").is_err());
        assert!(resolve_write_path(&vault.0, "linked/new.md").is_err());
        assert!(prepare_parent_dirs(&vault.0, "linked/new.md").is_err());
    }

    #[test]
    fn write_path_creates_directories_beneath_vault() {
        let vault = TempVault::new("create-dirs");
        let target = prepare_parent_dirs(&vault.0, "one/two/note.md").unwrap();
        assert!(target.starts_with(vault.0.canonicalize().unwrap()));
        assert!(vault.0.join("one/two").is_dir());
    }

    #[test]
    fn rename_no_replace_refuses_existing_destination() {
        let dir = TempVault::new("rename-no-replace");
        let from = dir.0.join("from.md");
        let to = dir.0.join("to.md");
        std::fs::write(&from, "from").unwrap();
        std::fs::write(&to, "to").unwrap();

        let err = rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(std::fs::read_to_string(&from).unwrap(), "from");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "to");
    }

    #[test]
    fn temp_files_are_unique_and_cleaned_up() {
        let dir = TempVault::new("temp-unique");
        let (first, first_path) = create_temp_file(&dir.0).unwrap();
        let (second, second_path) = create_temp_file(&dir.0).unwrap();
        assert_ne!(first_path, second_path);
        assert!(first_path.starts_with(&dir.0));
        drop(first);
        drop(second);
        std::fs::remove_file(&first_path).unwrap();
        std::fs::remove_file(&second_path).unwrap();

        let target = dir.0.join("note.md");
        write_atomic(&target, b"hello", false, None).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_refuses_external_change_at_commit() {
        let dir = TempVault::new("commit-recheck");
        let target = dir.0.join("note.md");
        std::fs::write(&target, "external edit").unwrap();

        let err = write_atomic(&target, b"api edit", true, Some(b"opened base")).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external edit");
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);

        write_atomic(&target, b"api edit", true, Some(b"external edit")).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "api edit");
    }

    #[tokio::test]
    async fn fts_snippet_markers_execute_and_escape() {
        let pool = memory_pool().await;
        sqlx::query("INSERT INTO docs_fts (path, title, content) VALUES (?, ?, ?)")
            .bind("x.md")
            .bind("X")
            .bind("before <img src=x onerror=alert(1)> hit after")
            .execute(&pool)
            .await
            .unwrap();

        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT path, snippet(docs_fts, 2, char(1), char(2), '…', 18) \
             FROM docs_fts WHERE docs_fts MATCH ? ORDER BY bm25(docs_fts) LIMIT 1",
        )
        .bind("hit*")
        .fetch_one(&pool)
        .await
        .unwrap();
        let rendered = render_snippet(row.1.as_deref().unwrap_or_default());
        assert!(rendered.contains("<mark>hit</mark>"));
        assert!(rendered.contains("&lt;img"));
        assert!(!rendered.contains("<img"));
    }

    #[tokio::test]
    async fn index_transaction_rolls_back_all_tables() {
        let pool = memory_pool().await;
        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "a.md", "[[b]] [[b]]", None)
            .await
            .unwrap();
        tx.rollback().await.unwrap();

        let documents: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM documents")
            .fetch_one(&pool)
            .await
            .unwrap();
        let links: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM links")
            .fetch_one(&pool)
            .await
            .unwrap();
        let fts: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM docs_fts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((documents.0, links.0, fts.0), (0, 0, 0));
    }

    #[tokio::test]
    async fn index_transaction_keeps_duplicates_and_resolution() {
        let pool = memory_pool().await;
        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "a.md", "[[b]] [[b]]", None)
            .await
            .unwrap();
        index_document_tx(&mut tx, "b.md", "body", None)
            .await
            .unwrap();
        let map = load_path_map(&mut tx).await.unwrap();
        resolve_dangling_tx(&mut tx, "b.md", &map).await.unwrap();
        tx.commit().await.unwrap();

        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT target_raw, target_path FROM links ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 2);
        for (raw, target) in rows {
            assert_eq!(raw, "b");
            assert_eq!(target.as_deref(), Some("b.md"));
        }
        let fts: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM docs_fts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fts.0, 2);
    }

    #[tokio::test]
    async fn index_failure_preserves_previous_rows() {
        let pool = memory_pool().await;
        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "a.md", "[[b]]", None)
            .await
            .unwrap();
        index_document_tx(&mut tx, "b.md", "old body", None)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "b.md", "new body [[a]]", None)
            .await
            .unwrap();
        let failing = sqlx::query("INSERT INTO links (source_path, target_raw) VALUES (NULL, 'x')")
            .execute(&mut *tx)
            .await;
        assert!(failing.is_err());
        tx.rollback().await.unwrap();

        let body: (String,) = sqlx::query_as("SELECT content FROM docs_fts WHERE path = 'b.md'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(body.0, "old body");
        let links: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM links WHERE source_path = 'b.md'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(links.0, 0);
    }

    #[tokio::test]
    async fn delete_nulls_shared_targets_and_removes_fts() {
        let pool = memory_pool().await;
        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "a.md", "[[b]]", None)
            .await
            .unwrap();
        index_document_tx(&mut tx, "b.md", "body", None)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        delete_document_rows(&mut tx, "b.md").await.unwrap();
        tx.commit().await.unwrap();

        let target: (Option<String>,) =
            sqlx::query_as("SELECT target_path FROM links WHERE source_path = 'a.md'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(target.0, None);
        let documents: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM documents WHERE path = 'b.md'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let fts: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM docs_fts WHERE path = 'b.md'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((documents.0, fts.0), (0, 0));
    }

    #[tokio::test]
    async fn rename_retargets_incoming_links_and_fts() {
        let pool = memory_pool().await;
        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "a.md", "[[b]]", None)
            .await
            .unwrap();
        let map = index_document_tx(&mut tx, "b.md", "body", None)
            .await
            .unwrap();
        resolve_dangling_tx(&mut tx, "b.md", &map).await.unwrap();
        tx.commit().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        rename_document_rows(&mut tx, "b.md", "c.md").await.unwrap();
        index_document_tx(&mut tx, "c.md", "body", None)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let target: (Option<String>,) =
            sqlx::query_as("SELECT target_path FROM links WHERE source_path = 'a.md'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(target.0.as_deref(), Some("c.md"));
        let documents: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM documents WHERE path IN ('b.md', 'c.md')")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(documents.0, 1);
        let fts: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM docs_fts WHERE path = 'c.md'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fts.0, 1);
    }

    #[tokio::test]
    async fn dangling_links_resolve_to_new_document_only() {
        let pool = memory_pool().await;
        let mut tx = pool.begin().await.unwrap();
        index_document_tx(&mut tx, "a.md", "[[Target]] [[Other]]", None)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let mut tx = pool.begin().await.unwrap();
        let map = index_document_tx(&mut tx, "notes/Target.md", "body", None)
            .await
            .unwrap();
        resolve_dangling_tx(&mut tx, "notes/Target.md", &map)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT target_raw, target_path FROM links ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows[0],
            ("Target".to_string(), Some("notes/Target.md".to_string()))
        );
        assert_eq!(rows[1], ("Other".to_string(), None));
    }
}
