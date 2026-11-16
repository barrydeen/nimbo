use crate::{
    agent,
    error::ApiError,
    files::{lock, upsert_fts_conn, vision_eligible, Node},
    state::AppState,
};
use std::io::Cursor;

const MAX_VISION_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Downscales an image for the vision provider. Returns JPEG bytes.
pub fn prepare_image(bytes: &[u8]) -> Result<Vec<u8>, ApiError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| ApiError::bad_request(format!("could not read image: {err}")))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut image = reader
        .decode()
        .map_err(|err| ApiError::bad_request(format!("could not decode image: {err}")))?;

    // Apply EXIF orientation so the model sees the image the same way a browser does.
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(bytes)) {
        if let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
            if let Some(value) = field.value.get_uint(0) {
                if let Some(orientation) = image::metadata::Orientation::from_exif(value as u8) {
                    image.apply_orientation(orientation);
                }
            }
        }
    }

    let resized = image.thumbnail(1536, 1536).to_rgb8();
    let mut output = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 85);
    encoder
        .encode_image(&resized)
        .map_err(|err| ApiError::internal(format!("could not encode image: {err}")))?;
    Ok(output)
}

pub async fn process(state: &AppState, node: Node) -> Result<(), ApiError> {
    let mime = node.mime.clone().unwrap_or_default();
    if !vision_eligible(&mime) {
        set_skipped(&state.pool, &node, "this image format cannot be described").await?;
        return Ok(());
    }
    let Some(cfg) = agent::load_config(state).await? else {
        set_skipped(&state.pool, &node, "no LLM provider configured").await?;
        return Ok(());
    };
    if !cfg.describe_images {
        set_skipped(
            &state.pool,
            &node,
            "automatic image descriptions are disabled",
        )
        .await?;
        return Ok(());
    }
    let target = match agent::indexing_target(&cfg) {
        Ok(target) => target,
        Err(err) => {
            set_skipped(&state.pool, &node, &err.message).await?;
            return Ok(());
        }
    };

    let blob = node.blob_path(state)?;
    let size = tokio::fs::metadata(&blob).await.map(|meta| meta.len())?;
    if size > MAX_VISION_IMAGE_BYTES {
        set_skipped(&state.pool, &node, "image is too large to describe").await?;
        return Ok(());
    }
    let bytes = tokio::fs::read(blob)
        .await
        .map_err(|err| ApiError::internal(format!("could not read image: {err}")))?;
    let jpeg = tokio::task::spawn_blocking(move || prepare_image(&bytes))
        .await
        .map_err(|err| ApiError::internal(format!("image task failed: {err}")))??;

    let described = agent::describe_image(&target, &jpeg, &node.name).await?;
    apply_description(&state.pool, &node, &described).await?;
    Ok(())
}

