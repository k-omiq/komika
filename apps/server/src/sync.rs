//! Extension-level source sync (source-sync §5).
//!
//! A one-time bulk ingest ("add all from extension") walks a source's listing ONCE,
//! so series added to that extension *later* are never discovered. And a single-added
//! series that dropped out of the Suwayomi library stops being scanned entirely. This
//! background job closes both gaps for every *subscribed* extension
//! (`extension_subscription`, driven by the admin `setExtensionSubscription` toggle):
//!
//!   1. **Reconcile** — backfill a `series_scan_state` row for every enrolled series that
//!      lacks one (that row is what the DB-driven scanner scans from), and re-assert
//!      `inLibrary=true` upstream for enrolled series missing from the library so
//!      Suwayomi's own state stays consistent for drifted/single-added series.
//!   2. **Discover** — re-walk each subscribed extension's sources (LATEST) and
//!      auto-enrol any series we don't have yet, via the same `ingest_source_series`
//!      path the bulk ingest uses (in-library → dedup → scan-on-enrol).
//!
//! Chapter *updates* are NOT produced here: Suwayomi's source listing carries no
//! chapter list, only per-manga metadata, so new-chapter detection stays with the
//! per-series scanner (`crate::scanner`). This job's job is discovery + enrolment;
//! keeping series in the library is what lets the scanner keep them fresh.
//!
//! Mirrors the `scanner`/`ingest` background-task conventions: supervised + restarted
//! on panic, every per-item error logged and skipped, never panics the process.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};

use crate::graphql::AppState;
use crate::suwayomi::FetchType;

/// Extensions with a sync pass currently in flight — a single-flight guard so a rapid
/// re-toggle (or a subscribe kick overlapping the daily pass) can't run two concurrent
/// LATEST walks of the same source (audit LOW). Enrolment is idempotent, so the harm is
/// only redundant upstream fetches, but skipping the overlap is cheap.
fn running_pkgs() -> &'static std::sync::Mutex<HashSet<String>> {
    static S: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    S.get_or_init(|| std::sync::Mutex::new(HashSet::new()))
}

/// RAII single-flight token for one extension's sync pass; releases on drop (incl. panic).
struct PkgGuard(String);
impl PkgGuard {
    fn try_acquire(pkg: &str) -> Option<PkgGuard> {
        let mut set = running_pkgs().lock().unwrap_or_else(|e| e.into_inner());
        set.insert(pkg.to_string())
            .then(|| PkgGuard(pkg.to_string()))
    }
}
impl Drop for PkgGuard {
    fn drop(&mut self) {
        running_pkgs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Polite throttle between browse pages — each page is one upstream fetch.
const PAGE_DELAY_MS: u64 = 750;
/// Polite throttle between reconcile `set_in_library` heal calls, so a first-run mass
/// drift can't fire thousands of sequential upstream writes back-to-back.
const HEAL_DELAY_MS: u64 = 50;
/// Stop a source's LATEST walk after this many CONSECUTIVE fully-already-known pages.
/// LATEST is newest/recently-updated first, so a run of all-known pages means we've
/// caught up; the `max_pages` cap still bounds the walk when this never trips.
const STOP_AFTER_KNOWN_PAGES: u32 = 2;

/// Spawn the source-sync scheduler. Runs until `shutdown` resolves. Supervised: a
/// panic in the tick loop is logged and the loop restarts after a short backoff.
pub fn spawn(
    state: Arc<AppState>,
    interval_seconds: u64,
    max_pages: i64,
    shutdown: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        loop {
            let handle = tokio::spawn(run_loop(
                state.clone(),
                interval_seconds,
                max_pages,
                shutdown.clone(),
            ));
            match handle.await {
                Ok(()) => break,
                Err(e) if e.is_panic() => {
                    if *shutdown.borrow() {
                        break;
                    }
                    tracing::error!("source-sync loop panicked; restarting in 30s");
                    tokio::time::sleep(Duration::from_secs(30)).await;
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
    interval_seconds: u64,
    max_pages: i64,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut ticker = interval(Duration::from_secs(interval_seconds));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tracing::info!(interval_seconds, max_pages, "source-sync scheduler started");
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                // `interval` fires its first tick immediately, so this also runs on every
                // restart. Skip the pass when a full one completed less than the interval
                // ago, so frequent redeploys don't re-run the upstream-heavy reconcile +
                // LATEST walks on every boot (audit #3). Scheduled ticks are `interval`
                // apart, so they're always due; only the redundant restart tick is skipped.
                if !crate::catalog::source_sync_due(&state.pool, interval_seconds).await {
                    tracing::info!("source-sync: a recent pass exists — skipping restart tick");
                    continue;
                }
                let (subs, added) = sync_all(&state, max_pages).await;
                tracing::info!(subscriptions = subs, series_added = added, "source-sync pass complete");
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("source-sync scheduler stopping");
                    break;
                }
            }
        }
    }
}

/// One full pass: refresh extension coordinates, reconcile library membership + scan
/// tracking, then discover new series for every subscribed extension. Returns
/// `(subscriptions_processed, series_added)`.
pub async fn sync_all(state: &AppState, max_pages: i64) -> (usize, i64) {
    // Extension coordinates change rarely; refresh them here (daily) rather than on
    // every scan tick. Non-fatal.
    crate::scanner::record_source_extensions(state).await;
    let reconciled = reconcile_library(state).await;

    let subs = match crate::catalog::subscribed_extensions(&state.pool).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "source-sync: failed to list subscriptions");
            return (0, 0);
        }
    };
    let mut total_added = 0i64;
    for pkg in &subs {
        total_added += sync_extension(state, pkg, max_pages).await;
    }
    // Stamp completion only when the reconcile actually SUCCEEDED, so a pass aborted by a
    // transient upstream/DB failure (library fetch down, couldn't enumerate enrolled) does
    // NOT throttle the next attempt as if it had finished — a restart then retries instead
    // of waiting a full interval (audit #3 follow-up). Stamped LAST, after the walks.
    if reconciled {
        if let Err(e) = crate::catalog::mark_source_sync_pass(&state.pool).await {
            tracing::warn!(error = %e, "source-sync: failed to record pass completion");
        }
    } else {
        tracing::info!("source-sync: reconcile incomplete — not stamping pass (will retry)");
    }
    (subs.len(), total_added)
}

