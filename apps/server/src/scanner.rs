//! Adaptive scan scheduler.
//!
//! A background tokio task that keeps the federated library catalog fresh. Every
//! `SCAN_TICK_SECONDS` it selects — straight from the DB, indexed on `next_scan_at` —
//! only the series that have reached their persisted `next_scan_at` and re-scans those.
//! It does NOT fetch the whole Suwayomi library per tick, so tick cost scales with the
//! DUE set, not the catalogue size (which matters at 100k+ series). Enrolment and the
//! daily source-sync reconcile (`crate::sync`) guarantee every enrolled series has a
//! `series_scan_state` row for this query to find. That schedule is adaptive:
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
use rand_core::RngCore;
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
/// `SCAN_TICK_SECONDS` (3600s / 1h default), so a `poll_every_minutes` below the tick
/// cadence can't actually poll faster than the tick anyway — with the hourly default,
/// the tick is the effective floor for overdue re-checks; 15min stays a gentle floor
/// for deployments that lower the tick.
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
    /// Snapshot of the chapter identity set at the last scan (sorted, comma-joined
    /// chapter ids). Drives set-difference new-chapter detection so a removal that
    /// offsets an insertion at/below the current max is still flagged. `None` =
    /// not yet captured (pre-migration row / first observation), which seeds a
    /// baseline without flagging.
    pub known_chapter_ids: Option<String>,
}

/// The column list for a `ScanState` row, shared by `scan_state` (pooled read)
/// and `record_scan`'s in-transaction read so the two can't drift out of sync
/// with the struct.
const SCAN_STATE_SELECT: &str =
    "SELECT avg_interval_hours, known_chapter_count, known_max_chapter, \
     last_scanned_at, next_scan_at, last_new_chapter_at, awaiting_since, known_chapter_ids \
     FROM series_scan_state WHERE series_id = ?";

/// Read the persisted scan state for a series, if any. The DB-driven scheduler selects
/// due ids directly and `record_scan` re-reads state in its own transaction, so this
/// standalone pooled read is now only used by tests.
#[cfg(test)]
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
/// Test-only helper for the SC4 new-chapter-detection assertions.
#[cfg(test)]
fn latest_number(chapters: &[SuwayomiChapter]) -> Option<f64> {
    chapters
        .iter()
        .map(|c| c.chapter_number)
        .fold(None, |acc, n| Some(acc.map_or(n, |a: f64| a.max(n))))
}

/// The highest *real* chapter number in the current list, dropping obvious
/// sentinels/outliers — Suwayomi's `-1.0` "unnumbered" marker, NaN, and absurdly
/// large values. Unlike a monotonic all-time high-water mark, this is derived
/// from the CURRENT list, so `known_max_chapter` self-heals once a garbage number
/// disappears upstream instead of being pinned forever (SC4).
fn robust_max_number(chapters: &[SuwayomiChapter]) -> Option<f64> {
    chapters
        .iter()
        .map(|c| c.chapter_number)
        .filter(|n| n.is_finite() && *n >= 0.0 && *n < 100_000.0)
        .fold(None, |acc, n| Some(acc.map_or(n, |a: f64| a.max(n))))
}

