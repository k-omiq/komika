//! DB-backed cover cache for canonical works.
//!
//! The web reader normally routes every cover through the Cloudflare image Worker
//! (CORS). To take covers off the Worker, we fetch each work's MangaDex cover
//! once, re-encode it to a bounded lossless WebP, and store the bytes in
//! `work_cover_blob` (Litestream-replicated, like `user_avatars`). The reader
//! then loads covers from our own origin at `/covers/{work_id}.webp` and the
//! Worker only ever sees chapter PAGES.
//!
//! Presence is signalled by `work.cover_cached_version` (NULL = not cached), a
//! column that rides on the `work` row already loaded on hot paths, so cover-URL
//! resolution costs no extra query.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::Utc;
use image::{imageops, GenericImageView};
use sqlx::SqlitePool;

use crate::avatar::{decode_limited, encode_webp_lossy};
use crate::mangadex::MangaDexClient;

/// Reject raw source images larger than this before decoding (decode-bomb guard).
/// Raised from 8 MB: some source thumbnails (a handful of Suwayomi scanlator
/// covers) ship 8–20 MB originals that decode fine and downscale to a tiny WebP —
/// the old cap rejected them outright. The real bomb guard is `decode_limited`'s
/// pixel-dimension + allocation ceiling, which still applies after this gate.
pub const MAX_SOURCE_BYTES: usize = 24 * 1024 * 1024;

/// Per-cover size budget for the stored WebP — the "100–150 KB" target. Lossy
/// encoding (below) keeps detailed art sharp at 512px while landing under this,
/// so the DB blob store + its Litestream replication stay cheap. Down from the old
/// 200 KB lossless budget.
pub const MAX_COVER_BYTES: usize = 150 * 1024;

/// Candidate longest-edge lengths (px), largest first — the portrait-preserving
/// dimension bound. With LOSSY encoding the quality ladder does most of the size
/// control, so the top edge (512) almost always wins; the smaller edges are a
/// fallback for pathological sources that won't fit even at low quality.
const CANDIDATE_EDGES: [u32; 3] = [512, 448, 384];

/// Lossy WebP quality ladder (libwebp scale, 0–100), highest first. The first
/// (edge, quality) pair encoding within [`MAX_COVER_BYTES`] wins, so sharpness is
/// preserved by trying high quality before stepping down.
const COVER_QUALITY_LADDER: [f32; 5] = [82.0, 74.0, 66.0, 58.0, 50.0];

/// Decode an upstream cover, downscale (aspect-preserving, never upscaling) to a
/// bounded longest edge, and re-encode as LOSSY WebP within [`MAX_COVER_BYTES`].
/// Tries each edge at descending quality and returns the first result under budget.
pub fn process_cover(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        bail!("empty cover source");
    }
    if bytes.len() > MAX_SOURCE_BYTES {
        bail!(
            "cover source too large (max {} MB)",
            MAX_SOURCE_BYTES / (1024 * 1024)
        );
    }
    let img = decode_limited(bytes)?;
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        bail!("cover has a zero dimension");
    }
    let long = w.max(h).max(1);
    let rgba = img.to_rgba8();

    let mut smallest: Option<Vec<u8>> = None;
    for edge in CANDIDATE_EDGES {
        // Downscale only — a source already smaller than the candidate keeps its
        // size (scale clamped to 1.0) instead of being blurrily enlarged.
        let scale = (edge as f32 / long as f32).min(1.0);
        let resized = if scale >= 1.0 {
            rgba.clone()
        } else {
            let tw = ((w as f32 * scale).round() as u32).max(1);
            let th = ((h as f32 * scale).round() as u32).max(1);
            imageops::resize(&rgba, tw, th, imageops::FilterType::Lanczos3)
        };
        for &quality in &COVER_QUALITY_LADDER {
            let encoded = encode_webp_lossy(&resized, quality)?;
            if encoded.len() <= MAX_COVER_BYTES {
                return Ok(encoded);
            }
            // Keep the smallest produced (smallest edge, lowest quality) as the
            // last-resort fallback if nothing fits the budget.
            smallest = Some(encoded);
        }
    }
    smallest.ok_or_else(|| anyhow!("no candidate cover size produced"))
}

/// Read a resized, origin-cached Suwayomi source cover (keyed by numeric manga id),
/// or `None` if it hasn't been materialized yet. Errors are swallowed to `None` so a
/// transient DB hiccup falls back to a live resize rather than failing the request.
pub async fn get_suwayomi_cover(covers: &SqlitePool, manga_id: i64) -> Option<Vec<u8>> {
    sqlx::query_scalar("SELECT webp FROM suwayomi_cover_blob WHERE manga_id = ?")
        .bind(manga_id)
        .fetch_optional(covers)
        .await
        .ok()
        .flatten()
}