/// Re-assert `inLibrary=true` upstream for every enrolled Suwayomi series that is NOT
/// currently in the library, and backfill scan-state rows. Runs once per interval
/// regardless of subscriptions (it heals single-adds), and is throttled against restarts
/// by `source_sync_due`. Cheap in steady state — after the first heal the "missing" set is
/// empty, so no upstream writes happen. The membership fetch is the paginated, id-only
/// `library_ids()` (bounded pages, no 100k-record materialisation). Scan eligibility now
/// flows from the backfilled `series_scan_state` row, NOT
/// from `library()` membership (the DB-driven scanner no longer iterates the library);
/// the in-library re-assert keeps Suwayomi's own state consistent.
/// Returns whether the reconcile completed (so `sync_all` only stamps a genuinely finished
/// pass). `false` = a transient failure (couldn't enumerate enrolled series, or the library
/// fetch failed) aborted it partway and it should be retried, not throttled.
async fn reconcile_library(state: &AppState) -> bool {
    // Backfill a "due now" scan-state row for any enrolled series that lacks one (e.g.
    // federated-search enrolments, which don't scan-on-enrol) so the DB-driven scanner
    // tracks them. One set-based query; cheap and idempotent.
    match crate::catalog::backfill_pending_scan_states(&state.pool).await {
        Ok(0) => {}
        Ok(n) => tracing::info!(added = n, "source-sync: backfilled scan-state rows"),
        Err(e) => tracing::warn!(error = %e, "source-sync: scan-state backfill failed"),
    }

    let enrolled = match crate::catalog::suwayomi_source_keys(&state.pool).await {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "source-sync: failed to list enrolled series");
            return false;
        }
    };
    if enrolled.is_empty() {
        return true;
    }
    // Paginated, id-only fetch — reconcile only needs the membership set, so don't
    // materialise 100k+ full manga records (audit #5).
    let in_library: HashSet<String> = match state.suwayomi.library_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "source-sync: failed to list library for reconcile");
            return false;
        }
    };
    let mut healed = 0usize;
    for key in enrolled {
        if in_library.contains(&key) {
            continue;
        }
        let Ok(id) = key.parse::<i64>() else { continue };
        match state.suwayomi.set_in_library(id, true).await {
            Ok(()) => healed += 1,
            Err(e) => {
                tracing::warn!(series_id = id, error = %e, "source-sync: reconcile set_in_library failed")
            }
        }
        // Throttle the heal like the LATEST walk: a first-run mass drift could otherwise
        // fire thousands of sequential `set_in_library` writes back-to-back at the engine
        // (audit LOW). Steady state the missing set is empty, so this never runs.
        tokio::time::sleep(Duration::from_millis(HEAL_DELAY_MS)).await;
    }
    if healed > 0 {
        tracing::info!(
            healed,
            "source-sync: re-enrolled drifted series into the library"
        );
    }
    true
}

