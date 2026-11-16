use crate::{crypto, db, error::ApiError, routes::docs, state::AppState};
use axum::response::sse::Event;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::convert::Infallible;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_temperature() -> f32 {
    0.7
}

pub fn default_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

pub async fn load_config(state: &AppState) -> Result<Option<LlmConfig>, ApiError> {
    let Some(stored) = db::get_setting(&state.pool, "llm_config").await? else {
        return Ok(None);
    };
    let plain = crypto::decrypt(&state.secret, &stored)
        .map_err(|_| ApiError::internal("could not decrypt LLM settings"))?;
    let cfg: LlmConfig = serde_json::from_slice(&plain)
        .map_err(|_| ApiError::internal("stored LLM settings are invalid"))?;
    Ok(Some(cfg))
}

pub async fn save_config(state: &AppState, cfg: &LlmConfig) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec(cfg)
        .map_err(|_| ApiError::internal("could not serialize LLM settings"))?;
    let encoded = crypto::encrypt(&state.secret, &bytes)
        .map_err(|_| ApiError::internal("could not encrypt LLM settings"))?;
    db::set_setting(&state.pool, "llm_config", &encoded).await?;
    Ok(())
}

fn endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .expect("http client")
}

/// Reads the current document and a handful of search-relevant documents to
/// ground the agent's answer in the vault.
pub async fn gather_context(
    state: &AppState,
    query: &str,
    path: Option<&str>,
) -> Result<String, ApiError> {
    let mut context = String::new();

    if let Some(path) = path {
        let rel = docs::normalize_path(path)?;
        if let Ok(content) = std::fs::read_to_string(state.cfg.vault_dir().join(&rel)) {
            context.push_str(&format!("## {rel}\n{}\n\n", truncate(&content, 6000)));
        }
    }

    let fts_query = docs::build_fts_query(query);
    if !fts_query.is_empty() {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT path FROM docs_fts WHERE docs_fts MATCH ? \
             ORDER BY bm25(docs_fts) LIMIT 5",
        )
        .bind(&fts_query)
        .fetch_all(&state.pool)
        .await?;

        for (rel,) in rows {
            if Some(rel.as_str()) == path {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(state.cfg.vault_dir().join(&rel)) {
                if context.len() > 16_000 {
                    break;
                }
                context.push_str(&format!("## {rel}\n{}\n\n", truncate(&content, 4000)));
            }
        }
    }

    Ok(context)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "\n…"
    }
}

pub fn system_prompt() -> String {
    "You are Nimbo, an assistant embedded in a self-hosted markdown knowledge base. \
     Answer using the provided vault context when it is relevant, and say so when it is not. \
     Be concise and practical. Refer to other documents with [[wikilinks]]."
        .to_string()
}

#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
    pub path: Option<String>,
}

pub fn chat_stream(
    cfg: LlmConfig,
    messages: Vec<Value>,
) -> impl futures_util::Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let payload = json!({
            "model": cfg.model,
            "messages": messages,
            "temperature": cfg.temperature,
            "stream": true,
        });

        let response = match client()
            .post(endpoint(&cfg.base_url))
            .bearer_auth(&cfg.api_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                yield Ok(Event::default().event("error").data(format!("request failed: {err}")));
                return;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            yield Ok(Event::default()
                .event("error")
                .data(format!("provider error {status}: {}", truncate(&body, 300))));
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield Ok(Event::default().event("error").data(format!("stream error: {err}")));
                    break;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line: String = buffer.drain(..=pos).collect();
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        yield Ok(Event::default().event("done").data(""));
                        return;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        if let Some(text) = value
                            .get("choices")
                            .and_then(|c| c.get(0))
                            .and_then(|c| c.get("delta"))
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !text.is_empty() {
                                yield Ok(Event::default().data(text));
                            }
                        }
                    }
                }
            }
        }

        yield Ok(Event::default().event("done").data(""));
    }
}

/// Non-streaming completion used by inline document actions.
pub async fn complete(
    cfg: &LlmConfig,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String, ApiError> {
    let payload = json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": cfg.temperature,
        "max_tokens": max_tokens,
    });

    let response = client()
        .post(endpoint(&cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|err| ApiError::bad_request(format!("provider request failed: {err}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ApiError::bad_request(format!(
            "provider error {status}: {}",
            truncate(&body, 300)
        )));
    }

    let value: Value = response
        .json()
        .await
        .map_err(|err| ApiError::bad_request(format!("invalid provider response: {err}")))?;

    value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request("provider returned no content"))
}
