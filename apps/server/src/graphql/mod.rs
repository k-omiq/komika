pub mod types;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_graphql::{Context, EmptySubscription, Error, Object, Result, Schema, SimpleObject, ID};
use chrono::Utc;
use sqlx::SqlitePool;

use crate::auth::{self, User};
use crate::catalog;
use crate::scanner::{scan_series, scan_state};
use crate::suwayomi::{FetchType, SuwayomiClient, SuwayomiManga};
use types::*;

const PAGE_SIZE: i64 = 20;

/// Aggregate scan-scheduler health, updated once per tick by `scanner::tick`.
#[derive(Default)]
pub struct ScanHealth {
    pub library_size: usize,
    pub overdue_count: usize,
    pub last_tick_at: Option<String>,
}

/// A dependency-light sliding-window rate limiter, keyed by an arbitrary string
/// (e.g. `login:<username>`). Prunes stale timestamps on each check.
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
    max: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max: u32, window_secs: u64) -> Self {
        Self {
            hits: Mutex::new(HashMap::new()),
            max,
            window: Duration::from_secs(window_secs),
        }
    }

    /// Read-only: is `key` already at the limit within the window? Returns the
    /// retry-after seconds if so. Prunes stale timestamps but does NOT record a
    /// new hit — callers that want to count an attempt call `record` explicitly.
    pub fn is_limited(&self, key: &str) -> Option<u64> {
        let now = Instant::now();
        let mut map = self.hits.lock().unwrap();
        let entry = map.entry(key.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < self.window);
        if entry.len() as u32 >= self.max {
            let oldest = entry.first().copied().unwrap_or(now);
            let retry = self.window.saturating_sub(now.duration_since(oldest));
            Some(retry.as_secs().max(1))
        } else {
            None
        }
    }

    /// Record one attempt against `key` (prunes stale timestamps first).
    pub fn record(&self, key: &str) {
        let now = Instant::now();
        let mut map = self.hits.lock().unwrap();
        let entry = map.entry(key.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < self.window);
        entry.push(now);
    }

    /// Check-and-record: `Err(retry)` if already at the limit, else records a hit.
    pub fn check(&self, key: &str) -> std::result::Result<(), u64> {
        if let Some(retry) = self.is_limited(key) {
            return Err(retry);
        }
        self.record(key);
        Ok(())
    }
}

/// Shared, request-independent application state.
pub struct AppState {
    pub pool: SqlitePool,
    pub suwayomi: SuwayomiClient,
    /// Direct MangaDex client — page resolution for canonical (MangaDex-mirrored)
    /// works via MangaDex@Home (CATALOGUE.md §5). Shared with the catalogue sync.
    pub mangadex: std::sync::Arc<crate::mangadex::MangaDexClient>,
    /// Usernames granted admin (see `Config::admin_users`).
    pub admin_users: Vec<String>,
    /// Aggregate scan-scheduler health (for `scanStatus`).
    pub scan_health: Mutex<ScanHealth>,
    /// Per-key sliding-window limiter for `login` / `register`.
    pub auth_limiter: RateLimiter,
}

/// Per-request auth: the bearer token from the `Authorization` header, if any.
#[derive(Clone, Default)]
pub struct RequestAuth(pub Option<String>);

/// Per-request client IP, resolved by the HTTP layer (X-Forwarded-For behind a
/// proxy, else the socket peer). Used to key the auth rate limiter so one actor
/// cannot exhaust another account's budget.
#[derive(Clone, Default)]
pub struct ClientIp(pub Option<String>);

pub type ApiSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

/// Build the schema over a shared `AppState`. The same `Arc` is handed to the scan
/// scheduler so resolvers and the background task see one set of state.
pub fn build_schema(state: std::sync::Arc<AppState>) -> ApiSchema {
    Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .extension(ErrorLogger)
        .data(state)
        .finish()
}

/// async-graphql extension that logs every resolver error server-side. GraphQL
/// returns HTTP 200 with errors in the body, so the TraceLayer access log never
/// sees them — without this, DB/upstream failures would be invisible in logs.
/// This is our dependency-light "error tracking": a Sentry/OpenTelemetry hook is
/// intentionally NOT wired (no hard dependency, no committed DSN); if one is ever
/// added it should live behind an env-gated optional cargo feature.
struct ErrorLogger;

impl async_graphql::extensions::ExtensionFactory for ErrorLogger {
    fn create(&self) -> std::sync::Arc<dyn async_graphql::extensions::Extension> {
        std::sync::Arc::new(ErrorLoggerExtension)
    }
}

struct ErrorLoggerExtension;

#[async_trait::async_trait]
impl async_graphql::extensions::Extension for ErrorLoggerExtension {
    async fn request(
        &self,
        ctx: &async_graphql::extensions::ExtensionContext<'_>,
        next: async_graphql::extensions::NextRequest<'_>,
    ) -> async_graphql::Response {
        let resp = next.run(ctx).await;
        for err in &resp.errors {
            tracing::warn!(
                error = %err.message,
                path = ?err.path,
                "graphql resolver error"
            );
        }
        resp
    }
}

fn state<'a>(ctx: &Context<'a>) -> &'a AppState {
    ctx.data_unchecked::<std::sync::Arc<AppState>>()
}

/// The current session token, if the request carried one.
fn token(ctx: &Context<'_>) -> Option<String> {
    ctx.data_opt::<RequestAuth>().and_then(|a| a.0.clone())
}

