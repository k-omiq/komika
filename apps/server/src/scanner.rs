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
    /// Highest chapter number seen at the last scan. `None` = not yet observed
    /// (pre-`0012` rows), which suppresses number-based new-chapter detection
    /// until a baseline is recorded (SC4).
    pub known_max_chapter: Option<f64>,
    pub last_scanned_at: Option<String>,
    pub next_scan_at: Option<String>,
    pub last_new_chapter_at: Option<String>,
}

/// Read the persisted scan state for a series, if any.
pub async fn scan_state(pool: &SqlitePool, series_id: &str) -> Option<ScanState> {
    sqlx::query_as::<_, ScanState>(
        "SELECT avg_interval_hours, known_chapter_count, known_max_chapter, last_scanned_at, \
         next_scan_at, last_new_chapter_at FROM series_scan_state WHERE series_id = ?",
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

/// The latest (highest) chapter number seen, or `None` for an empty list.
/// Used both for new-chapter detection (SC4) and log output.
fn latest_number(chapters: &[SuwayomiChapter]) -> Option<f64> {
    chapters
        .iter()
        .map(|c| c.chapter_number)
        .fold(None, |acc, n| Some(acc.map_or(n, |a: f64| a.max(n))))
}

/// Two chapter numbers within this tolerance are treated as equal, so float
/// round-trips through SQLite don't spuriously read as "a higher chapter."
const CHAPTER_NUMBER_EPS: f64 = 1e-6;

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
/// re-reads the admin override so it's self-contained for both callers; the
/// fetch + detection + persist is delegated to `record_scan`.
pub async fn scan_series(
    state: &AppState,
    m: &SuwayomiManga,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let series_id = m.id.to_string();
    let admin = scan_admin(&state.pool, &series_id).await;
    let chapters = state.suwayomi.chapters(m.id).await?;
    record_scan(&state.pool, &series_id, &m.title, &admin, &chapters, now).await
}

/// Detect new chapters from a freshly-fetched chapter list and persist the
/// series' scan state. Split out of `scan_series` so the bookkeeping is testable
/// without a live Suwayomi (it reads its own `prior` row, so the read-modify-write
/// is self-contained).
///
/// First observation (no prior row) records the baseline chapter count *without*
/// stamping `last_new_chapter_at` — otherwise a fresh deploy's first tick flags the
/// entire seeded back catalogue as just-updated and floods the `updates` feed [SC3].
async fn record_scan(
    pool: &SqlitePool,
    series_id: &str,
    title: &str,
    admin: &ScanAdmin,
    chapters: &[SuwayomiChapter],
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let prior_opt = scan_state(pool, series_id).await;
    let first_observation = prior_opt.is_none();
    let prior = prior_opt.unwrap_or_default();

    let count = chapters.len() as i64;
    let computed_avg = avg_interval_hours(chapters).unwrap_or(prior.avg_interval_hours);
    let latest = latest_number(chapters);
    // A higher chapter number than we've seen means a new chapter even when the
    // *count* is unchanged — upstream can drop one chapter and add another within
    // an interval, leaving count flat but the max number advanced (SC4). Only
    // compares when a prior max exists, so pre-`0012`/first-observation rows just
    // seed a baseline rather than firing on the jump from "unknown".
    let advanced_number = match (latest, prior.known_max_chapter) {
        (Some(l), Some(prev)) => l > prev + CHAPTER_NUMBER_EPS,
        _ => false,
    };
    // On first observation we only record the baseline; a series is never "new"
    // the first time we see it (SC3).
    let new_found = !first_observation && (count > prior.known_chapter_count || advanced_number);
    // Highest number seen is a high-water mark: never regress it just because
    // upstream removed the top chapter, so a later re-add doesn't re-flag.
    let known_max = match latest {
        Some(l) => Some(prior.known_max_chapter.map_or(l, |p| p.max(l))),
        None => prior.known_max_chapter,
    };
    // A shrinking count means upstream removed chapters; surface it (the count is
    // still overwritten downward, but it's no longer silent) (SC4).
    if !first_observation && count < prior.known_chapter_count {
        tracing::info!(
            series_id,
            title,
            prior = prior.known_chapter_count,
            total = count,
            "scan: chapter count regressed (upstream removed chapters)"
        );
    }
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
            title,
            added = count - prior.known_chapter_count,
            total = count,
            latest = latest.unwrap_or(f64::NAN),
            avg_interval_hours = computed_avg,
            "scan: new chapters detected"
        );
    } else {
        tracing::debug!(
            series_id,
            title,
            total = count,
            avg_interval_hours = computed_avg,
            first_observation,
            "scan: no new chapters"
        );
    }

    sqlx::query(
        "INSERT INTO series_scan_state \
           (series_id, avg_interval_hours, known_chapter_count, known_max_chapter, \
            last_scanned_at, next_scan_at, last_new_chapter_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(series_id) DO UPDATE SET \
           avg_interval_hours = excluded.avg_interval_hours, \
           known_chapter_count = excluded.known_chapter_count, \
           known_max_chapter = excluded.known_max_chapter, \
           last_scanned_at = excluded.last_scanned_at, \
           next_scan_at = excluded.next_scan_at, \
           last_new_chapter_at = excluded.last_new_chapter_at, \
           updated_at = excluded.updated_at",
    )
    .bind(series_id)
    .bind(computed_avg)
    .bind(count)
    .bind(known_max)
    .bind(&now_iso)
    .bind(&next_scan_at)
    .bind(&last_new_chapter_at)
    .bind(&now_iso)
    .execute(pool)
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn chap(upload_date: Option<&str>) -> SuwayomiChapter {
        chap_n(1, 1.0, upload_date)
    }

    fn chap_n(id: i64, number: f64, upload_date: Option<&str>) -> SuwayomiChapter {
        SuwayomiChapter {
            id,
            manga_id: 1,
            name: "c".into(),
            chapter_number: number,
            scanlator: None,
            upload_date: upload_date.map(|s| s.to_string()),
            is_read: false,
            is_bookmarked: false,
            is_downloaded: false,
            last_page_read: 0,
            page_count: 0,
        }
    }

    /// N chapters numbered 1..=N, undated (count-only fixtures).
    fn chaps(n: i64) -> Vec<SuwayomiChapter> {
        (1..=n).map(|i| chap_n(i, i as f64, None)).collect()
    }

    async fn migrated_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[test]
    fn never_scanned_series_is_overdue() {
        assert!(is_overdue(None, 24.0, at("2026-01-01T00:00:00Z")));
    }

    #[test]
    fn recent_scan_within_interval_is_not_overdue() {
        // last scan 1h ago, interval 24h
        assert!(!is_overdue(
            Some("2025-12-31T23:00:00Z"),
            24.0,
            at("2026-01-01T00:00:00Z")
        ));
    }

    #[test]
    fn stale_scan_past_interval_is_overdue() {
        // last scan 7d ago, interval 24h
        assert!(is_overdue(
            Some("2025-12-25T00:00:00Z"),
            24.0,
            at("2026-01-01T00:00:00Z")
        ));
    }

    #[test]
    fn avg_interval_needs_two_dated_chapters() {
        assert_eq!(avg_interval_hours(&[]), None);
        assert_eq!(avg_interval_hours(&[chap(Some("1000"))]), None);
        // two chapters 24h apart (epoch seconds)
        let day = 24 * 3600;
        let a = chap(Some("1000000"));
        let b = chap(Some(&(1_000_000 + day).to_string()));
        let avg = avg_interval_hours(&[a, b]).unwrap();
        assert!((avg - 24.0).abs() < 0.001, "expected ~24h, got {avg}");
    }

    #[test]
    fn max_interval_clamp_avoids_duration_overflow() {
        // The absurd-override guard clamps to MAX_INTERVAL_HOURS; the resulting
        // Duration math must not overflow or panic (this was the m2 finding).
        let ms = (MAX_INTERVAL_HOURS * 3_600_000.0) as i64;
        let now = at("2026-01-01T00:00:00Z");
        let future = now + chrono::Duration::milliseconds(ms);
        assert!(future > now);
    }

    // ---- record_scan bookkeeping (DB-backed) ----

    async fn persisted(pool: &SqlitePool, series_id: &str) -> ScanState {
        scan_state(pool, series_id)
            .await
            .expect("scan state row should exist")
    }

    #[tokio::test]
    async fn first_observation_records_baseline_without_flagging_new() {
        // SC3: a fresh series with a full back catalogue must NOT be flagged as
        // "new" on first sight — record the baseline count, leave last_new NULL.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();
        let now = at("2026-01-01T00:00:00Z");

        let new_found = record_scan(&pool, "1", "S", &admin, &chaps(12), now)
            .await
            .unwrap();
        assert!(!new_found, "first observation must not report new chapters");

        let row = persisted(&pool, "1").await;
        assert_eq!(row.known_chapter_count, 12);
        assert!(
            row.last_new_chapter_at.is_none(),
            "first observation must not stamp last_new_chapter_at"
        );
    }

    #[tokio::test]
    async fn subsequent_scan_flags_new_chapter() {
        // Steady state after a baseline: an added chapter IS reported and stamps
        // last_new_chapter_at.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();

        record_scan(
            &pool,
            "1",
            "S",
            &admin,
            &chaps(12),
            at("2026-01-01T00:00:00Z"),
        )
        .await
        .unwrap();

        let then = at("2026-01-08T00:00:00Z");
        let new_found = record_scan(&pool, "1", "S", &admin, &chaps(13), then)
            .await
            .unwrap();
        assert!(new_found, "an added chapter must be reported");

        let row = persisted(&pool, "1").await;
        assert_eq!(row.known_chapter_count, 13);
        assert_eq!(
            row.last_new_chapter_at.as_deref(),
            Some(then.to_rfc3339()).as_deref()
        );
    }

    #[tokio::test]
    async fn count_stable_but_higher_number_is_new() {
        // SC4: upstream drops one chapter and adds a higher-numbered one within an
        // interval — count is unchanged (3 -> 3) but the max number advanced.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();
        let base = vec![
            chap_n(1, 1.0, None),
            chap_n(2, 2.0, None),
            chap_n(3, 3.0, None),
        ];
        let churned = vec![
            chap_n(1, 1.0, None),
            chap_n(2, 2.0, None),
            chap_n(4, 4.0, None),
        ];

        record_scan(&pool, "1", "S", &admin, &base, at("2026-01-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(persisted(&pool, "1").await.known_max_chapter, Some(3.0));

        let new_found = record_scan(
            &pool,
            "1",
            "S",
            &admin,
            &churned,
            at("2026-01-02T00:00:00Z"),
        )
        .await
        .unwrap();
        assert!(new_found, "a higher chapter number must count as new");
        assert_eq!(persisted(&pool, "1").await.known_max_chapter, Some(4.0));
    }

    #[tokio::test]
    async fn max_chapter_is_a_high_water_mark() {
        // Removing the top chapter must not regress the stored max (else a later
        // re-add would spuriously re-flag), and no new chapter is reported.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();
        let full = vec![
            chap_n(1, 1.0, None),
            chap_n(2, 2.0, None),
            chap_n(3, 3.0, None),
        ];
        let trimmed = vec![chap_n(1, 1.0, None), chap_n(2, 2.0, None)];

        record_scan(&pool, "1", "S", &admin, &full, at("2026-01-01T00:00:00Z"))
            .await
            .unwrap();
        let new_found = record_scan(
            &pool,
            "1",
            "S",
            &admin,
            &trimmed,
            at("2026-01-02T00:00:00Z"),
        )
        .await
        .unwrap();
        assert!(!new_found, "a shrink must not report a new chapter");
        let row = persisted(&pool, "1").await;
        assert_eq!(
            row.known_chapter_count, 2,
            "count follows upstream downward"
        );
        assert_eq!(
            row.known_max_chapter,
            Some(3.0),
            "max stays at the high-water mark"
        );
    }
}
