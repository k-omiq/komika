pub mod types;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_graphql::{Context, EmptySubscription, Error, Object, Result, Schema, SimpleObject, ID};
use chrono::Utc;
use sqlx::SqlitePool;

use crate::auth::{self, User};
use crate::catalog;
use crate::scanner::scan_series;
use crate::suwayomi::{FetchType, SuwayomiChapter, SuwayomiClient, SuwayomiManga};
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
        // Recover from a poisoned lock rather than propagating the panic — a single
        // panic-while-held must not brick login/register for the process lifetime.
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        // Only inspect an existing entry — never insert on a read, or every
        // distinct client IP would leave a permanent key (unbounded RSS growth).
        let entry = map.get_mut(key)?;
        entry.retain(|t| now.duration_since(*t) < self.window);
        if entry.is_empty() {
            map.remove(key);
            return None;
        }
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
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        // Opportunistic sweep: when the map is large, drop keys whose window has
        // fully elapsed so a churn of distinct client IPs can't grow it without
        // bound (there is no background reaper).
        if map.len() >= 4096 {
            map.retain(|_, v| {
                v.retain(|t| now.duration_since(*t) < self.window);
                !v.is_empty()
            });
        }
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

/// Per-key single-flight locks (S1 TTL refresh). N concurrent misses/refreshes for
/// the same series/chapter id collapse onto ONE upstream fetch instead of
/// stampeding Suwayomi: each caller awaits the same per-key `tokio` mutex, and the
/// losers re-check the cache (now populated) after acquiring it. The outer map is
/// guarded by a std mutex held only for the O(1) lookup/insert — never across an
/// `await` — and dead entries are pruned via `Weak` so the map can't grow unbounded.
#[derive(Default)]
pub struct KeyedLocks {
    map: Mutex<HashMap<i64, std::sync::Weak<tokio::sync::Mutex<()>>>>,
}

impl KeyedLocks {
    /// Get (or create) the shared lock for `key`. Holding the returned `Arc`'s guard
    /// serializes the critical section for that key across tasks.
    pub fn lock_handle(&self, key: i64) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(&key).and_then(std::sync::Weak::upgrade) {
            return existing;
        }
        let arc = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        map.insert(key, std::sync::Arc::downgrade(&arc));
        // Opportunistically drop entries whose only reference was the (now-dead)
        // weak, so a long-lived process doesn't accumulate one entry per id ever seen.
        map.retain(|_, w| w.strong_count() > 0);
        arc
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
    /// Per-user sliding-window limiter for `searchAllSources` (C1). That endpoint
    /// fans out to many sources and performs writes (library enrollment + dedup
    /// persist), so an authenticated client must not be able to hammer it.
    pub federated_limiter: RateLimiter,
    /// Absolute session lifetime in seconds (see `Config::session_ttl_secs`).
    pub session_ttl_secs: i64,
    /// Single-flight locks collapsing concurrent Suwayomi series-metadata
    /// refreshes for the same id (S1 TTL refresh).
    pub series_inflight: KeyedLocks,
    /// Single-flight locks collapsing concurrent Suwayomi chapter-list refreshes
    /// for the same manga id (S1 TTL refresh).
    pub chapters_inflight: KeyedLocks,
}

/// Per-request auth: the bearer token from the `Authorization` header, if any.
#[derive(Clone, Default)]
pub struct RequestAuth(pub Option<String>);

/// Per-request client IP, resolved by the HTTP layer (X-Forwarded-For behind a
/// proxy, else the socket peer). Used to key the auth rate limiter so one actor
/// cannot exhaust another account's budget.
#[derive(Clone, Default)]
pub struct ClientIp(pub Option<String>);

/// Per-request memoization of the authenticated user. `current_user` is called
/// once per series in a feed (via `is_marked`/`viewer_show_nsfw`/…), so without a
/// cache a single `discovery`/`search`/`library` query does one `sessions⋈users`
/// lookup per item. A fresh `OnceCell` is attached per HTTP request, so the DB
/// lookup runs at most once regardless of how many resolvers ask.
#[derive(Clone, Default)]
pub struct RequestUserCache(pub std::sync::Arc<tokio::sync::OnceCell<Option<User>>>);

/// Upper bound on password length accepted by `login`/`register`, enforced
/// before hashing so an over-long password can't amplify Argon2 CPU cost (A7).
const MAX_PASSWORD_LEN: usize = 1024;

/// A fixed dummy Argon2 hash (of a random password nobody knows), used to run a
/// constant-work verify on the login missing-user path so response time doesn't
/// reveal whether a username exists (A3). Built once with the default params so
/// it costs the same as a real verify.
static DUMMY_PASSWORD_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    auth::hash_password("komika-constant-time-dummy-abc123").expect("dummy hash")
});

pub type ApiSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

/// Build the schema over a shared `AppState`. The same `Arc` is handed to the scan
/// scheduler so resolvers and the background task see one set of state. When
/// `disable_introspection` is set (production default), schema introspection is
/// turned off so the API surface isn't publicly enumerable.
pub fn build_schema(state: std::sync::Arc<AppState>, disable_introspection: bool) -> ApiSchema {
    let mut builder = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .extension(ErrorLogger)
        // Bound query cost on this public endpoint (H7): cap nesting depth and the
        // total resolved-field complexity so an unauthenticated client can't fan a
        // single aliased/nested request out to thousands of per-item DB resolvers.
        .limit_depth(20)
        .limit_complexity(1000)
        .data(state);
    if disable_introspection {
        builder = builder.disable_introspection();
    }
    builder.finish()
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

/// Resolver fields on `Series` that read the S2 enrichment tables (H2). Opt-in
/// per query — a feed that doesn't select them pays nothing. `self.id` is the
/// work id for a canonical series (`w_…`); for a numeric Suwayomi series id there
/// are no rows (those works aren't MangaDex-anchored) and both return empty.
#[async_graphql::ComplexObject]
impl Series {
    /// The all-time / 7-day / 24-hour view counts for this series (the popularity
    /// signal — see the `views` module). Resolved lazily against the normalised view
    /// key, so a series read under either identity reports one unified count. Feeds that
    /// don't select `views` never run this query.
    async fn views(&self, ctx: &Context<'_>) -> Result<SeriesViews> {
        let c = crate::views::counts_for(&state(ctx).pool, &self.id.0).await;
        Ok(SeriesViews {
            total: c.total as i32,
            last7d: c.last7d as i32,
            last24h: c.last24h as i32,
        })
    }

    /// Whether the signed-in viewer has this series in THEIR library (`user_library`).
    /// Per-viewer, resolved dynamically so every feed reflects the caller's own
    /// library; `false` for anonymous viewers (no made-up membership).
    async fn is_marked(&self, ctx: &Context<'_>) -> Result<bool> {
        let Some(user) = current_user(ctx).await else {
            return Ok(false);
        };
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_library WHERE user_id = ? AND series_id = ?)",
        )
        .bind(&user.id)
        .bind(&self.id.0)
        .fetch_one(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(exists != 0)
    }

    /// The shelf the viewer has explicitly filed this series under
    /// ('reading' | 'completed' | 'onhold' | 'plan'), or null when unset — in which
    /// case the client derives the shelf from read progress. Per-viewer; null for
    /// anonymous viewers and series not in the viewer's library.
    async fn library_status(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let Some(user) = current_user(ctx).await else {
            return Ok(None);
        };
        let row: Option<Option<String>> = sqlx::query_scalar(
            "SELECT status FROM user_library WHERE user_id = ? AND series_id = ?",
        )
        .bind(&user.id)
        .bind(&self.id.0)
        .fetch_optional(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(row.flatten())
    }

    /// Whether the viewer has favourited this series. Per-viewer; false for
    /// anonymous viewers and series not in the viewer's library.
    async fn is_favorite(&self, ctx: &Context<'_>) -> Result<bool> {
        let Some(user) = current_user(ctx).await else {
            return Ok(false);
        };
        let fav: Option<i64> = sqlx::query_scalar(
            "SELECT is_favorite FROM user_library WHERE user_id = ? AND series_id = ?",
        )
        .bind(&user.id)
        .bind(&self.id.0)
        .fetch_optional(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(fav.unwrap_or(0) != 0)
    }

    /// Every localized description of this work (all languages MangaDex carries),
    /// newest-language-agnostic, ordered by language tag. Empty for a work with no
    /// enrichment yet (run `backfillMangadexMetadata`) or a non-canonical series.
    async fn localized_descriptions(&self, ctx: &Context<'_>) -> Result<Vec<LocalizedDescription>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT lang, description FROM work_description WHERE work_id = ? ORDER BY lang",
        )
        .bind(&self.id.0)
        .fetch_all(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(rows
            .into_iter()
            .map(|(lang, description)| LocalizedDescription { lang, description })
            .collect())
    }

    /// The full author/artist credit list for this work (S2). The singular
    /// `author`/`artist` fields keep only the first of each; this returns all.
    async fn credits(&self, ctx: &Context<'_>) -> Result<Vec<Credit>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT role, name FROM work_credit WHERE work_id = ? ORDER BY role, name",
        )
        .bind(&self.id.0)
        .fetch_all(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(rows
            .into_iter()
            .map(|(role, name)| Credit { role, name })
            .collect())
    }

    /// The work's full cover set (F2), primary first then by volume. Empty for a
    /// work with no covers stored yet or a non-canonical series. `coverUrl` (the
    /// primary) keeps working independently.
    async fn covers(&self, ctx: &Context<'_>) -> Result<Vec<Cover>> {
        let pool = &state(ctx).pool;
        // Cover URLs are `covers/{mangadex_id}/{fileName}` — resolve the anchor.
        let mangadex_id: Option<String> = sqlx::query_scalar(
            "SELECT source_key FROM source_series \
             WHERE work_id = ? AND source_type = 'mangadex' LIMIT 1",
        )
        .bind(&self.id.0)
        .fetch_optional(pool)
        .await
        .map_err(gql_err)?;
        let Some(mid) = mangadex_id else {
            return Ok(Vec::new());
        };
        let covers = catalog::load_work_covers(pool, &self.id.0)
            .await
            .map_err(gql_err)?;
        Ok(covers
            .into_iter()
            .map(|c| Cover {
                url: crate::mangadex::cover_url(&mid, &c.file_name),
                thumbnail_url: crate::mangadex::cover_thumb_url(&mid, &c.file_name),
                file_name: c.file_name,
                lang: c.lang,
                volume: c.volume,
                is_primary: c.is_primary,
            })
            .collect())
    }
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
/// Memoized per request via `RequestUserCache` so the `sessions⋈users` lookup
/// runs at most once even when many resolvers ask (one per feed item).
async fn current_user(ctx: &Context<'_>) -> Option<User> {
    if let Some(cache) = ctx.data_opt::<RequestUserCache>() {
        return cache
            .0
            .get_or_init(|| async {
                match token(ctx) {
                    Some(tok) => auth::user_for_token(&state(ctx).pool, &tok)
                        .await
                        .ok()
                        .flatten(),
                    None => None,
                }
            })
            .await
            .clone();
    }
    // Fallback when no cache was attached (e.g. a code path that builds a bare request).
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

/// Wrap an internal/DB error for return to a GraphQL client. The concrete detail
/// (sqlx messages carry table/column names and SQL fragments) is logged
/// server-side only; the client receives a generic message so the API surface
/// isn't leaked. Deliberately user-facing errors are built with `Error::new(...)`
/// directly and never routed through here, so they're preserved.
fn gql_err(e: impl std::fmt::Display) -> Error {
    tracing::error!(error = %e, "internal error");
    Error::new("Internal error")
}

/// Aggregate the stored reviews for a series into a `RatingSummary`.
async fn rating_summary(pool: &SqlitePool, series_id: &str) -> RatingSummary {
    // Aggregate in SQL: GROUP BY bounds the result to <=10 rows regardless of how
    // many reviews a series has, instead of pulling every review row into memory
    // (the old `SELECT score` loaded the full review set on every feed item).
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT score, COUNT(*) FROM reviews WHERE series_id = ? GROUP BY score")
            .bind(series_id)
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, series_id, "rating_summary query failed");
                Vec::new()
            });
    if rows.is_empty() {
        return RatingSummary::empty();
    }
    let mut dist = vec![0i32; 10];
    let mut sum = 0i64;
    let mut count = 0i64;
    for (score, n) in &rows {
        sum += *score * *n;
        count += *n;
        let idx = (*score - 1).clamp(0, 9) as usize;
        dist[idx] += *n as i32;
    }
    RatingSummary {
        average: sum as f64 / count as f64,
        count: count as i32,
        distribution: dist,
    }
}

/// Komika-native per-series admin overrides (from `series_admin`).
#[derive(Clone, Default, sqlx::FromRow)]
struct AdminOverrides {
    override_interval_hours: Option<f64>,
    poll_every_minutes: Option<i64>,
    paused_override: Option<i64>,
    status_override: Option<String>,
}

/// Whether a federated Suwayomi series is NSFW per the canonical model (CATALOGUE.md
/// §2). True once it's linked to a `work` flagged NSFW; false when uncatalogued (we
/// only hide what we positively know is NSFW).
async fn canonical_is_nsfw(pool: &SqlitePool, suwayomi_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(COALESCE(w.is_nsfw_override, w.is_nsfw)), 0) FROM source_series ss \
         JOIN work w ON w.id = ss.work_id \
         WHERE ss.source_type = 'suwayomi' AND ss.source_key = ?",
    )
    .bind(suwayomi_id)
    .fetch_optional(pool)
    .await
    // Fail CLOSED on a DB error: treat the series as NSFW so a transient failure
    // can't surface NSFW content to an opted-out viewer (matches the fail-closed
    // viewer-preference half). A successful query with no row means "not linked to
    // an NSFW work" → SFW.
    .map(|row| row.unwrap_or(0) != 0)
    .unwrap_or(true)
}

/// The canonical type inputs (admin override + original language) for a federated
/// Suwayomi series, looked up through its linked `work`. Both `None` when the series
/// isn't catalogued yet — the caller then falls back to genre/title heuristics. The
/// Suwayomi source language is deliberately NOT used as the origin language (it is the
/// translation language and would misclassify every non-Japanese title as Manga).
/// The admin metadata overrides + type inputs for a federated Suwayomi series,
/// looked up through its linked `work`. All fields default when the series isn't
/// catalogued (the caller then falls back to the source values). Deterministic
/// (`ORDER BY w.id`) if a source_key ever maps to more than one work.
#[derive(Clone, Default)]
struct SuwayomiWorkOverrides {
    work_id: Option<String>,
    content_type_override: Option<String>,
    original_language: Option<String>,
    title_override: Option<String>,
    description_override: Option<String>,
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

/// Map a single federated Suwayomi manga onto a Komika `Series`. One-off callers
/// (series detail, mutation return values) route through the batch path with a
/// list of one, so the assembly logic has a single source of truth and stays
/// byte-identical between the single and list paths.
async fn map_series(st: &AppState, m: SuwayomiManga) -> Series {
    map_series_batch(st, vec![m])
        .await
        .pop()
        .expect("map_series_batch returns one Series per input")
}

/// Assemble a `Series` from a manga plus its already-resolved per-series lookups.
/// Pure (no DB): every value it needs is passed in, so the single- and batch-map
/// paths share exactly this construction. Mirrors the old inline `map_series` body.
fn assemble_series(
    st: &AppState,
    m: SuwayomiManga,
    rating: RatingSummary,
    ov: AdminOverrides,
    scan: Option<crate::scanner::ScanState>,
    alt_titles: Vec<String>,
    is_nsfw: bool,
    ov_meta: SuwayomiWorkOverrides,
    genres: Vec<String>,
) -> Series {
    let id = m.id.to_string();
    // Admin metadata overrides win over the source values (same as map_canonical_series,
    // so an edit shows on the numeric Suwayomi path too — feeds and the editor's return).
    let title = ov_meta.title_override.clone().unwrap_or_else(|| m.title.clone());
    let description = ov_meta.description_override.clone().or(m.description.clone());
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
            poll_every_minutes_override: ov.poll_every_minutes.map(|v| v as i32),
            last_scanned_at: scan
                .as_ref()
                .and_then(|s| s.last_scanned_at.clone())
                .or_else(|| to_iso(m.last_fetched_at.as_deref())),
            next_scan_at: scan.as_ref().and_then(|s| s.next_scan_at.clone()),
        },
        r#type: resolve_comic_type(
            ov_meta.content_type_override.as_deref(),
            ov_meta.original_language.as_deref(),
            &genres,
            &m.title,
        ),
        status,
        created_at: to_iso(m.in_library_at.as_deref()).unwrap_or_default(),
        updated_at: to_iso(m.last_fetched_at.as_deref()).unwrap_or_default(),
        chapter_count: m
            .chapters
            .as_ref()
            .map(|c| c.total_count as i32)
            .unwrap_or(0),
        is_nsfw,
        source_id: m.source_id,
        genres,
        author: m.author,
        artist: m.artist,
        description,
        alt_titles,
        title,
        id: ID(id),
    }
}

/// Map a whole list of federated Suwayomi mangas onto `Series` with O(1) grouped
/// queries per lookup instead of the old ~5·N serial per-series queries: a feed of
/// ~60 series previously issued ~300 round-trips; this issues ~6. Output is
/// byte-identical to mapping each item through the old `map_series` (same order,
/// same field values, same NSFW flag). `work_effective_genres` stays per-item (only
/// hit for the rare catalogued numeric series) — see the `// TODO batch` below.
async fn map_series_batch(st: &AppState, list: Vec<SuwayomiManga>) -> Vec<Series> {
    if list.is_empty() {
        return Vec::new();
    }
    // Deduped key set for the `IN (…)` lookups. For a Suwayomi series the numeric id
    // (as text) is both the `series_admin`/scan key and the `source_series.source_key`.
    let ids: Vec<String> = {
        let mut set = std::collections::BTreeSet::new();
        for m in &list {
            set.insert(m.id.to_string());
        }
        set.into_iter().collect()
    };

    // One grouped query per lookup (was one-per-series, run serially).
    let ratings = rating_summary_batch(&st.pool, &ids).await;
    let admins = admin_overrides_batch(&st.pool, &ids).await;
    let scans = scan_state_batch(&st.pool, &ids).await;
    let alts = canonical_alt_titles_batch(&st.pool, &ids).await;
    let nsfw = canonical_is_nsfw_batch(&st.pool, &ids).await;
    let ov_metas = canonical_overrides_batch(&st.pool, &ids).await;

    let mut out = Vec::with_capacity(list.len());
    for m in list {
        let id = m.id.to_string();
        let rating = ratings.get(&id).cloned().unwrap_or_else(RatingSummary::empty);
        let ov = admins.get(&id).cloned().unwrap_or_default();
        let scan = scans.get(&id).cloned();
        // Drop any alt title equal to the primary so only genuine alternatives show.
        let mut alt_titles = alts.get(&id).cloned().unwrap_or_default();
        alt_titles.retain(|t| t != &m.title);
        // `canonical_is_nsfw_batch` fails CLOSED: after a successful query a missing
        // key means "not linked to an NSFW work" → SFW (false); a DB error marks every
        // key NSFW inside the batch fn, so the `unwrap_or(false)` here can't leak it.
        let is_nsfw = nsfw.get(&id).copied().unwrap_or(false);
        let ov_meta = ov_metas.get(&id).cloned().unwrap_or_default();
        // Curated genres (work_tag) when catalogued, else the source genres. Still
        // per-item because it's only reached for a catalogued numeric series (rare on
        // this feed) and the source-genre branch needs no query. // TODO batch
        let genres = match &ov_meta.work_id {
            Some(wid) => catalog::work_effective_genres(&st.pool, wid).await,
            None => m.genre.clone(),
        };
        out.push(assemble_series(
            st, m, rating, ov, scan, alt_titles, is_nsfw, ov_meta, genres,
        ));
    }
    out
}

/// Build the `?,?,…` placeholder list for an `IN (…)` clause of `n` values. Values
/// are always bound (never interpolated), so this only ever emits placeholders.
fn in_placeholders(n: usize) -> String {
    std::iter::repeat("?").take(n).collect::<Vec<_>>().join(",")
}

/// Batched `rating_summary`: one grouped query for all series → per-series summary.
/// Missing ids (no reviews) are simply absent; the caller defaults them to empty.
async fn rating_summary_batch(
    pool: &SqlitePool,
    ids: &[String],
) -> HashMap<String, RatingSummary> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let sql = format!(
        "SELECT series_id, score, COUNT(*) FROM reviews WHERE series_id IN ({}) \
         GROUP BY series_id, score",
        in_placeholders(ids.len())
    );
    let mut q = sqlx::query_as::<_, (String, i64, i64)>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(pool).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "rating_summary_batch query failed");
        Vec::new()
    });
    // (distribution[10], sum, count) per series — same fold as `rating_summary`.
    let mut acc: HashMap<String, (Vec<i32>, i64, i64)> = HashMap::new();
    for (sid, score, n) in rows {
        let e = acc.entry(sid).or_insert_with(|| (vec![0i32; 10], 0i64, 0i64));
        e.1 += score * n;
        e.2 += n;
        let idx = (score - 1).clamp(0, 9) as usize;
        e.0[idx] += n as i32;
    }
    acc.into_iter()
        .map(|(sid, (dist, sum, count))| {
            (
                sid,
                RatingSummary {
                    average: sum as f64 / count as f64,
                    count: count as i32,
                    distribution: dist,
                },
            )
        })
        .collect()
}

/// Batched `admin_overrides`: one query for all series → per-series overrides.
async fn admin_overrides_batch(
    pool: &SqlitePool,
    ids: &[String],
) -> HashMap<String, AdminOverrides> {
    if ids.is_empty() {
        return HashMap::new();
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        series_id: String,
        #[sqlx(flatten)]
        ov: AdminOverrides,
    }
    let sql = format!(
        "SELECT series_id, override_interval_hours, poll_every_minutes, paused_override, \
         status_override FROM series_admin WHERE series_id IN ({})",
        in_placeholders(ids.len())
    );
    let mut q = sqlx::query_as::<_, Row>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    q.fetch_all(pool)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "admin_overrides_batch query failed"))
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.series_id, r.ov))
        .collect()
}

/// Batched `scan_state`: one query for all series → per-series scan state.
async fn scan_state_batch(
    pool: &SqlitePool,
    ids: &[String],
) -> HashMap<String, crate::scanner::ScanState> {
    if ids.is_empty() {
        return HashMap::new();
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        series_id: String,
        #[sqlx(flatten)]
        state: crate::scanner::ScanState,
    }
    // Same columns as `scanner::SCAN_STATE_SELECT`, plus the series_id key and IN(…).
    let sql = format!(
        "SELECT series_id, avg_interval_hours, known_chapter_count, known_max_chapter, \
         last_scanned_at, next_scan_at, last_new_chapter_at, awaiting_since, known_chapter_ids \
         FROM series_scan_state WHERE series_id IN ({})",
        in_placeholders(ids.len())
    );
    let mut q = sqlx::query_as::<_, Row>(&sql);
    for id in ids {
        q = q.bind(id);
    }
    q.fetch_all(pool)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "scan_state_batch query failed"))
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.series_id, r.state))
        .collect()
}

/// Batched `canonical_alt_titles`: one query for all source keys → per-key alt
/// titles, each ordered by `raw_title` (identical to the single-key path).
async fn canonical_alt_titles_batch(
    pool: &SqlitePool,
    keys: &[String],
) -> HashMap<String, Vec<String>> {
    if keys.is_empty() {
        return HashMap::new();
    }
    let sql = format!(
        "SELECT DISTINCT ss.source_key, wa.raw_title \
         FROM source_series ss \
         JOIN work_alias wa ON wa.work_id = ss.work_id \
         WHERE ss.source_type = 'suwayomi' AND ss.source_key IN ({}) \
         ORDER BY ss.source_key, wa.raw_title",
        in_placeholders(keys.len())
    );
    let mut q = sqlx::query_as::<_, (String, String)>(&sql);
    for k in keys {
        q = q.bind(k);
    }
    let rows = q.fetch_all(pool).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "canonical_alt_titles_batch query failed");
        Vec::new()
    });
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (key, raw_title) in rows {
        map.entry(key).or_default().push(raw_title);
    }
    map
}

/// Batched `canonical_is_nsfw`: one grouped query for all source keys. Fails CLOSED
/// exactly like the single-key path — on a DB error every key is marked NSFW (true)
/// so a transient failure can't surface NSFW content to an opted-out viewer. On
/// success, keys absent from the result are unlinked/SFW (the caller defaults them
/// to false).
async fn canonical_is_nsfw_batch(pool: &SqlitePool, keys: &[String]) -> HashMap<String, bool> {
    if keys.is_empty() {
        return HashMap::new();
    }
    let sql = format!(
        "SELECT ss.source_key, COALESCE(MAX(COALESCE(w.is_nsfw_override, w.is_nsfw)), 0) \
         FROM source_series ss \
         JOIN work w ON w.id = ss.work_id \
         WHERE ss.source_type = 'suwayomi' AND ss.source_key IN ({}) \
         GROUP BY ss.source_key",
        in_placeholders(keys.len())
    );
    let mut q = sqlx::query_as::<_, (String, i64)>(&sql);
    for k in keys {
        q = q.bind(k);
    }
    match q.fetch_all(pool).await {
        Ok(rows) => rows.into_iter().map(|(k, v)| (k, v != 0)).collect(),
        Err(e) => {
            // Fail closed: mark every key NSFW so nothing leaks on a transient error.
            tracing::warn!(error = %e, "canonical_is_nsfw_batch query failed; failing closed");
            keys.iter().map(|k| (k.clone(), true)).collect()
        }
    }
}

/// Batched `canonical_overrides`: one query for all source keys, then keep the first
/// row per key by `w.id` — byte-identical to the single-key `ORDER BY w.id LIMIT 1`.
async fn canonical_overrides_batch(
    pool: &SqlitePool,
    keys: &[String],
) -> HashMap<String, SuwayomiWorkOverrides> {
    if keys.is_empty() {
        return HashMap::new();
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        source_key: String,
        work_id: String,
        content_type_override: Option<String>,
        original_language: Option<String>,
        title_override: Option<String>,
        description_override: Option<String>,
    }
    let sql = format!(
        "SELECT ss.source_key, w.id AS work_id, w.content_type_override, w.original_language, \
                w.title_override, w.description_override \
         FROM source_series ss \
         JOIN work w ON w.id = ss.work_id \
         WHERE ss.source_type = 'suwayomi' AND ss.source_key IN ({}) \
         ORDER BY ss.source_key, w.id",
        in_placeholders(keys.len())
    );
    let mut q = sqlx::query_as::<_, Row>(&sql);
    for k in keys {
        q = q.bind(k);
    }
    let rows = q.fetch_all(pool).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "canonical_overrides_batch query failed");
        Vec::new()
    });
    let mut map: HashMap<String, SuwayomiWorkOverrides> = HashMap::new();
    for r in rows {
        let Row {
            source_key,
            work_id,
            content_type_override,
            original_language,
            title_override,
            description_override,
        } = r;
        // Rows are ordered by (source_key, w.id); keep only the first per key.
        map.entry(source_key).or_insert_with(|| SuwayomiWorkOverrides {
            work_id: Some(work_id),
            content_type_override,
            original_language,
            title_override,
            description_override,
        });
    }
    map
}

