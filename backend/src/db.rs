use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;
use std::time::Duration;

pub async fn init(data_dir: &Path) -> Result<SqlitePool> {
    std::fs::create_dir_all(data_dir.join("vault"))?;
    let db_path = data_dir.join("nimbo.db");
    let opts = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(pool)
}

pub async fn ensure_settings(pool: &SqlitePool) -> Result<()> {
    sqlx::query("INSERT OR IGNORE INTO settings (key, value) VALUES ('setup_complete', 'false')")
        .execute(pool)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO settings (key, value) VALUES ('instance_name', 'Nimbo')")
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.0))
}

#[allow(dead_code)]
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

/// Startup/periodic maintenance: expired sessions and old audit rows have no
/// operational value and otherwise grow forever on a long-lived instance.
pub async fn retention_cleanup(pool: &SqlitePool, audit_retention_days: i64) -> Result<()> {
    let now = crate::now();
    let removed_sessions = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(now)
        .execute(pool)
        .await?
        .rows_affected();

    let days = audit_retention_days.clamp(1, 3650);
    let cutoff = now.saturating_sub(days.saturating_mul(86_400));
    let removed_audit = sqlx::query("DELETE FROM audit_log WHERE at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected();

    if removed_sessions > 0 || removed_audit > 0 {
        tracing::info!(
            sessions = removed_sessions,
            audit_rows = removed_audit,
            "retention cleanup removed stale rows"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retention_removes_expired_sessions_and_old_audit_rows() {
        let dir = std::env::temp_dir().join(format!("nimbo-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = init(&dir).await.unwrap();
        let now = crate::now();

        sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at) \
             VALUES ('u1', 'alice', 'hash', 'admin', ?)",
        )
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, user_id, token_hash, created_at, expires_at) \
             VALUES ('s1', 'u1', 'expired', ?, ?), ('s2', 'u1', 'live', ?, ?)",
        )
        .bind(now - 100)
        .bind(now - 1)
        .bind(now)
        .bind(now + 3600)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO audit_log (user_id, action, target, at) VALUES \
             ('u1', 'old', NULL, ?), ('u1', 'new', NULL, ?)",
        )
        .bind(now - 200 * 86_400)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();

        retention_cleanup(&pool, 90).await.unwrap();

        let sessions: Vec<(String,)> = sqlx::query_as("SELECT id FROM sessions")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, vec![("s2".to_string(),)]);
        let audit: Vec<(String,)> = sqlx::query_as("SELECT action FROM audit_log")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(audit, vec![("new".to_string(),)]);
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