/// Store (or replace) a resized Suwayomi source cover in the un-replicated covers DB.
pub async fn put_suwayomi_cover(covers: &SqlitePool, manga_id: i64, webp: &[u8]) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO suwayomi_cover_blob (manga_id, webp, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(manga_id) DO UPDATE SET webp = excluded.webp, updated_at = excluded.updated_at",
    )
    .bind(manga_id)
    .bind(webp)
    .bind(&ts)
    .execute(covers)
    .await?;
    Ok(())
}

/// The public path stored/served for a cached work cover, cache-busted with
/// `?v=<version>` so the browser + edge refetch after a re-save. The
/// `/covers/{file}` route reads the bytes from `work_cover_blob` keyed by
/// `<work_id>`.
pub fn cover_path(work_id: &str, version: i64) -> String {
    format!("/covers/{work_id}.webp?v={version}")
}

/// Resolve the cover URL for a work. Cached ⇒ the versioned VPS blob path. Uncached
/// but cover-able (has a MangaDex anchor) ⇒ our own `/covers/{id}.webp` route, which
/// lazily fetches + caches the MangaDex cover on first request (and 302-falls back to
/// the CDN if that fails) — so covers land on our origin AS THEY'RE VIEWED, not only
/// via the slow background drainer. No anchor ⇒ empty. This is the single seam every
/// cover-URL site uses so the web/VPS split lives in one place.
pub fn work_cover_url(
    work_id: &str,
    cached_version: Option<i64>,
    mangadex_id: Option<&str>,
    cover_file_name: Option<&str>,
) -> String {
    if let Some(v) = cached_version {
        return cover_path(work_id, v);
    }
    match (mangadex_id, cover_file_name) {
        (Some(mid), Some(fname)) if !mid.is_empty() && !fname.is_empty() => {
            format!("/covers/{work_id}.webp")
        }
        _ => String::new(),
    }
}

/// The MangaDex anchor (`(source_key, cover_file_name)`) for a work — the oldest
/// MangaDex `source_series` (the same anchor the reader/drainer picks) plus the work's
/// cover file name. `None` if the work has no MangaDex source or no cover file name, in
/// which case there's nothing to lazily fetch. Used by `serve_cover`'s lazy cache.
pub async fn mangadex_cover_anchor(main: &SqlitePool, work_id: &str) -> Option<(String, String)> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT ss.source_key, w.cover_file_name \
         FROM work w \
         JOIN source_series ss ON ss.work_id = w.id AND ss.source_type = 'mangadex' \
         WHERE w.id = ? \
         ORDER BY ss.created_at ASC, ss.id ASC LIMIT 1",
    )
    .bind(work_id)
    .fetch_optional(main)
    .await
    .ok()
    .flatten()?;
    match row {
        (Some(mid), Some(fname)) if !mid.is_empty() && !fname.is_empty() => Some((mid, fname)),
        _ => None,
    }
}

/// Store (or replace) a work's cover blob (in the separate `covers` DB) and flip
/// `work.cover_cached_version` (in the `main` DB) to the new version. The version
/// is a wall-clock timestamp — monotonic enough to bust caches on every re-save.
///
/// The blob and its version pointer live in DIFFERENT databases (covers is
/// un-replicated; see `db::init_covers`), so a single cross-DB transaction is
/// impossible. We preserve the invariant "`cover_cached_version` set ⇒ blob exists"
/// by ORDERING: write the blob first, set the pointer second. A crash in between
/// leaves a blob with no pointer — harmless: the resolver falls back to the proxy
/// URL, and the drainer re-runs (it selects `cover_cached_version IS NULL`) and
/// re-sets the pointer. The reverse order could point at a missing blob → 404.
pub async fn put_work_cover(
    main: &SqlitePool,
    covers: &SqlitePool,
    work_id: &str,
    webp: &[u8],
) -> Result<()> {
    let now = Utc::now();
    let version = now.timestamp();
    let ts = now.to_rfc3339();
    // 1) Blob first, into the un-replicated covers DB.
    sqlx::query(
        "INSERT INTO work_cover_blob (work_id, webp, version, updated_at) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(work_id) DO UPDATE SET \
           webp = excluded.webp, version = excluded.version, updated_at = excluded.updated_at",
    )
    .bind(work_id)
    .bind(webp)
    .bind(version)
    .bind(&ts)
    .execute(covers)
    .await?;
    // 2) Version pointer second, into the main (replicated) DB.
    sqlx::query("UPDATE work SET cover_cached_version = ? WHERE id = ?")
        .bind(version)
        .bind(work_id)
        .execute(main)
        .await?;
    Ok(())
}