/// Encode a set of chapter ids as a stable, sorted, comma-joined string for the
/// `known_chapter_ids` snapshot (SC set-diff detection).
fn encode_id_set(ids: &std::collections::HashSet<i64>) -> String {
    let mut v: Vec<i64> = ids.iter().copied().collect();
    v.sort_unstable();
    v.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a `known_chapter_ids` snapshot back into a set.
fn parse_id_set(s: &str) -> std::collections::HashSet<i64> {
    s.split(',')
        .filter_map(|t| t.trim().parse::<i64>().ok())
        .collect()
}

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
/// affects the scan. Additive: it only writes `source_extension` rows. Called from the
/// daily source-sync reconcile (extensions change rarely, so a per-scan-tick refresh was
/// wasteful once the scan tick stopped touching Suwayomi).
pub(crate) async fn record_source_extensions(state: &AppState) {
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

/// Max due series scanned in a single tick — bounds a cold-start/backlog tick so it
/// can't run unbounded; the remainder drains on later ticks (ordered oldest-due first,
/// so nothing starves).
const DUE_BATCH_LIMIT: i64 = 5000;
/// Short breather between back-to-back drain batches, so a continuous backlog drain
/// doesn't peg the CPU/DB in a tight loop between 5k-series batches.
const DRAIN_BATCH_DELAY_MS: u64 = 250;
/// How far out a paused (COMPLETED/HIATUS/CANCELLED or admin-paused) series is parked so
/// it drops out of the frequent due-set — the whole point of pause. It's still
/// re-evaluated this often, so an upstream status flip / cleared override eventually
/// heals on its own without a full-library sweep. (Admin unpause resets `next_scan_at`
/// for promptness — see `set_series_paused` / `update_series_admin`.)
const PAUSED_PARK_HOURS: i64 = 24 * 14; // 14 days

/// Base delay (minutes) for the first failed scan's error-backoff. Subsequent
/// consecutive failures double this (30m, 1h, 2h, …) up to `ERROR_BACKOFF_MAX_HOURS`,
/// so a permanently-failing series (deleted upstream, 404) leaves the hot front of the
/// due-set instead of being retried every tick and starving healthy series (audit #4).
const ERROR_BACKOFF_BASE_MINUTES: i64 = 30;
/// Cap on the error-backoff so a permanently-failing series stops growing its delay and
/// settles into a daily re-check (it might come back). Independent of `PAUSED_PARK_HOURS`:
/// a dead/erroring id is retried ~daily, more often than a genuinely paused series' 14-day
/// park, because a fetch error (unlike a clean paused status) may be transient.
const ERROR_BACKOFF_MAX_HOURS: i64 = 24;

/// "Due now" sentinel for `next_scan_at`. A never-scanned / freshly-enrolled series is
/// stored with this far-past timestamp instead of NULL so the due-query can be a single
/// bounded `next_scan_at <= ?` range seek (index-early-terminating, O(due)) rather than an
/// `IS NULL OR <=` full index scan. It sorts before every real timestamp (RFC3339 string
/// order) so due-now rows come first. COMPLETENESS INVARIANT: every enrolled series must
/// have a NON-NULL `next_scan_at` — a stray NULL would never match `<= ?` and the series
/// would silently never scan. Every writer here (and `catalog::backfill_pending_scan_states`)
/// therefore writes a real time or this sentinel; migration 0048 backfilled legacy NULLs.
pub const DUE_NOW_SENTINEL: &str = "1970-01-01T00:00:00+00:00";

/// Outcome of one `tick`: enough to drive the drain loop and honest health reporting.
struct TickOutcome {
    /// Total tracked series (`series_scan_state` row count) — health "library size".
    tracked: usize,
    /// Rows the due-query selected this batch (== `DUE_BATCH_LIMIT` ⇒ likely more waiting).
    due: usize,
    /// Scans that completed and advanced `next_scan_at` (progress signal for the drain).
    ok: usize,
    /// Scans that errored and were backed off.
    failed: usize,
}

/// Run one scheduler pass, DB-driven: pull ONLY the series whose `next_scan_at` is due
/// (or NULL = never scanned / freshly enrolled) straight from `series_scan_state` — no
/// full-library fetch, no O(library) per-tick sweep — and scan them with small bounded
/// concurrency. Paused series park themselves far out (see `scan_due`), so they fall out
/// of the due-set naturally.
async fn tick(state: &AppState, shutdown: &tokio::sync::watch::Receiver<bool>) -> TickOutcome {
    let now = Utc::now();
    let due = due_series_ids(&state.pool, &now.to_rfc3339(), DUE_BATCH_LIMIT).await;
    let due_count = due.len();

    // SMALL bounded concurrency: each scan is a live upstream fetch through FlareSolverr
    // (which stalls — hence the 30s timeout), so overlap stays modest; distinct series
    // touch distinct DB rows so the concurrent writes don't collide (SQLite's single
    // writer + busy_timeout serialize).
    use futures::StreamExt as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const SCAN_CONCURRENCY: usize = 3;
    let ok = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    futures::stream::iter(due)
        .for_each_concurrent(SCAN_CONCURRENCY, |series_id| {
            let ok = &ok;
            let failed = &failed;
            async move {
                // Honor shutdown mid-batch: a full DUE_BATCH_LIMIT batch at concurrency 3
                // can run a long time, so stop starting new scans the moment shutdown fires
                // instead of blocking a graceful stop for the whole batch (audit LOW).
                if *shutdown.borrow() {
                    return;
                }
                match scan_due(state, &series_id, now).await {
                    Ok(_) => {
                        ok.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(series_id, error = %e, "scan: series scan failed; backed off");
                    }
                }
            }
        })
        .await;

    let tracked = scan_state_count(&state.pool).await;
    TickOutcome {
        tracked,
        due: due_count,
        ok: ok.into_inner(),
        failed: failed.into_inner(),
    }
}

/// Due series ids from the DB (uses `idx_scan_state_next_scan`). A single bounded
/// `next_scan_at <= ?` range seek: the index supplies the order (`ORDER BY next_scan_at
/// ASC`, no temp-b-tree sort) AND terminates the scan at the first future-dated row, so
/// future rows are never visited — cost is O(due), not O(catalogue). Never-scanned /
/// freshly-enrolled rows carry the far-past `DUE_NOW_SENTINEL` (not NULL), so they sort
/// first as due-now, then oldest-scheduled first — a cold-start backlog drains fairly
/// across ticks. See `DUE_NOW_SENTINEL` for the completeness invariant this relies on.
async fn due_series_ids(pool: &SqlitePool, now_iso: &str, limit: i64) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT series_id FROM series_scan_state \
         WHERE next_scan_at <= ? \
         ORDER BY next_scan_at ASC LIMIT ?",
    )
    .bind(now_iso)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "scan: due-series query failed");
        Vec::new()
    })
}

/// Count of tracked series — the health snapshot's "library size".
async fn scan_state_count(pool: &SqlitePool) -> usize {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM series_scan_state")
        .fetch_one(pool)
        .await
        .unwrap_or(0) as usize
}

