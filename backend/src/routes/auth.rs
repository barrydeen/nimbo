use crate::{
    auth::{self, AuthUser},
    db,
    error::ApiError,
    state::AppState,
    util::json_response,
};
use axum::{
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let username = req.username.trim().to_string();
    let key = username.to_lowercase();

    auth::check_rate_limit(&state, &key)?;

    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT id, username, password_hash FROM users WHERE lower(username) = lower(?)",
    )
    .bind(&username)
    .fetch_optional(&state.pool)
    .await?;

    let Some((id, canonical_username, password_hash)) = row else {
        auth::record_failure(&state, &key);
        return Err(ApiError::unauthorized());
    };

    if !auth::verify_password(&password_hash, &req.password) {
        auth::record_failure(&state, &key);
        return Err(ApiError::unauthorized());
    }

    auth::clear_failures(&state, &key);

    let role: (String,) = sqlx::query_as("SELECT role FROM users WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;

    let token = auth::create_session(&state.pool, &id, state.cfg.session_ttl_secs).await?;
    let cookie = auth::session_cookie(&token, state.cfg.session_ttl_secs, state.cfg.secure_cookies);

    Ok(json_response(
        StatusCode::OK,
        json!({
            "user": { "id": id, "username": canonical_username, "role": role.0 }
        }),
        Some(cookie),
    ))
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = auth::token_from_headers(&headers) {
        auth::delete_session(&state.pool, &token).await?;
    }
    Ok(json_response(
        StatusCode::OK,
        json!({ "ok": true }),
        Some(auth::clear_cookie(state.cfg.secure_cookies)),
    ))
}

pub async fn me(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    let instance_name = db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into());
    Ok(Json(json!({
        "user": user,
        "instance_name": instance_name,
    })))
}
