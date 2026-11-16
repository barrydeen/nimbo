use crate::{crypto, db, error::ApiError, routes::docs, state::AppState};
use axum::response::sse::Event;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{convert::Infallible, sync::OnceLock, time::Duration};
use tokio::sync::Semaphore;

pub const MAX_CHAT_MESSAGE_CHARS: usize = 8_000;
pub const MAX_CHAT_MESSAGE_BYTES: usize = 32 * 1024;
pub const MAX_ACTION_TEXT_BYTES: usize = 512 * 1024;
pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_CHAT_OUTPUT_TOKENS: u32 = 1_200;
pub const MAX_ACTION_OUTPUT_TOKENS: u32 = 1_200;
pub const MAX_LINK_PATHS: usize = 200;

const MAX_FTS_TERMS: usize = 16;
const MAX_CONTEXT_BYTES: usize = 24_000;
const MAX_PROVIDER_ERROR_BYTES: usize = 8 * 1024;
const MAX_PROVIDER_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SSE_LINE_BYTES: usize = 64 * 1024;
const LLM_CONCURRENCY: usize = 4;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(180);

static LLM_SEMAPHORE: Semaphore = Semaphore::const_new(LLM_CONCURRENCY);

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

/// Reused HTTP client; building a fresh client per call wastes connection pools
/// and TLS setup. Redirects are not followed for provider calls.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(PROVIDER_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(2))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
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

/// Normalized origin (`scheme://host[:port]`) used to decide whether a stored
/// provider secret may be reused. Paths/queries are intentionally ignored.
pub fn url_origin(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

fn with_auth(builder: reqwest::RequestBuilder, cfg: &LlmConfig) -> reqwest::RequestBuilder {
    let key = cfg.api_key.trim();
    if key.is_empty() {
        builder
    } else {
        builder.bearer_auth(key)
    }
}

/// Reads at most `max` bytes from a provider body. Unlike `.text()`/`.json()`
/// this bounds allocation even when the provider lies about or omits length.
async fn read_limited(response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| format!("stream error: {err}"))?;
        if out.len().saturating_add(chunk.len()) > max {
            return Err(format!("provider body exceeds {max} bytes"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
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
        if let Ok(content) = docs::read_document(state, &rel) {
            push_context(
                &mut context,
                format!("## {rel}\n{}\n\n", truncate(&content, 6_000)),
            );
        }
    }

    let fts_query = bounded_fts_query(query);
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
            if let Ok(content) = docs::read_document(state, &rel) {
                push_context(
                    &mut context,
                    format!("## {rel}\n{}\n\n", truncate(&content, 4_000)),
                );
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

        if !file_rows.is_empty() {
            let mut block = String::from("## Files in the drive\n");
            for (name, mime, summary) in file_rows {
                block.push_str(&format!(
                    "- {name} ({mime}){}\n",
                    if summary.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", truncate(summary.trim(), 600))
                    }
                ));
            }
            block.push('\n');
            push_context(&mut context, block);
        }
    }

    Ok(context)
}

/// The prompt is natural language, not an FTS program: cap the term count so a
/// long message cannot become an expensive query or a huge provider request.
fn bounded_fts_query(query: &str) -> String {
    let joined = query
        .split_whitespace()
        .take(MAX_FTS_TERMS)
        .collect::<Vec<_>>()
        .join(" ");
    docs::build_fts_query(&joined)
}

fn push_context(context: &mut String, block: String) {
    if context.len() >= MAX_CONTEXT_BYTES {
        return;
    }
    let remaining = MAX_CONTEXT_BYTES - context.len();
    context.push_str(truncate_bytes(&block, remaining));
}

fn truncate_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
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

/// Incremental SSE line decoder. Bytes are buffered until a newline so a
/// multibyte code point split across transport chunks is not corrupted, and
/// both total bytes and single-line length are hard-bounded.
#[derive(Debug, Default)]
pub struct SseBuffer {
    bytes: Vec<u8>,
    total: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SseError {
    TooLarge,
    LineTooLong,
    InvalidUtf8,
    Incomplete,
}

impl std::fmt::Display for SseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "provider stream exceeds {MAX_SSE_BYTES} bytes"),
            Self::LineTooLong => {
                write!(f, "provider stream line exceeds {MAX_SSE_LINE_BYTES} bytes")
            }
            Self::InvalidUtf8 => write!(f, "provider stream contained invalid UTF-8"),
            Self::Incomplete => write!(f, "provider stream ended mid-frame"),
        }
    }
}

