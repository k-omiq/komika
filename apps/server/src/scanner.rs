//! Adaptive scan scheduler.
//!
//! A background tokio task that keeps the federated library catalog fresh. Every
//! `SCAN_TICK_SECONDS` it walks the Suwayomi library and re-scans each series that
//! has reached its persisted `next_scan_at`. That schedule is adaptive:
//!
//!   steady_interval = admin override_interval_hours  (if set)
//!                     else rolling avg gap between chapter uploads
//!                     (clamped into a sane `[MIN, MAX]` range)
//!
//! After a scan finds a new chapter (or on first observation) the next scan is
//! scheduled a full `steady_interval` out. But once a series comes due and finds
//! *no* new chapter — the expected chapter is late — it enters an "awaiting" state
//! and is re-polled at the (clamped) admin `poll_every_minutes` cadence until the
//! chapter lands, then reverts to steady. The accelerated poll is bounded to a
//! window past the due time (`min(steady_interval, AWAITING_MAX_HOURS)`), so a
//! stalled series doesn't poll forever.
//!
//! Due series get their chapters re-fetched; new chapters (detected by chapter
//! count OR max chapter number vs. the last known values) are logged. All derived
//! state is persisted in `series_scan_state`, which `graphql::map_series` folds
//! back into `Series.scan` for the client and admin console.
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
/// fewer than two dated chapters). Without this, the steady interval would be 0.0
/// and the series would be re-scheduled for immediate re-fetch on every tick.
const DEFAULT_INTERVAL_HOURS: f64 = 24.0;

/// Upper bound on any effective interval, so an absurd admin override can't
/// overflow the `chrono::Duration` math when computing `next_scan_at`.
const MAX_INTERVAL_HOURS: f64 = 100.0 * 365.0 * 24.0; // ~100 years

/// Floor on an *inferred* (rolling-avg) interval. A same-day upload burst yields
/// sub-hour gaps, which would make the series overdue on essentially every 300s
/// tick and refetched every tick — needless source/FlareSolverr load. Real series
/// rarely sustain more than a few updates a day, so 6h is a safe floor for the
/// steady cadence (the accelerated overdue poll is a separate knob, see SC1).
const MIN_INTERVAL_HOURS: f64 = 6.0;

/// Hard floor applied to an explicit admin `override_interval_hours`. A deliberate
/// human override is allowed below `MIN_INTERVAL_HOURS` (that's the point of an
/// override), but never below this — it still protects upstreams from a 0.01h
/// hammer typo'd into the console.
const HARD_MIN_INTERVAL_HOURS: f64 = 1.0;

/// Resolve the steady effective interval (hours) from an optional admin override
/// and the inferred rolling average, clamping into a sane range (SC5):
///   - an explicit override wins and is clamped to `[HARD_MIN, MAX]`;
///   - otherwise the inferred avg (or the default when none) is clamped to
///     `[MIN, MAX]`, so a burst series isn't refetched every tick.
fn resolve_interval(override_interval_hours: Option<f64>, inferred_avg: f64) -> f64 {
    match override_interval_hours.filter(|v| *v > 0.0) {
        Some(o) => o.clamp(HARD_MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS),
        None => {
            let base = if inferred_avg > 0.0 {
                inferred_avg
            } else {
                DEFAULT_INTERVAL_HOURS
            };
            base.clamp(MIN_INTERVAL_HOURS, MAX_INTERVAL_HOURS)
        }
    }
}

/// Default accelerated re-poll cadence (minutes) when the admin hasn't set
/// `poll_every_minutes`. Mirrors the API default surfaced in `map_series`.
const DEFAULT_POLL_MINUTES: f64 = 30.0;

/// Floor on the accelerated re-poll cadence. The scan loop itself ticks every
/// `SCAN_TICK_SECONDS` (300s default), so a `poll_every_minutes` below the tick
/// cadence can't actually poll faster than the tick anyway; 15min is a gentle
/// floor that keeps overdue re-checks frequent without hammering upstreams.
const MIN_POLL_MINUTES: f64 = 15.0;

/// Absolute ceiling on how long a series stays in the accelerated poll cadence
/// past its due time before falling back to the steady cadence (SC1). Without a
/// bound, a stalled-but-ONGOING series (never auto-paused) — or one whose inferred
/// interval underestimates its true cadence — would poll every `poll_every_minutes`
/// indefinitely. A chapter that's actually coming almost always lands within a
/// couple of days of its cadence; past that the series is treated as steady again.
/// The effective window is `min(steady_interval, this)`, so short-cadence series
/// don't poll aggressively for many multiples of their own interval.
const AWAITING_MAX_HOURS: f64 = 48.0;