/// Scheduler entry point: scan ONE due series by id.
///
/// A single combined upstream fetch (fresh status + chapters), then record the scan —
/// *unconditionally*, even for a paused series. Scanning first is deliberate: it
/// baselines a never-observed series so even a COMPLETED/HIATUS series gets a real
/// chapter count (a paused series that had never been scanned otherwise shows 0 chapters
/// forever), and it refreshes status so an upstream *reopen* (COMPLETED → ONGOING)
/// auto-resumes scanning without waiting for an admin. If the series is *still* paused
/// afterwards, it is PARKED far out — overriding the steady next-scan `record_scan` just
/// set — so it leaves the frequent due-set. Net cost of a steady paused series is thus
/// one fetch per park window (~14d), which also catches the rare late chapter on a
/// "completed" series. (Zero-cost "never fetch paused" was rejected: it reintroduces the
/// 0-chapter bug and loses reopen detection the old live-`library()` sweep had.)
async fn scan_due(state: &AppState, series_id: &str, now: DateTime<Utc>) -> anyhow::Result<bool> {
    let Ok(id) = series_id.parse::<i64>() else {
        // A non-numeric id can't be a Suwayomi series; park it so it stops recurring.
        park_paused(&state.pool, series_id, now).await?;
        return Ok(false);
    };
    let admin = scan_admin(&state.pool, series_id).await;
    // ONE combined fetch for fresh status + chapters (falls back to two calls internally
    // on an older engine). On failure, back the series off (exponential, capped) so a
    // deleted/404'ing id or a transient outage doesn't leave it pinned at the front of
    // the due-ordering, re-tried every tick and starving healthy series (audit #1/#4).
    let (m, chapters) = match state.suwayomi.series_and_chapters(id).await {
        Ok(v) => v,
        Err(e) => {
            if let Err(be) = record_scan_failure(&state.pool, series_id, now).await {
                tracing::warn!(series_id, error = %be, "scan: failed to record backoff");
            }
            return Err(e);
        }
    };
    let found = match persist_scan(state, &m, &chapters, &admin, now).await {
        Ok(v) => v,
        Err(e) => {
            if let Err(be) = record_scan_failure(&state.pool, series_id, now).await {
                tracing::warn!(series_id, error = %be, "scan: failed to record backoff");
            }
            return Err(e);
        }
    };
    // Park AFTER the scan (overriding the steady cadence `record_scan` set) so a paused
    // series drops out of the frequent due-set but was still baselined + status-checked.
    if is_paused(effective_status(&m, &admin), &admin) {
        park_paused(&state.pool, series_id, now).await?;
    }
    Ok(found)
}

