mod auth;
mod avatar;
mod catalog;
mod config;
mod cover;
mod db;
mod dedup;
mod gc;
mod graphql;
mod ingest;
mod mangadex;
mod media;
mod notify;
mod phash;
mod scanner;
mod series_cache;
mod suwayomi;
mod sync;
mod views;

use std::sync::Arc;

use std::net::{IpAddr, SocketAddr};

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::{ConnectInfo, DefaultBodyLimit, FromRef, Multipart, Path as UrlPath, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use config::Config;
use graphql::{
    build_schema, ApiSchema, AppState, ClientIp, KeyedLocks, RateLimiter, RequestAuth,
    RequestLibraryCache, RequestUserCache, ScanHealth,
};
use suwayomi::SuwayomiClient;

/// Extract a bearer token from the `Authorization` header.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// A CIDR block (IPv4 or IPv6) used to recognize trusted reverse proxies.
#[derive(Clone, Copy, Debug)]
struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse `a.b.c.d/nn`, `a:b::/nn`, or a bare IP (treated as a /32 or /128
    /// host route). Returns `None` for malformed input or an out-of-range prefix.
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (addr, prefix) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p.trim().parse::<u8>().ok()?)),
            None => (s, None),
        };
        let network: IpAddr = addr.trim().parse().ok()?;
        let max = if network.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        if prefix > max {
            return None;
        }
        Some(Cidr { network, prefix })
    }

    /// Whether `ip` falls inside this block. A v4 block never matches a v6
    /// address (and vice versa); callers should canonicalize v4-mapped v6 first.
    fn contains(&self, ip: IpAddr) -> bool {
        match (self.network, ip) {
            (IpAddr::V4(net), IpAddr::V4(addr)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                u32::from(net) & mask == u32::from(addr) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(addr)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                u128::from(net) & mask == u128::from(addr) & mask
            }
            _ => false,
        }
    }
}

/// Parse a list of CIDR strings, logging (and dropping) any that don't parse.
fn parse_trusted_proxies(raw: &[String]) -> Vec<Cidr> {
    raw.iter()
        .filter_map(|s| match Cidr::parse(s) {
            Some(c) => Some(c),
            None => {
                tracing::warn!(cidr = %s, "ignoring malformed TRUSTED_PROXY_CIDRS entry");
                None
            }
        })
        .collect()
}

/// Resolve the client IP for rate-limiting. Client-supplied forwarding headers
/// (`X-Forwarded-For` leftmost hop, then `X-Real-IP`) are honored ONLY when the
/// direct socket peer is a configured trusted proxy (`TRUSTED_PROXY_CIDRS`);
/// otherwise the value is trivially spoofable, so we key on the socket peer. The
/// default (empty allowlist) always uses the peer, matching the shipped compose
/// that publishes `8080` directly with no proxy in front.
fn resolve_client_ip(headers: &HeaderMap, peer: SocketAddr, trusted: &[Cidr]) -> String {
    // Canonicalize v4-mapped v6 (e.g. `::ffff:127.0.0.1`) so a v4 CIDR matches a
    // dual-stack socket peer.
    let peer_ip = peer.ip().to_canonical();
    if trusted.iter().any(|c| c.contains(peer_ip)) {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
        if let Some(rip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            let ip = rip.trim();
            if !ip.is_empty() {
                return ip.to_string();
            }
        }
    }
    peer_ip.to_string()
}

/// Combined router state: the GraphQL schema (for `/graphql`) and the DB pool
/// (for the readiness probe). Each handler extracts just the piece it needs.
#[derive(Clone)]
struct RouterState {
    schema: ApiSchema,
    pool: sqlx::SqlitePool,
    /// Un-replicated cover-cache DB (`work_cover_blob`); served by `serve_cover`.
    /// A newtype (`CoverDb`) distinguishes it from the main `pool` in `FromRef`.
    cover_pool: sqlx::SqlitePool,
    trusted_proxies: Arc<Vec<Cidr>>,
    /// Per-user sliding-window limiter for the CPU-bound upload routes
    /// (`/avatar`, `/comment-media`), so a single account can't flood the
    /// decode/resize/encode blocking pool.
    upload_limiter: Arc<RateLimiter>,
    /// Shared app state — for the Suwayomi image proxy (`serve_suwayomi_image`),
    /// which streams cover/page bytes from the internal loopback engine.
    app_state: Arc<graphql::AppState>,
}
impl FromRef<RouterState> for ApiSchema {
    fn from_ref(s: &RouterState) -> Self {
        s.schema.clone()
    }
}
impl FromRef<RouterState> for sqlx::SqlitePool {
    fn from_ref(s: &RouterState) -> Self {
        s.pool.clone()
    }
}
/// Newtype for the cover-cache pool so a handler can extract it via `State`
/// without colliding with the main-pool `FromRef<RouterState> for SqlitePool`.
#[derive(Clone)]
struct CoverDb(sqlx::SqlitePool);
impl FromRef<RouterState> for CoverDb {
    fn from_ref(s: &RouterState) -> Self {
        CoverDb(s.cover_pool.clone())
    }
}
impl FromRef<RouterState> for Arc<Vec<Cidr>> {
    fn from_ref(s: &RouterState) -> Self {
        s.trusted_proxies.clone()
    }
}
impl FromRef<RouterState> for Arc<RateLimiter> {
    fn from_ref(s: &RouterState) -> Self {
        s.upload_limiter.clone()
    }
}
impl FromRef<RouterState> for Arc<graphql::AppState> {
    fn from_ref(s: &RouterState) -> Self {
        s.app_state.clone()
    }
}