/// Writes a described image only when the row still contains the claimed blob
/// and the claim still owns the `running` slot. A content replacement therefore
/// cancels the stale claim without attaching its description/text to new bytes.
/// The node update and FTS replacement commit atomically.
pub(crate) async fn apply_description(
    pool: &sqlx::SqlitePool,
    node: &Node,
    described: &agent::VisionDescription,
) -> Result<bool, ApiError> {
    let tags = serde_json::to_string(&described.tags).unwrap_or_else(|_| "[]".to_string());
    let _guard = lock::mutation().await;
    let mut tx = crate::files::begin_write(pool).await?;
    let updated = sqlx::query(
        "UPDATE nodes SET vision_status = 'done', vision_description = ?, vision_text = ?, \
         vision_tags = ?, vision_model = ?, vision_at = ?, vision_error = NULL \
         WHERE id = ? AND blob_sha IS ? AND vision_status = 'running'",
    )
    .bind(truncate(&described.description, 4000))
    .bind(truncate(&described.text, 8000))
    .bind(tags)
    .bind(&described.model)
    .bind(crate::now())
    .bind(&node.id)
    .bind(node.blob_sha.as_deref())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(false);
    }

    let current: Option<Node> = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&node.id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(current) = current else {
        tx.rollback().await?;
        return Ok(false);
    };
    if current.blob_sha.as_deref() != node.blob_sha.as_deref() {
        // Defensive: the UPDATE condition already guarantees this, but never
        // let an old description reach the FTS row for new bytes.
        tx.rollback().await?;
        return Ok(false);
    }
    let metadata = metadata_search_text(&current);
    upsert_fts_conn(
        &mut tx,
        &current.id,
        &current.name,
        current.text_content.as_deref(),
        current.vision_description.as_deref(),
        Some(&metadata),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

#[allow(dead_code)]
pub async fn set_failed(pool: &sqlx::SqlitePool, node: &Node, error: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET vision_status = 'failed', vision_error = ?, vision_at = ? \
         WHERE id = ? AND blob_sha IS ? AND vision_status = 'running'",
    )
    .bind(truncate(error, 500))
    .bind(crate::now())
    .bind(&node.id)
    .bind(node.blob_sha.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_skipped(
    pool: &sqlx::SqlitePool,
    node: &Node,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET vision_status = 'skipped', vision_error = ?, vision_at = ? \
         WHERE id = ? AND blob_sha IS ? AND vision_status = 'running'",
    )
    .bind(truncate(reason, 500))
    .bind(crate::now())
    .bind(&node.id)
    .bind(node.blob_sha.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn requeue_skipped(state: &AppState) -> Result<u64, ApiError> {
    let result = sqlx::query(
        "UPDATE nodes SET vision_status = 'pending', vision_error = NULL \
         WHERE kind = 'file' AND mime LIKE 'image/%' AND mime <> 'image/svg+xml' \
           AND vision_status = 'skipped'",
    )
    .execute(&state.pool)
    .await?;
    Ok(result.rows_affected())
}

pub fn metadata_search_text(node: &Node) -> String {
    let parsed: serde_json::Value =
        serde_json::from_str(&node.metadata_json).unwrap_or(serde_json::Value::Null);
    let mut parts = Vec::new();
    flatten_metadata(&parsed, &mut parts);
    parts.join(" ")
}

fn flatten_metadata(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                out.push(key.clone());
                flatten_metadata(value, out);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                flatten_metadata(item, out);
            }
        }
        serde_json::Value::String(text) => {
            if !text.is_empty() {
                out.push(text.clone());
            }
        }
        serde_json::Value::Number(number) => out.push(number.to_string()),
        serde_json::Value::Bool(value) => out.push(value.to_string()),
        serde_json::Value::Null => {}
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    #[test]
    fn metadata_search_includes_nested_strings() {
        let value = serde_json::json!({
            "image": {"width": 10, "exif": {"Make": "Nimbo"}},
            "text": {"words": 4}
        });
        let mut parts = Vec::new();
        flatten_metadata(&value, &mut parts);
        assert!(parts.iter().any(|part| part == "Nimbo"));
        assert!(parts.iter().any(|part| part == "words"));
    }

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

    async fn claimed_image(pool: &SqlitePool, blob: &str) -> Node {
        sqlx::query(
            "INSERT INTO nodes (id, parent_id, kind, name, created_at, updated_at, blob_sha, \
             mime, size, index_status, metadata_json, vision_status) \
             VALUES ('n1', 'root', 'file', 'photo.jpg', 1000, 1000, ?, 'image/jpeg', 4, \
             'done', '{}', 'running')",
        )
        .bind(blob)
        .execute(pool)
        .await
        .expect("insert claimed image");
        sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = 'n1'")
            .fetch_one(pool)
            .await
            .expect("claimed image")
    }

    fn description() -> agent::VisionDescription {
        agent::VisionDescription {
            description: "a stale description".into(),
            text: "stale visible text".into(),
            tags: vec!["stale".into()],
            model: "test-model".into(),
        }
    }

    #[tokio::test]
    async fn stale_description_is_not_applied_after_blob_replacement() {
        let pool = test_pool().await;
        let claimed = claimed_image(&pool, &"a".repeat(64)).await;

        sqlx::query(
            "UPDATE nodes SET blob_sha = ?, updated_at = 2000, vision_status = 'pending', \
             vision_description = NULL, vision_text = NULL, vision_tags = NULL, \
             vision_model = NULL, vision_at = NULL, vision_error = NULL WHERE id = 'n1'",
        )
        .bind("c".repeat(64))
        .execute(&pool)
        .await
        .unwrap();

        let applied = apply_description(&pool, &claimed, &description())
            .await
            .unwrap();
        assert!(!applied, "stale vision claim must be cancelled");

        let (status, description, text, tags): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT vision_status, vision_description, vision_text, vision_tags \
             FROM nodes WHERE id = 'n1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "pending");
        assert!(description.is_none());
        assert!(text.is_none());
        assert!(tags.is_none());
        let (fts,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM files_fts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fts, 0, "old description must not reach FTS");
    }

    #[tokio::test]
    async fn current_vision_claim_applies_description_and_refreshes_fts() {
        let pool = test_pool().await;
        let claimed = claimed_image(&pool, &"a".repeat(64)).await;

        let applied = apply_description(&pool, &claimed, &description())
            .await
            .unwrap();
        assert!(applied);

        let (status, description): (String, Option<String>) =
            sqlx::query_as("SELECT vision_status, vision_description FROM nodes WHERE id = 'n1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "done");
        assert_eq!(description.as_deref(), Some("a stale description"));
        let (indexed,): (String,) =
            sqlx::query_as("SELECT description FROM files_fts WHERE file_id = 'n1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed, "a stale description");
    }

    #[tokio::test]
    async fn stale_skip_marker_does_not_clobber_replacement_state() {
        let pool = test_pool().await;
        let claimed = claimed_image(&pool, &"a".repeat(64)).await;

        sqlx::query(
            "UPDATE nodes SET blob_sha = ?, vision_status = 'pending', vision_error = NULL \
             WHERE id = 'n1'",
        )
        .bind("c".repeat(64))
        .execute(&pool)
        .await
        .unwrap();

        set_skipped(&pool, &claimed, "no provider").await.unwrap();
        let (status, error): (String, Option<String>) =
            sqlx::query_as("SELECT vision_status, vision_error FROM nodes WHERE id = 'n1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "pending");
        assert!(error.is_none());
    }
}
