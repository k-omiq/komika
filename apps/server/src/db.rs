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

/// Open (creating if needed) the SEPARATE cover-cache DB and ensure its schema.
///
/// This DB holds only `work_cover_blob` (large, re-derivable-from-MangaDex cover
/// thumbnails). It is deliberately NOT the main DB: Litestream replicates only the
/// main DB to R2, so keeping covers here excludes them from backup while they're
/// still served from our own origin. Schema is a single table, created inline (no
/// migration directory) — there is no FK to `work` because that table lives in the
/// other DB; an orphaned blob (its work deleted) is harmless and re-derivable.
pub async fn init_covers(database_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS work_cover_blob (\
             work_id    TEXT PRIMARY KEY,\
             webp       BLOB NOT NULL,\
             version    INTEGER NOT NULL,\
             updated_at TEXT NOT NULL\
         )",
    )
    .execute(&pool)
    .await?;
    Ok(pool)
}
