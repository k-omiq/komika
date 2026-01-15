//! Adaptive scan scheduler.
//!
//! A background tokio task that keeps the federated library catalog fresh. Every
//! `SCAN_TICK_SECONDS` it walks the Suwayomi library and, for each series, decides
//! whether the series is *overdue* for a re-scan based on its adaptive cadence:
//!
//!   effective_interval = admin override_interval_hours  (if set)
//!                        else rolling avg gap between chapter uploads
//!
//! Overdue series get their chapters re-fetched; new chapters (detected by chapter
//! count / latest number vs. the last known count) are logged. All derived state is
//! persisted in `series_scan_state`, which `graphql::map_series` folds back into
//! `Series.scan` for the client and admin console.
//!
//! Resilience: a per-series error is logged and skipped — it never aborts the tick
//! or the loop. The loop exits cleanly when the provided shutdown signal fires.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio::time::{interval, MissedTickBehavior};

use crate::graphql::types::{komika_status, paused_for_status, status_from, SeriesStatus};
use crate::graphql::AppState;
use crate::suwayomi::{SuwayomiChapter, SuwayomiManga};

/// Fallback cadence when no interval can be inferred yet (e.g. a series with
/// fewer than two dated chapters). Without this, `effective_interval` stays 0.0
/// and `is_overdue` is always true, re-fetching the series on every single tick.
const DEFAULT_INTERVAL_HOURS: f64 = 24.0;

/// Upper bound on any effective interval, so an absurd admin override can't
/// overflow the `chrono::Duration` math when computing `next_scan_at`.
const MAX_INTERVAL_HOURS: f64 = 100.0 * 365.0 * 24.0; // ~100 years

/// Persisted scan state row (mirrors `series_scan_state`).
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct ScanState {
    pub avg_interval_hours: f64,
    pub known_chapter_count: i64,
    pub last_scanned_at: Option<String>,
    pub next_scan_at: Option<String>,
    pub last_new_chapter_at: Option<String>,
}