/// The request's client IP for rate-limiting, or `"unknown"` when it could not
/// be resolved (all such requests then share one conservative budget).
fn client_ip(ctx: &Context<'_>) -> String {
    ctx.data_opt::<ClientIp>()
        .and_then(|c| c.0.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Resolve the authenticated user, or `None` if the request is anonymous.
async fn current_user(ctx: &Context<'_>) -> Option<User> {
    let tok = token(ctx)?;
    auth::user_for_token(&state(ctx).pool, &tok)
        .await
        .ok()
        .flatten()
}

/// Resolve the authenticated user or fail — for mutations that require sign-in.
async fn require_user(ctx: &Context<'_>) -> Result<User> {
    current_user(ctx)
        .await
        .ok_or_else(|| Error::new("Not authenticated"))
}

/// Resolve the authenticated user and require the admin flag — for the console.
async fn require_admin(ctx: &Context<'_>) -> Result<User> {
    let user = require_user(ctx).await?;
    if user.is_admin == 0 {
        return Err(Error::new("Admin access required"));
    }
    Ok(user)
}

fn gql_err(e: impl std::fmt::Display) -> Error {
    Error::new(e.to_string())
}

/// Aggregate the stored reviews for a series into a `RatingSummary`.
async fn rating_summary(pool: &SqlitePool, series_id: &str) -> RatingSummary {
    let scores: Vec<i64> = sqlx::query_scalar("SELECT score FROM reviews WHERE series_id = ?")
        .bind(series_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    if scores.is_empty() {
        return RatingSummary::empty();
    }
    let mut dist = vec![0i32; 10];
    let mut sum = 0i64;
    for s in &scores {
        sum += *s;
        let idx = (*s - 1).clamp(0, 9) as usize;
        dist[idx] += 1;
    }
    RatingSummary {
        average: sum as f64 / scores.len() as f64,
        count: scores.len() as i32,
        distribution: dist,
    }
}

/// Komika-native per-series admin overrides (from `series_admin`).
#[derive(Default, sqlx::FromRow)]
struct AdminOverrides {
    override_interval_hours: Option<f64>,
    poll_every_minutes: Option<i64>,
    paused_override: Option<i64>,
    status_override: Option<String>,
}

/// Alt titles for a federated Suwayomi series from the canonical model (CATALOGUE.md
/// §6). Once a series has been added to a canonical `work` (Tier-2 add flow, or a
/// MangaDex spine entry it resolved to), its `work_alias` rows surface here as the
/// series' alt titles. Empty when the series hasn't been catalogued yet — the reader
/// then just shows no alt titles, exactly as before this wiring.
async fn canonical_alt_titles(pool: &SqlitePool, suwayomi_id: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT wa.raw_title \
         FROM source_series ss \
         JOIN work_alias wa ON wa.work_id = ss.work_id \
         WHERE ss.source_type = 'suwayomi' AND ss.source_key = ? \
         ORDER BY wa.raw_title",
    )
    .bind(suwayomi_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

/// Whether a federated Suwayomi series is NSFW per the canonical model (CATALOGUE.md
/// §2). True once it's linked to a `work` flagged NSFW; false when uncatalogued (we
/// only hide what we positively know is NSFW).
async fn canonical_is_nsfw(pool: &SqlitePool, suwayomi_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(w.is_nsfw), 0) FROM source_series ss \
         JOIN work w ON w.id = ss.work_id \
         WHERE ss.source_type = 'suwayomi' AND ss.source_key = ?",
    )
    .bind(suwayomi_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0)
        != 0
}

/// The viewer's NSFW preference (default false, including for anonymous requests).
async fn viewer_show_nsfw(ctx: &Context<'_>) -> bool {
    match current_user(ctx).await {
        Some(u) => user_show_nsfw(&state(ctx).pool, &u.id).await,
        None => false,
    }
}

/// Read a user's persisted `show_nsfw` flag (default false on any lookup failure).
async fn user_show_nsfw(pool: &SqlitePool, user_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT show_nsfw FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
        != 0
}

/// Drop NSFW series from a feed unless the viewer opted in. Pure over the already-set
/// `Series.is_nsfw`, so it's one canonical lookup per series (in `map_series`), not per feed.
fn filter_nsfw(show_nsfw: bool, items: Vec<Series>) -> Vec<Series> {
    if show_nsfw {
        return items;
    }
    items.into_iter().filter(|s| !s.is_nsfw).collect()
}

async fn admin_overrides(pool: &SqlitePool, series_id: &str) -> AdminOverrides {
    sqlx::query_as::<_, AdminOverrides>(
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

/// Map a federated Suwayomi manga onto a Komika `Series`, folding in the
/// Komika-native rating aggregate + admin overrides. (Both are per-series lookups
/// — fine for the small result sets here; batch them if list sizes grow.)
async fn map_series(st: &AppState, m: SuwayomiManga) -> Series {
    let id = m.id.to_string();
    let rating = rating_summary(&st.pool, &id).await;
    let ov = admin_overrides(&st.pool, &id).await;
    let scan = scan_state(&st.pool, &id).await;
    // Canonical alt titles (empty until the series is catalogued); drop any that
    // equal the primary title so the reader shows only genuine alternatives.
    let mut alt_titles = canonical_alt_titles(&st.pool, &id).await;
    alt_titles.retain(|t| t != &m.title);
    let is_nsfw = canonical_is_nsfw(&st.pool, &id).await;

    // Status: admin override wins over the source-derived status.
    let status = ov
        .status_override
        .as_deref()
        .and_then(komika_status)
        .unwrap_or_else(|| status_from(&m.status));
    // Paused: forced override wins; otherwise auto-pause by effective status.
    let paused = ov
        .paused_override
        .map(|v| v != 0)
        .unwrap_or_else(|| paused_for_status(status));

    Series {
        cover_url: st.suwayomi.abs(m.thumbnail_url.as_deref()),
        rating,
        scan: ScanPolicy {
            // Derived scan state (from the background scheduler) wins when present;
            // else fall back to Suwayomi's lastFetchedAt for last_scanned_at.
            avg_interval_hours: scan.as_ref().map(|s| s.avg_interval_hours).unwrap_or(0.0),
            override_interval_hours: ov.override_interval_hours,
            poll_every_minutes: ov.poll_every_minutes.map(|v| v as i32).unwrap_or(30),
            paused,
            status_override: ov.status_override.as_deref().and_then(komika_status),
            paused_override: ov.paused_override.map(|v| v != 0),
            last_scanned_at: scan
                .as_ref()
                .and_then(|s| s.last_scanned_at.clone())
                .or_else(|| to_iso(m.last_fetched_at.as_deref())),
            next_scan_at: scan.as_ref().and_then(|s| s.next_scan_at.clone()),
        },
        r#type: type_from_lang(m.source.as_ref().and_then(|s| s.lang.as_deref())),
        status,
        created_at: to_iso(m.in_library_at.as_deref()).unwrap_or_default(),
        updated_at: to_iso(m.last_fetched_at.as_deref()).unwrap_or_default(),
        chapter_count: m
            .chapters
            .as_ref()
            .map(|c| c.total_count as i32)
            .unwrap_or(0),
        is_marked: m.in_library,
        is_nsfw,
        source_id: m.source_id,
        genres: m.genre,
        author: m.author,
        artist: m.artist,
        description: m.description,
        alt_titles,
        title: m.title,
        id: ID(id),
    }
}

async fn map_series_list(st: &AppState, list: Vec<SuwayomiManga>) -> Vec<Series> {
    let mut out = Vec::with_capacity(list.len());
    for m in list {
        out.push(map_series(st, m).await);
    }
    out
}

/// Map a canonical `work` (MangaDex-mirrored) onto the shared `Series` shape so the
/// reader reuses its existing series/reader components (CATALOGUE.md §6). The series
/// `id` is the work id (its `w_` prefix distinguishes it from a numeric Suwayomi id,
/// so the reader routes it down the canonical path). Cover URLs point at
/// `uploads.mangadex.org` — the client resolves them through the Worker proxy.
/// Fields Komika doesn't mirror for MangaDex works (genres, ratings, library/scan
/// state) are empty/defaulted; reading is fully functional without them.
fn map_canonical_series(work: catalog::CanonicalWork, chapter_count: i32) -> Series {
    let cover_url = match (&work.mangadex_id, &work.cover_file_name) {
        (Some(mid), Some(fname)) => crate::mangadex::cover_thumb_url(mid, fname),
        _ => String::new(),
    };
    let title = work.primary_title.clone().unwrap_or_default();
    let mut alt_titles = work.alt_titles;
    alt_titles.retain(|t| t != &title);
    let status = work
        .status
        .as_deref()
        .and_then(komika_status)
        .unwrap_or(SeriesStatus::Unknown);
    Series {
        id: ID(work.work_id),
        title,
        alt_titles,
        author: work.author,
        artist: work.artist,
        description: work.description,
        genres: Vec::new(),
        r#type: type_from_lang(work.original_language.as_deref()),
        status,
        cover_url,
        source_id: "mangadex".to_string(),
        chapter_count,
        is_marked: false,
        is_nsfw: work.is_nsfw,
        rating: RatingSummary::empty(),
        scan: ScanPolicy {
            avg_interval_hours: 0.0,
            override_interval_hours: None,
            poll_every_minutes: 30,
            paused: false,
            status_override: None,
            paused_override: None,
            last_scanned_at: None,
            next_scan_at: None,
        },
        created_at: work.created_at,
        updated_at: work.updated_at,
    }
}

/// Map a mirrored MangaDex chapter onto the shared `Chapter` shape. The chapter `id`
/// is the MangaDex chapter uuid (the key `canonicalPages` fetches pages with);
/// `series_id` is the work id so navigation stays on the canonical path. Per-user
/// reading state isn't tracked for canonical works yet (defaults to unread).
fn map_canonical_chapter(work_id: &str, c: catalog::CanonicalChapter) -> Chapter {
    let number = c
        .number
        .as_deref()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    Chapter {
        id: ID(c.external_id),
        series_id: ID(work_id.to_string()),
        number,
        title: c.title,
        page_count: 0, // unknown until the at-home page list is fetched
        uploaded_at: c.published_at,
        scanlator: None,
        read: false,
        last_page_read: 0,
        bookmarked: false,
        is_downloaded: false,
    }
}

// ---- DB row shapes for social joins ----------------------------------------

#[derive(sqlx::FromRow)]
struct ReviewJoin {
    id: String,
    series_id: String,
    score: i64,
    body: String,
    has_spoiler: i64,
    created_at: String,
    updated_at: String,
    author_id: String,
    author_username: String,
    author_avatar: Option<String>,
}

impl From<ReviewJoin> for Review {
    fn from(r: ReviewJoin) -> Self {
        Review {
            id: ID(r.id),
            series_id: ID(r.series_id),
            author: UserRef {
                id: ID(r.author_id),
                username: r.author_username,
                avatar_url: r.author_avatar,
            },
            score: r.score as i32,
            body: r.body,
            has_spoiler: r.has_spoiler != 0,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Validate a comment target type, returning the canonical `&'static str` to bind.
/// Guards the polymorphic `comments.target_type` against arbitrary values.
fn validate_comment_target(target_type: &str) -> Result<&'static str> {
    match target_type {
        "chapter" => Ok("chapter"),
        "series" => Ok("series"),
        other => Err(Error::new(format!(
            "invalid comment target type: {other:?} (expected \"chapter\" or \"series\")"
        ))),
    }
}

#[derive(sqlx::FromRow)]
struct CommentJoin {
    id: String,
    target_type: String,
    target_id: String,
    body: String,
    has_spoiler: i64,
    created_at: String,
    author_id: String,
    author_username: String,
    author_avatar: Option<String>,
}

impl From<CommentJoin> for Comment {
    fn from(c: CommentJoin) -> Self {
        Comment {
            id: ID(c.id),
            target_type: c.target_type,
            target_id: ID(c.target_id),
            author: UserRef {
                id: ID(c.author_id),
                username: c.author_username,
                avatar_url: c.author_avatar,
            },
            body: c.body,
            has_spoiler: c.has_spoiler != 0,
            created_at: c.created_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct AdminUserRow {
    id: String,
    username: String,
    email: String,
    avatar_url: Option<String>,
    is_admin: i64,
    is_banned: i64,
    created_at: String,
}

impl From<AdminUserRow> for AdminUser {
    fn from(r: AdminUserRow) -> Self {
        AdminUser {
            id: ID(r.id),
            username: r.username,
            email: r.email,
            avatar_url: r.avatar_url,
            is_admin: r.is_admin != 0,
            is_banned: r.is_banned != 0,
            created_at: r.created_at,
        }
    }
}

fn session_user(u: &User, show_nsfw: bool) -> SessionUser {
    SessionUser {
        id: ID(u.id.clone()),
        username: u.username.clone(),
        avatar_url: u.avatar_url.clone(),
        is_admin: u.is_admin != 0,
        show_nsfw,
    }
}

// ---- Catalogue / dedup (CATALOGUE.md §4, §6) -------------------------------

/// Outcome of running the dedup matcher for a newly-added Tier-2 source series.
/// `decision` is one of `auto_merge` | `review` | `new`.
#[derive(SimpleObject)]
pub struct MatchResult {
    pub decision: String,
    /// The canonical work the series was linked to (its own new work for `new`).
    pub work_id: String,
    /// The matched canonical work (for `auto_merge` / `review`); absent for `new`.
    pub matched_work_id: Option<String>,
    pub score: Option<f64>,
    pub method: Option<String>,
    pub source_series_id: String,
}

/// One row of the canonical updates feed: a mirrored MangaDex work with its most
/// recent stored chapter (CATALOGUE.md §6). Served from the `chapter` mirror, not a
/// live Suwayomi round-trip. `work_id` opens the work through the canonical reader
/// path (`canonicalSeries`); `cover_url` is a proxy-ready MangaDex cover thumbnail.
#[derive(SimpleObject, sqlx::FromRow)]
pub struct CanonicalUpdate {
    pub work_id: String,
    pub mangadex_id: String,
    pub title: Option<String>,
    pub is_nsfw: bool,
    /// Proxy-ready cover thumbnail URL (`uploads.mangadex.org`), or null if the cover
    /// fileName hasn't been synced yet. The client resolves it through the Worker.
    pub cover_url: Option<String>,
    /// Latest stored chapter number (string — chapters can be "10.5").
    pub latest_chapter: Option<String>,
    pub latest_chapter_title: Option<String>,
    /// Publish time of the latest stored chapter (falls back to ingest time).
    pub latest_at: Option<String>,
}

/// A pending mid-confidence match awaiting manual admin review.
#[derive(SimpleObject, sqlx::FromRow)]
pub struct MergeCandidate {
    pub id: String,
    pub source_series_id: String,
    pub candidate_work_id: String,
    pub candidate_title: Option<String>,
    pub source_title: Option<String>,
    pub score: f64,
    pub method: String,
    pub status: String,
    pub created_at: String,
}

// ---- Query -----------------------------------------------------------------

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Curated discovery feeds over the federated catalog.
    async fn discovery(&self, ctx: &Context<'_>) -> Result<Vec<DiscoveryFeed>> {
        let st = state(ctx);
        let popular = st
            .suwayomi
            .fetch_source(FetchType::Popular, 1, None)
            .await
            .map_err(gql_err)?
            .1;
        let latest = st
            .suwayomi
            .fetch_source(FetchType::Latest, 1, None)
            .await
            .map(|r| r.1)
            .unwrap_or_default();

        // Hide NSFW-flagged works unless the viewer opted in (CATALOGUE.md §2).
        let show_nsfw = viewer_show_nsfw(ctx).await;
        let popular = filter_nsfw(show_nsfw, map_series_list(st, popular).await);
        let latest = filter_nsfw(show_nsfw, map_series_list(st, latest).await);

        let mut feeds = vec![
            DiscoveryFeed {
                kind: DiscoveryFeedKind::Popular,
                title: "Popular".into(),
                genre: None,
                items: popular.clone(),
            },
            DiscoveryFeed {
                kind: DiscoveryFeedKind::Trending,
                title: "Trending".into(),
                genre: None,
                items: popular.iter().take(6).cloned().collect(),
            },
        ];
        if !latest.is_empty() {
            feeds.push(DiscoveryFeed {
                kind: DiscoveryFeedKind::RecentlyUpdated,
                title: "Latest Updates".into(),
                genre: None,
                items: latest.clone(),
            });
            feeds.push(DiscoveryFeed {
                kind: DiscoveryFeedKind::RecentlyAdded,
                title: "Latest Added".into(),
                genre: None,
                items: latest.iter().take(6).cloned().collect(),
            });
        }
        Ok(feeds)
    }

    /// The reader's Updates feed: library series the adaptive scanner has
    /// detected new chapters for, newest-first. Driven by
    /// `series_scan_state.last_new_chapter_at` (written by `scanner::scan_series`)
    /// — this reflects OUR scanner, NOT Suwayomi's source "Latest" endpoint.
    async fn updates(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 1)] page: i32,
    ) -> Result<SeriesPage> {
        let st = state(ctx);
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        // Series ids with a detected new-chapter timestamp, newest-first. Fetch one
        // extra to compute has_next without a second round-trip.
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT series_id FROM series_scan_state \
             WHERE last_new_chapter_at IS NOT NULL \
             ORDER BY last_new_chapter_at DESC, series_id ASC LIMIT ? OFFSET ?",
        )
        .bind(PAGE_SIZE + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM series_scan_state WHERE last_new_chapter_at IS NOT NULL",
        )
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        let has_next = ids.len() as i64 > PAGE_SIZE;

        // Hydrate each id from Suwayomi. A series that has since been removed from
        // the source is skipped rather than failing the whole feed.
        let mut items = Vec::new();
        for id in ids.into_iter().take(PAGE_SIZE as usize) {
            let Ok(n) = id.parse::<i64>() else { continue };
            match st.suwayomi.series(n).await {
                Ok(m) => items.push(map_series(st, m).await),
                Err(e) => {
                    tracing::warn!(series_id = id, error = %e, "updates: skipping unresolvable series")
                }
            }
        }
        let items = filter_nsfw(viewer_show_nsfw(ctx).await, items);
        Ok(SeriesPage {
            items,
            page,
            has_next_page: has_next,
            total: Some(total as i32),
        })
    }

    /// Canonical updates feed: recently-updated mirrored MangaDex works with their
    /// latest stored chapter, newest first (CATALOGUE.md §6). Served from the `chapter`
    /// mirror (no live Suwayomi round-trip) and NSFW-filtered by the viewer's
    /// preference. Data-only — see `CanonicalUpdate`.
    async fn canonical_updates(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 1)] page: i32,
    ) -> Result<Vec<CanonicalUpdate>> {
        let st = state(ctx);
        let show_nsfw = viewer_show_nsfw(ctx).await;
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        // SQLite bare-column-with-MAX: latest_chapter / title / mangadex_id are taken
        // from the row holding MAX(latest_at) within each work group.
        let rows = sqlx::query_as::<_, CanonicalUpdate>(
            "SELECT ss.work_id AS work_id, ss.source_key AS mangadex_id, \
                    w.primary_title AS title, w.is_nsfw AS is_nsfw, \
                    CASE WHEN w.cover_file_name IS NOT NULL \
                         THEN 'https://uploads.mangadex.org/covers/' || ss.source_key || '/' \
                              || w.cover_file_name || '.512.jpg' \
                         ELSE NULL END AS cover_url, \
                    c.number AS latest_chapter, c.title AS latest_chapter_title, \
                    MAX(COALESCE(c.published_at, c.created_at)) AS latest_at \
             FROM chapter c \
             JOIN source_series ss ON ss.id = c.source_series_id \
             JOIN work w ON w.id = ss.work_id \
             WHERE ss.source_type = 'mangadex' AND (? = 1 OR w.is_nsfw = 0) \
             GROUP BY ss.work_id \
             ORDER BY latest_at DESC \
             LIMIT ? OFFSET ?",
        )
        .bind(show_nsfw as i64)
        .bind(PAGE_SIZE)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(rows)
    }

    /// Canonical reader path — a MangaDex-mirrored `work` as a `Series` (CATALOGUE.md §6).
    /// `workId` is the `w_`-prefixed canonical id (distinct from numeric Suwayomi ids).
    /// NSFW works are hidden unless the viewer opted in (same gate as the feeds). Reuses
    /// the `Series` shape so the reader's existing components render it unchanged.
    async fn canonical_series(&self, ctx: &Context<'_>, work_id: ID) -> Result<Series> {
        let st = state(ctx);
        let work = catalog::load_canonical_work(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?
            .ok_or_else(|| Error::new("No such work"))?;
        if work.is_nsfw && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such work"));
        }
        let chapters = catalog::load_canonical_chapters(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?;
        Ok(map_canonical_series(work, chapters.len() as i32))
    }

    /// Chapters of a canonical work, from the stored `chapter` mirror, deduped to one
    /// row per number (English preferred) and ordered ascending (CATALOGUE.md §6). Same
    /// NSFW gate as `canonicalSeries`.
    async fn canonical_chapters(&self, ctx: &Context<'_>, work_id: ID) -> Result<Vec<Chapter>> {
        let st = state(ctx);
        let work = catalog::load_canonical_work(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?
            .ok_or_else(|| Error::new("No such work"))?;
        if work.is_nsfw && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such work"));
        }
        let chapters = catalog::load_canonical_chapters(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?;
        Ok(chapters
            .into_iter()
            .map(|c| map_canonical_chapter(&work_id.0, c))
            .collect())
    }

    /// Ordered page URLs for a mirrored MangaDex chapter, via MangaDex@Home
    /// (CATALOGUE.md §5). `chapterId` is the MangaDex chapter uuid. The URLs are
    /// `*.mangadex.network` hosts the client resolves through the Worker proxy (never
    /// hotlinked). NSFW-gated by the owning work when the chapter is in the mirror.
    async fn canonical_pages(&self, ctx: &Context<'_>, chapter_id: ID) -> Result<Vec<Page>> {
        let st = state(ctx);
        // Gate on the owning work's NSFW flag when we know the chapter; an unknown
        // chapter (not in the mirror) is allowed through to the at-home fetch, which
        // fails cleanly if the id is bogus.
        if let Some(true) = catalog::chapter_owner_is_nsfw(&st.pool, &chapter_id.0)
            .await
            .map_err(gql_err)?
        {
            if !viewer_show_nsfw(ctx).await {
                return Err(Error::new("No such chapter"));
            }
        }
        let urls = st.mangadex.at_home(&chapter_id.0).await.map_err(gql_err)?;
        Ok(urls
            .into_iter()
            .enumerate()
            .map(|(index, source_url)| Page {
                index: index as i32,
                source_url,
                width: None,
                height: None,
            })
            .collect())
    }

    async fn search(
        &self,
        ctx: &Context<'_>,
        query: String,
        #[graphql(default = 1)] page: i32,
    ) -> Result<SeriesPage> {
        let st = state(ctx);
        let trimmed = query.trim();
        let (ty, q) = if trimmed.is_empty() {
            (FetchType::Popular, None)
        } else {
            (FetchType::Search, Some(trimmed))
        };
        let (has_next, mangas) = st
            .suwayomi
            .fetch_source(ty, page, q)
            .await
            .map_err(gql_err)?;
        Ok(SeriesPage {
            items: filter_nsfw(
                viewer_show_nsfw(ctx).await,
                map_series_list(st, mangas).await,
            ),
            page,
            has_next_page: has_next,
            total: None,
        })
    }

    async fn series(&self, ctx: &Context<'_>, id: ID) -> Result<Series> {
        let st = state(ctx);
        let n = id.0.parse::<i64>().map_err(gql_err)?;
        let m = st.suwayomi.series(n).await.map_err(gql_err)?;
        Ok(map_series(st, m).await)
    }

    async fn chapters(&self, ctx: &Context<'_>, series_id: ID) -> Result<Vec<Chapter>> {
        let st = state(ctx);
        let n = series_id.0.parse::<i64>().map_err(gql_err)?;
        let list = st.suwayomi.chapters(n).await.map_err(gql_err)?;
        Ok(list.into_iter().map(map_chapter).collect())
    }

    async fn pages(&self, ctx: &Context<'_>, chapter_id: ID) -> Result<Vec<Page>> {
        let st = state(ctx);
        let n = chapter_id.0.parse::<i64>().map_err(gql_err)?;
        let urls = st.suwayomi.pages(n).await.map_err(gql_err)?;
        Ok(urls
            .into_iter()
            .enumerate()
            .map(|(index, source_url)| Page {
                index: index as i32,
                source_url,
                width: None,
                height: None,
            })
            .collect())
    }

    async fn library(&self, ctx: &Context<'_>) -> Result<Vec<Series>> {
        let st = state(ctx);
        let list = st.suwayomi.library().await.map_err(gql_err)?;
        Ok(map_series_list(st, list).await)
    }

    async fn reviews(
        &self,
        ctx: &Context<'_>,
        series_id: ID,
        #[graphql(default = 1)] page: i32,
    ) -> Result<ReviewPage> {
        let st = state(ctx);
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        let rows: Vec<ReviewJoin> = sqlx::query_as(
            "SELECT r.id, r.series_id, r.score, r.body, r.has_spoiler, r.created_at, r.updated_at, \
             u.id AS author_id, u.username AS author_username, u.avatar_url AS author_avatar \
             FROM reviews r JOIN users u ON u.id = r.user_id \
             WHERE r.series_id = ? ORDER BY r.created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(series_id.0.clone())
        .bind(PAGE_SIZE + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reviews WHERE series_id = ?")
            .bind(series_id.0.clone())
            .fetch_one(&st.pool)
            .await
            .map_err(gql_err)?;
        let has_next = rows.len() as i64 > PAGE_SIZE;
        let items = rows
            .into_iter()
            .take(PAGE_SIZE as usize)
            .map(Review::from)
            .collect();
        Ok(ReviewPage {
            items,
            page,
            has_next_page: has_next,
            total: Some(total as i32),
        })
    }

    async fn comments(
        &self,
        ctx: &Context<'_>,
        target_type: String,
        target_id: ID,
        #[graphql(default = 1)] page: i32,
    ) -> Result<CommentPage> {
        let target_type = validate_comment_target(&target_type)?;
        let st = state(ctx);
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        let rows: Vec<CommentJoin> = sqlx::query_as(
            "SELECT c.id, c.target_type, c.target_id, c.body, c.has_spoiler, c.created_at, \
             u.id AS author_id, u.username AS author_username, u.avatar_url AS author_avatar \
             FROM comments c JOIN users u ON u.id = c.user_id \
             WHERE c.target_type = ? AND c.target_id = ? ORDER BY c.created_at ASC LIMIT ? OFFSET ?",
        )
        .bind(target_type)
        .bind(target_id.0.clone())
        .bind(PAGE_SIZE + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM comments WHERE target_type = ? AND target_id = ?",
        )
        .bind(target_type)
        .bind(target_id.0.clone())
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        let has_next = rows.len() as i64 > PAGE_SIZE;
        let items = rows
            .into_iter()
            .take(PAGE_SIZE as usize)
            .map(Comment::from)
            .collect();
        Ok(CommentPage {
            items,
            page,
            has_next_page: has_next,
            total: Some(total as i32),
        })
    }

    /// Aggregate health of the background scan scheduler (admin console).
    async fn scan_status(&self, ctx: &Context<'_>) -> Result<ScanStatus> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let (library_size, overdue_count, last_tick_at) = {
            let h = st.scan_health.lock().unwrap();
            (
                h.library_size as i32,
                h.overdue_count as i32,
                h.last_tick_at.clone(),
            )
        };
        // Earliest upcoming next_scan_at across all tracked series.
        let next_due_at: Option<String> = sqlx::query_scalar(
            "SELECT MIN(next_scan_at) FROM series_scan_state WHERE next_scan_at IS NOT NULL",
        )
        .fetch_optional(&st.pool)
        .await
        .ok()
        .flatten();
        Ok(ScanStatus {
            library_size,
            overdue_count,
            last_tick_at,
            next_due_at,
        })
    }

    /// Admin user-management console: a paginated list of accounts, newest first.
    async fn users(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 1)] page: i32,
    ) -> Result<AdminUserPage> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        let rows: Vec<AdminUserRow> = sqlx::query_as(
            "SELECT id, username, email, avatar_url, is_admin, is_banned, created_at \
             FROM users ORDER BY created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(PAGE_SIZE + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&st.pool)
            .await
            .map_err(gql_err)?;
        let has_next = rows.len() as i64 > PAGE_SIZE;
        let items = rows
            .into_iter()
            .take(PAGE_SIZE as usize)
            .map(AdminUser::from)
            .collect();
        Ok(AdminUserPage {
            items,
            page,
            has_next_page: has_next,
            total: Some(total as i32),
        })
    }

    /// Admin dedup review queue: pending mid-confidence matches, newest first, with
    /// the candidate work's title and the source series' current title for context.
    async fn merge_queue(&self, ctx: &Context<'_>) -> Result<Vec<MergeCandidate>> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let rows = sqlx::query_as::<_, MergeCandidate>(
            "SELECT mc.id, mc.source_series_id, mc.candidate_work_id, \
                    cw.primary_title AS candidate_title, sw.primary_title AS source_title, \
                    mc.score, mc.method, mc.status, mc.created_at \
             FROM merge_candidate mc \
             JOIN work cw ON cw.id = mc.candidate_work_id \
             JOIN source_series ss ON ss.id = mc.source_series_id \
             JOIN work sw ON sw.id = ss.work_id \
             WHERE mc.status = 'pending' \
             ORDER BY mc.created_at DESC LIMIT 200",
        )
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(rows)
    }

    async fn session(&self, ctx: &Context<'_>) -> Result<Option<Session>> {
        let Some(tok) = token(ctx) else {
            return Ok(None);
        };
        match current_user(ctx).await {
            Some(u) => {
                let show_nsfw = user_show_nsfw(&state(ctx).pool, &u.id).await;
                Ok(Some(Session {
                    token: tok,
                    user: session_user(&u, show_nsfw),
                }))
            }
            None => Ok(None),
        }
    }
}

// ---- Mutation --------------------------------------------------------------

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn mark(&self, ctx: &Context<'_>, series_id: ID, marked: bool) -> Result<Series> {
        let st = state(ctx);
        let n = series_id.0.parse::<i64>().map_err(gql_err)?;
        st.suwayomi
            .set_in_library(n, marked)
            .await
            .map_err(gql_err)?;
        let m = st.suwayomi.series(n).await.map_err(gql_err)?;
        Ok(map_series(st, m).await)
    }

    async fn set_progress(
        &self,
        ctx: &Context<'_>,
        chapter_id: ID,
        last_page_read: i32,
        read: bool,
    ) -> Result<bool> {
        if last_page_read < 0 {
            return Err(Error::new("lastPageRead must be non-negative"));
        }
        let st = state(ctx);
        let n = chapter_id.0.parse::<i64>().map_err(gql_err)?;
        st.suwayomi
            .set_progress(n, last_page_read as i64, read)
            .await
            .map_err(gql_err)?;
        Ok(true)
    }

    async fn post_review(&self, ctx: &Context<'_>, input: PostReviewInput) -> Result<Review> {
        if !(1..=10).contains(&input.score) {
            return Err(Error::new("score must be between 1 and 10"));
        }
        // An empty body is allowed: it represents a pure rating (the Series
        // rating widget) with no written review. The body-bearing reviews are
        // what the client shows as the comment thread.
        let user = require_user(ctx).await?;
        let st = state(ctx);
        let now = Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        // One review per (series, user): upsert, keeping the original id/created_at.
        sqlx::query(
            "INSERT INTO reviews (id, series_id, user_id, score, body, has_spoiler, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(series_id, user_id) DO UPDATE SET \
               score = excluded.score, body = excluded.body, \
               has_spoiler = excluded.has_spoiler, updated_at = excluded.updated_at",
        )
        .bind(&id)
        .bind(input.series_id.0.clone())
        .bind(&user.id)
        .bind(input.score as i64)
        .bind(input.body.trim())
        .bind(input.has_spoiler)
        .bind(&now)
        .bind(&now)
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;

        let row: ReviewJoin = sqlx::query_as(
            "SELECT r.id, r.series_id, r.score, r.body, r.has_spoiler, r.created_at, r.updated_at, \
             u.id AS author_id, u.username AS author_username, u.avatar_url AS author_avatar \
             FROM reviews r JOIN users u ON u.id = r.user_id \
             WHERE r.series_id = ? AND r.user_id = ?",
        )
        .bind(input.series_id.0.clone())
        .bind(&user.id)
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(row.into())
    }

    async fn post_comment(&self, ctx: &Context<'_>, input: PostCommentInput) -> Result<Comment> {
        let target_type = validate_comment_target(&input.target_type)?;
        if input.body.trim().is_empty() {
            return Err(Error::new("comment body must not be empty"));
        }
        let user = require_user(ctx).await?;
        let st = state(ctx);
        let now = Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO comments (id, target_type, target_id, user_id, body, has_spoiler, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(target_type)
        .bind(input.target_id.0.clone())
        .bind(&user.id)
        .bind(input.body.trim())
        .bind(input.has_spoiler)
        .bind(&now)
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(Comment {
            id: ID(id),
            target_type: target_type.to_string(),
            target_id: input.target_id,
            author: UserRef {
                id: ID(user.id),
                username: user.username,
                avatar_url: user.avatar_url,
            },
            body: input.body.trim().to_string(),
            has_spoiler: input.has_spoiler,
            created_at: now,
        })
    }

    async fn login(
        &self,
        ctx: &Context<'_>,
        username: String,
        password: String,
    ) -> Result<Session> {
        let st = state(ctx);
        // Rate-limit by client IP (not username, which would let an attacker lock
        // out a victim). Check before verifying, but only *record* failed attempts
        // below — successful logins must not consume the budget.
        let key = format!("login:{}", client_ip(ctx));
        if let Some(retry) = st.auth_limiter.is_limited(&key) {
            return Err(Error::new(format!(
                "Too many login attempts — try again in {retry}s"
            )));
        }
        let user = sqlx::query_as::<_, User>(
            "SELECT id, username, email, password_hash, avatar_url, is_admin, is_banned FROM users WHERE username = ?",
        )
        .bind(&username)
        .fetch_optional(&st.pool)
        .await
        .map_err(gql_err)?
        .filter(|u| auth::verify_password(&password, &u.password_hash));
        let user = match user {
            Some(u) => u,
            None => {
                st.auth_limiter.record(&key); // count only failed attempts
                return Err(Error::new("Invalid username or password"));
            }
        };
        // A suspended account can't sign in even with the right password. This
        // is not a failed credential attempt, so it doesn't consume the budget.
        if user.is_banned != 0 {
            return Err(Error::new("This account has been suspended."));
        }
        let tok = new_session(&st.pool, &user.id).await?;
        let show_nsfw = user_show_nsfw(&st.pool, &user.id).await;
        Ok(Session {
            token: tok,
            user: session_user(&user, show_nsfw),
        })
    }

    async fn register(&self, ctx: &Context<'_>, input: RegisterInput) -> Result<Session> {
        let st = state(ctx);
        // Rate-limit by client IP so one source can't mass-create accounts (and
        // can't grief a specific username's registration).
        if let Err(retry) = st
            .auth_limiter
            .check(&format!("register:{}", client_ip(ctx)))
        {
            return Err(Error::new(format!(
                "Too many registration attempts — try again in {retry}s"
            )));
        }
        let username = input.username.trim();
        let email = input.email.trim();
        if username.len() < 3 {
            return Err(Error::new("username must be at least 3 characters"));
        }
        if !email.contains('@') {
            return Err(Error::new("a valid email is required"));
        }
        if input.password.len() < 8 {
            return Err(Error::new("password must be at least 8 characters"));
        }
        let hash = auth::hash_password(&input.password).map_err(gql_err)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let is_admin = st
            .admin_users
            .iter()
            .any(|u| u.eq_ignore_ascii_case(username));
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash, avatar_url, is_admin, created_at) \
             VALUES (?, ?, ?, ?, NULL, ?, ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(email)
        .bind(&hash)
        .bind(is_admin as i64)
        .bind(&now)
        .execute(&st.pool)
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                Error::new("username or email already taken")
            } else {
                gql_err(e)
            }
        })?;
        let tok = new_session(&st.pool, &id).await?;
        Ok(Session {
            token: tok,
            user: SessionUser {
                id: ID(id),
                username: username.to_string(),
                avatar_url: None,
                is_admin,
                show_nsfw: false, // fresh accounts default to hiding NSFW
            },
        })
    }

    async fn logout(&self, ctx: &Context<'_>) -> Result<bool> {
        let st = state(ctx);
        if let Some(tok) = token(ctx) {
            sqlx::query("DELETE FROM sessions WHERE token = ?")
                .bind(&tok)
                .execute(&st.pool)
                .await
                .map_err(gql_err)?;
        }
        Ok(true)
    }

    /// Set the signed-in user's NSFW visibility preference (CATALOGUE.md §2).
    /// Returns the new value.
    async fn set_show_nsfw(&self, ctx: &Context<'_>, value: bool) -> Result<bool> {
        let user = require_user(ctx).await?;
        sqlx::query("UPDATE users SET show_nsfw = ? WHERE id = ?")
            .bind(value as i64)
            .bind(&user.id)
            .execute(&state(ctx).pool)
            .await
            .map_err(gql_err)?;
        Ok(value)
    }

    /// Admin "manga DB" console: upsert the per-series overrides (whole-state;
    /// a null field clears that override) and return the recomputed series.
    async fn update_series_admin(
        &self,
        ctx: &Context<'_>,
        input: SeriesAdminInput,
    ) -> Result<Series> {
        require_admin(ctx).await?;
        // Reject absurd/invalid overrides up front (the scanner also clamps, but
        // give the admin a clean error instead of silently coercing).
        if let Some(hours) = input.override_interval_hours {
            if !hours.is_finite() || hours <= 0.0 || hours > 876_000.0 {
                return Err(Error::new(
                    "overrideIntervalHours must be between 0 and 876000 (100 years)",
                ));
            }
        }
        if let Some(poll) = input.poll_every_minutes {
            if poll <= 0 {
                return Err(Error::new("pollEveryMinutes must be a positive integer"));
            }
        }
        let st = state(ctx);
        let now = Utc::now().to_rfc3339();
        let status = input.status.map(status_word);
        let paused = input.paused.map(|p| p as i64);
        sqlx::query(
            "INSERT INTO series_admin \
               (series_id, override_interval_hours, poll_every_minutes, paused_override, status_override, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(series_id) DO UPDATE SET \
               override_interval_hours = excluded.override_interval_hours, \
               poll_every_minutes = excluded.poll_every_minutes, \
               paused_override = excluded.paused_override, \
               status_override = excluded.status_override, \
               updated_at = excluded.updated_at",
        )
        .bind(input.series_id.0.clone())
        .bind(input.override_interval_hours)
        .bind(input.poll_every_minutes)
        .bind(paused)
        .bind(status)
        .bind(&now)
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;

        let n = input.series_id.0.parse::<i64>().map_err(gql_err)?;
        let m = st.suwayomi.series(n).await.map_err(gql_err)?;
        Ok(map_series(st, m).await)
    }

    /// Admin: force an immediate re-scan of one series, bypassing the adaptive
    /// overdue/pause gating. Returns the series with its refreshed scan state.
    async fn trigger_scan(&self, ctx: &Context<'_>, series_id: ID) -> Result<Series> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let n = series_id.0.parse::<i64>().map_err(gql_err)?;
        let m = st.suwayomi.series(n).await.map_err(gql_err)?;
        scan_series(st, &m, Utc::now()).await.map_err(gql_err)?;
        Ok(map_series(st, m).await)
    }

    /// Admin moderation: suspend or restore a user account. A banned user can't
    /// sign in and their active sessions are revoked immediately. Admins can't
    /// ban themselves or another admin.
    async fn ban_user(&self, ctx: &Context<'_>, user_id: ID, banned: bool) -> Result<UserRef> {
        let admin = require_admin(ctx).await?;
        let st = state(ctx);
        if user_id.0 == admin.id {
            return Err(Error::new("You cannot ban your own account."));
        }
        let target: Option<(String, String, Option<String>, i64)> =
            sqlx::query_as("SELECT id, username, avatar_url, is_admin FROM users WHERE id = ?")
                .bind(&user_id.0)
                .fetch_optional(&st.pool)
                .await
                .map_err(gql_err)?;
        let Some((id, username, avatar_url, is_admin)) = target else {
            return Err(Error::new("No such user."));
        };
        if is_admin != 0 {
            return Err(Error::new("You cannot ban an admin account."));
        }
        sqlx::query("UPDATE users SET is_banned = ? WHERE id = ?")
            .bind(banned as i64)
            .bind(&id)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
        if banned {
            // Revoke active sessions so the ban takes effect at once.
            sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                .bind(&id)
                .execute(&st.pool)
                .await
                .map_err(gql_err)?;
        }
        Ok(UserRef {
            id: ID(id),
            username,
            avatar_url,
        })
    }

    /// Admin moderation: delete a chapter comment. Returns false if it was
    /// already gone. (Authors don't self-delete here — this is the mod action.)
    async fn delete_comment(&self, ctx: &Context<'_>, comment_id: ID) -> Result<bool> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let res = sqlx::query("DELETE FROM comments WHERE id = ?")
            .bind(&comment_id.0)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
        Ok(res.rows_affected() > 0)
    }

    /// Admin user-management: grant or revoke a user's admin flag. An admin can't
    /// revoke their own access (that could lock every admin out of the console).
    async fn set_user_admin(
        &self,
        ctx: &Context<'_>,
        user_id: ID,
        is_admin: bool,
    ) -> Result<AdminUser> {
        let admin = require_admin(ctx).await?;
        let st = state(ctx);
        if user_id.0 == admin.id && !is_admin {
            return Err(Error::new("You cannot remove your own admin access."));
        }
        let res = sqlx::query("UPDATE users SET is_admin = ? WHERE id = ?")
            .bind(is_admin as i64)
            .bind(&user_id.0)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
        if res.rows_affected() == 0 {
            return Err(Error::new("No such user."));
        }
        let row: AdminUserRow = sqlx::query_as(
            "SELECT id, username, email, avatar_url, is_admin, is_banned, created_at \
             FROM users WHERE id = ?",
        )
        .bind(&user_id.0)
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(row.into())
    }

    /// Tier-2 add flow (CATALOGUE.md §6): pull a source series' detail from Suwayomi,
    /// run the dedup matcher, and wire it into the canonical model. `auto_merge` links
    /// straight to the matched work; `review` creates a provisional work AND enqueues a
    /// `merge_candidate` for manual confirmation; `new` creates a first-class work.
    async fn add_source_series(
        &self,
        ctx: &Context<'_>,
        suwayomi_manga_id: ID,
    ) -> Result<MatchResult> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let mid: i64 = suwayomi_manga_id
            .0
            .parse()
            .map_err(|_| Error::new("suwayomiMangaId must be an integer id"))?;
        let m = st.suwayomi.series(mid).await.map_err(gql_err)?;

        let cand = crate::dedup::Candidate {
            title: m.title.clone(),
            alt_titles: Vec::new(),
            description: m.description.clone(),
            author: m.author.clone(),
            year: None,
            cover_phash: None,
            external_ids: Vec::new(),
        };
        let decision = crate::dedup::resolve(&st.pool, &cand)
            .await
            .map_err(gql_err)?;

        // The work this source series is (provisionally) linked to. For `new` and
        // `review` we mint a first-class work from the Suwayomi metadata.
        let make_work = || crate::catalog::WorkInput {
            primary_title: Some(m.title.clone()),
            description: m.description.clone(),
            author: m.author.clone(),
            artist: m.artist.clone(),
            status: Some(m.status.clone()),
            is_nsfw: false,
            aliases: vec![crate::catalog::Alias {
                raw: m.title.clone(),
                lang: None,
            }],
            ..Default::default()
        };

        use crate::dedup::Decision;
        let (decision_str, matched_work_id, score, method, work_id) = match &decision {
            Decision::AutoMerge {
                work_id,
                score,
                method,
            } => (
                "auto_merge",
                Some(work_id.clone()),
                Some(*score),
                Some(method.clone()),
                work_id.clone(),
            ),
            Decision::Review {
                work_id,
                score,
                method,
            } => {
                let provisional = crate::catalog::create_work(&st.pool, &make_work())
                    .await
                    .map_err(gql_err)?;
                (
                    "review",
                    Some(work_id.clone()),
                    Some(*score),
                    Some(method.clone()),
                    provisional,
                )
            }
            Decision::New => {
                let created = crate::catalog::create_work(&st.pool, &make_work())
                    .await
                    .map_err(gql_err)?;
                ("new", None, None, None, created)
            }
        };

        let ssid = crate::catalog::upsert_source_series(
            &st.pool,
            &work_id,
            "suwayomi",
            &m.source_id,
            &mid.to_string(),
            None,
            false,
        )
        .await
        .map_err(gql_err)?;

        if let Decision::Review {
            work_id: cand_work,
            score,
            method,
        } = &decision
        {
            crate::catalog::insert_merge_candidate(&st.pool, &ssid, cand_work, *score, method)
                .await
                .map_err(gql_err)?;
        }

        Ok(MatchResult {
            decision: decision_str.to_string(),
            work_id,
            matched_work_id,
            score,
            method,
            source_series_id: ssid,
        })
    }

    /// Resolve a pending dedup review. `accept` repoints the source series onto the
    /// candidate work and drops the now-orphaned provisional work; rejecting keeps the
    /// provisional work as a distinct first-class entry. Either way the row is closed.
    async fn resolve_merge_candidate(
        &self,
        ctx: &Context<'_>,
        id: ID,
        accept: bool,
    ) -> Result<bool> {
        require_admin(ctx).await?;
        let st = state(ctx);

        #[derive(sqlx::FromRow)]
        struct Row {
            source_series_id: String,
            candidate_work_id: String,
            status: String,
        }
        let row: Option<Row> = sqlx::query_as(
            "SELECT source_series_id, candidate_work_id, status FROM merge_candidate WHERE id = ?",
        )
        .bind(&id.0)
        .fetch_optional(&st.pool)
        .await
        .map_err(gql_err)?;
        let Some(row) = row else {
            return Err(Error::new("No such merge candidate."));
        };
        if row.status != "pending" {
            return Err(Error::new("This merge candidate is already resolved."));
        }

        if accept {
            let old_work: Option<String> =
                sqlx::query_scalar("SELECT work_id FROM source_series WHERE id = ?")
                    .bind(&row.source_series_id)
                    .fetch_optional(&st.pool)
                    .await
                    .map_err(gql_err)?;
            sqlx::query("UPDATE source_series SET work_id = ? WHERE id = ?")
                .bind(&row.candidate_work_id)
                .bind(&row.source_series_id)
                .execute(&st.pool)
                .await
                .map_err(gql_err)?;
            // Drop the provisional work if nothing else references it now.
            if let Some(old) = old_work {
                if old != row.candidate_work_id {
                    sqlx::query(
                        "DELETE FROM work WHERE id = ? \
                         AND NOT EXISTS (SELECT 1 FROM source_series WHERE work_id = ?)",
                    )
                    .bind(&old)
                    .bind(&old)
                    .execute(&st.pool)
                    .await
                    .map_err(gql_err)?;
                }
            }
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE merge_candidate SET status = ?, resolved_at = ? WHERE id = ?")
            .bind(if accept { "confirmed" } else { "rejected" })
            .bind(&now)
            .bind(&id.0)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
        Ok(true)
    }
}