/// A work still missing a cached cover, with the MangaDex anchor needed to fetch
/// one.
struct PendingCover {
    work_id: String,
    mangadex_id: String,
    file_name: String,
}

/// How many canonical works still need a cover materialized (cacheable = has a
/// MangaDex anchor + cover fileName, no blob yet). Drives the admin button's
/// "queued N" feedback.
pub async fn pending_cover_count(pool: &SqlitePool) -> Result<i64> {
    // Uncached works we can materialize a cover for: a MangaDex-anchored work with a
    // cover file name, OR a Suwayomi-anchored work (cover comes from its thumbnail).
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM work w \
         WHERE w.cover_cached_version IS NULL AND ( \
             (w.cover_file_name IS NOT NULL \
              AND EXISTS (SELECT 1 FROM source_series ss \
                          WHERE ss.work_id = w.id AND ss.source_type = 'mangadex')) \
             OR EXISTS (SELECT 1 FROM source_series ss \
                        WHERE ss.work_id = w.id AND ss.source_type = 'suwayomi'))",
    )
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Map a `process_cover` / store error to a stable machine reason code for
/// `work_cover_issue.reason` (surfaced in the admin panel).
pub fn classify_cover_error(err: &anyhow::Error) -> &'static str {
    let s = err.to_string().to_ascii_lowercase();
    if s.contains("source too large") {
        "too_large"
    } else if s.contains("unsupported") || s.contains("corrupt") || s.contains("zero dimension") {
        "unsupported"
    } else if s.contains("empty") {
        "empty"
    } else {
        "encode"
    }
}

/// Record (or refresh) a cover-processing failure so the crawl stops re-attempting
/// a deterministically-unprocessable work every tick. Best-effort — a failure to
/// write the marker only means we retry next tick, never breaks the crawl.
pub async fn record_cover_issue(pool: &SqlitePool, work_id: &str, reason: &str, detail: &str) {
    let now = Utc::now().to_rfc3339();
    // Truncate an over-long error so a pathological message can't bloat the row.
    let detail: String = detail.chars().take(500).collect();
    if let Err(e) = sqlx::query(
        "INSERT INTO work_cover_issue (work_id, reason, detail, attempts, first_seen, last_seen) \
         VALUES (?, ?, ?, 1, ?, ?) \
         ON CONFLICT(work_id) DO UPDATE SET \
           reason = excluded.reason, detail = excluded.detail, \
           attempts = work_cover_issue.attempts + 1, last_seen = excluded.last_seen",
    )
    .bind(work_id)
    .bind(reason)
    .bind(&detail)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    {
        tracing::warn!(work_id, error = %e, "cover issue: failed to record");
    }
}

/// Clear a work's recorded cover issue (a later attempt — auto or manual — succeeded,
/// so it should re-enter the normal cached/uncached flow). Best-effort.
pub async fn clear_cover_issue(pool: &SqlitePool, work_id: &str) {
    if let Err(e) = sqlx::query("DELETE FROM work_cover_issue WHERE work_id = ?")
        .bind(work_id)
        .execute(pool)
        .await
    {
        tracing::warn!(work_id, error = %e, "cover issue: failed to clear");
    }
}