async fn graphql_handler(
    State(schema): State<ApiSchema>,
    State(trusted): State<Arc<Vec<Cidr>>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let auth = RequestAuth(bearer(&headers));
    let ip = ClientIp(Some(resolve_client_ip(&headers, peer, &trusted)));
    // Fresh per-request caches so `current_user` does at most one session lookup and each
    // series' `user_library` row is read at most once, even when many resolvers ask (one
    // per feed item).
    let user_cache = RequestUserCache::default();
    let library_cache = RequestLibraryCache::default();
    schema
        .execute(
            req.into_inner()
                .data(auth)
                .data(ip)
                .data(user_cache)
                .data(library_cache),
        )
        .await
        .into()
}

async fn graphiql() -> impl IntoResponse {
    Html(GraphiQLSource::build().endpoint("/graphql").finish())
}

/// A GraphQL-shaped JSON error body for the REST avatar routes, so the reader's
/// error handling reads the same `message` field it does from `/graphql`.
fn avatar_error(status: StatusCode, message: &str) -> axum::response::Response {
    (status, Json(serde_json::json!({ "message": message }))).into_response()
}

/// `POST /avatar` — authenticated multipart avatar upload. The bytes are decoded,
/// squared, and re-encoded as budgeted lossless WebP (`avatar::process_avatar`),
/// stored as a BLOB in `user_avatars`, and the resulting path stored on the user
/// row. Returns `{ "avatarUrl": "/avatars/<id>.webp?v=<ts>" }`.
async fn upload_avatar(
    State(pool): State<sqlx::SqlitePool>,
    State(limiter): State<Arc<RateLimiter>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> axum::response::Response {
    let Some(tok) = bearer(&headers) else {
        return avatar_error(StatusCode::UNAUTHORIZED, "Not authenticated");
    };
    let user = match auth::user_for_token(&pool, &tok).await {
        Ok(Some(u)) => u,
        Ok(None) => return avatar_error(StatusCode::UNAUTHORIZED, "Not authenticated"),
        Err(e) => {
            tracing::warn!(error = %e, "avatar upload: token lookup failed");
            return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        }
    };
    if limiter.check(&format!("upload:{}", user.id)).is_err() {
        return avatar_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many uploads — please slow down",
        );
    }

    // Take the first file part (the reader sends a single `avatar` field).
    let mut data: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let is_file = field.name() == Some("avatar") || field.file_name().is_some();
                if is_file {
                    match field.bytes().await {
                        Ok(b) => {
                            data = Some(b.to_vec());
                            break;
                        }
                        Err(_) => {
                            return avatar_error(
                                StatusCode::BAD_REQUEST,
                                "Upload too large or could not be read",
                            )
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return avatar_error(StatusCode::BAD_REQUEST, "Malformed upload"),
        }
    }
    let Some(bytes) = data else {
        return avatar_error(StatusCode::BAD_REQUEST, "No image file provided");
    };

    // Decoding + resizing + encoding is CPU-bound: keep it off the async runtime.
    let webp = match tokio::task::spawn_blocking(move || avatar::process_avatar(&bytes)).await {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => return avatar_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => {
            tracing::error!(error = %e, "avatar processing task panicked");
            return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not process image");
        }
    };

    // Millisecond granularity so two uploads within the same second still produce
    // distinct `?v=` values (the served avatar is immutable/1-year-cached).
    let version = chrono::Utc::now().timestamp_millis();
    let now = chrono::Utc::now().to_rfc3339();
    let url = avatar::avatar_url(&user.id, version);
    // Upsert the BLOB and repoint the user row in one transaction so the stored
    // `avatar_url` version can never disagree with the bytes on record.
    let stored = async {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO user_avatars (user_id, webp, version, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET \
               webp = excluded.webp, version = excluded.version, updated_at = excluded.updated_at",
        )
        .bind(&user.id)
        .bind(&webp)
        .bind(version)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE users SET avatar_url = ? WHERE id = ?")
            .bind(&url)
            .bind(&user.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = stored {
        tracing::error!(error = %e, "avatar save failed");
        return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save avatar");
    }
    Json(serde_json::json!({ "avatarUrl": url })).into_response()
}

/// `POST /admin/cover/{work_id}` — ADMIN multipart cover upload. Decodes the image,
/// re-encodes it as a budgeted lossy WebP (`cover::process_cover`), stores it in
/// `work_cover_blob`, flips `work.cover_cached_version`, and clears any recorded
/// `work_cover_issue`. The manual-recovery path for covers the crawl can't process.
/// Returns `{ "coverUrl": "/covers/<work_id>.webp?v=<version>" }`.
async fn upload_cover(
    State(app): State<Arc<graphql::AppState>>,
    State(limiter): State<Arc<RateLimiter>>,
    UrlPath(work_id): UrlPath<String>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> axum::response::Response {
    let Some(tok) = bearer(&headers) else {
        return avatar_error(StatusCode::UNAUTHORIZED, "Not authenticated");
    };
    let user = match auth::user_for_token(&app.pool, &tok).await {
        Ok(Some(u)) => u,
        Ok(None) => return avatar_error(StatusCode::UNAUTHORIZED, "Not authenticated"),
        Err(e) => {
            tracing::warn!(error = %e, "cover upload: token lookup failed");
            return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        }
    };
    if user.is_admin == 0 {
        return avatar_error(StatusCode::FORBIDDEN, "Admin only");
    }
    if limiter.check(&format!("upload:{}", user.id)).is_err() {
        return avatar_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many uploads — please slow down",
        );
    }
    // Guard against an unknown work id (the FK would also reject, but a clear 404 is
    // friendlier and avoids processing an image we can't attach).
    match sqlx::query_scalar::<_, i64>("SELECT 1 FROM work WHERE id = ?")
        .bind(&work_id)
        .fetch_optional(&app.pool)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return avatar_error(StatusCode::NOT_FOUND, "No such work"),
        Err(e) => {
            tracing::warn!(error = %e, "cover upload: work lookup failed");
            return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        }
    }

    let mut data: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let is_file = field.name() == Some("cover") || field.file_name().is_some();
                if is_file {
                    match field.bytes().await {
                        Ok(b) => {
                            data = Some(b.to_vec());
                            break;
                        }
                        Err(_) => {
                            return avatar_error(
                                StatusCode::BAD_REQUEST,
                                "Upload too large or could not be read",
                            )
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return avatar_error(StatusCode::BAD_REQUEST, "Malformed upload"),
        }
    }
    let Some(bytes) = data else {
        return avatar_error(StatusCode::BAD_REQUEST, "No image file provided");
    };

    let webp = match tokio::task::spawn_blocking(move || cover::process_cover(&bytes)).await {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => return avatar_error(StatusCode::BAD_REQUEST, &e.to_string()),
        Err(e) => {
            tracing::error!(error = %e, "cover processing task panicked");
            return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not process image");
        }
    };

    if let Err(e) = cover::put_work_cover(&app.pool, &app.cover_pool, &work_id, &webp).await {
        tracing::error!(error = %e, work_id = %work_id, "cover upload: store failed");
        return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save cover");
    }
    cover::clear_cover_issue(&app.pool, &work_id).await;

    // Report the freshly-versioned URL so the admin UI can show the new cover
    // immediately (cache-busted by the stored version).
    let version =
        sqlx::query_scalar::<_, Option<i64>>("SELECT cover_cached_version FROM work WHERE id = ?")
            .bind(&work_id)
            .fetch_optional(&app.pool)
            .await
            .ok()
            .flatten()
            .flatten();
    let url = match version {
        Some(v) => cover::cover_path(&work_id, v),
        None => format!("/covers/{work_id}.webp"),
    };
    Json(serde_json::json!({ "coverUrl": url })).into_response()
}

/// `GET /avatars/{file}` — serve a stored avatar from `user_avatars`. Immutable +
/// long-cache; the stored `avatar_url` carries a `?v=<ts>` so a new upload busts
/// the cache. `{file}` is `<user_id>.webp`; the id is looked up as a bind param
/// (no path/SQL injection surface), returning 404 for a bad shape or unknown id.
async fn serve_avatar(
    State(pool): State<sqlx::SqlitePool>,
    UrlPath(file): UrlPath<String>,
) -> axum::response::Response {
    let Some(user_id) = file.strip_suffix(".webp") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let webp: Option<Vec<u8>> =
        match sqlx::query_scalar("SELECT webp FROM user_avatars WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(&pool)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "avatar read failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
    match webp {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/webp"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /covers/{work_id}.webp` — public serve of a work cover from `work_cover_blob`.
/// Same BLOB-in-SQLite + immutable-cache model as avatars. On a cache MISS for a
/// MangaDex-anchored work, lazily fetch its MangaDex cover, downscale to a bounded
/// WebP, store it (so it's on our origin from now on), and serve it — "save covers as
/// we fetch". If the lazy fetch/resize fails (or the work has a version but a missing
/// blob), 302-redirect to the MangaDex CDN so the cover still renders; a genuinely
/// coverless work 404s.
async fn serve_cover(
    State(app): State<Arc<graphql::AppState>>,
    State(CoverDb(pool)): State<CoverDb>,
    UrlPath(file): UrlPath<String>,
) -> axum::response::Response {
    let Some(work_id) = file.strip_suffix(".webp") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Fast path: already-cached blob.
    match sqlx::query_scalar::<_, Vec<u8>>("SELECT webp FROM work_cover_blob WHERE work_id = ?")
        .bind(work_id)
        .fetch_optional(&pool)
        .await
    {
        Ok(Some(bytes)) => return webp_cover_response(bytes),
        Ok(None) => {} // fall through to lazy fetch
        Err(e) => {
            tracing::warn!(error = %e, "cover read failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    // Lazy cache: this URL is only built for MangaDex-anchored works, so look up the
    // anchor and materialize the cover on first request.
    let Some((mangadex_id, file_name)) = cover::mangadex_cover_anchor(&app.pool, work_id).await
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cdn_fallback = crate::mangadex::cover_thumb_url(&mangadex_id, &file_name);
    let Some(bytes) = app
        .mangadex
        .cover_thumb_bytes(&mangadex_id, &file_name)
        .await
    else {
        return axum::response::Redirect::temporary(&cdn_fallback).into_response();
    };
    // Decode + resize + WebP encode is CPU-bound — keep it off the async runtime.
    let resized = tokio::task::spawn_blocking(move || cover::process_cover(&bytes)).await;
    match resized {
        Ok(Ok(webp)) => {
            if let Err(e) = cover::put_work_cover(&app.pool, &pool, work_id, &webp).await {
                tracing::warn!(error = %e, work_id, "lazy cover cache store failed");
            } else {
                // This lazy materialization is an "auto success": if the crawl had
                // recorded a cover issue for this work, clear it so it drops out of the
                // admin Bugs panel and stops being excluded from the drainer's SELECTs.
                cover::clear_cover_issue(&app.pool, work_id).await;
            }
            webp_cover_response(webp)
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, work_id, "lazy cover resize failed; redirecting to CDN");
            axum::response::Redirect::temporary(&cdn_fallback).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, work_id, "lazy cover resize task panicked");
            axum::response::Redirect::temporary(&cdn_fallback).into_response()
        }
    }
}

/// Whether `rest` — the path tail after `/api/v1/manga/` — is one of the exactly two
/// Suwayomi image endpoints we proxy: `{id}/thumbnail` (a cover) or
/// `{mangaId}/chapter/{chapterIndex}/page/{pageIndex}` (a page). Every id segment must
/// be numeric. This keeps `serve_suwayomi_image` from becoming a general Suwayomi REST
/// proxy — the GraphQL/library/mutation surface on `:4567` stays unreachable.
fn is_suwayomi_image_path(rest: &str) -> bool {
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match rest.split('/').collect::<Vec<_>>().as_slice() {
        [id, "thumbnail"] => numeric(id),
        [mid, "chapter", ci, "page", pi] => numeric(mid) && numeric(ci) && numeric(pi),
        _ => false,
    }
}

/// `GET /api/v1/manga/{id}/thumbnail` and
/// `GET /api/v1/manga/{mangaId}/chapter/{chapterIndex}/page/{pageIndex}` — public proxy
/// for a Suwayomi-source cover or chapter page.
///
/// Suwayomi (`localhost:4567`) is not publicly exposed, so the browser can't reach its
/// image endpoints. Cover/page URLs are built against THIS origin (set
/// `SUWAYOMI_PUBLIC_URL=https://api.komiq.cc`) and we fetch the bytes internally
/// (`suwayomi.fetch_image`, which uses the loopback host) and re-serve them with an
/// immutable cache — covers load directly from here in the admin + reader, and page
/// images ride the image Worker's edge cache (which allowlists this host). Only the two
/// numeric image paths above are honoured (`is_suwayomi_image_path`); anything else 404s.
async fn serve_suwayomi_image(
    State(app): State<Arc<graphql::AppState>>,
    State(CoverDb(covers)): State<CoverDb>,
    UrlPath(rest): UrlPath<String>,
) -> axum::response::Response {
    if !is_suwayomi_image_path(&rest) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Covers (`{id}/thumbnail`) are downscaled to a bounded WebP and origin-cached —
    // the feeds serve these full-resolution (avg ~0.8 MB, up to several MB) at card
    // size, so resizing is a ~40x transfer win. Chapter PAGES are served raw: the
    // reader needs full-resolution page images.
    if let Some(manga_id) = thumbnail_manga_id(&rest) {
        return serve_suwayomi_cover(&app, &covers, manga_id).await;
    }
    let path = format!("/api/v1/manga/{rest}");
    match app.suwayomi.fetch_image(&path).await {
        Ok((bytes, content_type)) => raw_image_response(content_type, bytes),
        Err(e) => {
            tracing::warn!(error = %e, path = %path, "suwayomi image proxy failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// Numeric manga id if `rest` (the tail after `/api/v1/manga/`) is a cover-thumbnail
/// path (`{id}/thumbnail`), else `None` (a chapter page). `is_suwayomi_image_path`
/// has already validated the shape + numeric id, so the parse is infallible here.
fn thumbnail_manga_id(rest: &str) -> Option<i64> {
    match rest.split('/').collect::<Vec<_>>().as_slice() {
        [id, "thumbnail"] => id.parse::<i64>().ok(),
        _ => None,
    }
}

/// Serve a Suwayomi source cover as a bounded WebP: the origin-cached blob when
/// present, else a live fetch + downscale (`cover::process_cover`) that is cached for
/// next time. If the source can't be resized (too large / undecodable) the raw bytes
/// are served so the cover still renders — just unoptimized, and not cached so the
/// resize is retried on a later request.
async fn serve_suwayomi_cover(
    app: &graphql::AppState,
    covers: &sqlx::SqlitePool,
    manga_id: i64,
) -> axum::response::Response {
    if let Some(webp) = cover::get_suwayomi_cover(covers, manga_id).await {
        return webp_cover_response(webp);
    }
    let path = format!("/api/v1/manga/{manga_id}/thumbnail");
    match app.suwayomi.fetch_image(&path).await {
        Ok((bytes, content_type)) => {
            // Decode + up to 5x Lanczos3 resize + lossless WebP encode is CPU-bound
            // (~130ms even at opt-level=3): keep it off the async runtime like the
            // avatar/comment handlers do. On failure the closure hands the original
            // bytes back so the cover still renders raw.
            let resized = tokio::task::spawn_blocking(move || {
                cover::process_cover(&bytes).map_err(|e| (e.to_string(), bytes))
            })
            .await;
            match resized {
                Ok(Ok(webp)) => {
                    if let Err(e) = cover::put_suwayomi_cover(covers, manga_id, &webp).await {
                        tracing::warn!(error = %e, manga_id, "suwayomi cover cache store failed");
                    }
                    webp_cover_response(webp)
                }
                Ok(Err((err, bytes))) => {
                    tracing::warn!(error = %err, manga_id, "suwayomi cover resize failed; serving raw");
                    raw_image_response(content_type, bytes)
                }
                Err(e) => {
                    tracing::error!(error = %e, manga_id, "suwayomi cover resize task panicked");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path, "suwayomi cover proxy failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// Immutable-cached response for a resized WebP cover.
fn webp_cover_response(bytes: Vec<u8>) -> axum::response::Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("image/webp")),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// Immutable-cached response for a raw proxied image (chapter pages, un-resizable
/// covers) — passes the upstream `Content-Type` through.
fn raw_image_response(content_type: String, bytes: Vec<u8>) -> axum::response::Response {
    let ct = HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| HeaderValue::from_static("image/jpeg"));
    (
        [
            (header::CONTENT_TYPE, ct),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// `POST /comment-media` — authenticated multipart upload of one image to attach to
/// a comment. The bytes are decoded, downscaled (aspect-preserving) and re-encoded
/// as budgeted lossless WebP (`media::process_comment_image`), then stored as a
/// staged BLOB in `comment_media` (with `comment_id` NULL, owned by the uploader).
/// Returns `{ "mediaId", "url", "width", "height" }`; the client passes `mediaId`
/// to `postComment`, which links the row to the new comment. Unlinked rows are
/// drafts the user never posted.
async fn upload_comment_media(
    State(pool): State<sqlx::SqlitePool>,
    State(limiter): State<Arc<RateLimiter>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> axum::response::Response {
    let Some(tok) = bearer(&headers) else {
        return avatar_error(StatusCode::UNAUTHORIZED, "Not authenticated");
    };
    let user = match auth::user_for_token(&pool, &tok).await {
        Ok(Some(u)) => u,
        Ok(None) => return avatar_error(StatusCode::UNAUTHORIZED, "Not authenticated"),
        Err(e) => {
            tracing::warn!(error = %e, "comment media upload: token lookup failed");
            return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        }
    };
    if limiter.check(&format!("upload:{}", user.id)).is_err() {
        return avatar_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many uploads — please slow down",
        );
    }
    // Cap the number of staged (unattached) uploads a user can accumulate, so the
    // GC-eligible backlog can't be inflated faster than it's swept. 20 pending
    // drafts is far more than any real compose flow needs.
    let staged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM comment_media WHERE user_id = ? AND comment_id IS NULL",
    )
    .bind(&user.id)
    .fetch_one(&pool)
    .await
    .unwrap_or(0);
    if staged >= 20 {
        return avatar_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many pending image uploads — post or discard some first",
        );
    }

    // Take the first file part (the reader sends a single `image` field).
    let mut data: Option<Vec<u8>> = None;
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let is_file = field.name() == Some("image") || field.file_name().is_some();
                if is_file {
                    match field.bytes().await {
                        Ok(b) => {
                            data = Some(b.to_vec());
                            break;
                        }
                        Err(_) => {
                            return avatar_error(
                                StatusCode::BAD_REQUEST,
                                "Upload too large or could not be read",
                            )
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(_) => return avatar_error(StatusCode::BAD_REQUEST, "Malformed upload"),
        }
    }
    let Some(bytes) = data else {
        return avatar_error(StatusCode::BAD_REQUEST, "No image file provided");
    };

    // Decoding + resizing + encoding is CPU-bound: keep it off the async runtime.
    let processed =
        match tokio::task::spawn_blocking(move || media::process_comment_image(&bytes)).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return avatar_error(StatusCode::BAD_REQUEST, &e.to_string()),
            Err(e) => {
                tracing::error!(error = %e, "comment media processing task panicked");
                return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not process image");
            }
        };

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let stored = sqlx::query(
        "INSERT INTO comment_media (id, comment_id, user_id, webp, width, height, created_at) \
         VALUES (?, NULL, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&processed.webp)
    .bind(processed.width as i64)
    .bind(processed.height as i64)
    .bind(&now)
    .execute(&pool)
    .await;
    if let Err(e) = stored {
        tracing::error!(error = %e, "comment media save failed");
        return avatar_error(StatusCode::INTERNAL_SERVER_ERROR, "Could not save image");
    }
    Json(serde_json::json!({
        "mediaId": id,
        "url": media::comment_media_url(&id),
        "width": processed.width,
        "height": processed.height,
    }))
    .into_response()
}

/// `GET /comment-media/{file}` — serve a stored comment image from `comment_media`.
/// Immutable + long-cache: the id is a random uuid, so the bytes never change.
/// `{file}` is `<id>.webp`; the id is looked up as a bind param (no injection
/// surface), returning 404 for a bad shape or unknown id.
async fn serve_comment_media(
    State(pool): State<sqlx::SqlitePool>,
    UrlPath(file): UrlPath<String>,
) -> axum::response::Response {
    let Some(id) = file.strip_suffix(".webp") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let webp: Option<Vec<u8>> =
        match sqlx::query_scalar("SELECT webp FROM comment_media WHERE id = ?")
            .bind(id)
            .fetch_optional(&pool)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "comment media read failed");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
    match webp {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/webp"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Liveness: cheap, dependency-free — used by the container HEALTHCHECK. A blip
/// in the DB must not flap this (that would trigger restart loops).
async fn health() -> &'static str {
    "ok"
}

/// Readiness: verifies the process can actually serve — i.e. the DB answers.
/// For a load balancer / orchestrator readiness gate, not the liveness probe.
async fn ready(State(pool): State<sqlx::SqlitePool>) -> impl IntoResponse {
    match sqlx::query("SELECT 1").execute(&pool).await {
        Ok(_) => (axum::http::StatusCode::OK, "ready"),
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            (axum::http::StatusCode::SERVICE_UNAVAILABLE, "not ready")
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "komika_server=info,tower_http=info".into());
    // LOG_FORMAT=json emits structured JSON logs for prod log aggregation;
    // anything else keeps the human-readable formatter for local dev.
    let json_logs = std::env::var("LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let cfg = Config::from_env();
    tracing::info!(?cfg, "starting komika-server");

    let pool = db::init(&cfg.database_url).await?;
    // Separate, un-replicated DB for cover blobs — see `db::init_covers`. Kept out
    // of the main DB so Litestream ships only accounts/social/catalogue to R2, never
    // the large (re-derivable) cover thumbnails.
    let cover_pool = db::init_covers(&cfg.covers_database_url).await?;

    // DR reconciliation: if the covers DB is empty (e.g. a fresh host that restored
    // the main DB from R2 but has no covers.sqlite3), clear any surviving
    // `cover_cached_version` pointers so cover URLs fall back to the Worker proxy
    // until the drainer re-materializes them — otherwise those pointers would 404.
    // Cheap no-op in steady state (covers DB non-empty → skip the UPDATE).
    let cover_blob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_cover_blob")
        .fetch_one(&cover_pool)
        .await?;
    if cover_blob_count == 0 {
        let cleared = sqlx::query(
            "UPDATE work SET cover_cached_version = NULL WHERE cover_cached_version IS NOT NULL",
        )
        .execute(&pool)
        .await?;
        if cleared.rows_affected() > 0 {
            tracing::warn!(
                cleared = cleared.rows_affected(),
                "covers DB empty — cleared stale cover version pointers; covers will re-cache"
            );
        }
    }

    // Provision/promote configured admin accounts (idempotent). This is the only
    // path to admin: `register` reserves these names and never grants admin.
    graphql::provision_admins(
        &pool,
        &cfg.admin_users,
        cfg.admin_password.0.as_deref(),
        cfg.admin_email.as_deref(),
    )
    .await?;

    let suwayomi = SuwayomiClient::new(
        cfg.suwayomi_url.clone(),
        cfg.suwayomi_public_url.clone(),
        cfg.source_id.clone(),
    );
    let mangadex = Arc::new(mangadex::MangaDexClient::new(
        &cfg.mangadex_user_agent,
        cfg.mangadex_rate_per_sec,
        cfg.mangadex_athome_per_min,
    ));
    let state = Arc::new(AppState {
        pool: pool.clone(),
        cover_pool: cover_pool.clone(),
        suwayomi,
        mangadex: mangadex.clone(),
        admin_users: cfg.admin_users.clone(),
        scan_health: std::sync::Mutex::new(ScanHealth::default()),
        auth_limiter: RateLimiter::new(cfg.auth_rate_limit_max, cfg.auth_rate_limit_window_secs),
        federated_limiter: RateLimiter::new(
            cfg.federated_rate_limit_max,
            cfg.federated_rate_limit_window_secs,
        ),
        session_ttl_secs: cfg.session_ttl_secs,
        series_inflight: KeyedLocks::default(),
        chapters_inflight: KeyedLocks::default(),
        cover_crawl_running: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        catalogue_cover_phash: cfg.catalogue_cover_phash,
    });

    // Startup recovery: an ingest job still `running` was interrupted by the
    // previous shutdown — its task is gone, so mark it failed (otherwise the
    // partial unique index blocks new jobs for that source forever).
    match ingest::mark_interrupted_jobs(&pool).await {
        Ok(0) => {}
        Ok(n) => tracing::warn!(n, "marked interrupted source-ingest jobs as failed"),
        Err(e) => tracing::warn!(error = %e, "failed to sweep interrupted ingest jobs"),
    }

    // Background adaptive scan scheduler, sharing the same AppState.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    scanner::spawn(state.clone(), cfg.scan_tick_seconds, shutdown_rx);

    // Extension-level source-sync scheduler: re-walks *subscribed* extensions (LATEST)
    // to auto-discover newly-added series, and (independently of subscriptions) reconciles
    // library membership + backfills scan-state rows once per interval. Discovery is a
    // no-op with nothing subscribed, but the reconcile still runs — throttled to once per
    // interval so restarts don't re-trigger it (see `sync::run_loop`).
    sync::spawn(
        state.clone(),
        cfg.source_sync_interval_seconds,
        cfg.source_sync_max_pages,
        shutdown_tx.subscribe(),
    );

    // Hourly garbage collection of orphaned staged comment-media uploads (rows the
    // uploader never attached to a comment), so the BLOB store can't grow unbounded.
    gc::spawn(pool.clone(), shutdown_tx.subscribe());

    // Direct-MangaDex catalogue sync — opt-in (CATALOGUE.md §5). Off unless
    // CATALOGUE_SYNC is set, so the default deployment never hits MangaDex. Recurring:
    // seeds on startup, then incrementally refreshes (updatedAtSince) every interval.
    //
    // FLEET CONSTRAINT (M4, CATALOGUE.md §9): the MangaDex rate limiter is
    // in-process, so it only bounds THIS process. Enable CATALOGUE_SYNC on exactly
    // one replica — N replicas syncing = N× the shared-IP budget → 429/ban. Moving
    // to a shared (DB/Redis) limiter is the prerequisite for multi-replica sync.
    if cfg.catalogue_sync_enabled {
        mangadex::spawn_recurring(
            pool.clone(),
            mangadex.clone(),
            cfg.catalogue_cover_phash,
            cfg.catalogue_sync_interval_secs,
            shutdown_tx.subscribe(),
        );
    } else {
        tracing::info!("catalogue sync disabled (set CATALOGUE_SYNC=on to enable)");
    }

    // Recurring auto-enrichment drainer (X1) — opt-in. Keeps newly-ingested
    // MangaDex-anchored works self-enriching (S2 metadata + F2 covers) in small
    // polite batches; shares the MangaDex rate limiter.
    if cfg.metadata_backfill_enabled {
        graphql::spawn_metadata_backfill(
            state.clone(),
            cfg.metadata_backfill_interval_secs,
            cfg.metadata_backfill_batch,
            shutdown_tx.subscribe(),
        );
    } else {
        tracing::info!("metadata auto-enrichment disabled (set METADATA_BACKFILL=on to enable)");
    }

    // Automatic cover-cache drainer — populates the DB cover cache (`/covers/…`,
    // off the CF image Worker) with NO manual trigger. Defaults ON; hits MangaDex
    // under the shared limiter, so keep it to one replica (COVER_CACHE=off elsewhere).
    if cfg.cover_cache_enabled {
        cover::spawn(
            pool.clone(),
            cover_pool.clone(),
            mangadex.clone(),
            state.suwayomi.clone(),
            state.cover_crawl_running.clone(),
            cfg.cover_cache_interval_secs,
            cfg.cover_cache_batch,
            shutdown_tx.subscribe(),
        );
    } else {
        tracing::info!("cover cache drainer disabled (COVER_CACHE=off)");
    }

    let schema = build_schema(state.clone(), !cfg.graphiql_enabled);

    let origins: Vec<_> = cfg
        .cors_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    // GraphiQL (GET /graphql) is served only when explicitly enabled; otherwise a
    // GET is a 405 and only the POST endpoint exists. Introspection is disabled in
    // the same off state (see build_schema above).
    let graphql_route = if cfg.graphiql_enabled {
        get(graphiql).post(graphql_handler)
    } else {
        post(graphql_handler)
    };

    let app = Router::new()
        .route("/graphql", graphql_route)
        .route("/health", get(health))
        .route("/health/ready", get(ready))
        // Authenticated avatar upload + public serve (VM data volume). The upload
        // route raises the body limit above the raw-image cap enforced in
        // `avatar::process_avatar` (axum's default is 2 MB).
        .route(
            "/avatar",
            post(upload_avatar).layer(DefaultBodyLimit::max(
                avatar::MAX_UPLOAD_BYTES + 1024 * 1024,
            )),
        )
        .route("/avatars/{file}", get(serve_avatar))
        // Public serve of DB-backed work covers (WebP BLOB in `work_cover_blob`).
        // Lets the web reader load covers from our own origin instead of the CF
        // image Worker; the bytes are materialized by `cover::crawl_uncached_covers`
        // (admin `materializeCatalogueCovers`).
        .route("/covers/{file}", get(serve_cover))
        // Admin: replace ONE work's cover from an uploaded image (the "Bugs" panel's
        // manual-upload action for covers the crawl can't process). Same multipart
        // model as avatars; body limit raised above the raw-image cap.
        .route(
            "/admin/cover/{work_id}",
            post(upload_cover).layer(DefaultBodyLimit::max(cover::MAX_SOURCE_BYTES + 1024 * 1024)),
        )
        // Public proxy for Suwayomi-source cover thumbnails + chapter pages. Suwayomi
        // is loopback-only, so its image endpoints are unreachable from the browser;
        // these URLs point HERE (SUWAYOMI_PUBLIC_URL=https://api.komiq.cc) and we fetch
        // the bytes internally. Restricted to the two numeric image paths
        // (`is_suwayomi_image_path`) so it is not a general Suwayomi REST proxy.
        .route("/api/v1/manga/{*rest}", get(serve_suwayomi_image))
        // Authenticated comment-image upload + public serve, same BLOB-in-SQLite
        // model as avatars. The upload route raises the body limit above the raw
        // image cap enforced in `media::process_comment_image`.
        .route(
            "/comment-media",
            post(upload_comment_media)
                .layer(DefaultBodyLimit::max(media::MAX_UPLOAD_BYTES + 1024 * 1024)),
        )
        .route("/comment-media/{file}", get(serve_comment_media))
        // Request-id + access-log span. SetRequestId runs first (generates an
        // x-request-id when the client didn't send one), TraceLayer's span picks
        // it up, and PropagateRequestId echoes it back on the response.
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &axum::http::Request<axum::body::Body>| {
                            let request_id = request
                                .headers()
                                .get("x-request-id")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("-");
                            tracing::info_span!(
                                "request",
                                method = %request.method(),
                                uri = %request.uri(),
                                request_id = %request_id,
                            )
                        })
                        .on_response(DefaultOnResponse::new().level(Level::INFO)),
                )
                .layer(PropagateRequestIdLayer::x_request_id()),
        )
        .layer(cors)
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static(
                "geolocation=(), camera=(), microphone=(), payment=(), usb=()",
            ),
        ))
        // Outermost: a panicking resolver/handler becomes a 500 (logged) instead
        // of dropping the connection or killing the worker task.
        .layer(CatchPanicLayer::custom(handle_panic))
        .with_state(RouterState {
            schema,
            pool,
            cover_pool,
            trusted_proxies: Arc::new(parse_trusted_proxies(&cfg.trusted_proxy_cidrs)),
            // ~20 uploads/min per user across the avatar + comment-media routes.
            upload_limiter: Arc::new(RateLimiter::new(20, 60)),
            app_state: state,
        });

    let addr = format!("0.0.0.0:{}", cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}/graphql");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_tx))
    .await?;
    Ok(())
}

/// Turn a caught panic into a JSON 500 (GraphQL-shaped) and log it with the
/// panic payload, so a single bad request can't take the server down.
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let details = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        "unknown panic".to_string()
    };
    tracing::error!(panic = %details, "caught panic; returning 500");
    let body = r#"{"data":null,"errors":[{"message":"Internal Server Error"}]}"#;
    axum::http::Response::builder()
        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .expect("static panic response is valid")
}

async fn shutdown_signal(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutting down");
    let _ = shutdown_tx.send(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdrs(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn peer(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 12345)
    }

    #[test]
    fn suwayomi_image_path_allows_only_cover_and_page() {
        // The two proxied image endpoints (tail after `/api/v1/manga/`).
        assert!(is_suwayomi_image_path("123/thumbnail"));
        assert!(is_suwayomi_image_path("123/chapter/4/page/0"));
        assert!(is_suwayomi_image_path("1/chapter/0/page/25"));
        // Everything else must be rejected so this is not a general Suwayomi REST
        // proxy — the GraphQL/library/mutation surface on :4567 stays unreachable.
        assert!(!is_suwayomi_image_path("123")); // bare manga
        assert!(!is_suwayomi_image_path("123/chapters")); // chapter list
        assert!(!is_suwayomi_image_path("abc/thumbnail")); // non-numeric id
        assert!(!is_suwayomi_image_path("123/thumbnail/../../graphql")); // traversal
        assert!(!is_suwayomi_image_path("123/chapter/4/page")); // missing index
        assert!(!is_suwayomi_image_path("123/chapter/x/page/0")); // non-numeric chapter
        assert!(!is_suwayomi_image_path("")); // empty
        assert!(!is_suwayomi_image_path("123/thumbnail/")); // trailing slash → empty seg
    }

    #[test]
    fn cidr_ipv4_contains() {
        let c = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(c.contains("10.1.2.3".parse().unwrap()));
        assert!(c.contains("10.255.255.255".parse().unwrap()));
        assert!(!c.contains("11.0.0.1".parse().unwrap()));
        assert!(!c.contains("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn cidr_bare_ip_is_host_route() {
        let c = Cidr::parse("127.0.0.1").unwrap();
        assert!(c.contains("127.0.0.1".parse().unwrap()));
        assert!(!c.contains("127.0.0.2".parse().unwrap()));
    }

    #[test]
    fn cidr_ipv6_and_cross_family() {
        let c = Cidr::parse("2001:db8::/32").unwrap();
        assert!(c.contains("2001:db8::1".parse().unwrap()));
        assert!(!c.contains("2001:db9::1".parse().unwrap()));
        // a v4 block never matches a v6 address
        let v4 = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(!v4.contains("::1".parse().unwrap()));
    }

    #[test]
    fn cidr_rejects_malformed() {
        assert!(Cidr::parse("not-an-ip").is_none());
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("::1/129").is_none());
    }

    #[test]
    fn untrusted_peer_ignores_forwarded_headers() {
        // No trusted proxies configured (the default): the socket peer wins and a
        // spoofed X-Forwarded-For cannot move the rate-limit key.
        let h = hdrs(&[("x-forwarded-for", "1.2.3.4"), ("x-real-ip", "5.6.7.8")]);
        let ip = resolve_client_ip(&h, peer("203.0.113.9"), &[]);
        assert_eq!(ip, "203.0.113.9");
    }

    #[test]
    fn trusted_peer_honors_xff_leftmost() {
        let trusted = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        let h = hdrs(&[("x-forwarded-for", "1.2.3.4, 10.0.0.5")]);
        let ip = resolve_client_ip(&h, peer("10.0.0.5"), &trusted);
        assert_eq!(ip, "1.2.3.4");
    }

    #[test]
    fn trusted_peer_falls_back_to_x_real_ip() {
        let trusted = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        let h = hdrs(&[("x-real-ip", "9.9.9.9")]);
        let ip = resolve_client_ip(&h, peer("10.0.0.5"), &trusted);
        assert_eq!(ip, "9.9.9.9");
    }

    #[test]
    fn trusted_peer_without_headers_uses_peer() {
        let trusted = vec![Cidr::parse("10.0.0.0/8").unwrap()];
        let ip = resolve_client_ip(&HeaderMap::new(), peer("10.0.0.5"), &trusted);
        assert_eq!(ip, "10.0.0.5");
    }

    #[test]
    fn v4_mapped_v6_peer_matches_v4_cidr() {
        let trusted = vec![Cidr::parse("127.0.0.0/8").unwrap()];
        let h = hdrs(&[("x-forwarded-for", "1.2.3.4")]);
        // A dual-stack socket may present a v4 peer as `::ffff:127.0.0.1`.
        let ip = resolve_client_ip(&h, peer("::ffff:127.0.0.1"), &trusted);
        assert_eq!(ip, "1.2.3.4");
    }
}
