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
    /// Required only when `NIMBO_SETUP_TOKEN` is configured.
    #[serde(default)]
    pub setup_token: Option<String>,
}

async fn setup_complete(state: &AppState) -> Result<bool, ApiError> {
    Ok(db::get_setting(&state.pool, "setup_complete")
        .await?
        .map(|value| value == "true")
        .unwrap_or(false))
}

async fn instance_name(state: &AppState) -> Result<String, ApiError> {
    Ok(db::get_setting(&state.pool, "instance_name")
        .await?
        .unwrap_or_else(|| "Nimbo".into()))
}

pub async fn status(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "setup_complete": setup_complete(&state).await?,
        "instance_name": instance_name(&state).await?,
        "setup_token_required": state.cfg.setup_token.is_some(),
    })))
}

fn verify_setup_token(state: &AppState, supplied: Option<&str>) -> Result<(), ApiError> {
    check_setup_token(state.cfg.setup_token.as_deref(), supplied)
}

fn check_setup_token(configured: Option<&str>, supplied: Option<&str>) -> Result<(), ApiError> {
    let Some(expected) = configured else {
        return Ok(());
    };
    let supplied = supplied.unwrap_or("");
    if !auth::constant_time_eq(supplied, expected) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "a valid setup token is required",
        ));
    }
    Ok(())
}

pub async fn complete_setup(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetupRequest>,
) -> Result<Response, ApiError> {
    // Authenticate the bootstrap path before any validation or expensive work.
    verify_setup_token(&state, req.setup_token.as_deref())?;

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
    if !(3..=32).contains(&username.chars().count()) || username.len() > 64 {
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
    auth::validate_password(&req.password)?;

    // Cheap rejection before expensive Argon2 hashing; the transaction below
    // still rechecks so concurrent requests cannot both win.
    if setup_complete(&state).await? {
        return Err(ApiError::conflict("instance is already set up"));
    }

    let password_hash = auth::hash_password_bounded(&state, &req.password).await?;
    let user_id = uuid::Uuid::new_v4().to_string();
    let now = crate::now();

    let mut tx = state.pool.begin().await?;

    let existing: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'setup_complete'")
            .fetch_optional(&mut *tx)
            .await?;
    if existing.map(|row| row.0 == "true").unwrap_or(false) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_token_is_optional_when_unconfigured() {
        assert!(check_setup_token(None, None).is_ok());
        assert!(check_setup_token(None, Some("anything")).is_ok());
    }

    #[test]
    fn configured_setup_token_fails_closed() {
        let configured = Some("0123456789abcdef");
        assert!(check_setup_token(configured, Some("0123456789abcdef")).is_ok());
        assert!(check_setup_token(configured, Some("0123456789abcdee")).is_err());
        assert!(check_setup_token(configured, None).is_err());
        assert!(check_setup_token(configured, Some("")).is_err());
    }
}
