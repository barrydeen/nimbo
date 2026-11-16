use crate::{auth::AuthUser, error::ApiError, now};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};
use std::path::PathBuf;

pub mod extract;
pub mod lock;
pub mod storage;
pub mod vision;
pub mod worker;

pub const ROOT_ID: &str = "root";
pub const MAX_NAME_BYTES: usize = 255;
/// Bound for recursive tree traversal. Cycles are prevented by the visited set;
/// the depth bound is defence in depth for corrupt data.
pub const MAX_TREE_DEPTH: usize = 256;
/// Upper bound on a single subtree operation (delete/zip/authorization scan).
pub const MAX_SUBTREE_NODES: usize = 200_000;

/// Recursive subtree walk that is cycle-safe and depth-bounded. Binds the root id.
pub const SUBTREE_CTE: &str = "WITH RECURSIVE subtree(id, visited) AS ( \
   SELECT id, ',' || id || ',' FROM nodes WHERE id = ? \
   UNION ALL \
   SELECT n.id, subtree.visited || n.id || ',' FROM nodes n \
     JOIN subtree ON n.parent_id = subtree.id \
   WHERE instr(subtree.visited, ',' || n.id || ',') = 0 \
     AND length(subtree.visited) < 40000";

/// Scope-limited subtree walk used by backup-token queries. Binds the scope root
/// first. With the drive root as seed it covers every reachable node, which also
/// hides unreachable/orphaned rows from listings and search.
pub const SCOPE_CTE: &str = "WITH RECURSIVE scope(id, visited) AS ( \
   SELECT id, ',' || id || ',' FROM nodes WHERE id = ? \
   UNION ALL \
   SELECT n.id, scope.visited || n.id || ',' FROM nodes n \
     JOIN scope ON n.parent_id = scope.id \
   WHERE instr(scope.visited, ',' || n.id || ',') = 0 \
     AND length(scope.visited) < 40000";

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

/// Authorization scope derived from the authenticated principal. Full-scope
/// sessions and (backward-compatible) device tokens see the whole drive; a
/// backup-scope token is confined to its assigned backup root subtree.
#[derive(Debug, Clone)]
pub struct Scope {
    pub root: String,
    pub restricted: bool,
}

impl Scope {
    pub fn for_user(user: &AuthUser) -> Result<Self, ApiError> {
        if user.scope == "full" {
            return Ok(Self {
                root: ROOT_ID.to_string(),
                restricted: false,
            });
        }
        let root = user
            .backup_root_id
            .as_deref()
            .filter(|root| !root.trim().is_empty())
            .ok_or_else(|| ApiError::forbidden("this token is not bound to a backup folder"))?;
        Ok(Self {
            root: root.to_string(),
            restricted: true,
        })
    }
}

/// Denies non-file surfaces to restricted tokens. Backup tokens may only use
/// instance/auth-self/logout and the file routes.
pub fn require_full_scope(user: &AuthUser) -> Result<(), ApiError> {
    if user.scope == "full" {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "this token is restricted to backup file access",
        ))
    }
}

/// Parses `name` into `(stem, extension)` at the last dot.
pub fn split_extension(name: &str) -> (&str, Option<&str>) {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => {
            (&name[..index], Some(&name[index + 1..]))
        }
        _ => (name, None),
    }
}

/// Returns a prefix of `text` that is at most `max` bytes and ends on a character
/// boundary.
fn take_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

/// Fits `stem + suffix + "." + ext` into the 255-byte name budget. The extension
/// is shortened first, then the stem, so the complete candidate always fits even
/// for one-byte stems with enormous extensions.
fn fit_name(stem: &str, suffix: &str, ext: Option<&str>) -> String {
    let max_ext = MAX_NAME_BYTES.saturating_sub(suffix.len() + 2);
    let ext = ext
        .map(|ext| take_bytes(ext, max_ext))
        .filter(|ext| !ext.is_empty());
    let ext_part = ext.map(|ext| format!(".{ext}")).unwrap_or_default();
    let budget = MAX_NAME_BYTES.saturating_sub(suffix.len() + ext_part.len());
    let stem = take_bytes(stem, budget);
    format!("{stem}{suffix}{ext_part}")
}

