use crate::{auth, db, error::ApiError, state::AppState};
use axum::{extract::State, http::HeaderMap, Json};
use serde_json::{json, Value};
use std::sync::Arc;

/// Public bootstrap payload for the SPA: setup state plus the current user, if any.
pub async fn bootstrap(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let setup_complete = db::get_setting(&state.pool, "setup_complete")
        .await?
        .map(|value| value == "true")
        .unwrap_or(false);
    let instance_name = db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into());

    let user = match auth::token_from_headers(&headers) {
        Some(token) => auth::lookup_session(&state.pool, &token).await?,
        None => None,
    };

    Ok(Json(json!({
        "setup_complete": setup_complete,
        "setup_token_required": state.cfg.setup_token.is_some(),
        "instance_name": instance_name,
        "user": user,
    })))
}