/// Discover + auto-enrol new series for ONE extension, across all its sources. Records
/// the pass outcome on the subscription row. Returns the number of series enrolled.
pub async fn sync_extension(state: &AppState, pkg_name: &str, max_pages: i64) -> i64 {
    // Single-flight per extension (audit LOW): a concurrent pass for the same pkg (rapid
    // re-toggle, or a subscribe kick landing during the daily pass) just skips.
    let Some(_guard) = PkgGuard::try_acquire(pkg_name) else {
        tracing::info!(
            pkg = pkg_name,
            "source-sync: extension already syncing — skipping"
        );
        return 0;
    };
    let sources = match state.suwayomi.list_sources().await {
        Ok(list) => list
            .into_iter()
            .filter(|s| s.pkg_name.as_deref() == Some(pkg_name))
            .map(|s| s.id)
            .collect::<Vec<_>>(),
        Err(e) => {
            let msg = format!("list_sources failed: {e}");
            tracing::warn!(pkg = pkg_name, error = %e, "source-sync: list_sources failed");
            let _ = crate::catalog::mark_subscription_synced(&state.pool, pkg_name, 0, Some(&msg))
                .await;
            return 0;
        }
    };

    let mut added = 0i64;
    let mut error: Option<String> = None;
    for source_id in &sources {
        match sync_source_latest(state, source_id, max_pages).await {
            Ok(n) => added += n,
            Err(e) => {
                tracing::warn!(pkg = pkg_name, source_id, error = %e, "source-sync: source walk failed");
                error = Some(format!("source {source_id}: {e}"));
            }
        }
    }
    if let Err(e) =
        crate::catalog::mark_subscription_synced(&state.pool, pkg_name, added, error.as_deref())
            .await
    {
        tracing::warn!(pkg = pkg_name, error = %e, "source-sync: failed to record pass outcome");
    }
    if added > 0 {
        tracing::info!(pkg = pkg_name, added, "source-sync: enrolled new series");
    }
    added
}

/// Walk one source's LATEST listing, auto-enrolling every series we don't yet have.
/// Stops after `STOP_AFTER_KNOWN_PAGES` consecutive fully-known pages (caught up), when
/// the source reports no next page, or at the `max_pages` cap (logged — a capped sweep
/// is not "fully caught up").
async fn sync_source_latest(
    state: &AppState,
    source_id: &str,
    max_pages: i64,
) -> anyhow::Result<i64> {
    let mut added = 0i64;
    let mut page: i32 = 1;
    let mut consecutive_known = 0u32;
    loop {
        let (has_next, mangas) = state
            .suwayomi
            .browse_source(source_id, FetchType::Latest, page, None)
            .await?;
        if mangas.is_empty() {
            break;
        }

        // Which of this page's series are not yet enrolled? (Suwayomi manga ids are the
        // `source_key` and globally unique, so a key-only lookup is unambiguous.) One
        // set-based query per page instead of a lookup per manga (audit #10).
        let keys: Vec<String> = mangas.iter().map(|m| m.id.to_string()).collect();
        let known = crate::catalog::existing_source_keys(&state.pool, "suwayomi", &keys).await?;
        let new_ids: Vec<i64> = mangas
            .iter()
            .filter(|m| !known.contains(&m.id.to_string()))
            .map(|m| m.id)
            .collect();

        if new_ids.is_empty() {
            consecutive_known += 1;
            if consecutive_known >= STOP_AFTER_KNOWN_PAGES {
                break;
            }
        } else {
            consecutive_known = 0;
            // Enrol sequentially — discovery volume per pass is small (only genuinely
            // new series), and this keeps the upstream engine gently loaded. Each
            // `ingest_source_series` sets in-library, dedups, and scan-on-enrols.
            for id in new_ids {
                match crate::graphql::ingest_source_series(state, &id.to_string()).await {
                    Ok(_) => added += 1,
                    Err(e) => {
                        tracing::warn!(source_id, manga_id = id, error = %e, "source-sync: enrol failed")
                    }
                }
            }
        }

        if !has_next {
            break;
        }
        if page as i64 >= max_pages {
            tracing::info!(
                source_id,
                max_pages,
                "source-sync: hit page cap — sweep truncated, not necessarily fully caught up"
            );
            break;
        }
        page += 1;
        tokio::time::sleep(Duration::from_millis(PAGE_DELAY_MS)).await;
    }
    Ok(added)
}

/// Fire-and-forget a single-extension sync (the `setExtensionSubscription` enable path),
/// so the admin sees discovery results without waiting for the next scheduler tick.
/// `SOURCE_SYNC_MAX_PAGES` isn't threaded here; a modest cap keeps this interactive kick
/// bounded (a fuller sweep runs on the next scheduled pass).
pub fn spawn_extension_sync(state: Arc<AppState>, pkg_name: String) {
    tokio::spawn(async move {
        let added = sync_extension(&state, &pkg_name, DEFAULT_KICK_MAX_PAGES).await;
        tracing::info!(pkg = %pkg_name, added, "source-sync: on-subscribe kick complete");
    });
}

/// Page cap for the on-subscribe interactive kick (see `spawn_extension_sync`).
const DEFAULT_KICK_MAX_PAGES: i64 = 5;