/// Returns a sibling-unique name, auto-suffixing duplicates the way desktop file
/// managers do (`photo.jpg`, `photo (2).jpg`).
pub async fn unique_child_name_conn(
    conn: &mut SqliteConnection,
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
        if candidate.is_empty() || candidate.len() > MAX_NAME_BYTES {
            continue;
        }
        let exists: Option<(String,)> = sqlx::query_as(
            "SELECT id FROM nodes WHERE parent_id = ? AND name = ? COLLATE NOCASE \
             AND (? IS NULL OR id <> ?) LIMIT 1",
        )
        .bind(parent_id)
        .bind(&candidate)
        .bind(exclude_id)
        .bind(exclude_id)
        .fetch_optional(&mut *conn)
        .await?;
        if exists.is_none() {
            return Ok(candidate);
        }
    }
    Err(ApiError::conflict("too many files with a similar name"))
}

pub async fn get_node(pool: &SqlitePool, id: &str) -> Result<Option<Node>, ApiError> {
    let mut conn = pool.acquire().await?;
    get_node_conn(&mut conn, id).await
}

pub async fn get_node_conn(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<Node>, ApiError> {
    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(node)
}

pub async fn require_node(pool: &SqlitePool, id: &str) -> Result<Node, ApiError> {
    get_node(pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("item not found"))
}

pub async fn require_node_conn(conn: &mut SqliteConnection, id: &str) -> Result<Node, ApiError> {
    get_node_conn(conn, id)
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

pub async fn require_folder_conn(conn: &mut SqliteConnection, id: &str) -> Result<Node, ApiError> {
    let node = require_node_conn(conn, id).await?;
    if !node.is_folder() {
        return Err(ApiError::bad_request("destination is not a folder"));
    }
    Ok(node)
}

pub async fn breadcrumbs(pool: &SqlitePool, folder_id: &str) -> Result<Vec<Crumb>, ApiError> {
    let mut conn = pool.acquire().await?;
    breadcrumbs_conn(&mut conn, folder_id).await
}

pub async fn breadcrumbs_conn(
    conn: &mut SqliteConnection,
    folder_id: &str,
) -> Result<Vec<Crumb>, ApiError> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "WITH RECURSIVE chain(id, name, parent_id, depth, visited) AS ( \
           SELECT id, name, parent_id, 0, ',' || id || ',' FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, n.name, n.parent_id, chain.depth + 1, chain.visited || n.id || ',' \
           FROM nodes n JOIN chain ON n.id = chain.parent_id \
           WHERE chain.parent_id <> '' AND chain.depth < ? \
             AND instr(chain.visited, ',' || n.id || ',') = 0 \
         ) \
         SELECT id, name, depth FROM chain ORDER BY depth DESC",
    )
    .bind(folder_id)
    .bind(MAX_TREE_DEPTH as i64)
    .fetch_all(&mut *conn)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, name, _)| Crumb { id, name })
        .collect())
}

/// Full virtual path for a node, e.g. `photos/2026/beach.jpg`.
pub async fn path_for(pool: &SqlitePool, id: &str) -> Result<String, ApiError> {
    let mut conn = pool.acquire().await?;
    path_for_conn(&mut conn, id).await
}

pub async fn path_for_conn(conn: &mut SqliteConnection, id: &str) -> Result<String, ApiError> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "WITH RECURSIVE chain(id, name, parent_id, depth, visited) AS ( \
           SELECT id, name, parent_id, 0, ',' || id || ',' FROM nodes WHERE id = ? \
           UNION ALL \
           SELECT n.id, n.name, n.parent_id, chain.depth + 1, chain.visited || n.id || ',' \
           FROM nodes n JOIN chain ON n.id = chain.parent_id \
           WHERE chain.parent_id <> '' AND chain.depth < ? \
             AND instr(chain.visited, ',' || n.id || ',') = 0 \
         ) \
         SELECT name, depth FROM chain ORDER BY depth DESC",
    )
    .bind(id)
    .bind(MAX_TREE_DEPTH as i64)
    .fetch_all(&mut *conn)
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
    let mut conn = pool.acquire().await?;
    is_within_conn(&mut conn, ancestor_id, candidate_id).await
}

pub async fn is_within_conn(
    conn: &mut SqliteConnection,
    ancestor_id: &str,
    candidate_id: &str,
) -> Result<bool, ApiError> {
    let sql = format!("{SUBTREE_CTE} ) SELECT 1 FROM subtree WHERE id = ? LIMIT 1");
    let row: Option<(i64,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(ancestor_id)
        .bind(candidate_id)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.is_some())
}