impl SseBuffer {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, SseError> {
        if self.total.saturating_add(chunk.len()) > MAX_SSE_BYTES {
            return Err(SseError::TooLarge);
        }
        self.total += chunk.len();
        self.bytes.extend_from_slice(chunk);

        let mut lines = Vec::new();
        while let Some(position) = self.bytes.iter().position(|byte| *byte == b'\n') {
            if position > MAX_SSE_LINE_BYTES {
                return Err(SseError::LineTooLong);
            }
            let drained: Vec<u8> = self.bytes.drain(..=position).collect();
            let line_bytes = &drained[..position];
            let line = std::str::from_utf8(line_bytes).map_err(|_| SseError::InvalidUtf8)?;
            lines.push(line.trim_end_matches('\r').to_string());
        }
        if self.bytes.len() > MAX_SSE_LINE_BYTES {
            return Err(SseError::LineTooLong);
        }
        Ok(lines)
    }

    pub fn finish(&self) -> Result<(), SseError> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(SseError::Incomplete)
        }
    }
}

pub fn chat_stream(
    cfg: LlmConfig,
    messages: Vec<Value>,
) -> impl futures_util::Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let permit = match LLM_SEMAPHORE.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                yield Ok(Event::default().event("error").data("provider concurrency limit unavailable"));
                return;
            }
        };
        let _permit = permit;

        let payload = json!({
            "model": cfg.model,
            "messages": messages,
            "temperature": cfg.temperature,
            "max_tokens": MAX_CHAT_OUTPUT_TOKENS,
            "stream": true,
        });

        let response = match with_auth(http_client().post(endpoint(&cfg.base_url)), &cfg)
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
            let body = read_limited(response, MAX_PROVIDER_ERROR_BYTES)
                .await
                .unwrap_or_default();
            let body = String::from_utf8_lossy(&body);
            yield Ok(Event::default()
                .event("error")
                .data(format!("provider error {status}: {}", truncate(&body, 300))));
            return;
        }

        let mut stream = response.bytes_stream();
        let mut buffer = SseBuffer::default();

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield Ok(Event::default().event("error").data(format!("stream error: {err}")));
                    return;
                }
            };
            let lines = match buffer.push(&chunk) {
                Ok(lines) => lines,
                Err(err) => {
                    yield Ok(Event::default().event("error").data(err.to_string()));
                    return;
                }
            };

            for line in lines {
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    yield Ok(Event::default().event("done").data(""));
                    return;
                }
                let value = match serde_json::from_str::<Value>(data) {
                    Ok(value) => value,
                    Err(_) => {
                        yield Ok(Event::default()
                            .event("error")
                            .data("provider sent malformed stream data"));
                        return;
                    }
                };
                if let Some(text) = value
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(|content| content.as_str())
                {
                    if !text.is_empty() {
                        yield Ok(Event::default().data(text));
                    }
                }
            }
        }

        if let Err(err) = buffer.finish() {
            yield Ok(Event::default().event("error").data(err.to_string()));
            return;
        }
        // A provider that closes the stream without [DONE] gave us an incomplete
        // answer; reporting `done` would falsely mark it successful.
        yield Ok(Event::default()
            .event("error")
            .data("provider stream ended before completion"));
    }
}

