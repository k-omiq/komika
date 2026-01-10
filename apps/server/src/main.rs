mod auth;
mod config;
mod db;
mod graphql;
mod scanner;
mod suwayomi;

use std::sync::Arc;

use std::net::SocketAddr;

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderValue, Method},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

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

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "komika_server=info,tower_http=warn".into()),
        )
        .init();

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
    let state = Arc::new(AppState {
        pool,
        suwayomi,
        admin_users: cfg.admin_users.clone(),
        scan_health: std::sync::Mutex::new(ScanHealth::default()),
        auth_limiter: RateLimiter::new(cfg.auth_rate_limit_max, cfg.auth_rate_limit_window_secs),
    });

    // Background adaptive scan scheduler, sharing the same AppState.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    scanner::spawn(state.clone(), cfg.scan_tick_seconds, shutdown_rx);

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
        .with_state(schema);

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

async fn shutdown_signal(shutdown_tx: tokio::sync::watch::Sender<bool>) {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
    let _ = shutdown_tx.send(true);
}
