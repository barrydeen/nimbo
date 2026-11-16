use crate::config::Config;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Mutex;

pub struct AppState {
    pub pool: SqlitePool,
    pub cfg: Config,
    /// Instance-wide secret used to encrypt sensitive settings (e.g. API keys).
    #[allow(dead_code)]
    pub secret: [u8; 32],
    /// Failed login attempts keyed by username, used for basic rate limiting.
    pub login_failures: Mutex<HashMap<String, Vec<i64>>>,
}
