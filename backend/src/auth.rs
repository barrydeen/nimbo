use crate::{error::ApiError, state::AppState, util::sha256_hex};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method},
    middleware::Next,
    response::Response,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

pub const COOKIE_NAME: &str = "nimbo_session";
pub const DEVICE_TOKEN_PREFIX: &str = "nmb_";

/// Largest username accepted by the login route. Applied before any allocation
/// or limiter key derivation.
pub const MAX_LOGIN_USERNAME_BYTES: usize = 64;
/// Argon2 cost grows with input length; cap it well above real passphrases.
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Concurrent Argon2 operations allowed per process.
pub const PASSWORD_HASH_CONCURRENCY: usize = 2;

const ACCOUNT_MAX_ATTEMPTS: usize = 5;
const ACCOUNT_WINDOW_SECS: i64 = 300;
/// After the account window is exhausted, a correct password may still be
/// verified at most once per this cooldown, so lockout is temporary without
/// turning the locked window into an unlimited guessing oracle.
const ACCOUNT_BYPASS_COOLDOWN_SECS: i64 = 30;
const SOURCE_MAX_ATTEMPTS: usize = 20;
const SOURCE_WINDOW_SECS: i64 = 300;
const GLOBAL_MAX_ATTEMPTS: usize = 200;
const GLOBAL_WINDOW_SECS: i64 = 300;
const MAX_LIMITER_KEYS: usize = 4096;
const PASSWORD_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_TOKEN_TOUCH_SECS: i64 = 60;

#[derive(Clone, Debug, serde::Serialize)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: String,
    /// `full` for browser sessions and ordinary device tokens, `backup` for a
    /// token restricted to its `Phone Backups` subtree.
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_root_id: Option<String>,
}

impl AuthUser {
    pub fn is_backup_scope(&self) -> bool {
        self.scope == "backup"
    }
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

/// Runs Argon2 hashing on a blocking thread, bounded by the process-wide
/// semaphore so unauthenticated requests cannot occupy all Tokio workers.
pub async fn hash_password_bounded(state: &AppState, password: &str) -> Result<String, ApiError> {
    let permit = acquire_password_permit(state).await?;
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&password)
    })
    .await
    .map_err(|err| {
        tracing::error!(error = %err, "password hashing task failed");
        ApiError::internal("password hashing failed")
    })?
}

/// Constant-time-enough password check on a blocking thread. Returns an error
/// only when the bounded work queue is unavailable, never password details.
pub async fn verify_password_bounded(
    state: &AppState,
    hash: &str,
    password: &str,
) -> Result<bool, ApiError> {
    let permit = acquire_password_permit(state).await?;
    let hash = hash.to_string();
    let password = password.to_string();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(&hash, &password)
    })
    .await
    .map_err(|err| {
        tracing::error!(error = %err, "password verification task failed");
        ApiError::internal("password verification failed")
    })
}

async fn acquire_password_permit(
    state: &AppState,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    tokio::time::timeout(
        PASSWORD_OPERATION_TIMEOUT,
        state.password_semaphore.clone().acquire_owned(),
    )
    .await
    .map_err(|_| ApiError::service_unavailable("password work is busy, try again shortly"))?
    .map_err(|_| ApiError::service_unavailable("password work is unavailable"))
}

