use crate::{
    agent::{self, LlmConfig, ModelRef, ProviderConfig},
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
use std::{collections::HashSet, sync::Arc};

const MAX_BASE_URL_BYTES: usize = 2_048;
const MAX_API_KEY_BYTES: usize = 4_096;

fn require_admin(user: &AuthUser) -> Result<(), ApiError> {
    if user.role == "admin" {
        Ok(())
    } else {
        Err(ApiError::forbidden("admin only"))
    }
}

fn model_ref_json(model_ref: &ModelRef) -> Value {
    json!({
        "provider_id": &model_ref.provider_id,
        "model": &model_ref.model,
    })
}

fn provider_json(provider: &ProviderConfig) -> Value {
    json!({
        "id": &provider.id,
        "name": &provider.name,
        "base_url": &provider.base_url,
        "api_key_set": !provider.api_key.is_empty(),
        "models": &provider.models,
    })
}

fn config_json(cfg: Option<&LlmConfig>) -> Value {
    match cfg {
        Some(cfg) => json!({
            "configured": agent::is_configured(cfg),
            "providers": cfg.providers.iter().map(provider_json).collect::<Vec<_>>(),
            "document_model": cfg.document_model.as_ref().map(model_ref_json),
            "indexing_model": cfg.indexing_model.as_ref().map(model_ref_json),
            "temperature": cfg.temperature,
            "describe_images": cfg.describe_images,
        }),
        None => json!({
            "configured": false,
            "providers": [],
            "document_model": null,
            "indexing_model": null,
            "temperature": 0.7,
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
pub struct ProviderRequest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    /// Omitted or empty retains; a non-empty value replaces.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Explicitly remove the stored provider key.
    #[serde(default)]
    pub clear_api_key: bool,
    /// Required when changing provider origin while retaining a stored key.
    #[serde(default)]
    pub confirm_reuse_api_key: bool,
}

#[derive(Deserialize)]
pub struct ModelRefRequest {
    pub provider_id: String,
    pub model: String,
}

#[derive(Deserialize)]
pub struct LlmRequest {
    #[serde(default)]
    pub providers: Vec<ProviderRequest>,
    #[serde(default)]
    pub document_model: Option<ModelRefRequest>,
    #[serde(default)]
    pub indexing_model: Option<ModelRefRequest>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub describe_images: Option<bool>,
}

#[derive(Deserialize)]
pub struct ModelsRequest {
    /// Existing provider whose stored key may be reused when the origin matches.
    #[serde(default)]
    pub provider_id: Option<String>,
    pub base_url: String,
    /// Omitted or empty falls back to the stored provider key for the same origin.
    #[serde(default)]
    pub api_key: Option<String>,
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

fn normalize_base_url(raw: &str) -> Result<(String, String), ApiError> {
    let base_url = raw.trim().trim_end_matches('/').to_string();
    if base_url.len() > MAX_BASE_URL_BYTES {
        return Err(ApiError::bad_request("base URL is too long"));
    }
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err(ApiError::bad_request(
            "base URL must start with http:// or https://",
        ));
    }
    let origin = agent::url_origin(&base_url)
        .ok_or_else(|| ApiError::bad_request("base URL must start with http:// or https://"))?;
    Ok((base_url, origin))
}

fn normalize_request_models(models: Vec<String>) -> Result<Vec<String>, ApiError> {
    if models.len() > agent::MAX_MODELS {
        return Err(ApiError::bad_request(format!(
            "a provider may have at most {} models",
            agent::MAX_MODELS
        )));
    }

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for model in models {
        let model = model.trim().to_string();
        if model.is_empty() {
            continue;
        }
        if model.len() > agent::MAX_MODEL_BYTES {
            return Err(ApiError::bad_request(format!(
                "model names must be at most {} bytes",
                agent::MAX_MODEL_BYTES
            )));
        }
        if seen.insert(model.clone()) {
            out.push(model);
        }
    }
    Ok(out)
}

fn validate_model_ref(
    model_ref: &ModelRefRequest,
    providers: &[ProviderConfig],
    role: &str,
) -> Result<ModelRef, ApiError> {
    let provider_id = model_ref.provider_id.trim();
    let model = model_ref.model.trim();
    if provider_id.is_empty() {
        return Err(ApiError::bad_request(format!(
            "{role} model is missing its provider"
        )));
    }
    if model.is_empty() {
        return Err(ApiError::bad_request(format!("{role} model is required")));
    }
    if model.len() > agent::MAX_MODEL_BYTES {
        return Err(ApiError::bad_request(format!(
            "{role} model name is too long"
        )));
    }
    let provider = providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| {
            ApiError::bad_request(format!("{role} model references an unknown provider"))
        })?;
    if !provider.models.iter().any(|available| available == model) {
        return Err(ApiError::bad_request(format!(
            "{role} model must be one of the selected models for its provider"
        )));
    }
    Ok(ModelRef {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
    })
}

pub async fn put_llm(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<LlmRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;

    if req.providers.len() > agent::MAX_PROVIDERS {
        return Err(ApiError::bad_request(format!(
            "at most {} providers are supported",
            agent::MAX_PROVIDERS
        )));
    }

    let existing = agent::load_config(&state).await?;
    let mut provider_ids = HashSet::new();
    let mut providers = Vec::new();

    for request in req.providers {
        let id = request.id.trim().to_string();
        if id.is_empty() {
            return Err(ApiError::bad_request("provider id is required"));
        }
        if id.len() > agent::MAX_PROVIDER_ID_BYTES {
            return Err(ApiError::bad_request("provider id is too long"));
        }
        if !provider_ids.insert(id.clone()) {
            return Err(ApiError::bad_request("provider ids must be unique"));
        }

        let name = request.name.trim().to_string();
        if name.len() > agent::MAX_PROVIDER_NAME_BYTES {
            return Err(ApiError::bad_request(format!(
                "provider names must be at most {} bytes",
                agent::MAX_PROVIDER_NAME_BYTES
            )));
        }

        let (base_url, origin) = normalize_base_url(&request.base_url)?;
        let models = normalize_request_models(request.models)?;

        let existing_provider = existing
            .as_ref()
            .and_then(|cfg| cfg.providers.iter().find(|provider| provider.id == id));
        let existing_origin =
            existing_provider.and_then(|provider| agent::url_origin(&provider.base_url));
        let api_key = next_api_key(
            existing_provider.map(|provider| provider.api_key.as_str()),
            existing_origin.as_deref(),
            &origin,
            request.api_key.as_deref(),
            request.clear_api_key,
            request.confirm_reuse_api_key,
        )?;

        providers.push(ProviderConfig {
            id,
            name,
            base_url,
            api_key,
            models,
        });
    }

    let document_model = match req.document_model {
        Some(model_ref) => Some(validate_model_ref(
            &model_ref,
            &providers,
            "document agent",
        )?),
        None => None,
    };
    let indexing_model = match req.indexing_model {
        Some(model_ref) => Some(validate_model_ref(&model_ref, &providers, "file indexing")?),
        None => None,
    };

    if providers.is_empty() {
        if document_model.is_some() || indexing_model.is_some() {
            return Err(ApiError::bad_request(
                "remove model roles when no providers are configured",
            ));
        }
    } else if document_model.is_none() {
        return Err(ApiError::bad_request("choose a document agent model"));
    }

    let temperature = req
        .temperature
        .or_else(|| existing.as_ref().map(|cfg| cfg.temperature))
        .unwrap_or(0.7);
    if !temperature.is_finite() {
        return Err(ApiError::bad_request("temperature must be a finite number"));
    }
    let describe_images = req
        .describe_images
        .or_else(|| existing.as_ref().map(|cfg| cfg.describe_images))
        .unwrap_or(true);

    let cfg = LlmConfig {
        providers,
        document_model,
        indexing_model,
        temperature: temperature.clamp(0.0, 2.0),
        describe_images,
    };
    agent::save_config(&state, &cfg).await?;
    if let Err(err) = crate::files::vision::requeue_skipped(&state).await {
        tracing::warn!(error = %err, "could not requeue image descriptions");
    }

    Ok(Json(config_json(Some(&cfg))))
}

pub async fn list_llm_models(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
    Json(req): Json<ModelsRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;

    let (base_url, origin) = normalize_base_url(&req.base_url)?;
    let supplied = req
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let mut api_key = supplied.unwrap_or("").to_string();

    if api_key.is_empty() {
        if let Some(provider_id) = req
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            if let Some(cfg) = agent::load_config(&state).await? {
                if let Some(provider) = cfg
                    .providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
                {
                    if agent::url_origin(&provider.base_url).as_deref() == Some(origin.as_str()) {
                        api_key = provider.api_key.clone();
                    }
                }
            }
        }
    }

    let models = agent::list_models(&base_url, &api_key).await?;
    Ok(Json(json!({ "models": models })))
}

pub async fn test_llm(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<AuthUser>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&user)?;
    let cfg = agent::load_config(&state)
        .await?
        .ok_or_else(|| ApiError::bad_request("no provider configured"))?;
    let target = agent::document_target(&cfg)?;

    let reply = agent::complete(
        &target,
        cfg.temperature,
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

    #[test]
    fn model_refs_must_be_selected_for_their_provider() {
        let providers = vec![ProviderConfig {
            id: "p1".into(),
            name: String::new(),
            base_url: "https://api.example.com/v1".into(),
            api_key: String::new(),
            models: vec!["gpt-4o".into()],
        }];

        let ok = validate_model_ref(
            &ModelRefRequest {
                provider_id: "p1".into(),
                model: "gpt-4o".into(),
            },
            &providers,
            "document agent",
        )
        .unwrap();
        assert_eq!(ok.model, "gpt-4o");

        assert!(validate_model_ref(
            &ModelRefRequest {
                provider_id: "p1".into(),
                model: "missing".into(),
            },
            &providers,
            "document agent",
        )
        .is_err());

        assert!(validate_model_ref(
            &ModelRefRequest {
                provider_id: "p2".into(),
                model: "gpt-4o".into(),
            },
            &providers,
            "document agent",
        )
        .is_err());
    }

    #[test]
    fn request_models_are_trimmed_and_deduped() {
        let models =
            normalize_request_models(vec![" gpt-4o ".into(), "gpt-4o".into(), "".into()]).unwrap();
        assert_eq!(models, vec!["gpt-4o"]);
    }
}
