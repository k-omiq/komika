//! Direct MangaDex API client + catalogue sync (CATALOGUE.md §5).
//!
//! Talks to `api.mangadex.org` directly (NOT via Suwayomi) — only the direct API
//! exposes `createdAt` windowing, the external-ID `links` field, and full
//! `altTitles`. Writes the canonical spine through `crate::catalog`. A global token
//! bucket keeps the crawl under MangaDex's ~5 req/s per-IP ceiling (the egress IP is
//! shared fleet-wide, so this is a fleet budget). The whole subsystem is gated off
//! by default (`CATALOGUE_SYNC`); nothing here runs unless explicitly enabled.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::catalog::{self, Alias, WorkInput};

const API_BASE: &str = "https://api.mangadex.org";
const COVERS_BASE: &str = "https://uploads.mangadex.org/covers";
const PAGE_LIMIT: i64 = 100; // MangaDex list max for /manga
/// Stop paging a window before the hard `offset + limit <= 10_000` cap and slide
/// the `since` window instead.
const WINDOW_OFFSET_CAP: i64 = 9_900;

/// Which timestamp a sweep windows on. The full seed walks the whole catalogue by
/// `createdAt`; recurring incremental refreshes walk only recently-changed records by
/// `updatedAt` (CATALOGUE.md §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncWindow {
    Created,
    Updated,
}

impl SyncWindow {
    /// The `...Since` query parameter name.
    fn since_param(self) -> &'static str {
        match self {
            SyncWindow::Created => "createdAtSince",
            SyncWindow::Updated => "updatedAtSince",
        }
    }
    /// The `order[...]` query parameter key (always ascending, so the window slides forward).
    fn order_key(self) -> &'static str {
        match self {
            SyncWindow::Created => "order[createdAt]",
            SyncWindow::Updated => "order[updatedAt]",
        }
    }
}

/// A simple async token bucket. `capacity` tokens, refilled at `refill_per_sec`.
struct TokenBucket {
    inner: Mutex<BucketState>,
    capacity: f64,
    refill_per_sec: f64,
}

struct BucketState {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rate_per_sec: f64) -> Self {
        let rate = rate_per_sec.max(0.1);
        Self {
            inner: Mutex::new(BucketState {
                tokens: rate,
                last: Instant::now(),
            }),
            capacity: rate,
            refill_per_sec: rate,
        }
    }

    /// Block until one token is available, then consume it.
    async fn acquire(&self) {
        loop {
            let wait = {
                let mut st = self.inner.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(st.last).as_secs_f64();
                st.tokens = (st.tokens + elapsed * self.refill_per_sec).min(self.capacity);
                st.last = now;
                if st.tokens >= 1.0 {
                    st.tokens -= 1.0;
                    return;
                }
                (1.0 - st.tokens) / self.refill_per_sec
            };
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait.max(0.001))).await;
        }
    }
}

/// The direct MangaDex API client. Cheap to clone-share via `Arc`.
pub struct MangaDexClient {
    http: reqwest::Client,
    limiter: TokenBucket,
}