/// Apply the S4 search filters to a mapped result set: keep series matching ANY of
/// `genres` (case-insensitive; empty/None = no genre filter) AND whose aggregate
/// user rating falls in `[min_rating, max_rating]`. A `min_rating > 0` excludes
/// unrated series (rating average 0 with 0 reviews). Pure so it's unit-testable.
fn apply_search_filters(
    items: Vec<Series>,
    genres: Option<&[String]>,
    min_rating: Option<f64>,
    max_rating: Option<f64>,
) -> Vec<Series> {
    let want: Vec<String> = genres
        .map(|g| {
            g.iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    items
        .into_iter()
        .filter(|s| {
            if !want.is_empty() {
                let have: Vec<String> = s.genres.iter().map(|g| g.to_lowercase()).collect();
                if !want.iter().any(|w| have.iter().any(|h| h == w)) {
                    return false;
                }
            }
            let avg = s.rating.average;
            if let Some(min) = min_rating {
                if avg < min {
                    return false;
                }
            }
            if let Some(max) = max_rating {
                if avg > max {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Group raw cross-source chapter rows (S2) into one `AggregatedChapter` per
/// chapter number, collecting each source that provides it, ascending by number.
/// Pure so the dedupe/ordering is unit-testable without a DB. A number is bucketed
/// by `round(number*100)` so "10.5" ≠ "10" but float noise can't split a number.
fn group_aggregated_chapters(rows: Vec<catalog::WorkChapterRow>) -> Vec<AggregatedChapter> {
    use std::collections::BTreeMap;
    let mut by_num: BTreeMap<i64, AggregatedChapter> = BTreeMap::new();
    for r in rows {
        let key = (r.number * 100.0).round() as i64;
        let entry = by_num.entry(key).or_insert_with(|| AggregatedChapter {
            number: r.number,
            title: r.title.clone(),
            sources: Vec::new(),
        });
        // Keep the first non-empty title we see for the number.
        if entry.title.as_deref().unwrap_or("").is_empty() {
            entry.title = r.title.clone();
        }
        entry.sources.push(ChapterSource {
            source_type: r.source_type,
            source_id: r.source_id,
            suwayomi_manga_id: r.suwayomi_manga_id.map(ID),
            chapter_id: ID(r.chapter_id),
            scanlator: r.scanlator,
        });
    }
    by_num.into_values().collect()
}

/// S1: resolve one Suwayomi series DB-first — return the cached row when it's still
/// fresh; on a stale hit or a miss, revalidate upstream under a single-flight lock
/// (so N concurrent readers of the same id trigger ONE fetch), and fall back to the
/// stale cached row if the refetch fails (the reader never hard-fails on upstream
/// trouble).
/// The aggregate bucket key for a chapter number (matches `group_aggregated_chapters`
/// and the `chapter_override.chapter_key` column).
fn chapter_key(number: f64) -> String {
    ((number * 100.0).round() as i64).to_string()
}

/// Admin chapter overrides for a work, keyed by aggregate bucket key → (hidden,
/// title_override). Empty (and best-effort) when the work has none.
async fn chapter_overrides(
    pool: &SqlitePool,
    work_id: &str,
) -> std::collections::HashMap<String, (bool, Option<String>)> {
    #[derive(sqlx::FromRow)]
    struct Row {
        chapter_key: String,
        hidden: i64,
        title_override: Option<String>,
    }
    sqlx::query_as::<_, Row>(
        "SELECT chapter_key, hidden, title_override FROM chapter_override WHERE work_id = ?",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| (r.chapter_key, (r.hidden != 0, r.title_override)))
    .collect()
}

/// Resolve a catalog series id (numeric Suwayomi id or `w_` canonical id) to the
/// canonical work id its admin overrides live on. `None` when the series isn't
/// catalogued (numeric id with no `source_series` row, or an unknown `w_` id).
async fn resolve_work_id(pool: &SqlitePool, series_id: &str) -> Option<String> {
    if series_id.starts_with("w_") {
        return sqlx::query_scalar::<_, String>("SELECT id FROM work WHERE id = ?")
            .bind(series_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    }
    sqlx::query_scalar::<_, String>(
        "SELECT work_id FROM source_series \
         WHERE source_type = 'suwayomi' AND source_key = ? ORDER BY work_id LIMIT 1",
    )
    .bind(series_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

async fn resolve_series_cached(st: &AppState, id: i64) -> anyhow::Result<SuwayomiManga> {
    // Fast path: a fresh cache hit needs no lock and no upstream call.
    let stale = match crate::series_cache::get_series_fresh(&st.pool, id).await? {
        Some((m, true)) => return Ok(m),
        Some((m, false)) => Some(m), // stale — refresh below, keep as fallback
        None => None,                // miss
    };

    // Single-flight: collapse concurrent misses/refreshes for this id.
    let lock = st.series_inflight.lock_handle(id);
    let _guard = lock.lock().await;

    // Re-check after acquiring: a task we queued behind may have just refreshed it.
    if let Some((m, true)) = crate::series_cache::get_series_fresh(&st.pool, id).await? {
        return Ok(m);
    }

    match st.suwayomi.series(id).await {
        Ok(m) => {
            let _ = crate::series_cache::put_series(&st.pool, &m).await;
            Ok(m)
        }
        // Refetch failed: serve the stale cached row rather than erroring the reader.
        Err(e) => match stale {
            Some(m) => Ok(m),
            None => Err(e),
        },
    }
}

/// S1: resolve one series' chapter list DB-first — cached rows when present AND
/// fresh, else revalidate upstream under a single-flight lock and cache. An empty
/// cache is a miss (a series with genuinely zero chapters re-checks the source until
/// scanned). A refetch failure falls back to the stale cached rows. The returned
/// rows carry GLOBAL chapter state; the per-viewer `suwayomi_progress` overlay is
/// applied downstream by the caller and is unaffected by this freshness gate.
async fn resolve_chapters_cached(
    st: &AppState,
    manga_id: i64,
) -> anyhow::Result<Vec<SuwayomiChapter>> {
    let cached = crate::series_cache::get_chapters(&st.pool, manga_id).await?;
    if !cached.is_empty() {
        let last = crate::series_cache::chapters_last_fetched(&st.pool, manga_id).await?;
        if crate::series_cache::is_fresh(last.as_deref(), crate::series_cache::CHAPTERS_TTL_SECS) {
            return Ok(cached);
        }
    }

    // Single-flight: collapse concurrent misses/refreshes for this manga id.
    let lock = st.chapters_inflight.lock_handle(manga_id);
    let _guard = lock.lock().await;

    // Re-check after acquiring: a queued-behind task may have just refreshed.
    let cached = crate::series_cache::get_chapters(&st.pool, manga_id).await?;
    if !cached.is_empty() {
        let last = crate::series_cache::chapters_last_fetched(&st.pool, manga_id).await?;
        if crate::series_cache::is_fresh(last.as_deref(), crate::series_cache::CHAPTERS_TTL_SECS) {
            return Ok(cached);
        }
    }

    match st.suwayomi.chapters(manga_id).await {
        Ok(list) => {
            let _ = crate::series_cache::put_chapters(&st.pool, manga_id, &list).await;
            Ok(list)
        }
        // Refetch failed: serve stale cached rows if we have any, else propagate.
        Err(e) => {
            if !cached.is_empty() {
                Ok(cached)
            } else {
                Err(e)
            }
        }
    }
}

async fn map_series_list(st: &AppState, list: Vec<SuwayomiManga>) -> Vec<Series> {
    // Batched: O(1) grouped queries per lookup instead of ~5 per series (see
    // `map_series_batch`). Output order/values are identical to the old per-item map.
    map_series_batch(st, list).await
}

/// Load a batch of series by their (normalised) view keys, preserving order and
/// dropping any that no longer resolve. Numeric keys resolve to Suwayomi series (DB
/// cache first, live fetch on a miss); `w_` keys to canonical works. Turns a Trending
/// ranking (`views::trending_keys`) into displayable series. Bounded (≤ the row size),
/// so the per-key loads never fan out across the whole catalogue.
async fn series_by_keys(st: &AppState, keys: &[String]) -> Vec<Series> {
    let mut out: Vec<Option<Series>> = Vec::with_capacity(keys.len());
    let mut pending: Vec<(usize, SuwayomiManga)> = Vec::new();
    for key in keys {
        if key.starts_with("w_") {
            if let Ok(Some(work)) = catalog::load_canonical_work(&st.pool, key).await {
                let chapters = catalog::load_canonical_chapters(&st.pool, key)
                    .await
                    .unwrap_or_default();
                out.push(Some(
                    map_canonical_series(
                        &st.pool,
                        None,
                        work,
                        catalog::main_chapter_count_str(&chapters) as i32,
                    )
                    .await,
                ));
            } else {
                out.push(None);
            }
        } else if let Ok(n) = key.parse::<i64>() {
            let m = match crate::series_cache::get_series(&st.pool, n).await.ok().flatten() {
                Some(m) => Some(m),
                None => st.suwayomi.series(n).await.ok(),
            };
            if let Some(m) = m {
                pending.push((out.len(), m));
                out.push(None); // slot filled by the batched map below
            } else {
                out.push(None);
            }
        } else {
            out.push(None);
        }
    }
    let (indices, mangas): (Vec<usize>, Vec<SuwayomiManga>) = pending.into_iter().unzip();
    for (idx, series) in indices.into_iter().zip(map_series_batch(st, mangas).await) {
        out[idx] = Some(series);
    }
    out.into_iter().flatten().collect()
}

/// Map a canonical `work` (MangaDex-mirrored) onto the shared `Series` shape so the
/// reader reuses its existing series/reader components (CATALOGUE.md §6). The series
/// `id` is the work id (its `w_` prefix distinguishes it from a numeric Suwayomi id,
/// so the reader routes it down the canonical path). Cover URLs point at
/// `uploads.mangadex.org` — the client resolves them through the Worker proxy.
/// Fields Komika doesn't mirror for MangaDex works (genres, ratings, library/scan
/// state) are empty/defaulted; reading is fully functional without them.
async fn map_canonical_series(
    pool: &SqlitePool,
    // `isMarked` is now resolved per-viewer in the ComplexObject impl, so the
    // caller's user id is no longer needed here; kept in the signature so the
    // several call sites don't need to change.
    _user_id: Option<&str>,
    work: catalog::CanonicalWork,
    chapter_count: i32,
) -> Series {
    let cover_url = match (&work.mangadex_id, &work.cover_file_name) {
        (Some(mid), Some(fname)) => crate::mangadex::cover_thumb_url(mid, fname),
        _ => String::new(),
    };
    let title = work
        .title_override
        .clone()
        .or_else(|| work.primary_title.clone())
        .unwrap_or_default();
    let mut alt_titles = work.alt_titles;
    alt_titles.retain(|t| t != &title);
    let status = work
        .status
        .as_deref()
        .and_then(komika_status)
        .unwrap_or(SeriesStatus::Unknown);
    // Rating reuses the string-keyed `reviews` aggregate (a `w_` id round-trips with
    // no schema change). Library membership (`isMarked`) is resolved per-viewer in
    // the `#[ComplexObject]` impl against `user_library`, not computed here.
    let rating = rating_summary(pool, &work.work_id).await;
    // chapterCount is the English reader-list count (deduped to one row per number
    // by `load_canonical_chapters`) — Komika serves only English chapters, so that is
    // the number the details page must show. The cross-source aggregate
    // (`aggregate_chapter_count`) is only a FALLBACK for works whose MangaDex spine has
    // 0 English chapters but whose Suwayomi source carries some: without the English
    // spine there is nothing else to count. When the English mirror is non-empty we
    // never fall back, because the Suwayomi branch of the aggregate is not
    // language-filtered (`suwayomi_chapter` has no `lang` column) and would inflate the
    // count with non-English / half / re-numbered chapters (e.g. Tsukimichi: 120 → 151).
    let chapter_count = if chapter_count > 0 {
        chapter_count
    } else {
        catalog::aggregate_chapter_count(pool, &work.work_id)
            .await
            .unwrap_or(0)
            .max(0) as i32
    };
    // "Last updated" = publish time of the newest English chapter, not the work's
    // metadata timestamp (which a routine re-sync bumps to now). Fall back to the
    // metadata timestamp only when no English chapter is mirrored yet. Computed here
    // (before the struct literal moves `work.work_id`).
    let updated_at = catalog::latest_english_chapter_at(pool, &work.work_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| work.updated_at.clone());
    let genres = catalog::work_effective_genres(pool, &work.work_id).await;
    let comic_type = resolve_comic_type(
        work.content_type_override.as_deref(),
        work.original_language.as_deref(),
        &genres,
        &title,
    );
    Series {
        id: ID(work.work_id),
        title,
        alt_titles,
        author: work.author,
        artist: work.artist,
        description: work.description_override.clone().or(work.description),
        genres,
        r#type: comic_type,
        status,
        cover_url,
        source_id: "mangadex".to_string(),
        chapter_count,
        is_nsfw: work.is_nsfw_override.unwrap_or(work.is_nsfw),
        rating,
        scan: ScanPolicy {
            avg_interval_hours: 0.0,
            override_interval_hours: None,
            poll_every_minutes: 30,
            paused: false,
            status_override: None,
            paused_override: None,
            poll_every_minutes_override: None,
            last_scanned_at: None,
            next_scan_at: None,
        },
        created_at: work.created_at,
        updated_at,
    }
}

/// Map a mirrored MangaDex chapter onto the shared `Chapter` shape. The chapter `id`
/// is the MangaDex chapter uuid (the key `canonicalPages` fetches pages with);
/// `series_id` is the work id so navigation stays on the canonical path. Per-user
/// reading state (`progress`, keyed by the chapter uuid) drives resume-at-last-chapter;
/// anonymous / never-read chapters default to unread (CR6).
fn map_canonical_chapter(
    work_id: &str,
    c: catalog::CanonicalChapter,
    progress: Option<(i32, bool)>,
) -> Chapter {
    let number = c
        .number
        .as_deref()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(0.0);
    let (last_page_read, read) = progress.unwrap_or((0, false));
    Chapter {
        id: ID(c.external_id),
        series_id: ID(work_id.to_string()),
        number,
        title: c.title,
        page_count: 0, // unknown until the at-home page list is fetched
        uploaded_at: c.published_at,
        scanlator: None,
        read,
        last_page_read,
        bookmarked: false,
        is_downloaded: false,
    }
}

/// Per-user read state for every chapter of one canonical work, keyed by the MangaDex
/// chapter uuid. One query per series (not per chapter), matching the cost `map_series`
/// already pays. Anonymous viewers get an empty map (all chapters read as unread).
async fn canonical_progress_map(
    pool: &SqlitePool,
    user_id: Option<&str>,
    work_id: &str,
) -> HashMap<String, (i32, bool)> {
    let Some(uid) = user_id else {
        return HashMap::new();
    };
    let rows = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT chapter_id, last_page_read, read FROM canonical_progress \
         WHERE user_id = ? AND work_id = ?",
    )
    .bind(uid)
    .bind(work_id)
    .fetch_all(pool)
    .await
    // Log (don't swallow) DB errors; still fall back to an empty map gracefully.
    .inspect_err(|e| tracing::warn!(error = %e, work_id, "canonical_progress_map query failed"))
    .unwrap_or_default();
    rows.into_iter()
        .map(|(cid, lpr, rd)| (cid, (lpr as i32, rd != 0)))
        .collect()
}

/// Per-user read state for every chapter of one numeric Suwayomi series, keyed by the
/// numeric chapter id. One query per series (mirrors `canonical_progress_map`).
/// Anonymous viewers get an empty map (all chapters read as unread).
async fn suwayomi_progress_map(
    pool: &SqlitePool,
    user_id: Option<&str>,
    series_id: i64,
) -> HashMap<i64, (i32, bool)> {
    let Some(uid) = user_id else {
        return HashMap::new();
    };
    let rows = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT chapter_id, last_page_read, read FROM suwayomi_progress \
         WHERE user_id = ? AND series_id = ?",
    )
    .bind(uid)
    .bind(series_id.to_string())
    .fetch_all(pool)
    .await
    // Log (don't swallow) DB errors; still fall back to an empty map gracefully.
    .inspect_err(|e| tracing::warn!(error = %e, series_id, "suwayomi_progress_map query failed"))
    .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(cid, lpr, rd)| {
            cid.parse::<i64>()
                .ok()
                .map(|id| (id, (lpr as i32, rd != 0)))
        })
        .collect()
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
    parent_id: Option<String>,
    body: String,
    has_spoiler: i64,
    created_at: String,
    author_id: String,
    author_username: String,
    author_avatar: Option<String>,
    media_id: Option<String>,
    media_width: Option<i64>,
    media_height: Option<i64>,
    likes: i64,
    dislikes: i64,
    my_vote: i64,
}

impl From<CommentJoin> for Comment {
    fn from(c: CommentJoin) -> Self {
        Comment {
            id: ID(c.id),
            target_type: c.target_type,
            target_id: ID(c.target_id),
            parent_id: c.parent_id.map(ID),
            author: UserRef {
                id: ID(c.author_id),
                username: c.author_username,
                avatar_url: c.author_avatar,
            },
            body: c.body,
            has_spoiler: c.has_spoiler != 0,
            media_url: c.media_id.as_deref().map(crate::media::comment_media_url),
            media_width: c.media_width.map(|n| n as i32),
            media_height: c.media_height.map(|n| n as i32),
            created_at: c.created_at,
            likes: c.likes as i32,
            dislikes: c.dislikes as i32,
            my_vote: c.my_vote as i32,
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

/// Load the editable profile fields not carried on the auth `User` row loader
/// (`display_name`, `bio`, `created_at`). Falls back to empties on any lookup
/// failure so building a session never fails on a cold profile.
async fn user_profile_fields(
    pool: &SqlitePool,
    user_id: &str,
) -> (Option<String>, Option<String>, String) {
    sqlx::query_as::<_, (Option<String>, Option<String>, String)>(
        "SELECT display_name, bio, created_at FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    // Log (don't swallow) DB errors; still fall back to empties gracefully.
    .inspect_err(|e| tracing::warn!(error = %e, user_id, "user_profile_fields query failed"))
    .ok()
    .flatten()
    .unwrap_or((None, None, String::new()))
}

/// Build the client-facing `SessionUser` for a resolved auth `User`, loading its
/// profile fields. Every session-returning path funnels through this so the
/// shape stays consistent.
async fn build_session_user(pool: &SqlitePool, u: &User, show_nsfw: bool) -> SessionUser {
    let (display_name, bio, joined_at) = user_profile_fields(pool, &u.id).await;
    SessionUser {
        id: ID(u.id.clone()),
        username: u.username.clone(),
        display_name,
        bio,
        avatar_url: u.avatar_url.clone(),
        is_admin: u.is_admin != 0,
        show_nsfw,
        joined_at,
    }
}

/// Record one entry in a user's activity feed. Best-effort: a failed insert is
/// logged and swallowed so it can never fail the user's actual action (posting
/// a review/comment, adding to the library).
async fn log_activity(
    pool: &SqlitePool,
    user_id: &str,
    kind: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
) {
    let res = sqlx::query(
        "INSERT INTO user_activity (id, user_id, kind, target_type, target_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(kind)
    .bind(target_type)
    .bind(target_id)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, kind, "failed to record user activity");
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
        // S1: serve from the DB cache once the catalogue has been persisted (the
        // admin "save everything" / ingest / scan populate it), so the home page
        // reads from SQLite instead of a live source browse. Fall back to a live
        // browse only while the cache is still empty (fresh install), caching what
        // it fetches so the next load is fast.
        // `recent` = titles most recently ADDED to our catalogue (ordered by
        // first-persist time), distinct from `latest` (upstream recently-updated).
        let (popular, latest, recent) = if crate::series_cache::count(&st.pool)
            .await
            .map_err(gql_err)?
            > 0
        {
            let lib = crate::series_cache::library(&st.pool, PAGE_SIZE)
                .await
                .map_err(gql_err)?;
            let recent = crate::series_cache::recently_added(&st.pool, PAGE_SIZE)
                .await
                .map_err(gql_err)?;
            (lib, Vec::new(), recent)
        } else {
            let popular = st
                .suwayomi
                .fetch_source(FetchType::Popular, 1, None)
                .await
                .map_err(gql_err)?
                .1;
            for m in &popular {
                let _ = crate::series_cache::put_series(&st.pool, m).await;
            }
            let latest = st
                .suwayomi
                .fetch_source(FetchType::Latest, 1, None)
                .await
                .map(|r| r.1)
                .unwrap_or_default();
            // Pre-cache (fresh install) there's no catalogue-insertion history yet;
            // the source "Latest" is the best available proxy for "newly added".
            let recent = latest.clone();
            (popular, latest, recent)
        };

        // Hide NSFW-flagged works unless the viewer opted in (CATALOGUE.md §2).
        let show_nsfw = viewer_show_nsfw(ctx).await;
        let popular = filter_nsfw(show_nsfw, map_series_list(st, popular).await);
        let latest = filter_nsfw(show_nsfw, map_series_list(st, latest).await);
        let recent = filter_nsfw(show_nsfw, map_series_list(st, recent).await);

        // Trending = the 10 most-viewed series over the LAST 24 HOURS (the real
        // popularity signal from `recordView`, replacing the old "first 6 of Popular").
        // During cold start — before reads accumulate — this is empty, so pad with
        // Popular (dedup by id) to keep the row populated; it becomes fully
        // view-ranked as views come in.
        let trending = {
            let keys: Vec<String> = crate::views::trending_keys(&st.pool, 10)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            let mut items = filter_nsfw(show_nsfw, series_by_keys(st, &keys).await);
            let mut seen: std::collections::HashSet<String> =
                items.iter().map(|s| s.id.0.clone()).collect();
            for s in &popular {
                if items.len() >= 10 {
                    break;
                }
                if seen.insert(s.id.0.clone()) {
                    items.push(s.clone());
                }
            }
            items.truncate(10);
            items
        };

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
                items: trending,
            },
        ];
        if !latest.is_empty() {
            feeds.push(DiscoveryFeed {
                kind: DiscoveryFeedKind::RecentlyUpdated,
                title: "Latest Updates".into(),
                genre: None,
                items: latest.clone(),
            });
        }
        if !recent.is_empty() {
            feeds.push(DiscoveryFeed {
                kind: DiscoveryFeedKind::RecentlyAdded,
                title: "Latest Added".into(),
                genre: None,
                items: recent,
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
        let show_nsfw = viewer_show_nsfw(ctx).await;
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        // NSFW is filtered in SQL (like canonical_updates) rather than after the page
        // slice, so `total`/`has_next` count only the rows the viewer can see — no skew
        // where a page under-fills yet reports another page (N3). A series is NSFW when
        // its Suwayomi source_series links to a work flagged NSFW.
        const NSFW_FILTER: &str = "(? = 1 OR NOT EXISTS ( \
             SELECT 1 FROM source_series ss JOIN work w ON w.id = ss.work_id \
             WHERE ss.source_type = 'suwayomi' AND ss.source_key = sss.series_id \
               AND w.is_nsfw = 1))";
        // Series ids with a detected new-chapter timestamp, newest-first, carrying that
        // timestamp so the feed can report a FAITHFUL "updated" time (when the series
        // actually got a new chapter — not its last poll, which `updatedAt` otherwise
        // reflects). Fetch one extra to compute has_next without a second round-trip.
        let rows: Vec<(String, String)> = sqlx::query_as(&format!(
            "SELECT series_id, last_new_chapter_at FROM series_scan_state sss \
             WHERE last_new_chapter_at IS NOT NULL AND {NSFW_FILTER} \
             ORDER BY last_new_chapter_at DESC, series_id ASC LIMIT ? OFFSET ?"
        ))
        .bind(show_nsfw as i64)
        .bind(PAGE_SIZE + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let new_chapter_at: std::collections::HashMap<String, String> =
            rows.iter().cloned().collect();
        let ids: Vec<String> = rows.into_iter().map(|(id, _)| id).collect();
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM series_scan_state sss \
             WHERE last_new_chapter_at IS NOT NULL AND {NSFW_FILTER}"
        ))
        .bind(show_nsfw as i64)
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        let has_next = ids.len() as i64 > PAGE_SIZE;

        // Hydrate each id DB-first (S1) — cached row when present, live-fetch on a
        // miss. A series that has since been removed from the source is skipped
        // rather than failing the whole feed. NSFW is already filtered in SQL above.
        // Collect the resolved mangas first, then map the whole page in ONE batched
        // pass (O(1) grouped queries per lookup) instead of ~5 queries per series.
        let mut resolved = Vec::new();
        for id in ids.into_iter().take(PAGE_SIZE as usize) {
            let Ok(n) = id.parse::<i64>() else { continue };
            match resolve_series_cached(st, n).await {
                Ok(m) => resolved.push(m),
                Err(e) => {
                    tracing::warn!(series_id = id, error = %e, "updates: skipping unresolvable series")
                }
            }
        }
        let mut items = map_series_batch(st, resolved).await;
        // Override `updatedAt` with the new-chapter detection time (RFC3339) so the
        // reader's "Latest Updates" row shows when each series last gained a chapter.
        for it in &mut items {
            if let Some(ts) = new_chapter_at.get(&it.id.0) {
                it.updated_at = ts.clone();
            }
        }
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
             WHERE ss.source_type = 'mangadex' AND c.lang = 'en' AND (? = 1 OR w.is_nsfw = 0) \
             GROUP BY ss.work_id \
             ORDER BY latest_at DESC, ss.work_id DESC \
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
        // The canonical path is MangaDex-anchored by contract: a backfilled
        // `w_<numeric>` work has no mangadex source (mangadex_id = None) → empty cover
        // and zero chapters. Reject it as not-found rather than serving a shell (CR3).
        if work.mangadex_id.is_none() {
            return Err(Error::new("No such work"));
        }
        if work.is_nsfw_override.unwrap_or(work.is_nsfw) && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such work"));
        }
        let chapters = catalog::load_canonical_chapters(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?;
        let user = current_user(ctx).await;
        Ok(map_canonical_series(
            &st.pool,
            user.as_ref().map(|u| u.id.as_str()),
            work,
            catalog::main_chapter_count_str(&chapters) as i32,
        )
        .await)
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
        // MangaDex-anchored by contract; reject a non-anchored backfilled work (CR3).
        if work.mangadex_id.is_none() {
            return Err(Error::new("No such work"));
        }
        if work.is_nsfw_override.unwrap_or(work.is_nsfw) && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such work"));
        }
        let chapters = catalog::load_canonical_chapters(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?;
        let user = current_user(ctx).await;
        let progress =
            canonical_progress_map(&st.pool, user.as_ref().map(|u| u.id.as_str()), &work_id.0)
                .await;
        Ok(chapters
            .into_iter()
            .map(|c| {
                let p = progress.get(&c.external_id).copied();
                map_canonical_chapter(&work_id.0, c, p)
            })
            .collect())
    }

    /// S2: the aggregated chapter list of a canonical work — every chapter number
    /// available across ALL its sources (installed Suwayomi sources + the MangaDex
    /// mirror), deduped by number, each carrying the per-source availability so the
    /// reader can pick a translator. This is how Solo Leveling shows its asurascans
    /// chapters even though its MangaDex spine has none. Ascending by number.
    async fn aggregated_chapters(
        &self,
        ctx: &Context<'_>,
        work_id: ID,
    ) -> Result<Vec<AggregatedChapter>> {
        let st = state(ctx);
        let work = catalog::load_canonical_work(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?
            .ok_or_else(|| Error::new("No such work"))?;
        if work.is_nsfw_override.unwrap_or(work.is_nsfw) && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such work"));
        }
        let rows = catalog::work_source_chapters(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?;
        // (the NSFW gate above uses the effective flag incl. admin override)
        let mut chapters = group_aggregated_chapters(rows);
        // Apply admin chapter overrides: drop soft-hidden chapters and apply renames
        // (non-destructive — the underlying cached rows are untouched).
        let ov = chapter_overrides(&st.pool, &work_id.0).await;
        if !ov.is_empty() {
            chapters.retain(|c| !matches!(ov.get(&chapter_key(c.number)), Some((true, _))));
            for c in &mut chapters {
                if let Some((_, Some(t))) = ov.get(&chapter_key(c.number)) {
                    c.title = Some(t.clone());
                }
            }
        }
        Ok(chapters)
    }

    /// Admin: the raw metadata-override state of a series' canonical work, so the
    /// series-detail editor can show what is pinned vs derived. `workId` is null when
    /// the series isn't catalogued yet (nothing can be pinned without a work).
    async fn series_admin_meta(&self, ctx: &Context<'_>, series_id: ID) -> Result<SeriesAdminMeta> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let Some(work_id) = resolve_work_id(&st.pool, &series_id.0).await else {
            return Ok(SeriesAdminMeta {
                work_id: None,
                title_override: None,
                description_override: None,
                content_type_override: None,
                is_nsfw_override: None,
                tags: Vec::new(),
                has_curated_tags: false,
            });
        };
        #[derive(sqlx::FromRow)]
        struct Row {
            title_override: Option<String>,
            description_override: Option<String>,
            content_type_override: Option<String>,
            is_nsfw_override: Option<i64>,
        }
        let row = sqlx::query_as::<_, Row>(
            "SELECT title_override, description_override, content_type_override, is_nsfw_override \
             FROM work WHERE id = ?",
        )
        .bind(&work_id)
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        let curated: Vec<String> =
            sqlx::query_scalar("SELECT tag FROM work_tag WHERE work_id = ? ORDER BY ord, tag")
                .bind(&work_id)
                .fetch_all(&st.pool)
                .await
                .unwrap_or_default();
        let has_curated_tags = !curated.is_empty();
        let tags = if has_curated_tags {
            curated
        } else {
            catalog::work_effective_genres(&st.pool, &work_id).await
        };
        Ok(SeriesAdminMeta {
            work_id: Some(ID(work_id)),
            title_override: row.title_override,
            description_override: row.description_override,
            content_type_override: row.content_type_override.as_deref().and_then(comic_type_from_word),
            is_nsfw_override: row.is_nsfw_override.map(|v| v != 0),
            tags,
            has_curated_tags,
        })
    }

    /// Admin: a work's aggregated chapters WITH their override state (hidden/renamed),
    /// UNFILTERED — the series-detail editor needs to see and un-hide soft-hidden
    /// chapters, unlike the reader's `aggregatedChapters`.
    async fn work_chapters_admin(&self, ctx: &Context<'_>, work_id: ID) -> Result<Vec<AdminChapter>> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let rows = catalog::work_source_chapters(&st.pool, &work_id.0)
            .await
            .map_err(gql_err)?;
        let aggregated = group_aggregated_chapters(rows);
        let ov = chapter_overrides(&st.pool, &work_id.0).await;
        Ok(aggregated
            .into_iter()
            .map(|c| {
                let key = chapter_key(c.number);
                let (hidden, title_override) = ov.get(&key).cloned().unwrap_or((false, None));
                let effective_title = title_override.clone().or_else(|| c.title.clone());
                AdminChapter {
                    number: c.number,
                    key,
                    source_title: c.title,
                    title_override,
                    effective_title,
                    hidden,
                    source_count: c.sources.len() as i32,
                }
            })
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

    /// The catalogued source mappings for one canonical work, plus each source's
    /// extension coordinates (§2.2) — what a native client needs to install the right
    /// extension and fetch chapters. The MangaDex-native mapping sorts first, then the
    /// rest by recency. Public (mirrors the catalogue reads); an opted-out viewer simply
    /// doesn't see NSFW source mappings.
    async fn work_sources(&self, ctx: &Context<'_>, work_id: ID) -> Result<Vec<WorkSource>> {
        let st = state(ctx);
        let show_nsfw = viewer_show_nsfw(ctx).await;
        load_work_sources(&st.pool, &work_id.0, show_nsfw).await
    }

    /// Batched `workSources`: one `WorkSourceGroup` per requested id, in input order. A
    /// work with no (visible) sources yields an empty `sources` list. NSFW gating is
    /// identical to `workSources`.
    async fn work_sources_batch(
        &self,
        ctx: &Context<'_>,
        work_ids: Vec<ID>,
    ) -> Result<Vec<WorkSourceGroup>> {
        // Public endpoint (H5): cap the input so one anonymous request can't fan out
        // to thousands of serial queries and drain the connection pool.
        const MAX_WORK_IDS: usize = 200;
        if work_ids.len() > MAX_WORK_IDS {
            return Err(Error::new(format!(
                "Too many work ids (max {MAX_WORK_IDS})"
            )));
        }
        let st = state(ctx);
        let show_nsfw = viewer_show_nsfw(ctx).await;
        // Single IN(...) query instead of a serial per-id loop.
        let ids: Vec<String> = work_ids.iter().map(|w| w.0.clone()).collect();
        let mut by_work = load_work_sources_batch(&st.pool, &ids, show_nsfw).await?;
        let groups = work_ids
            .into_iter()
            .map(|work_id| {
                let sources = by_work.remove(&work_id.0).unwrap_or_default();
                WorkSourceGroup { work_id, sources }
            })
            .collect();
        Ok(groups)
    }

    /// Catalogue search with optional genre + rating-range filters (S4 — drives the
    /// refined UI's rating slider + genre chips). An empty query browses the
    /// persisted catalogue from the DB cache (so filters apply across everything
    /// materialized); a text query does a live source search. `genres` matches ANY
    /// of the given genres; `minRating`/`maxRating` filter by the work's aggregate
    /// user rating (0–10; a `minRating > 0` excludes unrated series). Filters are
    /// applied to the result set (a text-query page is filtered post-fetch).
    async fn search(
        &self,
        ctx: &Context<'_>,
        query: String,
        #[graphql(default = 1)] page: i32,
        genres: Option<Vec<String>>,
        min_rating: Option<f64>,
        max_rating: Option<f64>,
    ) -> Result<SeriesPage> {
        let st = state(ctx);
        let trimmed = query.trim();
        let show_nsfw = viewer_show_nsfw(ctx).await;
        if trimmed.is_empty() {
            // F2: empty query → the filters are applied in SQL across the ENTIRE
            // persisted catalogue and paginated, so `search(genres:["Action"])`
            // returns the full catalogue-wide Action set (paged), not a slice of the
            // first N. `total` is the filtered catalogue-wide count.
            let genres_v = genres.unwrap_or_default();
            let (total, mangas) = crate::series_cache::search_catalogue(
                &st.pool,
                &genres_v,
                min_rating,
                max_rating,
                show_nsfw,
                page.max(1) as i64,
                PAGE_SIZE,
            )
            .await
            .map_err(gql_err)?;
            let items = map_series_list(st, mangas).await;
            let has_next = (page.max(1) as i64) * PAGE_SIZE < total;
            return Ok(SeriesPage {
                items,
                page,
                has_next_page: has_next,
                total: Some(total as i32),
            });
        }
        // Text query → live source search (the source's own text index isn't in our
        // DB); genre/rating filters are applied to the fetched page.
        let (has_next, mangas) = st
            .suwayomi
            .fetch_source(FetchType::Search, page, Some(trimmed))
            .await
            .map_err(gql_err)?;
        let mapped = filter_nsfw(show_nsfw, map_series_list(st, mangas).await);
        let items = apply_search_filters(mapped, genres.as_deref(), min_rating, max_rating);
        Ok(SeriesPage {
            items,
            page,
            has_next_page: has_next,
            total: None,
        })
    }

    /// The available genre/tag facets across the persisted catalogue (S4), most
    /// common first — the full set the sources provide, for the search UI's genre
    /// filter. Empty until the catalogue has been persisted (ingest/scan/persistCatalogue).
    async fn genre_facets(&self, ctx: &Context<'_>) -> Result<Vec<GenreFacet>> {
        let st = state(ctx);
        Ok(crate::series_cache::genre_facets(&st.pool)
            .await
            .map_err(gql_err)?
            .into_iter()
            .map(|(genre, count)| GenreFacet {
                genre,
                count: count as i32,
            })
            .collect())
    }

    /// Federated multi-extension catalogue search (S3). Fans the query out to
    /// every installed Suwayomi source (bounded concurrency + per-source timeout,
    /// failures skipped), runs each hit through the dedup matcher against the
    /// native catalogue, PERSISTS the matching/top-N source mappings so results
    /// consolidate under one canonical work, and returns deduped canonical entries
    /// each with its per-source translator list. User-facing: NSFW sources/results
    /// are hidden unless the viewer opted in (same posture as `search`).
    async fn search_all_sources(
        &self,
        ctx: &Context<'_>,
        query: String,
        #[graphql(default = 1)] page: i32,
    ) -> Result<FederatedSearchPage> {
        // C1: authenticated only — this endpoint fans out to many sources AND
        // persists (library enrollment + dedup writes), so anonymous callers are
        // rejected outright (no anonymous writes).
        let user = require_user(ctx).await?;
        let trimmed = query.trim().to_string();
        if trimmed.is_empty() {
            return Err(Error::new("query must not be empty"));
        }
        let st = ctx.data_unchecked::<std::sync::Arc<AppState>>().clone();
        // C1: per-user rate limit so an authed client can't hammer the engine /
        // MangaDex through the fan-out + persist path.
        if let Some(retry) = st.federated_limiter.is_limited(&format!("fed:{}", user.id)) {
            return Err(Error::new(format!(
                "Too many source searches — retry in {retry}s"
            )));
        }
        st.federated_limiter.record(&format!("fed:{}", user.id));
        let show_nsfw = user_show_nsfw(&st.pool, &user.id).await;
        federated_search(&st, &trimmed, page.max(1), show_nsfw, Some(user)).await
    }

    async fn series(&self, ctx: &Context<'_>, id: ID) -> Result<Series> {
        let st = state(ctx);
        let n = id.0.parse::<i64>().map_err(gql_err)?;
        // Suwayomi ids are sequential integers, so gate before the source round-trip:
        // an opted-out viewer must not read the detail of an NSFW series by id (N2).
        if canonical_is_nsfw(&st.pool, &id.0).await && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such series"));
        }
        // S1: serve from the DB cache; only live-fetch (and cache) on a miss.
        let m = resolve_series_cached(st, n).await.map_err(gql_err)?;
        Ok(map_series(st, m).await)
    }

    async fn chapters(&self, ctx: &Context<'_>, series_id: ID) -> Result<Vec<Chapter>> {
        let st = state(ctx);
        let n = series_id.0.parse::<i64>().map_err(gql_err)?;
        // Gate the chapter list on the owning series' NSFW flag (same as `series`, N2).
        if canonical_is_nsfw(&st.pool, &series_id.0).await && !viewer_show_nsfw(ctx).await {
            return Err(Error::new("No such series"));
        }
        // S1: serve from the DB cache; only live-fetch (and cache) on a miss.
        let list = resolve_chapters_cached(st, n).await.map_err(gql_err)?;
        // Overlay the VIEWER's per-user read state (`suwayomi_progress`) — the cached
        // `suwayomi_chapter` read flags are global and no longer authoritative (CR6).
        let user = current_user(ctx).await;
        let progress =
            suwayomi_progress_map(&st.pool, user.as_ref().map(|u| u.id.as_str()), n).await;
        Ok(list
            .into_iter()
            .map(|c| {
                let p = progress.get(&c.id).copied();
                map_chapter(c, p)
            })
            .collect())
    }

    async fn pages(&self, ctx: &Context<'_>, chapter_id: ID) -> Result<Vec<Page>> {
        let st = state(ctx);
        let n = chapter_id.0.parse::<i64>().map_err(gql_err)?;
        // The NSFW flag lives on the owning series/work, not the chapter, and Suwayomi
        // chapters aren't mirrored locally — so resolve the manga id from the source and
        // gate the page images exactly like `series`/`chapters` (N2). An unknown chapter
        // is allowed through to the (cleanly-failing) page fetch, mirroring canonicalPages.
        if let Some(manga_id) = st.suwayomi.chapter_manga_id(n).await.map_err(gql_err)? {
            if canonical_is_nsfw(&st.pool, &manga_id.to_string()).await
                && !viewer_show_nsfw(ctx).await
            {
                return Err(Error::new("No such chapter"));
            }
        }
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
        // "Your Library" is PER-USER: only the series the viewer has added
        // (`user_library`), newest-added first. An anonymous visitor has no library —
        // return empty rather than the whole catalogue. (This replaced returning
        // Suwayomi's shared in-library set, which showed every visitor the same
        // ~571-series "library".)
        let Some(user) = current_user(ctx).await else {
            return Ok(Vec::new());
        };
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT series_id FROM user_library WHERE user_id = ? ORDER BY created_at DESC",
        )
        .bind(&user.id)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        // Build the library in the stored (newest-added-first) order. Canonical works
        // fill their slot immediately; numeric Suwayomi series reserve a slot and are
        // mapped together in ONE batched pass afterwards (O(1) grouped queries per
        // lookup instead of ~5 per series), preserving the interleaved order.
        let mut out: Vec<Option<Series>> = Vec::with_capacity(ids.len());
        let mut pending: Vec<(usize, SuwayomiManga)> = Vec::new();
        for sid in ids {
            if sid.starts_with("w_") {
                // Canonical (MangaDex-mirrored) work.
                if let Some(work) = catalog::load_canonical_work(&st.pool, &sid)
                    .await
                    .map_err(gql_err)?
                {
                    let chapters = catalog::load_canonical_chapters(&st.pool, &sid)
                        .await
                        .map_err(gql_err)?;
                    out.push(Some(
                        map_canonical_series(
                            &st.pool,
                            Some(user.id.as_str()),
                            work,
                            catalog::main_chapter_count_str(&chapters) as i32,
                        )
                        .await,
                    ));
                }
            } else if let Ok(n) = sid.parse::<i64>() {
                // Numeric Suwayomi series — DB cache first, live fetch on a miss.
                let m = match crate::series_cache::get_series(&st.pool, n)
                    .await
                    .map_err(gql_err)?
                {
                    Some(m) => Some(m),
                    None => st.suwayomi.series(n).await.ok(),
                };
                if let Some(m) = m {
                    pending.push((out.len(), m));
                    out.push(None); // slot filled by the batched map below
                }
            }
        }
        // Map all numeric Suwayomi series at once, then drop each into its slot.
        let (indices, mangas): (Vec<usize>, Vec<SuwayomiManga>) = pending.into_iter().unzip();
        for (idx, series) in indices.into_iter().zip(map_series_batch(st, mangas).await) {
            out[idx] = Some(series);
        }
        let out: Vec<Series> = out.into_iter().flatten().collect();
        // Hide NSFW series from the library too, unless the viewer opted in (N2).
        Ok(filter_nsfw(viewer_show_nsfw(ctx).await, out))
    }

    /// Per-series read progress for the viewer's library, in a couple of grouped
    /// queries. The Library and Profile screens use this to shelve series by progress
    /// (reading / completed / plan) without fetching each series' chapter list — the
    /// N-round-trip fan-out that hung both pages. Empty for anonymous viewers.
    ///
    /// Numeric Suwayomi series read counts come from the viewer's per-user
    /// `suwayomi_progress`; canonical `w_` works from the per-user `canonical_progress`.
    /// A series with no progress rows is omitted; the client then treats it as unread
    /// and uses `chapterCount` for the total.
    async fn library_progress(&self, ctx: &Context<'_>) -> Result<Vec<SeriesProgress>> {
        let st = state(ctx);
        let Some(user) = current_user(ctx).await else {
            return Ok(Vec::new());
        };
        // Numeric Suwayomi series in the viewer's library — read count from the viewer's
        // own `suwayomi_progress` (no longer the shared `suwayomi_chapter.is_read`).
        // `total` is left 0 so the client falls back to the series' `chapterCount`
        // (only chapters the viewer has touched have progress rows).
        let mut out: Vec<SeriesProgress> = sqlx::query_as::<_, (String, i64)>(
            "SELECT sp.series_id, COALESCE(SUM(sp.read), 0) AS read \
             FROM suwayomi_progress sp \
             JOIN user_library ul ON ul.series_id = sp.series_id AND ul.user_id = sp.user_id \
             WHERE sp.user_id = ? \
             GROUP BY sp.series_id",
        )
        .bind(&user.id)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?
        .into_iter()
        .map(|(id, read)| SeriesProgress {
            id: ID(id),
            read: read as i32,
            total: 0,
        })
        .collect();
        // Canonical works in the viewer's library — read from per-user progress;
        // `total` is left 0 so the client falls back to the work's `chapterCount`.
        let canon = sqlx::query_as::<_, (String, i64)>(
            "SELECT cp.work_id, COALESCE(SUM(cp.read), 0) AS read \
             FROM canonical_progress cp \
             JOIN user_library ul ON ul.series_id = cp.work_id AND ul.user_id = cp.user_id \
             WHERE cp.user_id = ? \
             GROUP BY cp.work_id",
        )
        .bind(&user.id)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        for (work_id, read) in canon {
            out.push(SeriesProgress {
                id: ID(work_id),
                read: read as i32,
                total: 0,
            });
        }
        Ok(out)
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
             WHERE r.series_id = ? AND u.is_banned = 0 ORDER BY r.created_at DESC, r.id DESC LIMIT ? OFFSET ?",
        )
        .bind(series_id.0.clone())
        .bind(PAGE_SIZE + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM reviews r JOIN users u ON u.id = r.user_id \
             WHERE r.series_id = ? AND u.is_banned = 0",
        )
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

    /// The signed-in viewer's own review for a series, fetched by user identity
    /// so it's always retrievable regardless of pagination. The paginated
    /// `reviews` list only returns page 1 (`created_at DESC`, `PAGE_SIZE`); on a
    /// busy series the viewer's earlier review can fall off it, which would show
    /// them as unrated with an empty body. Returns `null` when the viewer has no
    /// review, or when the request is anonymous. No `is_banned` filter here: this
    /// returns the viewer's OWN review and the viewer is authenticated (a banned
    /// user can't sign in), so the ban filter is both unnecessary and wrong.
    async fn my_review(&self, ctx: &Context<'_>, series_id: ID) -> Result<Option<Review>> {
        let Some(user) = current_user(ctx).await else {
            return Ok(None);
        };
        let st = state(ctx);
        let row: Option<ReviewJoin> = sqlx::query_as(
            "SELECT r.id, r.series_id, r.score, r.body, r.has_spoiler, r.created_at, r.updated_at, \
             u.id AS author_id, u.username AS author_username, u.avatar_url AS author_avatar \
             FROM reviews r JOIN users u ON u.id = r.user_id \
             WHERE r.series_id = ? AND r.user_id = ?",
        )
        .bind(series_id.0.clone())
        .bind(&user.id)
        .fetch_optional(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(row.map(Review::from))
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
        // The viewer's own vote per comment is selected inline; '' (anonymous) matches
        // no vote row, yielding my_vote = 0.
        let viewer_id = current_user(ctx).await.map(|u| u.id).unwrap_or_default();
        // Pagination is over ROOT comments (whole threads); a page returns those
        // roots plus *all* their descendants (flat, ascending) so the client can
        // assemble the full reply tree without extra round-trips. Banned authors'
        // subtrees are pruned at every depth (the recursion re-checks `is_banned`),
        // so a banned reply never orphans the branch beneath it in the response.
        // Like/dislike tallies and the viewer's vote are correlated subqueries so the
        // whole thread's votes come back in this one query (no per-comment fan-out).
        let rows: Vec<CommentJoin> = sqlx::query_as(
            "WITH RECURSIVE roots AS ( \
                 SELECT c.id, c.created_at FROM comments c JOIN users u ON u.id = c.user_id \
                 WHERE c.target_type = ? AND c.target_id = ? AND c.parent_id IS NULL \
                   AND u.is_banned = 0 \
                 ORDER BY c.created_at ASC, c.id ASC LIMIT ? OFFSET ? \
             ), \
             thread(id) AS ( \
                 SELECT id FROM roots \
                 UNION ALL \
                 SELECT c.id FROM comments c JOIN thread t ON c.parent_id = t.id \
                     JOIN users u ON u.id = c.user_id WHERE u.is_banned = 0 \
             ) \
             SELECT c.id, c.target_type, c.target_id, c.parent_id, c.body, c.has_spoiler, \
                    c.created_at, u.id AS author_id, u.username AS author_username, \
                    u.avatar_url AS author_avatar, \
                    m.id AS media_id, m.width AS media_width, m.height AS media_height, \
                    COALESCE((SELECT COUNT(*) FROM comment_votes v \
                              WHERE v.comment_id = c.id AND v.value = 1), 0) AS likes, \
                    COALESCE((SELECT COUNT(*) FROM comment_votes v \
                              WHERE v.comment_id = c.id AND v.value = -1), 0) AS dislikes, \
                    COALESCE((SELECT v.value FROM comment_votes v \
                              WHERE v.comment_id = c.id AND v.user_id = ?), 0) AS my_vote \
             FROM comments c JOIN users u ON u.id = c.user_id \
             LEFT JOIN comment_media m ON m.comment_id = c.id \
             WHERE c.id IN (SELECT id FROM thread) \
             ORDER BY c.created_at ASC",
        )
        .bind(target_type)
        .bind(target_id.0.clone())
        .bind(PAGE_SIZE)
        .bind(offset)
        .bind(&viewer_id)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        // `total` and `has_next_page` count root threads (the paginated unit).
        let total_roots: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM comments c JOIN users u ON u.id = c.user_id \
             WHERE c.target_type = ? AND c.target_id = ? AND c.parent_id IS NULL \
               AND u.is_banned = 0",
        )
        .bind(target_type)
        .bind(target_id.0.clone())
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;
        let has_next = offset + PAGE_SIZE < total_roots;
        let items = rows.into_iter().map(Comment::from).collect();
        Ok(CommentPage {
            items,
            page,
            has_next_page: has_next,
            total: Some(total_roots as i32),
        })
    }

    /// Aggregate health of the background scan scheduler (admin console).
    async fn scan_status(&self, ctx: &Context<'_>) -> Result<ScanStatus> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let (library_size, overdue_count, last_tick_at) = {
            // Recover from a poisoned lock rather than propagating the panic.
            let h = st.scan_health.lock().unwrap_or_else(|e| e.into_inner());
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
             FROM users ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
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
                    user: build_session_user(&state(ctx).pool, &u, show_nsfw).await,
                }))
            }
            None => Ok(None),
        }
    }

    /// The signed-in user's recent activity feed (newest first). Empty when
    /// signed out. `limit` is clamped to [1, 50].
    async fn my_activity(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 20)] limit: i32,
    ) -> Result<Vec<Activity>> {
        let Some(user) = current_user(ctx).await else {
            return Ok(vec![]);
        };
        let limit = limit.clamp(1, 50) as i64;
        let rows = sqlx::query_as::<_, ActivityRow>(
            "SELECT id, kind, target_type, target_id, created_at \
             FROM user_activity WHERE user_id = ? \
             ORDER BY created_at DESC LIMIT ?",
        )
        .bind(&user.id)
        .bind(limit)
        .fetch_all(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// The viewer's inbound notifications, newest-first (the bell feed). Empty for
    /// anonymous viewers. Each carries the actor, a short excerpt of the viewer's own
    /// comment, and the thread target for deep-linking.
    async fn notifications(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 1)] page: i32,
    ) -> Result<Vec<Notification>> {
        let Some(user) = current_user(ctx).await else {
            return Ok(vec![]);
        };
        let st = state(ctx);
        let offset = (page.max(1) as i64 - 1) * PAGE_SIZE;
        let rows: Vec<NotificationRow> = sqlx::query_as(
            "SELECT n.id, n.kind, n.count, n.created_at, n.read_at, n.target_type, \
                    n.target_id, n.comment_id, a.id AS actor_id, a.username AS actor_username, \
                    a.avatar_url AS actor_avatar, substr(c.body, 1, 140) AS comment_excerpt \
             FROM notifications n \
             LEFT JOIN users a ON a.id = n.actor_id \
             LEFT JOIN comments c ON c.id = n.comment_id \
             WHERE n.user_id = ? \
             ORDER BY n.created_at DESC LIMIT ? OFFSET ?",
        )
        .bind(&user.id)
        .bind(PAGE_SIZE)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(rows.into_iter().map(Notification::from).collect())
    }

    /// Count of the viewer's UNREAD notifications — drives the bell badge. 0 for
    /// anonymous viewers.
    async fn unread_notification_count(&self, ctx: &Context<'_>) -> Result<i32> {
        let Some(user) = current_user(ctx).await else {
            return Ok(0);
        };
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notifications WHERE user_id = ? AND read_at IS NULL",
        )
        .bind(&user.id)
        .fetch_one(&state(ctx).pool)
        .await
        .map_err(gql_err)?;
        Ok(n as i32)
    }

    /// Admin "Sources & Extensions" console (EXT-1): every Keiyoushi/Mihon
    /// extension known to the Suwayomi engine, installed or not. On first use it
    /// seeds the curated Keiyoushi index as the default store (idempotent) and
    /// refreshes the list when the engine has never fetched one. NSFW extensions
    /// are hidden unless the admin opted in via `show_nsfw` (CATALOGUE.md §2).
    async fn extensions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = false)] installed_only: bool,
        lang: Option<String>,
        nsfw: Option<bool>,
        // Re-fetch the store indexes before listing so `hasUpdate`/versions are
        // fresh (a network round-trip per store). Default false: list from the
        // engine's cached index.
        #[graphql(
            default = false,
            desc = "Re-fetch the store indexes before listing so hasUpdate/versions are fresh."
        )]
        refresh: bool,
    ) -> Result<Vec<ExtensionInfo>> {
        let user = require_admin(ctx).await?;
        let st = state(ctx);
        ensure_default_extension_store(st).await?;
        if refresh {
            st.suwayomi.refresh_extensions().await.map_err(gql_err)?;
        }
        let mut list = st.suwayomi.list_extensions().await.map_err(gql_err)?;
        // Fresh engine: the store may be registered but its index never fetched.
        if list.is_empty() && !refresh {
            st.suwayomi.refresh_extensions().await.map_err(gql_err)?;
            list = st.suwayomi.list_extensions().await.map_err(gql_err)?;
        }
        let show_nsfw = user_show_nsfw(&st.pool, &user.id).await;
        Ok(
            filter_extensions(list, installed_only, lang.as_deref(), nsfw, show_nsfw)
                .into_iter()
                .map(|e| map_extension_info(st, e))
                .collect(),
        )
    }

    /// Admin: the installed Suwayomi sources — the picker that feeds
    /// `sourceBrowse(sourceId)` (EXT-1). NSFW sources are hidden unless the
    /// admin opted in via `show_nsfw` (same posture as the extension listing and
    /// the browse gate).
    async fn sources(&self, ctx: &Context<'_>) -> Result<Vec<SourceInfo>> {
        let user = require_admin(ctx).await?;
        let st = state(ctx);
        let show_nsfw = user_show_nsfw(&st.pool, &user.id).await;
        let list = st.suwayomi.list_sources().await.map_err(gql_err)?;
        Ok(list
            .into_iter()
            .filter(|s| show_nsfw || !s.is_nsfw)
            .map(|s| {
                let icon = st.suwayomi.abs(s.icon_url.as_deref());
                SourceInfo {
                    id: ID(s.id),
                    name: s.name,
                    lang: s.lang,
                    is_nsfw: s.is_nsfw,
                    icon_url: (!icon.is_empty()).then_some(icon),
                    pkg_name: s.pkg_name,
                }
            })
            .collect())
    }

    /// Admin: "add all from source" ingest jobs, newest first (S1). Pass
    /// `active: true` for only the currently-running ones. Poll this for job
    /// progress — counters are flushed after every page.
    async fn source_ingest_jobs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = false)] active: bool,
    ) -> Result<Vec<SourceIngestJob>> {
        require_admin(ctx).await?;
        let st = state(ctx);
        Ok(crate::ingest::list_jobs(&st.pool, active)
            .await
            .map_err(gql_err)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Admin catalogue provenance, batched: for each Suwayomi series id, the
    /// canonical work it's linked to and every `source_series` mapping on that
    /// work with its extension coordinates — what the console needs to render an
    /// "extension"/source column next to search results. One group per requested
    /// id, in input order; an uncatalogued series yields `workId: null` and an
    /// empty `sources` list. NSFW mappings are hidden for an opted-out admin
    /// (same posture as `workSources`).
    async fn series_sources_batch(
        &self,
        ctx: &Context<'_>,
        series_ids: Vec<ID>,
    ) -> Result<Vec<SeriesSourceGroup>> {
        require_admin(ctx).await?;
        if series_ids.len() > 200 {
            return Err(Error::new("At most 200 ids per seriesSourcesBatch call"));
        }
        let st = state(ctx);
        let show_nsfw = viewer_show_nsfw(ctx).await;
        if series_ids.is_empty() {
            return Ok(Vec::new());
        }

        // X2: one lookup for every requested key (was one query_scalar per id).
        // A series key can map twice — the migration-0005 backfill minted
        // placeholder works with a BLANK source_id, and a later real ingest links
        // the same key under its real source id. Fetch all rows and pick, per key,
        // the real mapping (then most-recent) so provenance never surfaces the shell.
        let keys: Vec<String> = series_ids.iter().map(|id| id.0.clone()).collect();
        let placeholders = std::iter::repeat_n("?", keys.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT source_key, work_id, (source_id != '') AS is_real, last_seen \
             FROM source_series \
             WHERE source_type = 'suwayomi' AND source_key IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, (String, String, i64, String)>(&sql);
        for k in &keys {
            q = q.bind(k);
        }
        let rows = q.fetch_all(&st.pool).await.map_err(gql_err)?;
        // Best work_id per key: real mapping first, then latest last_seen.
        let mut best: HashMap<String, (i64, String, String)> = HashMap::new();
        for (key, wid, is_real, last_seen) in rows {
            let cand = (is_real, last_seen, wid);
            match best.get(&key) {
                Some((r, ls, _)) if (*r, ls.as_str()) >= (cand.0, cand.1.as_str()) => {}
                _ => {
                    best.insert(key, cand);
                }
            }
        }
        let key_to_work: HashMap<String, String> =
            best.into_iter().map(|(k, (_, _, wid))| (k, wid)).collect();

        // X2: one batched work-sources load for all distinct linked works.
        let distinct_works: Vec<String> = {
            let mut v: Vec<String> = key_to_work.values().cloned().collect();
            v.sort();
            v.dedup();
            v
        };
        let sources_by_work = load_work_sources_batch(&st.pool, &distinct_works, show_nsfw).await?;

        // Assemble in input order.
        let groups = series_ids
            .into_iter()
            .map(|series_id| {
                let work_id = key_to_work.get(&series_id.0).cloned();
                let sources = work_id
                    .as_ref()
                    .and_then(|w| sources_by_work.get(w).cloned())
                    .unwrap_or_default();
                SeriesSourceGroup {
                    series_id,
                    work_id: work_id.map(ID),
                    sources,
                }
            })
            .collect();
        Ok(groups)
    }

    /// Admin bulk-ingest picker (EXT-1): browse/search one Suwayomi source's
    /// catalogue. Every returned manga is persisted by Suwayomi and gets the
    /// internal id `bulkAddSourceSeries` consumes (GQL-SCHEMA-FINDINGS.md §A0).
    /// An NSFW source is refused unless the admin opted in (show_nsfw posture).
    async fn source_browse(
        &self,
        ctx: &Context<'_>,
        source_id: ID,
        #[graphql(name = "type")] ty: SourceBrowseType,
        #[graphql(default = 1)] page: i32,
        query: Option<String>,
    ) -> Result<SourceBrowsePage> {
        let user = require_admin(ctx).await?;
        let st = state(ctx);
        if !user_show_nsfw(&st.pool, &user.id).await {
            let (_, source_nsfw) = st
                .suwayomi
                .source_meta(&source_id.0)
                .await
                .map_err(gql_err)?;
            if source_nsfw {
                return Err(Error::new(
                    "This source is NSFW — enable NSFW in your settings to browse it",
                ));
            }
        }
        let (has_next, mangas) = st
            .suwayomi
            .browse_source(&source_id.0, ty.into(), page, query.as_deref())
            .await
            .map_err(gql_err)?;
        let items = mangas
            .into_iter()
            .map(|m| {
                let thumb = st.suwayomi.abs(m.thumbnail_url.as_deref());
                SourceBrowseEntry {
                    suwayomi_manga_id: ID(m.id.to_string()),
                    title: m.title,
                    thumbnail_url: (!thumb.is_empty()).then_some(thumb),
                    in_library: m.in_library,
                }
            })
            .collect();
        Ok(SourceBrowsePage {
            items,
            page,
            has_next_page: has_next,
        })
    }
}

/// The curated Keiyoushi extension index — the hardcoded default store
/// (CATALOGUE.md §Tier-2: curated, not crawled; `keiyoushi.github.io` 404s, the
/// raw `repo` branch is the working host).
const KEIYOUSHI_INDEX_URL: &str =
    "https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.min.json";

/// Seed the Keiyoushi index as the default extension store when the engine has
/// NONE configured. Presence can't be checked by URL equality (Suwayomi
/// canonicalizes `index.min.json` → `index.pb` on add), so "any store exists"
/// is the idempotency signal — an operator who removed the default on purpose
/// and added another store is left alone.
async fn ensure_default_extension_store(st: &AppState) -> Result<()> {
    let count = st.suwayomi.extension_store_count().await.map_err(gql_err)?;
    if count == 0 {
        let name = st
            .suwayomi
            .add_extension_store(KEIYOUSHI_INDEX_URL)
            .await
            .map_err(gql_err)?;
        tracing::info!(store = name, "seeded default Keiyoushi extension store");
    }
    Ok(())
}

/// Derive a browser-reachable, store-hosted icon URL for an extension from its
/// repo index URL: the Mihon/Keiyoushi repo layout hosts icons next to the index
/// at `icon/{pkgName}.png` (verified live: 200 image/png for installed and
/// not-installed extensions alike). GitHub `/raw/` paths are rewritten to the
/// direct `raw.githubusercontent.com` host to skip the 302 redirect. `None` when
/// the repo URL doesn't end in an index file (unknown layout — caller falls back
/// to the engine icon endpoint).
fn store_icon_url(repo: &str, pkg_name: &str) -> Option<String> {
    let (base, index) = repo.rsplit_once('/')?;
    if !index.starts_with("index.") {
        return None;
    }
    let base = match base.strip_prefix("https://github.com/") {
        Some(rest) if rest.contains("/raw/") => format!(
            "https://raw.githubusercontent.com/{}",
            rest.replacen("/raw/", "/", 1)
        ),
        _ => base.to_string(),
    };
    Some(format!("{base}/icon/{pkg_name}.png"))
}

/// Map a Suwayomi extension row onto the GraphQL `ExtensionInfo`. The icon URL
/// prefers the STORE-hosted icon (derived from the repo index URL): the engine's
/// own `/api/v1/extension/icon/…` endpoint serves them too, but only where the
/// engine host is browser-reachable — in deployments where Suwayomi stays
/// internal, those icons never load in the admin UI. The engine URL (absolutized
/// against the public image base) remains the fallback for extensions without a
/// derivable store icon (e.g. locally-uploaded APKs).
fn map_extension_info(st: &AppState, mut e: crate::suwayomi::ExtensionListEntry) -> ExtensionInfo {
    let store_icon = e
        .repo
        .as_deref()
        .and_then(|r| store_icon_url(r, &e.pkg_name));
    let icon = store_icon.or_else(|| {
        // L1: `store_icon_url` only knows the GitHub raw / `index.*` layout. For a
        // repo it can't map, we fall back to the engine icon endpoint — which only
        // loads where the Suwayomi host is browser-reachable. Log it so a
        // non-loading icon in a non-GitHub-repo deployment is diagnosable rather
        // than silent.
        if e.repo.is_some() {
            tracing::debug!(
                pkg = %e.pkg_name,
                repo = e.repo.as_deref().unwrap_or(""),
                "extension icon: no store-hosted URL for this repo layout; falling back to engine icon"
            );
        }
        let a = st.suwayomi.abs(e.icon_url.as_deref());
        (!a.is_empty()).then_some(a)
    });
    e.icon_url = icon;
    e.into()
}

/// Pure filter for the `extensions` listing. The viewer's `show_nsfw` posture
/// wins over any explicit `nsfw` filter: an opted-out admin never sees NSFW
/// extensions, even asking for them (CATALOGUE.md §2).
fn filter_extensions(
    list: Vec<crate::suwayomi::ExtensionListEntry>,
    installed_only: bool,
    lang: Option<&str>,
    nsfw: Option<bool>,
    show_nsfw: bool,
) -> Vec<crate::suwayomi::ExtensionListEntry> {
    list.into_iter()
        .filter(|e| !installed_only || e.is_installed)
        .filter(|e| lang.is_none_or(|l| e.lang == l))
        .filter(|e| nsfw.is_none_or(|n| e.is_nsfw == n))
        .filter(|e| show_nsfw || !e.is_nsfw)
        .collect()
}

/// Fold per-id `bulkAddSourceSeries` outcomes into the summary counts.
fn summarize_bulk(entries: Vec<BulkAddEntry>) -> BulkAddResult {
    let total = entries.len() as i32;
    let (mut succeeded, mut failed) = (0, 0);
    let (mut new_works, mut auto_merged, mut queued_for_review, mut already_existing) =
        (0, 0, 0, 0);
    for e in &entries {
        match &e.result {
            Some(r) => {
                succeeded += 1;
                match r.decision.as_str() {
                    "new" => new_works += 1,
                    "auto_merge" => auto_merged += 1,
                    "review" => queued_for_review += 1,
                    "existing" => already_existing += 1,
                    _ => {}
                }
            }
            None => failed += 1,
        }
    }
    BulkAddResult {
        entries,
        total,
        succeeded,
        failed,
        new_works,
        auto_merged,
        queued_for_review,
        already_existing,
    }
}

/// One `source_series` row LEFT-JOINed to its `source_extension` coordinates. The
/// extension columns are all `Option` because a MangaDex-native mapping (source_id
/// `'mangadex'`) has no extension row, and a Suwayomi source may not be catalogued yet.
#[derive(sqlx::FromRow)]
struct WorkSourceRow {
    work_id: String,
    source_type: String,
    source_id: String,
    source_key: String,
    source_url: Option<String>,
    is_nsfw: i64,
    pkg_name: Option<String>,
    repo_url: Option<String>,
    apk_name: Option<String>,
    version_code: Option<i64>,
    ext_lang: Option<String>,
}

/// Map a joined row to a `WorkSource`, honoring the opted-out NSFW filter (returns
/// `None` for an NSFW mapping a non-opted-in viewer must not see).
fn work_source_from_row(r: WorkSourceRow, show_nsfw: bool) -> Option<WorkSource> {
    if !show_nsfw && r.is_nsfw != 0 {
        return None;
    }
    // The LEFT JOIN matches iff the source has a catalogued extension.
    let extension = match (r.pkg_name, r.repo_url) {
        (Some(pkg_name), Some(repo_url)) => Some(SourceExtension {
            pkg_name,
            repo_url,
            apk_name: r.apk_name,
            version_code: r.version_code.map(|v| v as i32),
            lang: r.ext_lang.clone(),
        }),
        _ => None,
    };
    Some(WorkSource {
        source_type: r.source_type,
        source_id: r.source_id,
        source_key: r.source_key,
        source_url: r.source_url,
        is_nsfw: r.is_nsfw != 0,
        lang: r.ext_lang,
        extension,
    })
}

const WORK_SOURCE_SELECT: &str =
    "SELECT ss.work_id, ss.source_type, ss.source_id, ss.source_key, ss.source_url, ss.is_nsfw, \
            se.pkg_name, se.repo_url, se.apk_name, se.version_code, se.lang AS ext_lang \
     FROM source_series ss \
     LEFT JOIN source_extension se ON se.source_id = ss.source_id";

/// The AUTHORITATIVE Suwayomi `source_key`s for a work (F1) — one per source_id,
/// the mapping with the most cached chapters. Used to drop redundant same-source
/// mappings from `workSources`/translators so they agree with `aggregatedChapters`.
async fn authoritative_key_set(
    pool: &SqlitePool,
    work_id: &str,
) -> Result<std::collections::HashSet<String>> {
    Ok(catalog::authoritative_suwayomi_mappings(pool, work_id)
        .await
        .map_err(gql_err)?
        .into_iter()
        .map(|m| m.source_key)
        .collect())
}

/// Keep a work's source mappings, dropping redundant same-source Suwayomi mappings
/// (F1): a Suwayomi row survives only if its `source_key` is the authoritative one
/// for its `source_id`. MangaDex / non-Suwayomi rows always survive.
fn retain_authoritative(
    sources: Vec<WorkSource>,
    keys: &std::collections::HashSet<String>,
) -> Vec<WorkSource> {
    sources
        .into_iter()
        .filter(|s| s.source_type != "suwayomi" || keys.contains(&s.source_key))
        .collect()
}

/// Load a canonical work's source mappings, MangaDex-native first then by recency,
/// dropping NSFW rows for an opted-out viewer AND redundant same-source Suwayomi
/// mappings (F1 — only the authoritative, most-complete mapping per source survives,
/// so translators agree with `aggregatedChapters`). The extension is populated only
/// when the LEFT JOIN matched; `WorkSource.lang` is the extension's lang.
async fn load_work_sources(
    pool: &SqlitePool,
    work_id: &str,
    show_nsfw: bool,
) -> Result<Vec<WorkSource>> {
    let rows = sqlx::query_as::<_, WorkSourceRow>(&format!(
        "{WORK_SOURCE_SELECT} WHERE ss.work_id = ? \
         ORDER BY (ss.source_type = 'mangadex') DESC, ss.last_seen DESC"
    ))
    .bind(work_id)
    .fetch_all(pool)
    .await
    .map_err(gql_err)?;
    let mapped: Vec<WorkSource> = rows
        .into_iter()
        .filter_map(|r| work_source_from_row(r, show_nsfw))
        .collect();
    let keys = authoritative_key_set(pool, work_id).await?;
    Ok(retain_authoritative(mapped, &keys))
}

/// Batched `load_work_sources` (X2): one query for many work ids, returning a map
/// `work_id -> [WorkSource]`. Each work's list keeps the same MangaDex-first,
/// recency order as the single-work loader. Empty input → empty map.
async fn load_work_sources_batch(
    pool: &SqlitePool,
    work_ids: &[String],
    show_nsfw: bool,
) -> Result<HashMap<String, Vec<WorkSource>>> {
    if work_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat_n("?", work_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "{WORK_SOURCE_SELECT} WHERE ss.work_id IN ({placeholders}) \
         ORDER BY (ss.source_type = 'mangadex') DESC, ss.last_seen DESC"
    );
    let mut q = sqlx::query_as::<_, WorkSourceRow>(&sql);
    for wid in work_ids {
        q = q.bind(wid);
    }
    let rows = q.fetch_all(pool).await.map_err(gql_err)?;
    let mut map: HashMap<String, Vec<WorkSource>> = HashMap::new();
    for r in rows {
        let wid = r.work_id.clone();
        if let Some(ws) = work_source_from_row(r, show_nsfw) {
            map.entry(wid).or_default().push(ws);
        }
    }
    // F1: drop redundant same-source Suwayomi mappings per work (keep the
    // authoritative, most-complete one), consistent with the single-work loader.
    for (wid, sources) in map.iter_mut() {
        let keys = authoritative_key_set(pool, wid).await?;
        let taken = std::mem::take(sources);
        *sources = retain_authoritative(taken, &keys);
    }
    Ok(map)
}

// ---- Federated multi-extension search (S3) ---------------------------------

/// Deterministic relevance ranking for federated hits (X4): exact normalized-title
/// matches to `query` first, ties keeping the incoming (source-index) order — a
/// stable sort. Pure so the ordering is unit-testable without a live fan-out.
fn rank_federated_hits(hits: &mut [crate::suwayomi::SuwayomiManga], query: &str) {
    let nq = crate::catalog::normalize::normalize_title(query);
    hits.sort_by_key(|m| crate::catalog::normalize::normalize_title(&m.title) != nq);
}

/// Max installed sources fanned out per federated search. The MangaDex extension
/// alone exposes ~60 per-language sources; searching all of them for one query is
/// wasteful, so the fan-out dedupes to one source per extension (English-preferred)
/// and caps the total here.
const FEDERATED_MAX_SOURCES: usize = 24;
/// Bounded concurrency for the fan-out (polite toward the shared engine).
const FEDERATED_CONCURRENCY: usize = 6;
/// Per-source search timeout — a slow/hanging source is skipped, not awaited.
const FEDERATED_SOURCE_TIMEOUT_SECS: u64 = 8;
/// Anti-bloat policy (S3): a hit that does NOT match an existing work is persisted
/// only when it's among the first N ranked hits; matches always persist. Bounds how
/// many brand-new works a single user search can mint.
const FEDERATED_TOPN_NEW: usize = 20;
/// Hard ceiling on total persists per search, so a pathological fan-out can't run
/// hundreds of detail-fetch + library-write round-trips.
const FEDERATED_MAX_PERSIST: usize = 40;

/// Pick the sources to fan out to: drop NSFW ones for an opted-out viewer, then
/// keep ONE source per extension package (English-preferred, else first seen),
/// capped at `FEDERATED_MAX_SOURCES`. Deduping by pkg avoids the 60×-per-language
/// MangaDex explosion. Pure so it's unit-testable.
fn select_federated_sources(
    mut sources: Vec<crate::suwayomi::SuwayomiSource>,
    show_nsfw: bool,
) -> Vec<crate::suwayomi::SuwayomiSource> {
    sources.retain(|s| s.id != "0" && (show_nsfw || !s.is_nsfw));
    // Stable English-first ordering so the per-pkg pick is deterministic.
    sources.sort_by(|a, b| {
        let ae = (a.lang != "en") as u8;
        let be = (b.lang != "en") as u8;
        ae.cmp(&be).then_with(|| a.id.cmp(&b.id))
    });
    let mut seen_pkg: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for s in sources {
        // Key on the extension pkg when known, else the source id (a source with no
        // pkg is its own extension for dedup purposes).
        let key = s.pkg_name.clone().unwrap_or_else(|| s.id.clone());
        if seen_pkg.insert(key) {
            out.push(s);
            if out.len() >= FEDERATED_MAX_SOURCES {
                break;
            }
        }
    }
    out
}

/// Map a work's `WorkSource` rows onto `Translator`s, enriching each Suwayomi
/// mapping with its source's live display name + icon from `sources_by_id`
/// (built once per search from the installed-source list). The MangaDex spine
/// mapping becomes a "MangaDex" translator with no `suwayomiMangaId`.
fn work_sources_to_translators(
    st: &AppState,
    sources: Vec<WorkSource>,
    sources_by_id: &HashMap<String, crate::suwayomi::SuwayomiSource>,
) -> Vec<Translator> {
    sources
        .into_iter()
        .map(|ws| {
            if ws.source_type == "mangadex" {
                return Translator {
                    source_type: ws.source_type,
                    source_id: ws.source_id,
                    source_name: Some("MangaDex".to_string()),
                    lang: ws.lang,
                    suwayomi_manga_id: None,
                    extension_pkg_name: None,
                    extension_icon_url: None,
                };
            }
            let live = sources_by_id.get(&ws.source_id);
            // Prefer the STORE-hosted extension icon (browser-reachable even when
            // the engine host is internal), like the `extensions` surface; fall
            // back to the live source's engine icon (absolutized).
            let icon = ws
                .extension
                .as_ref()
                .and_then(|e| store_icon_url(&e.repo_url, &e.pkg_name))
                .or_else(|| {
                    live.and_then(|s| s.icon_url.as_deref())
                        .map(|u| st.suwayomi.abs(Some(u)))
                        .filter(|u| !u.is_empty())
                });
            Translator {
                source_name: live.map(|s| s.name.clone()),
                lang: live
                    .map(|s| s.lang.clone())
                    .or(ws.lang.clone())
                    .filter(|l| !l.is_empty()),
                extension_pkg_name: ws
                    .extension
                    .as_ref()
                    .map(|e| e.pkg_name.clone())
                    .or_else(|| live.and_then(|s| s.pkg_name.clone())),
                extension_icon_url: icon,
                // The Suwayomi manga id the reader fetches chapters with.
                suwayomi_manga_id: Some(ID(ws.source_key)),
                source_type: ws.source_type,
                source_id: ws.source_id,
            }
        })
        .collect()
}

/// The federated search core. Fans out, consolidates via dedup, and builds the
/// per-work translator lists. Separated from the resolver so the pieces
/// (`select_federated_sources`) stay unit-testable.
async fn federated_search(
    st: &std::sync::Arc<AppState>,
    query: &str,
    page: i32,
    show_nsfw: bool,
    user: Option<User>,
) -> Result<FederatedSearchPage> {
    // Installed sources → the fan-out set + a lookup for translator enrichment.
    let all_sources = st.suwayomi.list_sources().await.map_err(gql_err)?;
    let sources_by_id: HashMap<String, crate::suwayomi::SuwayomiSource> = all_sources
        .iter()
        .cloned()
        .map(|s| (s.id.clone(), s))
        .collect();
    let targets = select_federated_sources(all_sources, show_nsfw);
    let sources_queried = targets.len() as i32;

    // Fan out with bounded concurrency + a per-source timeout. A failing or slow
    // source is skipped (logged), never fatal. Each task carries its source's
    // selection INDEX so results can be reassembled deterministically regardless of
    // completion order (X4).
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(FEDERATED_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for (idx, src) in targets.into_iter().enumerate() {
        let st2 = st.clone();
        let sem2 = sem.clone();
        let q = query.to_string();
        set.spawn(async move {
            let _permit = sem2.acquire().await.ok()?;
            let fut = st2
                .suwayomi
                .browse_source(&src.id, FetchType::Search, page, Some(&q));
            match tokio::time::timeout(
                std::time::Duration::from_secs(FEDERATED_SOURCE_TIMEOUT_SECS),
                fut,
            )
            .await
            {
                Ok(Ok((has_next, mangas))) => Some((idx, has_next, mangas)),
                Ok(Err(e)) => {
                    tracing::warn!(source_id = src.id, error = %e, "federated: source search failed");
                    None
                }
                Err(_) => {
                    tracing::warn!(source_id = src.id, "federated: source search timed out");
                    None
                }
            }
        });
    }
    // Gather per-source results, then reassemble in source-index order (X4: NOT
    // JoinSet completion order, which is nondeterministic and made which hits fell
    // inside the top-N vary per request).
    let mut per_source: Vec<(usize, Vec<crate::suwayomi::SuwayomiManga>)> = Vec::new();
    let mut any_has_next = false;
    while let Some(joined) = set.join_next().await {
        if let Ok(Some((idx, has_next, mangas))) = joined {
            any_has_next = any_has_next || has_next;
            per_source.push((idx, mangas));
        }
    }
    per_source.sort_by_key(|(idx, _)| *idx);
    // Flatten preserving source order + per-source position, deduped by manga id.
    let mut hits: Vec<crate::suwayomi::SuwayomiManga> = Vec::new();
    let mut seen_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (_, mangas) in per_source {
        for m in mangas {
            if seen_ids.insert(m.id) {
                hits.push(m);
            }
        }
    }
    // Rank exact-title matches first (relevance), keeping source order within ties
    // — a stable sort over the already source-ordered list. This makes the top-N
    // cutoff deterministic and relevance-led rather than fastest-source-led (X4).
    rank_federated_hits(&mut hits, query);

    // Consolidate: persist matching / top-N hits so they resolve to canonical
    // works. The ranked order above makes the top-N deterministic (X4).
    let mut work_order: Vec<String> = Vec::new();
    let mut seen_works: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut persisted = 0usize;
    for (idx, m) in hits.iter().enumerate() {
        if persisted >= FEDERATED_MAX_PERSIST {
            break;
        }
        // Cheap match probe: does this title resolve to an existing work? (Title
        // only — the full corroborated dedup runs inside federated_ingest.)
        let cand = crate::dedup::Candidate {
            title: m.title.clone(),
            ..Default::default()
        };
        let probe = crate::dedup::resolve(&st.pool, &cand)
            .await
            .map_err(gql_err)?;
        let matched_work = match &probe {
            crate::dedup::Decision::AutoMerge { work_id, .. }
            | crate::dedup::Decision::Review { work_id, .. } => Some(work_id.clone()),
            crate::dedup::Decision::New => None,
        };
        // M1: never persist NOR library-enroll an NSFW hit for an opted-out viewer.
        // NSFW = genre signal on the hit, OR it matches an already-NSFW work.
        if !show_nsfw {
            let matched_nsfw = match &matched_work {
                Some(wid) => {
                    sqlx::query_scalar::<_, i64>("SELECT is_nsfw FROM work WHERE id = ?")
                        .bind(wid)
                        .fetch_optional(&st.pool)
                        .await
                        .map_err(gql_err)?
                        .unwrap_or(0)
                        != 0
                }
                None => false,
            };
            if genre_is_nsfw(&m.genre) || matched_nsfw {
                continue;
            }
        }
        // Anti-bloat: persist a match always; a non-match only within the top-N.
        if matched_work.is_none() && idx >= FEDERATED_TOPN_NEW {
            continue;
        }
        match federated_ingest(st, &m.id.to_string()).await {
            Ok(res) => {
                persisted += 1;
                if seen_works.insert(res.work_id.clone()) {
                    work_order.push(res.work_id);
                }
            }
            Err(e) => tracing::warn!(manga_id = m.id, error = %e, "federated: persist failed"),
        }
    }

    // Build one FederatedSeries per consolidated work, in discovery order.
    let uid = user.as_ref().map(|u| u.id.as_str());
    let mut items = Vec::new();
    for wid in work_order {
        let Some(work) = catalog::load_canonical_work(&st.pool, &wid)
            .await
            .map_err(gql_err)?
        else {
            continue;
        };
        // NSFW works are hidden from an opted-out viewer (same gate as feeds).
        if work.is_nsfw_override.unwrap_or(work.is_nsfw) && !show_nsfw {
            continue;
        }
        let chapters = catalog::load_canonical_chapters(&st.pool, &wid)
            .await
            .map_err(gql_err)?;
        let series =
            map_canonical_series(&st.pool, uid, work, catalog::main_chapter_count_str(&chapters) as i32)
                .await;
        let translators = work_sources_to_translators(
            st,
            load_work_sources(&st.pool, &wid, show_nsfw).await?,
            &sources_by_id,
        );
        items.push(FederatedSeries {
            series,
            translators,
        });
    }

    // L2 (known limitation): `has_next_page` is a coarse OR across sources — true
    // if ANY queried source reported more results. Because each source paginates
    // independently and the results consolidate under canonical works, page N+1 can
    // re-yield works already seen on page N (from a source that had no more, while
    // another did). Treat it as "there may be more", not a clean cursor. A precise
    // cursor would need per-source page state carried across requests; deferred.
    Ok(FederatedSearchPage {
        items,
        page,
        has_next_page: any_has_next,
        sources_queried,
    })
}

/// Row loader for `user_activity`, mapped to the `Activity` GraphQL type.
#[derive(sqlx::FromRow)]
struct ActivityRow {
    id: String,
    kind: String,
    target_type: Option<String>,
    target_id: Option<String>,
    created_at: String,
}

impl From<ActivityRow> for Activity {
    fn from(r: ActivityRow) -> Self {
        Activity {
            id: ID(r.id),
            kind: r.kind,
            target_type: r.target_type,
            target_id: r.target_id.map(ID),
            created_at: r.created_at,
        }
    }
}

/// Row loader for `notifications` (joined to the actor user + source comment excerpt),
/// mapped to the `Notification` GraphQL type.
#[derive(sqlx::FromRow)]
struct NotificationRow {
    id: String,
    kind: String,
    count: Option<i64>,
    created_at: String,
    read_at: Option<String>,
    target_type: Option<String>,
    target_id: Option<String>,
    comment_id: Option<String>,
    actor_id: Option<String>,
    actor_username: Option<String>,
    actor_avatar: Option<String>,
    comment_excerpt: Option<String>,
}

impl From<NotificationRow> for Notification {
    fn from(r: NotificationRow) -> Self {
        Notification {
            id: ID(r.id),
            kind: r.kind,
            actor: r.actor_id.map(|id| UserRef {
                id: ID(id),
                username: r.actor_username.unwrap_or_default(),
                avatar_url: r.actor_avatar,
            }),
            comment_id: r.comment_id.map(ID),
            comment_excerpt: r.comment_excerpt,
            target_type: r.target_type,
            target_id: r.target_id.map(ID),
            count: r.count.map(|n| n as i32),
            created_at: r.created_at,
            read: r.read_at.is_some(),
        }
    }
}

/// Reload a series (canonical `w_` work or numeric Suwayomi id) as a `Series`
/// whose per-viewer fields (`isMarked` / `libraryStatus` / `isFavorite`) resolve
/// against `user_id`. Shared by the library mutations to echo the updated state.
async fn reload_series(st: &AppState, user_id: &str, series_id: &str) -> Result<Series> {
    if series_id.starts_with("w_") {
        let work = catalog::load_canonical_work(&st.pool, series_id)
            .await
            .map_err(gql_err)?
            .ok_or_else(|| Error::new("No such work"))?;
        let chapters = catalog::load_canonical_chapters(&st.pool, series_id)
            .await
            .map_err(gql_err)?;
        return Ok(map_canonical_series(
            &st.pool,
            Some(user_id),
            work,
            catalog::main_chapter_count_str(&chapters) as i32,
        )
        .await);
    }
    let n = series_id.parse::<i64>().map_err(gql_err)?;
    // DB cache first, live source only on a miss — so filing a shelf / favouriting
    // a numeric series works even when the upstream source (Suwayomi) is offline
    // (the write has already landed; this only echoes the series back).
    let m = match crate::series_cache::get_series(&st.pool, n)
        .await
        .map_err(gql_err)?
    {
        Some(m) => m,
        None => st.suwayomi.series(n).await.map_err(gql_err)?,
    };
    Ok(map_series(st, m).await)
}

// ---- Mutation --------------------------------------------------------------

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn mark(&self, ctx: &Context<'_>, series_id: ID, marked: bool) -> Result<Series> {
        let st = state(ctx);
        // "Add to library" is a PER-USER action — it writes the viewer's own
        // `user_library`, not Suwayomi's shared in-library flag. Requires a session
        // (an anonymous visitor has no library to add to). Works for both numeric
        // Suwayomi ids and `w_` canonical ids.
        let user = require_user(ctx).await?;
        // Validate the id shape BEFORE writing, so a malformed id (neither a `w_`
        // canonical id nor a numeric Suwayomi id) can't persist an orphan
        // `user_library` row that later reads skip silently.
        if !series_id.0.starts_with("w_") && series_id.0.parse::<i64>().is_err() {
            return Err(Error::new("Invalid series id"));
        }
        if marked {
            let now = Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO user_library (user_id, series_id, created_at) VALUES (?, ?, ?) \
                 ON CONFLICT(user_id, series_id) DO NOTHING",
            )
            .bind(&user.id)
            .bind(&series_id.0)
            .bind(&now)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
            log_activity(
                &st.pool,
                &user.id,
                "library_add",
                Some("series"),
                Some(&series_id.0),
            )
            .await;
        } else {
            sqlx::query("DELETE FROM user_library WHERE user_id = ? AND series_id = ?")
                .bind(&user.id)
                .bind(&series_id.0)
                .execute(&st.pool)
                .await
                .map_err(gql_err)?;
        }
        // Return the series with the (now-updated) per-viewer `isMarked` resolved by
        // the ComplexObject impl. Canonical `w_` works load from the mirror; numeric
        // ids load the source series.
        if series_id.0.starts_with("w_") {
            let work = catalog::load_canonical_work(&st.pool, &series_id.0)
                .await
                .map_err(gql_err)?
                .ok_or_else(|| Error::new("No such work"))?;
            let chapters = catalog::load_canonical_chapters(&st.pool, &series_id.0)
                .await
                .map_err(gql_err)?;
            return Ok(map_canonical_series(
                &st.pool,
                Some(user.id.as_str()),
                work,
                catalog::main_chapter_count_str(&chapters) as i32,
            )
            .await);
        }
        let n = series_id.0.parse::<i64>().map_err(gql_err)?;
        let m = st.suwayomi.series(n).await.map_err(gql_err)?;
        Ok(map_series(st, m).await)
    }

    /// File a series under an explicit shelf for the viewer ('reading' |
    /// 'completed' | 'onhold' | 'plan'), or clear it (`status: null`) to fall back
    /// to progress-derived shelving. Adds the series to the viewer's library if it
    /// isn't already there — filing a shelf implies membership. Per-user.
    async fn set_library_status(
        &self,
        ctx: &Context<'_>,
        series_id: ID,
        status: Option<String>,
    ) -> Result<Series> {
        let st = state(ctx);
        let user = require_user(ctx).await?;
        if let Some(s) = status.as_deref() {
            if !matches!(s, "reading" | "completed" | "onhold" | "plan") {
                return Err(Error::new(
                    "status must be one of: reading, completed, onhold, plan",
                ));
            }
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO user_library (user_id, series_id, created_at, status) VALUES (?, ?, ?, ?) \
             ON CONFLICT(user_id, series_id) DO UPDATE SET status = excluded.status",
        )
        .bind(&user.id)
        .bind(&series_id.0)
        .bind(&now)
        .bind(&status)
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;
        reload_series(st, &user.id, &series_id.0).await
    }

    /// Toggle whether the viewer has favourited a series. Adds it to the library if
    /// not already present — favouriting implies membership. Per-user.
    async fn set_favorite(&self, ctx: &Context<'_>, series_id: ID, favorite: bool) -> Result<Series> {
        let st = state(ctx);
        let user = require_user(ctx).await?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO user_library (user_id, series_id, created_at, is_favorite) VALUES (?, ?, ?, ?) \
             ON CONFLICT(user_id, series_id) DO UPDATE SET is_favorite = excluded.is_favorite",
        )
        .bind(&user.id)
        .bind(&series_id.0)
        .bind(&now)
        .bind(favorite as i64)
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;
        reload_series(st, &user.id, &series_id.0).await
    }

    /// Record one view (a chapter open) for a series — the popularity signal behind
    /// Trending and the series-page view counts. Intentionally NO auth: every reader,
    /// signed-in or anonymous, counts (the product decision is to count everyone). The
    /// reader fires this once per chapter open. Best-effort: a counter write must never
    /// fail the read, so an error is logged and swallowed, and the mutation still
    /// returns `true`.
    async fn record_view(&self, ctx: &Context<'_>, series_id: ID) -> Result<bool> {
        let st = state(ctx);
        if let Err(e) = crate::views::record(&st.pool, &series_id.0).await {
            tracing::warn!(series_id = %series_id.0, error = %e, "recordView failed");
        }
        Ok(true)
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
        // Canonical chapter ids are MangaDex uuids (not all-digits); persist their
        // progress in `canonical_progress`. Numeric Suwayomi ids fall through (CR6).
        let is_numeric =
            !chapter_id.0.is_empty() && chapter_id.0.bytes().all(|b| b.is_ascii_digit());
        if !is_numeric {
            let user = require_user(ctx).await?;
            // The owning work, for per-series aggregation. If the chapter isn't in the
            // mirror, store with work_id = '' rather than erroring — it's per-user private.
            let work_id: String = sqlx::query_scalar(
                "SELECT ss.work_id FROM chapter c JOIN source_series ss ON ss.id = c.source_series_id \
                 WHERE c.external_id = ? AND ss.source_type = 'mangadex' LIMIT 1",
            )
            .bind(&chapter_id.0)
            .fetch_optional(&st.pool)
            .await
            .map_err(gql_err)?
            .unwrap_or_default();
            let now = Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO canonical_progress \
                   (user_id, chapter_id, work_id, last_page_read, read, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(user_id, chapter_id) DO UPDATE SET \
                   last_page_read = excluded.last_page_read, read = excluded.read, \
                   work_id = excluded.work_id, updated_at = excluded.updated_at",
            )
            .bind(&user.id)
            .bind(&chapter_id.0)
            .bind(&work_id)
            .bind(last_page_read as i64)
            .bind(read)
            .bind(&now)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
            return Ok(true);
        }
        // Numeric Suwayomi chapter: per-user progress in `suwayomi_progress` (CR6).
        // Suwayomi is a content source only now — we no longer push read state to it.
        let n = chapter_id.0.parse::<i64>().map_err(gql_err)?;
        let user = require_user(ctx).await?;
        // The owning series id, for per-series aggregation in `libraryProgress`. If the
        // chapter isn't cached yet, store with series_id = '' rather than erroring —
        // the row is per-user private and the read state still round-trips.
        let series_id: Option<i64> =
            sqlx::query_scalar("SELECT manga_id FROM suwayomi_chapter WHERE id = ? LIMIT 1")
                .bind(n)
                .fetch_optional(&st.pool)
                .await
                .map_err(gql_err)?;
        let series_id = series_id.map(|s| s.to_string()).unwrap_or_default();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO suwayomi_progress \
               (user_id, chapter_id, series_id, last_page_read, read, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(user_id, chapter_id) DO UPDATE SET \
               last_page_read = excluded.last_page_read, read = excluded.read, \
               series_id = excluded.series_id, updated_at = excluded.updated_at",
        )
        .bind(&user.id)
        .bind(chapter_id.0.clone())
        .bind(&series_id)
        .bind(last_page_read as i64)
        .bind(read)
        .bind(&now)
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;
        Ok(true)
    }

    async fn post_review(&self, ctx: &Context<'_>, input: PostReviewInput) -> Result<Review> {
        if !(1..=10).contains(&input.score) {
            return Err(Error::new("score must be between 1 and 10"));
        }
        // Cap the written body (mirrors the bio cap in `update_profile`) so a client
        // can't inflate the DB with multi-megabyte reviews.
        if input.body.trim().chars().count() > 4000 {
            return Err(Error::new("review must be at most 4000 characters"));
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
        log_activity(
            &st.pool,
            &user.id,
            "review",
            Some("series"),
            Some(&input.series_id.0),
        )
        .await;
        Ok(row.into())
    }

    async fn post_comment(&self, ctx: &Context<'_>, input: PostCommentInput) -> Result<Comment> {
        let target_type = validate_comment_target(&input.target_type)?;
        let body = input.body.trim().to_string();
        // A comment must carry text OR an image (an image-only comment is fine).
        if body.is_empty() && input.media_id.is_none() {
            return Err(Error::new("comment must have text or an image"));
        }
        // Cap the body (mirrors the bio cap in `update_profile`) to bound per-row
        // size and keep recursive thread reads cheap.
        if body.chars().count() > 4000 {
            return Err(Error::new("comment must be at most 4000 characters"));
        }
        let user = require_user(ctx).await?;
        let st = state(ctx);

        // A reply must point at an existing comment on the SAME target — this keeps
        // a thread's tree self-consistent and blocks cross-thread / cross-series
        // parent ids. The root of the tree is a NULL parent.
        if let Some(parent) = input.parent_id.as_ref() {
            let ok: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM comments WHERE id = ? AND target_type = ? AND target_id = ?",
            )
            .bind(&parent.0)
            .bind(target_type)
            .bind(input.target_id.0.clone())
            .fetch_optional(&st.pool)
            .await
            .map_err(gql_err)?;
            if ok.is_none() {
                return Err(Error::new("reply target not found on this thread"));
            }
        }

        let now = Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        let parent_id = input.parent_id.as_ref().map(|p| p.0.clone());

        // Insert the comment and (optionally) claim the staged image in one
        // transaction, so a comment never ends up referencing media it failed to
        // link (or media stays orphaned after the comment committed).
        let media: Option<(i64, i64)> = {
            let mut tx = st.pool.begin().await.map_err(gql_err)?;
            sqlx::query(
                "INSERT INTO comments \
                   (id, target_type, target_id, parent_id, user_id, body, has_spoiler, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(target_type)
            .bind(input.target_id.0.clone())
            .bind(&parent_id)
            .bind(&user.id)
            .bind(&body)
            .bind(input.has_spoiler)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(gql_err)?;

            let dims = if let Some(media_id) = input.media_id.as_ref() {
                // Claim the caller's own, not-yet-linked upload. RETURNING gives us
                // the stored dimensions for the response in the same statement.
                let row: Option<(i64, i64)> = sqlx::query_as(
                    "UPDATE comment_media SET comment_id = ? \
                     WHERE id = ? AND user_id = ? AND comment_id IS NULL \
                     RETURNING width, height",
                )
                .bind(&id)
                .bind(&media_id.0)
                .bind(&user.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(gql_err)?;
                if row.is_none() {
                    // Bad/foreign/already-used media id: abort so the comment isn't
                    // committed with a phantom attachment.
                    return Err(Error::new("attached image not found"));
                }
                row
            } else {
                None
            };
            tx.commit().await.map_err(gql_err)?;
            dims
        };

        log_activity(
            &st.pool,
            &user.id,
            "comment",
            Some(target_type),
            Some(&input.target_id.0),
        )
        .await;
        // Notify the parent comment's author that someone replied — one notification
        // per reply, never to yourself. Fire-and-forget (must not fail the post).
        if let Some(pid) = parent_id.as_ref() {
            let parent_author: Option<String> =
                sqlx::query_scalar("SELECT user_id FROM comments WHERE id = ?")
                    .bind(pid)
                    .fetch_optional(&st.pool)
                    .await
                    .ok()
                    .flatten();
            if let Some(pa) = parent_author {
                if pa != user.id {
                    crate::notify::record(
                        &st.pool,
                        &pa,
                        "reply",
                        Some(&user.id),
                        Some(pid),
                        Some(target_type),
                        Some(&input.target_id.0),
                        None,
                    )
                    .await;
                }
            }
        }
        let media_url = input
            .media_id
            .as_ref()
            .map(|m| crate::media::comment_media_url(&m.0));
        Ok(Comment {
            id: ID(id),
            target_type: target_type.to_string(),
            target_id: input.target_id,
            parent_id: input.parent_id,
            author: UserRef {
                id: ID(user.id),
                username: user.username,
                avatar_url: user.avatar_url,
            },
            body,
            has_spoiler: input.has_spoiler,
            media_url,
            media_width: media.map(|(w, _)| w as i32),
            media_height: media.map(|(_, h)| h as i32),
            created_at: now,
            likes: 0,
            dislikes: 0,
            my_vote: 0,
        })
    }

    /// Like (1), dislike (-1), or clear (0) the viewer's vote on a comment. One vote
    /// per (comment, viewer); re-voting replaces it. Returns the fresh tallies so the
    /// client can update that comment without refetching the thread. A like that pushes
    /// the comment's like count onto a milestone notifies its author (never yourself).
    async fn vote_comment(&self, ctx: &Context<'_>, comment_id: ID, value: i32) -> Result<CommentVote> {
        if !(-1..=1).contains(&value) {
            return Err(Error::new("value must be -1, 0, or 1"));
        }
        let user = require_user(ctx).await?;
        let st = state(ctx);
        // Resolve the comment's author + thread (for the notification and self-check).
        let row: Option<(String, String, String)> = sqlx::query_as(
            "SELECT user_id, target_type, target_id FROM comments WHERE id = ?",
        )
        .bind(&comment_id.0)
        .fetch_optional(&st.pool)
        .await
        .map_err(gql_err)?;
        let Some((author_id, target_type, target_id)) = row else {
            return Err(Error::new("comment not found"));
        };
        let prior_likes: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM comment_votes WHERE comment_id = ? AND value = 1")
                .bind(&comment_id.0)
                .fetch_one(&st.pool)
                .await
                .map_err(gql_err)?;

        if value == 0 {
            sqlx::query("DELETE FROM comment_votes WHERE comment_id = ? AND user_id = ?")
                .bind(&comment_id.0)
                .bind(&user.id)
                .execute(&st.pool)
                .await
                .map_err(gql_err)?;
        } else {
            sqlx::query(
                "INSERT INTO comment_votes (comment_id, user_id, value, created_at) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT(comment_id, user_id) DO UPDATE SET \
                   value = excluded.value, created_at = excluded.created_at",
            )
            .bind(&comment_id.0)
            .bind(&user.id)
            .bind(value)
            .bind(Utc::now().to_rfc3339())
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
        }

        let (likes, dislikes): (i64, i64) = sqlx::query_as(
            "SELECT \
               COALESCE(SUM(CASE WHEN value = 1 THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN value = -1 THEN 1 ELSE 0 END), 0) \
             FROM comment_votes WHERE comment_id = ?",
        )
        .bind(&comment_id.0)
        .fetch_one(&st.pool)
        .await
        .map_err(gql_err)?;

        // Milestone notification: only when a like just raised the count ONTO a
        // milestone, for someone else's comment, and not already sent for that count.
        if value == 1
            && likes > prior_likes
            && crate::notify::is_like_milestone(likes)
            && author_id != user.id
            && !crate::notify::like_milestone_exists(&st.pool, &comment_id.0, likes).await
        {
            crate::notify::record(
                &st.pool,
                &author_id,
                "like_milestone",
                None,
                Some(&comment_id.0),
                Some(&target_type),
                Some(&target_id),
                Some(likes),
            )
            .await;
        }

        Ok(CommentVote {
            likes: likes as i32,
            dislikes: dislikes as i32,
            my_vote: value,
        })
    }

    /// Mark the viewer's notifications read. Pass a list of ids to mark just those, or
    /// omit/empty to mark ALL unread read (the "mark all read" action). Returns how many
    /// rows this transition changed. Scoped to the viewer — you can only read your own.
    async fn mark_notifications_read(&self, ctx: &Context<'_>, ids: Option<Vec<ID>>) -> Result<i32> {
        let user = require_user(ctx).await?;
        let st = state(ctx);
        let now = Utc::now().to_rfc3339();
        let affected = match ids {
            Some(ids) if !ids.is_empty() => {
                let mut n: u64 = 0;
                let mut tx = st.pool.begin().await.map_err(gql_err)?;
                for id in ids {
                    let r = sqlx::query(
                        "UPDATE notifications SET read_at = ? \
                         WHERE id = ? AND user_id = ? AND read_at IS NULL",
                    )
                    .bind(&now)
                    .bind(&id.0)
                    .bind(&user.id)
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
                    n += r.rows_affected();
                }
                tx.commit().await.map_err(gql_err)?;
                n
            }
            _ => sqlx::query(
                "UPDATE notifications SET read_at = ? WHERE user_id = ? AND read_at IS NULL",
            )
            .bind(&now)
            .bind(&user.id)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?
            .rows_affected(),
        };
        Ok(affected as i32)
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
        // Reject over-long passwords before any Argon2 work (A7). Such a password
        // can never be valid (registration caps at the same length), so this is
        // just the wrong-credentials path.
        if password.len() > MAX_PASSWORD_LEN {
            st.auth_limiter.record(&key);
            return Err(Error::new("Invalid username or password"));
        }
        let row = sqlx::query_as::<_, User>(
            "SELECT id, username, email, password_hash, avatar_url, is_admin, is_banned FROM users WHERE username = ?",
        )
        .bind(&username)
        .fetch_optional(&st.pool)
        .await
        .map_err(gql_err)?;
        // Always run an Argon2 verify — against the real hash if the user exists,
        // else a fixed dummy hash — so login time doesn't reveal whether the
        // username exists (A3).
        let password_ok = match &row {
            Some(u) => auth::verify_password(&password, &u.password_hash),
            None => {
                auth::verify_password(&password, &DUMMY_PASSWORD_HASH);
                false
            }
        };
        let user = match row {
            Some(u) if password_ok => u,
            _ => {
                st.auth_limiter.record(&key); // count only failed attempts
                return Err(Error::new("Invalid username or password"));
            }
        };
        // A suspended account can't sign in even with the right password. This
        // is not a failed credential attempt, so it doesn't consume the budget.
        if user.is_banned != 0 {
            return Err(Error::new("This account has been suspended."));
        }
        let tok = new_session(&st.pool, &user.id, st.session_ttl_secs).await?;
        let show_nsfw = user_show_nsfw(&st.pool, &user.id).await;
        Ok(Session {
            token: tok,
            user: build_session_user(&st.pool, &user, show_nsfw).await,
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
        // Count glyphs, not bytes, so a multibyte username can't slip under the
        // minimum; also cap the maximum and restrict the charset (letters, digits,
        // and `_ - .`) to keep control chars / whitespace / homoglyph tricks out.
        let uname_len = username.chars().count();
        if uname_len < 3 {
            return Err(Error::new("username must be at least 3 characters"));
        }
        if uname_len > 32 {
            return Err(Error::new("username must be at most 32 characters"));
        }
        if !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        {
            return Err(Error::new(
                "username may only contain letters, digits, and _ - .",
            ));
        }
        if email.chars().count() > 254 {
            return Err(Error::new("a valid email is required"));
        }
        if !email.contains('@') {
            return Err(Error::new("a valid email is required"));
        }
        if input.password.len() < 8 {
            return Err(Error::new("password must be at least 8 characters"));
        }
        if input.password.len() > MAX_PASSWORD_LEN {
            return Err(Error::new("password must be at most 1024 characters"));
        }
        // Admin usernames are reserved: open registration must never grant admin
        // (A5). Admin accounts are provisioned/promoted at startup from
        // KOMIKA_ADMIN_USERS + KOMIKA_ADMIN_PASSWORD (see provision_admins), so a
        // stranger cannot squat a configured admin name to self-elevate.
        if st
            .admin_users
            .iter()
            .any(|u| u.eq_ignore_ascii_case(username))
        {
            return Err(Error::new("This username is reserved."));
        }
        // Case-insensitive uniqueness pre-check (L): the DB `UNIQUE` is byte-exact,
        // so without this `alvee` and `Alvee` register as distinct accounts and one
        // can impersonate the other. (Residual race: two concurrent registrations of
        // case-variant names can still both pass this check; the byte-exact UNIQUE
        // only stops exact-duplicate inserts. A COLLATE NOCASE unique index would
        // close it fully but needs a migration owned elsewhere.)
        let taken: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM users WHERE username = ? COLLATE NOCASE")
                .bind(username)
                .fetch_optional(&st.pool)
                .await
                .map_err(gql_err)?;
        if taken.is_some() {
            return Err(Error::new("username or email already taken"));
        }
        let hash = auth::hash_password(&input.password).map_err(gql_err)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let is_admin = false;
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
        let tok = new_session(&st.pool, &id, st.session_ttl_secs).await?;
        Ok(Session {
            token: tok,
            user: SessionUser {
                id: ID(id),
                username: username.to_string(),
                display_name: None,
                bio: None,
                avatar_url: None,
                is_admin,
                show_nsfw: false, // fresh accounts default to hiding NSFW
                joined_at: now,
            },
        })
    }

    async fn logout(&self, ctx: &Context<'_>) -> Result<bool> {
        let st = state(ctx);
        if let Some(tok) = token(ctx) {
            // Stored column is sha256(token) — hash the presented token to match.
            sqlx::query("DELETE FROM sessions WHERE token = ?")
                .bind(auth::hash_token(&tok))
                .execute(&st.pool)
                .await
                .map_err(gql_err)?;
        }
        Ok(true)
    }

    /// Update the signed-in user's editable profile (display name + bio).
    /// A blank/`null` field clears that value (display falls back to username).
    /// Returns the refreshed `SessionUser`.
    async fn update_profile(
        &self,
        ctx: &Context<'_>,
        input: UpdateProfileInput,
    ) -> Result<SessionUser> {
        let user = require_user(ctx).await?;
        // Trim, then treat an empty string as "clear" (store NULL).
        let display_name = input
            .display_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let bio = input
            .bio
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(name) = &display_name {
            if name.chars().count() > 50 {
                return Err(Error::new("display name must be at most 50 characters"));
            }
        }
        if let Some(b) = &bio {
            if b.chars().count() > 500 {
                return Err(Error::new("bio must be at most 500 characters"));
            }
        }
        let st = state(ctx);
        sqlx::query("UPDATE users SET display_name = ?, bio = ? WHERE id = ?")
            .bind(&display_name)
            .bind(&bio)
            .bind(&user.id)
            .execute(&st.pool)
            .await
            .map_err(gql_err)?;
        let show_nsfw = user_show_nsfw(&st.pool, &user.id).await;
        Ok(build_session_user(&st.pool, &user, show_nsfw).await)
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

    /// Admin: pause or unpause one series' scanning — the targeted toggle
    /// (`updateSeriesAdmin` is whole-state and would clobber the other overrides).
    /// Sets the forced `paused_override` (winning over the auto-by-status pause);
    /// unpausing also triggers an immediate re-scan so the chapter list and count
    /// refresh at once instead of waiting for the next cadence. Returns the
    /// recomputed series. To CLEAR the override (back to auto-by-status), use
    /// `updateSeriesAdmin` with `paused: null`.
    async fn set_series_paused(
        &self,
        ctx: &Context<'_>,
        series_id: ID,
        paused: bool,
    ) -> Result<Series> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let n = series_id.0.parse::<i64>().map_err(gql_err)?;
        // Resolve the series first so a bogus id fails before any write.
        let m = st.suwayomi.series(n).await.map_err(gql_err)?;
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO series_admin (series_id, paused_override, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT(series_id) DO UPDATE SET \
               paused_override = excluded.paused_override, \
               updated_at = excluded.updated_at",
        )
        .bind(&series_id.0)
        .bind(paused as i64)
        .bind(now.to_rfc3339())
        .execute(&st.pool)
        .await
        .map_err(gql_err)?;
        if !paused {
            scan_series(st, &m, now).await.map_err(gql_err)?;
        }
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

    /// Admin series-detail editor: edit a canonical work's user-facing metadata as an
    /// override layer — the source-derived fields stay immutable and these overrides
    /// win at read time (like `series_admin` for scan/status). Each field is
    /// three-valued: OMITTED => leave unchanged; null => clear the override; a value =>
    /// set it. `tags` is a whole-list replace of the curated set. Returns the
    /// recomputed series (in the same id shape the caller passed).
    async fn update_series_metadata(
        &self,
        ctx: &Context<'_>,
        input: SeriesMetadataInput,
    ) -> Result<Series> {
        use async_graphql::MaybeUndefined::{Null, Undefined, Value};
        require_admin(ctx).await?;
        let st = state(ctx);
        let work_id = resolve_work_id(&st.pool, &input.series_id.0)
            .await
            .ok_or_else(|| {
                Error::new(
                    "Series is not catalogued — add it to the catalogue before editing its metadata.",
                )
            })?;
        let now = Utc::now().to_rfc3339();

        // All override writes in ONE transaction so a partial failure can't leave a
        // half-applied edit. Each singular column follows the three-valued input:
        // Undefined => leave; Null => clear (NULL); Value => set.
        let mut tx = st.pool.begin().await.map_err(gql_err)?;
        match &input.title {
            Undefined => {}
            Null | Value(_) => {
                let v = match &input.title {
                    Value(v) => Some(v.as_str()),
                    _ => None,
                };
                sqlx::query("UPDATE work SET title_override = ?, updated_at = ? WHERE id = ?")
                    .bind(v)
                    .bind(&now)
                    .bind(&work_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
            }
        }
        match &input.description {
            Undefined => {}
            Null | Value(_) => {
                let v = match &input.description {
                    Value(v) => Some(v.as_str()),
                    _ => None,
                };
                sqlx::query("UPDATE work SET description_override = ?, updated_at = ? WHERE id = ?")
                    .bind(v)
                    .bind(&now)
                    .bind(&work_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
            }
        }
        match input.r#type {
            Undefined => {}
            Null | Value(_) => {
                let word = match input.r#type {
                    Value(t) => Some(content_type_word(t)),
                    _ => None,
                };
                sqlx::query("UPDATE work SET content_type_override = ?, updated_at = ? WHERE id = ?")
                    .bind(word)
                    .bind(&now)
                    .bind(&work_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
            }
        }
        match input.is_nsfw {
            Undefined => {}
            Null | Value(_) => {
                let val = match input.is_nsfw {
                    Value(b) => Some(b as i64),
                    _ => None,
                };
                sqlx::query("UPDATE work SET is_nsfw_override = ?, updated_at = ? WHERE id = ?")
                    .bind(val)
                    .bind(&now)
                    .bind(&work_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
            }
        }
        // Tags: `tags: []` and `tags: null` both leave zero curated rows → the work
        // reverts to source-derived genres (curated-empty is intentionally not
        // expressible; the console only sends `tags` when the admin edits them).
        match &input.tags {
            Undefined => {}
            Null | Value(_) => {
                sqlx::query("DELETE FROM work_tag WHERE work_id = ?")
                    .bind(&work_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
                if let Value(list) = &input.tags {
                    for (i, tag) in list.iter().enumerate() {
                        let t = tag.trim();
                        if t.is_empty() {
                            continue;
                        }
                        sqlx::query(
                            "INSERT OR IGNORE INTO work_tag (work_id, tag, ord) VALUES (?, ?, ?)",
                        )
                        .bind(&work_id)
                        .bind(t)
                        .bind(i as i64)
                        .execute(&mut *tx)
                        .await
                        .map_err(gql_err)?;
                    }
                }
            }
        }
        tx.commit().await.map_err(gql_err)?;

        // Recompute the series in the caller's id shape so the console updates in place.
        if input.series_id.0.starts_with("w_") {
            let work = catalog::load_canonical_work(&st.pool, &work_id)
                .await
                .map_err(gql_err)?
                .ok_or_else(|| Error::new("No such work"))?;
            let chapters = catalog::load_canonical_chapters(&st.pool, &work_id)
                .await
                .map_err(gql_err)?;
            let user = current_user(ctx).await;
            Ok(map_canonical_series(
                &st.pool,
                user.as_ref().map(|u| u.id.as_str()),
                work,
                catalog::main_chapter_count_str(&chapters) as i32,
            )
            .await)
        } else {
            let n = input.series_id.0.parse::<i64>().map_err(gql_err)?;
            let m = resolve_series_cached(st, n).await.map_err(gql_err)?;
            Ok(map_series(st, m).await)
        }
    }

    /// Admin: force an immediate re-scan of every installed Suwayomi source of a
    /// canonical work (each source's `source_key` is a Suwayomi manga id). Returns how
    /// many sources were successfully scanned; unresolvable sources are skipped.
    async fn rescan_work(&self, ctx: &Context<'_>, work_id: ID) -> Result<i32> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT source_key FROM source_series \
             WHERE work_id = ? AND source_type = 'suwayomi'",
        )
        .bind(&work_id.0)
        .fetch_all(&st.pool)
        .await
        .map_err(gql_err)?;
        let now = Utc::now();
        let mut scanned = 0;
        for key in keys {
            let Ok(n) = key.parse::<i64>() else { continue };
            match st.suwayomi.series(n).await {
                Ok(m) => {
                    if scan_series(st, &m, now).await.is_ok() {
                        scanned += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(source_key = key, error = %e, "rescanWork: skipping unresolvable source")
                }
            }
        }
        Ok(scanned)
    }

    /// Admin series-detail editor: soft-hide (reversible) or rename one chapter of a
    /// work by aggregate number. Non-destructive — the cached chapter rows are
    /// untouched, so a re-scan can't resurrect a hidden chapter. Clearing both fields
    /// removes the override row entirely.
    async fn set_chapter_override(
        &self,
        ctx: &Context<'_>,
        input: ChapterOverrideInput,
    ) -> Result<bool> {
        use async_graphql::MaybeUndefined::{Null, Undefined, Value};
        require_admin(ctx).await?;
        let st = state(ctx);
        let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM work WHERE id = ?")
            .bind(&input.work_id.0)
            .fetch_optional(&st.pool)
            .await
            .map_err(gql_err)?;
        if exists.is_none() {
            return Err(Error::new("No such work"));
        }
        // Read-modify-write under ONE transaction so two concurrent partial edits (one
        // toggling `hidden`, one renaming) can't clobber each other's field.
        let mut tx = st.pool.begin().await.map_err(gql_err)?;
        let existing = sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT hidden, title_override FROM chapter_override WHERE work_id = ? AND chapter_key = ?",
        )
        .bind(&input.work_id.0)
        .bind(&input.chapter_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(gql_err)?;
        let (mut hidden, mut title) = existing.map(|(h, t)| (h != 0, t)).unwrap_or((false, None));
        match input.hidden {
            Undefined => {}
            Null => hidden = false,
            Value(b) => hidden = b,
        }
        match input.title {
            Undefined => {}
            Null => title = None,
            Value(v) => {
                let v = v.trim().to_string();
                title = if v.is_empty() { None } else { Some(v) };
            }
        }
        // A no-op override (visible + no rename) is stored as the absence of a row.
        if !hidden && title.is_none() {
            sqlx::query("DELETE FROM chapter_override WHERE work_id = ? AND chapter_key = ?")
                .bind(&input.work_id.0)
                .bind(&input.chapter_key)
                .execute(&mut *tx)
                .await
                .map_err(gql_err)?;
            tx.commit().await.map_err(gql_err)?;
            return Ok(true);
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO chapter_override (work_id, chapter_key, hidden, title_override, updated_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(work_id, chapter_key) DO UPDATE SET \
               hidden = excluded.hidden, \
               title_override = excluded.title_override, \
               updated_at = excluded.updated_at",
        )
        .bind(&input.work_id.0)
        .bind(&input.chapter_key)
        .bind(hidden as i64)
        .bind(&title)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(gql_err)?;
        tx.commit().await.map_err(gql_err)?;
        Ok(true)
    }

    /// Admin moderation: suspend or restore a user account. A banned user can't
    /// sign in and their active sessions are revoked immediately. Admins can't
    /// ban themselves or another admin.
    async fn ban_user(&self, ctx: &Context<'_>, user_id: ID, banned: bool) -> Result<AdminUser> {
        let admin = require_admin(ctx).await?;
        let st = state(ctx);
        if user_id.0 == admin.id {
            return Err(Error::new("You cannot ban your own account."));
        }
        let target: Option<(String, String, String, Option<String>, i64, String)> = sqlx::query_as(
            "SELECT id, username, email, avatar_url, is_admin, created_at FROM users WHERE id = ?",
        )
        .bind(&user_id.0)
        .fetch_optional(&st.pool)
        .await
        .map_err(gql_err)?;
        let Some((id, username, email, avatar_url, is_admin, created_at)) = target else {
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
        Ok(AdminUser {
            id: ID(id),
            username,
            email,
            avatar_url,
            is_admin: is_admin != 0,
            is_banned: banned,
            created_at,
        })
    }

    /// Admin moderation: delete a comment and its entire reply subtree. Returns
    /// false if it was already gone. (Authors don't self-delete here — this is the
    /// mod action.) The subtree and its attached images are removed explicitly
    /// (via a recursive CTE) rather than relying on `ON DELETE CASCADE`, so it
    /// behaves the same on connections without the foreign_keys pragma.
    async fn delete_comment(&self, ctx: &Context<'_>, comment_id: ID) -> Result<bool> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let mut tx = st.pool.begin().await.map_err(gql_err)?;
        // Drop the attached images of the whole subtree first (FK-safe ordering).
        sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM comments WHERE id = ? \
                 UNION ALL \
                 SELECT c.id FROM comments c JOIN subtree s ON c.parent_id = s.id \
             ) \
             DELETE FROM comment_media WHERE comment_id IN (SELECT id FROM subtree)",
        )
        .bind(&comment_id.0)
        .execute(&mut *tx)
        .await
        .map_err(gql_err)?;
        let res = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM comments WHERE id = ? \
                 UNION ALL \
                 SELECT c.id FROM comments c JOIN subtree s ON c.parent_id = s.id \
             ) \
             DELETE FROM comments WHERE id IN (SELECT id FROM subtree)",
        )
        .bind(&comment_id.0)
        .execute(&mut *tx)
        .await
        .map_err(gql_err)?;
        tx.commit().await.map_err(gql_err)?;
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

        // DD1: compute the cover pHash server-side (strongest cheap dedup signal).
        // Best-effort — `None` on any fetch/decode failure just drops the signal.
        // Done here (the only async/network step) so the core is unit-testable
        // without a live Suwayomi.
        let cover_phash = match st.suwayomi.cover_bytes(m.thumbnail_url.as_deref()).await {
            Some(bytes) => crate::phash::dhash(&bytes),
            None => None,
        };
        add_source_series_core(&st.pool, &m, cover_phash)
            .await
            .map_err(gql_err)
    }

    /// Admin (D1): fold one canonical work into another — for cleaning up two
    /// already-created duplicates (the `merge_candidate` review flow only handles a
    /// source-series-vs-provisional at add time, not two existing works). All of
    /// the source work's source-series mappings + user data (library, progress,
    /// reviews, comments) re-point to the target; the target keeps its identity and
    /// gains the source's aliases/external-ids (and cover pHash if it had none); the
    /// empty source work is deleted. Pick `target` as the canonical/enriched one
    /// (e.g. the MangaDex-anchored work).
    async fn merge_works(
        &self,
        ctx: &Context<'_>,
        source_work_id: ID,
        target_work_id: ID,
    ) -> Result<MergeWorksResult> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let outcome = catalog::merge_works(&st.pool, &source_work_id.0, &target_work_id.0)
            .await
            .map_err(gql_err)?;
        Ok(MergeWorksResult {
            target_work_id,
            moved_source_series: outcome.moved_source_series as i32,
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
        // Fast-path only — the correctness guard is the atomic claim below.
        if row.status != "pending" {
            return Err(Error::new("This merge candidate is already resolved."));
        }

        let mut tx = st.pool.begin().await.map_err(gql_err)?;

        // Atomically claim the candidate: only the admin whose UPDATE flips a
        // still-`pending` row proceeds. A concurrent resolver that already claimed
        // it leaves `rows_affected() == 0` here, so the loser never repoints.
        let now = Utc::now().to_rfc3339();
        let claim = sqlx::query(
            "UPDATE merge_candidate SET status = ?, resolved_at = ? \
             WHERE id = ? AND status = 'pending'",
        )
        .bind(if accept { "confirmed" } else { "rejected" })
        .bind(&now)
        .bind(&id.0)
        .execute(&mut *tx)
        .await
        .map_err(gql_err)?;
        if claim.rows_affected() == 0 {
            tx.rollback().await.map_err(gql_err)?;
            return Err(Error::new("This merge candidate is already resolved."));
        }

        if accept {
            let old_work: Option<String> =
                sqlx::query_scalar("SELECT work_id FROM source_series WHERE id = ?")
                    .bind(&row.source_series_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(gql_err)?;
            sqlx::query("UPDATE source_series SET work_id = ? WHERE id = ?")
                .bind(&row.candidate_work_id)
                .bind(&row.source_series_id)
                .execute(&mut *tx)
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
                    .execute(&mut *tx)
                    .await
                    .map_err(gql_err)?;
                }
            }
        }

        tx.commit().await.map_err(gql_err)?;
        Ok(true)
    }

    // ---- Sources & Extensions admin surface (EXT-1) --------------------------

    /// Admin: register an extension repo (store) on the Suwayomi engine by its
    /// index URL and refresh the available-extension list from every store.
    /// Idempotent upstream (re-adding an existing store is a no-op). Returns how
    /// many extensions are now known.
    async fn add_extension_repo(&self, ctx: &Context<'_>, index_url: String) -> Result<i32> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let url = index_url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(Error::new("indexUrl must be an http(s) URL"));
        }
        st.suwayomi
            .add_extension_store(url)
            .await
            .map_err(gql_err)?;
        let count = st.suwayomi.refresh_extensions().await.map_err(gql_err)?;
        Ok(count as i32)
    }

    /// Admin: install a store extension onto the Suwayomi engine. An NSFW
    /// extension is refused unless the admin opted in (show_nsfw posture — the
    /// listing hides it, and it can't be installed by pkgName either).
    async fn install_extension(
        &self,
        ctx: &Context<'_>,
        pkg_name: String,
    ) -> Result<ExtensionInfo> {
        let user = require_admin(ctx).await?;
        let st = state(ctx);
        if !user_show_nsfw(&st.pool, &user.id).await {
            let ext = st
                .suwayomi
                .get_extension(&pkg_name)
                .await
                .map_err(gql_err)?;
            if ext.is_nsfw {
                return Err(Error::new(
                    "This extension is NSFW — enable NSFW in your settings to install it",
                ));
            }
        }
        let e = st
            .suwayomi
            .install_extension(&pkg_name)
            .await
            .map_err(gql_err)?;
        Ok(map_extension_info(st, e))
    }

    /// Admin: uninstall an extension from the Suwayomi engine. No NSFW gate —
    /// removing content is always allowed.
    async fn uninstall_extension(
        &self,
        ctx: &Context<'_>,
        pkg_name: String,
    ) -> Result<ExtensionInfo> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let e = st
            .suwayomi
            .uninstall_extension(&pkg_name)
            .await
            .map_err(gql_err)?;
        Ok(map_extension_info(st, e))
    }

    /// Admin: update an installed extension to the store's latest version. Gated
    /// like `installExtension` (an update installs a new APK).
    async fn update_extension(&self, ctx: &Context<'_>, pkg_name: String) -> Result<ExtensionInfo> {
        let user = require_admin(ctx).await?;
        let st = state(ctx);
        if !user_show_nsfw(&st.pool, &user.id).await {
            let ext = st
                .suwayomi
                .get_extension(&pkg_name)
                .await
                .map_err(gql_err)?;
            if ext.is_nsfw {
                return Err(Error::new(
                    "This extension is NSFW — enable NSFW in your settings to update it",
                ));
            }
        }
        let e = st
            .suwayomi
            .update_extension(&pkg_name)
            .await
            .map_err(gql_err)?;
        Ok(map_extension_info(st, e))
    }

    /// Admin (S2/H1/F2): backfill MangaDex enrichment — all-language alt titles,
    /// localized descriptions, full author/artist credits, AND the full per-volume
    /// cover set (F2) — for works anchored to MangaDex not yet enriched. Selects on
    /// `metadata_synced_at IS NULL OR covers_synced_at IS NULL`, and marks both, so
    /// it advances past works legitimately lacking descriptions/covers instead of
    /// re-selecting them forever (drains, doesn't thrash). Metadata is fetched
    /// batched (100/req); covers are one `/cover` request per work, so the default
    /// `limit` is modest — call repeatedly until it returns 0.
    async fn backfill_mangadex_metadata(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 200)] limit: i32,
    ) -> Result<i32> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let limit = limit.clamp(1, 500) as i64;
        let ids = works_needing_enrichment(&st.pool, limit).await?;
        enrich_works(st, &ids).await
    }

    /// Admin (S1): "save everything to DB" — materialize the whole Suwayomi library
    /// into the DB cache so reader loads serve from SQLite. Series METADATA (title,
    /// cover reference, description, author/artist, genres, status, chapter count)
    /// is written synchronously from one `library()` call and returned immediately;
    /// the per-series CHAPTER LISTS are filled by a spawned background task (each is
    /// one source fetch, so a large library would otherwise block the request past
    /// client/proxy timeouts). PRODUCTION maintenance action gated ONLY by
    /// `require_admin` (admin identity is the gate — no separate feature flag).
    /// Returns how many series were persisted.
    async fn persist_catalogue(&self, ctx: &Context<'_>) -> Result<i32> {
        require_admin(ctx).await?;
        let st_arc = ctx.data_unchecked::<std::sync::Arc<AppState>>().clone();
        let library = st_arc.suwayomi.library().await.map_err(gql_err)?;
        let mut ids = Vec::with_capacity(library.len());
        let mut persisted = 0i32;
        for mut m in library {
            m.in_library = true;
            match crate::series_cache::put_series(&st_arc.pool, &m).await {
                Ok(()) => {
                    ids.push(m.id);
                    persisted += 1;
                }
                Err(e) => {
                    tracing::warn!(series_id = m.id, error = %e, "persistCatalogue: series write failed")
                }
            }
        }
        // Fill chapter lists in the background (best-effort, sequential + polite) so
        // the request returns immediately even for a large library.
        let st_bg = st_arc.clone();
        tokio::spawn(async move {
            for id in ids {
                match st_bg.suwayomi.chapters(id).await {
                    Ok(chapters) => {
                        let _ = crate::series_cache::put_chapters(&st_bg.pool, id, &chapters).await;
                    }
                    Err(e) => {
                        tracing::warn!(series_id = id, error = %e, "persistCatalogue(bg): chapter fetch failed")
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            tracing::info!("persistCatalogue: background chapter fill complete");
        });
        tracing::info!(
            persisted,
            "persistCatalogue: metadata materialized; chapters filling in background"
        );
        Ok(persisted)
    }

    /// Admin: start an "add all from this source" background ingest job (S1).
    /// Walks the source's POPULAR listing page by page and runs every entry
    /// through the Tier-2 dedup add flow; progress is persisted on the job row
    /// (poll `sourceIngestJobs`). Refused while a job is already running for the
    /// source, and NSFW sources are refused unless the admin opted in.
    async fn start_source_ingest(
        &self,
        ctx: &Context<'_>,
        source_id: ID,
    ) -> Result<SourceIngestJob> {
        let user = require_admin(ctx).await?;
        let st_arc = ctx.data_unchecked::<std::sync::Arc<AppState>>().clone();
        // Validates the source exists AND carries the NSFW posture gate.
        let (_, source_nsfw) = st_arc
            .suwayomi
            .source_meta(&source_id.0)
            .await
            .map_err(gql_err)?;
        if source_nsfw && !user_show_nsfw(&st_arc.pool, &user.id).await {
            return Err(Error::new(
                "This source is NSFW — enable NSFW in your settings to ingest it",
            ));
        }
        let Some(job) = crate::ingest::try_start_job(&st_arc.pool, &source_id.0)
            .await
            .map_err(gql_err)?
        else {
            return Err(Error::new(
                "An ingest job is already running for this source",
            ));
        };
        crate::ingest::spawn_runner(st_arc, job.id.clone(), source_id.0.clone());
        Ok(job.into())
    }

    /// Admin: request cancellation of a running ingest job. The runner observes
    /// it between items and stops; progress up to that point is preserved.
    /// Cancelling an already-finished job is a no-op returning the row as-is.
    async fn cancel_source_ingest(&self, ctx: &Context<'_>, job_id: ID) -> Result<SourceIngestJob> {
        require_admin(ctx).await?;
        let st = state(ctx);
        crate::ingest::cancel_job(&st.pool, &job_id.0)
            .await
            .map_err(gql_err)?
            .map(Into::into)
            .ok_or_else(|| Error::new("No such ingest job"))
    }

    /// Admin (F1): "add all from the whole EXTENSION" — start an ingest job for
    /// EVERY installed Suwayomi source belonging to `pkgName` in one action (an
    /// extension like MangaDex exposes ~60 per-language sources). NSFW sources are
    /// skipped for an opted-out admin; a source that already has a running job is
    /// skipped (its existing job is still returned so the UI can track them
    /// together), never erroring the whole call. Errors only when no source
    /// matches the package. Returns every started + already-running job.
    async fn start_extension_ingest(
        &self,
        ctx: &Context<'_>,
        pkg_name: ID,
    ) -> Result<Vec<SourceIngestJob>> {
        let user = require_admin(ctx).await?;
        let st_arc = ctx.data_unchecked::<std::sync::Arc<AppState>>().clone();
        let show_nsfw = user_show_nsfw(&st_arc.pool, &user.id).await;
        let sources = st_arc.suwayomi.list_sources().await.map_err(gql_err)?;
        let matching: Vec<_> = sources
            .into_iter()
            .filter(|s| s.pkg_name.as_deref() == Some(pkg_name.0.as_str()))
            .filter(|s| show_nsfw || !s.is_nsfw)
            .collect();
        if matching.is_empty() {
            return Err(Error::new(
                "No installed (visible) sources for this extension",
            ));
        }
        let mut jobs = Vec::new();
        for src in matching {
            match crate::ingest::try_start_job(&st_arc.pool, &src.id)
                .await
                .map_err(gql_err)?
            {
                Some(job) => {
                    crate::ingest::spawn_runner(st_arc.clone(), job.id.clone(), src.id.clone());
                    jobs.push(job.into());
                }
                // Already running for this source — surface its existing job.
                None => {
                    if let Some(existing) = crate::ingest::load_running_job(&st_arc.pool, &src.id)
                        .await
                        .map_err(gql_err)?
                    {
                        jobs.push(existing.into());
                    }
                }
            }
        }
        Ok(jobs)
    }

    /// Admin (F1): cancel every running ingest job for an extension's sources.
    /// Returns the cancelled job rows (empty if none were running).
    async fn cancel_extension_ingest(
        &self,
        ctx: &Context<'_>,
        pkg_name: ID,
    ) -> Result<Vec<SourceIngestJob>> {
        require_admin(ctx).await?;
        let st = state(ctx);
        let source_ids: Vec<String> = st
            .suwayomi
            .list_sources()
            .await
            .map_err(gql_err)?
            .into_iter()
            .filter(|s| s.pkg_name.as_deref() == Some(pkg_name.0.as_str()))
            .map(|s| s.id)
            .collect();
        let cancelled = crate::ingest::cancel_running_for_sources(&st.pool, &source_ids)
            .await
            .map_err(gql_err)?;
        Ok(cancelled.into_iter().map(Into::into).collect())
    }

    /// Admin bulk catalogue ingest (EXT-1): for each Suwayomi manga id, ensure
    /// the manga is in the Suwayomi library (so the adaptive scanner tracks it)
    /// and run the Tier-2 dedup add flow (`add_source_series` semantics). A
    /// failing id is recorded in its entry — it never aborts the batch.
    async fn bulk_add_source_series(
        &self,
        ctx: &Context<'_>,
        suwayomi_manga_ids: Vec<ID>,
    ) -> Result<BulkAddResult> {
        require_admin(ctx).await?;
        if suwayomi_manga_ids.is_empty() {
            return Err(Error::new("suwayomiMangaIds must not be empty"));
        }
        if suwayomi_manga_ids.len() > 100 {
            return Err(Error::new("At most 100 ids per bulkAddSourceSeries call"));
        }
        let st = state(ctx);
        let mut entries = Vec::with_capacity(suwayomi_manga_ids.len());
        for id in suwayomi_manga_ids {
            let entry = match ingest_source_series(st, &id.0).await {
                Ok(r) => BulkAddEntry {
                    suwayomi_manga_id: id,
                    result: Some(r),
                    error: None,
                },
                Err(e) => BulkAddEntry {
                    suwayomi_manga_id: id,
                    result: None,
                    error: Some(e.to_string()),
                },
            };
            entries.push(entry);
        }
        Ok(summarize_bulk(entries))
    }
}

/// One id of the bulk-ingest flow: resolve the Suwayomi manga, put it in the
/// Suwayomi library, compute the cover pHash (best-effort, DD1) and run the
/// shared Tier-2 dedup core. Mirrors `add_source_series` plus the library step.
pub(crate) async fn ingest_source_series(
    st: &AppState,
    raw_id: &str,
) -> anyhow::Result<MatchResult> {
    let mid: i64 = raw_id
        .parse()
        .map_err(|_| anyhow::anyhow!("suwayomiMangaId must be an integer id"))?;
    let mut m = st.suwayomi.series(mid).await?;
    st.suwayomi.set_in_library(mid, true).await?;
    m.in_library = true;
    // S1: cache the series METADATA so reader loads serve from the DB. Chapters are
    // cached by the scanner (which scans the library) and lazily on first read — NOT
    // fetched here, so a bulk ingest isn't slowed by a chapter fetch per item (S3).
    let _ = crate::series_cache::put_series(&st.pool, &m).await;
    let cover_phash = match st.suwayomi.cover_bytes(m.thumbnail_url.as_deref()).await {
        Some(bytes) => crate::phash::dhash(&bytes),
        None => None,
    };
    add_source_series_core(&st.pool, &m, cover_phash).await
}

/// Minimum normalized-title length for a federated silent consolidation (C2).
/// Below this, a common short title (e.g. "hero", "love") could collide across
/// unrelated series, so even a corroborated match falls back to cautious review.
const FEDERATED_MIN_TITLE_CHARS: usize = 5;

/// C2 guard: a federated direct-link (silent, un-mergeable consolidation) is only
/// safe when a mid-confidence (`Decision::Review`) match is CORROBORATED beyond the
/// bare title. A title-only exact-alias hit scores exactly `dedup::MID` (0.6 =
/// 0.6·1.0 title + 0 corroboration); any cover-pHash / description / author / year
/// signal pushes it strictly above. We additionally require a non-trivial title
/// length so a short common title can't merge two different series. Matches that
/// fail this fall back to the cautious provisional + `merge_candidate` path — never
/// an irreversible merge.
fn federated_consolidate_ok(score: f64, title: &str) -> bool {
    score > crate::dedup::MID + 1e-9
        && crate::catalog::normalize::normalize_title(title)
            .chars()
            .count()
            >= FEDERATED_MIN_TITLE_CHARS
}

/// Derive a source-level NSFW signal from a Suwayomi manga's genres (CATALOGUE.md
/// §2). Shared by the Tier-2 add flow and the federated persist gate (M1).
fn genre_is_nsfw(genre: &[String]) -> bool {
    genre.iter().any(|g| {
        let g = g.to_ascii_lowercase();
        ["hentai", "erotica", "smut", "pornographic", "adult"]
            .iter()
            .any(|k| g.contains(k))
    })
}

/// Select up to `batch` MangaDex-anchored works still needing enrichment
/// (missing metadata OR cover set), oldest first. Shared by the backfill mutation
/// and the X1 scheduler.
async fn works_needing_enrichment(pool: &SqlitePool, batch: i64) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT ss.source_key FROM source_series ss \
         JOIN work w ON w.id = ss.work_id \
         WHERE ss.source_type = 'mangadex' \
           AND (w.metadata_synced_at IS NULL OR w.covers_synced_at IS NULL) \
         ORDER BY ss.created_at ASC LIMIT ?",
    )
    .bind(batch)
    .fetch_all(pool)
    .await
    .map_err(gql_err)
}

/// X1: recurring auto-enrichment. Every `interval_secs` it drains a small batch of
/// un-enriched MangaDex-anchored works (S2 metadata + F2 covers) so newly-ingested
/// works self-enrich without an operator. Shares the MangaDex rate limiter (via
/// `enrich_works`), logs what it did, and does nothing — no thrash — when the
/// backlog is empty. Mirrors `scanner::spawn`. Off unless `METADATA_BACKFILL=on`.
pub fn spawn_metadata_backfill(
    state: std::sync::Arc<AppState>,
    interval_secs: u64,
    batch: i64,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!(interval_secs, batch, "metadata auto-enrichment started");
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let ids = match works_needing_enrichment(&state.pool, batch).await {
                        Ok(ids) => ids,
                        Err(e) => { tracing::warn!(error = %e.message, "enrich tick: selection failed"); continue; }
                    };
                    if ids.is_empty() {
                        tracing::debug!("enrich tick: nothing to enrich");
                        continue;
                    }
                    match enrich_works(&state, &ids).await {
                        Ok(n) => tracing::info!(selected = ids.len(), refreshed = n, "enrich tick: done"),
                        Err(e) => tracing::warn!(error = %e.message, "enrich tick: enrich_works failed"),
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("metadata auto-enrichment stopping");
                        break;
                    }
                }
            }
        }
    });
}