/// Resolve the accelerated re-poll cadence (minutes) for an *awaiting* series —
/// one that's overdue for a new chapter that hasn't landed yet (SC1). Clamped to
/// at least `MIN_POLL_MINUTES` and never above the steady interval (a poll slower
/// than the steady cadence would be no acceleration at all).
fn resolve_poll_minutes(poll_every_minutes: Option<i64>, steady_interval_hours: f64) -> f64 {
    let requested = poll_every_minutes
        .filter(|v| *v > 0)
        .map(|v| v as f64)
        .unwrap_or(DEFAULT_POLL_MINUTES);
    let steady_minutes = steady_interval_hours * 60.0;
    // `max` then `min` (not `clamp`) so a degenerate steady_minutes < MIN can't
    // panic; it just collapses the poll cadence onto the steady one.
    requested.max(MIN_POLL_MINUTES).min(steady_minutes)
}

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
    /// When the current overdue-awaiting streak began (SC1). `None` = not awaiting
    /// (a chapter is on schedule). Bounds how long the accelerated poll runs.
    pub awaiting_since: Option<String>,
}

/// The column list for a `ScanState` row, shared by `scan_state` (pooled read)
/// and `record_scan`'s in-transaction read so the two can't drift out of sync
/// with the struct.
const SCAN_STATE_SELECT: &str =
    "SELECT avg_interval_hours, known_chapter_count, known_max_chapter, \
     last_scanned_at, next_scan_at, last_new_chapter_at, awaiting_since \
     FROM series_scan_state WHERE series_id = ?";

