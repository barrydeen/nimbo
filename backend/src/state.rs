use crate::{auth::LoginLimiter, config::Config};
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Config,
    /// Instance-wide secret used to encrypt sensitive settings (e.g. API keys).
    #[allow(dead_code)]
    pub secret: [u8; 32],
    /// Bounded, evicting, atomically-reserved login attempt limiter.
    pub login_limiter: Mutex<LoginLimiter>,
    /// Caps the number of concurrent (expensive) Argon2 operations.
    pub password_semaphore: Arc<Semaphore>,
    /// Hash of a random throwaway password, verified for unknown usernames so
    /// user enumeration does not get a cheap timing signal.
    pub dummy_password_hash: String,
}