impl MangaDexClient {
    pub fn new(user_agent: &str, rate_per_sec: f64) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(user_agent.to_string())
            .build()
            .expect("reqwest client builds");
        Self {
            http,
            limiter: TokenBucket::new(rate_per_sec),
        }
    }

    /// One page of `/manga`, ordered by `createdAt` asc, cover expanded. Returns the
    /// parsed mangas (bad individual records are skipped, not fatal) and the total.
    pub async fn list_manga(
        &self,
        window: SyncWindow,
        since: Option<&str>,
        offset: i64,
    ) -> Result<(Vec<MdManga>, i64)> {
        let mut params: Vec<(String, String)> = vec![
            ("limit".into(), PAGE_LIMIT.to_string()),
            ("offset".into(), offset.to_string()),
            ("includes[]".into(), "cover_art".into()),
            ("includes[]".into(), "author".into()),
            ("includes[]".into(), "artist".into()),
            (window.order_key().into(), "asc".into()),
            // Skip nothing on content rating — we store the flag and gate at query time.
            ("contentRating[]".into(), "safe".into()),
            ("contentRating[]".into(), "suggestive".into()),
            ("contentRating[]".into(), "erotica".into()),
            ("contentRating[]".into(), "pornographic".into()),
        ];
        if let Some(since) = since {
            params.push((window.since_param().into(), since.to_string()));
        }
        self.limiter.acquire().await;
        let res = self
            .http
            .get(format!("{API_BASE}/manga"))
            .query(&params)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(anyhow!("MangaDex /manga error {}", res.status()));
        }
        let body: RawList = res.json().await?;
        let mut mangas = Vec::with_capacity(body.data.len());
        let mut skipped = 0usize;
        for raw in body.data {
            match serde_json::from_value::<MdManga>(raw) {
                Ok(m) => mangas.push(m),
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            tracing::warn!(skipped, "mangadex: skipped unparseable manga records");
        }
        Ok((mangas, body.total))
    }

    /// One page of the global `/chapter` firehose, ordered by `createdAt` asc.
    pub async fn list_chapters(
        &self,
        window: SyncWindow,
        since: Option<&str>,
        offset: i64,
    ) -> Result<(Vec<MdChapter>, i64)> {
        let mut params: Vec<(String, String)> = vec![
            ("limit".into(), PAGE_LIMIT.to_string()),
            ("offset".into(), offset.to_string()),
            (window.order_key().into(), "asc".into()),
            ("includes[]".into(), "manga".into()),
            // English-only: Komika serves only English chapters, so filter the firehose
            // at the source (smaller pages, no non-English rows to mirror).
            ("translatedLanguage[]".into(), "en".into()),
        ];
        if let Some(since) = since {
            params.push((window.since_param().into(), since.to_string()));
        }
        self.limiter.acquire().await;
        let res = self
            .http
            .get(format!("{API_BASE}/chapter"))
            .query(&params)
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(anyhow!("MangaDex /chapter error {}", res.status()));
        }
        let body: RawChapterList = res.json().await?;
        let mut chapters = Vec::with_capacity(body.data.len());
        for raw in body.data {
            if let Ok(c) = serde_json::from_value::<MdChapter>(raw) {
                chapters.push(c);
            }
        }
        Ok((chapters, body.total))
    }

    /// Download a work's cover thumbnail and compute its perceptual hash. Uses the
    /// 512px thumbnail (smaller download, always JPEG). Best-effort: returns `None`
    /// on any network/decode failure — a missing hash just means one fewer dedup
    /// signal, never a failed sync. Rate-limited like the API calls.
    pub async fn cover_phash(&self, manga_id: &str, file_name: &str) -> Option<String> {
        let url = format!("{}.512.jpg", cover_url(manga_id, file_name));
        self.limiter.acquire().await;
        let res = self.http.get(url).send().await.ok()?;
        if !res.status().is_success() {
            return None;
        }
        let bytes = res.bytes().await.ok()?;
        crate::phash::dhash(&bytes)
    }

    /// Resolve a chapter's ordered page image URLs via MangaDex@Home
    /// (`GET /at-home/server/{chapterId}` → `{ baseUrl, chapter: { hash, data[] } }`).
    /// Each page URL is `{baseUrl}/data/{hash}/{filename}`. These are dynamic
    /// `*.mangadex.network` hosts and MUST be proxied by the Worker (hotlinks get a
    /// wrong response). Rate-limited (this endpoint is capped at 40/min; the global
    /// ~5 req/s bucket keeps us well under). CATALOGUE.md §5, §9.
    pub async fn at_home(&self, chapter_id: &str) -> Result<Vec<String>> {
        self.limiter.acquire().await;
        let res = self
            .http
            .get(format!("{API_BASE}/at-home/server/{chapter_id}"))
            .send()
            .await?;
        if !res.status().is_success() {
            return Err(anyhow!("MangaDex /at-home error {}", res.status()));
        }
        let body: AtHome = res.json().await?;
        let base = body.base_url.trim_end_matches('/');
        let hash = &body.chapter.hash;
        Ok(body
            .chapter
            .data
            .into_iter()
            .map(|filename| format!("{base}/data/{hash}/{filename}"))
            .collect())
    }
}

