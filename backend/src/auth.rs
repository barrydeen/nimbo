use crate::{error::ApiError, state::AppState, util::sha256_hex};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, Method},
    middleware::Next,
    response::Response,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use std::sync::Arc;

pub const COOKIE_NAME: &str = "nimbo_session";
pub const DEVICE_TOKEN_PREFIX: &str = "nmb_";
const MAX_FAILURES: usize = 5;
const FAILURE_WINDOW_SECS: i64 = 300;

#[derive(Clone, Debug, serde::Serialize)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: String,
}

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| ApiError::internal(format!("password hashing failed: {err}")))
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Returns (raw token, stored hash). Only the hash is persisted.
pub fn new_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let hash = sha256_hex(token.as_bytes());
    (token, hash)
}

/// Device tokens carry a `nmb_` prefix so they are recognisable and greppable
/// in config files. Returns (raw token, stored hash of the full raw token).
pub fn new_device_token() -> (String, String) {
    let (secret, _) = new_token();
    let raw = format!("{DEVICE_TOKEN_PREFIX}{secret}");
    let hash = sha256_hex(raw.as_bytes());
    (raw, hash)
}

pub fn session_cookie(token: &str, max_age_secs: i64, secure: bool) -> String {
    let mut cookie =
        format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub fn clear_cookie(secure: bool) -> String {
    let mut cookie = format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{COOKIE_NAME}=");
    raw.split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&prefix).map(str::to_string))
}

pub fn bearer_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

pub async fn create_session(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    ttl_secs: i64,
) -> Result<String, ApiError> {
    let (token, token_hash) = new_token();
    let now = crate::now();
    sqlx::query(
        "INSERT INTO sessions (id, user_id, token_hash, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(&token_hash)
    .bind(now)
    .bind(now + ttl_secs)
    .execute(pool)
    .await?;
    Ok(token)
}

pub async fn lookup_session(
    pool: &sqlx::SqlitePool,
    token: &str,
) -> Result<Option<AuthUser>, ApiError> {
    let token_hash = sha256_hex(token.as_bytes());
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT u.id, u.username, u.role FROM sessions s \
         JOIN users u ON u.id = s.user_id \
         WHERE s.token_hash = ? AND s.expires_at > ?",
    )
    .bind(&token_hash)
    .bind(crate::now())
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, username, role)| AuthUser { id, username, role }))
}

pub async fn delete_session(pool: &sqlx::SqlitePool, token: &str) -> Result<(), ApiError> {
    let token_hash = sha256_hex(token.as_bytes());
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// Resolve a device token (`nmb_…`) to its user, touching `last_used_at`.
pub async fn lookup_device_token(
    pool: &sqlx::SqlitePool,
    token: &str,
) -> Result<Option<AuthUser>, ApiError> {
    let token_hash = sha256_hex(token.as_bytes());
    let now = crate::now();
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT t.id, u.id, u.username, u.role FROM device_tokens t \
         JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = ? AND t.revoked_at IS NULL \
           AND (t.expires_at IS NULL OR t.expires_at > ?)",
    )
    .bind(&token_hash)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    let Some((token_id, id, username, role)) = row else {
        return Ok(None);
    };
    sqlx::query("UPDATE device_tokens SET last_used_at = ? WHERE id = ?")
        .bind(now)
        .bind(&token_id)
        .execute(pool)
        .await?;
    Ok(Some(AuthUser { id, username, role }))
}

/// Middleware: rejects requests without valid credentials. Accepts either a
/// device token (`Authorization: Bearer nmb_…`) for native clients or the
/// `nimbo_session` cookie for browser sessions.
pub async fn require_session(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let user = if let Some(bearer) = bearer_token_from_headers(req.headers()) {
        lookup_device_token(&state.pool, &bearer)
            .await?
            .ok_or_else(ApiError::unauthorized)?
    } else {
        let token = token_from_headers(req.headers()).ok_or_else(ApiError::unauthorized)?;
        lookup_session(&state.pool, &token)
            .await?
            .ok_or_else(ApiError::unauthorized)?
    };
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

/// Middleware: rejects cross-origin mutating requests.
pub async fn origin_check(
    State(_state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        if let Some(origin) = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
        {
            let host = req
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let origin_host = origin.split("://").nth(1).unwrap_or("");
            if !host.is_empty() && origin_host != host {
                return Err(ApiError::forbidden("cross-origin request rejected"));
            }
        }
    }
    Ok(next.run(req).await)
}

/// Basic in-memory brute-force protection keyed by username.
pub fn check_rate_limit(state: &AppState, key: &str) -> Result<(), ApiError> {
    let now = crate::now();
    let mut guard = state.login_failures.lock().expect("limiter lock");
    let failures = guard.entry(key.to_string()).or_default();
    failures.retain(|t| now - *t < FAILURE_WINDOW_SECS);
    if failures.len() >= MAX_FAILURES {
        return Err(ApiError::too_many());
    }
    Ok(())
}

pub fn record_failure(state: &AppState, key: &str) {
    let now = crate::now();
    let mut guard = state.login_failures.lock().expect("limiter lock");
    guard.entry(key.to_string()).or_default().push(now);
}

pub fn clear_failures(state: &AppState, key: &str) {
    let mut guard = state.login_failures.lock().expect("limiter lock");
    guard.remove(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(verify_password(&hash, "correct horse battery"));
        assert!(!verify_password(&hash, "wrong"));
    }

    #[test]
    fn tokens_are_unique_and_hashed() {
        let (a, ha) = new_token();
        let (b, hb) = new_token();
        assert_ne!(a, b);
        assert_ne!(ha, hb);
        assert_eq!(ha, sha256_hex(a.as_bytes()));
    }
}
