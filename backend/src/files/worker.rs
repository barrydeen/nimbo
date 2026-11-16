use crate::{
    error::ApiError,
    files::{
        extract::{self, Extraction},
        storage, upsert_fts, vision, Node,
    },
    state::AppState,
};
use std::sync::Arc;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub fn spawn(state: Arc<AppState>) {
    let index_state = state.clone();
    tokio::spawn(async move {
        if let Err(err) = reset_running(&index_state).await {
            tracing::warn!(error = %err, "could not reset file jobs");
        }
        index_loop(index_state).await;
    });
    tokio::spawn(async move {
        vision_loop(state).await;
    });
}

async fn reset_running(state: &AppState) -> Result<(), ApiError> {
    sqlx::query("UPDATE nodes SET index_status = 'pending' WHERE index_status = 'running'")
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE nodes SET vision_status = 'pending' WHERE vision_status = 'running'")
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
        "UPDATE nodes SET index_status = 'running', index_error = NULL \
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

async fn process_next_index(state: &AppState) -> Result<bool, ApiError> {
    let Some(node) = claim_index(state).await? else {
        return Ok(false);
    };

    let path = node.blob_path(state)?;
    let name = node.name.clone();
    let mime = node
        .mime
        .clone()
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let joined = tokio::task::spawn_blocking(move || extract::extract(&path, &name, &mime)).await;

    let extraction = match joined {
        Ok(extraction) => extraction,
        Err(err) => {
            let message = format!("extraction task failed: {err}");
            sqlx::query("UPDATE nodes SET index_status = 'failed', index_error = ? WHERE id = ?")
                .bind(truncate(&message, 500))
                .bind(&node.id)
                .execute(&state.pool)
                .await?;
            return Ok(true);
        }
    };

    finish_index(state, node, extraction).await?;
    Ok(true)
}

async fn finish_index(
    state: &AppState,
    node: Node,
    extraction: Extraction,
) -> Result<(), ApiError> {
    let metadata_json = extraction.metadata_json();
    let thumbnail = match extraction.thumbnail.as_deref() {
        Some(bytes) => Some(storage::store_bytes(state, bytes).await?),
        None => None,
    };
    let status = if extraction.error.is_some() || extraction.partial {
        "partial"
    } else {
        "done"
    };

    sqlx::query(
        "UPDATE nodes SET index_status = ?, index_error = ?, text_content = ?, \
         metadata_json = ?, thumb_sha = COALESCE(?, thumb_sha) WHERE id = ?",
    )
    .bind(status)
    .bind(extraction.error.as_deref().map(|e| truncate(e, 500)))
    .bind(
        extraction
            .text
            .as_deref()
            .map(|t| truncate(t, 2 * 1024 * 1024)),
    )
    .bind(&metadata_json)
    .bind(thumbnail.as_deref())
    .bind(&node.id)
    .execute(&state.pool)
    .await?;

    let mime = node.mime.clone().unwrap_or_default();
    if vision_eligible(&mime) {
        sqlx::query(
            "UPDATE nodes SET vision_status = 'pending' \
             WHERE id = ? AND vision_status = 'n/a'",
        )
        .bind(&node.id)
        .execute(&state.pool)
        .await?;
    } else if mime.starts_with("image/") {
        sqlx::query(
            "UPDATE nodes SET vision_status = 'skipped', vision_error = ? \
             WHERE id = ? AND vision_status IN ('n/a', 'pending')",
        )
        .bind("this image format cannot be described automatically")
        .bind(&node.id)
        .execute(&state.pool)
        .await?;
    }

    let updated = sqlx::query_as::<_, Node>("SELECT * FROM nodes WHERE id = ?")
        .bind(&node.id)
        .fetch_one(&state.pool)
        .await?;
    let metadata_text = vision::metadata_search_text(&updated);
    upsert_fts(
        &state.pool,
        &updated,
        updated.text_content.as_deref(),
        updated.vision_description.as_deref(),
        Some(&metadata_text),
    )
    .await?;
    Ok(())
}

fn vision_eligible(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/svg+xml"
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
        "UPDATE nodes SET vision_status = 'running', vision_error = NULL \
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

async fn process_next_vision(state: &AppState) -> Result<bool, ApiError> {
    let Some(node) = claim_vision(state).await? else {
        return Ok(false);
    };
    if let Err(err) = vision::process(state, node.clone()).await {
        tracing::warn!(file = %node.name, error = %err, "image description failed");
        vision::set_failed(state, &node.id, &err.message).await?;
    }
    Ok(true)
}

#[allow(dead_code)]
fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
