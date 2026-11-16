use crate::{error::ApiError, now};
use serde::Serialize;
use sqlx::SqlitePool;
use std::path::PathBuf;

pub mod extract;
pub mod storage;
pub mod vision;
pub mod worker;

pub const ROOT_ID: &str = "root";
pub const MAX_NAME_BYTES: usize = 255;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Node {
    pub id: String,
    pub parent_id: String,
    pub kind: String,
    pub name: String,
    #[allow(dead_code)]
    pub owner_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub blob_sha: Option<String>,
    pub mime: Option<String>,
    pub size: i64,
    pub thumb_sha: Option<String>,
    pub index_status: String,
    pub index_error: Option<String>,
    pub text_content: Option<String>,
    pub metadata_json: String,
    pub vision_status: String,
    pub vision_description: Option<String>,
    pub vision_text: Option<String>,
    pub vision_tags: Option<String>,
    pub vision_model: Option<String>,
    pub vision_at: Option<i64>,
    pub vision_error: Option<String>,
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.kind == "folder"
    }

    pub fn is_file(&self) -> bool {
        self.kind == "file"
    }

    pub fn blob_path(&self, state: &crate::state::AppState) -> Result<PathBuf, ApiError> {
        let sha = self
            .blob_sha
            .as_deref()
            .ok_or_else(|| ApiError::internal("file has no stored content"))?;
        storage::blob_path(state, sha)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Crumb {
    pub id: String,
    pub name: String,
}

/// Validates a user-supplied file or folder name. Names are single virtual path
/// segments, so separators and control characters are rejected.
pub fn validate_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    if name == "." || name == ".." {
        return Err(ApiError::bad_request("name may not be '.' or '..'"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(ApiError::bad_request("name is too long"));
    }
    if name.contains('\0') {
        return Err(ApiError::bad_request("name contains a null byte"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(ApiError::bad_request(
            "name may not contain path separators",
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(ApiError::bad_request("name contains control characters"));
    }
    Ok(name.to_string())
}

/// Splits `report.final.pdf` into (`report.final`, `pdf`).
pub fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => {
            (&name[..index], Some(&name[index + 1..]))
        }
        _ => (name, None),
    }
}

fn fit_name(stem: &str, suffix: &str, ext: Option<&str>) -> String {
    let mut stem = stem.to_string();
    loop {
        let name = match ext {
            Some(ext) => format!("{stem}{suffix}.{ext}"),
            None => format!("{stem}{suffix}"),
        };
        if name.len() <= MAX_NAME_BYTES || stem.is_empty() {
            return name;
        }
        stem.pop();
    }
}

/// Returns a sibling-unique name, auto-suffixing duplicates the way desktop file
/// managers do (`photo.jpg`, `photo (2).jpg`).
pub async fn unique_child_name(
    pool: &SqlitePool,
    parent_id: &str,
    name: &str,
    exclude_id: Option<&str>,
) -> Result<String, ApiError> {
    let (stem, ext) = split_extension(name);
    for attempt in 1..10_000 {
        let candidate = if attempt == 1 {
            name.to_string()
        } else {
            fit_name(stem, &format!(" ({attempt})"), ext)
        };
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM nodes WHERE parent_id = ? AND name = ? COLLATE NOCASE \
             AND (? IS NULL OR id <> ?) LIMIT 1",
        )
        .bind(parent_id)
        .bind(&candidate)
        .bind(exclude_id)
        .bind(exclude_id)
        .fetch_optional(pool)
        .await?;
        if exists.is_none() {
            return Ok(candidate);
        }
    }
    Err(ApiError::conflict("too many files with a similar name"))
}

pub async fn get_node(pool: &SqlitePool, id: &str) -> Result<Option<Node>, ApiError> {
    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(node)
}

pub async fn require_node(pool: &SqlitePool, id: &str) -> Result<Node, ApiError> {
    get_node(pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("item not found"))
}

pub async fn require_folder(pool: &SqlitePool, id: &str) -> Result<Node, ApiError> {
    let node = require_node(pool, id).await?;
    if !node.is_folder() {
        return Err(ApiError::bad_request("destination is not a folder"));
    }
    Ok(node)
}

pub async fn breadcrumbs(pool: &SqlitePool, folder_id: &str) -> Result<Vec<Crumb>, ApiError> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "WITH RECURSIVE chain(id, name, parent_id, depth) AS ( \
           SELECT id, name, parent_id, 0 FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, n.name, n.parent_id, chain.depth + 1 \
           FROM nodes n JOIN chain ON n.id = chain.parent_id \
           WHERE chain.parent_id <> '' \
         ) \
         SELECT id, name, depth FROM chain ORDER BY depth DESC",
    )
    .bind(folder_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, _)| Crumb { id, name })
        .collect())
}

