use axum::Json;
use serde_json::{json, Value};

pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn api_not_found() -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::NOT_FOUND,
        Json(json!({ "error": "not found" })),
    )
}
