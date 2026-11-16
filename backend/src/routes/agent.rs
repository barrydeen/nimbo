use crate::{agent, auth::AuthUser, error::ApiError, routes::docs, state::AppState};
use axum::{
    extract::{Extension, State},
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{convert::Infallible, sync::Arc, time::Duration};

const MAX_ACTION_CHARS: usize = 8_000;
const MAX_TAIL_CHARS: usize = 4_000;
const MAX_ACTION_NAME_BYTES: usize = 64;
const MAX_INSTRUCTION_BYTES: usize = 4 * 1024;
const MAX_LINK_PATH_TEXT_CHARS: usize = 12_000;

pub async fn chat(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Json(req): Json<agent::ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    agent::validate_chat_request(&req)?;

    let cfg = agent::load_config(&state)
        .await?
        .ok_or_else(|| ApiError::bad_request("no LLM provider configured"))?;

    let stream = agent::agent_stream(state, cfg, req);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

#[derive(Deserialize)]
pub struct ActionRequest {
    pub action: String,
    pub path: Option<String>,
    pub text: Option<String>,
    pub instruction: Option<String>,
}

fn validate_path(path: Option<&str>) -> Result<(), ApiError> {
    if let Some(path) = path {
        if path.len() > agent::MAX_PATH_BYTES {
            return Err(ApiError::bad_request("document path is too long"));
        }
    }
    Ok(())
}

fn prefix(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn suffix(text: &str, max_chars: usize) -> String {
    let mut chars: Vec<char> = text.chars().rev().take(max_chars).collect();
    chars.reverse();
    chars.into_iter().collect()
}

pub async fn action(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Json(req): Json<ActionRequest>,
) -> Result<Json<Value>, ApiError> {
    let action = req.action.trim().to_string();
    if action.is_empty() || action.len() > MAX_ACTION_NAME_BYTES {
        return Err(ApiError::bad_request("invalid action name"));
    }
    validate_path(req.path.as_deref())?;
    if let Some(text) = req.text.as_deref() {
        if text.len() > agent::MAX_ACTION_TEXT_BYTES {
            return Err(ApiError::payload_too_large(format!(
                "action text must be at most {} bytes",
                agent::MAX_ACTION_TEXT_BYTES
            )));
        }
    }
    if let Some(instruction) = req.instruction.as_deref() {
        if instruction.len() > MAX_INSTRUCTION_BYTES {
            return Err(ApiError::bad_request(format!(
                "instruction must be at most {MAX_INSTRUCTION_BYTES} bytes"
            )));
        }
    }

    let cfg = agent::load_config(&state)
        .await?
        .ok_or_else(|| ApiError::bad_request("no LLM provider configured"))?;
    let target = agent::document_target(&cfg)?;

    let text = match req.text {
        Some(text) if !text.trim().is_empty() => text,
        _ => {
            let path = req
                .path
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("provide text or a document path"))?;
            docs::read_document(&state, path)?
        }
    };

    if text.trim().is_empty() {
        return Err(ApiError::bad_request("document is empty"));
    }

    let total_chars = text.chars().count();
    let processed = match action.as_str() {
        "suggest" | "continue" => suffix(&text, MAX_TAIL_CHARS),
        _ => prefix(&text, MAX_ACTION_CHARS),
    };
    let processed_chars = processed.chars().count();
    let truncated = total_chars > processed_chars;

    let (system, user) = match action.as_str() {
        "suggest" => (
            "You are an inline autocomplete for a markdown editor. Given the text before \
             the cursor, write the single next line or short sentence (at most 25 words). \
             Match the existing tone, language and markdown style. If the text ends \
             mid-sentence, finish it. Output only the continuation text: no preamble, no \
             quotes, no code fences, no leading newline.",
            processed,
        ),
        "continue" => (
            "You are a writing assistant. Continue the text naturally from where it stops. \
             Match the existing tone and markdown style. Output only the continuation, with no preamble.",
            processed,
        ),
        "grammar" => (
            "Fix spelling, grammar, punctuation and clarity. Preserve the author's meaning, tone \
             and markdown formatting. Output only the corrected text.",
            processed,
        ),
        "rewrite" => (
            "Rewrite the text to be clearer and more concise while keeping the meaning and \
             markdown structure. Output only the rewritten text.",
            processed,
        ),
        "summarize" => (
            "Summarize the text in a short paragraph followed by three bullet points. \
             Output only the summary.",
            processed,
        ),
        "links" => {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT path FROM documents ORDER BY lower(path) LIMIT ?")
                    .bind(agent::MAX_LINK_PATHS as i64)
                    .fetch_all(&state.pool)
                    .await?;
            let paths = rows
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>()
                .join("\n")
                .chars()
                .take(MAX_LINK_PATH_TEXT_CHARS)
                .collect::<String>();
            (
                "You suggest wikilinks. Given a document and a list of existing document paths, \
                 propose up to five links using [[path]] syntax, each with a one-line reason. \
                 Only use paths from the list. Output a plain list.",
                format!("Document:\n\n{processed}\n\nExisting documents:\n{paths}"),
            )
        }
        "tags" => (
            "Suggest up to five short lowercase tags for the document, comma separated. \
             Output only the tags.",
            processed,
        ),
        other => {
            let instruction = req.instruction.clone().unwrap_or_else(|| other.to_string());
            (
                "You are a writing assistant. Follow the instruction and output only the result.",
                format!("Instruction: {instruction}\n\nText:\n\n{processed}"),
            )
        }
    };

    let max_tokens = if action == "suggest" {
        90
    } else {
        agent::MAX_ACTION_OUTPUT_TOKENS
    };
    let result = agent::complete(&target, cfg.temperature, system, &user, max_tokens).await?;
    Ok(Json(json!({
        "action": action,
        "text": result,
        "processed_chars": processed_chars,
        "truncated": truncated,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_suffixes_are_unicode_scalar_based() {
        let text = "a\u{e9}\u{1f600}bc";
        assert_eq!(prefix(text, 3), "a\u{e9}\u{1f600}");
        assert_eq!(suffix(text, 3), "\u{1f600}bc");
        assert_eq!(prefix(text, 100), text);
        assert_eq!(suffix(text, 100), text);
    }

    #[test]
    fn processed_counts_match_the_truncation_window() {
        let long = "x".repeat(MAX_ACTION_CHARS + 10);
        let processed = prefix(&long, MAX_ACTION_CHARS);
        assert_eq!(processed.chars().count(), MAX_ACTION_CHARS);
        assert!(long.chars().count() > processed.chars().count());

        let short = "x".repeat(MAX_ACTION_CHARS);
        let processed = prefix(&short, MAX_ACTION_CHARS);
        assert!(!(short.chars().count() > processed.chars().count()));
    }
}
