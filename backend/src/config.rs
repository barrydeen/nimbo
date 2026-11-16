use std::{env, net::SocketAddr, path::PathBuf};

const DEFAULT_BIND: &str = "127.0.0.1:8080";
const SECONDS_PER_DAY: i64 = 86_400;
const BYTES_PER_MIB: u64 = 1024 * 1024;

const MIN_SESSION_TTL_DAYS: i64 = 1;
const MAX_SESSION_TTL_DAYS: i64 = 3650;
const MIN_UPLOAD_MB: u64 = 1;
const MAX_UPLOAD_MB: u64 = 10_240;
const MIN_AUDIT_RETENTION_DAYS: i64 = 1;
const MAX_AUDIT_RETENTION_DAYS: i64 = 3650;
const MIN_SETUP_TOKEN_BYTES: usize = 16;
const MAX_SETUP_TOKEN_BYTES: usize = 512;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub static_dir: Option<PathBuf>,
    pub secure_cookies: bool,
    pub session_ttl_secs: i64,
    pub max_upload_bytes: u64,
    /// Optional one-time bootstrap secret required by `POST /api/setup`.
    pub setup_token: Option<String>,
    /// How long security audit rows are retained.
    pub audit_retention_days: i64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env_optional("NIMBO_BIND")?.unwrap_or_else(|| DEFAULT_BIND.to_string());
        validate_bind(&bind)?;

        let data_dir =
            PathBuf::from(env_optional("NIMBO_DATA_DIR")?.unwrap_or_else(|| "./data".into()));
        if data_dir.as_os_str().is_empty() {
            anyhow::bail!("NIMBO_DATA_DIR may not be empty");
        }

        let static_dir = env_optional("NIMBO_STATIC_DIR")?
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);

        let secure_cookies = match env_optional("NIMBO_SECURE_COOKIES")? {
            Some(value) => parse_strict_bool("NIMBO_SECURE_COOKIES", &value)?,
            None => false,
        };

        let ttl_days = match env_optional("NIMBO_SESSION_TTL_DAYS")? {
            Some(value) => parse_bounded_i64(
                "NIMBO_SESSION_TTL_DAYS",
                &value,
                MIN_SESSION_TTL_DAYS,
                MAX_SESSION_TTL_DAYS,
            )?,
            None => 30,
        };
        let session_ttl_secs = ttl_days
            .checked_mul(SECONDS_PER_DAY)
            .ok_or_else(|| anyhow::anyhow!("NIMBO_SESSION_TTL_DAYS is too large"))?;

        let max_upload_mb = match env_optional("NIMBO_MAX_UPLOAD_MB")? {
            Some(value) => {
                parse_bounded_u64("NIMBO_MAX_UPLOAD_MB", &value, MIN_UPLOAD_MB, MAX_UPLOAD_MB)?
            }
            None => 200,
        };
        let max_upload_bytes = max_upload_mb
            .checked_mul(BYTES_PER_MIB)
            .ok_or_else(|| anyhow::anyhow!("NIMBO_MAX_UPLOAD_MB is too large"))?;

        let audit_retention_days = match env_optional("NIMBO_AUDIT_RETENTION_DAYS")? {
            Some(value) => parse_bounded_i64(
                "NIMBO_AUDIT_RETENTION_DAYS",
                &value,
                MIN_AUDIT_RETENTION_DAYS,
                MAX_AUDIT_RETENTION_DAYS,
            )?,
            None => 90,
        };

        let setup_token = match env_optional("NIMBO_SETUP_TOKEN")? {
            Some(token) => {
                validate_setup_token(&token)?;
                Some(token)
            }
            None => None,
        };

        Ok(Self {
            bind,
            data_dir,
            static_dir,
            secure_cookies,
            session_ttl_secs,
            max_upload_bytes,
            setup_token,
            audit_retention_days,
        })
    }

    /// Body limit for the multipart upload and replacement routes: the configured
    /// per-file maximum plus one MiB of multipart framing overhead.
    pub fn upload_request_limit(&self) -> anyhow::Result<usize> {
        let base = usize::try_from(self.max_upload_bytes)
            .map_err(|_| anyhow::anyhow!("NIMBO_MAX_UPLOAD_MB does not fit this platform"))?;
        base.checked_add(BYTES_PER_MIB as usize)
            .ok_or_else(|| anyhow::anyhow!("upload body limit overflow"))
    }

    /// True when the listener is reachable beyond the local host while session
    /// cookies are not marked `Secure`; startup warns about this combination.
    pub fn non_loopback_plain_http(&self) -> bool {
        !bind_is_loopback(&self.bind) && !self.secure_cookies
    }

    pub fn vault_dir(&self) -> PathBuf {
        self.data_dir.join("vault")
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.files_dir().join("blobs")
    }

    pub fn upload_tmp_dir(&self) -> PathBuf {
        self.files_dir().join("tmp")
    }
}