pub fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < 8 {
        return Err(ApiError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::bad_request(format!(
            "password must be at most {MAX_PASSWORD_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Trims and bounds a login username before it reaches any limiter allocation.
pub fn normalize_login_username(raw: &str) -> Result<String, ApiError> {
    let username = raw.trim();
    if username.is_empty() {
        return Err(ApiError::bad_request("username is required"));
    }
    if username.len() > MAX_LOGIN_USERNAME_BYTES {
        return Err(ApiError::bad_request(format!(
            "username must be at most {MAX_LOGIN_USERNAME_BYTES} bytes"
        )));
    }
    if username.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "username contains control characters",
        ));
    }
    Ok(username.to_string())
}

/// Fixed-size limiter key so attacker-supplied usernames never become retained
/// map entries. The instance secret keeps keys unguessable across restarts.
pub fn login_account_key(secret: &[u8; 32], username: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update([0u8]);
    hasher.update(username.to_lowercase().as_bytes());
    hex::encode(hasher.finalize())
}

/// Compares two strings without an early exit on the first differing byte.
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = Sha256::digest(left.as_bytes());
    let right = Sha256::digest(right.as_bytes());
    left.iter()
        .zip(right.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginDecision {
    /// Attempt reserved; run the normal credential check.
    Reserved,
    /// The per-account window is exhausted but its short cooldown has elapsed.
    /// One verification is permitted (bounded by source/global budgets) so a
    /// correct password is never permanently locked out by username-only
    /// failures.
    VerifyDespiteAccountLimit,
    /// Source/global budget or account cooldown exhausted; reject without
    /// hashing.
    Limited,
}

/// Bounded login attempt limiter. Attempts are reserved atomically under one
/// lock; stale entries and whole keys are evicted so memory use is capped.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    accounts: HashMap<String, VecDeque<i64>>,
    sources: HashMap<String, VecDeque<i64>>,
    global: VecDeque<i64>,
}

impl LoginLimiter {
    pub fn begin(&mut self, account_key: &str, source_key: &str) -> LoginDecision {
        self.begin_at(crate::now(), account_key, source_key)
    }

    fn begin_at(&mut self, now: i64, account_key: &str, source_key: &str) -> LoginDecision {
        prune(&mut self.global, now, GLOBAL_WINDOW_SECS);
        prune_map(&mut self.accounts, now, ACCOUNT_WINDOW_SECS);
        prune_map(&mut self.sources, now, SOURCE_WINDOW_SECS);

        let source_attempts = self.sources.get(source_key).map_or(0, VecDeque::len);
        if self.global.len() >= GLOBAL_MAX_ATTEMPTS || source_attempts >= SOURCE_MAX_ATTEMPTS {
            return LoginDecision::Limited;
        }

        let account_attempts = self.accounts.get(account_key).map_or(0, VecDeque::len);
        if account_attempts >= ACCOUNT_MAX_ATTEMPTS {
            let last = self
                .accounts
                .get(account_key)
                .and_then(|queue| queue.back())
                .copied()
                .unwrap_or(now);
            if now - last < ACCOUNT_BYPASS_COOLDOWN_SECS {
                return LoginDecision::Limited;
            }
            // One cooldown-gated verification; it counts as a further attempt so
            // concurrent requests cannot all take the bypass.
            push_attempt(&mut self.accounts, account_key, now, MAX_LIMITER_KEYS);
            push_attempt(&mut self.sources, source_key, now, MAX_LIMITER_KEYS);
            self.global.push_back(now);
            return LoginDecision::VerifyDespiteAccountLimit;
        }

        // Reserve the attempt before any database or Argon2 work. This is what
        // makes concurrent bursts unable to exceed the configured budget.
        push_attempt(&mut self.sources, source_key, now, MAX_LIMITER_KEYS);
        self.global.push_back(now);
        push_attempt(&mut self.accounts, account_key, now, MAX_LIMITER_KEYS);
        LoginDecision::Reserved
    }

    pub fn clear_account(&mut self, account_key: &str) {
        self.accounts.remove(account_key);
    }

    #[cfg(test)]
    fn account_attempts(&self, key: &str) -> usize {
        self.accounts.get(key).map_or(0, VecDeque::len)
    }
}

fn prune(queue: &mut VecDeque<i64>, now: i64, window_secs: i64) {
    while queue
        .front()
        .map(|timestamp| now - *timestamp >= window_secs)
        .unwrap_or(false)
    {
        queue.pop_front();
    }
}

fn prune_map(map: &mut HashMap<String, VecDeque<i64>>, now: i64, window_secs: i64) {
    map.retain(|_, queue| {
        prune(queue, now, window_secs);
        !queue.is_empty()
    });
}

fn push_attempt(map: &mut HashMap<String, VecDeque<i64>>, key: &str, now: i64, max_keys: usize) {
    if !map.contains_key(key) && map.len() >= max_keys {
        let victim = map
            .iter()
            .min_by_key(|(_, queue)| queue.back().copied().unwrap_or(i64::MIN))
            .map(|(key, _)| key.to_string());
        if let Some(victim) = victim {
            map.remove(&victim);
        }
    }
    map.entry(key.to_string()).or_default().push_back(now);
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
    Ok(row.map(|(id, username, role)| AuthUser {
        id,
        username,
        role,
        scope: "full".to_string(),
        backup_root_id: None,
    }))
}

pub async fn delete_session(pool: &sqlx::SqlitePool, token: &str) -> Result<(), ApiError> {
    let token_hash = sha256_hex(token.as_bytes());
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// (token id, user id, username, role, scope, backup root, last used at)
type DeviceTokenLookup = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
);

/// Resolve a device token (`nmb_…`) to its user and scope. `last_used_at` is
/// persisted at most once per minute and never blocks authorization on error.
pub async fn lookup_device_token(
    pool: &sqlx::SqlitePool,
    token: &str,
) -> Result<Option<AuthUser>, ApiError> {
    let token_hash = sha256_hex(token.as_bytes());
    let now = crate::now();
    let row: Option<DeviceTokenLookup> = sqlx::query_as(
        "SELECT t.id, u.id, u.username, u.role, t.scope, t.backup_root_id, t.last_used_at \
         FROM device_tokens t \
         JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = ? AND t.revoked_at IS NULL \
           AND (t.expires_at IS NULL OR t.expires_at > ?)",
    )
    .bind(&token_hash)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    let Some((token_id, id, username, role, scope, backup_root_id, last_used_at)) = row else {
        return Ok(None);
    };
    if scope != "full" && scope != "backup" {
        tracing::warn!(scope = %scope, "device token has an invalid scope");
        return Ok(None);
    }
    if scope == "backup" && backup_root_id.is_none() {
        tracing::warn!("backup device token is not bound to a root folder");
        return Ok(None);
    }

    if last_used_at
        .map(|seen| now - seen >= DEVICE_TOKEN_TOUCH_SECS)
        .unwrap_or(true)
    {
        let touched = sqlx::query(
            "UPDATE device_tokens SET last_used_at = ? WHERE id = ? \
             AND (last_used_at IS NULL OR last_used_at < ?)",
        )
        .bind(now)
        .bind(&token_id)
        .bind(now - DEVICE_TOKEN_TOUCH_SECS)
        .execute(pool)
        .await;
        if let Err(err) = touched {
            // Telemetry is best-effort; a busy database must not reject a valid token.
            tracing::debug!(error = %err, "could not persist device token last-use time");
        }
    }

    Ok(Some(AuthUser {
        id,
        username,
        role,
        scope,
        backup_root_id,
    }))
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

    if user.is_backup_scope() && !backup_scope_allows(req.uri().path()) {
        return Err(ApiError::forbidden(
            "backup tokens may only access file routes and their own account",
        ));
    }

    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

/// Backup-scope tokens are deliberately narrow: they can browse/upload inside
/// their bound subtree and read their own account, but cannot touch documents,
/// the agent, settings, shares, or token administration.
pub fn backup_scope_allows(path: &str) -> bool {
    path == "/api/auth/me"
        || path == "/api/auth/logout"
        || path == "/api/instance"
        || path == "/api/files"
        || path.starts_with("/api/files/")
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

/// Ensures `/api/*` framework failures (body-limit, method, extractor
/// rejections) use the same `{ "error": ... }` JSON shape as ApiError instead of
/// leaking plain-text framework bodies. Internal details are never included.
pub async fn json_error_shape(req: Request, next: Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    let response = next.run(req).await;
    if !is_api {
        return response;
    }
    if !(response.status().is_client_error() || response.status().is_server_error()) {
        return response;
    }
    let already_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("application/json"))
        .unwrap_or(false);
    if already_json {
        return response;
    }

    let status = response.status();
    let body = serde_json::json!({
        "error": status.canonical_reason().unwrap_or("request failed"),
    })
    .to_string();
    let (mut parts, _) = response.into_parts();
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, axum::body::Body::from(body))
}

/// Global browser hardening. Same-origin framing is intentional: the SPA
/// renders same-origin PDF previews in an iframe. External origins remain
/// blocked by both `X-Frame-Options` and CSP `frame-ancestors`.
const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "SAMEORIGIN"),
    ("referrer-policy", "no-referrer"),
    (
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=()",
    ),
    (
        "content-security-policy",
        "default-src 'self'; base-uri 'none'; object-src 'none'; \
         frame-ancestors 'self'; form-action 'self'; \
         img-src 'self' data: blob:; media-src 'self' blob:; \
         style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; \
         connect-src 'self'; font-src 'self' data:; worker-src 'self' blob:; \
         manifest-src 'self'",
    ),
];