pub async fn subtree_ids_conn(
    conn: &mut SqliteConnection,
    root_id: &str,
) -> Result<Vec<String>, ApiError> {
    let sql = format!("{SUBTREE_CTE} ) SELECT id FROM subtree LIMIT ?");
    let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(root_id)
        .bind((MAX_SUBTREE_NODES + 1) as i64)
        .fetch_all(&mut *conn)
        .await?;
    if rows.len() > MAX_SUBTREE_NODES {
        return Err(ApiError::payload_too_large(
            "folder contains too many items for a single operation",
        ));
    }
    Ok(rows.into_iter().map(|row| row.0).collect())
}

/// Inserts a node inside a caller-owned transaction.
pub async fn insert_node_conn(
    conn: &mut SqliteConnection,
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
    .execute(&mut *conn)
    .await?;
    require_node_conn(conn, &id).await
}

/// Replaces the FTS row for a node. Must be called inside the transaction that
/// writes the node, so a DELETE/INSERT from two writers cannot interleave into
/// duplicate rows and the indexed name/text comes from the committed snapshot.
pub async fn upsert_fts_conn(
    conn: &mut SqliteConnection,
    node_id: &str,
    name: &str,
    text: Option<&str>,
    description: Option<&str>,
    metadata: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM files_fts WHERE file_id = ?")
        .bind(node_id)
        .execute(&mut *conn)
        .await?;
    let current: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT name, kind, text_content, vision_description FROM nodes WHERE id = ?",
    )
    .bind(node_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((current_name, kind, text_content, vision_description)) = current else {
        // The node was deleted concurrently; never recreate an index row.
        return Ok(());
    };
    if kind != "file" {
        return Ok(());
    }
    let indexed_name: &str = if name.is_empty() { &current_name } else { name };
    sqlx::query(
        "INSERT INTO files_fts (file_id, name, text, description, metadata) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(node_id)
    .bind(indexed_name)
    .bind(text.or(text_content.as_deref()).unwrap_or(""))
    .bind(description.or(vision_description.as_deref()).unwrap_or(""))
    .bind(metadata.unwrap_or(""))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Shared queue eligibility check so the HTTP retry route and both workers agree
/// on which MIME types can ever be claimed for vision.
pub fn vision_eligible(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/svg+xml"
}

/// Starts a transaction that immediately takes SQLite's write lock. Deferred
/// transactions that read first can fail with `SQLITE_BUSY_SNAPSHOT` when an
/// unrelated route (for example a document save) commits between the read and
/// the first write; taking the write lock up front makes the check-then-write
/// sequence atomic without relying on retry loops.
pub async fn begin_write(
    pool: &SqlitePool,
) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE nodes SET updated_at = updated_at WHERE 0")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Removes stale duplicate FTS rows (from older non-transactional writers) and
/// rows whose node no longer exists. New writes always replace transactionally.
pub async fn reconcile_fts(state: &crate::state::AppState) -> Result<(), ApiError> {
    let mut tx = begin_write(&state.pool).await?;
    sqlx::query(
        "DELETE FROM files_fts WHERE rowid NOT IN ( \
           SELECT MAX(rowid) FROM files_fts GROUP BY file_id \
         )",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM files_fts WHERE file_id NOT IN (SELECT id FROM nodes)")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
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

    #[test]
    fn huge_extensions_are_shortened_to_fit() {
        let stem = "a";
        let ext = "e".repeat(253);
        let fitted = fit_name(stem, " (2)", Some(&ext));
        assert!(fitted.len() <= MAX_NAME_BYTES);
        assert!(fitted.starts_with("a (2)."));
        assert!(fitted[6..].bytes().all(|b| b == b'e'));
    }

    #[test]
    fn multibyte_stems_are_cut_on_character_boundaries() {
        let stem = "é".repeat(200);
        let fitted = fit_name(&stem, " (2)", Some("txt"));
        assert!(fitted.len() <= MAX_NAME_BYTES);
        assert!(std::str::from_utf8(fitted.as_bytes()).is_ok());
    }

    #[test]
    fn backup_scope_requires_a_bound_root() {
        let mut user = AuthUser {
            id: "u1".into(),
            username: "u".into(),
            role: "admin".into(),
            scope: "backup".into(),
            backup_root_id: None,
        };
        assert!(Scope::for_user(&user).is_err());
        user.backup_root_id = Some("folder".into());
        let scope = Scope::for_user(&user).unwrap();
        assert!(scope.restricted);
        assert_eq!(scope.root, "folder");

        user.scope = "full".into();
        let scope = Scope::for_user(&user).unwrap();
        assert!(!scope.restricted);
        assert_eq!(scope.root, ROOT_ID);
        assert!(require_full_scope(&user).is_ok());
        user.scope = "backup".into();
        assert!(require_full_scope(&user).is_err());
    }

    async fn test_pool() -> SqlitePool {
        use sqlx::sqlite::SqlitePoolOptions;
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite")
    }

    #[tokio::test]
    async fn migrations_apply_and_enforce_tree_invariants() {
        let pool = test_pool().await;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations apply");
        let ts = crate::now();

        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at) \
             VALUES ('a', 'root', 'folder', 'a', ?, ?)",
        )
        .bind(ts)
        .bind(ts)
        .execute(&pool)
        .await
        .expect("valid child");

        let missing_parent = sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at) \
             VALUES ('orphan', 'missing', 'folder', 'orphan', ?, ?)",
        )
        .bind(ts)
        .bind(ts)
        .execute(&pool)
        .await;
        assert!(missing_parent.is_err(), "missing parent must be rejected");

        let long = "x".repeat(MAX_NAME_BYTES + 1);
        let long_name = sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at) \
             VALUES ('long', 'a', 'file', ?, ?, ?)",
        )
        .bind(&long)
        .bind(ts)
        .bind(ts)
        .execute(&pool)
        .await;
        assert!(long_name.is_err(), "overlong name must be rejected");

        let bad_json = sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at, metadata_json) \
             VALUES ('bad', 'a', 'file', 'bad', ?, ?, 'not json')",
        )
        .bind(ts)
        .bind(ts)
        .execute(&pool)
        .await;
        assert!(bad_json.is_err(), "invalid metadata must be rejected");

        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at) \
             VALUES ('b', 'a', 'folder', 'b', ?, ?)",
        )
        .bind(ts)
        .bind(ts)
        .execute(&pool)
        .await
        .expect("nested folder");

        let cycle = sqlx::query("UPDATE nodes SET parent_id = 'b' WHERE id = 'a'")
            .execute(&pool)
            .await;
        assert!(cycle.is_err(), "cycle must be rejected");
    }

    #[tokio::test]
    async fn recursive_traversal_terminates_on_cycles() {
        let pool = test_pool().await;
        sqlx::query("CREATE TABLE nodes (id TEXT PRIMARY KEY, parent_id TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO nodes (id, parent_id) VALUES ('a', 'b'), ('b', 'a')")
            .execute(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        assert!(is_within_conn(&mut conn, "a", "b").await.unwrap());
        let ids = subtree_ids_conn(&mut conn, "a").await.unwrap();
        assert_eq!(ids.len(), 2, "visited set must stop the cycle");
    }

    #[tokio::test]
    async fn fts_upserts_replace_instead_of_duplicating() {
        let pool = test_pool().await;
        sqlx::query(
            "CREATE TABLE nodes (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, \
             text_content TEXT, vision_description TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE VIRTUAL TABLE files_fts USING fts5( \
             file_id UNINDEXED, name, text, description, metadata)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO nodes (id, name, kind, text_content) \
             VALUES ('n1', 'old.txt', 'file', 'hello')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        upsert_fts_conn(&mut conn, "n1", "old.txt", None, None, Some(""))
            .await
            .unwrap();
        upsert_fts_conn(&mut conn, "n1", "old.txt", None, None, Some(""))
            .await
            .unwrap();
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files_fts")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(count, 1);

        sqlx::query("UPDATE nodes SET name = 'new.txt' WHERE id = 'n1'")
            .execute(&mut *conn)
            .await
            .unwrap();
        upsert_fts_conn(&mut conn, "n1", "new.txt", None, None, Some(""))
            .await
            .unwrap();
        let (name,): (String,) = sqlx::query_as("SELECT name FROM files_fts WHERE file_id = 'n1'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(name, "new.txt");
    }
}