/// Read the persisted scan state for a series, if any.
pub async fn scan_state(pool: &SqlitePool, series_id: &str) -> Option<ScanState> {
    sqlx::query_as::<_, ScanState>(
        "SELECT avg_interval_hours, known_chapter_count, last_scanned_at, next_scan_at, \
         last_new_chapter_at FROM series_scan_state WHERE series_id = ?",
    )
    .bind(series_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

/// Admin overrides the scanner cares about (interval + pause), read from `series_admin`.
#[derive(Default, sqlx::FromRow)]
struct ScanAdmin {
    override_interval_hours: Option<f64>,
    paused_override: Option<i64>,
    status_override: Option<String>,
}

async fn scan_admin(pool: &SqlitePool, series_id: &str) -> ScanAdmin {
    sqlx::query_as::<_, ScanAdmin>(
        "SELECT override_interval_hours, paused_override, status_override \
         FROM series_admin WHERE series_id = ?",
    )
    .bind(series_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

/// Coerce a Suwayomi epoch timestamp (seconds or millis, as a string) to millis.
/// Mirrors the guard logic in `types::to_iso`.
fn epoch_millis(v: Option<&str>) -> Option<i64> {
    let n: i64 = v?.trim().parse().ok()?;
    if n <= 0 {
        return None;
    }
    Some(if n > 1_000_000_000_000 { n } else { n * 1000 })
}

/// Compute the rolling average interval (hours) between chapter uploads.
///
/// Sorts upload timestamps descending, diffs consecutive pairs, and averages them.
/// Garbage/missing timestamps are dropped. Returns `None` when fewer than two usable
/// timestamps exist (no cadence can be inferred).
pub fn avg_interval_hours(chapters: &[SuwayomiChapter]) -> Option<f64> {
    let mut ts: Vec<i64> = chapters
        .iter()
        .filter_map(|c| epoch_millis(c.upload_date.as_deref()))
        .collect();
    if ts.len() < 2 {
        return None;
    }
    ts.sort_unstable_by(|a, b| b.cmp(a)); // desc
    let mut total_ms: i64 = 0;
    let mut gaps: i64 = 0;
    for pair in ts.windows(2) {
        let diff = pair[0] - pair[1]; // newer - older >= 0
        if diff > 0 {
            total_ms += diff;
            gaps += 1;
        }
    }
    if gaps == 0 {
        return None;
    }
    let avg_ms = total_ms as f64 / gaps as f64;
    Some(avg_ms / 3_600_000.0)
}

/// The latest (highest) chapter number seen, for logging new-chapter detection.
fn latest_number(chapters: &[SuwayomiChapter]) -> f64 {
    chapters
        .iter()
        .map(|c| c.chapter_number)
        .fold(f64::MIN, f64::max)
}

fn parse_iso(v: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(v?)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Decide whether `now - last_scanned_at >= effective_interval`. A series that has
/// never been scanned (no `last_scanned_at`) is always due.
fn is_overdue(
    last_scanned_at: Option<&str>,
    effective_interval_hours: f64,
    now: DateTime<Utc>,
) -> bool {
    match parse_iso(last_scanned_at) {
        None => true,
        Some(last) => {
            let elapsed_hours = (now - last).num_seconds() as f64 / 3600.0;
            elapsed_hours >= effective_interval_hours
        }
    }
}

/// Resolve the effective status for a series, honoring an admin status override.
fn effective_status(m: &SuwayomiManga, admin: &ScanAdmin) -> SeriesStatus {
    admin
        .status_override
        .as_deref()
        .and_then(komika_status)
        .unwrap_or_else(|| status_from(&m.status))
}

/// Resolve whether the series is paused: forced admin override wins, else auto by status.
fn is_paused(status: SeriesStatus, admin: &ScanAdmin) -> bool {
    admin
        .paused_override
        .map(|v| v != 0)
        .unwrap_or_else(|| paused_for_status(status))
}

/// Run one scan tick over the whole library. Returns `(library_size, overdue_seen)`
/// for aggregate health reporting. Per-series errors are logged and skipped.
async fn tick(state: &AppState) -> (usize, usize) {
    let library = match state.suwayomi.library().await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "scan tick: failed to list Suwayomi library");
            return (0, 0);
        }
    };
    let library_size = library.len();
    let now = Utc::now();
    let mut overdue_seen = 0usize;

    for m in library {
        let series_id = m.id.to_string();
        let admin = scan_admin(&state.pool, &series_id).await;
        let status = effective_status(&m, &admin);
        if is_paused(status, &admin) {
            continue;
        }

        let prior = scan_state(&state.pool, &series_id)
            .await
            .unwrap_or_default();

        // Effective interval: admin override wins; else the last-computed rolling
        // avg; else a sane default. A brand-new series (no last_scanned_at) is
        // always overdue and scanned promptly; the default only matters once it
        // has been scanned but still yields no inferable cadence, so it isn't
        // re-fetched every tick.
        let effective_interval = admin
            .override_interval_hours
            .filter(|v| *v > 0.0)
            .unwrap_or(prior.avg_interval_hours);
        let effective_interval = if effective_interval > 0.0 {
            effective_interval
        } else {
            DEFAULT_INTERVAL_HOURS
        };

        if !is_overdue(prior.last_scanned_at.as_deref(), effective_interval, now) {
            continue;
        }
        overdue_seen += 1;

        if let Err(e) = scan_series(state, &m, now).await {
            tracing::warn!(series_id, error = %e, "scan: series scan failed; skipping");
        }
    }

    (library_size, overdue_seen)
}

/// Re-fetch one series' chapters, detect new ones, and persist its scan state
/// (rolling avg, chapter count, `last_scanned_at`, next `next_scan_at`). Returns
/// whether new chapters were found.
///
/// Shared by the scheduler `tick` (which gates on pause/overdue first) and the
/// admin `triggerScan` mutation (which forces a scan regardless of gating). It
/// re-reads the admin override and prior state so it's self-contained for both
/// callers.
pub async fn scan_series(
    state: &AppState,
    m: &SuwayomiManga,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let series_id = m.id.to_string();
    let admin = scan_admin(&state.pool, &series_id).await;
    let prior = scan_state(&state.pool, &series_id)
        .await
        .unwrap_or_default();

    let chapters = state.suwayomi.chapters(m.id).await?;
    let count = chapters.len() as i64;
    let computed_avg = avg_interval_hours(&chapters).unwrap_or(prior.avg_interval_hours);
    let new_found = count > prior.known_chapter_count;
    let now_iso = now.to_rfc3339();
    // Recompute the *next* effective interval from fresh data (admin still wins).
    let next_interval = admin
        .override_interval_hours
        .filter(|v| *v > 0.0)
        .unwrap_or(computed_avg);
    // Same default when no cadence is known, and clamp so an absurd override
    // can't overflow the Duration math below.
    let next_interval = if next_interval > 0.0 {
        next_interval
    } else {
        DEFAULT_INTERVAL_HOURS
    }
    .min(MAX_INTERVAL_HOURS);
    let next_scan_at =
        (now + chrono::Duration::milliseconds((next_interval * 3_600_000.0) as i64)).to_rfc3339();
    let last_new_chapter_at = if new_found {
        Some(now_iso.clone())
    } else {
        prior.last_new_chapter_at.clone()
    };

    if new_found {
        tracing::info!(
            series_id,
            title = %m.title,
            added = count - prior.known_chapter_count,
            total = count,
            latest = latest_number(&chapters),
            avg_interval_hours = computed_avg,
            "scan: new chapters detected"
        );
    } else {
        tracing::debug!(
            series_id,
            title = %m.title,
            total = count,
            avg_interval_hours = computed_avg,
            "scan: no new chapters"
        );
    }

    sqlx::query(
        "INSERT INTO series_scan_state \
           (series_id, avg_interval_hours, known_chapter_count, last_scanned_at, \
            next_scan_at, last_new_chapter_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(series_id) DO UPDATE SET \
           avg_interval_hours = excluded.avg_interval_hours, \
           known_chapter_count = excluded.known_chapter_count, \
           last_scanned_at = excluded.last_scanned_at, \
           next_scan_at = excluded.next_scan_at, \
           last_new_chapter_at = excluded.last_new_chapter_at, \
           updated_at = excluded.updated_at",
    )
    .bind(&series_id)
    .bind(computed_avg)
    .bind(count)
    .bind(&now_iso)
    .bind(&next_scan_at)
    .bind(&last_new_chapter_at)
    .bind(&now_iso)
    .execute(&state.pool)
    .await?;

    Ok(new_found)
}

/// Spawn the scan scheduler loop. Runs until `shutdown` resolves.
pub fn spawn(
    state: Arc<AppState>,
    tick_seconds: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(tick_seconds));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        tracing::info!(tick_seconds, "scan scheduler started");
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let started = Utc::now();
                    let (size, overdue) = tick(&state).await;
                    {
                        let mut h = state.scan_health.lock().unwrap();
                        h.library_size = size;
                        h.overdue_count = overdue;
                        h.last_tick_at = Some(started.to_rfc3339());
                    }
                    tracing::info!(library_size = size, overdue = overdue, "scan tick complete");
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("scan scheduler stopping");
                        break;
                    }
                }
            }
        }
    });
}
