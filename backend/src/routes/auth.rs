use crate::{
    auth::{self, AuthUser},
    db,
    error::ApiError,
    state::AppState,
    util::json_response,
};
use axum::{
    extract::{ConnectInfo, Extension, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{Arc, MutexGuard},
};

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

fn lock_limiter(state: &AppState) -> Result<MutexGuard<'_, auth::LoginLimiter>, ApiError> {
    state
        .login_limiter
        .lock()
        .map_err(|_| ApiError::internal("login limiter is unavailable"))
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    connect: ConnectInfo<SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let username = auth::normalize_login_username(&req.username)?;
    if req.password.len() > auth::MAX_PASSWORD_BYTES {
        return Err(ApiError::bad_request(format!(
            "password must be at most {} bytes",
            auth::MAX_PASSWORD_BYTES
        )));
    }

    let account_key = auth::login_account_key(&state.secret, &username);
    let source_key = connect.0.ip().to_string();
    let decision = lock_limiter(&state)?.begin(&account_key, &source_key);
    if decision == auth::LoginDecision::Limited {
        return Err(ApiError::too_many());
    }

    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, username, role, password_hash FROM users WHERE lower(username) = lower(?)",
    )
    .bind(&username)
    .fetch_optional(&state.pool)
    .await?;

    let verified = match row.as_ref() {
        Some((_, _, _, password_hash)) => {
            auth::verify_password_bounded(&state, password_hash, &req.password).await?
        }
        None => {
            // Spend comparable work for unknown usernames; the result is ignored.
            let _ =
                auth::verify_password_bounded(&state, &state.dummy_password_hash, &req.password)
                    .await;
            false
        }
    };

    let Some((id, canonical_username, role, _)) = row else {
        return Err(ApiError::unauthorized());
    };
    if !verified {
        return Err(ApiError::unauthorized());
    }

    {
        let mut limiter = lock_limiter(&state)?;
        limiter.clear_account(&account_key);
    }

    let token = auth::create_session(&state.pool, &id, state.cfg.session_ttl_secs).await?;
    let cookie = auth::session_cookie(&token, state.cfg.session_ttl_secs, state.cfg.secure_cookies);

    Ok(json_response(
        StatusCode::OK,
        json!({
            "user": { "id": id, "username": canonical_username, "role": role }
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