fn env_optional(name: &str) -> anyhow::Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{name} must be valid UTF-8")
        }
    }
}

/// Strictly parses the boolean spellings the deployment documentation allows.
/// Unknown values are configuration errors instead of silently becoming `false`.
pub fn parse_strict_bool(name: &str, value: &str) -> anyhow::Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => anyhow::bail!("{name} must be one of true/false, 1/0, yes/no (got {value:?})"),
    }
}

fn parse_bounded_i64(name: &str, value: &str, min: i64, max: i64) -> anyhow::Result<i64> {
    let parsed = value
        .trim()
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("{name} must be an integer (got {value:?})"))?;
    if !(min..=max).contains(&parsed) {
        anyhow::bail!("{name} must be between {min} and {max} (got {parsed})");
    }
    Ok(parsed)
}

fn parse_bounded_u64(name: &str, value: &str, min: u64, max: u64) -> anyhow::Result<u64> {
    let parsed = value
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("{name} must be a non-negative integer (got {value:?})"))?;
    if !(min..=max).contains(&parsed) {
        anyhow::bail!("{name} must be between {min} and {max} (got {parsed})");
    }
    Ok(parsed)
}

pub fn validate_bind(value: &str) -> anyhow::Result<()> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("NIMBO_BIND may not be empty");
    }
    if value.parse::<SocketAddr>().is_ok() {
        return Ok(());
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        let host_ok = host.eq_ignore_ascii_case("localhost")
            || (host.starts_with('[') && host.ends_with(']'));
        if host_ok && port.parse::<u16>().is_ok() {
            return Ok(());
        }
    }
    anyhow::bail!(
        "NIMBO_BIND must be a host:port socket address such as 127.0.0.1:8080 (got {value:?})"
    )
}

pub fn bind_is_loopback(value: &str) -> bool {
    let value = value.trim();
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return addr.ip().is_loopback();
    }
    value
        .rsplit_once(':')
        .map(|(host, _)| host.eq_ignore_ascii_case("localhost") || host == "[::1]")
        .unwrap_or(false)
}

fn validate_setup_token(token: &str) -> anyhow::Result<()> {
    if token.len() < MIN_SETUP_TOKEN_BYTES {
        anyhow::bail!(
            "NIMBO_SETUP_TOKEN must be at least {MIN_SETUP_TOKEN_BYTES} bytes; use a random secret"
        );
    }
    if token.len() > MAX_SETUP_TOKEN_BYTES {
        anyhow::bail!("NIMBO_SETUP_TOKEN must be at most {MAX_SETUP_TOKEN_BYTES} bytes");
    }
    if token.chars().any(char::is_control) {
        anyhow::bail!("NIMBO_SETUP_TOKEN may not contain control characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_booleans_reject_typos() {
        assert!(parse_strict_bool("X", "true").unwrap());
        assert!(parse_strict_bool("X", "YES").unwrap());
        assert!(!parse_strict_bool("X", "0").unwrap());
        assert!(parse_strict_bool("X", "yes please").is_err());
        assert!(parse_strict_bool("X", "on").is_err());
        assert!(parse_strict_bool("X", "").is_err());
    }

    #[test]
    fn binds_are_validated_and_loopback_detected() {
        assert!(validate_bind("127.0.0.1:8080").is_ok());
        assert!(validate_bind("0.0.0.0:8080").is_ok());
        assert!(validate_bind("localhost:8080").is_ok());
        assert!(validate_bind("[::1]:8080").is_ok());
        assert!(validate_bind("nonsense").is_err());
        assert!(validate_bind("").is_err());
        assert!(bind_is_loopback("127.0.0.1:8080"));
        assert!(bind_is_loopback("localhost:8080"));
        assert!(bind_is_loopback("[::1]:8080"));
        assert!(!bind_is_loopback("0.0.0.0:8080"));
        assert!(!bind_is_loopback("192.168.1.4:8080"));
    }

    #[test]
    fn bounded_numbers_reject_zero_and_overflow() {
        assert!(parse_bounded_i64("X", "0", 1, 10).is_err());
        assert!(parse_bounded_i64("X", "11", 1, 10).is_err());
        assert_eq!(parse_bounded_i64("X", "7", 1, 10).unwrap(), 7);
        assert!(parse_bounded_u64("X", "-1", 1, 10).is_err());
        assert!(parse_bounded_u64("X", "99999999999999999999", 1, 10).is_err());
    }

    #[test]
    fn setup_tokens_require_entropy_and_bounds() {
        assert!(validate_setup_token("short").is_err());
        assert!(validate_setup_token("0123456789abcdef").is_ok());
        assert!(validate_setup_token(&"a".repeat(MAX_SETUP_TOKEN_BYTES + 1)).is_err());
        assert!(validate_setup_token("0123456789abcde\n").is_err());
    }
}
