use crate::{auth, db, error::ApiError, state::AppState, util::json_response};
use axum::{extract::State, http::StatusCode, response::Response, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SetupRequest {
    pub instance_name: String,
    pub username: String,
    pub password: String,
}

pub async fn status(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let setup_complete = db::get_setting(&state.pool, "setup_complete")
        .await?
        .map(|v| v == "true")
        .unwrap_or(false);
    let instance_name = db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into());
    Ok(Json(json!({
        "setup_complete": setup_complete,
        "instance_name": instance_name,
    })))
}

pub async fn complete_setup(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetupRequest>,
) -> Result<Response, ApiError> {
    let instance_name = req.instance_name.trim().to_string();
    let username = req.username.trim().to_string();

    if instance_name.is_empty() || instance_name.chars().count() > 64 {
        return Err(ApiError::bad_request(
            "instance name must be between 1 and 64 characters",
        ));
    }
    if instance_name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "instance name contains control characters",
        ));
    }
    if !(3..=32).contains(&username.chars().count()) {
        return Err(ApiError::bad_request(
            "username must be between 3 and 32 characters",
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(ApiError::bad_request(
            "username may contain letters, numbers, dots, dashes and underscores",
        ));
    }
    if req.password.chars().count() < 8 {
        return Err(ApiError::bad_request(
            "password must be at least 8 characters",
        ));
    }

    let password_hash = auth::hash_password(&req.password)?;
    let user_id = uuid::Uuid::new_v4().to_string();
    let now = crate::now();

    let mut tx = state.pool.begin().await?;

    // Re-check inside the transaction so concurrent setup requests cannot both win.
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'setup_complete'")
            .fetch_optional(&mut *tx)
            .await?;
    if existing.map(|r| r.0 == "true").unwrap_or(false) {
        return Err(ApiError::conflict("instance is already set up"));
    }

    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) \
         VALUES (?, ?, ?, 'admin', ?)",
    )
    .bind(&user_id)
    .bind(&username)
    .bind(&password_hash)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE settings SET value = 'true' WHERE key = 'setup_complete'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE settings SET value = ? WHERE key = 'instance_name'")
        .bind(&instance_name)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let token = auth::create_session(&state.pool, &user_id, state.cfg.session_ttl_secs).await?;
    let cookie = auth::session_cookie(&token, state.cfg.session_ttl_secs, state.cfg.secure_cookies);

    Ok(json_response(
        StatusCode::CREATED,
        json!({
            "instance_name": instance_name,
            "user": { "id": user_id, "username": username, "role": "admin" }
        }),
        Some(cookie),
    ))
}
