use crate::{crypto, db, error::ApiError, routes::docs, state::AppState};
use axum::response::sse::Event;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
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
    /// Optional vision-capable model override. Falls back to `model`.
    #[serde(default)]
    pub vision_model: Option<String>,
    /// Whether newly uploaded images are described automatically.
    #[serde(default = "default_true")]
    pub describe_images: bool,
}

fn default_temperature() -> f32 {
    0.7
}

fn default_true() -> bool {
    true
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

        let file_rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT n.name, COALESCE(n.mime, 'unknown'), \
                    COALESCE(NULLIF(n.vision_description, ''), substr(n.text_content, 1, 600), '') \
             FROM files_fts f JOIN nodes n ON n.id = f.file_id \
             WHERE files_fts MATCH ? \
             ORDER BY bm25(files_fts, 0.0, 10.0, 1.0, 5.0, 2.0) LIMIT 4",
        )
        .bind(&fts_query)
        .fetch_all(&state.pool)
        .await?;

        if !file_rows.is_empty() && context.len() <= 20_000 {
            context.push_str("## Files in the drive\n");
            for (name, mime, summary) in file_rows {
                context.push_str(&format!(
                    "- {name} ({mime}){}\n",
                    if summary.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", truncate(summary.trim(), 600))
                    }
                ));
            }
            context.push('\n');
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
    "You are Nimbo, an assistant embedded in a self-hosted markdown knowledge base and file drive. \
     Answer using the provided vault and file context when it is relevant, and say so when it is not. \
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

#[derive(Clone, Debug, Default)]
pub struct VisionDescription {
    pub description: String,
    pub text: String,
    pub tags: Vec<String>,
    pub model: String,
}

#[derive(Deserialize, Default)]
struct VisionReply {
    #[serde(default)]
    description: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    tags: Vec<String>,
}

/// Sends a downscaled JPEG to an OpenAI-compatible vision model and asks for a
/// searchable description, any visible text, and tags.
pub async fn describe_image(
    cfg: &LlmConfig,
    jpeg: &[u8],
    filename: &str,
) -> Result<VisionDescription, ApiError> {
    let model = cfg
        .vision_model
        .as_deref()
        .filter(|model| !model.trim().is_empty())
        .unwrap_or(&cfg.model)
        .to_string();
    let data_url = format!("data:image/jpeg;base64,{}", BASE64.encode(jpeg));
    let prompt = format!(
        "Describe this image for search and accessibility (filename: {filename}). \
         Return only a JSON object with exactly these keys: \
         {{\"description\": \"one useful paragraph describing what is visible\", \
           \"text\": \"all legible text in the image, verbatim; empty string if none\", \
           \"tags\": [\"5 to 12 short lowercase keywords\"]}}. \
         Do not invent details that are not visible."
    );
    let payload = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": data_url } }
            ]
        }],
        "temperature": 0.2,
        "max_tokens": 900
    });

    let response = client()
        .post(endpoint(&cfg.base_url))
        .bearer_auth(&cfg.api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|err| ApiError::bad_request(format!("vision request failed: {err}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ApiError::bad_request(format!(
            "vision provider error {status}: {}. Choose a vision-capable model in Settings.",
            truncate(&body, 300)
        )));
    }

    let value: Value = response
        .json()
        .await
        .map_err(|err| ApiError::bad_request(format!("invalid vision response: {err}")))?;
    let content = value
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| ApiError::bad_request("vision provider returned no content"))?;

    let mut described = parse_vision_reply(content);
    described.model = model;
    Ok(described)
}

pub fn parse_vision_reply(content: &str) -> VisionDescription {
    let cleaned = content.trim();
    let cleaned = cleaned
        .strip_prefix("```json")
        .or_else(|| cleaned.strip_prefix("```"))
        .unwrap_or(cleaned)
        .trim()
        .trim_end_matches("```")
        .trim();

    let candidate = if let (Some(start), Some(end)) = (cleaned.find('{'), cleaned.rfind('}')) {
        &cleaned[start..=end]
    } else {
        cleaned
    };

    if let Ok(reply) = serde_json::from_str::<VisionReply>(candidate) {
        let description = if reply.description.trim().is_empty() {
            cleaned.to_string()
        } else {
            reply.description.trim().to_string()
        };
        let tags = reply
            .tags
            .into_iter()
            .map(|tag| tag.trim().to_lowercase())
            .filter(|tag| !tag.is_empty())
            .take(20)
            .collect();
        return VisionDescription {
            description,
            text: reply.text.trim().to_string(),
            tags,
            model: String::new(),
        };
    }

    // Some models ignore the JSON instruction. Keep their prose as the description
    // so the image is still searchable.
    VisionDescription {
        description: cleaned.to_string(),
        text: String::new(),
        tags: Vec::new(),
        model: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_vision_reply() {
        let reply = parse_vision_reply(
            "```json\n{\"description\":\"A red sailboat\",\"text\":\"SEA 42\",\"tags\":[\"Boat\",\" sea \"]}\n```",
        );
        assert_eq!(reply.description, "A red sailboat");
        assert_eq!(reply.text, "SEA 42");
        assert_eq!(reply.tags, vec!["boat", "sea"]);
    }

    #[test]
    fn falls_back_to_plain_prose() {
        let reply = parse_vision_reply("Just a plain description of a cat.");
        assert_eq!(reply.description, "Just a plain description of a cat.");
        assert!(reply.tags.is_empty());
    }
}
