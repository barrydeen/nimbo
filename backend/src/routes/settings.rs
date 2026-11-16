use crate::{
    agent::{self, LlmConfig},
    auth::AuthUser,
    error::ApiError,
    state::AppState,
};
use axum::{
    extract::{Extension, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

fn require_admin(user: &AuthUser) -> Result<(), ApiError> {
    if user.role == "admin" {
        Ok(())
    } else {
        Err(ApiError::forbidden("admin only"))
    }
}

pub async fn get_llm(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    match agent::load_config(&state).await? {
        Some(cfg) => Ok(Json(json!({
            "configured": true,
            "base_url": cfg.base_url,
            "model": cfg.model,
            "temperature": cfg.temperature,
            "api_key_set": !cfg.api_key.is_empty(),
        }))),
        None => Ok(Json(json!({
            "configured": false,
            "base_url": agent::default_base_url(),
            "model": "",
            "temperature": 0.7,
            "api_key_set": false,
        }))),
    }
}

#[derive(Deserialize)]
pub struct LlmRequest {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
}

pub async fn put_llm(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<LlmRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;

    let base_url = req.base_url.trim().trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(ApiError::bad_request(
            "base URL must start with http:// or https://",
        ));
    }
    let model = req.model.trim().to_string();
    if model.is_empty() {
        return Err(ApiError::bad_request("model is required"));
    }

    let existing = agent::load_config(&state).await?;
    let api_key = match req.api_key {
        Some(key) if !key.trim().is_empty() => key.trim().to_string(),
        _ => existing.map(|cfg| cfg.api_key).unwrap_or_default(),
    };

    let cfg = LlmConfig {
        base_url,
        model,
        api_key,
        temperature: req.temperature.unwrap_or(0.7).clamp(0.0, 2.0),
    };
    agent::save_config(&state, &cfg).await?;

    Ok(Json(json!({
        "configured": true,
        "base_url": cfg.base_url,
        "model": cfg.model,
        "temperature": cfg.temperature,
        "api_key_set": !cfg.api_key.is_empty(),
    })))
}

pub async fn test_llm(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;
    let cfg = agent::load_config(&state)
        .await?
        .ok_or_else(|| ApiError::bad_request("no provider configured"))?;

    let reply = agent::complete(
        &cfg,
        "You are a connectivity check. Reply with the single word OK.",
        "ping",
        8,
    )
    .await?;

    Ok(Json(json!({ "ok": true, "reply": reply })))
}
