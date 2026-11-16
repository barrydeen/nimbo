use crate::{auth::AuthUser, error::ApiError, state::AppState, util::sha256_hex};
use axum::{
    extract::{Extension, Query, State},
    Json,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

fn link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\[([^\]\n]{1,300})\]\]").expect("valid regex"))
}

fn word_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^\p{L}\p{N}_]+").expect("valid regex"))
}

/// Validates and canonicalizes a vault-relative document path.
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

fn vault_path(state: &AppState, rel: &str) -> PathBuf {
    state.cfg.vault_dir().join(rel)
}

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
fn parse_links(content: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    for capture in link_regex().captures_iter(content) {
        let raw = capture[1].trim().to_string();
        if let Some(normalized) = normalize_target(&raw) {
            links.push((raw, normalized));
        }
    }
    links
}

/// Re-resolves links that previously pointed at documents which did not exist yet.
async fn resolve_dangling_links(state: &AppState) -> Result<(), ApiError> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, target_raw FROM links WHERE target_path IS NULL")
            .fetch_all(&state.pool)
            .await?;
    for (id, raw) in rows {
        let Some(target) = normalize_target(&raw) else {
            continue;
        };
        if let Some(path) = resolve_target(&state.pool, &target).await? {
            sqlx::query("UPDATE links SET target_path = ? WHERE id = ?")
                .bind(path)
                .bind(id)
                .execute(&state.pool)
                .await?;
        }
    }
    Ok(())
}

async fn resolve_target(pool: &sqlx::SqlitePool, target: &str) -> Result<Option<String>, ApiError> {
    let basename = format!("%/{}", target);
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT path FROM documents \
         WHERE lower(path) = lower(?) OR lower(path) LIKE lower(?) \
         ORDER BY length(path) LIMIT 1",
    )
    .bind(target)
    .bind(basename)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.0))
}

async fn index_document(
    state: &AppState,
    path: &str,
    content: &str,
    owner: Option<&str>,
) -> Result<(), ApiError> {
    let hash = sha256_hex(content.as_bytes());
    let title = extract_title(path, content);
    let size = content.len() as i64;
    let now = crate::now();

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
    .bind(&title)
    .bind(&hash)
    .bind(size)
    .bind(now)
    .bind(owner)
    .execute(&state.pool)
    .await?;

    sqlx::query("DELETE FROM links WHERE source_path = ?")
        .bind(path)
        .execute(&state.pool)
        .await?;

    for (raw, target) in parse_links(content) {
        let resolved = resolve_target(&state.pool, &target).await?;
        sqlx::query("INSERT INTO links (source_path, target_raw, target_path) VALUES (?, ?, ?)")
            .bind(path)
            .bind(&raw)
            .bind(resolved)
            .execute(&state.pool)
            .await?;
    }

    sqlx::query("DELETE FROM docs_fts WHERE path = ?")
        .bind(path)
        .execute(&state.pool)
        .await?;
    sqlx::query("INSERT INTO docs_fts (path, title, content) VALUES (?, ?, ?)")
        .bind(path)
        .bind(&title)
        .bind(content)
        .execute(&state.pool)
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

pub async fn reindex_all(state: &AppState) -> anyhow::Result<()> {
    let vault = state.cfg.vault_dir();
    let mut files = Vec::new();
    collect_md(&vault, &vault, &mut files)?;

    sqlx::query("DELETE FROM documents")
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM links")
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM docs_fts")
        .execute(&state.pool)
        .await?;

    for rel in files {
        if let Ok(content) = std::fs::read_to_string(vault.join(&rel)) {
            index_document(state, &rel, &content, None).await?;
        }
    }
    resolve_dangling_links(state).await?;
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

pub async fn get_content(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = normalize_path(&params.path)?;
    let path = vault_path(&state, &rel);
    let content =
        std::fs::read_to_string(&path).map_err(|_| ApiError::not_found("document not found"))?;
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
    if req.content.len() > 8 * 1024 * 1024 {
        return Err(ApiError::bad_request("document exceeds 8 MiB"));
    }

    let path = vault_path(&state, &rel);
    let existing = std::fs::read_to_string(&path).ok();
    let current_hash = existing.as_ref().map(|c| sha256_hex(c.as_bytes()));

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

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, req.content.as_bytes())?;
    std::fs::rename(&tmp, &path)?;

    index_document(&state, &rel, &req.content, Some(&user.id)).await?;
    resolve_dangling_links(&state).await?;

    Ok(Json(SaveResponse {
        path: rel,
        hash: sha256_hex(req.content.as_bytes()),
    }))
}

pub async fn delete_content(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PathQuery>,
) -> Result<Json<Value>, ApiError> {
    let rel = normalize_path(&params.path)?;
    let path = vault_path(&state, &rel);
    if !path.exists() {
        return Err(ApiError::not_found("document not found"));
    }
    std::fs::remove_file(&path)?;

    sqlx::query("DELETE FROM documents WHERE path = ?")
        .bind(&rel)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM links WHERE source_path = ?")
        .bind(&rel)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE links SET target_path = NULL WHERE target_path = ?")
        .bind(&rel)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM docs_fts WHERE path = ?")
        .bind(&rel)
        .execute(&state.pool)
        .await?;

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

    let from_path = vault_path(&state, &from);
    let to_path = vault_path(&state, &to);
    if !from_path.exists() {
        return Err(ApiError::not_found("source document not found"));
    }
    if to_path.exists() {
        return Err(ApiError::conflict("destination already exists"));
    }
    if let Some(parent) = to_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&from_path, &to_path)?;

    let content = std::fs::read_to_string(&to_path)?;

    sqlx::query("DELETE FROM documents WHERE path = ?")
        .bind(&from)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM links WHERE source_path = ?")
        .bind(&from)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM docs_fts WHERE path = ?")
        .bind(&from)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE links SET target_path = ? WHERE target_path = ?")
        .bind(&to)
        .bind(&from)
        .execute(&state.pool)
        .await?;

    index_document(&state, &to, &content, None).await?;
    resolve_dangling_links(&state).await?;

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

    let link_rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT target_raw, target_path FROM links WHERE source_path = ? ORDER BY id",
    )
    .bind(&rel)
    .fetch_all(&state.pool)
    .await?;

    let backlinks: Vec<Value> = backlink_rows
        .into_iter()
        .map(|(path, title)| json!({ "path": path, "title": title.unwrap_or_default() }))
        .collect();
    let links: Vec<Value> = link_rows
        .into_iter()
        .map(|(raw, resolved)| {
            json!({
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
                snippet(docs_fts, 2, '<mark>', '</mark>', '…', 18) AS snip, \
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
                    "snippet": snippet.unwrap_or_default(),
                    "score": score,
                })
            })
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "fts query failed, falling back to LIKE");
            let like = format!("%{}%", query);
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT path, title FROM documents \
                 WHERE lower(title) LIKE lower(?) OR lower(path) LIKE lower(?) LIMIT 50",
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
    fn fts_query_is_sanitized() {
        assert_eq!(build_fts_query("rust + axum"), "rust* AND axum*");
        assert_eq!(build_fts_query("!!!"), "");
    }
}