/// Safe response headers applied to API and static responses alike. The CSP is
/// a defense-in-depth backstop; stored HTML is sanitized by the frontend.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    for &(name, value) in SECURITY_HEADERS {
        headers
            .entry(HeaderName::from_static(name))
            .or_insert(HeaderValue::from_static(value));
    }
    response
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
    fn password_length_bounds() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("long enough").is_ok());
        assert!(validate_password(&"a".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
    }

    #[test]
    fn tokens_are_unique_and_hashed() {
        let (a, ha) = new_token();
        let (b, hb) = new_token();
        assert_ne!(a, b);
        assert_ne!(ha, hb);
        assert_eq!(ha, sha256_hex(a.as_bytes()));
    }

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }

    #[test]
    fn login_usernames_are_bounded_before_limiting() {
        assert!(normalize_login_username("  alice  ").is_ok());
        assert!(normalize_login_username(&"a".repeat(MAX_LOGIN_USERNAME_BYTES + 1)).is_err());
        assert!(normalize_login_username("ali\nce").is_err());
        assert!(normalize_login_username("   ").is_err());

        let secret = [9u8; 32];
        let key = login_account_key(&secret, "Alice");
        assert_eq!(key.len(), 64);
        assert_eq!(key, login_account_key(&secret, "alice"));
        assert_ne!(key, login_account_key(&secret, "bob"));
        assert_ne!(key, login_account_key(&[8u8; 32], "alice"));
    }

    #[test]
    fn limiter_reserves_atomically_and_clears_on_success() {
        let mut limiter = LoginLimiter::default();
        let key = "account";
        for attempt in 1..=ACCOUNT_MAX_ATTEMPTS {
            assert_eq!(
                limiter.begin_at(1_000, key, "source"),
                LoginDecision::Reserved,
                "attempt {attempt} should be reserved"
            );
        }
        assert_eq!(
            limiter.begin_at(1_000, key, "source"),
            LoginDecision::Limited,
            "immediate retries are rejected without hashing"
        );
        assert_eq!(
            limiter.begin_at(1_000 + ACCOUNT_BYPASS_COOLDOWN_SECS, key, "source"),
            LoginDecision::VerifyDespiteAccountLimit,
            "after the short cooldown a correct password may still be checked"
        );
        assert_eq!(
            limiter.begin_at(1_000 + ACCOUNT_BYPASS_COOLDOWN_SECS, key, "source"),
            LoginDecision::Limited,
            "concurrent bypass attempts cannot all run"
        );
        assert_eq!(limiter.account_attempts(key), ACCOUNT_MAX_ATTEMPTS + 1);

        limiter.clear_account(key);
        assert_eq!(limiter.account_attempts(key), 0);
        assert_eq!(
            limiter.begin_at(1_000 + ACCOUNT_BYPASS_COOLDOWN_SECS, key, "source"),
            LoginDecision::Reserved
        );
    }

    #[test]
    fn limiter_expires_old_attempts() {
        let mut limiter = LoginLimiter::default();
        for _ in 0..ACCOUNT_MAX_ATTEMPTS {
            let _ = limiter.begin_at(1_000, "account", "source");
        }
        assert_eq!(
            limiter.begin_at(1_000 + ACCOUNT_WINDOW_SECS, "account", "source"),
            LoginDecision::Reserved
        );
    }

    #[test]
    fn limiter_has_source_and_global_budgets() {
        let mut limiter = LoginLimiter::default();
        for index in 0..SOURCE_MAX_ATTEMPTS {
            assert_eq!(
                limiter.begin_at(1_000, &format!("account-{index}"), "one-source"),
                LoginDecision::Reserved
            );
        }
        assert_eq!(
            limiter.begin_at(1_000, "fresh-account", "one-source"),
            LoginDecision::Limited
        );
        // A different source still has budget.
        assert_eq!(
            limiter.begin_at(1_000, "fresh-account", "other-source"),
            LoginDecision::Reserved
        );
    }

    #[test]
    fn limiter_maps_evict_when_capped() {
        let mut map: HashMap<String, VecDeque<i64>> = HashMap::new();
        push_attempt(&mut map, "a", 10, 2);
        push_attempt(&mut map, "b", 20, 2);
        push_attempt(&mut map, "c", 30, 2);
        assert_eq!(map.len(), 2);
        assert!(!map.contains_key("a"));
        assert!(map.contains_key("b"));
        assert!(map.contains_key("c"));
    }

    #[test]
    fn framing_policy_allows_only_same_origin() {
        let mut frame = None;
        let mut csp = None;
        for &(name, value) in SECURITY_HEADERS {
            assert!(
                HeaderName::from_bytes(name.as_bytes()).is_ok(),
                "invalid security header name {name}"
            );
            assert!(
                HeaderValue::from_bytes(value.as_bytes()).is_ok(),
                "invalid security header value for {name}"
            );
            match name {
                "x-frame-options" => frame = Some(value),
                "content-security-policy" => csp = Some(value),
                _ => {}
            }
        }

        assert_eq!(frame, Some("SAMEORIGIN"));
        let csp = csp.expect("content-security-policy header is missing");
        assert!(
            csp.contains("frame-ancestors 'self'"),
            "same-origin PDF previews must remain frameable"
        );
        assert!(!csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("frame-ancestors *"));
        assert!(csp.contains("default-src 'self'"));
    }

    #[test]
    fn backup_scope_is_narrow() {
        assert!(backup_scope_allows("/api/files"));
        assert!(backup_scope_allows("/api/files/upload"));
        assert!(backup_scope_allows("/api/files/abc/content"));
        assert!(backup_scope_allows("/api/auth/me"));
        assert!(backup_scope_allows("/api/auth/logout"));
        assert!(backup_scope_allows("/api/instance"));
        assert!(!backup_scope_allows("/api/docs/content"));
        assert!(!backup_scope_allows("/api/search"));
        assert!(!backup_scope_allows("/api/agent/chat"));
        assert!(!backup_scope_allows("/api/settings/llm"));
        assert!(!backup_scope_allows("/api/shares"));
        assert!(!backup_scope_allows("/api/tokens"));
        assert!(!backup_scope_allows("/api/filesevil"));
    }
}
