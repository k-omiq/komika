use std::str::FromStr;
use std::time::Duration;

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

/// Open (creating if needed) the SQLite database and run migrations.
///
/// WAL journaling is required for Litestream continuous backup (it ships the WAL
/// to object storage) and lets the background scan scheduler write without
/// blocking readers; `synchronous = NORMAL` is the WAL-safe durability level, and
/// a busy timeout absorbs the brief writer contention from scanner ticks.
pub async fn init(database_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
