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

const MAX_BASE_URL_BYTES: usize = 2_048;
const MAX_MODEL_BYTES: usize = 200;
const MAX_API_KEY_BYTES: usize = 4_096;

fn require_admin(user: &AuthUser) -> Result<(), ApiError> {
    if user.role == "admin" {
        Ok(())
    } else {
        Err(ApiError::forbidden("admin only"))
    }
}

fn config_json(cfg: Option<&LlmConfig>) -> Value {
    match cfg {
        Some(cfg) => json!({
            "configured": true,
            "base_url": cfg.base_url,
            "model": cfg.model,
            "temperature": cfg.temperature,
            "api_key_set": !cfg.api_key.is_empty(),
            "vision_model": cfg.vision_model,
            "describe_images": cfg.describe_images,
        }),
        None => json!({
            "configured": false,
            "base_url": agent::default_base_url(),
            "model": "",
            "temperature": 0.7,
            "api_key_set": false,
            "vision_model": null,
            "describe_images": true,
        }),
    }
}

pub async fn get_llm(State(state): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    Ok(Json(config_json(
        agent::load_config(&state).await?.as_ref(),
    )))
}

#[derive(Deserialize)]
pub struct LlmRequest {
    pub base_url: String,
    pub model: String,
    /// Omitted or empty retains; a non-empty value replaces.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Explicitly remove the stored provider key.
    #[serde(default)]
    pub clear_api_key: bool,
    /// Required when changing provider origin while retaining a stored key.
    #[serde(default)]
    pub confirm_reuse_api_key: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Omitted retains; empty string clears the override; non-empty replaces.
    #[serde(default)]
    pub vision_model: Option<String>,
    #[serde(default)]
    pub describe_images: Option<bool>,
}

/// Explicit retain/replace/clear semantics for the provider secret. A stored key
/// is never silently carried to a different origin without confirmation.
fn next_api_key(
    existing_key: Option<&str>,
    existing_origin: Option<&str>,
    new_origin: &str,
    supplied_key: Option<&str>,
    clear_api_key: bool,
    confirm_reuse_api_key: bool,
) -> Result<String, ApiError> {
    let supplied_key = supplied_key.map(str::trim).filter(|key| !key.is_empty());

    if clear_api_key && supplied_key.is_some() {
        return Err(ApiError::bad_request(
            "provide either api_key or clear_api_key, not both",
        ));
    }
    if clear_api_key {
        return Ok(String::new());
    }
    if let Some(key) = supplied_key {
        if key.len() > MAX_API_KEY_BYTES {
            return Err(ApiError::bad_request(format!(
                "api key must be at most {MAX_API_KEY_BYTES} bytes"
            )));
        }
        return Ok(key.to_string());
    }

    let stored = existing_key.unwrap_or("").to_string();
    if !stored.is_empty() && existing_origin != Some(new_origin) && !confirm_reuse_api_key {
        return Err(ApiError::conflict(
            "base URL origin changed; re-enter the API key or set \
             confirm_reuse_api_key=true to deliberately reuse the stored key",
        ));
    }
    Ok(stored)
}

pub async fn put_llm(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<LlmRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;

    let base_url = req.base_url.trim().trim_end_matches('/').to_string();
    if base_url.len() > MAX_BASE_URL_BYTES {
        return Err(ApiError::bad_request("base URL is too long"));
    }
    let origin = agent::url_origin(&base_url)
        .ok_or_else(|| ApiError::bad_request("base URL must start with http:// or https://"))?;
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(ApiError::bad_request(
            "base URL must start with http:// or https://",
        ));
    }
    let model = req.model.trim().to_string();
    if model.is_empty() {
        return Err(ApiError::bad_request("model is required"));
    }
    if model.len() > MAX_MODEL_BYTES {
        return Err(ApiError::bad_request("model name is too long"));
    }

    let existing = agent::load_config(&state).await?;
    let existing_origin = existing
        .as_ref()
        .and_then(|cfg| agent::url_origin(&cfg.base_url));
    let api_key = next_api_key(
        existing.as_ref().map(|cfg| cfg.api_key.as_str()),
        existing_origin.as_deref(),
        &origin,
        req.api_key.as_deref(),
        req.clear_api_key,
        req.confirm_reuse_api_key,
    )?;

    let vision_model = match req.vision_model {
        Some(model) => {
            let model = model.trim();
            if model.is_empty() {
                None
            } else if model.len() > MAX_MODEL_BYTES {
                return Err(ApiError::bad_request("vision model name is too long"));
            } else {
                Some(model.to_string())
            }
        }
        None => existing.as_ref().and_then(|cfg| cfg.vision_model.clone()),
    };
    let describe_images = req
        .describe_images
        .or_else(|| existing.as_ref().map(|cfg| cfg.describe_images))
        .unwrap_or(true);
    let temperature = req
        .temperature
        .or_else(|| existing.as_ref().map(|cfg| cfg.temperature))
        .unwrap_or(0.7);
    if !temperature.is_finite() {
        return Err(ApiError::bad_request("temperature must be a finite number"));
    }

    let cfg = LlmConfig {
        base_url,
        model,
        api_key,
        temperature: temperature.clamp(0.0, 2.0),
        vision_model,
        describe_images,
    };
    agent::save_config(&state, &cfg).await?;
    if let Err(err) = crate::files::vision::requeue_skipped(&state).await {
        tracing::warn!(error = %err, "could not requeue image descriptions");
    }

    Ok(Json(config_json(Some(&cfg))))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_key_retains_when_origin_is_unchanged() {
        let key = next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://api.example.com",
            None,
            false,
            false,
        )
        .unwrap();
        assert_eq!(key, "stored");
    }

    #[test]
    fn omitted_key_is_rejected_across_origin_without_confirmation() {
        let result = next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://other.example.com",
            None,
            false,
            false,
        );
        assert!(
            matches!(result, Err(ApiError { status, .. }) if status == axum::http::StatusCode::CONFLICT)
        );

        let confirmed = next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://other.example.com",
            None,
            false,
            true,
        )
        .unwrap();
        assert_eq!(confirmed, "stored");
    }

    #[test]
    fn new_key_replaces_and_clear_wins() {
        let replaced = next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://other.example.com",
            Some("fresh"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(replaced, "fresh");

        let cleared = next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://api.example.com",
            None,
            true,
            false,
        )
        .unwrap();
        assert!(cleared.is_empty());

        assert!(next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://api.example.com",
            Some("fresh"),
            true,
            false,
        )
        .is_err());
    }

    #[test]
    fn blank_key_keeps_stored_secret() {
        let key = next_api_key(
            Some("stored"),
            Some("https://api.example.com"),
            "https://api.example.com",
            Some("   "),
            false,
            false,
        )
        .unwrap();
        assert_eq!(key, "stored");
    }
}
