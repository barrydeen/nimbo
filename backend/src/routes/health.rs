use crate::{db, state::AppState};
use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

const READY_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn livez() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Bounded readiness probe: a failed/unavailable database or missing data
/// directories report 503 instead of a false healthy status.
pub async fn healthz(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    let probe = tokio::time::timeout(
        READY_TIMEOUT,
        db::get_setting(&state.pool, "setup_complete"),
    )
    .await;
    let (database_ok, setup_complete) = match probe {
        Ok(Ok(value)) => (true, value.map(|value| value == "true").unwrap_or(false)),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness database probe failed");
            (false, false)
        }
        Err(_) => {
            tracing::warn!("readiness database probe timed out");
            (false, false)
        }
    };

    let required_paths = [
        state.cfg.data_dir.clone(),
        state.cfg.vault_dir(),
        state.cfg.files_dir(),
        state.cfg.blobs_dir(),
        state.cfg.upload_tmp_dir(),
    ];
    let storage_ok = required_paths.iter().all(|path| path.is_dir());

    if database_ok && storage_ok {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "database": "ok",
                "storage": "ok",
                "setup_complete": setup_complete,
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "degraded",
                "database": if database_ok { "ok" } else { "unavailable" },
                "storage": if storage_ok { "ok" } else { "missing" },
                "error": "readiness check failed",
            })),
        )
    }
}

pub async fn api_not_found() -> (StatusCode, Json<Value>) {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })))
}