/// Read the persisted scan state for a series, if any.
pub async fn scan_state(pool: &SqlitePool, series_id: &str) -> Option<ScanState> {
    sqlx::query_as::<_, ScanState>(SCAN_STATE_SELECT)
        .bind(series_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Admin overrides the scanner cares about (interval + poll cadence + pause),
/// read from `series_admin`.
#[derive(Default, sqlx::FromRow)]
struct ScanAdmin {
    override_interval_hours: Option<f64>,
    /// Accelerated re-poll cadence (minutes) used once a series is overdue for a
    /// new chapter that hasn't landed yet (SC1). `None` -> `DEFAULT_POLL_MINUTES`.
    poll_every_minutes: Option<i64>,
    paused_override: Option<i64>,
    status_override: Option<String>,
}

async fn scan_admin(pool: &SqlitePool, series_id: &str) -> ScanAdmin {
    sqlx::query_as::<_, ScanAdmin>(
        "SELECT override_interval_hours, poll_every_minutes, paused_override, status_override \
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

/// Decide whether a series is due for a scan by gating on its persisted
/// `next_scan_at` (SC1/SC7). A series with no scheduled scan (never scanned, or a
/// pre-existing row without one) is always due. Gating and the admin console's
/// "next due" now read the same stored value, so they can't disagree.
fn is_due(next_scan_at: Option<&str>, now: DateTime<Utc>) -> bool {
    match parse_iso(next_scan_at) {
        None => true,
        Some(next) => now >= next,
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

/// Best-effort: record each installed extension's coordinates against its source id
/// (§2.1), so a native device can install the exact extension a `source_series` came
/// from. Runs once per tick and is fully non-fatal — any failure (Suwayomi down, an
/// upstream schema mismatch, a write error) is logged and swallowed so it never
/// affects the scan. Additive: it only writes `source_extension` rows.
async fn record_source_extensions(state: &AppState) {
    let extensions = match state.suwayomi.fetch_extensions().await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, "scan tick: failed to fetch Suwayomi extensions");
            return;
        }
    };
    for ext in extensions {
        // An extension with no repo can't be installed from coordinates; skip it.
        let Some(repo_url) = ext.repo.clone() else {
            continue;
        };
        let input = crate::catalog::SourceExtensionInput {
            pkg_name: ext.pkg_name.clone(),
            repo_url,
            apk_name: ext.apk_name.clone(),
            version_code: ext.version_code,
            lang: ext.lang.clone(),
            is_nsfw: ext.is_nsfw,
        };
        for source_id in &ext.source_ids {
            if let Err(e) =
                crate::catalog::upsert_source_extension(&state.pool, source_id, &input).await
            {
                tracing::warn!(source_id, error = %e, "scan tick: failed to record source extension");
            }
        }
    }
}

/// Run one scan tick over the whole library. Returns `(library_size, overdue_seen)`
/// for aggregate health reporting. Per-series errors are logged and skipped.
async fn tick(state: &AppState) -> (usize, usize) {
    // Refresh extension coordinates first (§2.1); non-fatal, never affects the scan.
    record_source_extensions(state).await;

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

        // Gate on the persisted `next_scan_at`, which `scan_series` schedules from
        // the steady cadence normally and from the accelerated poll cadence while a
        // series is awaiting an overdue chapter (SC1). Gating and the admin
        // console's "next due" now read the same stored value, so they agree (SC7).
        // A brand-new series has no `next_scan_at` and is scanned promptly.
        if !is_due(prior.next_scan_at.as_deref(), now) {
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
///
/// The prior read and the upsert run in one transaction [SC6]: an admin
/// `triggerScan` overlapping a scheduler tick can no longer interleave their
/// read-modify-write and double-count or clobber `known_chapter_count`. The slow
/// chapter fetch already happened in `scan_series`, so the tx spans only the two
/// DB ops; under WAL a losing concurrent writer gets `SQLITE_BUSY_SNAPSHOT` and
/// this scan errors out (logged + skipped by the tick, retried next interval)
/// rather than committing a stale write.
async fn record_scan(
    pool: &SqlitePool,
    series_id: &str,
    title: &str,
    admin: &ScanAdmin,
    chapters: &[SuwayomiChapter],
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let mut tx = pool.begin().await?;
    let prior_opt = sqlx::query_as::<_, ScanState>(SCAN_STATE_SELECT)
        .bind(series_id)
        .fetch_optional(&mut *tx)
        .await?;
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
    // Steady cadence from fresh data (admin override wins), clamped into
    // `[MIN|HARD_MIN, MAX]` (SC5).
    let steady_interval = resolve_interval(admin.override_interval_hours, computed_avg);
    // "Awaiting" = the series was *genuinely due* (past its persisted `next_scan_at`)
    // and found no new chapter, so the expected chapter is late (SC1). We gate on
    // due-ness — not merely "a scan found nothing" — so an admin `triggerScan` that
    // force-scans a series *before* its cadence doesn't wrongly flip it into the
    // accelerated poll. First observation just recorded a baseline — not awaiting.
    let due_now = is_due(prior.next_scan_at.as_deref(), now);
    let awaiting = due_now && !new_found && !first_observation;
    // Stamp when the awaiting streak began (preserve the original start across
    // repeated polls); clear it as soon as a chapter lands or we're not awaiting.
    let awaiting_since = if awaiting {
        prior
            .awaiting_since
            .clone()
            .or_else(|| Some(now_iso.clone()))
    } else {
        None
    };
    // Re-poll fast only within a bounded window past the due time; beyond it the
    // series is treated as steady again so a stalled/underestimated series doesn't
    // poll forever (SC1). The window scales with the cadence but is capped.
    let awaiting_window_hours = steady_interval.min(AWAITING_MAX_HOURS);
    let awaited_hours = parse_iso(awaiting_since.as_deref())
        .map(|start| (now - start).num_seconds() as f64 / 3600.0)
        .unwrap_or(0.0);
    let next_interval_hours = if awaiting && awaited_hours < awaiting_window_hours {
        resolve_poll_minutes(admin.poll_every_minutes, steady_interval) / 60.0
    } else {
        steady_interval
    };
    let next_scan_at = (now
        + chrono::Duration::milliseconds((next_interval_hours * 3_600_000.0) as i64))
    .to_rfc3339();
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
            last_scanned_at, next_scan_at, last_new_chapter_at, awaiting_since, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(series_id) DO UPDATE SET \
           avg_interval_hours = excluded.avg_interval_hours, \
           known_chapter_count = excluded.known_chapter_count, \
           known_max_chapter = excluded.known_max_chapter, \
           last_scanned_at = excluded.last_scanned_at, \
           next_scan_at = excluded.next_scan_at, \
           last_new_chapter_at = excluded.last_new_chapter_at, \
           awaiting_since = excluded.awaiting_since, \
           updated_at = excluded.updated_at",
    )
    .bind(series_id)
    .bind(computed_avg)
    .bind(count)
    .bind(known_max)
    .bind(&now_iso)
    .bind(&next_scan_at)
    .bind(&last_new_chapter_at)
    .bind(&awaiting_since)
    .bind(&now_iso)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
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
    fn unscheduled_series_is_due() {
        // No next_scan_at (never scanned) -> always due.
        assert!(is_due(None, at("2026-01-01T00:00:00Z")));
    }

    #[test]
    fn future_next_scan_is_not_due() {
        assert!(!is_due(
            Some("2026-01-02T00:00:00Z"),
            at("2026-01-01T00:00:00Z")
        ));
    }

    #[test]
    fn past_next_scan_is_due() {
        assert!(is_due(
            Some("2025-12-31T00:00:00Z"),
            at("2026-01-01T00:00:00Z")
        ));
    }

    #[test]
    fn poll_cadence_is_clamped() {
        // Default poll (30m) passes through under a weekly steady interval.
        assert_eq!(resolve_poll_minutes(None, 168.0), DEFAULT_POLL_MINUTES);
        // A 1-minute poll floors to MIN_POLL_MINUTES (can't out-run the tick).
        assert_eq!(resolve_poll_minutes(Some(1), 168.0), MIN_POLL_MINUTES);
        // A poll slower than the steady interval collapses onto it (no negative
        // "acceleration"): steady 1h = 60m caps a 120m poll.
        assert_eq!(resolve_poll_minutes(Some(120), 1.0), 60.0);
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

    #[test]
    fn inferred_interval_clamps_up_to_min() {
        // A same-day burst (avg 0.2h) must be floored to MIN, not left sub-hour.
        assert_eq!(resolve_interval(None, 0.2), MIN_INTERVAL_HOURS);
        // A normal weekly avg passes straight through.
        assert_eq!(resolve_interval(None, 168.0), 168.0);
        // No inferable cadence -> default (which is itself >= MIN).
        assert_eq!(resolve_interval(None, 0.0), DEFAULT_INTERVAL_HOURS);
    }

    #[test]
    fn override_bypasses_min_but_not_hard_floor() {
        // A deliberate override may go below MIN...
        assert_eq!(resolve_interval(Some(2.0), 168.0), 2.0);
        // ...but never below the hard floor that protects upstreams.
        assert_eq!(resolve_interval(Some(0.01), 168.0), HARD_MIN_INTERVAL_HOURS);
        // Upper clamp still holds.
        assert_eq!(resolve_interval(Some(1e9), 0.0), MAX_INTERVAL_HOURS);
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

    /// Hours between `now` and a series' persisted `next_scan_at`.
    async fn hours_until_next_scan(pool: &SqlitePool, series_id: &str, now: DateTime<Utc>) -> f64 {
        let next = persisted(pool, series_id)
            .await
            .next_scan_at
            .expect("next_scan_at should be scheduled");
        let next = DateTime::parse_from_rfc3339(&next)
            .unwrap()
            .with_timezone(&Utc);
        (next - now).num_milliseconds() as f64 / 3_600_000.0
    }

    #[tokio::test]
    async fn awaiting_series_repolls_at_poll_cadence_then_reverts() {
        // SC1: a weekly series (override 168h) that comes due and finds no new
        // chapter re-polls at poll_every_minutes (30m), not a full week; once a
        // chapter lands it reverts to the steady 168h cadence.
        let pool = migrated_pool().await;
        let admin = ScanAdmin {
            override_interval_hours: Some(168.0),
            poll_every_minutes: Some(30),
            ..Default::default()
        };

        // Baseline (first observation): steady cadence, not awaiting.
        let t0 = at("2026-01-01T00:00:00Z");
        record_scan(&pool, "1", "S", &admin, &chaps(5), t0)
            .await
            .unwrap();
        assert!(
            (hours_until_next_scan(&pool, "1", t0).await - 168.0).abs() < 0.01,
            "first observation schedules the steady interval"
        );

        // Came due, no new chapter -> awaiting -> accelerated 30m poll.
        let t1 = at("2026-01-08T00:00:00Z");
        let new_found = record_scan(&pool, "1", "S", &admin, &chaps(5), t1)
            .await
            .unwrap();
        assert!(!new_found);
        assert!(
            (hours_until_next_scan(&pool, "1", t1).await - 0.5).abs() < 0.01,
            "awaiting series re-polls at the 30-minute poll cadence"
        );

        // Chapter finally lands -> revert to the steady interval.
        let t2 = at("2026-01-08T00:30:00Z");
        let new_found = record_scan(&pool, "1", "S", &admin, &chaps(6), t2)
            .await
            .unwrap();
        assert!(new_found);
        assert!(
            (hours_until_next_scan(&pool, "1", t2).await - 168.0).abs() < 0.01,
            "a landed chapter reverts to the steady cadence"
        );
        assert!(
            persisted(&pool, "1").await.awaiting_since.is_none(),
            "awaiting_since is cleared once a chapter lands"
        );
    }

    #[tokio::test]
    async fn early_manual_scan_does_not_enter_awaiting() {
        // SC1/item-2: an admin triggerScan that force-scans a series BEFORE its
        // cadence (not yet due) and finds nothing must NOT flip it into the
        // accelerated poll — that would be a false "overdue".
        let pool = migrated_pool().await;
        let admin = ScanAdmin {
            override_interval_hours: Some(168.0),
            poll_every_minutes: Some(30),
            ..Default::default()
        };

        let t0 = at("2026-01-01T00:00:00Z");
        record_scan(&pool, "1", "S", &admin, &chaps(5), t0)
            .await
            .unwrap();

        // Force-scan just 1h later — long before the 168h cadence.
        let early = at("2026-01-01T01:00:00Z");
        record_scan(&pool, "1", "S", &admin, &chaps(5), early)
            .await
            .unwrap();
        assert!(
            (hours_until_next_scan(&pool, "1", early).await - 168.0).abs() < 0.01,
            "a not-yet-due scan keeps the steady cadence"
        );
        assert!(
            persisted(&pool, "1").await.awaiting_since.is_none(),
            "a not-yet-due scan does not enter awaiting"
        );
    }

    #[tokio::test]
    async fn awaiting_backs_off_to_steady_after_window() {
        // SC1: a series that stays overdue past the awaiting window stops polling
        // aggressively and falls back to the steady cadence, so it can't poll
        // forever. Window = min(steady 168h, AWAITING_MAX_HOURS 48h) = 48h.
        let pool = migrated_pool().await;
        let admin = ScanAdmin {
            override_interval_hours: Some(168.0),
            poll_every_minutes: Some(30),
            ..Default::default()
        };

        let t0 = at("2026-01-01T00:00:00Z");
        record_scan(&pool, "1", "S", &admin, &chaps(5), t0)
            .await
            .unwrap();

        // First due-empty scan -> awaiting starts, accelerated poll.
        let t1 = at("2026-01-08T00:00:00Z");
        record_scan(&pool, "1", "S", &admin, &chaps(5), t1)
            .await
            .unwrap();
        let awaiting_since = persisted(&pool, "1").await.awaiting_since;
        assert_eq!(
            awaiting_since.as_deref(),
            Some(t1.to_rfc3339()).as_deref(),
            "awaiting streak starts at the first due-empty scan"
        );

        // Still empty 50h later (> 48h window) -> back off to steady, but the
        // awaiting streak start is preserved (not reset).
        let t2 = at("2026-01-10T02:00:00Z");
        record_scan(&pool, "1", "S", &admin, &chaps(5), t2)
            .await
            .unwrap();
        assert!(
            (hours_until_next_scan(&pool, "1", t2).await - 168.0).abs() < 0.01,
            "past the awaiting window the series reverts to the steady cadence"
        );
        assert_eq!(
            persisted(&pool, "1").await.awaiting_since.as_deref(),
            Some(t1.to_rfc3339()).as_deref(),
            "awaiting_since is preserved across the back-off"
        );
    }

    #[tokio::test]
    async fn repeated_identical_scan_does_not_double_count() {
        // SC6: the read-prior + upsert is a single transaction and `count` is set
        // from the fresh list (not incremented), so a triggerScan repeating a tick
        // over the same chapters can't double-count or re-flag.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();

        record_scan(
            &pool,
            "1",
            "S",
            &admin,
            &chaps(7),
            at("2026-01-01T00:00:00Z"),
        )
        .await
        .unwrap();
        let after_first = persisted(&pool, "1").await;

        let new_found = record_scan(
            &pool,
            "1",
            "S",
            &admin,
            &chaps(7),
            at("2026-01-01T00:05:00Z"),
        )
        .await
        .unwrap();
        assert!(!new_found, "an identical re-scan reports no new chapters");
        let after_second = persisted(&pool, "1").await;
        assert_eq!(
            after_second.known_chapter_count, 7,
            "count is not double-counted"
        );
        assert_eq!(
            after_first.known_chapter_count,
            after_second.known_chapter_count
        );
        assert!(
            after_second.last_new_chapter_at.is_none(),
            "an identical re-scan does not stamp last_new_chapter_at"
        );
    }

    /// End-to-end smoke test against a *live* Suwayomi. Ignored by default (needs a
    /// running Suwayomi with a seeded library); run with:
    ///   KOMIKA_LIVE_SUWAYOMI=http://localhost:4567 \
    ///     cargo test --bin komika-server -- --ignored live_suwayomi_end_to_end --nocapture
    /// It validates the real GraphQL queries/deserialization (which the synthetic
    /// unit tests can't) and drives `record_scan` off real chapter data.
    #[tokio::test]
    #[ignore = "requires a live Suwayomi with a seeded library (KOMIKA_LIVE_SUWAYOMI)"]
    async fn live_suwayomi_end_to_end() {
        let Ok(base) = std::env::var("KOMIKA_LIVE_SUWAYOMI") else {
            eprintln!("skipped: set KOMIKA_LIVE_SUWAYOMI=http://localhost:4567");
            return;
        };
        let client = crate::suwayomi::SuwayomiClient::new(base, None, None);

        // (1) Real Suwayomi schema: library()/series()/chapters() deserialize.
        let lib = client.library().await.expect("library()");
        assert!(
            !lib.is_empty(),
            "seed a series into the Suwayomi library first"
        );
        let m = lib.into_iter().next().unwrap();
        let detail = client.series(m.id).await.expect("series()");
        assert_eq!(detail.id, m.id, "series() resolves the same manga");
        let chapters = client.chapters(m.id).await.expect("chapters()");
        assert!(!chapters.is_empty(), "expected the series to have chapters");

        // (2) Real millisecond uploadDate strings yield a positive finite cadence.
        let avg = avg_interval_hours(&chapters);
        if let Some(a) = avg {
            assert!(a > 0.0 && a.is_finite(), "avg_interval_hours = {a}");
        }

        let count = chapters.len() as i64;
        let real_max = latest_number(&chapters).expect("a max chapter number");
        eprintln!(
            "live '{}' (#{}) — {} chapters, max #{}, avg_interval_hours={:?}",
            m.title, m.id, count, real_max, avg
        );

        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();
        let sid = m.id.to_string();
        let now = at("2026-07-15T00:00:00Z");

        // (3) First observation over REAL data: baseline recorded, nothing flagged
        // as new (SC3), and known_max_chapter captured even though it exceeds the
        // count (SC4 — here 166.5 > 102 for Oshi no Ko).
        let new_found = record_scan(&pool, &sid, &m.title, &admin, &chapters, now)
            .await
            .unwrap();
        assert!(
            !new_found,
            "first observation must not flag the back catalogue"
        );
        let row = persisted(&pool, &sid).await;
        assert_eq!(row.known_chapter_count, count);
        assert_eq!(row.known_max_chapter, Some(real_max));
        assert!(row.last_new_chapter_at.is_none());

        // (4) Identical re-scan a minute later: not due, no double-count (SC6).
        let again = record_scan(
            &pool,
            &sid,
            &m.title,
            &admin,
            &chapters,
            now + chrono::Duration::minutes(1),
        )
        .await
        .unwrap();
        assert!(!again, "an identical re-scan reports no new chapters");
        assert_eq!(persisted(&pool, &sid).await.known_chapter_count, count);

        // (5) Splice a higher-numbered chapter onto the real list and mark the
        // series due: SC4 detects the number advance; a landed chapter clears
        // awaiting and reverts to steady cadence (SC1).
        let mut plus = chapters.clone();
        let mut top = plus[0].clone();
        top.id = i64::MAX;
        top.chapter_number = real_max + 1.0;
        plus.push(top);
        sqlx::query("UPDATE series_scan_state SET next_scan_at = NULL WHERE series_id = ?")
            .bind(&sid)
            .execute(&pool)
            .await
            .unwrap();
        let landed = record_scan(
            &pool,
            &sid,
            &m.title,
            &admin,
            &plus,
            now + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        assert!(landed, "a higher chapter number must be detected as new");
        let row = persisted(&pool, &sid).await;
        assert_eq!(row.known_max_chapter, Some(real_max + 1.0));
        assert!(row.last_new_chapter_at.is_some());
        assert!(
            row.awaiting_since.is_none(),
            "a landed chapter is not awaiting"
        );
    }
}
