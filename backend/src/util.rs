use axum::{
    body::Body,
    http::{header, StatusCode},
    response::Response,
};
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Builds a JSON response, optionally attaching a Set-Cookie header.
pub fn json_response(
    status: StatusCode,
    value: serde_json::Value,
    cookie: Option<String>,
) -> Response {
    let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header(header::SET_COOKIE, cookie);
    }
    builder.body(Body::from(body)).expect("valid json response")
}
