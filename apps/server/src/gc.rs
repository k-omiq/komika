//! Background garbage collection of orphaned staged uploads.
//!
//! `POST /comment-media` inserts a `comment_media` row with `comment_id = NULL`
//! (a staged draft owned by the uploader); `postComment` later links it to the new
//! comment. A draft the user never posts stays NULL forever. Migration 0023 always
//! intended these to be "garbage-collected by age" — this task is that GC. It runs
//! hourly and deletes staged rows older than [`STAGED_TTL_HOURS`], bounding
//! unbounded SQLite growth (and the Litestream → R2 replication it drives).

use std::time::Duration;

use sqlx::SqlitePool;

/// Staged (unlinked) uploads older than this are swept. A user posting a comment
/// always links its image within seconds, so a day is a generous grace period for
/// slow multi-step composes without letting abandoned drafts accumulate.
const STAGED_TTL_HOURS: i64 = 24;

/// How often the sweep runs.
const SWEEP_INTERVAL_SECS: u64 = 3600;

/// Spawn the recurring GC sweep. Runs until `shutdown` resolves.
pub fn spawn(pool: SqlitePool, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!("comment-media GC sweep started");
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    sweep(&pool).await;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("comment-media GC sweep stopping");
                        break;
                    }
                }
            }
        }
    });
}

/// Delete staged (`comment_id IS NULL`) comment-media rows older than the TTL.
/// Best-effort: any error is logged and swallowed so a transient DB blip doesn't
/// kill the sweep loop.
async fn sweep(pool: &SqlitePool) {
    // created_at is written as `chrono::Utc::now().to_rfc3339()`, so a UTC RFC-3339
    // cutoff compares correctly against it lexically.
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(STAGED_TTL_HOURS)).to_rfc3339();
    match sqlx::query("DELETE FROM comment_media WHERE comment_id IS NULL AND created_at < ?")
        .bind(&cutoff)
        .execute(pool)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::info!(deleted = r.rows_affected(), "gc: swept orphaned comment media");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "gc: comment-media sweep failed"),
    }

    // Prune view buckets past the retention window so `series_view_bucket` stays bounded
    // (the all-time totals in `series_views` are kept). Best-effort like the sweep above.
    match crate::views::prune_buckets(pool).await {
        Ok(n) if n > 0 => tracing::info!(deleted = n, "gc: pruned old view buckets"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "gc: view-bucket prune failed"),
    }
}