/// Full virtual path for a node, e.g. `photos/2026/beach.jpg`.
pub async fn path_for(pool: &SqlitePool, id: &str) -> Result<String, ApiError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "WITH RECURSIVE chain(id, name, parent_id, depth) AS ( \
           SELECT id, name, parent_id, 0 FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, n.name, n.parent_id, chain.depth + 1 \
           FROM nodes n JOIN chain ON n.id = chain.parent_id \
           WHERE chain.parent_id <> '' \
         ) \
         SELECT name, depth FROM chain ORDER BY depth DESC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, _)| name)
        .collect::<Vec<_>>()
        .join("/"))
}

/// True when `candidate` is `ancestor` or lives somewhere beneath it.
pub async fn is_within(
    pool: &SqlitePool,
    ancestor_id: &str,
    candidate_id: &str,
) -> Result<bool, ApiError> {
    let row: Option<(String,)> = sqlx::query_as(
        "WITH RECURSIVE subtree(id) AS ( \
           SELECT id FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id FROM nodes n JOIN subtree s ON n.parent_id = s.id \
         ) \
         SELECT id FROM subtree WHERE id = ? LIMIT 1",
    )
    .bind(ancestor_id)
    .bind(candidate_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

pub async fn subtree_ids(pool: &SqlitePool, root_id: &str) -> Result<Vec<String>, ApiError> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "WITH RECURSIVE subtree(id) AS ( \
           SELECT id FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id FROM nodes n JOIN subtree s ON n.parent_id = s.id \
         ) \
         SELECT id FROM subtree",
    )
    .bind(root_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Ensures a file/folder name exists exactly once in its parent and inserts it.
pub async fn insert_node(
    pool: &SqlitePool,
    parent_id: &str,
    kind: &str,
    name: &str,
    owner_id: Option<&str>,
) -> Result<Node, ApiError> {
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    sqlx::query(
        "INSERT INTO nodes (id, parent_id, kind, name, owner_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(parent_id)
    .bind(kind)
    .bind(name)
    .bind(owner_id)
    .bind(ts)
    .bind(ts)
    .execute(pool)
    .await?;
    require_node(pool, &id).await
}

pub async fn upsert_fts(
    pool: &SqlitePool,
    node: &Node,
    text: Option<&str>,
    description: Option<&str>,
    metadata: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM files_fts WHERE file_id = ?")
        .bind(&node.id)
        .execute(pool)
        .await?;
    if node.is_file() {
        sqlx::query(
            "INSERT INTO files_fts (file_id, name, text, description, metadata) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&node.id)
        .bind(&node.name)
        .bind(text.or(node.text_content.as_deref()).unwrap_or(""))
        .bind(
            description
                .or(node.vision_description.as_deref())
                .unwrap_or(""),
        )
        .bind(metadata.unwrap_or(""))
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_reject_separators_and_dots() {
        assert!(validate_name("notes.txt").is_ok());
        assert!(validate_name("  notes.txt  ").is_ok());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("bad\nname").is_err());
    }

    #[test]
    fn extensions_split_at_last_dot() {
        assert_eq!(
            split_extension("report.final.pdf"),
            ("report.final", Some("pdf"))
        );
        assert_eq!(split_extension(".env"), (".env", None));
        assert_eq!(split_extension("README"), ("README", None));
    }

    #[test]
    fn suffixed_names_fit_the_limit() {
        let stem = "x".repeat(MAX_NAME_BYTES);
        let fitted = fit_name(&stem, " (2)", Some("pdf"));
        assert!(fitted.len() <= MAX_NAME_BYTES);
        assert!(fitted.ends_with(".pdf"));
    }
}
