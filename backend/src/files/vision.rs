use crate::{
    agent,
    error::ApiError,
    files::{upsert_fts, Node},
    state::AppState,
};
use std::io::Cursor;

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
    let Some(cfg) = agent::load_config(state).await? else {
        set_skipped(state, &node.id, "no LLM provider configured").await?;
        return Ok(());
    };
    if !cfg.describe_images {
        set_skipped(state, &node.id, "automatic image descriptions are disabled").await?;
        return Ok(());
    }

    let blob = node.blob_path(state)?;
    let bytes = tokio::fs::read(blob)
        .await
        .map_err(|err| ApiError::internal(format!("could not read image: {err}")))?;
    let jpeg = tokio::task::spawn_blocking(move || prepare_image(&bytes))
        .await
        .map_err(|err| ApiError::internal(format!("image task failed: {err}")))??;

    let described = agent::describe_image(&cfg, &jpeg, &node.name).await?;
    let tags = serde_json::to_string(&described.tags).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        "UPDATE nodes SET vision_status = 'done', vision_description = ?, vision_text = ?, \
         vision_tags = ?, vision_model = ?, vision_at = ?, vision_error = NULL WHERE id = ?",
    )
    .bind(truncate(&described.description, 4000))
    .bind(truncate(&described.text, 8000))
    .bind(tags)
    .bind(&described.model)
    .bind(crate::now())
    .bind(&node.id)
    .execute(&state.pool)
    .await?;

    refresh_fts(state, &node.id).await?;
    Ok(())
}

pub async fn set_failed(state: &AppState, id: &str, error: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET vision_status = 'failed', vision_error = ?, vision_at = ? WHERE id = ?",
    )
    .bind(truncate(error, 500))
    .bind(crate::now())
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn set_skipped(state: &AppState, id: &str, reason: &str) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE nodes SET vision_status = 'skipped', vision_error = ?, vision_at = ? WHERE id = ?",
    )
    .bind(truncate(reason, 500))
    .bind(crate::now())
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

pub async fn requeue_skipped(state: &AppState) -> Result<u64, ApiError> {
    let result = sqlx::query(
        "UPDATE nodes SET vision_status = 'pending', vision_error = NULL \
         WHERE kind = 'file' AND mime LIKE 'image/%' AND vision_status = 'skipped'",
    )
    .execute(&state.pool)
    .await?;
    Ok(result.rows_affected())
}

async fn refresh_fts(state: &AppState, id: &str) -> Result<(), ApiError> {
    let node = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("item not found"))?;
    let metadata = metadata_search_text(&node);
    upsert_fts(
        &state.pool,
        &node,
        node.text_content.as_deref(),
        node.vision_description.as_deref(),
        Some(&metadata),
    )
    .await?;
    Ok(())
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
}
