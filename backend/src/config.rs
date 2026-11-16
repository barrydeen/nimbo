use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub static_dir: Option<PathBuf>,
    pub secure_cookies: bool,
    pub session_ttl_secs: i64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env::var("NIMBO_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let data_dir =
            PathBuf::from(env::var("NIMBO_DATA_DIR").unwrap_or_else(|_| "./data".into()));
        let static_dir = env::var("NIMBO_STATIC_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let secure_cookies = env::var("NIMBO_SECURE_COOKIES")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let ttl_days = env::var("NIMBO_SESSION_TTL_DAYS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(30);
        Ok(Self {
            bind,
            data_dir,
            static_dir,
            secure_cookies,
            session_ttl_secs: ttl_days * 86_400,
        })
    }

    pub fn vault_dir(&self) -> PathBuf {
        self.data_dir.join("vault")
    }
}