/// Record a failed scan: bump `consecutive_failures` and push `next_scan_at` out with an
/// exponential, capped backoff (30m, 1h, 2h, … up to `ERROR_BACKOFF_MAX_HOURS`). Upsert so
/// a never-scanned series with no row yet is still backed off. A successful `record_scan`
/// resets `consecutive_failures` to 0. This is what keeps a permanently-failing series from
/// starving every healthy series behind it in the due-ordering (audit #1/#4).
async fn record_scan_failure(
    pool: &SqlitePool,
    series_id: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let prior: i64 = sqlx::query_scalar(
        "SELECT consecutive_failures FROM series_scan_state WHERE series_id = ?",
    )
    .bind(series_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or(0);
    let failures = prior.saturating_add(1);
    // base * 2^(failures-1), capped. `checked_shl` guards the shift; the `.min` caps it
    // well below any overflow, so a huge streak just pins at the cap.
    let shift = (failures - 1).clamp(0, 40) as u32;
    let backoff_minutes = ERROR_BACKOFF_BASE_MINUTES
        .saturating_mul(1i64.checked_shl(shift).unwrap_or(i64::MAX))
        .min(ERROR_BACKOFF_MAX_HOURS * 60);
    let next = (now + chrono::Duration::minutes(backoff_minutes)).to_rfc3339();
    let now_iso = now.to_rfc3339();
    sqlx::query(
        "INSERT INTO series_scan_state (series_id, next_scan_at, consecutive_failures, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(series_id) DO UPDATE SET \
           next_scan_at = excluded.next_scan_at, \
           consecutive_failures = excluded.consecutive_failures, \
           updated_at = excluded.updated_at",
    )
    .bind(series_id)
    .bind(&next)
    .bind(failures)
    .bind(&now_iso)
    .execute(pool)
    .await?;
    Ok(())
}

/// Park a paused series' next scan far out so it drops out of the frequent due-set,
/// leaving its other scan-state fields intact. Upsert so a never-scanned paused series
/// (a pending row, or none yet) is parked too.
async fn park_paused(pool: &SqlitePool, series_id: &str, now: DateTime<Utc>) -> anyhow::Result<()> {
    // Jitter the park window by ±(PAUSED_PARK_HOURS/10) so a cold-start cohort parked in
    // the same drain doesn't all come back due in the same window 14 days later and
    // re-cluster into a thundering herd (audit #9).
    let spread = (PAUSED_PARK_HOURS / 5).max(1);
    let jitter = (rand_core::OsRng.next_u64() % spread as u64) as i64 - spread / 2;
    let next = (now + chrono::Duration::hours(PAUSED_PARK_HOURS + jitter)).to_rfc3339();
    let now_iso = now.to_rfc3339();
    // Also clear any stale `awaiting_since` (a paused series is not "awaiting a late
    // chapter") and reset the failure counter — the fetch that led here succeeded.
    sqlx::query(
        "INSERT INTO series_scan_state (series_id, next_scan_at, awaiting_since, consecutive_failures, updated_at) \
         VALUES (?, ?, NULL, 0, ?) \
         ON CONFLICT(series_id) DO UPDATE SET \
           next_scan_at = excluded.next_scan_at, \
           awaiting_since = NULL, \
           consecutive_failures = 0, \
           updated_at = excluded.updated_at",
    )
    .bind(series_id)
    .bind(&next)
    .bind(&now_iso)
    .execute(pool)
    .await?;
    Ok(())
}

/// Ensure a series has a scan-state row so the DB-driven scheduler will pick it up.
/// Inserts a minimal "due now" row (`next_scan_at = DUE_NOW_SENTINEL`) without disturbing
/// an existing one. Used by enrol paths that DON'T scan-on-enrol (federated search) and by
/// the daily reconcile to backfill any pre-existing enrolled series that lacks a row. The
/// sentinel (not NULL) is required by the due-query's `<= ?` completeness invariant.
pub async fn ensure_pending(pool: &SqlitePool, series_id: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO series_scan_state (series_id, next_scan_at, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(series_id) DO NOTHING",
    )
    .bind(series_id)
    .bind(DUE_NOW_SENTINEL)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Re-fetch one series' chapters, detect new ones, and persist its scan state
/// (rolling avg, chapter count, `last_scanned_at`, next `next_scan_at`). Returns
/// whether new chapters were found.
///
/// The enrol paths (`add_source_series`, `ingest_source_series`) and the admin
/// `triggerScan` / unpause mutations call this with a manga they already hold; the
/// scheduler uses `scan_due` (which fetches manga + chapters together). It re-reads the
/// admin override so it's self-contained; fetch + persist is delegated to `persist_scan`.
pub async fn scan_series(
    state: &AppState,
    m: &SuwayomiManga,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let admin = scan_admin(&state.pool, &m.id.to_string()).await;
    let chapters = state.suwayomi.chapters(m.id).await?;
    persist_scan(state, m, &chapters, &admin, now).await
}

/// Persist a scan from an ALREADY-FETCHED manga + chapters: refresh the DB caches (S1 —
/// so reader requests serve from SQLite instead of live-fetching), then detect new
/// chapters + schedule the next scan (`record_scan`). Cache writes are best-effort — a
/// cache hiccup must never fail the scan. Shared by `scan_series` and `scan_due`.
async fn persist_scan(
    state: &AppState,
    m: &SuwayomiManga,
    chapters: &[SuwayomiChapter],
    admin: &ScanAdmin,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let series_id = m.id.to_string();
    if let Err(e) = crate::series_cache::put_series(&state.pool, m).await {
        tracing::warn!(series_id, error = %e, "scan: series cache write failed");
    }
    if let Err(e) = crate::series_cache::put_chapters(&state.pool, m.id, chapters).await {
        tracing::warn!(series_id, error = %e, "scan: chapter cache write failed");
    }
    record_scan(&state.pool, &series_id, &m.title, admin, chapters, now).await
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
    // `known_max` is derived from the CURRENT list (sentinels dropped) so it heals
    // downward once garbage disappears upstream, instead of being pinned forever by
    // a single bad number. Falls back to the prior value only when the current list
    // has no real numbers at all (SC4).
    let latest = robust_max_number(chapters);
    let known_max = latest.or(prior.known_max_chapter);

    // New-chapter detection is a set-difference of chapter identities against the
    // prior snapshot: a chapter present now but absent before is new — even when
    // upstream simultaneously removed one (count stays flat) or the new chapter is
    // numbered at/below the current max. A missing prior snapshot (pre-migration
    // row, or first observation) seeds a baseline WITHOUT flagging, so neither an
    // upgrade nor a fresh series floods the `updates` feed (SC3).
    let current_ids: std::collections::HashSet<i64> = chapters.iter().map(|c| c.id).collect();
    let prior_ids = prior
        .known_chapter_ids
        .as_deref()
        .map(parse_id_set)
        .unwrap_or_default();
    let have_prior_snapshot = prior.known_chapter_ids.is_some();
    let new_found = !first_observation
        && have_prior_snapshot
        && current_ids.iter().any(|id| !prior_ids.contains(id));
    let known_chapter_ids = encode_id_set(&current_ids);
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
    // accelerated poll. We also require a prior chapter SNAPSHOT: the FIRST real scan of
    // a series is a baseline, not a late chapter — even when a `series_scan_state` row
    // already exists from `ensure_pending`/backfill (which pre-seeds a row with a due-now
    // sentinel `next_scan_at` but NO `known_chapter_ids`), so `first_observation` alone
    // isn't enough to recognise a baseline. Without this, every newly-enrolled and
    // federated-ingested series would wrongly enter the 30-min accelerated poll on its
    // first scheduler scan.
    let due_now = is_due(prior.next_scan_at.as_deref(), now);
    let awaiting = due_now && !new_found && !first_observation && have_prior_snapshot;
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
            last_scanned_at, next_scan_at, last_new_chapter_at, awaiting_since, \
            known_chapter_ids, consecutive_failures, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?) \
         ON CONFLICT(series_id) DO UPDATE SET \
           avg_interval_hours = excluded.avg_interval_hours, \
           known_chapter_count = excluded.known_chapter_count, \
           known_max_chapter = excluded.known_max_chapter, \
           last_scanned_at = excluded.last_scanned_at, \
           next_scan_at = excluded.next_scan_at, \
           last_new_chapter_at = excluded.last_new_chapter_at, \
           awaiting_since = excluded.awaiting_since, \
           known_chapter_ids = excluded.known_chapter_ids, \
           consecutive_failures = 0, \
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
    .bind(&known_chapter_ids)
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
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    // Supervisor: run the tick loop in a child task and, if it panics, restart it
    // after a short backoff so a single panic doesn't permanently kill the scan
    // scheduler (leaving a silently-stale catalogue). A clean (shutdown) exit ends
    // supervision.
    tokio::spawn(async move {
        loop {
            let handle = tokio::spawn(run_loop(state.clone(), tick_seconds, shutdown.clone()));
            match handle.await {
                Ok(()) => break,
                Err(e) if e.is_panic() => {
                    if *shutdown.borrow() {
                        break;
                    }
                    tracing::error!("scan scheduler loop panicked; restarting in 10s");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    if *shutdown.borrow() {
                        break;
                    }
                }
                Err(_) => break, // cancelled
            }
        }
    });
}

async fn run_loop(
    state: Arc<AppState>,
    tick_seconds: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = interval(Duration::from_secs(tick_seconds));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tracing::info!(tick_seconds, "scan scheduler started");
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let started = Utc::now();
                // Accumulate across the whole drain pass so health reports total work
                // done this pass, not just the last batch's counts.
                let mut drain_ok = 0usize;
                let mut drain_failed = 0usize;
                // Drain: a tick processes at most DUE_BATCH_LIMIT series. When it comes
                // back full there's almost certainly more due (a cold-start backlog, or
                // catch-up after downtime), so keep draining immediately instead of
                // idling a whole interval between 5k-series batches — then settle back
                // to the steady cadence once a batch comes back short (caught up).
                loop {
                    let out = tick(&state, &shutdown).await;
                    drain_ok += out.ok;
                    drain_failed += out.failed;
                    {
                        // Recover from a poisoned lock rather than propagating the panic
                        // (which would kill the scheduler task) — a stale health snapshot
                        // is harmless compared with a dead scan loop.
                        let mut h = state.scan_health.lock().unwrap_or_else(|e| e.into_inner());
                        h.library_size = out.tracked;
                        h.overdue_count = out.due;
                        h.last_tick_at = Some(started.to_rfc3339());
                        h.scanned_ok = drain_ok;
                        h.scanned_failed = drain_failed;
                        if out.ok > 0 {
                            h.last_success_at = Some(Utc::now().to_rfc3339());
                        }
                        // "Stuck" = a batch that ATTEMPTED work but advanced nothing
                        // (upstream outage, or a wall of dead ids) — gated on failures-with-
                        // no-success, NOT on batch size, so a normal-scale outage (a small
                        // due-set, all failing) trips it too, not only a >=DUE_BATCH_LIMIT
                        // backlog. An idle tick (nothing due, no failures) resets it.
                        if out.failed > 0 && out.ok == 0 {
                            h.consecutive_stuck_ticks = h.consecutive_stuck_ticks.saturating_add(1);
                        } else {
                            h.consecutive_stuck_ticks = 0;
                        }
                    }
                    tracing::info!(
                        library_size = out.tracked,
                        overdue = out.due,
                        ok = out.ok,
                        failed = out.failed,
                        "scan tick complete"
                    );
                    // Keep draining ONLY while a full batch is still making real progress.
                    // A full batch that scanned nothing successfully (total upstream outage)
                    // must NOT tight-loop — break to the interval so we back off instead of
                    // hammering a dead upstream; the per-series error-backoff has already
                    // pushed those rows out of the hot front (audit #1).
                    let more_due = out.due >= DUE_BATCH_LIMIT as usize;
                    if !more_due || out.ok == 0 || *shutdown.borrow() {
                        break;
                    }
                    // Gentle breather between continuous drain batches.
                    tokio::time::sleep(Duration::from_millis(DRAIN_BATCH_DELAY_MS)).await;
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("scan scheduler stopping");
                    break;
                }
            }
        }
    }
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
    async fn max_chapter_heals_on_shrink_without_flagging() {
        // Removing the top chapter is not a new chapter (set-difference: no id
        // present now was absent before). Unlike the old monotonic high-water mark,
        // known_max now HEALS down to the current list's real max (SC4), so a
        // transient garbage number can't pin it forever.
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
            Some(2.0),
            "max heals to the current list's real max"
        );
    }

    #[tokio::test]
    async fn sentinel_number_does_not_pin_known_max() {
        // A garbage/sentinel chapter number (Suwayomi's -1.0, or an absurd value)
        // must not become the stored known_max — otherwise number-derived state is
        // pinned forever. The robust max ignores it and uses the real top chapter.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();
        let with_sentinel = vec![
            chap_n(1, 1.0, None),
            chap_n(2, 2.0, None),
            chap_n(3, -1.0, None),
            chap_n(4, 999_999.0, None),
        ];
        record_scan(
            &pool,
            "1",
            "S",
            &admin,
            &with_sentinel,
            at("2026-01-01T00:00:00Z"),
        )
        .await
        .unwrap();
        assert_eq!(
            persisted(&pool, "1").await.known_max_chapter,
            Some(2.0),
            "sentinels are excluded from known_max"
        );
    }

    #[tokio::test]
    async fn removal_offsetting_insertion_is_flagged() {
        // A removal that offsets an insertion at/below the current max (count flat,
        // max unchanged) is still a new chapter via set-difference of identities.
        let pool = migrated_pool().await;
        let admin = ScanAdmin::default();
        // ids {10,11,12}, numbers 1,2,3.
        let base = vec![
            chap_n(10, 1.0, None),
            chap_n(11, 2.0, None),
            chap_n(12, 3.0, None),
        ];
        // Drop id 11 (number 2), add id 13 renumbered 2 — count flat (3), max flat.
        let churned = vec![
            chap_n(10, 1.0, None),
            chap_n(13, 2.0, None),
            chap_n(12, 3.0, None),
        ];
        record_scan(&pool, "1", "S", &admin, &base, at("2026-01-01T00:00:00Z"))
            .await
            .unwrap();
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
        assert!(
            new_found,
            "a new chapter identity must be flagged even with flat count and max"
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
    async fn first_real_scan_of_preregistered_series_is_not_awaiting() {
        // Regression (verify pass): enrol paths (add_source_series/federated_ingest) and
        // the reconcile backfill now pre-seed a due-now `series_scan_state` row via
        // ensure_pending — which has NO chapter snapshot. The first REAL scan must still be
        // treated as a baseline (steady cadence, not awaiting), even though a row already
        // exists (so `first_observation` is false). Without gating awaiting on
        // `have_prior_snapshot`, every newly-enrolled series would flip into the 30-min
        // accelerated poll for up to 48h on its first scan.
        let pool = migrated_pool().await;
        let admin = ScanAdmin {
            override_interval_hours: Some(168.0),
            poll_every_minutes: Some(30),
            ..Default::default()
        };
        // Pre-register with a due-now sentinel row + no snapshot (what ensure_pending does).
        ensure_pending(&pool, "1").await.unwrap();
        let t0 = at("2026-01-01T00:00:00Z");
        let new_found = record_scan(&pool, "1", "S", &admin, &chaps(5), t0)
            .await
            .unwrap();
        assert!(
            !new_found,
            "first real scan is a baseline, not new chapters"
        );
        assert!(
            (hours_until_next_scan(&pool, "1", t0).await - 168.0).abs() < 0.01,
            "a pre-registered series' first scan schedules the STEADY interval, not the poll cadence"
        );
        assert!(
            persisted(&pool, "1").await.awaiting_since.is_none(),
            "a pre-registered series' first scan must not enter awaiting"
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

    // ── DB-driven work-selection ─────────────────────────────────────────────────

    /// Insert a scan-state row. `None` models a "due now" series — stored as the
    /// `DUE_NOW_SENTINEL` (the real representation), NOT a raw NULL, so it matches the
    /// bounded `next_scan_at <= ?` due-query like a real freshly-enrolled row.
    async fn put_state(pool: &SqlitePool, id: &str, next_scan_at: Option<&str>) {
        sqlx::query(
            "INSERT INTO series_scan_state (series_id, next_scan_at, updated_at) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(next_scan_at.unwrap_or(DUE_NOW_SENTINEL))
        .bind("2024-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap();
    }

    async fn next_scan_of(pool: &SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT next_scan_at FROM series_scan_state WHERE series_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn due_query_takes_null_and_past_orders_nulls_first_and_limits() {
        let pool = migrated_pool().await;
        let now = at("2024-01-01T00:00:00Z");
        put_state(&pool, "sentinel", None).await; // never scanned (sentinel) -> due now
        put_state(&pool, "past", Some("2023-12-31T00:00:00Z")).await; // overdue
        put_state(&pool, "future", Some("2024-02-01T00:00:00Z")).await; // not due

        let due = due_series_ids(&pool, &now.to_rfc3339(), 10).await;
        assert_eq!(
            due,
            vec!["sentinel".to_string(), "past".to_string()],
            "sentinel (due-now) sorts first, then overdue; future is excluded"
        );

        let capped = due_series_ids(&pool, &now.to_rfc3339(), 1).await;
        assert_eq!(capped, vec!["sentinel".to_string()], "LIMIT is honored");
    }

    #[tokio::test]
    async fn ensure_pending_is_due_now_and_never_clobbers() {
        let pool = migrated_pool().await;
        let now = at("2024-01-01T00:00:00Z");

        // Fresh row is due now — stored as the sentinel (never NULL) and selected by the
        // bounded due-query.
        ensure_pending(&pool, "42").await.unwrap();
        assert_eq!(
            next_scan_of(&pool, "42").await.as_deref(),
            Some(DUE_NOW_SENTINEL)
        );
        assert!(
            due_series_ids(&pool, &now.to_rfc3339(), 10)
                .await
                .contains(&"42".to_string()),
            "a sentinel row is due now"
        );

        // Park it, then re-run ensure_pending: the parked schedule must survive.
        park_paused(&pool, "42", now).await.unwrap();
        let parked = next_scan_of(&pool, "42").await;
        assert!(parked.is_some());
        ensure_pending(&pool, "42").await.unwrap();
        assert_eq!(
            next_scan_of(&pool, "42").await,
            parked,
            "ensure_pending must not disturb an existing row"
        );
    }

    #[tokio::test]
    async fn park_pushes_next_scan_out_and_drops_from_due_set() {
        let pool = migrated_pool().await;
        let now = at("2024-01-01T00:00:00Z");
        put_state(&pool, "done", None).await; // starts due
        park_paused(&pool, "done", now).await.unwrap();

        let next = next_scan_of(&pool, "done").await.unwrap();
        let hours = (at(&next) - now).num_hours();
        // Parked ~PAUSED_PARK_HOURS out, but jittered by ±(PAUSED_PARK_HOURS/10) to avoid a
        // cold-start cohort re-clustering into a thundering herd 14 days later (audit #9).
        let spread = PAUSED_PARK_HOURS / 5;
        assert!(
            (hours - PAUSED_PARK_HOURS).abs() <= spread,
            "parked ~{PAUSED_PARK_HOURS}h out (±{spread}), got {hours}h"
        );

        let due = due_series_ids(&pool, &now.to_rfc3339(), 10).await;
        assert!(
            !due.contains(&"done".to_string()),
            "parked series isn't due"
        );
    }

    async fn failures_of(pool: &SqlitePool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT consecutive_failures FROM series_scan_state WHERE series_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn record_scan_failure_backs_off_exponentially_then_resets_on_success() {
        // audit #1/#4: a failing scan must push next_scan_at out (growing) and bump the
        // failure counter, and a later success must clear the counter.
        let pool = migrated_pool().await;
        let now = at("2024-01-01T00:00:00Z");
        put_state(&pool, "dead", None).await; // due now

        record_scan_failure(&pool, "dead", now).await.unwrap();
        let after1 = (at(&next_scan_of(&pool, "dead").await.unwrap()) - now).num_minutes();
        assert_eq!(failures_of(&pool, "dead").await, 1);
        assert_eq!(
            after1, ERROR_BACKOFF_BASE_MINUTES,
            "first failure backs off base"
        );

        record_scan_failure(&pool, "dead", now).await.unwrap();
        let after2 = (at(&next_scan_of(&pool, "dead").await.unwrap()) - now).num_minutes();
        assert_eq!(failures_of(&pool, "dead").await, 2);
        assert_eq!(
            after2,
            ERROR_BACKOFF_BASE_MINUTES * 2,
            "second failure doubles"
        );

        // A successful scan resets the counter to 0.
        record_scan(&pool, "dead", "S", &ScanAdmin::default(), &chaps(3), now)
            .await
            .unwrap();
        assert_eq!(
            failures_of(&pool, "dead").await,
            0,
            "a success clears the failure streak"
        );
    }

    #[tokio::test]
    async fn backed_off_failure_no_longer_starves_a_healthy_overdue_series() {
        // audit #4: after a failure backs a dead series off into the future, a genuinely
        // overdue healthy series sorts AHEAD of it in the due-ordering (no starvation).
        let pool = migrated_pool().await;
        let now = at("2024-02-01T00:00:00Z");
        put_state(&pool, "dead", None).await; // was pinned due-now
        record_scan_failure(&pool, "dead", now).await.unwrap(); // now backed off ~30m out
        put_state(&pool, "healthy", Some("2024-01-31T00:00:00Z")).await; // overdue (past)

        let due = due_series_ids(&pool, &now.to_rfc3339(), 10).await;
        assert_eq!(
            due.first().map(String::as_str),
            Some("healthy"),
            "an overdue healthy series is no longer stuck behind the backed-off dead id"
        );
        assert!(
            !due.contains(&"dead".to_string()),
            "the backed-off dead id has left the current due-set"
        );
    }

    #[tokio::test]
    async fn park_clears_awaiting_and_failures() {
        // audit #5(low)/#9: parking a paused series must clear a stale awaiting streak and
        // reset the failure counter (the fetch that led to the park succeeded).
        let pool = migrated_pool().await;
        let now = at("2024-01-01T00:00:00Z");
        sqlx::query(
            "INSERT INTO series_scan_state \
               (series_id, next_scan_at, awaiting_since, consecutive_failures, updated_at) \
             VALUES ('p', NULL, '2023-12-01T00:00:00Z', 3, '2024-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        park_paused(&pool, "p", now).await.unwrap();

        let row = scan_state(&pool, "p").await.unwrap();
        assert!(row.awaiting_since.is_none(), "park clears awaiting_since");
        assert_eq!(failures_of(&pool, "p").await, 0, "park resets failures");
    }

    #[tokio::test]
    async fn backfill_covers_only_untracked_suwayomi_series() {
        let pool = migrated_pool().await;
        // A work for the source_series FK (foreign keys are enforced).
        sqlx::query(
            "INSERT INTO work (id, created_at, updated_at) \
             VALUES ('w', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let ins_ss = |key: &str, ty: &str| {
            let pool = pool.clone();
            let key = key.to_string();
            let ty = ty.to_string();
            async move {
                sqlx::query(
                    "INSERT INTO source_series (id, work_id, source_type, source_key, created_at) \
                     VALUES (?, 'w', ?, ?, '2024-01-01T00:00:00Z')",
                )
                .bind(format!("ss_{ty}_{key}"))
                .bind(&ty)
                .bind(&key)
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        ins_ss("100", "suwayomi").await; // untracked suwayomi -> should be backfilled
        ins_ss("200", "suwayomi").await; // already tracked (below) -> left alone
        ins_ss("300", "mangadex").await; // not suwayomi -> ignored
        park_paused(&pool, "200", at("2024-01-01T00:00:00Z"))
            .await
            .unwrap();
        let parked_200 = next_scan_of(&pool, "200").await;

        let added = crate::catalog::backfill_pending_scan_states(&pool)
            .await
            .unwrap();
        assert_eq!(added, 1, "only the untracked suwayomi series is backfilled");
        assert_eq!(
            next_scan_of(&pool, "100").await.as_deref(),
            Some(DUE_NOW_SENTINEL),
            "backfilled row is due now (sentinel, never NULL)"
        );
        assert_eq!(
            next_scan_of(&pool, "200").await,
            parked_200,
            "already-tracked series is untouched"
        );
        assert!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM series_scan_state WHERE series_id = '300'"
            )
            .fetch_one(&pool)
            .await
            .unwrap()
                == 0,
            "non-suwayomi series is never tracked"
        );
    }

    #[tokio::test]
    async fn enrol_paths_never_leave_null_next_scan_at() {
        // COMPLETENESS INVARIANT (audit #6): the due-query is a bounded `next_scan_at <= ?`,
        // so a NULL row can never match and would silently never scan. Assert the enrol
        // paths write the sentinel, never NULL, and that such rows are due now.
        let pool = migrated_pool().await;
        let now = at("2024-01-01T00:00:00Z");
        ensure_pending(&pool, "1").await.unwrap();
        ensure_pending(&pool, "2").await.unwrap();
        let nulls: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM series_scan_state WHERE next_scan_at IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(nulls, 0, "no enrolled row may have a NULL next_scan_at");
        let due = due_series_ids(&pool, &now.to_rfc3339(), 10).await;
        assert!(
            due.contains(&"1".to_string()) && due.contains(&"2".to_string()),
            "sentinel rows are due now"
        );
    }

    // Locks the due-query plan: it must use the next_scan_at index, take its ordering from
    // the index (no temp-b-tree sort), and be a bounded SEARCH (range seek) — NOT a full
    // SCAN. The bounded SEARCH is what makes the query O(due): with "due now" stored as the
    // far-past sentinel (not NULL), the `next_scan_at <= ?` range early-terminates at the
    // first future row, so future-dated rows are never visited (audit #6).
    #[tokio::test]
    async fn due_query_is_index_backed_bounded_and_unsorted() {
        use sqlx::Row as _;
        let pool = migrated_pool().await;
        let plan: Vec<String> = sqlx::query(
            "EXPLAIN QUERY PLAN SELECT series_id FROM series_scan_state \
             WHERE next_scan_at <= ? ORDER BY next_scan_at ASC LIMIT ?",
        )
        .bind("2024-01-01T00:00:00Z")
        .bind(DUE_BATCH_LIMIT)
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("detail"))
        .collect();
        let joined = plan.join(" | ");
        assert!(
            joined.contains("idx_scan_state_next_scan"),
            "due-query must use the next_scan_at index, got: {joined}"
        );
        assert!(
            !joined.to_uppercase().contains("TEMP B-TREE"),
            "due-query ordering must come from the index, not a sort, got: {joined}"
        );
        // A bounded range seek reports as SEARCH … (next_scan_at<?); a full walk reports as
        // SCAN. The `<= ?` sentinel design must yield the former (early-terminating).
        let upper = joined.to_uppercase();
        assert!(
            upper.contains("SEARCH") && !upper.contains("SCAN SERIES_SCAN_STATE"),
            "due-query must be a bounded SEARCH (range seek), not a full SCAN, got: {joined}"
        );
    }
}
