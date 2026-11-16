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
