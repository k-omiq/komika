mod auth;
mod catalog;
mod config;
mod db;
mod dedup;
mod graphql;
mod mangadex;
mod phash;
mod scanner;
mod suwayomi;

use std::sync::Arc;

use std::net::SocketAddr;

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::{ConnectInfo, FromRef, State},
    http::{header, HeaderMap, HeaderValue, Method},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use config::Config;
use graphql::{build_schema, ApiSchema, AppState, ClientIp, RateLimiter, RequestAuth, ScanHealth};
use suwayomi::SuwayomiClient;

/// Extract a bearer token from the `Authorization` header.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Resolve the client IP for rate-limiting. Behind nginx (see deploy/nginx.conf)
/// the real client is in `X-Forwarded-For` (first hop) or `X-Real-IP`; direct
/// dev connections fall back to the socket peer address.
fn resolve_client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
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
    peer.ip().to_string()
}

/// Combined router state: the GraphQL schema (for `/graphql`) and the DB pool
/// (for the readiness probe). Each handler extracts just the piece it needs.
#[derive(Clone)]
struct RouterState {
    schema: ApiSchema,
    pool: sqlx::SqlitePool,
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

async fn graphql_handler(
    State(schema): State<ApiSchema>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let auth = RequestAuth(bearer(&headers));
    let ip = ClientIp(Some(resolve_client_ip(&headers, peer)));
    schema
        .execute(req.into_inner().data(auth).data(ip))
        .await
        .into()
}

async fn graphiql() -> impl IntoResponse {
    Html(GraphiQLSource::build().endpoint("/graphql").finish())
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

    // Promote configured admin usernames (idempotent, case-insensitive to match
    // the register-time admin check).
    for username in &cfg.admin_users {
        sqlx::query("UPDATE users SET is_admin = 1 WHERE username = ? COLLATE NOCASE")
            .bind(username)
            .execute(&pool)
            .await?;
        tracing::info!(username, "ensured admin");
    }

    let suwayomi = SuwayomiClient::new(
        cfg.suwayomi_url.clone(),
        cfg.suwayomi_public_url.clone(),
        cfg.source_id.clone(),
    );
    let mangadex = Arc::new(mangadex::MangaDexClient::new(
        &cfg.mangadex_user_agent,
        cfg.mangadex_rate_per_sec,
    ));
    let state = Arc::new(AppState {
        pool: pool.clone(),
        suwayomi,
        admin_users: cfg.admin_users.clone(),
        scan_health: std::sync::Mutex::new(ScanHealth::default()),
        auth_limiter: RateLimiter::new(cfg.auth_rate_limit_max, cfg.auth_rate_limit_window_secs),
    });

    // Background adaptive scan scheduler, sharing the same AppState.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    scanner::spawn(state.clone(), cfg.scan_tick_seconds, shutdown_rx);

    // Direct-MangaDex catalogue sync — opt-in (CATALOGUE.md §5). Off unless
    // CATALOGUE_SYNC is set, so the default deployment never hits MangaDex. Recurring:
    // seeds on startup, then incrementally refreshes (updatedAtSince) every interval.
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

    let schema = build_schema(state);

    let origins: Vec<_> = cfg
        .cors_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    let app = Router::new()
        .route("/graphql", get(graphiql).post(graphql_handler))
        .route("/health", get(health))
        .route("/health/ready", get(ready))
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
        .with_state(RouterState { schema, pool });

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
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
    let _ = shutdown_tx.send(true);
}