/// Attempt to materialize ONE work's cover on demand (admin "retry" action). Tries
/// the MangaDex anchor first, then the Suwayomi thumbnail. Clears the issue on
/// success; re-records it (refreshed reason) on a deterministic failure; leaves it
/// untouched on a transient upstream-fetch failure. Returns Ok(true) when a cover
/// was stored, Ok(false) when neither source yielded processable bytes.
pub async fn retry_one_cover(
    main: &SqlitePool,
    covers: &SqlitePool,
    mangadex: &MangaDexClient,
    suwayomi: &crate::suwayomi::SuwayomiClient,
    work_id: &str,
) -> Result<bool> {
    // MangaDex anchor (oldest source_series) + cover file name, mirroring the crawl.
    let md = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT (SELECT ss.source_key FROM source_series ss \
                 WHERE ss.work_id = w.id AND ss.source_type = 'mangadex' \
                 ORDER BY ss.created_at ASC, ss.id ASC LIMIT 1), \
                w.cover_file_name \
         FROM work w WHERE w.id = ?",
    )
    .bind(work_id)
    .fetch_optional(main)
    .await?;

    // Collect candidate source byte-fetches in priority order.
    let mut source_bytes: Option<Vec<u8>> = None;
    if let Some((Some(mangadex_id), Some(file_name))) = &md {
        if !mangadex_id.is_empty() {
            source_bytes = mangadex.cover_thumb_bytes(mangadex_id, file_name).await;
        }
    }
    if source_bytes.is_none() {
        // Fall back to the Suwayomi thumbnail anchor.
        let thumb = sqlx::query_scalar::<_, Option<String>>(
            "SELECT s.thumbnail_url FROM source_series ss \
             JOIN suwayomi_series s ON CAST(s.id AS TEXT) = ss.source_key \
             WHERE ss.work_id = ? AND ss.source_type = 'suwayomi' \
             ORDER BY ss.created_at ASC, ss.id ASC LIMIT 1",
        )
        .bind(work_id)
        .fetch_optional(main)
        .await?
        .flatten();
        if let Some(t) = thumb.filter(|t| !t.is_empty()) {
            source_bytes = suwayomi.cover_bytes(Some(&t)).await;
        }
    }

    let Some(bytes) = source_bytes else {
        // Transient — couldn't fetch a source. Leave any existing issue as-is.
        return Ok(false);
    };

    let work_id_owned = work_id.to_string();
    match tokio::task::spawn_blocking(move || process_cover(&bytes)).await {
        Ok(Ok(webp)) => {
            put_work_cover(main, covers, work_id, &webp).await?;
            clear_cover_issue(main, work_id).await;
            Ok(true)
        }
        Ok(Err(e)) => {
            record_cover_issue(main, work_id, classify_cover_error(&e), &e.to_string()).await;
            Err(anyhow!("cover processing failed: {e}"))
        }
        Err(e) => {
            let _ = work_id_owned;
            Err(anyhow!("cover processing task failed: {e}"))
        }
    }
}