/// Enrich a set of MangaDex-anchored works (S2 metadata + F2 full cover set) and
/// mark them so the backfill cursor advances. Shared by the interactive backfill
/// mutation and the recurring auto-enrichment scheduler (X1). Metadata is fetched
/// batched (100/req); covers are one `/cover` request per work. Every requested id
/// is marked (even if MangaDex returns nothing) so the drain terminates. Returns
/// how many works were upserted.
pub(crate) async fn enrich_works(st: &AppState, ids: &[String]) -> Result<i32> {
    let mut refreshed = 0i32;
    for chunk in ids.chunks(100) {
        let mangas = st.mangadex.get_manga_by_ids(chunk).await.map_err(gql_err)?;
        for m in &mangas {
            let (id, mut input) = crate::mangadex::to_work_input(m);
            // F2: fetch the full per-volume cover set and mark the primary (the one
            // the sweep mirrors on work.cover_file_name). Best-effort — a /cover
            // failure just leaves the sweep's primary cover.
            let primary = crate::mangadex::cover_file_name(m);
            match st.mangadex.list_covers(&id, 100).await {
                Ok(fetched) if !fetched.is_empty() => {
                    input.covers = crate::mangadex::covers_from_fetch(fetched, primary.as_deref());
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(manga = %id, error = %e, "enrich: /cover fetch failed"),
            }
            match catalog::upsert_work_from_mangadex(&st.pool, &id, &input).await {
                Ok(_) => refreshed += 1,
                Err(e) => tracing::warn!(manga = %id, error = %e, "enrich: upsert failed"),
            }
        }
        // Advance the cursor past every requested id — including ones MangaDex
        // didn't return — so the drain can't loop (H1/F2).
        catalog::mark_metadata_synced(&st.pool, chunk)
            .await
            .map_err(gql_err)?;
        catalog::mark_covers_synced(&st.pool, chunk)
            .await
            .map_err(gql_err)?;
    }
    Ok(refreshed)
}

/// Federated-search persist (S3): like `ingest_source_series` but consolidating —
/// a mid-confidence title match links straight to the existing work so the same
/// series across extensions resolves to ONE canonical entry.
async fn federated_ingest(st: &AppState, raw_id: &str) -> anyhow::Result<MatchResult> {
    let mid: i64 = raw_id
        .parse()
        .map_err(|_| anyhow::anyhow!("suwayomiMangaId must be an integer id"))?;
    let mut m = st.suwayomi.series(mid).await?;
    st.suwayomi.set_in_library(mid, true).await?;
    m.in_library = true;
    // S1: cache series METADATA (chapters fill on scan / first read — not per item).
    let _ = crate::series_cache::put_series(&st.pool, &m).await;
    let cover_phash = match st.suwayomi.cover_bytes(m.thumbnail_url.as_deref()).await {
        Some(bytes) => crate::phash::dhash(&bytes),
        None => None,
    };
    add_source_series_core_ex(&st.pool, &m, cover_phash, true).await
}

/// Create a session row and return its opaque token.
async fn new_session(pool: &SqlitePool, user_id: &str, ttl_secs: i64) -> Result<String> {
    let tok = auth::generate_token();
    // Store only sha256(token) at rest (defense-in-depth): a leaked DB snapshot
    // never yields a replayable bearer token. The plaintext `tok` is returned to
    // the caller unchanged and is what the client presents on later requests
    // (`user_for_token` re-hashes before lookup).
    let token_hash = auth::hash_token(&tok);
    let now = Utc::now();
    let created = now.to_rfc3339();
    let expires = auth::format_ts(now + chrono::Duration::seconds(ttl_secs));
    sqlx::query(
        "INSERT INTO sessions (token, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&token_hash)
    .bind(user_id)
    .bind(&created)
    .bind(&expires)
    .execute(pool)
    .await
    .map_err(gql_err)?;
    // Opportunistic GC: drop rows that have already expired so the table (and its
    // index) don't accumulate dead sessions between event-driven deletes.
    let _ = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
        .bind(auth::format_ts(now))
        .execute(pool)
        .await;
    Ok(tok)
}

/// Core of the Tier-2 "add source series" flow (DD2/DD1/N1). Separated from the
/// resolver's Suwayomi fetch so it's unit-testable without a live Suwayomi:
/// idempotency pre-check, dedup, provisional work creation, source-series link,
/// and review-queue insert. `cover_phash` is the already-computed cover hash
/// (None when unavailable).
async fn add_source_series_core(
    pool: &SqlitePool,
    m: &crate::suwayomi::SuwayomiManga,
    cover_phash: Option<String>,
) -> anyhow::Result<MatchResult> {
    add_source_series_core_ex(pool, m, cover_phash, false).await
}

/// As `add_source_series_core`, but `consolidate` controls the mid-confidence
/// (Review-band) branch. When false (the Tier-2 admin add flow): a Review match
/// mints a PROVISIONAL work + a `merge_candidate` for a human to confirm — cautious
/// by design. When true (federated search, S3): a Review match links DIRECTLY to
/// the matched work so the same series across extensions consolidates under ONE
/// canonical entry in search results, as the reader UX requires. Federated
/// consolidation is optimistic and does NOT enqueue a review row (that would
/// re-split the very entry the feature is meant to unify).
async fn add_source_series_core_ex(
    pool: &SqlitePool,
    m: &crate::suwayomi::SuwayomiManga,
    cover_phash: Option<String>,
    consolidate: bool,
) -> anyhow::Result<MatchResult> {
    let source_key = m.id.to_string();

    // DD2 idempotency: if this source series is already linked to a work, return
    // that linkage untouched instead of re-running the matcher (which would mint an
    // orphan work and, for a review, a duplicate merge_candidate row).
    if let Some((ssid, existing_work_id)) =
        crate::catalog::find_source_series(pool, "suwayomi", &m.source_id, &source_key).await?
    {
        return Ok(MatchResult {
            decision: "existing".into(),
            work_id: existing_work_id,
            matched_work_id: None,
            score: None,
            method: None,
            source_series_id: ssid,
        });
    }

    // N1/N5: source-level NSFW derived from the genres already fetched (Suwayomi
    // exposes no confirmed manga nsfw boolean). CATALOGUE.md §2.
    let source_nsfw = genre_is_nsfw(&m.genre);

    // Suwayomi carries no external tracker IDs (no AniList/MAL on MangaType), so
    // `external_ids` stays empty — the external-ID dedup rung is a no-op here.
    let cand = crate::dedup::Candidate {
        title: m.title.clone(),
        alt_titles: Vec::new(),
        description: m.description.clone(),
        author: m.author.clone(),
        year: None,
        cover_phash: cover_phash.clone(),
        external_ids: Vec::new(),
    };
    let decision = crate::dedup::resolve(pool, &cand).await?;

    // The work this source series is (provisionally) linked to. For `new` and
    // `review` we mint a first-class work from the Suwayomi metadata.
    let make_work = || crate::catalog::WorkInput {
        primary_title: Some(m.title.clone()),
        description: m.description.clone(),
        author: m.author.clone(),
        artist: m.artist.clone(),
        status: Some(m.status.clone()),
        is_nsfw: source_nsfw,
        cover_phash: cover_phash.clone(),
        aliases: vec![crate::catalog::Alias {
            raw: m.title.clone(),
            lang: None,
        }],
        ..Default::default()
    };

    use crate::dedup::Decision;
    // `minted_work` is the work we FRESHLY created in this call (New / provisional
    // Review). It's the orphan-cleanup target if we lose the concurrent claim below
    // — AutoMerge / review_consolidated reuse an existing work and must never be
    // deleted here.
    let mut minted_work: Option<String> = None;
    let (mut decision_str, matched_work_id, score, method, mut work_id) = match &decision {
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
        // Federated (S3/C2): consolidate onto the matched work ONLY when the match
        // is corroborated beyond the bare title and the title is non-trivial —
        // otherwise this falls through to the cautious provisional arm below.
        Decision::Review {
            work_id,
            score,
            method,
        } if consolidate && federated_consolidate_ok(*score, &m.title) => (
            "review_consolidated",
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
            // Cautious path: a provisional work + (below) a merge_candidate for a
            // human. Reached by the admin add flow AND by federated matches that
            // failed the C2 corroboration/length guard — never a silent merge.
            let provisional = crate::catalog::create_work(pool, &make_work()).await?;
            minted_work = Some(provisional.clone());
            (
                "review",
                Some(work_id.clone()),
                Some(*score),
                Some(method.clone()),
                provisional,
            )
        }
        Decision::New => {
            let created = crate::catalog::create_work(pool, &make_work()).await?;
            minted_work = Some(created.clone());
            ("new", None, None, None, created)
        }
    };

    let ssid = crate::catalog::upsert_source_series(
        pool,
        &work_id,
        "suwayomi",
        &m.source_id,
        &source_key,
        None,
        source_nsfw,
    )
    .await?;

    // H6 — post-claim re-check. The natural-key `ON CONFLICT` in
    // `upsert_source_series` is the atomic claim: exactly one work_id ends up
    // stored for this (source_type, source_id, source_key). If a concurrent add of
    // the SAME series won the claim, the stored work_id differs from the one we
    // just linked. Adopt the authoritative stored linkage and, if WE minted a fresh
    // work, delete it so it isn't orphaned. This closes the false-split / orphan
    // race without a cross-helper transaction refactor.
    // Residual risk: two concurrent *New* decisions for what is genuinely the same
    // series (both saw no existing work) still mint two works; the claim serializes
    // them so only one is linked and the loser's work is reclaimed here — but a
    // concurrent add matching a DIFFERENT existing work than the winner would keep
    // its own (non-minted) work. That window is narrow and human-reviewable.
    if let Some((_, stored_work_id)) =
        crate::catalog::find_source_series(pool, "suwayomi", &m.source_id, &source_key).await?
    {
        if stored_work_id != work_id {
            if minted_work.as_deref() == Some(work_id.as_str()) {
                crate::catalog::delete_work_cascade(pool, &work_id).await?;
            }
            // The concurrent winner already established (and, if applicable, queued
            // a review for) the canonical linkage — treat this add as idempotent.
            work_id = stored_work_id;
            decision_str = "existing";
        }
    }

    // N4: the source-level NSFW signal must reach `work.is_nsfw` — the only column the
    // gating reads consult. `new`/`review` already mint the work with it via make_work;
    // an `auto_merge` reuses an existing (possibly SFW) work, so OR the flag in there.
    if source_nsfw {
        crate::catalog::mark_work_nsfw(pool, &work_id).await?;
    }

    // Enqueue a review row whenever we took the PROVISIONAL review path (decision
    // "review") — the admin add flow, and the federated fallback for an
    // uncorroborated match. A directly-consolidated federated match
    // ("review_consolidated") already linked to the matched work, so a candidate
    // would ask a human to re-split the very entry we consolidated.
    if decision_str == "review" {
        if let Decision::Review {
            work_id: cand_work,
            score,
            method,
        } = &decision
        {
            crate::catalog::insert_merge_candidate(pool, &ssid, cand_work, *score, method).await?;
        }
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

/// Ensure every configured admin username exists and is an admin. An existing
/// account is promoted (never re-passworded); a missing one is CREATED from
/// `admin_password` (with `is_admin = 1`). When no password is configured, a
/// missing admin can only be logged — it cannot self-register, since admin names
/// are reserved from open registration (A5). This is the sole path to admin
/// status: `register` never grants it.
pub async fn provision_admins(
    pool: &SqlitePool,
    admin_users: &[String],
    admin_password: Option<&str>,
    admin_email: Option<&str>,
) -> anyhow::Result<()> {
    for (idx, username) in admin_users.iter().enumerate() {
        let existing: Option<(String,)> =
            sqlx::query_as("SELECT id FROM users WHERE username = ? COLLATE NOCASE")
                .bind(username)
                .fetch_optional(pool)
                .await?;
        if let Some((id,)) = existing {
            sqlx::query("UPDATE users SET is_admin = 1 WHERE id = ?")
                .bind(&id)
                .execute(pool)
                .await?;
            tracing::info!(username, "ensured admin (promoted existing account)");
            continue;
        }
        let Some(pw) = admin_password else {
            tracing::warn!(
                username,
                "configured admin user is missing and KOMIKA_ADMIN_PASSWORD is unset — \
                 cannot provision it, and the reserved name cannot self-register"
            );
            continue;
        };
        let hash = auth::hash_password(pw)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        // The primary admin gets the configured email; any others get a
        // collision-free synthetic address (email is UNIQUE).
        let email = match (idx, admin_email) {
            (0, Some(e)) => e.to_string(),
            _ => format!("{username}@admin.local"),
        };
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash, avatar_url, is_admin, created_at) \
             VALUES (?, ?, ?, ?, NULL, 1, ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(&email)
        .bind(&hash)
        .bind(&now)
        .execute(pool)
        .await?;
        tracing::info!(
            username,
            "provisioned admin account from KOMIKA_ADMIN_PASSWORD"
        );
    }
    Ok(())
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

    #[test]
    fn limiter_does_not_leak_keys() {
        // A read on an unknown key must not insert a map entry (A4) — otherwise
        // every distinct client IP would grow the map without bound.
        let rl = RateLimiter::new(3, 60);
        for i in 0..100 {
            assert!(rl.is_limited(&format!("k{i}")).is_none());
        }
        assert_eq!(
            rl.hits.lock().unwrap().len(),
            0,
            "reads must not insert keys"
        );

        // A key whose window has fully elapsed is evicted on the next read.
        let rl0 = RateLimiter::new(3, 0); // zero-length window → immediately stale
        rl0.record("k");
        assert_eq!(rl0.hits.lock().unwrap().len(), 1);
        assert!(rl0.is_limited("k").is_none());
        assert_eq!(
            rl0.hits.lock().unwrap().len(),
            0,
            "a fully-stale key is evicted on read"
        );
    }

    // ---- Tier-2 add-source-series core (DD2/DD1/N1) -----------------------

    fn suwayomi_manga(
        id: i64,
        title: &str,
        genre: &[&str],
        source_id: &str,
    ) -> crate::suwayomi::SuwayomiManga {
        crate::suwayomi::SuwayomiManga {
            id,
            title: title.into(),
            thumbnail_url: None,
            author: None,
            artist: None,
            description: None,
            genre: genre.iter().map(|g| g.to_string()).collect(),
            status: "ONGOING".into(),
            in_library: false,
            in_library_at: None,
            last_fetched_at: None,
            source_id: source_id.into(),
            source: None,
            chapters: None,
        }
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

    #[tokio::test]
    async fn add_source_series_new_sets_nsfw_and_is_idempotent() {
        let pool = migrated_pool().await;
        // A unique-title, NSFW-genre series → decision "new", work.is_nsfw = 1 (N1).
        let m = suwayomi_manga(42, "A Very Spicy One-Off", &["Action", "Hentai"], "src1");
        let r1 = add_source_series_core(&pool, &m, None).await.unwrap();
        assert_eq!(r1.decision, "new");
        let nsfw: i64 = sqlx::query_scalar("SELECT is_nsfw FROM work WHERE id = ?")
            .bind(&r1.work_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(nsfw, 1, "N1: an NSFW genre sets work.is_nsfw");

        let works_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work")
            .fetch_one(&pool)
            .await
            .unwrap();
        // DD2: re-adding the same source id returns the existing linkage and mints
        // nothing new.
        let r2 = add_source_series_core(&pool, &m, None).await.unwrap();
        assert_eq!(r2.decision, "existing");
        assert_eq!(r2.work_id, r1.work_id);
        assert_eq!(r2.source_series_id, r1.source_series_id);
        let works_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(works_after, works_before, "DD2: no orphan work on re-add");
    }

    #[tokio::test]
    async fn add_source_series_review_does_not_duplicate_on_re_add() {
        let pool = migrated_pool().await;
        // Pre-seed a work with a title so a same-titled add lands in Review (title
        // match, no corroboration → mid score).
        let existing = crate::catalog::create_work(
            &pool,
            &crate::catalog::WorkInput {
                primary_title: Some("Twin Star Exorcists".into()),
                aliases: vec![crate::catalog::Alias {
                    raw: "Twin Star Exorcists".into(),
                    lang: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let m = suwayomi_manga(7, "Twin Star Exorcists", &["Action"], "src1");
        let r1 = add_source_series_core(&pool, &m, None).await.unwrap();
        assert_eq!(r1.decision, "review", "title match with no corroboration");
        assert_eq!(r1.matched_work_id.as_deref(), Some(existing.as_str()));

        let works_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mc_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_candidate")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mc_before, 1, "one pending review row after the first add");

        // DD2: re-add → existing; neither the orphan work nor a duplicate
        // merge_candidate is created.
        let r2 = add_source_series_core(&pool, &m, None).await.unwrap();
        assert_eq!(r2.decision, "existing");
        let works_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mc_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_candidate")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(works_after, works_before, "DD2: no orphan work on re-add");
        assert_eq!(mc_after, mc_before, "DD2: no duplicate merge_candidate");
    }

    #[test]
    fn federated_consolidate_ok_requires_corroboration_and_title_length() {
        use crate::dedup::MID;
        // A bare title-only exact match scores exactly MID → never consolidates.
        assert!(!federated_consolidate_ok(MID, "Twin Star Exorcists"));
        // Any corroboration pushes score above MID → consolidates (long title).
        assert!(federated_consolidate_ok(MID + 0.05, "Twin Star Exorcists"));
        // Even corroborated, a too-short/common title is refused.
        assert!(!federated_consolidate_ok(0.9, "ao"));
        assert!(!federated_consolidate_ok(0.9, "hero")); // 4 chars < 5
        assert!(federated_consolidate_ok(0.9, "naruto"));
    }

    #[tokio::test]
    async fn federated_corroborated_match_consolidates_to_existing_work() {
        // C2: a mid-confidence match CORROBORATED beyond the title (here a shared
        // description) links DIRECTLY to the existing work — one work, no review row.
        let pool = migrated_pool().await;
        let blurb = "Twin exorcists destined to marry and birth the Miko.";
        let existing = crate::catalog::create_work(
            &pool,
            &crate::catalog::WorkInput {
                primary_title: Some("Twin Star Exorcists".into()),
                description: Some(blurb.into()),
                aliases: vec![crate::catalog::Alias {
                    raw: "Twin Star Exorcists".into(),
                    lang: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let mut m = suwayomi_manga(7, "Twin Star Exorcists", &["Action"], "src-ext2");
        m.description = Some(blurb.into()); // corroborating description
        let r = add_source_series_core_ex(&pool, &m, None, true)
            .await
            .unwrap();
        assert_eq!(r.decision, "review_consolidated");
        assert_eq!(r.work_id, existing, "links to the existing work");
        let works: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(works, 1, "no provisional work created");
        let mc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_candidate")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mc, 0, "corroborated consolidation enqueues no review row");
    }

    #[tokio::test]
    async fn federated_bare_title_collision_does_not_silently_merge() {
        // C2 (the critical guard): a bare title-only match — zero corroboration,
        // exactly MID — must NOT be silently merged even in the federated
        // (consolidate) path. It falls back to the cautious provisional + a
        // merge_candidate for human review, so two different same-titled series
        // are never irreversibly joined.
        let pool = migrated_pool().await;
        let existing = crate::catalog::create_work(
            &pool,
            &crate::catalog::WorkInput {
                primary_title: Some("Twin Star Exorcists".into()),
                aliases: vec![crate::catalog::Alias {
                    raw: "Twin Star Exorcists".into(),
                    lang: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Same title, NO description/cover → title-only, score == MID.
        let m = suwayomi_manga(7, "Twin Star Exorcists", &["Action"], "src-ext2");
        let r = add_source_series_core_ex(&pool, &m, None, true)
            .await
            .unwrap();
        assert_eq!(r.decision, "review", "bare title-only is NOT consolidated");
        assert_ne!(
            r.work_id, existing,
            "a distinct provisional work is minted, not a silent merge"
        );
        let works: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(works, 2, "two distinct works remain (no merge)");
        let mc: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM merge_candidate")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mc, 1, "a merge_candidate is queued for human review");
        // The new mapping points at the provisional, and the candidate is the existing.
        let linked: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_id = 'src-ext2'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked, r.work_id);
        let cand: String = sqlx::query_scalar("SELECT candidate_work_id FROM merge_candidate")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cand, existing, "the review candidate is the existing work");
    }

    #[tokio::test]
    async fn add_source_series_safe_genre_is_not_nsfw() {
        let pool = migrated_pool().await;
        let m = suwayomi_manga(
            99,
            "Wholesome Slice of Life",
            &["Comedy", "Slice of Life"],
            "src1",
        );
        let r = add_source_series_core(&pool, &m, None).await.unwrap();
        let nsfw: i64 = sqlx::query_scalar("SELECT is_nsfw FROM work WHERE id = ?")
            .bind(&r.work_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(nsfw, 0, "safe genres → work.is_nsfw stays 0");
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
        // Sessions store sha256(token); clients still present the raw token
        // ("admintok"/"bobtok") in the Authorization header, which `user_for_token`
        // re-hashes before lookup — so seed the hashed value here.
        for (tok, uid) in [("admintok", "admin-id"), ("bobtok", "bob-id")] {
            sqlx::query(
                "INSERT INTO sessions (token, user_id, created_at, expires_at) \
                 VALUES (?, ?, '2020-01-01T00:00:00Z', '2999-01-01T00:00:00Z')",
            )
            .bind(auth::hash_token(tok))
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
        }
        // An already-expired session — its token must not resolve (A1).
        sqlx::query(
            "INSERT INTO sessions (token, user_id, created_at, expires_at) \
             VALUES (?, 'bob-id', '2020-01-01T00:00:00Z', '2020-02-01T00:00:00Z')",
        )
        .bind(auth::hash_token("expiredtok"))
        .execute(&pool)
        .await
        .unwrap();
        let state = std::sync::Arc::new(AppState {
            pool: pool.clone(),
            suwayomi: crate::suwayomi::SuwayomiClient::new("http://127.0.0.1:1".into(), None, None),
            mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new(
                "test-ua", 5.0, 40.0,
            )),
            admin_users: vec![],
            scan_health: Mutex::new(ScanHealth::default()),
            auth_limiter: RateLimiter::new(max, 60),
            federated_limiter: RateLimiter::new(100, 60),
            session_ttl_secs: 30 * 24 * 60 * 60,
            series_inflight: KeyedLocks::default(),
            chapters_inflight: KeyedLocks::default(),
        });
        (build_schema(state, false), pool)
    }

    async fn exec(
        schema: &ApiSchema,
        query: &str,
        token: Option<&str>,
        ip: &str,
    ) -> async_graphql::Response {
        let req = async_graphql::Request::new(query)
            .data(RequestAuth(token.map(|t| t.to_string())))
            .data(ClientIp(Some(ip.to_string())))
            .data(RequestUserCache::default());
        schema.execute(req).await
    }

    fn first_error(resp: &async_graphql::Response) -> String {
        resp.errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn updates_feed_is_newest_first_and_reports_new_chapter_time() {
        // "Latest Updates" orders by the scanner's new-chapter detection time
        // (newest first) AND reports that time as `updatedAt` — not the series' last
        // poll — so the reader shows a faithful "updated X ago".
        let (s, pool) = setup_full(100).await;
        for (id, title, new_at) in [
            (10_i64, "Older Update", "2026-07-01T00:00:00+00:00"),
            (20, "Newer Update", "2026-07-10T00:00:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO suwayomi_series (id, title, status, source_id, chapter_count, updated_at) \
                 VALUES (?, ?, 'ONGOING', 'src', 5, '2026-07-15T00:00:00+00:00')",
            )
            .bind(id)
            .bind(title)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO series_scan_state \
                   (series_id, avg_interval_hours, known_chapter_count, last_new_chapter_at, updated_at) \
                 VALUES (?, 0, 5, ?, '2026-07-15T00:00:00+00:00')",
            )
            .bind(id.to_string())
            .bind(new_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        let r = exec(
            &s,
            r#"{ updates { items { id title updatedAt } total } }"#,
            None,
            "1.2.3.4",
        )
        .await;
        assert!(r.errors.is_empty(), "updates failed: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let items = data["updates"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        // Newest new-chapter time first.
        assert_eq!(items[0]["title"], serde_json::json!("Newer Update"));
        assert_eq!(items[1]["title"], serde_json::json!("Older Update"));
        // updatedAt is the new-chapter time, NOT the '2026-07-15' poll/update stamp.
        assert_eq!(
            items[0]["updatedAt"],
            serde_json::json!("2026-07-10T00:00:00+00:00")
        );
        assert_eq!(
            items[1]["updatedAt"],
            serde_json::json!("2026-07-01T00:00:00+00:00")
        );
    }

    #[tokio::test]
    async fn comment_votes_and_notifications_flow() {
        let (s, _pool) = setup_full(100).await;
        // admin posts a root comment on series "s1".
        let r = exec(
            &s,
            r#"mutation { postComment(input: { targetType: "series", targetId: "s1", body: "hi", hasSpoiler: false }) { id likes myVote } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "postComment: {:?}", r.errors);
        let cid = r.data.into_json().unwrap()["postComment"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // bob replies -> admin gets a 'reply' notification.
        let reply = format!(
            r#"mutation {{ postComment(input: {{ targetType: "series", targetId: "s1", parentId: "{cid}", body: "yo", hasSpoiler: false }}) {{ id }} }}"#
        );
        let r = exec(&s, &reply, Some("bobtok"), "2.2.2.2").await;
        assert!(r.errors.is_empty(), "reply: {:?}", r.errors);

        // bob likes admin's comment -> tally = 1 like, and admin gets a 'like_milestone'.
        let vote =
            format!(r#"mutation {{ voteComment(commentId: "{cid}", value: 1) {{ likes dislikes myVote }} }}"#);
        let r = exec(&s, &vote, Some("bobtok"), "2.2.2.2").await;
        assert!(r.errors.is_empty(), "vote: {:?}", r.errors);
        let v = r.data.into_json().unwrap()["voteComment"].clone();
        assert_eq!(v["likes"], serde_json::json!(1));
        assert_eq!(v["myVote"], serde_json::json!(1));

        // admin sees 2 unread notifications (reply + like_milestone).
        let r = exec(
            &s,
            r#"{ unreadNotificationCount notifications { kind count actor { username } commentExcerpt } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        let d = r.data.into_json().unwrap();
        assert_eq!(d["unreadNotificationCount"], serde_json::json!(2));
        let notifs = d["notifications"].as_array().unwrap();
        let kinds: Vec<&str> = notifs.iter().map(|n| n["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"reply") && kinds.contains(&"like_milestone"));
        // The reply notification names the actor; the milestone carries the count.
        let reply_n = notifs.iter().find(|n| n["kind"] == "reply").unwrap();
        assert_eq!(reply_n["actor"]["username"], serde_json::json!("bob"));
        let ms = notifs.iter().find(|n| n["kind"] == "like_milestone").unwrap();
        assert_eq!(ms["count"], serde_json::json!(1));

        // The comments query shows the like tally; bob (the liker) sees myVote = 1.
        let r = exec(
            &s,
            r#"{ comments(targetType: "series", targetId: "s1") { items { id likes myVote } } }"#,
            Some("bobtok"),
            "2.2.2.2",
        )
        .await;
        let items = r.data.into_json().unwrap()["comments"]["items"]
            .as_array()
            .unwrap()
            .clone();
        let root = items.iter().find(|c| c["id"] == serde_json::json!(cid)).unwrap();
        assert_eq!(root["likes"], serde_json::json!(1));
        assert_eq!(root["myVote"], serde_json::json!(1));

        // admin marks all read -> 0 unread.
        let r = exec(&s, r#"mutation { markNotificationsRead }"#, Some("admintok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "markRead: {:?}", r.errors);
        let r = exec(&s, r#"{ unreadNotificationCount }"#, Some("admintok"), "1.1.1.1").await;
        assert_eq!(
            r.data.into_json().unwrap()["unreadNotificationCount"],
            serde_json::json!(0)
        );

        // bob (the replier/liker) is never notified of his own actions.
        let r = exec(&s, r#"{ unreadNotificationCount }"#, Some("bobtok"), "2.2.2.2").await;
        assert_eq!(
            r.data.into_json().unwrap()["unreadNotificationCount"],
            serde_json::json!(0)
        );
    }

    #[tokio::test]
    async fn record_view_counts_anonymously_and_surfaces_on_series() {
        // Views are the popularity signal: `recordView` needs NO auth (anonymous reads
        // count too), and the count surfaces on `series.views` across all windows. Here
        // a cached numeric series is viewed three times by an anonymous client (no
        // token), then read back.
        let (s, pool) = setup_full(100).await;
        sqlx::query(
            "INSERT INTO suwayomi_series (id, title, status, source_id, chapter_count, updated_at) \
             VALUES (777, 'Viewed Series', 'ONGOING', 'src', 0, '2020-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        for _ in 0..3 {
            let r = exec(&s, r#"mutation { recordView(seriesId: "777") }"#, None, "9.9.9.9").await;
            assert!(r.errors.is_empty(), "recordView failed: {:?}", r.errors);
            assert_eq!(
                r.data.into_json().unwrap()["recordView"],
                serde_json::json!(true)
            );
        }

        let r = exec(
            &s,
            r#"{ series(id: "777") { views { total last7d last24h } } }"#,
            None,
            "9.9.9.9",
        )
        .await;
        assert!(r.errors.is_empty(), "series query failed: {:?}", r.errors);
        let v = r.data.into_json().unwrap()["series"]["views"].clone();
        assert_eq!(v["total"], serde_json::json!(3));
        assert_eq!(v["last24h"], serde_json::json!(3));
        assert_eq!(v["last7d"], serde_json::json!(3));
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

    #[tokio::test]
    async fn update_profile_persists_and_is_reflected_in_session() {
        let s = setup().await;
        // Fresh account: display name/bio are null, joinedAt is present.
        let r = exec(
            &s,
            r#"{ session { user { displayName bio joinedAt } } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let j = data_json(&r);
        assert!(j.contains("\"displayName\":null"), "unexpected: {j}");
        assert!(j.contains("\"bio\":null"), "unexpected: {j}");
        assert!(j.contains("\"joinedAt\":\"2020-01-01"), "unexpected: {j}");

        // Update both; the mutation returns the refreshed user.
        let r = exec(
            &s,
            r#"mutation { updateProfile(input: { displayName: "  Bob the Reader  ", bio: "I read manga." }) { displayName bio } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let j = data_json(&r);
        assert!(
            j.contains("\"displayName\":\"Bob the Reader\""),
            "trimmed: {j}"
        );
        assert!(j.contains("\"bio\":\"I read manga.\""), "{j}");

        // A blank display name clears it (falls back to username in the UI).
        let r = exec(
            &s,
            r#"mutation { updateProfile(input: { displayName: "   ", bio: "still here" }) { displayName bio } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(data_json(&r).contains("\"displayName\":null"), "cleared");

        // Anonymous cannot update.
        let r = exec(
            &s,
            r#"mutation { updateProfile(input: { bio: "x" }) { bio } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "Not authenticated");
    }

    #[tokio::test]
    async fn update_profile_rejects_overlong_fields() {
        let s = setup().await;
        let long_name = "a".repeat(51);
        let r = exec(
            &s,
            &format!(
                r#"mutation {{ updateProfile(input: {{ displayName: "{long_name}" }}) {{ id }} }}"#
            ),
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(
            first_error(&r),
            "display name must be at most 50 characters"
        );
    }

    #[tokio::test]
    async fn my_activity_records_reviews_and_is_empty_when_signed_out() {
        let s = setup().await;
        // Signed out → empty feed, never an error.
        let r = exec(&s, r#"{ myActivity { id } }"#, None, "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        assert!(data_json(&r).contains("\"myActivity\":[]"));

        // Posting a review records a 'review' activity targeting the series.
        let r = exec(
            &s,
            r#"mutation { postReview(input: { seriesId: "42", score: 8, body: "great", hasSpoiler: false }) { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);

        let r = exec(
            &s,
            r#"{ myActivity { kind targetType targetId } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let j = data_json(&r);
        assert!(j.contains("\"kind\":\"review\""), "{j}");
        assert!(j.contains("\"targetType\":\"series\""), "{j}");
        assert!(j.contains("\"targetId\":\"42\""), "{j}");
    }

    #[tokio::test]
    async fn introspection_toggles_with_flag() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let mk_state = || {
            std::sync::Arc::new(AppState {
                pool: pool.clone(),
                suwayomi: crate::suwayomi::SuwayomiClient::new(
                    "http://127.0.0.1:1".into(),
                    None,
                    None,
                ),
                mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new(
                    "test-ua", 5.0, 40.0,
                )),
                admin_users: vec![],
                scan_health: Mutex::new(ScanHealth::default()),
                auth_limiter: RateLimiter::new(100, 60),
                federated_limiter: RateLimiter::new(100, 60),
                session_ttl_secs: 30 * 24 * 60 * 60,
                series_inflight: KeyedLocks::default(),
                chapters_inflight: KeyedLocks::default(),
            })
        };
        const Q: &str = "{ __schema { queryType { name } } }";
        // Disabled (production default): `__schema` resolves to null, so the API
        // surface is not enumerable.
        let disabled = build_schema(mk_state(), true);
        let r = disabled.execute(Q).await;
        assert_eq!(
            serde_json::to_string(&r.data).unwrap(),
            "{\"__schema\":null}",
            "disabled introspection must not leak the schema (errors: {:?})",
            r.errors
        );
        // Enabled (dev): introspection returns the real schema.
        let enabled = build_schema(mk_state(), false);
        let r = enabled.execute(Q).await;
        let json = serde_json::to_string(&r.data).unwrap();
        assert!(
            r.errors.is_empty() && json.contains("queryType") && json != "{\"__schema\":null}",
            "introspection should work when enabled: {json} (errors: {:?})",
            r.errors
        );
    }

    #[tokio::test]
    async fn provision_admins_creates_promotes_and_is_idempotent() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        // Missing admin + password → created as admin with the configured email.
        provision_admins(&pool, &["admin".into()], Some("s3cret-pw"), Some("a@b.com"))
            .await
            .unwrap();
        let (is_admin, email, hash): (i64, String, String) = sqlx::query_as(
            "SELECT is_admin, email, password_hash FROM users WHERE username='admin'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(is_admin, 1);
        assert_eq!(email, "a@b.com");
        assert!(auth::verify_password("s3cret-pw", &hash), "password usable");

        // Idempotent re-run with a different password: no duplicate, existing
        // password preserved (never re-passworded).
        provision_admins(
            &pool,
            &["admin".into()],
            Some("different-pw"),
            Some("a@b.com"),
        )
        .await
        .unwrap();
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE username='admin'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "no duplicate admin row");
        let (hash2,): (String,) =
            sqlx::query_as("SELECT password_hash FROM users WHERE username='admin'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            auth::verify_password("s3cret-pw", &hash2),
            "existing password preserved, not reset"
        );

        // Existing non-admin gets promoted; no password required.
        seed_user(&pool, "bob-id", "bob", 0, 0).await;
        provision_admins(&pool, &["bob".into()], None, None)
            .await
            .unwrap();
        let (bob_admin,): (i64,) =
            sqlx::query_as("SELECT is_admin FROM users WHERE username='bob'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(bob_admin, 1, "existing account promoted");
    }

    #[tokio::test]
    async fn register_rejects_reserved_admin_username() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let state = std::sync::Arc::new(AppState {
            pool: pool.clone(),
            suwayomi: crate::suwayomi::SuwayomiClient::new("http://127.0.0.1:1".into(), None, None),
            mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new(
                "test-ua", 5.0, 40.0,
            )),
            admin_users: vec!["admin".into()],
            scan_health: Mutex::new(ScanHealth::default()),
            auth_limiter: RateLimiter::new(100, 60),
            federated_limiter: RateLimiter::new(100, 60),
            session_ttl_secs: 30 * 24 * 60 * 60,
            series_inflight: KeyedLocks::default(),
            chapters_inflight: KeyedLocks::default(),
        });
        let s = build_schema(state, false);
        // A configured admin name is reserved (case-insensitive) — open
        // registration cannot squat it or self-elevate.
        let r = exec(
            &s,
            r#"mutation { register(input:{username:"Admin", email:"x@y.com", password:"password123"}) { token } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "This username is reserved.");
        // A normal name registers fine and is NOT admin.
        let r = exec(
            &s,
            r#"mutation { register(input:{username:"alice", email:"a@y.com", password:"password123"}) { user { isAdmin } } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        assert!(data_json(&r).contains("\"isAdmin\":false"));
    }

    #[tokio::test]
    async fn expired_session_token_does_not_resolve() {
        let s = setup().await;
        // A live session resolves.
        let r = exec(
            &s,
            r#"{ session { user { username } } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(data_json(&r).contains("\"username\":\"bob\""));
        // The seeded expired token (expires_at 2020-02-01) must resolve to null.
        let r = exec(
            &s,
            r#"{ session { user { username } } }"#,
            Some("expiredtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        assert_eq!(data_json(&r), "{\"session\":null}");
        // An expired token is also treated as anonymous for auth-gated mutations.
        let r = exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("expiredtok"),
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
                lang: Some("en".into()),
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
        for (ext, num, lang) in [
            ("md-ch-2", "2", "en"),
            ("md-ch-1", "1", "en"),
            ("md-ch-3-es", "3", "es"), // non-English → must never surface
        ] {
            crate::catalog::upsert_chapter(
                &pool,
                &ssid,
                &crate::catalog::ChapterInput {
                    external_id: ext.into(),
                    number: Some(num.into()),
                    lang: Some(lang.into()),
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
        let q = format!(r#"{{ canonicalChapters(workId: "{work_id}") {{ id number seriesId }} }}"#);
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
    async fn nsfw_override_flips_gating_both_directions() {
        // The admin editor's is_nsfw_override must win over the source flag on EVERY
        // gate (regression for the half-enforced override: a leak when forced NSFW, a
        // broken un-gate when forced SFW).
        let (s, pool) = setup_full(100).await;

        // A source-SFW work an admin force-marks NSFW → hidden from an opted-out viewer.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-forcensfw",
            &crate::catalog::WorkInput {
                primary_title: Some("Forced NSFW".into()),
                is_nsfw: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let forced_nsfw: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-forcensfw'")
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("UPDATE work SET is_nsfw_override = 1 WHERE id = ?")
            .bind(&forced_nsfw)
            .execute(&pool)
            .await
            .unwrap();
        let q = format!(r#"{{ canonicalSeries(workId: "{forced_nsfw}") {{ title }} }}"#);
        assert_eq!(
            first_error(&exec(&s, &q, Some("bobtok"), "1.1.1.1").await),
            "No such work",
            "force-NSFW must hide canonicalSeries"
        );
        let qa = format!(r#"{{ aggregatedChapters(workId: "{forced_nsfw}") {{ number }} }}"#);
        assert_eq!(
            first_error(&exec(&s, &qa, Some("bobtok"), "1.1.1.1").await),
            "No such work",
            "force-NSFW must hide aggregatedChapters"
        );

        // A source-NSFW work an admin force-marks SFW → visible to an opted-out viewer.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-forcesfw",
            &crate::catalog::WorkInput {
                primary_title: Some("Forced SFW".into()),
                is_nsfw: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let forced_sfw: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-forcesfw'")
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("UPDATE work SET is_nsfw_override = 0 WHERE id = ?")
            .bind(&forced_sfw)
            .execute(&pool)
            .await
            .unwrap();
        let q = format!(r#"{{ canonicalSeries(workId: "{forced_sfw}") {{ title }} }}"#);
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "force-SFW must un-gate: {:?}", r.errors);
        assert!(data_json(&r).contains("Forced SFW"));
    }

    #[tokio::test]
    async fn series_exposes_localized_descriptions_and_credits() {
        // H2: the S2 enrichment tables are now readable via resolver fields on the
        // Series type (canonicalSeries path).
        let (s, pool) = setup_full(100).await;
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-enr",
            &crate::catalog::WorkInput {
                primary_title: Some("Enriched Work".into()),
                cover_file_name: Some("c.jpg".into()),
                descriptions: vec![
                    ("en".into(), "English blurb.".into()),
                    ("ja".into(), "日本語の紹介".into()),
                ],
                credits: vec![
                    ("author".into(), "Author One".into()),
                    ("artist".into(), "Artist Two".into()),
                ],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let work_id: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-enr'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let q = format!(
            r#"{{ canonicalSeries(workId: "{work_id}") {{ localizedDescriptions {{ lang description }} credits {{ role name }} }} }}"#
        );
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let descs = data["canonicalSeries"]["localizedDescriptions"]
            .as_array()
            .unwrap();
        assert_eq!(descs.len(), 2, "both languages surface");
        assert_eq!(descs[0]["lang"], serde_json::json!("en"));
        assert_eq!(descs[0]["description"], serde_json::json!("English blurb."));
        assert_eq!(descs[1]["lang"], serde_json::json!("ja"));
        let credits = data["canonicalSeries"]["credits"].as_array().unwrap();
        assert_eq!(credits.len(), 2);
        assert_eq!(credits[0]["role"], serde_json::json!("artist"));
        assert_eq!(credits[0]["name"], serde_json::json!("Artist Two"));
        assert_eq!(credits[1]["role"], serde_json::json!("author"));
    }

    #[tokio::test]
    async fn work_sources_returns_mappings_ordered_and_extension_joined() {
        let (s, pool) = setup_full(100).await;
        // A MangaDex-native mapping (no extension) plus a Suwayomi mapping whose
        // source_id has catalogued extension coordinates.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-x",
            &crate::catalog::WorkInput {
                primary_title: Some("Joined Work".into()),
                is_nsfw: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let work_id: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-x'")
                .fetch_one(&pool)
                .await
                .unwrap();
        crate::catalog::upsert_source_series(
            &pool, &work_id, "suwayomi", "1024", "slug-1", None, false,
        )
        .await
        .unwrap();
        crate::catalog::upsert_source_extension(
            &pool,
            "1024",
            &crate::catalog::SourceExtensionInput {
                pkg_name: "pkg.x".into(),
                repo_url: "https://r".into(),
                apk_name: None,
                version_code: Some(7),
                lang: Some("en".into()),
                is_nsfw: false,
            },
        )
        .await
        .unwrap();

        let q = format!(
            r#"{{ workSources(workId: "{work_id}") {{ sourceType sourceId sourceKey isNsfw lang extension {{ pkgName repoUrl versionCode }} }} }}"#
        );
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let rows = data["workSources"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "both source mappings surface");
        // The MangaDex-native mapping sorts first and has no extension.
        assert_eq!(rows[0]["sourceType"], serde_json::json!("mangadex"));
        assert_eq!(rows[0]["extension"], serde_json::Value::Null);
        // The Suwayomi mapping carries its joined extension coordinates.
        assert_eq!(rows[1]["sourceType"], serde_json::json!("suwayomi"));
        assert_eq!(rows[1]["sourceKey"], serde_json::json!("slug-1"));
        assert_eq!(rows[1]["extension"]["pkgName"], serde_json::json!("pkg.x"));
        assert_eq!(
            rows[1]["extension"]["repoUrl"],
            serde_json::json!("https://r")
        );
        assert_eq!(rows[1]["extension"]["versionCode"], serde_json::json!(7));
        assert_eq!(rows[1]["lang"], serde_json::json!("en"));
    }

    #[tokio::test]
    async fn work_sources_hides_nsfw_for_opted_out_viewer() {
        let (s, pool) = setup_full(100).await;
        // A safe MangaDex mapping plus an NSFW Suwayomi mapping on the same work.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-y",
            &crate::catalog::WorkInput {
                primary_title: Some("Mixed Work".into()),
                is_nsfw: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let work_id: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-y'")
                .fetch_one(&pool)
                .await
                .unwrap();
        crate::catalog::upsert_source_series(
            &pool,
            &work_id,
            "suwayomi",
            "2048",
            "spicy-slug",
            None,
            true,
        )
        .await
        .unwrap();

        let q =
            format!(r#"{{ workSources(workId: "{work_id}") {{ sourceType sourceKey isNsfw }} }}"#);
        // Opted-out viewer: only the safe mapping is visible.
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let rows = data["workSources"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "nsfw mapping hidden for opted-out viewer");
        assert_eq!(rows[0]["sourceType"], serde_json::json!("mangadex"));

        // After opting in, the NSFW mapping appears.
        exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "nsfw opt-in failed: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let rows = data["workSources"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "nsfw mapping visible after opt-in");
        assert!(
            rows.iter()
                .any(|row| row["sourceKey"] == serde_json::json!("spicy-slug")),
            "the nsfw mapping surfaces: {data}"
        );
    }

    #[tokio::test]
    async fn canonical_resolvers_reject_non_mangadex_anchored_work() {
        // A backfilled `w_<numeric>` work has no mangadex source (mangadex_id = None):
        // `isCanonicalId` would still route it here, and the resolver must return
        // "No such work" rather than a titleless/coverless/chapterless shell (CR3).
        let (s, pool) = setup_full(100).await;
        sqlx::query("INSERT INTO work (id, primary_title, is_nsfw, created_at, updated_at) VALUES ('w_42', 'Backfilled Shell', 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        // A non-mangadex (suwayomi) source_series — no mangadex anchor exists.
        sqlx::query("INSERT INTO source_series (id, work_id, source_type, source_id, source_key, created_at) VALUES ('ss_42', 'w_42', 'suwayomi', '', '42', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

        let q = r#"{ canonicalSeries(workId: "w_42") { title } }"#;
        let r = exec(&s, q, Some("bobtok"), "1.1.1.1").await;
        assert_eq!(first_error(&r), "No such work");

        let q = r#"{ canonicalChapters(workId: "w_42") { id } }"#;
        let r = exec(&s, q, Some("bobtok"), "1.1.1.1").await;
        assert_eq!(first_error(&r), "No such work");

        // A normal mangadex-anchored canonical work still resolves.
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-anchored",
            &crate::catalog::WorkInput {
                primary_title: Some("Anchored Work".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let anchored: String = sqlx::query_scalar(
            "SELECT work_id FROM source_series WHERE source_key = 'md-anchored'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let q = format!(r#"{{ canonicalSeries(workId: "{anchored}") {{ title }} }}"#);
        let r = exec(&s, &q, Some("bobtok"), "1.1.1.1").await;
        assert!(
            r.errors.is_empty(),
            "anchored work should resolve: {:?}",
            r.errors
        );
        assert!(data_json(&r).contains("Anchored Work"));
    }

    #[tokio::test]
    async fn canonical_progress_library_and_rating_round_trip() {
        // CR6: a canonical (w_) work gets per-user library + progress state and a
        // reused (reviews-backed) rating aggregate — all keyed on the opaque w_/uuid.
        let (s, pool) = setup_full(100).await;
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-cr6",
            &crate::catalog::WorkInput {
                primary_title: Some("Stateful Work".into()),
                cover_file_name: Some("cover.jpg".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let ssid = crate::catalog::find_source_series_id(&pool, "mangadex", "mangadex", "md-cr6")
            .await
            .unwrap()
            .unwrap();
        for (ext, num) in [("uuid-a", "1"), ("uuid-b", "2")] {
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
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-cr6'")
                .fetch_one(&pool)
                .await
                .unwrap();

        // ---- Library: mark persists + reflects, unmark removes ----
        let mark_q = |m: bool| {
            format!(r#"mutation {{ mark(seriesId: "{work_id}", marked: {m}) {{ isMarked }} }}"#)
        };
        // Anonymous cannot persist.
        let r = exec(&s, &mark_q(true), None, "1.1.1.1").await;
        assert_eq!(first_error(&r), "Not authenticated");

        let r = exec(&s, &mark_q(true), Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "mark failed: {:?}", r.errors);
        assert!(data_json(&r).contains("\"isMarked\":true"));
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_library WHERE user_id = 'bob-id' AND series_id = ?",
        )
        .bind(&work_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
        // canonicalSeries reflects the mark for bob but not for an anonymous viewer.
        let series_q = format!(r#"{{ canonicalSeries(workId: "{work_id}") {{ isMarked }} }}"#);
        let r = exec(&s, &series_q, Some("bobtok"), "1.1.1.1").await;
        assert!(data_json(&r).contains("\"isMarked\":true"));
        let r = exec(&s, &series_q, None, "1.1.1.1").await;
        assert!(data_json(&r).contains("\"isMarked\":false"));
        // Unmark removes.
        let r = exec(&s, &mark_q(false), Some("bobtok"), "1.1.1.1").await;
        assert!(data_json(&r).contains("\"isMarked\":false"));
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_library WHERE user_id = 'bob-id'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(rows, 0, "unmark deletes the row");

        // ---- Progress: setProgress on a uuid persists + drives resume ----
        let prog_q =
            r#"mutation { setProgress(chapterId: "uuid-a", lastPageRead: 12, read: true) }"#;
        // Anonymous cannot persist.
        let r = exec(&s, prog_q, None, "1.1.1.1").await;
        assert_eq!(first_error(&r), "Not authenticated");

        let r = exec(&s, prog_q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "setProgress failed: {:?}", r.errors);
        // canonicalChapters surfaces the per-user read state (and only for that user).
        let chapters_q =
            format!(r#"{{ canonicalChapters(workId: "{work_id}") {{ id read lastPageRead }} }}"#);
        let r = exec(&s, &chapters_q, Some("bobtok"), "1.1.1.1").await;
        let data = r.data.into_json().unwrap();
        let chs = data["canonicalChapters"].as_array().unwrap();
        let a = chs.iter().find(|c| c["id"] == "uuid-a").unwrap();
        assert_eq!(a["read"], serde_json::json!(true));
        assert_eq!(a["lastPageRead"], serde_json::json!(12));
        // The owning work_id was resolved from the chapter uuid.
        let stored_work: String = sqlx::query_scalar(
            "SELECT work_id FROM canonical_progress WHERE user_id = 'bob-id' AND chapter_id = 'uuid-a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored_work, work_id);
        // A second call updates in place — no duplicate row.
        let r = exec(
            &s,
            r#"mutation { setProgress(chapterId: "uuid-a", lastPageRead: 30, read: false) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let (cnt, lpr, rd): (i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), MAX(last_page_read), MAX(read) FROM canonical_progress \
             WHERE user_id = 'bob-id' AND chapter_id = 'uuid-a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((cnt, lpr, rd), (1, 30, 0), "upsert in place");
        // An anonymous viewer sees the chapter as unread.
        let r = exec(&s, &chapters_q, None, "1.1.1.1").await;
        let data = r.data.into_json().unwrap();
        let a = data["canonicalChapters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "uuid-a")
            .unwrap()
            .clone();
        assert_eq!(a["read"], serde_json::json!(false));
        assert_eq!(a["lastPageRead"], serde_json::json!(0));

        // ---- Rating: postReview on the w_ id aggregates via reused reviews ----
        let r = exec(
            &s,
            &format!(
                r#"mutation {{ postReview(input: {{ seriesId: "{work_id}", score: 9, body: "", hasSpoiler: false }}) {{ score }} }}"#
            ),
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "postReview failed: {:?}", r.errors);
        let r = exec(
            &s,
            &format!(
                r#"{{ canonicalSeries(workId: "{work_id}") {{ rating {{ average count }} }} }}"#
            ),
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let data = r.data.into_json().unwrap();
        assert_eq!(
            data["canonicalSeries"]["rating"]["average"],
            serde_json::json!(9.0)
        );
        assert_eq!(
            data["canonicalSeries"]["rating"]["count"],
            serde_json::json!(1)
        );
    }

    #[tokio::test]
    async fn suwayomi_progress_is_per_user() {
        // CR6: numeric Suwayomi series get per-user read state + resume, mirroring the
        // canonical path — two signed-in users are independent, anonymous is all-unread
        // and cannot persist, and library_progress counts only the viewer's own reads.
        let (s, pool) = setup_full(100).await;
        // Seed a cached numeric series (id 500) with two chapters so `chapters()` reads
        // from the cache (no live source fetch) and `setProgress` can resolve series_id.
        for (cid, num) in [(9001_i64, 1.0_f64), (9002, 2.0)] {
            sqlx::query(
                "INSERT INTO suwayomi_chapter \
                   (id, manga_id, name, chapter_number, is_read, last_page_read, page_count, updated_at) \
                 VALUES (?, 500, ?, ?, 1, 99, 20, '2020-01-01T00:00:00Z')",
            )
            .bind(cid)
            .bind(format!("Chapter {num}"))
            .bind(num)
            .execute(&pool)
            .await
            .unwrap();
        }
        // Both users add series 500 to their library.
        for uid in ["bob-id", "admin-id"] {
            sqlx::query(
                "INSERT INTO user_library (user_id, series_id, created_at) \
                 VALUES (?, '500', '2020-01-01T00:00:00Z')",
            )
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Anonymous cannot persist progress.
        let prog_q = r#"mutation { setProgress(chapterId: "9001", lastPageRead: 12, read: true) }"#;
        let r = exec(&s, prog_q, None, "1.1.1.1").await;
        assert_eq!(first_error(&r), "Not authenticated");

        // Bob reads chapter 9001; admin reads nothing.
        let r = exec(&s, prog_q, Some("bobtok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "setProgress failed: {:?}", r.errors);
        // series_id was resolved from the chapter's cached manga_id.
        let stored: (String, i64, i64) = sqlx::query_as(
            "SELECT series_id, last_page_read, read FROM suwayomi_progress \
             WHERE user_id = 'bob-id' AND chapter_id = '9001'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, ("500".into(), 12, 1));

        // chapters() overlays only the VIEWER's state — despite the cached global
        // is_read=1/last_page_read=99, bob sees his own values and admin sees unread.
        let chapters_q = r#"{ chapters(seriesId: "500") { id read lastPageRead } }"#;
        let read_state = |r: async_graphql::Response, id: &str| {
            r.data.into_json().unwrap()["chapters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .unwrap()
                .clone()
        };
        let bob = read_state(
            exec(&s, chapters_q, Some("bobtok"), "1.1.1.1").await,
            "9001",
        );
        assert_eq!(bob["read"], serde_json::json!(true));
        assert_eq!(bob["lastPageRead"], serde_json::json!(12));
        let admin = read_state(
            exec(&s, chapters_q, Some("admintok"), "1.1.1.1").await,
            "9001",
        );
        assert_eq!(admin["read"], serde_json::json!(false), "admin independent");
        assert_eq!(admin["lastPageRead"], serde_json::json!(0));
        // Anonymous also sees everything unread (cached global flag ignored).
        let anon = read_state(exec(&s, chapters_q, None, "1.1.1.1").await, "9001");
        assert_eq!(anon["read"], serde_json::json!(false));
        assert_eq!(anon["lastPageRead"], serde_json::json!(0));

        // libraryProgress reflects each viewer's own read count; total is 0 (client
        // falls back to chapterCount).
        let lib_q = r#"{ libraryProgress { id read total } }"#;
        let bob_lib = exec(&s, lib_q, Some("bobtok"), "1.1.1.1").await;
        let lp = bob_lib.data.into_json().unwrap()["libraryProgress"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "500")
            .unwrap()
            .clone();
        assert_eq!(lp["read"], serde_json::json!(1));
        assert_eq!(lp["total"], serde_json::json!(0));
        // Admin has no progress rows → series omitted entirely.
        let admin_lib = exec(&s, lib_q, Some("admintok"), "1.1.1.1").await;
        assert!(admin_lib.data.into_json().unwrap()["libraryProgress"]
            .as_array()
            .unwrap()
            .is_empty());

        // A second write updates in place — no duplicate row.
        let r = exec(
            &s,
            r#"mutation { setProgress(chapterId: "9001", lastPageRead: 5, read: false) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let (cnt, lpr, rd): (i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*), MAX(last_page_read), MAX(read) FROM suwayomi_progress \
             WHERE user_id = 'bob-id' AND chapter_id = '9001'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((cnt, lpr, rd), (1, 5, 0), "upsert in place");
    }

    #[tokio::test]
    async fn updates_total_excludes_nsfw_for_opted_out_viewer() {
        // N3: `total`/`has_next` must count only rows the viewer can see, filtered in
        // SQL rather than after the page slice. Seed two series with a new-chapter
        // timestamp — one linked to an NSFW work, one to a SFW work.
        let (s, pool) = setup_full(100).await;
        for (sid, title, nsfw) in [("100", "Safe Feed", false), ("200", "Spicy Feed", true)] {
            let wid = crate::catalog::create_work(
                &pool,
                &crate::catalog::WorkInput {
                    primary_title: Some(title.into()),
                    is_nsfw: nsfw,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            crate::catalog::upsert_source_series(
                &pool, &wid, "suwayomi", "suwayomi", sid, None, nsfw,
            )
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO series_scan_state (series_id, last_new_chapter_at, updated_at) \
                 VALUES (?, '2026-07-10T00:00:00Z', '2026-07-10T00:00:00Z')",
            )
            .bind(sid)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Opted-out (default): total counts only the SFW series (no skew).
        let r = exec(
            &s,
            r#"{ updates { total hasNextPage } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        assert_eq!(
            data["updates"]["total"],
            serde_json::json!(1),
            "N3: nsfw row must not inflate total"
        );
        assert_eq!(data["updates"]["hasNextPage"], serde_json::json!(false));

        // Opted in: both rows count.
        exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let r = exec(&s, r#"{ updates { total } }"#, Some("bobtok"), "1.1.1.1").await;
        let data = r.data.into_json().unwrap();
        assert_eq!(data["updates"]["total"], serde_json::json!(2));
    }

    #[tokio::test]
    async fn auto_merge_ors_source_nsfw_into_existing_work() {
        // N4: an auto-merge onto an existing SFW work must escalate that work to NSFW
        // when the source signals it — every gating read consults `work.is_nsfw`, never
        // `source_series.is_nsfw`, so setting only the latter would leak.
        let pool = migrated_pool().await;
        // A SFW existing work with a title + cover so an exact-title + matching-cover
        // add auto-merges (DD3 requires cover corroboration for auto-merge).
        let existing = crate::catalog::create_work(
            &pool,
            &crate::catalog::WorkInput {
                primary_title: Some("Border Town Tales".into()),
                cover_phash: Some("aabbccddeeff0011".into()),
                is_nsfw: false,
                aliases: vec![crate::catalog::Alias {
                    raw: "Border Town Tales".into(),
                    lang: None,
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Same title, an NSFW genre (source_nsfw = true), identical cover hash.
        let m = suwayomi_manga(55, "Border Town Tales", &["Action", "Hentai"], "src1");
        let r = add_source_series_core(&pool, &m, Some("aabbccddeeff0011".into()))
            .await
            .unwrap();
        assert_eq!(
            r.decision, "auto_merge",
            "expected auto-merge, got {}",
            r.decision
        );
        assert_eq!(r.work_id, existing);
        let nsfw: i64 = sqlx::query_scalar("SELECT is_nsfw FROM work WHERE id = ?")
            .bind(&existing)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            nsfw, 1,
            "N4: source nsfw signal must OR into the merged work"
        );
    }

    #[tokio::test]
    async fn suwayomi_detail_and_reader_paths_gate_nsfw() {
        let (s, pool) = setup_full(100).await;
        // A federated Suwayomi series (id 4242) linked to an NSFW work — the state left
        // by the Tier-2 add flow once it flags a source series as NSFW (3.1). The
        // Suwayomi ids are sequential, so an opted-out viewer could otherwise hand-craft
        // this id to read full detail / chapter list / page images (N2).
        let work_id = crate::catalog::create_work(
            &pool,
            &crate::catalog::WorkInput {
                primary_title: Some("Spicy Suwa".into()),
                is_nsfw: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        crate::catalog::upsert_source_series(
            &pool, &work_id, "suwayomi", "suwayomi", "4242", None, true,
        )
        .await
        .unwrap();

        // Opted-out viewer: detail + chapter list are hidden *before* any source
        // round-trip, so the gate resolves without the (unreachable) Suwayomi server.
        let r = exec(
            &s,
            r#"{ series(id: "4242") { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "No such series");
        let r = exec(
            &s,
            r#"{ chapters(seriesId: "4242") { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "No such series");

        // Opting in passes the gate: the query then fails only because the test
        // Suwayomi server is unreachable, NOT with the NSFW not-found.
        exec(
            &s,
            r#"mutation { setShowNsfw(value: true) }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let r = exec(
            &s,
            r#"{ series(id: "4242") { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_ne!(
            first_error(&r),
            "No such series",
            "opted-in viewer must pass the gate: {:?}",
            r.errors
        );

        // A safe/uncatalogued series is never over-blocked: even for an opted-out
        // viewer (admin defaults to hidden) the gate passes and the resolver proceeds to
        // the (unreachable) source, so the error is not the NSFW not-found.
        let r = exec(
            &s,
            r#"{ series(id: "999999") { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert_ne!(first_error(&r), "No such series");
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
    async fn login_with_unknown_username_is_rejected_uniformly() {
        let s = setup().await;
        // A3: an unknown username returns the same error as a wrong password
        // (and, in prod, after the same constant-work Argon2 verify).
        let r = exec(
            &s,
            r#"mutation { login(username:"nobody-here", password:"password123") { token } }"#,
            None,
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "Invalid username or password");
    }

    #[tokio::test]
    async fn overlong_passwords_are_rejected() {
        let s = setup().await;
        let long = "x".repeat(MAX_PASSWORD_LEN + 1);
        // A7: register rejects an over-long password.
        let q = format!(
            r#"mutation {{ register(input:{{username:"newbie", email:"n@e.com", password:"{long}"}}) {{ token }} }}"#
        );
        let r = exec(&s, &q, None, "1.1.1.1").await;
        assert_eq!(first_error(&r), "password must be at most 1024 characters");
        // A7: login rejects an over-long password without hashing it.
        let q = format!(r#"mutation {{ login(username:"bob", password:"{long}") {{ token }} }}"#);
        let r = exec(&s, &q, None, "1.1.1.1").await;
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
        // admin bans bob -> bob can no longer log in. The mutation now returns
        // the full AdminUser, so `isBanned` is selectable and reflects the write.
        let ban = exec(
            &s,
            r#"mutation { banUser(userId:"bob-id", banned:true) { isBanned username } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(ban.errors.is_empty(), "ban failed: {:?}", ban.errors);
        let ban_data = ban.data.into_json().unwrap();
        assert_eq!(ban_data["banUser"]["isBanned"], serde_json::json!(true));
        assert_eq!(ban_data["banUser"]["username"], serde_json::json!("bob"));
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
    async fn ban_hides_comments_and_reviews() {
        let s = setup().await;
        // Bob posts a comment on a series thread and a review on that series.
        let posted_comment = exec(
            &s,
            r#"mutation { postComment(input:{ targetType:"series", targetId:"s1", body:"great read", hasSpoiler:false }) { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(
            posted_comment.errors.is_empty(),
            "post comment failed: {:?}",
            posted_comment.errors
        );
        let posted_review = exec(
            &s,
            r#"mutation { postReview(input:{ seriesId:"s1", score:9, body:"loved it", hasSpoiler:false }) { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(
            posted_review.errors.is_empty(),
            "post review failed: {:?}",
            posted_review.errors
        );

        // Before the ban both surface, with total == 1.
        let before = exec(
            &s,
            r#"{ comments(targetType:"series", targetId:"s1") { items { id } total }
                reviews(seriesId:"s1") { items { id } total } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let b = before.data.into_json().unwrap();
        assert_eq!(b["comments"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(b["comments"]["total"], serde_json::json!(1));
        assert_eq!(b["reviews"]["items"].as_array().unwrap().len(), 1);
        assert_eq!(b["reviews"]["total"], serde_json::json!(1));

        // Admin bans bob.
        let ban = exec(
            &s,
            r#"mutation { banUser(userId:"bob-id", banned:true) { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(ban.errors.is_empty(), "ban failed: {:?}", ban.errors);

        // After the ban bob's comment and review are hidden server-side and the
        // totals decrement, so the admin's "removed" feedback is truthful on reload.
        let after = exec(
            &s,
            r#"{ comments(targetType:"series", targetId:"s1") { items { id } total }
                reviews(seriesId:"s1") { items { id } total } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        let a = after.data.into_json().unwrap();
        assert!(a["comments"]["items"].as_array().unwrap().is_empty());
        assert_eq!(a["comments"]["total"], serde_json::json!(0));
        assert!(a["reviews"]["items"].as_array().unwrap().is_empty());
        assert_eq!(a["reviews"]["total"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn threaded_replies_nest_and_paginate_by_root() {
        let s = setup().await;
        // Root comment on the series thread.
        let root = exec(
            &s,
            r#"mutation { postComment(input:{ targetType:"series", targetId:"s1", body:"root", hasSpoiler:false }) { id parentId } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(
            root.errors.is_empty(),
            "root post failed: {:?}",
            root.errors
        );
        let root_json = root.data.into_json().unwrap();
        assert_eq!(
            root_json["postComment"]["parentId"],
            serde_json::json!(null)
        );
        let root_id = root_json["postComment"]["id"].as_str().unwrap().to_string();

        // Reply to the root, and a nested reply to that reply (arbitrary depth).
        let reply = exec(
            &s,
            &format!(
                r#"mutation {{ postComment(input:{{ targetType:"series", targetId:"s1", parentId:"{root_id}", body:"reply", hasSpoiler:false }}) {{ id parentId }} }}"#
            ),
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(reply.errors.is_empty(), "reply failed: {:?}", reply.errors);
        let reply_json = reply.data.into_json().unwrap();
        assert_eq!(
            reply_json["postComment"]["parentId"],
            serde_json::json!(root_id)
        );
        let reply_id = reply_json["postComment"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let nested = exec(
            &s,
            &format!(
                r#"mutation {{ postComment(input:{{ targetType:"series", targetId:"s1", parentId:"{reply_id}", body:"nested", hasSpoiler:false }}) {{ id }} }}"#
            ),
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(
            nested.errors.is_empty(),
            "nested failed: {:?}",
            nested.errors
        );

        // The thread query returns all three (flat, ascending) but counts ONE root.
        let list = exec(
            &s,
            r#"{ comments(targetType:"series", targetId:"s1") { items { id parentId body } total hasNextPage } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let lj = list.data.into_json().unwrap();
        assert_eq!(lj["comments"]["items"].as_array().unwrap().len(), 3);
        assert_eq!(
            lj["comments"]["total"],
            serde_json::json!(1),
            "one root thread"
        );
        assert_eq!(lj["comments"]["hasNextPage"], serde_json::json!(false));

        // A reply whose parent lives on a different target is rejected.
        let cross = exec(
            &s,
            &format!(
                r#"mutation {{ postComment(input:{{ targetType:"series", targetId:"s2", parentId:"{root_id}", body:"x", hasSpoiler:false }}) {{ id }} }}"#
            ),
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&cross), "reply target not found on this thread");

        // Deleting the root removes the whole subtree (admin moderation).
        let del = exec(
            &s,
            &format!(r#"mutation {{ deleteComment(commentId:"{root_id}") }}"#),
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(del.errors.is_empty(), "delete failed: {:?}", del.errors);
        assert_eq!(
            del.data.into_json().unwrap()["deleteComment"],
            serde_json::json!(true)
        );
        let after = exec(
            &s,
            r#"{ comments(targetType:"series", targetId:"s1") { items { id } total } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let aj = after.data.into_json().unwrap();
        assert!(
            aj["comments"]["items"].as_array().unwrap().is_empty(),
            "subtree gone"
        );
        assert_eq!(aj["comments"]["total"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn comment_media_links_once_and_rejects_bad_ids() {
        let (s, pool) = setup_full(100).await;
        // Stage an uploaded image row for bob (as POST /comment-media would).
        sqlx::query(
            "INSERT INTO comment_media (id, comment_id, user_id, webp, width, height, created_at) \
             VALUES ('m1', NULL, 'bob-id', X'00', 800, 600, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Empty body AND no media → rejected.
        let empty = exec(
            &s,
            r#"mutation { postComment(input:{ targetType:"series", targetId:"s1", body:"   ", hasSpoiler:false }) { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&empty), "comment must have text or an image");

        // Someone else's / unlinked media id owned by bob: another user can't claim it.
        let steal = exec(
            &s,
            r#"mutation { postComment(input:{ targetType:"series", targetId:"s1", body:"mine now", hasSpoiler:false, mediaId:"m1" }) { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&steal), "attached image not found");

        // Bob attaches his image (image-only comment, empty body allowed).
        let posted = exec(
            &s,
            r#"mutation { postComment(input:{ targetType:"series", targetId:"s1", body:"", hasSpoiler:false, mediaId:"m1" }) { id mediaUrl mediaWidth mediaHeight } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(
            posted.errors.is_empty(),
            "attach failed: {:?}",
            posted.errors
        );
        let pj = posted.data.into_json().unwrap();
        assert_eq!(
            pj["postComment"]["mediaUrl"],
            serde_json::json!("/comment-media/m1.webp")
        );
        assert_eq!(pj["postComment"]["mediaWidth"], serde_json::json!(800));
        assert_eq!(pj["postComment"]["mediaHeight"], serde_json::json!(600));

        // The image is now linked, so it can't be attached again.
        let reuse = exec(
            &s,
            r#"mutation { postComment(input:{ targetType:"series", targetId:"s1", body:"again", hasSpoiler:false, mediaId:"m1" }) { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&reuse), "attached image not found");

        // The thread query surfaces the media on the stored comment.
        let list = exec(
            &s,
            r#"{ comments(targetType:"series", targetId:"s1") { items { mediaUrl mediaWidth } } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let lj = list.data.into_json().unwrap();
        assert_eq!(
            lj["comments"]["items"][0]["mediaUrl"],
            serde_json::json!("/comment-media/m1.webp")
        );
        assert_eq!(
            lj["comments"]["items"][0]["mediaWidth"],
            serde_json::json!(800)
        );
    }

    #[tokio::test]
    async fn my_review_survives_pagination_and_is_null_when_signed_out() {
        let (s, pool) = setup_full(100).await;
        // Bob reviews s1 early (created_at = now, e.g. 2026).
        let posted = exec(
            &s,
            r#"mutation { postReview(input:{ seriesId:"s1", score:7, body:"my early take", hasSpoiler:false }) { id } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(
            posted.errors.is_empty(),
            "post review failed: {:?}",
            posted.errors
        );
        let my_id = posted.data.into_json().unwrap()["postReview"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        // 20 other users each post a NEWER review on s1, so a page-1 query
        // (LIMIT PAGE_SIZE, `created_at DESC`) no longer includes bob's earlier one.
        for i in 0..20 {
            let uid = format!("filler-{i}");
            seed_user(&pool, &uid, &format!("filler{i}"), 0, 0).await;
            sqlx::query(
                "INSERT INTO reviews (id, series_id, user_id, score, body, has_spoiler, created_at, updated_at) \
                 VALUES (?, 's1', ?, 8, 'filler', 0, ?, ?)",
            )
            .bind(format!("rev-{i}"))
            .bind(&uid)
            .bind(format!("2099-01-01T00:00:{i:02}Z"))
            .bind(format!("2099-01-01T00:00:{i:02}Z"))
            .execute(&pool)
            .await
            .unwrap();
        }

        // Page 1 of the public reviews list drops bob's earlier review.
        let page1 = exec(
            &s,
            r#"{ reviews(seriesId:"s1") { items { id } } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        let ids: Vec<String> = page1.data.into_json().unwrap()["reviews"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 20, "page 1 is capped at PAGE_SIZE");
        assert!(
            !ids.contains(&my_id),
            "bob's early review should have fallen off page 1"
        );

        // But myReview retrieves bob's own review by identity, regardless of paging.
        let mine = exec(
            &s,
            r#"{ myReview(seriesId:"s1") { id score body } }"#,
            Some("bobtok"),
            "1.1.1.1",
        )
        .await;
        assert!(mine.errors.is_empty(), "myReview failed: {:?}", mine.errors);
        let m = mine.data.into_json().unwrap();
        assert_eq!(m["myReview"]["id"], serde_json::json!(my_id));
        assert_eq!(m["myReview"]["score"], serde_json::json!(7));
        assert_eq!(m["myReview"]["body"], serde_json::json!("my early take"));

        // A signed-out viewer gets null (and no error) — not `require_user`.
        let anon = exec(&s, r#"{ myReview(seriesId:"s1") { id } }"#, None, "1.1.1.1").await;
        assert!(
            anon.errors.is_empty(),
            "anon myReview should not error: {:?}",
            anon.errors
        );
        assert_eq!(
            anon.data.into_json().unwrap()["myReview"],
            serde_json::Value::Null
        );

        // A signed-in viewer with no review on this series also gets null.
        let admin_none = exec(
            &s,
            r#"{ myReview(seriesId:"s1") { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(admin_none.errors.is_empty());
        assert_eq!(
            admin_none.data.into_json().unwrap()["myReview"],
            serde_json::Value::Null
        );
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
            mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new(
                "test-ua", 5.0, 40.0,
            )),
            admin_users: vec![],
            scan_health: Mutex::new(ScanHealth::default()),
            auth_limiter: RateLimiter::new(100, 60),
            federated_limiter: RateLimiter::new(100, 60),
            session_ttl_secs: 30 * 24 * 60 * 60,
            series_inflight: KeyedLocks::default(),
            chapters_inflight: KeyedLocks::default(),
        });
        let s = build_schema(state, false);
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

    /// Build a minimal `AppState` around a migrated pool (Suwayomi points at a
    /// dead port; `map_series` never dials it, so read-shape tests stay offline).
    fn state_with_pool(pool: SqlitePool) -> std::sync::Arc<AppState> {
        std::sync::Arc::new(AppState {
            pool,
            suwayomi: crate::suwayomi::SuwayomiClient::new("http://127.0.0.1:1".into(), None, None),
            mangadex: std::sync::Arc::new(crate::mangadex::MangaDexClient::new(
                "test-ua", 5.0, 40.0,
            )),
            admin_users: vec![],
            scan_health: Mutex::new(ScanHealth::default()),
            auth_limiter: RateLimiter::new(100, 60),
            federated_limiter: RateLimiter::new(100, 60),
            session_ttl_secs: 30 * 24 * 60 * 60,
            series_inflight: KeyedLocks::default(),
            chapters_inflight: KeyedLocks::default(),
        })
    }

    /// AD1: the raw poll override is exposed nullable, distinct from the folded
    /// effective value. With no admin row `pollEveryMinutesOverride` is null while
    /// `pollEveryMinutes` still reports the folded default (30); once an override
    /// is set the raw field echoes it. A row whose poll column is NULL (only a
    /// sibling override set) must keep the raw poll override null.
    #[tokio::test]
    async fn scan_policy_exposes_raw_poll_override_nullable() {
        let pool = migrated_pool().await;
        let st = state_with_pool(pool.clone());
        let m = suwayomi_manga(3, "AD1 Fixture", &["Action"], "src1");

        // No admin row: override is null, effective folds to the default.
        let scan = map_series(&st, m.clone()).await.scan;
        assert_eq!(scan.poll_every_minutes, 30, "effective folds to default");
        assert_eq!(
            scan.poll_every_minutes_override, None,
            "no admin row => raw poll override is null"
        );

        // A sibling-only override (poll column left NULL) must NOT pin poll.
        sqlx::query(
            "INSERT INTO series_admin \
               (series_id, override_interval_hours, poll_every_minutes, paused_override, status_override, updated_at) \
             VALUES ('3', 12.0, NULL, NULL, NULL, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let scan = map_series(&st, m.clone()).await.scan;
        assert_eq!(
            scan.poll_every_minutes, 30,
            "NULL poll still folds to default"
        );
        assert_eq!(
            scan.poll_every_minutes_override, None,
            "a NULL poll column stays an unset raw override"
        );

        // An explicit poll override echoes the raw value on both fields.
        sqlx::query("UPDATE series_admin SET poll_every_minutes = 45 WHERE series_id = '3'")
            .execute(&pool)
            .await
            .unwrap();
        let scan = map_series(&st, m).await.scan;
        assert_eq!(
            scan.poll_every_minutes, 45,
            "effective reflects the override"
        );
        assert_eq!(
            scan.poll_every_minutes_override,
            Some(45),
            "raw poll override echoes the explicit value"
        );
    }

    /// AD1: saving the whole admin state with no poll override must not create or
    /// pin a `poll_every_minutes` row — a null clears the column rather than
    /// writing the folded default. (Suwayomi hydration of the returned Series is
    /// unreachable in tests, so the mutation surfaces an error, but the upsert has
    /// already committed and is what we assert on.)
    #[tokio::test]
    async fn update_series_admin_null_poll_does_not_pin_override() {
        let (s, pool) = setup_full(100).await;

        // Save with only a sibling override; poll omitted => null.
        let _ = exec(
            &s,
            r#"mutation { updateSeriesAdmin(input:{seriesId:"3", overrideIntervalHours:12}) { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        let poll: Option<i64> =
            sqlx::query_scalar("SELECT poll_every_minutes FROM series_admin WHERE series_id = '3'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            poll, None,
            "a null poll override leaves the column NULL, not 30"
        );

        // An explicit poll override persists the raw value.
        let _ = exec(
            &s,
            r#"mutation { updateSeriesAdmin(input:{seriesId:"3", pollEveryMinutes:45}) { id } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        let poll: Option<i64> =
            sqlx::query_scalar("SELECT poll_every_minutes FROM series_admin WHERE series_id = '3'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(poll, Some(45), "explicit override persists the raw value");
    }

    /// AD2: resolving a merge candidate must be an atomic claim, not a
    /// check-then-write TOCTOU. Once a candidate is accepted, a second resolve of
    /// the same id must fail with the already-resolved error and must NOT re-run
    /// the source_series repoint or the provisional-work delete — the guarded
    /// `WHERE ... AND status='pending'` UPDATE turns the second attempt into a
    /// no-op, so `resolved_at` stays exactly as the first (winning) call left it.
    #[tokio::test]
    async fn resolve_merge_candidate_is_an_atomic_claim() {
        let (s, pool) = setup_full(100).await;

        // Two works: the provisional one the source currently points at, and the
        // canonical target the candidate proposes merging onto.
        for wid in ["w_prov", "w_canon"] {
            sqlx::query(
                "INSERT INTO work (id, is_nsfw, created_at, updated_at) \
                 VALUES (?, 0, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z')",
            )
            .bind(wid)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO source_series (id, work_id, source_type, source_key, created_at) \
             VALUES ('ss1', 'w_prov', 'suwayomi', 'k1', '2020-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO merge_candidate \
             (id, source_series_id, candidate_work_id, score, method, status, created_at) \
             VALUES ('mc1', 'ss1', 'w_canon', 0.8, 'title_exact', 'pending', '2020-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // First resolve (accept): repoints ss1 onto w_canon and drops the now-orphan
        // provisional work.
        let r = exec(
            &s,
            r#"mutation { resolveMergeCandidate(id: "mc1", accept: true) }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert!(r.errors.is_empty(), "first resolve failed: {:?}", r.errors);
        assert!(data_json(&r).contains("\"resolveMergeCandidate\":true"));

        let work_id: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE id = 'ss1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            work_id, "w_canon",
            "accept repoints the source onto the canonical work"
        );
        let prov_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work WHERE id = 'w_prov'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(prov_count, 0, "the orphaned provisional work is deleted");
        let (status, resolved_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, resolved_at FROM merge_candidate WHERE id = 'mc1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "confirmed");
        let resolved_at = resolved_at.expect("resolved_at set on first resolve");

        // Second resolve of the SAME id: the guarded UPDATE matches zero pending
        // rows, so this is rejected and mutates nothing further.
        let r2 = exec(
            &s,
            r#"mutation { resolveMergeCandidate(id: "mc1", accept: true) }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(
            first_error(&r2),
            "This merge candidate is already resolved.",
            "the second resolve is refused"
        );

        // The claim made the second attempt a no-op: work_id unchanged, provisional
        // work still gone, and resolved_at identical to the winning call (a blind
        // re-UPDATE would have stamped a fresh timestamp here).
        let work_id2: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE id = 'ss1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            work_id2, "w_canon",
            "second resolve does not re-repoint the source"
        );
        let resolved_at2: Option<String> =
            sqlx::query_scalar("SELECT resolved_at FROM merge_candidate WHERE id = 'mc1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            resolved_at2.as_deref(),
            Some(resolved_at.as_str()),
            "resolved_at is untouched — the guarded UPDATE claimed nothing the second time"
        );
    }

    // ---- Sources & Extensions admin surface (EXT-1) ------------------------

    fn ext(
        pkg: &str,
        lang: &str,
        installed: bool,
        nsfw: bool,
    ) -> crate::suwayomi::ExtensionListEntry {
        crate::suwayomi::ExtensionListEntry {
            pkg_name: pkg.into(),
            name: pkg.into(),
            lang: lang.into(),
            version_name: "1.0".into(),
            is_installed: installed,
            has_update: false,
            is_nsfw: nsfw,
            icon_url: None,
            repo: None,
        }
    }

    #[test]
    fn filter_extensions_show_nsfw_posture_wins_over_explicit_filter() {
        let list = vec![ext("a", "en", false, false), ext("b", "en", false, true)];
        // Opted-out viewer never sees NSFW — even asking for nsfw:true yields none.
        let out = filter_extensions(list.clone(), false, None, Some(true), false);
        assert!(out.is_empty(), "opted-out viewer sees no NSFW extensions");
        // Opted-out default listing drops the NSFW entry.
        let out = filter_extensions(list.clone(), false, None, None, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pkg_name, "a");
        // Opted-in viewer can filter to NSFW-only.
        let out = filter_extensions(list, false, None, Some(true), true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pkg_name, "b");
    }

    #[test]
    fn filter_extensions_installed_and_lang() {
        let list = vec![
            ext("a", "en", true, false),
            ext("b", "en", false, false),
            ext("c", "ja", true, false),
        ];
        let out = filter_extensions(list.clone(), true, None, None, true);
        assert_eq!(
            out.iter().map(|e| e.pkg_name.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"],
            "installedOnly keeps only installed"
        );
        let out = filter_extensions(list, false, Some("ja"), None, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pkg_name, "c");
    }

    #[test]
    fn summarize_bulk_counts_decisions_and_failures() {
        let mk = |decision: &str| MatchResult {
            decision: decision.into(),
            work_id: "w_x".into(),
            matched_work_id: None,
            score: None,
            method: None,
            source_series_id: "ss_x".into(),
        };
        let entries = vec![
            BulkAddEntry {
                suwayomi_manga_id: ID("1".into()),
                result: Some(mk("new")),
                error: None,
            },
            BulkAddEntry {
                suwayomi_manga_id: ID("2".into()),
                result: Some(mk("auto_merge")),
                error: None,
            },
            BulkAddEntry {
                suwayomi_manga_id: ID("3".into()),
                result: Some(mk("review")),
                error: None,
            },
            BulkAddEntry {
                suwayomi_manga_id: ID("4".into()),
                result: Some(mk("existing")),
                error: None,
            },
            BulkAddEntry {
                suwayomi_manga_id: ID("5".into()),
                result: None,
                error: Some("boom".into()),
            },
        ];
        let r = summarize_bulk(entries);
        assert_eq!(r.total, 5);
        assert_eq!(r.succeeded, 4);
        assert_eq!(r.failed, 1);
        assert_eq!(r.new_works, 1);
        assert_eq!(r.auto_merged, 1);
        assert_eq!(r.queued_for_review, 1);
        assert_eq!(r.already_existing, 1);
    }

    #[test]
    fn federated_source_selection_dedupes_by_pkg_and_gates_nsfw() {
        use crate::suwayomi::SuwayomiSource;
        let src = |id: &str, lang: &str, nsfw: bool, pkg: Option<&str>| SuwayomiSource {
            id: id.into(),
            name: format!("src-{id}"),
            lang: lang.into(),
            is_nsfw: nsfw,
            icon_url: None,
            pkg_name: pkg.map(Into::into),
        };
        // Two per-language MangaDex sources (same pkg) + a separate SFW extension +
        // an NSFW extension + the local source (id "0").
        let sources = vec![
            src("0", "und", false, Some("local")),
            src("md-ja", "ja", true, Some("pkg.mangadex")),
            src("md-en", "en", true, Some("pkg.mangadex")),
            src("pill", "en", false, Some("pkg.pill")),
            src("adult", "en", true, Some("pkg.adult")),
        ];

        // Opted-out viewer: NSFW sources dropped, local dropped, one per pkg.
        let sfw = select_federated_sources(sources.clone(), false);
        let ids: Vec<&str> = sfw.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["pill"],
            "only the SFW non-local extension survives"
        );

        // Opted-in viewer: NSFW allowed, but MangaDex still deduped to ONE source,
        // and the English one wins the per-pkg pick.
        let all = select_federated_sources(sources, true);
        let ids: std::collections::HashSet<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert!(
            ids.contains("md-en"),
            "English MangaDex source is the pkg pick"
        );
        assert!(
            !ids.contains("md-ja"),
            "the other MangaDex language is deduped out"
        );
        assert!(ids.contains("pill") && ids.contains("adult"));
        assert!(!ids.contains("0"), "the local source is never fanned out");
        assert_eq!(all.len(), 3, "one source per extension pkg");
    }

    #[test]
    fn apply_search_filters_by_genre_and_rating() {
        let mk = |title: &str, genres: &[&str], avg: f64, count: i32| Series {
            id: ID(title.into()),
            title: title.into(),
            alt_titles: vec![],
            author: None,
            artist: None,
            description: None,
            genres: genres.iter().map(|g| g.to_string()).collect(),
            r#type: ComicType::Manga,
            status: SeriesStatus::Ongoing,
            cover_url: String::new(),
            source_id: "s".into(),
            chapter_count: 0,
            is_nsfw: false,
            rating: RatingSummary {
                average: avg,
                count,
                distribution: vec![0; 10],
            },
            scan: ScanPolicy {
                avg_interval_hours: 0.0,
                override_interval_hours: None,
                poll_every_minutes: 30,
                paused: false,
                status_override: None,
                paused_override: None,
                poll_every_minutes_override: None,
                last_scanned_at: None,
                next_scan_at: None,
            },
            created_at: String::new(),
            updated_at: String::new(),
        };
        let items = vec![
            mk("Action8", &["Action", "Comedy"], 8.0, 3),
            mk("Drama5", &["Drama"], 5.0, 2),
            mk("Action3", &["Action"], 3.0, 1),
            mk("Unrated", &["Action"], 0.0, 0),
        ];

        // Genre filter (case-insensitive, ANY): only Action titles.
        let out = apply_search_filters(items.clone(), Some(&["action".into()]), None, None);
        let ids: Vec<&str> = out.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(ids, vec!["Action8", "Action3", "Unrated"]);

        // Rating range [4,10] excludes the 3.0 and the unrated (0.0).
        let out = apply_search_filters(items.clone(), None, Some(4.0), Some(10.0));
        let ids: Vec<&str> = out.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(ids, vec!["Action8", "Drama5"]);

        // Combined: Action AND rating >= 4 → only Action8.
        let out = apply_search_filters(items.clone(), Some(&["Action".into()]), Some(4.0), None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Action8");

        // No filters → unchanged.
        assert_eq!(
            apply_search_filters(items.clone(), None, None, None).len(),
            4
        );
    }

    #[test]
    fn group_aggregated_chapters_dedupes_by_number_keeps_sources() {
        // S2: one entry per number, ascending, each keeping every source (translator)
        // that provides it.
        let row = |num: f64, st: &str, sid: &str, mid: Option<&str>, cid: &str| {
            crate::catalog::WorkChapterRow {
                number: num,
                title: Some(format!("Ch {num}")),
                source_type: st.into(),
                source_id: sid.into(),
                suwayomi_manga_id: mid.map(Into::into),
                chapter_id: cid.into(),
                scanlator: None,
            }
        };
        // Number 1 from two sources; 2 from one; 10.5 distinct from 10; out of order.
        let rows = vec![
            row(2.0, "suwayomi", "a", Some("333"), "c2"),
            row(1.0, "suwayomi", "a", Some("333"), "c1"),
            row(1.0, "mangadex", "mangadex", None, "md-uuid"),
            row(10.5, "suwayomi", "a", Some("333"), "c105"),
            row(10.0, "suwayomi", "a", Some("333"), "c10"),
        ];
        let out = group_aggregated_chapters(rows);
        let nums: Vec<f64> = out.iter().map(|c| c.number).collect();
        assert_eq!(nums, vec![1.0, 2.0, 10.0, 10.5], "deduped + ascending");
        // Number 1 kept both sources.
        let first = &out[0];
        assert_eq!(first.sources.len(), 2);
        assert!(first.sources.iter().any(|s| s.source_type == "mangadex"));
        assert!(first
            .sources
            .iter()
            .any(|s| s.suwayomi_manga_id.as_ref().map(|i| i.0.as_str()) == Some("333")));
    }

    #[test]
    fn rank_federated_hits_exact_title_first_stable() {
        // X4: exact-title matches sort first; ties keep incoming (source) order, so
        // the ranking is deterministic regardless of fan-out completion order.
        let mk = |id: i64, title: &str| crate::suwayomi::SuwayomiManga {
            id,
            title: title.into(),
            thumbnail_url: None,
            author: None,
            artist: None,
            description: None,
            genre: vec![],
            status: "ONGOING".into(),
            in_library: false,
            in_library_at: None,
            last_fetched_at: None,
            source_id: "s".into(),
            source: None,
            chapters: None,
        };
        // Incoming already in source order: a fuzzy, then two exact, then fuzzy.
        let mut hits = vec![
            mk(1, "Naruto Gaiden"),
            mk(2, "Naruto"),
            mk(3, "NARUTO"), // normalizes equal to "naruto"
            mk(4, "Boruto"),
        ];
        rank_federated_hits(&mut hits, "naruto");
        let ids: Vec<i64> = hits.iter().map(|m| m.id).collect();
        // The two exact matches (2,3) come first in their original relative order,
        // then the non-exact ones (1,4) in original order.
        assert_eq!(ids, vec![2, 3, 1, 4]);
    }

    #[test]
    fn store_icon_url_derives_store_hosted_icons() {
        // Keiyoushi's canonicalized repo URL → the direct raw.githubusercontent icon.
        assert_eq!(
            store_icon_url(
                "https://github.com/keiyoushi/extensions/raw/repo/index.pb",
                "eu.kanade.tachiyomi.extension.en.foo"
            )
            .as_deref(),
            Some(
                "https://raw.githubusercontent.com/keiyoushi/extensions/repo/icon/eu.kanade.tachiyomi.extension.en.foo.png"
            )
        );
        // The pre-canonicalization min.json form works too.
        assert_eq!(
            store_icon_url(
                "https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.min.json",
                "pkg.x"
            )
            .as_deref(),
            Some("https://raw.githubusercontent.com/keiyoushi/extensions/repo/icon/pkg.x.png")
        );
        // A repo URL that doesn't end in an index file → None (fall back to engine).
        assert_eq!(store_icon_url("https://example.com/store", "pkg.x"), None);
        assert_eq!(store_icon_url("", "pkg.x"), None);
    }

    #[tokio::test]
    async fn series_sources_batch_returns_provenance_in_input_order() {
        let (s, pool) = setup_full(100).await;
        // A canonical work with a MangaDex mapping + a Suwayomi mapping whose
        // source_key is the Suwayomi series id the console holds ("42").
        crate::catalog::upsert_work_from_mangadex(
            &pool,
            "md-y",
            &crate::catalog::WorkInput {
                primary_title: Some("Prov Work".into()),
                is_nsfw: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let work_id: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = 'md-y'")
                .fetch_one(&pool)
                .await
                .unwrap();
        crate::catalog::upsert_source_series(
            &pool, &work_id, "suwayomi", "2048", "42", None, false,
        )
        .await
        .unwrap();
        crate::catalog::upsert_source_extension(
            &pool,
            "2048",
            &crate::catalog::SourceExtensionInput {
                pkg_name: "pkg.prov".into(),
                repo_url: "https://r".into(),
                apk_name: None,
                version_code: None,
                lang: Some("en".into()),
                is_nsfw: false,
            },
        )
        .await
        .unwrap();

        let q = r#"{ seriesSourcesBatch(seriesIds: ["42", "999"]) {
            seriesId workId sources { sourceType sourceId extension { pkgName } } } }"#;
        let r = exec(&s, q, Some("admintok"), "1.1.1.1").await;
        assert!(r.errors.is_empty(), "unexpected: {:?}", r.errors);
        let data = r.data.into_json().unwrap();
        let groups = data["seriesSourcesBatch"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "one group per requested id, in order");
        // Catalogued series: linked work + both mappings, extension joined.
        assert_eq!(groups[0]["seriesId"], serde_json::json!("42"));
        assert_eq!(groups[0]["workId"], serde_json::json!(work_id));
        let sources = groups[0]["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["sourceType"], serde_json::json!("mangadex"));
        assert_eq!(sources[1]["sourceType"], serde_json::json!("suwayomi"));
        assert_eq!(
            sources[1]["extension"]["pkgName"],
            serde_json::json!("pkg.prov")
        );
        // Uncatalogued series: null work, empty sources.
        assert_eq!(groups[1]["seriesId"], serde_json::json!("999"));
        assert_eq!(groups[1]["workId"], serde_json::Value::Null);
        assert!(groups[1]["sources"].as_array().unwrap().is_empty());

        // Admin-gated: a non-admin viewer is refused.
        let r = exec(&s, q, Some("bobtok"), "1.1.1.1").await;
        assert_eq!(first_error(&r), "Admin access required");
    }

    #[tokio::test]
    async fn extension_surface_requires_admin() {
        // Every EXT-1 query/mutation is admin-gated — a signed-in non-admin (and
        // an anonymous caller) is refused before any Suwayomi round-trip (the
        // test client points at a dead port, so passing the gate would error
        // differently).
        let s = setup().await;
        for (q, who) in [
            (r#"{ extensions { pkgName } }"#, Some("bobtok")),
            (r#"{ extensions { pkgName } }"#, None),
            (r#"{ sources { id } }"#, Some("bobtok")),
            (r#"{ sources { id } }"#, None),
            (
                r#"{ sourceBrowse(sourceId: "1", type: POPULAR) { page } }"#,
                Some("bobtok"),
            ),
            (
                r#"mutation { addExtensionRepo(indexUrl: "https://example.com/index.json") }"#,
                Some("bobtok"),
            ),
            (
                r#"mutation { installExtension(pkgName: "x") { pkgName } }"#,
                Some("bobtok"),
            ),
            (
                r#"mutation { uninstallExtension(pkgName: "x") { pkgName } }"#,
                Some("bobtok"),
            ),
            (
                r#"mutation { updateExtension(pkgName: "x") { pkgName } }"#,
                Some("bobtok"),
            ),
            (
                r#"mutation { bulkAddSourceSeries(suwayomiMangaIds: ["1"]) { total } }"#,
                Some("bobtok"),
            ),
            (
                r#"mutation { setSeriesPaused(seriesId: "1", paused: false) { id } }"#,
                Some("bobtok"),
            ),
        ] {
            let r = exec(&s, q, who, "1.1.1.1").await;
            let msg = first_error(&r);
            assert!(
                msg == "Admin access required" || msg == "Not authenticated",
                "expected auth refusal for {q}, got: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn bulk_add_source_series_validates_input_size() {
        let s = setup().await;
        let r = exec(
            &s,
            r#"mutation { bulkAddSourceSeries(suwayomiMangaIds: []) { total } }"#,
            Some("admintok"),
            "1.1.1.1",
        )
        .await;
        assert_eq!(first_error(&r), "suwayomiMangaIds must not be empty");

        let ids = (0..101)
            .map(|i| format!("\"{i}\""))
            .collect::<Vec<_>>()
            .join(",");
        let q =
            format!(r#"mutation {{ bulkAddSourceSeries(suwayomiMangaIds: [{ids}]) {{ total }} }}"#);
        let r = exec(&s, &q, Some("admintok"), "1.1.1.1").await;
        assert_eq!(
            first_error(&r),
            "At most 100 ids per bulkAddSourceSeries call"
        );
    }
}
