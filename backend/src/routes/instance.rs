use crate::{auth::AuthUser, db, error::ApiError, state::AppState};
use axum::{
    extract::{Extension, State},
    Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn instance(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    let setup_complete = db::get_setting(&state.pool, "setup_complete")
        .await?
        .map(|value| value == "true")
        .unwrap_or(false);
    let instance_name = db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into());
    Ok(Json(json!({
        "setup_complete": setup_complete,
        "instance_name": instance_name,
        "max_upload_bytes": state.cfg.max_upload_bytes,
        "backup_scope": user.is_backup_scope(),
    })))
}