/// Fetch + store covers for EVERY canonical work that still lacks one (bounded by
/// the MangaDex rate limiter inside `cover_thumb_bytes`, so this is a polite,
/// slow background crawl). Idempotent and resumable: it only ever selects works
/// with `cover_cached_version IS NULL` (and no recorded `work_cover_issue`), so a
/// re-run drains whatever remains (e.g. works whose upstream fetch failed last
/// time). Best-effort per work — one failure never aborts the crawl.
pub async fn crawl_uncached_covers(
    main: &SqlitePool,
    covers: &SqlitePool,
    mangadex: &MangaDexClient,
    suwayomi: &crate::suwayomi::SuwayomiClient,
    limit: Option<i64>,
) {
    // The mangadex anchor lives on `source_series`, not `work`; pick the same
    // deterministic anchor the reader uses (oldest source_series) so the cover
    // matches the pages. `limit` bounds a single (auto-drainer) tick; None = drain
    // everything still uncached (the manual admin crawl).
    const BASE: &str = "SELECT w.id, \
                (SELECT ss.source_key FROM source_series ss \
                 WHERE ss.work_id = w.id AND ss.source_type = 'mangadex' \
                 ORDER BY ss.created_at ASC, ss.id ASC LIMIT 1) AS mangadex_id, \
                w.cover_file_name \
         FROM work w \
         WHERE w.cover_cached_version IS NULL AND w.cover_file_name IS NOT NULL \
           AND w.id NOT IN (SELECT work_id FROM work_cover_issue)";
    let loaded = match limit {
        Some(n) => {
            sqlx::query_as::<_, (String, Option<String>, Option<String>)>(&format!(
                "{BASE} LIMIT ?"
            ))
            .bind(n)
            .fetch_all(main)
            .await
        }
        None => {
            sqlx::query_as::<_, (String, Option<String>, Option<String>)>(BASE)
                .fetch_all(main)
                .await
        }
    };
    let pending = match loaded {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "cover crawl: failed to load pending works");
            return;
        }
    };

    let jobs: Vec<PendingCover> = pending
        .into_iter()
        .filter_map(|(work_id, mid, fname)| match (mid, fname) {
            (Some(mangadex_id), Some(file_name)) if !mangadex_id.is_empty() => Some(PendingCover {
                work_id,
                mangadex_id,
                file_name,
            }),
            _ => None,
        })
        .collect();

    let total = jobs.len();
    tracing::info!(total, "cover crawl: starting");
    let mut saved = 0usize;
    let mut failed = 0usize;
    for job in jobs {
        match mangadex
            .cover_thumb_bytes(&job.mangadex_id, &job.file_name)
            .await
        {
            Some(bytes) => match process_cover(&bytes) {
                Ok(webp) => match put_work_cover(main, covers, &job.work_id, &webp).await {
                    Ok(()) => {
                        saved += 1;
                        clear_cover_issue(main, &job.work_id).await;
                    }
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(work_id = %job.work_id, error = %e, "cover crawl: store failed");
                        record_cover_issue(main, &job.work_id, "store", &e.to_string()).await;
                    }
                },
                // Deterministic decode/encode/size failure — record it so the crawl
                // stops re-attempting this work every tick (it would fail identically).
                Err(e) => {
                    failed += 1;
                    tracing::warn!(work_id = %job.work_id, error = %e, "cover crawl: encode failed");
                    record_cover_issue(main, &job.work_id, classify_cover_error(&e), &e.to_string())
                        .await;
                }
            },
            // Upstream fetch failure is TRANSIENT (rate limit / network) — do NOT
            // record an issue, so the next tick retries it.
            None => {
                failed += 1;
                tracing::warn!(work_id = %job.work_id, "cover crawl: upstream fetch failed");
            }
        }
    }
    tracing::info!(saved, failed, total, "cover crawl: complete");

    // Second pass: works with no MangaDex cover but anchored to a Suwayomi source
    // (the bulk of the scanlator/MangaDex-via-Suwayomi catalogue). Materialize their
    // cover from the Suwayomi thumbnail into the SAME work_cover_blob, so they too
    // serve from our own `/covers/{work_id}.webp` (immutable, CDN-cacheable) instead
    // of a live proxy to the Suwayomi engine on every request. Deterministic anchor:
    // the oldest suwayomi source_series (matches the reader's source pick).
    const SUW_BASE: &str = "SELECT w.id, \
                (SELECT s.thumbnail_url FROM source_series ss \
                 JOIN suwayomi_series s ON CAST(s.id AS TEXT) = ss.source_key \
                 WHERE ss.work_id = w.id AND ss.source_type = 'suwayomi' \
                 ORDER BY ss.created_at ASC, ss.id ASC LIMIT 1) AS thumbnail_url \
         FROM work w \
         WHERE w.cover_cached_version IS NULL \
           AND w.id NOT IN (SELECT work_id FROM work_cover_issue) \
           AND EXISTS (SELECT 1 FROM source_series ss WHERE ss.work_id = w.id \
                       AND ss.source_type = 'suwayomi')";
    let suw_loaded = match limit {
        Some(n) => {
            sqlx::query_as::<_, (String, Option<String>)>(&format!("{SUW_BASE} LIMIT ?"))
                .bind(n)
                .fetch_all(main)
                .await
        }
        None => {
            sqlx::query_as::<_, (String, Option<String>)>(SUW_BASE)
                .fetch_all(main)
                .await
        }
    };
    let suw_jobs: Vec<(String, String)> = match suw_loaded {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|(work_id, thumb)| {
                thumb.filter(|t| !t.is_empty()).map(|t| (work_id, t))
            })
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "cover crawl (suwayomi): failed to load pending works");
            return;
        }
    };
    let suw_total = suw_jobs.len();
    tracing::info!(total = suw_total, "cover crawl (suwayomi): starting");
    // OPT-9: run this pass with bounded concurrency instead of one-at-a-time, and move
    // the CPU-bound `process_cover` (decode + WebP re-encode) off the reactor via
    // spawn_blocking. A small fan-out overlaps the network waits without flooding the
    // Suwayomi engine or contending hard with the DB writer during a seed. Each item
    // yields (saved, failed) which we sum after the stream drains.
    use futures::StreamExt as _;
    const SUW_CRAWL_CONCURRENCY: usize = 5;
    let (suw_saved, suw_failed) = futures::stream::iter(suw_jobs)
        .map(|(work_id, thumb)| async move {
            let bytes = match suwayomi.cover_bytes(Some(&thumb)).await {
                Some(b) => b,
                None => {
                    tracing::warn!(work_id = %work_id, "cover crawl (suwayomi): upstream fetch failed");
                    return (0usize, 1usize);
                }
            };
            match tokio::task::spawn_blocking(move || process_cover(&bytes)).await {
                Ok(Ok(webp)) => match put_work_cover(main, covers, &work_id, &webp).await {
                    Ok(()) => {
                        clear_cover_issue(main, &work_id).await;
                        (1, 0)
                    }
                    Err(e) => {
                        tracing::warn!(work_id = %work_id, error = %e, "cover crawl (suwayomi): store failed");
                        record_cover_issue(main, &work_id, "store", &e.to_string()).await;
                        (0, 1)
                    }
                },
                // Deterministic — record so we stop re-attempting an unprocessable source.
                Ok(Err(e)) => {
                    tracing::warn!(work_id = %work_id, error = %e, "cover crawl (suwayomi): encode failed");
                    record_cover_issue(main, &work_id, classify_cover_error(&e), &e.to_string()).await;
                    (0, 1)
                }
                Err(e) => {
                    // A spawn_blocking JoinError (panic) is not necessarily deterministic;
                    // don't permanently skip on it — let a later tick retry.
                    tracing::warn!(work_id = %work_id, error = %e, "cover crawl (suwayomi): encode task panicked");
                    (0, 1)
                }
            }
        })
        .buffer_unordered(SUW_CRAWL_CONCURRENCY)
        .fold((0usize, 0usize), |(s, f), (ds, df)| async move { (s + ds, f + df) })
        .await;
    tracing::info!(
        saved = suw_saved,
        failed = suw_failed,
        total = suw_total,
        "cover crawl (suwayomi): complete"
    );
}