/// MangaDex@Home server response for a chapter (the fields we build page URLs from).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AtHome {
    base_url: String,
    chapter: AtHomeChapter,
}

#[derive(Deserialize)]
struct AtHomeChapter {
    hash: String,
    #[serde(default)]
    data: Vec<String>,
}

/// Proxy-ready cover URL for a MangaDex work. Callers must route this through the
/// Worker proxy — MangaDex serves a wrong response to hotlinks. Used by cover
/// pHash ingest and canonical-model serving (CATALOGUE.md §5–6).
pub fn cover_url(manga_id: &str, file_name: &str) -> String {
    format!("{COVERS_BASE}/{manga_id}/{file_name}")
}

/// A smaller (512px) cover thumbnail URL — same proxy rules as `cover_url`. Used for
/// reader browse/list surfaces where the full-resolution cover is wasteful.
pub fn cover_thumb_url(manga_id: &str, file_name: &str) -> String {
    format!("{COVERS_BASE}/{manga_id}/{file_name}.512.jpg")
}

// ---- Response shapes -------------------------------------------------------

#[derive(Deserialize)]
struct RawList {
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default)]
    total: i64,
}

#[derive(Deserialize)]
struct RawChapterList {
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default)]
    total: i64,
}

#[derive(Debug, Deserialize)]
pub struct MdManga {
    pub id: String,
    pub attributes: MdAttrs,
    #[serde(default)]
    pub relationships: Vec<MdRel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MdAttrs {
    #[serde(default)]
    pub title: HashMap<String, String>,
    #[serde(default)]
    pub alt_titles: Vec<HashMap<String, String>>,
    #[serde(default)]
    pub description: HashMap<String, String>,
    pub original_language: Option<String>,
    pub status: Option<String>,
    pub year: Option<i64>,
    pub publication_demographic: Option<String>,
    pub content_rating: Option<String>,
    #[serde(default)]
    pub links: HashMap<String, String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MdRel {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub attributes: Option<MdRelAttrs>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MdRelAttrs {
    pub file_name: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MdChapter {
    pub id: String,
    pub attributes: MdChapterAttrs,
    #[serde(default)]
    pub relationships: Vec<MdRel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MdChapterAttrs {
    pub chapter: Option<String>,
    pub volume: Option<String>,
    pub title: Option<String>,
    pub translated_language: Option<String>,
    pub publish_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

// ---- Mapping ---------------------------------------------------------------

const EXTERNAL_LINK_KEYS: &[&str] = &["al", "mal", "mu", "kt", "ap"];

/// Map a MangaDex manga to `(mangadex_id, WorkInput)` for the canonical spine.
pub fn to_work_input(m: &MdManga) -> (String, WorkInput) {
    let a = &m.attributes;

    let (primary_lang, primary_title) = pick_localized(&a.title);
    let description = pick_localized(&a.description).1;

    // Aliases: every localized primary title + every altTitle entry.
    let mut aliases: Vec<Alias> = Vec::new();
    for (lang, val) in &a.title {
        aliases.push(Alias {
            raw: val.clone(),
            lang: Some(lang.clone()),
        });
    }
    for alt in &a.alt_titles {
        for (lang, val) in alt {
            aliases.push(Alias {
                raw: val.clone(),
                lang: Some(lang.clone()),
            });
        }
    }

    let mut external_ids: Vec<(String, String)> = Vec::new();
    for key in EXTERNAL_LINK_KEYS {
        if let Some(v) = a.links.get(*key) {
            if !v.is_empty() {
                external_ids.push(((*key).to_string(), v.clone()));
            }
        }
    }

    let content_rating = a.content_rating.clone();
    let is_nsfw = matches!(
        content_rating.as_deref(),
        Some("erotica") | Some("pornographic")
    );

    (
        m.id.clone(),
        WorkInput {
            primary_title,
            primary_lang,
            description,
            year: a.year,
            original_language: a.original_language.clone(),
            status: map_status(a.status.as_deref()),
            demographic: a.publication_demographic.clone(),
            content_rating,
            is_nsfw,
            author: rel_name(m, "author"),
            artist: rel_name(m, "artist"),
            cover_phash: None, // filled by cover pHash ingest in sync_catalogue when enabled
            // From the already-expanded cover_art relationship — no extra request. Powers
            // the reader cover URL for canonical works (independent of the pHash ingest).
            cover_file_name: cover_file_name(m),
            aliases,
            external_ids,
        },
    )
}

/// Prefer an English value, then a romanized Japanese one, then any entry.
fn pick_localized(map: &HashMap<String, String>) -> (Option<String>, Option<String>) {
    for key in ["en", "ja-ro", "ja"] {
        if let Some(v) = map.get(key) {
            if !v.is_empty() {
                return (Some(key.to_string()), Some(v.clone()));
            }
        }
    }
    map.iter()
        .find(|(_, v)| !v.is_empty())
        .map(|(k, v)| (Some(k.clone()), Some(v.clone())))
        .unwrap_or((None, None))
}

fn rel_name(m: &MdManga, kind: &str) -> Option<String> {
    m.relationships
        .iter()
        .find(|r| r.kind == kind)
        .and_then(|r| r.attributes.as_ref())
        .and_then(|a| a.name.clone())
        .filter(|s| !s.is_empty())
}

/// MangaDex cover fileName for a manga (first cover_art relationship), if expanded.
/// Feeds cover pHash ingest (CATALOGUE.md §5).
pub fn cover_file_name(m: &MdManga) -> Option<String> {
    m.relationships
        .iter()
        .find(|r| r.kind == "cover_art")
        .and_then(|r| r.attributes.as_ref())
        .and_then(|a| a.file_name.clone())
}

fn map_status(s: Option<&str>) -> Option<String> {
    Some(
        match s {
            Some("ongoing") => "ONGOING",
            Some("completed") => "COMPLETED",
            Some("hiatus") => "HIATUS",
            Some("cancelled") => "CANCELLED",
            _ => return None,
        }
        .to_string(),
    )
}

/// The timestamp a manga sweep slides its window on (per `SyncWindow`).
fn manga_window_ts(m: &MdManga, window: SyncWindow) -> Option<String> {
    match window {
        SyncWindow::Created => m.attributes.created_at.clone(),
        SyncWindow::Updated => m.attributes.updated_at.clone(),
    }
}

/// The timestamp a chapter sweep slides its window on (per `SyncWindow`).
fn chapter_window_ts(c: &MdChapter, window: SyncWindow) -> Option<String> {
    match window {
        SyncWindow::Created => c.attributes.created_at.clone(),
        SyncWindow::Updated => c.attributes.updated_at.clone(),
    }
}

/// The `manga` relationship id on a chapter (which work the chapter belongs to).
pub fn chapter_manga_id(c: &MdChapter) -> Option<String> {
    c.relationships
        .iter()
        .find(|r| r.kind == "manga")
        .map(|r| r.id.clone())
}

/// Coerce an ISO-8601 timestamp (e.g. `2018-10-04T22:16:00+00:00`) into the
/// `createdAtSince` form MangaDex expects (`YYYY-MM-DDTHH:MM:SS`, no offset).
pub fn to_since(ts: &str) -> Option<String> {
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string())
}

// ---- Sync loops ------------------------------------------------------------

/// Full/incremental catalogue sweep. Pages `/manga` ordered by `createdAt`, sliding
/// the `createdAtSince` window past the 10k offset cap, upserting each work. Best
/// effort: a failed page/record is logged and does not abort the sweep. Pass
/// `initial_since` to resume/incrementally refresh from a known timestamp.
pub async fn sync_catalogue(
    pool: &sqlx::SqlitePool,
    client: &MangaDexClient,
    window: SyncWindow,
    initial_since: Option<String>,
    cover_phash: bool,
) -> Result<u64> {
    let mut since = initial_since;
    let mut upserted: u64 = 0;
    loop {
        let mut offset = 0i64;
        let mut last_created: Option<String> = None;
        let mut done = false;
        loop {
            let (mangas, total) = client.list_manga(window, since.as_deref(), offset).await?;
            if mangas.is_empty() {
                done = true;
                break;
            }
            let page_len = mangas.len() as i64;
            for m in &mangas {
                let (id, mut input) = to_work_input(m);
                // Cover pHash ingest (opt-in): one extra cover download per work,
                // hashed for the dedup cover signal. Best-effort — a failure leaves
                // cover_phash None and COALESCE keeps any prior hash on re-sync.
                if cover_phash {
                    if let Some(fname) = cover_file_name(m) {
                        input.cover_phash = client.cover_phash(&id, &fname).await;
                    }
                }
                match catalog::upsert_work_from_mangadex(pool, &id, &input).await {
                    Ok(_) => upserted += 1,
                    Err(e) => tracing::warn!(manga = %id, error = %e, "mangadex: upsert failed"),
                }
                if let Some(ts) = manga_window_ts(m, window) {
                    last_created = Some(ts);
                }
            }
            offset += page_len;
            tracing::info!(offset, total, upserted, "mangadex: catalogue page");
            if page_len < PAGE_LIMIT {
                done = true; // last (partial) page of this window and overall
                break;
            }
            if offset >= WINDOW_OFFSET_CAP {
                break; // slide the window
            }
        }
        if done {
            break;
        }
        // Slide the window forward. If it can't advance (all records share the
        // boundary second), stop rather than loop forever.
        let next_since = last_created.as_deref().and_then(to_since);
        if next_since.is_none() || next_since == since {
            tracing::warn!("mangadex: window could not advance; stopping sweep");
            break;
        }
        since = next_since;
    }
    tracing::info!(upserted, "mangadex: catalogue sweep complete");
    Ok(upserted)
}

/// Global `/chapter` firehose → mirrored `chapter` rows. Each chapter is attached to
/// the `mangadex` source_series of its `manga` relationship; chapters whose work
/// hasn't been catalogued yet are skipped (a later catalogue sweep + re-run picks
/// them up). Same `createdAt` windowing as the catalogue sweep.
pub async fn sync_chapters(
    pool: &sqlx::SqlitePool,
    client: &MangaDexClient,
    window: SyncWindow,
    initial_since: Option<String>,
) -> Result<u64> {
    let mut since = initial_since;
    let mut stored: u64 = 0;
    loop {
        let mut offset = 0i64;
        let mut last_created: Option<String> = None;
        let mut done = false;
        loop {
            let (chapters, total) = client
                .list_chapters(window, since.as_deref(), offset)
                .await?;
            if chapters.is_empty() {
                done = true;
                break;
            }
            let page_len = chapters.len() as i64;
            for c in &chapters {
                if let Some(ts) = chapter_window_ts(c, window) {
                    last_created = Some(ts);
                }
                // English-only mirror: the firehose is already filtered to `en`, but
                // guard defensively so a stray non-English row is never stored.
                if c.attributes.translated_language.as_deref() != Some("en") {
                    continue;
                }
                let Some(manga_id) = chapter_manga_id(c) else {
                    continue;
                };
                let ssid = match catalog::find_source_series_id(
                    pool, "mangadex", "mangadex", &manga_id,
                )
                .await
                {
                    Ok(Some(id)) => id,
                    Ok(None) => continue, // work not catalogued yet
                    Err(e) => {
                        tracing::warn!(error = %e, "mangadex: chapter source_series lookup failed");
                        continue;
                    }
                };
                let ch = catalog::ChapterInput {
                    external_id: c.id.clone(),
                    number: c.attributes.chapter.clone(),
                    volume: c.attributes.volume.clone(),
                    lang: c.attributes.translated_language.clone(),
                    title: c.attributes.title.clone(),
                    published_at: c.attributes.publish_at.clone(),
                };
                match catalog::upsert_chapter(pool, &ssid, &ch).await {
                    Ok(_) => stored += 1,
                    Err(e) => tracing::warn!(error = %e, "mangadex: chapter upsert failed"),
                }
            }
            offset += page_len;
            if page_len < PAGE_LIMIT {
                done = true;
                break;
            }
            if offset >= WINDOW_OFFSET_CAP {
                break;
            }
            let _ = total;
        }
        if done {
            break;
        }
        let next_since = last_created.as_deref().and_then(to_since);
        if next_since.is_none() || next_since == since {
            tracing::warn!("mangadex: chapter window could not advance; stopping");
            break;
        }
        since = next_since;
    }
    tracing::info!(stored, "mangadex: chapter sweep complete");
    Ok(stored)
}

/// Run one catalogue + chapter sync cycle. A job with no stored cursor does a full
/// `createdAt` seed; a job with a cursor does an incremental `updatedAtSince` refresh
/// (CATALOGUE.md §5). The cursor advances to the cycle's start time only on success,
/// so a failed or interrupted cycle safely retries the same window next tick. Catalogue
/// runs before chapters (chapters attach to already-catalogued works).
async fn sync_cycle(pool: &sqlx::SqlitePool, client: &MangaDexClient, cover_phash: bool) {
    // Wall-clock at cycle start, in MangaDex `since` form. Anything updated during the
    // cycle is >= this, so it's caught by the next cycle rather than missed.
    let run_start = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();

    match catalog::get_sync_cursor(pool, "catalogue").await {
        Ok(cur) => {
            let (window, since) = match &cur {
                Some(ts) => (SyncWindow::Updated, Some(ts.clone())),
                None => (SyncWindow::Created, None),
            };
            match sync_catalogue(pool, client, window, since, cover_phash).await {
                Ok(n) => {
                    tracing::info!(
                        upserted = n,
                        incremental = cur.is_some(),
                        "mangadex: catalogue cycle done"
                    );
                    if let Err(e) = catalog::set_sync_cursor(pool, "catalogue", &run_start).await {
                        tracing::warn!(error = %e, "mangadex: failed to persist catalogue cursor");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "mangadex: catalogue cycle failed (cursor unchanged)")
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "mangadex: catalogue cursor read failed; skipping"),
    }

    match catalog::get_sync_cursor(pool, "chapters").await {
        Ok(cur) => {
            let (window, since) = match &cur {
                Some(ts) => (SyncWindow::Updated, Some(ts.clone())),
                None => (SyncWindow::Created, None),
            };
            match sync_chapters(pool, client, window, since).await {
                Ok(n) => {
                    tracing::info!(
                        stored = n,
                        incremental = cur.is_some(),
                        "mangadex: chapter cycle done"
                    );
                    if let Err(e) = catalog::set_sync_cursor(pool, "chapters", &run_start).await {
                        tracing::warn!(error = %e, "mangadex: failed to persist chapter cursor");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "mangadex: chapter cycle failed (cursor unchanged)")
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "mangadex: chapter cursor read failed; skipping"),
    }
}

/// Spawn the recurring catalogue + chapter sync. The first tick fires immediately
/// (seeding on startup), then every `interval_secs`. Exits cleanly when `shutdown`
/// fires. Mirrors the `scanner::spawn` background-task pattern.
pub fn spawn_recurring(
    pool: sqlx::SqlitePool,
    client: Arc<MangaDexClient>,
    cover_phash: bool,
    interval_secs: u64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(
            interval_secs,
            cover_phash,
            "mangadex: recurring catalogue sync started"
        );
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    sync_cycle(&pool, &client, cover_phash).await;
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("mangadex: catalogue sync stopping");
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

    fn manga_json() -> Value {
        serde_json::json!({
            "id": "md-uuid-1",
            "attributes": {
                "title": { "ja-ro": "Tensei Shitara Slime Datta Ken" },
                "altTitles": [ { "en": "That Time I Got Reincarnated as a Slime" }, { "ja": "転生したらスライムだった件" } ],
                "description": { "en": "Mikami Satoru is reincarnated as a slime." },
                "originalLanguage": "ja",
                "status": "ongoing",
                "year": 2015,
                "publicationDemographic": "seinen",
                "contentRating": "safe",
                "links": { "al": "101517", "mal": "70951", "raw": "https://example.com" },
                "createdAt": "2018-10-04T22:16:00+00:00"
            },
            "relationships": [
                { "id": "auth-1", "type": "author", "attributes": { "name": "Fuse" } },
                { "id": "cov-1", "type": "cover_art", "attributes": { "fileName": "abc.jpg" } }
            ]
        })
    }

    #[test]
    fn maps_manga_to_work_input() {
        let m: MdManga = serde_json::from_value(manga_json()).unwrap();
        let (id, input) = to_work_input(&m);
        assert_eq!(id, "md-uuid-1");
        assert_eq!(input.primary_lang.as_deref(), Some("ja-ro"));
        assert_eq!(input.status.as_deref(), Some("ONGOING"));
        assert_eq!(input.year, Some(2015));
        assert_eq!(input.author.as_deref(), Some("Fuse"));
        assert!(!input.is_nsfw);
        // altTitles + localized titles all become aliases.
        assert!(input
            .aliases
            .iter()
            .any(|a| a.raw.contains("Slime Datta Ken")));
        assert!(input.aliases.iter().any(|a| a.raw.contains("Reincarnated")));
        // Only known external providers are captured ("raw" is ignored).
        assert!(input.external_ids.contains(&("al".into(), "101517".into())));
        assert!(input.external_ids.contains(&("mal".into(), "70951".into())));
        assert!(!input.external_ids.iter().any(|(p, _)| p == "raw"));
        assert_eq!(cover_file_name(&m).as_deref(), Some("abc.jpg"));
        // The fileName is carried on the work input so a cover URL can be built later.
        assert_eq!(input.cover_file_name.as_deref(), Some("abc.jpg"));
        assert_eq!(
            cover_url("md-uuid-1", "abc.jpg"),
            "https://uploads.mangadex.org/covers/md-uuid-1/abc.jpg"
        );
        assert_eq!(
            cover_thumb_url("md-uuid-1", "abc.jpg"),
            "https://uploads.mangadex.org/covers/md-uuid-1/abc.jpg.512.jpg"
        );
    }

    #[test]
    fn nsfw_flag_from_content_rating() {
        let mut v = manga_json();
        v["attributes"]["contentRating"] = serde_json::json!("pornographic");
        let m: MdManga = serde_json::from_value(v).unwrap();
        assert!(to_work_input(&m).1.is_nsfw);
    }

    #[test]
    fn to_since_strips_offset() {
        assert_eq!(
            to_since("2018-10-04T22:16:00+00:00").as_deref(),
            Some("2018-10-04T22:16:00")
        );
        assert_eq!(to_since("garbage"), None);
    }
}