/// Create a session row and return its opaque token.
async fn new_session(pool: &SqlitePool, user_id: &str) -> Result<String> {
    let tok = auth::generate_token();
    let now = Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO sessions (token, user_id, created_at) VALUES (?, ?, ?)")
        .bind(&tok)
        .bind(user_id)
        .bind(&now)
        .execute(pool)
        .await
        .map_err(gql_err)?;
    Ok(tok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    // ---- RateLimiter unit tests ----

    #[test]
    fn limiter_blocks_only_after_max_records() {
        let rl = RateLimiter::new(2, 60);
        assert!(rl.is_limited("k").is_none(), "fresh key is not limited");
        rl.record("k");
        assert!(rl.is_limited("k").is_none(), "1 < max still allowed");
        rl.record("k");
        assert!(rl.is_limited("k").is_some(), "2 >= max is limited");
        // a different key has its own budget
        assert!(rl.is_limited("other").is_none());
    }

    #[test]
    fn limiter_is_read_only_until_record() {
        let rl = RateLimiter::new(1, 60);
        // repeated reads never trip the limit on their own
        for _ in 0..5 {
            assert!(rl.is_limited("k").is_none());
        }
        rl.record("k");
        assert!(rl.is_limited("k").is_some());
    }

    // ---- GraphQL security integration tests (no Suwayomi needed) ----

    async fn seed_user(pool: &SqlitePool, id: &str, username: &str, is_admin: i64, is_banned: i64) {
        let hash = auth::hash_password("password123").unwrap();
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash, avatar_url, is_admin, is_banned, created_at) \
             VALUES (?, ?, ?, ?, NULL, ?, ?, '2020-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(username)
        .bind(format!("{username}@example.com"))
        .bind(&hash)
        .bind(is_admin)
        .bind(is_banned)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn setup() -> ApiSchema {
        setup_with_limit(100).await
    }

    async fn setup_with_limit(max: u32) -> ApiSchema {
        setup_full(max).await.0
    }

    /// Like `setup_with_limit`, but also hands back the pool so tests can seed
    /// canonical-catalogue rows directly.
    async fn setup_full(max: u32) -> (ApiSchema, SqlitePool) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        seed_user(&pool, "admin-id", "admin", 1, 0).await;
        seed_user(&pool, "bob-id", "bob", 0, 0).await;
        seed_user(&pool, "banned-id", "carol", 0, 1).await;
        for (tok, uid) in [("admintok", "admin-id"), ("bobtok", "bob-id")] {
            sqlx::query("INSERT INTO sessions (token, user_id, created_at) VALUES (?, ?, '2020-01-01T00:00:00Z')")
                .bind(tok)
                .bind(uid)
                .execute(&pool)
                .await
                .unwrap();
        }
        let state = std::sync::Arc::new(AppState {
            pool: pool.clone(),
            suwayomi: crate::suwayomi::SuwayomiClient::new("http://127.0.0.1:1".into(), None, None),
            mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new("test-ua", 5.0)),
            admin_users: vec![],
            scan_health: Mutex::new(ScanHealth::default()),
            auth_limiter: RateLimiter::new(max, 60),
        });
        (build_schema(state), pool)
    }

    async fn exec(
        schema: &ApiSchema,
        query: &str,
        token: Option<&str>,
        ip: &str,
    ) -> async_graphql::Response {
        let req = async_graphql::Request::new(query)
            .data(RequestAuth(token.map(|t| t.to_string())))
            .data(ClientIp(Some(ip.to_string())));
        schema.execute(req).await
    }

    fn first_error(resp: &async_graphql::Response) -> String {
        resp.errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_default()
    }

    fn data_json(resp: &async_graphql::Response) -> String {
        serde_json::to_string(&resp.data).unwrap()
    }

    #[tokio::test]
    async fn set_show_nsfw_persists_and_requires_auth() {
        let s = setup().await;
        // Default is hidden.
        let r = exec(
            &s,
            r#"{ session { user { showNsfw } } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(data_json(&r).contains("\"showNsfw\":false"));
        // Toggle on.
        let r = exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let r = exec(
            &s,
            r#"{ session { user { showNsfw } } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(data_json(&r).contains("\"showNsfw\":true"));
        // Anonymous cannot set it.
        let r = exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "Not authenticated");
    }

    async fn seed_canonical(pool: &SqlitePool, md_id: &str, title: &str, nsfw: bool, ch: &str) {
        let input = crate::catalog::WorkInput {
            primary_title: Some(title.to_string()),
            is_nsfw: nsfw,
            ..Default::default()
        };
        crate::catalog::upsert_work_from_mangadex(pool, md_id, &input)
            .await
            .unwrap();
        let ssid = crate::catalog::find_source_series_id(pool, "mangadex", "mangadex", md_id)
            .await
            .unwrap()
            .unwrap();
        crate::catalog::upsert_chapter(
            pool,
            &ssid,
            &crate::catalog::ChapterInput {
                external_id: format!("{md_id}-{ch}"),
                number: Some(ch.to_string()),
                published_at: Some(format!("2026-07-0{ch}T00:00:00Z")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn canonical_updates_filters_nsfw_by_preference() {
        let (s, pool) = setup_full(100).await;
        seed_canonical(&pool, "md-safe", "Safe Work", false, "2").await;
        seed_canonical(&pool, "md-nsfw", "Spicy Work", true, "1").await;

        // Default (hidden): only the safe work; newest chapter first.
        let r = exec(
            &s,
            r#"{ canonicalUpdates { title latestChapter isNsfw } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let json = data_json(&r);
        assert!(json.contains("Safe Work"), "{json}");
        assert!(
            !json.contains("Spicy Work"),
            "nsfw work must be hidden: {json}"
        );

        // Opt in → both appear.
        exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let r = exec(
            &s,
            r#"{ canonicalUpdates { title } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let json = data_json(&r);
        assert!(
            json.contains("Safe Work") && json.contains("Spicy Work"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn canonical_series_and_chapters_are_readable_and_nsfw_gated() {
        let (s, pool) = setup_full(100).await;
        // A safe work with a cover + two chapters, and an NSFW work.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-safe",
            &crate::catalog::WorkInput {
                primary_title: Some("Readable Work".into()),
                cover_file_name: Some("cover.jpg".into()),
                is_nsfw: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ssid = crate::catalog::find_source_series_id(&pool, "mangadex", "mangadex", "md-safe")
            .await
            .unwrap()
            .unwrap();
        for (ext, num) in [("md-ch-2", "2"), ("md-ch-1", "1")] {
            crate::catalog::upsert_chapter(
                &pool,
                &ssid,
                &crate::catalog::ChapterInput {
                    external_id: ext.into(),
                    number: Some(num.into()),
                    lang: Some("en".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let work_id: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-safe'")
                .fetch_one(&pool)
                .await
                .unwrap();

        // canonicalSeries maps the work; cover URL points at uploads.mangadex.org.
        let q = format!(
            r#"{{ canonicalSeries(workId: "{work_id}") {{ id title coverUrl sourceId chapterCount }} }}"#
        );
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let json = data_json(&r);
        assert!(json.contains("Readable Work"), "{json}");
        assert!(
            json.contains("uploads.mangadex.org/covers/md-safe/cover.jpg"),
            "{json}"
        );
        assert!(json.contains("\"chapterCount\":2"), "{json}");

        // canonicalChapters returns ordered chapters keyed by MangaDex uuid.
        let q = format!(
            r#"{{ canonicalChapters(workId: "{work_id}") {{ id number seriesId }} }}"#
        );
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let chs = data["canonicalChapters"].as_array().unwrap();
        assert_eq!(chs.len(), 2);
        assert_eq!(chs[0]["id"], serde_json::json!("md-ch-1"));
        assert_eq!(chs[0]["number"], serde_json::json!(1.0));
        assert_eq!(chs[0]["seriesId"], serde_json::json!(work_id));

        // An NSFW work is hidden from a viewer who hasn't opted in.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-nsfw",
            &crate::catalog::WorkInput {
                primary_title: Some("Spicy Work".into()),
                is_nsfw: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let nsfw_work: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-nsfw'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let q = format!(r#"{{ canonicalSeries(workId: "{nsfw_work}") {{ title }} }}"#);
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert_eq!(first_error(&r), "No such work");
        // After opting in, it resolves.
        exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "nsfw opt-in failed: {:?}", r.errors);
        assert!(data_json(&r).contains("Spicy Work"));
    }

    #[tokio::test]
    async fn login_succeeds_with_correct_password() {
        let s = setup().await;
        let r = exec(
            &s,
            r#"mutation { login(username:"bob", password:"password123") { token } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
    }

    #[tokio::test]
    async fn login_rejects_wrong_password() {
        let s = setup().await;
        let r = exec(
            &s,
            r#"mutation { login(username:"bob", password:"nope") { token } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "Invalid username or password");
    }

    #[tokio::test]
    async fn banned_user_cannot_login_even_with_correct_password() {
        let s = setup().await;
        let r = exec(
            &s,
            r#"mutation { login(username:"carol", password:"password123") { token } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "This account has been suspended.");
    }

    #[tokio::test]
    async fn rate_limit_keys_on_ip_and_ignores_successes() {
        let s = setup_with_limit(2).await;
        let wrong = r#"mutation { login(username:"bob", password:"nope") { token } }"#;
        let right = r#"mutation { login(username:"bob", password:"password123") { token } }"#;
        // two failures from the attacker IP exhaust its budget
        for _ in 0..2 {
            let r = exec(&s, wrong, None, "9.9.9.9").await;
            assert_eq!(first_error(&r), "Invalid username or password");
        }
        // third attempt from that IP is blocked...
        let blocked = exec(&s, right, None, "9.9.9.9").await;
        assert!(
            first_error(&blocked).contains("Too many login attempts"),
            "got: {}",
            first_error(&blocked)
        );
        // ...but the victim's own IP is unaffected (M1)
        let victim = exec(&s, right, None, "8.8.8.8").await;
        assert!(
            victim.errors.is_empty(),
            "cross-IP lockout: {:?}",
            victim.errors
        );
        // and repeated *successful* logins never trip the limit (M2)
        for _ in 0..5 {
            let r = exec(&s, right, None, "7.7.7.7").await;
            assert!(
                r.errors.is_empty(),
                "success counted against limit: {:?}",
                r.errors
            );
        }
    }

    #[tokio::test]
    async fn admin_only_query_is_gated() {
        let s = setup().await;
        let q = "{ scanStatus { librarySize } }";
        assert_eq!(
            first_error(&exec(&s, q, None, "1.1.1.1").await),
            "Not authenticated"
        );
        assert_eq!(
            first_error(&exec(&s, q, Some("bobtok"), "1.1.1.1").await),
            "Admin access required"
        );
        let ok = exec(&s, q, Some("admintok"), "1.1.1.1").await;
        assert!(ok.errors.is_empty(), "admin blocked: {:?}", ok.errors);
    }

    #[tokio::test]
    async fn delete_comment_requires_admin() {
        let s = setup().await;
        let q = r#"mutation { deleteComment(commentId:"nope") }"#;
        assert_eq!(
            first_error(&exec(&s, q, Some("bobtok"), "1.1.1.1").await),
            "Admin access required"
        );
        // admin: no such comment => false, no error
        let r = exec(&s, q, Some("admintok"), "1.1.1.1").await;
        assert!(r.errors.is_empty());
        assert_eq!(
            r.data.into_json().unwrap()["deleteComment"],
            serde_json::json!(false)
        );
    }

    #[tokio::test]
    async fn ban_user_guards() {
        let s = setup().await;
        // non-admin rejected
        assert_eq!(
            first_error(
                &exec(
                    &s,
                    r#"mutation { banUser(userId:"bob-id", banned:true) { id } }"#,
                    Some("bobtok"),
                    "1.1.1.1"
                )
                .await
            ),
            "Admin access required"
        );
        // admin can't ban self
        assert_eq!(
            first_error(
                &exec(
                    &s,
                    r#"mutation { banUser(userId:"admin-id", banned:true) { id } }"#,
                    Some("admintok"),
                    "1.1.1.1"
                )
                .await
            ),
            "You cannot ban your own account."
        );
        // admin can't ban another admin (bob promoted first would be needed; use nonexistent)
        assert_eq!(
            first_error(
                &exec(
                    &s,
                    r#"mutation { banUser(userId:"ghost", banned:true) { id } }"#,
                    Some("admintok"),
                    "1.1.1.1"
                )
                .await
            ),
            "No such user."
        );
        // admin bans bob -> bob can no longer log in
        let ban = exec(
            &s,
            r#"mutation { banUser(userId:"bob-id", banned:true) { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(ban.errors.is_empty(), "ban failed: {:?}", ban.errors);
        let login = exec(
            &s,
            r#"mutation { login(username:"bob", password:"password123") { token } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&login), "This account has been suspended.");
    }

    #[tokio::test]
    async fn set_user_admin_cannot_demote_self() {
        let s = setup().await;
        assert_eq!(
            first_error(
                &exec(
                    &s,
                    r#"mutation { setUserAdmin(userId:"admin-id", isAdmin:false) { isAdmin } }"#,
                    Some("admintok"),
                    "1.1.1.1"
                )
                .await
            ),
            "You cannot remove your own admin access."
        );
        // promoting bob works
        let r = exec(
            &s,
            r#"mutation { setUserAdmin(userId:"bob-id", isAdmin:true) { isAdmin } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "promote failed: {:?}", r.errors);
        assert_eq!(
            r.data.into_json().unwrap()["setUserAdmin"]["isAdmin"],
            serde_json::json!(true)
        );
    }

    #[tokio::test]
    async fn updates_counts_dated_scan_state_rows() {
        // Build state directly so we can seed series_scan_state, then query updates.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        for (sid, ts) in [
            ("10", Some("2026-02-01T00:00:00Z")),
            ("11", Some("2026-03-01T00:00:00Z")),
            ("12", None),
        ] {
            sqlx::query(
                "INSERT INTO series_scan_state \
                   (series_id, avg_interval_hours, known_chapter_count, last_new_chapter_at, updated_at) \
                 VALUES (?, 0, 0, ?, '2026-01-01T00:00:00Z')",
            )
            .bind(sid)
            .bind(ts)
            .execute(&pool)
            .await
            .unwrap();
        }
        let state = std::sync::Arc::new(AppState {
            pool,
            suwayomi: crate::suwayomi::SuwayomiClient::new("http://127.0.0.1:1".into(), None, None),
            mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new("test-ua", 5.0)),
            admin_users: vec![],
            scan_health: Mutex::new(ScanHealth::default()),
            auth_limiter: RateLimiter::new(100, 60),
        });
        let s = build_schema(state);
        let r = exec(
            &s,
            r#"{ updates { total hasNextPage items { id } } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "updates errored: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        // Two rows carry a last_new_chapter_at; the null one is excluded.
        assert_eq!(data["updates"]["total"], serde_json::json!(2));
        // Suwayomi is unreachable in tests, so hydration is skipped and items are
        // empty — but the count/pagination path is exercised.
        assert_eq!(data["updates"]["items"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn set_progress_rejects_negative_page() {
        let s = setup().await;
        let r = exec(
            &s,
            r#"mutation { setProgress(chapterId:"1", lastPageRead:-5, read:false) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "lastPageRead must be non-negative");
    }

    #[tokio::test]
    async fn update_series_admin_rejects_out_of_range() {
        let s = setup().await;
        assert!(first_error(
            &exec(
                &s,
                r#"mutation { updateSeriesAdmin(input:{seriesId:"3", pollEveryMinutes:0}) { id } }"#,
                Some("admintok"),
                "1.1.1.1"
            )
            .await
        )
        .contains("pollEveryMinutes"));
        assert!(first_error(
            &exec(
                &s,
                r#"mutation { updateSeriesAdmin(input:{seriesId:"3", overrideIntervalHours:1000000000}) { id } }"#,
                Some("admintok"),
                "1.1.1.1"
            )
            .await
        )
        .contains("overrideIntervalHours"));
    }
}
