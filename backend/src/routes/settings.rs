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
            "vision_model": cfg.vision_model,
            "describe_images": cfg.describe_images,
        }))),
        None => Ok(Json(json!({
            "configured": false,
            "base_url": agent::default_base_url(),
            "model": "",
            "temperature": 0.7,
            "api_key_set": false,
            "vision_model": null,
            "describe_images": true,
        }))),
    }
}

#[derive(Deserialize)]
pub struct LlmRequest {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub temperature: Option<f32>,
    pub vision_model: Option<String>,
    pub describe_images: Option<bool>,
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
        _ => existing
            .as_ref()
            .map(|cfg| cfg.api_key.clone())
            .unwrap_or_default(),
    };
    let vision_model = match req.vision_model {
        Some(model) if !model.trim().is_empty() => Some(model.trim().to_string()),
        _ => existing.as_ref().and_then(|cfg| cfg.vision_model.clone()),
    };
    let describe_images = req
        .describe_images
        .or_else(|| existing.as_ref().map(|cfg| cfg.describe_images))
        .unwrap_or(true);

    let cfg = LlmConfig {
        base_url,
        model,
        api_key,
        temperature: req.temperature.unwrap_or(0.7).clamp(0.0, 2.0),
        vision_model,
        describe_images,
    };
    agent::save_config(&state, &cfg).await?;
    if let Err(err) = crate::files::vision::requeue_skipped(&state).await {
        tracing::warn!(error = %err, "could not requeue image descriptions");
    }

    Ok(Json(json!({
        "configured": true,
        "base_url": cfg.base_url,
        "model": cfg.model,
        "temperature": cfg.temperature,
        "api_key_set": !cfg.api_key.is_empty(),
        "vision_model": cfg.vision_model,
        "describe_images": cfg.describe_images,
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