/// Recurring background drainer that keeps the cover cache full with NO manual
/// trigger: every `interval_secs` it fetches + stores up to `batch` still-uncached
/// covers, so works added by catalogue sync / ingest get their cover cached
/// automatically within an interval or two. The first tick fires immediately on
/// startup. Batches are bounded so a tick stays short (polite + responsive to
/// shutdown), and it shares `inflight` with the manual admin crawl so the two
/// never hammer MangaDex at once. Mirrors `graphql::spawn_metadata_backfill`.
///
/// FLEET CONSTRAINT: like the catalogue sync / metadata backfill, this hits
/// MangaDex under the in-process rate limiter, so run it on exactly ONE replica.
pub fn spawn(
    main: SqlitePool,
    covers: SqlitePool,
    mangadex: Arc<MangaDexClient>,
    suwayomi: crate::suwayomi::SuwayomiClient,
    inflight: Arc<AtomicBool>,
    interval_secs: u64,
    batch: i64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(interval_secs, batch, "cover cache drainer started");
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    // Yield to a manual full crawl if one is in flight (don't stack
                    // two MangaDex crawls); we'll pick up the remainder next tick.
                    if inflight
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        continue;
                    }
                    match pending_cover_count(&main).await {
                        Ok(0) => { /* nothing to do this tick */ }
                        Ok(_) => crawl_uncached_covers(&main, &covers, &mangadex, &suwayomi, Some(batch)).await,
                        Err(e) => tracing::warn!(error = %e, "cover drainer: count failed"),
                    }
                    inflight.store(false, Ordering::SeqCst);
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("cover cache drainer stopping");
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
    use image::{DynamicImage, RgbImage};

    fn noisy_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let r = ((x * 73 + y * 151) % 256) as u8;
            let g = ((x * 199 + y * 37) % 256) as u8;
            let b = ((x ^ (y.wrapping_mul(101))) % 256) as u8;
            *px = image::Rgb([r, g, b]);
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    /// P0.5 opt-level bench (not part of the 191-test suite — `#[ignore]`d).
    /// Times the two dominant CPU hot paths on a realistic high-entropy cover
    /// (forces the full 5-edge resize+encode search — the worst case). Run under
    /// the release profile so it reflects the shipped binary; toggle
    /// `[profile.release] opt-level` between runs to compare:
    ///   cargo test --release --bin komika-server opt_level_hot_path_bench \
    ///     -- --ignored --nocapture
    #[test]
    #[ignore]
    fn opt_level_hot_path_bench() {
        use std::time::Instant;

        fn stats(label: &str, mut ns: Vec<u128>) {
            ns.sort_unstable();
            let n = ns.len();
            let median = ns[n / 2] as f64 / 1e6;
            let min = ns[0] as f64 / 1e6;
            let mean = ns.iter().sum::<u128>() as f64 / n as f64 / 1e6;
            eprintln!(
                "  {label:<28} n={n:>4}  min={min:8.3}ms  median={median:8.3}ms  mean={mean:8.3}ms"
            );
        }

        // Realistic portrait cover source (~700x1000), high-entropy so no early
        // candidate edge fits the byte budget → full resize+encode loop runs.
        let cover_src = noisy_png(700, 1000);
        let hash_src = noisy_png(512, 728);

        // Warm up (allocator, code paths, page cache) before timing.
        for _ in 0..3 {
            let _ = process_cover(&cover_src).expect("warmup process_cover");
            let _ = crate::phash::dhash(&hash_src);
        }

        const N_COVER: usize = 20;
        const N_HASH: usize = 100;

        let mut cover_ns = Vec::with_capacity(N_COVER);
        for _ in 0..N_COVER {
            let t = Instant::now();
            let out = process_cover(&cover_src).expect("process_cover");
            cover_ns.push(t.elapsed().as_nanos());
            assert!(out.len() <= MAX_COVER_BYTES);
        }

        let mut hash_ns = Vec::with_capacity(N_HASH);
        for _ in 0..N_HASH {
            let t = Instant::now();
            let out = crate::phash::dhash(&hash_src);
            hash_ns.push(t.elapsed().as_nanos());
            assert!(out.is_some());
        }

        eprintln!("\n=== opt-level hot-path bench (release profile) ===");
        stats("process_cover(700x1000)", cover_ns);
        stats("dhash(512x728)", hash_ns);
        eprintln!("=================================================\n");
    }

    #[test]
    fn preserves_aspect_ratio_within_budget() {
        // A portrait, noisy cover: stays portrait (not squared) and fits budget.
        let src = noisy_png(700, 1000);
        let webp = process_cover(&src).expect("processes");
        assert!(
            webp.len() <= MAX_COVER_BYTES,
            "cover {} bytes exceeds budget {}",
            webp.len(),
            MAX_COVER_BYTES
        );
        let (w, h) = image::load_from_memory(&webp).unwrap().dimensions();
        assert!(h > w, "portrait aspect ratio must be preserved");
        assert!(w.max(h) <= CANDIDATE_EDGES[0], "longest edge within cap");
    }

    #[test]
    fn never_upscales_small_source() {
        let src = noisy_png(120, 180);
        let webp = process_cover(&src).unwrap();
        let (w, h) = image::load_from_memory(&webp).unwrap().dimensions();
        assert_eq!((w, h), (120, 180), "small source kept at native size");
    }

    #[test]
    fn rejects_empty_and_non_image() {
        assert!(process_cover(&[]).is_err());
        assert!(process_cover(b"not an image").is_err());
    }

    #[test]
    fn lossy_cover_is_sharp_and_well_under_budget() {
        // A realistic portrait cover already within the 512 longest-edge cap (360x512),
        // smooth gradient — what real art resembles, vs. adversarial pure noise. Lossy
        // must keep its FULL native dimensions (not shrink) AND land under the 150 KB
        // budget, decoding back to a valid image. (Pure noise legitimately shrinks
        // instead — covered by preserves_aspect_ratio_within_budget.)
        let mut img = RgbImage::new(360, 512);
        for (x, y, px) in img.enumerate_pixels_mut() {
            // Smooth full-range gradient (no wrap discontinuity) — compresses well
            // with lossy, so it stays at native size under budget.
            let r = (x * 255 / 359) as u8;
            let g = (y * 255 / 511) as u8;
            let b = ((x + y) * 255 / (359 + 511)) as u8;
            *px = image::Rgb([r, g, b]);
        }
        let mut src = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut src), image::ImageFormat::Png)
            .unwrap();
        let webp = process_cover(&src).expect("processes");
        assert!(
            webp.len() <= MAX_COVER_BYTES,
            "lossy cover {} bytes over budget {}",
            webp.len(),
            MAX_COVER_BYTES
        );
        let (w, h) = image::load_from_memory(&webp).unwrap().dimensions();
        assert_eq!((w, h), (360, 512), "kept native dimensions via lossy, not shrunk");
    }

    #[test]
    fn gif_source_decodes_and_processes() {
        // A GIF source (previously "unsupported") must now decode + process to WebP.
        let img = RgbImage::from_pixel(300, 420, image::Rgb([180, 40, 90]));
        let mut gif = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif)
            .expect("encode gif");
        let webp = process_cover(&gif).expect("gif cover processes");
        image::load_from_memory(&webp).expect("valid webp from gif source");
    }

    #[test]
    fn classify_cover_error_maps_reasons() {
        assert_eq!(
            classify_cover_error(&anyhow!("cover source too large (max 24 MB)")),
            "too_large"
        );
        assert_eq!(
            classify_cover_error(&anyhow!("image too large or unsupported")),
            "unsupported"
        );
        assert_eq!(classify_cover_error(&anyhow!("empty cover source")), "empty");
        assert_eq!(classify_cover_error(&anyhow!("libwebp exploded")), "encode");
    }

    #[tokio::test]
    async fn cover_issue_record_increments_then_clears() {
        use sqlx::sqlite::SqlitePoolOptions;
        let main = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&main).await.unwrap();
        sqlx::query(
            "INSERT INTO work (id, created_at, updated_at) \
             VALUES ('w_iss', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&main)
        .await
        .unwrap();

        record_cover_issue(&main, "w_iss", "unsupported", "boom").await;
        record_cover_issue(&main, "w_iss", "too_large", "again").await;
        let (reason, attempts): (String, i64) =
            sqlx::query_as("SELECT reason, attempts FROM work_cover_issue WHERE work_id = ?")
                .bind("w_iss")
                .fetch_one(&main)
                .await
                .unwrap();
        assert_eq!(attempts, 2, "second record increments attempts");
        assert_eq!(reason, "too_large", "reason refreshed to the latest");

        clear_cover_issue(&main, "w_iss").await;
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_cover_issue WHERE work_id = ?")
            .bind("w_iss")
            .fetch_one(&main)
            .await
            .unwrap();
        assert_eq!(n, 0, "clear removes the issue row");
    }

    #[test]
    fn cover_path_is_versioned() {
        assert_eq!(cover_path("w_abc", 99), "/covers/w_abc.webp?v=99");
    }

    #[tokio::test]
    async fn put_work_cover_stores_blob_and_flips_version() {
        use sqlx::sqlite::SqlitePoolOptions;
        // Main (replicated) DB: `work` + the `cover_cached_version` pointer. After
        // migration 0040 it has NO `work_cover_blob` table.
        let main = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&main).await.unwrap();
        // Covers (un-replicated) DB: just the blob table, as `db::init_covers` builds.
        let covers = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE work_cover_blob (work_id TEXT PRIMARY KEY, webp BLOB NOT NULL, \
             version INTEGER NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&covers)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work (id, created_at, updated_at) \
             VALUES ('w_test', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(&main)
        .await
        .unwrap();

        // No cache yet → resolver falls back to the MangaDex thumbnail URL.
        let ver0: Option<i64> =
            sqlx::query_scalar("SELECT cover_cached_version FROM work WHERE id = 'w_test'")
                .fetch_one(&main)
                .await
                .unwrap();
        assert!(ver0.is_none());

        let webp = process_cover(&noisy_png(400, 600)).unwrap();
        put_work_cover(&main, &covers, "w_test", &webp)
            .await
            .unwrap();

        // Blob is stored in the covers DB and the work's version pointer (main DB)
        // matches it (invariant: no version pointer without the bytes behind it).
        let (blob, blob_ver): (Vec<u8>, i64) =
            sqlx::query_as("SELECT webp, version FROM work_cover_blob WHERE work_id = 'w_test'")
                .fetch_one(&covers)
                .await
                .unwrap();
        let work_ver: Option<i64> =
            sqlx::query_scalar("SELECT cover_cached_version FROM work WHERE id = 'w_test'")
                .fetch_one(&main)
                .await
                .unwrap();
        assert_eq!(blob, webp, "stored bytes round-trip");
        assert_eq!(
            Some(blob_ver),
            work_ver,
            "version pointer matches stored blob"
        );
        assert_eq!(
            work_cover_url("w_test", work_ver, Some("md"), Some("f.jpg")),
            cover_path("w_test", blob_ver),
            "resolver now prefers the cached VPS path"
        );

        // pending_cover_count excludes the now-cached work.
        assert_eq!(pending_cover_count(&main).await.unwrap(), 0);
    }

    #[test]
    fn work_cover_url_prefers_cached_then_own_lazy_route() {
        // Cached → versioned VPS blob path.
        assert_eq!(
            work_cover_url("w_1", Some(7), Some("md-uuid"), Some("f.jpg")),
            "/covers/w_1.webp?v=7"
        );
        // Uncached but cover-able → our own lazy /covers/ route (serve_cover fetches +
        // caches the MangaDex cover on first hit), NOT the raw CDN URL.
        assert_eq!(
            work_cover_url("w_1", None, Some("md-uuid"), Some("f.jpg")),
            "/covers/w_1.webp"
        );
        // No anchor → empty.
        assert_eq!(work_cover_url("w_1", None, None, None), "");
        assert_eq!(work_cover_url("w_1", None, Some("md-uuid"), None), "");
    }
}
