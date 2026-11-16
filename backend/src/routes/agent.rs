use crate::{agent, auth::AuthUser, error::ApiError, routes::docs, state::AppState};
use axum::{
    extract::{Extension, State},
    response::sse::{Event, Sse},
    Json,
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;

pub async fn chat(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Json(req): Json<agent::ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    if req.message.trim().is_empty() {
        return Err(ApiError::bad_request("message is required"));
    }
    let cfg = agent::load_config(&state)
        .await?
        .ok_or_else(|| ApiError::bad_request("no LLM provider configured"))?;

    let context = agent::gather_context(&state, &req.message, req.path.as_deref()).await?;
    let mut messages = vec![json!({
        "role": "system",
        "content": agent::system_prompt(),
    })];
    if !context.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": format!("Vault context:\n\n{context}"),
        }));
    }
    messages.push(json!({ "role": "user", "content": req.message }));

    Ok(Sse::new(agent::chat_stream(cfg, messages)))
}

#[derive(Deserialize)]
pub struct ActionRequest {
    pub action: String,
    pub path: Option<String>,
    pub text: Option<String>,
    pub instruction: Option<String>,
}

pub async fn action(
    State(state): State<Arc<AppState>>,
    Extension(_user): Extension<AuthUser>,
    Json(req): Json<ActionRequest>,
) -> Result<Json<Value>, ApiError> {
    let cfg = agent::load_config(&state)
        .await?
        .ok_or_else(|| ApiError::bad_request("no LLM provider configured"))?;

    let text = match req.text {
        Some(text) if !text.trim().is_empty() => text,
        _ => {
            let path = req
                .path
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("provide text or a document path"))?;
            let rel = docs::normalize_path(path)?;
            std::fs::read_to_string(state.cfg.vault_dir().join(&rel))?
        }
    };

    if text.trim().is_empty() {
        return Err(ApiError::bad_request("document is empty"));
    }

    let clipped = text.chars().take(8000).collect::<String>();

    let (system, user) = match req.action.as_str() {
        "suggest" => {
            let tail: String = text
                .chars()
                .rev()
                .take(4000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            (
                "You are an inline autocomplete for a markdown editor. Given the text before \
                 the cursor, write the single next line or short sentence (at most 25 words). \
                 Match the existing tone, language and markdown style. If the text ends \
                 mid-sentence, finish it. Output only the continuation text: no preamble, no \
                 quotes, no code fences, no leading newline.",
                tail,
            )
        }
        "continue" => {
            let tail: String = text
                .chars()
                .rev()
                .take(4000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            (
                "You are a writing assistant. Continue the text naturally from where it stops. \
                 Match the existing tone and markdown style. Output only the continuation, with no preamble.",
                tail,
            )
        }
        "grammar" => (
            "Fix spelling, grammar, punctuation and clarity. Preserve the author's meaning, tone \
             and markdown formatting. Output only the corrected text.",
            clipped,
        ),
        "rewrite" => (
            "Rewrite the text to be clearer and more concise while keeping the meaning and \
             markdown structure. Output only the rewritten text.",
            clipped,
        ),
        "summarize" => (
            "Summarize the text in a short paragraph followed by three bullet points. \
             Output only the summary.",
            clipped,
        ),
        "links" => {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT path FROM documents ORDER BY lower(path)")
                    .fetch_all(&state.pool)
                    .await?;
            let paths = rows.into_iter().map(|r| r.0).collect::<Vec<_>>().join("\n");
            (
                "You suggest wikilinks. Given a document and a list of existing document paths, \
                 propose up to five links using [[path]] syntax, each with a one-line reason. \
                 Only use paths from the list. Output a plain list.",
                format!("Document:\n\n{clipped}\n\nExisting documents:\n{paths}"),
            )
        }
        "tags" => (
            "Suggest up to five short lowercase tags for the document, comma separated. \
             Output only the tags.",
            clipped,
        ),
        other => {
            let instruction = req.instruction.clone().unwrap_or_else(|| other.to_string());
            (
                "You are a writing assistant. Follow the instruction and output only the result.",
                format!("Instruction: {instruction}\n\nText:\n\n{clipped}"),
            )
        }
    };

    let max_tokens = if req.action == "suggest" { 90 } else { 1200 };
    let result = agent::complete(&cfg, system, &user, max_tokens).await?;
    Ok(Json(json!({ "action": req.action, "text": result })))
}