/// Non-streaming completion used by inline document actions.
pub async fn complete(
    cfg: &LlmConfig,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String, ApiError> {
    let _permit = LLM_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| ApiError::service_unavailable("provider concurrency limit unavailable"))?;
    let payload = json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": cfg.temperature,
        "max_tokens": max_tokens.clamp(1, MAX_ACTION_OUTPUT_TOKENS),
    });

    let response = with_auth(http_client().post(endpoint(&cfg.base_url)), cfg)
        .json(&payload)
        .send()
        .await
        .map_err(|err| ApiError::bad_request(format!("provider request failed: {err}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = read_limited(response, MAX_PROVIDER_ERROR_BYTES)
            .await
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&body);
        return Err(ApiError::bad_request(format!(
            "provider error {status}: {}",
            truncate(&body, 300)
        )));
    }

    let body = read_limited(response, MAX_PROVIDER_JSON_BYTES)
        .await
        .map_err(|err| ApiError::bad_request(format!("invalid provider response: {err}")))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|err| ApiError::bad_request(format!("invalid provider response: {err}")))?;

    value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
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
    let _permit = LLM_SEMAPHORE
        .acquire()
        .await
        .map_err(|_| ApiError::service_unavailable("provider concurrency limit unavailable"))?;
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

    let response = with_auth(http_client().post(endpoint(&cfg.base_url)), cfg)
        .json(&payload)
        .send()
        .await
        .map_err(|err| ApiError::bad_request(format!("vision request failed: {err}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = read_limited(response, MAX_PROVIDER_ERROR_BYTES)
            .await
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&body);
        return Err(ApiError::bad_request(format!(
            "vision provider error {status}: {}. Choose a vision-capable model in Settings.",
            truncate(&body, 300)
        )));
    }

    let body = read_limited(response, MAX_PROVIDER_JSON_BYTES)
        .await
        .map_err(|err| ApiError::bad_request(format!("invalid vision response: {err}")))?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|err| ApiError::bad_request(format!("invalid vision response: {err}")))?;
    let content = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
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

    #[test]
    fn origins_are_normalized_for_secret_reuse() {
        assert_eq!(
            url_origin("https://API.Example.com:443/v1/").as_deref(),
            Some("https://api.example.com:443")
        );
        assert_eq!(
            url_origin("http://localhost:11434/v1").as_deref(),
            Some("http://localhost:11434")
        );
        assert_ne!(
            url_origin("https://api.example.com/v1"),
            url_origin("https://evil.example.com/v1")
        );
        assert!(url_origin("not a url").is_none());
    }

    #[test]
    fn sse_buffer_preserves_split_utf8() {
        let line = "data: h\u{e9}llo \u{1f600}\n";
        let bytes = line.as_bytes();
        let split = 7; // inside the two-byte é sequence
        let mut buffer = SseBuffer::default();
        assert!(buffer.push(&bytes[..split]).unwrap().is_empty());
        let lines = buffer.push(&bytes[split..]).unwrap();
        assert_eq!(lines, vec!["data: h\u{e9}llo \u{1f600}"]);
        assert!(buffer.finish().is_ok());
    }

    #[test]
    fn sse_buffer_bounds_lines_and_incomplete_frames() {
        let mut buffer = SseBuffer::default();
        let oversized = vec![b'a'; MAX_SSE_LINE_BYTES + 1];
        assert_eq!(buffer.push(&oversized), Err(SseError::LineTooLong));

        let mut buffer = SseBuffer::default();
        buffer.push(b"data: {").unwrap();
        assert_eq!(buffer.finish(), Err(SseError::Incomplete));
    }

    #[test]
    fn context_truncation_respects_char_boundaries() {
        assert_eq!(truncate_bytes("abc", 10), "abc");
        assert_eq!(truncate_bytes("h\u{e9}llo", 2), "h");
        assert_eq!(truncate_bytes("h\u{e9}llo", 3), "h\u{e9}");
    }

    #[test]
    fn fts_query_terms_are_capped() {
        let query = (0..100)
            .map(|index| format!("term{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let bounded = bounded_fts_query(&query);
        assert!(bounded.contains("term0"));
        assert!(bounded.contains("term15"));
        assert!(!bounded.contains("term16"));
    }
}
