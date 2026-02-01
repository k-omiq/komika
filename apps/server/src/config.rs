use std::env;

/// Runtime configuration, sourced from env vars (see `.env.example`).
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub database_url: String,
    pub suwayomi_url: String,
    /// Publicly reachable base for Suwayomi image URLs (covers + pages). GraphQL
    /// federation uses the internal `suwayomi_url`, but image URLs are handed to
    /// the browser, so behind Docker/compose they must use a public host, not the
    /// internal `suwayomi:4567`. Falls back to `suwayomi_url` when unset.
    pub suwayomi_public_url: Option<String>,
    /// Pin a specific Suwayomi source id; otherwise an English source is auto-picked.
    pub source_id: Option<String>,
    /// Origins allowed by CORS (the reader dev/preview servers by default).
    pub cors_origins: Vec<String>,
    /// Usernames that are granted admin (for the "manga DB" console). Promoted at
    /// startup and on registration.
    pub admin_users: Vec<String>,
    /// How often the background scan scheduler wakes to re-evaluate the library.
    pub scan_tick_seconds: u64,
    /// Max failed login/register attempts per key within the rate-limit window
    /// before further attempts are rejected.
    pub auth_rate_limit_max: u32,
    /// Sliding window (seconds) over which `auth_rate_limit_max` is counted.
    pub auth_rate_limit_window_secs: u64,
    /// Enable the direct-MangaDex catalogue sync (CATALOGUE.md §5). Off by default —
    /// nothing hits MangaDex unless `CATALOGUE_SYNC=on`.
    pub catalogue_sync_enabled: bool,
    /// Global request budget for the MangaDex crawl (fleet-wide; shared egress IP).
    /// MangaDex's per-IP ceiling is ~5 req/s.
    pub mangadex_rate_per_sec: f64,
    /// User-Agent sent to MangaDex (required by their API).
    pub mangadex_user_agent: String,
}

impl Config {
    pub fn from_env() -> Self {
        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);
        let database_url =
            env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://komika.sqlite3".to_string());
        let suwayomi_url =
            env::var("SUWAYOMI_URL").unwrap_or_else(|_| "http://localhost:4567".to_string());
        let suwayomi_public_url = env::var("SUWAYOMI_PUBLIC_URL")
            .ok()
            .filter(|s| !s.is_empty());
        let source_id = env::var("SUWAYOMI_SOURCE_ID")
            .ok()
            .filter(|s| !s.is_empty());
        let cors_origins = env::var("CORS_ORIGINS")
            .unwrap_or_else(|_| {
                "http://localhost:5173,http://localhost:4173,http://tauri.localhost".to_string()
            })
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let admin_users = env::var("KOMIKA_ADMIN_USERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let scan_tick_seconds = env::var("SCAN_TICK_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(300);
        let auth_rate_limit_max = env::var("AUTH_RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(10);
        let auth_rate_limit_window_secs = env::var("AUTH_RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(300);
        let catalogue_sync_enabled = env::var("CATALOGUE_SYNC")
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                v == "on" || v == "1" || v == "true"
            })
            .unwrap_or(false);
        let mangadex_rate_per_sec = env::var("MANGADEX_RATE_PER_SEC")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&v: &f64| v > 0.0)
            .unwrap_or(5.0);
        let mangadex_user_agent = env::var("MANGADEX_USER_AGENT")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Komika/0.1 (+https://github.com/komika)".to_string());
        Self {
            port,
            database_url,
            suwayomi_url,
            suwayomi_public_url,
            source_id,
            cors_origins,
            admin_users,
            scan_tick_seconds,
            auth_rate_limit_max,
            auth_rate_limit_window_secs,
            catalogue_sync_enabled,
            mangadex_rate_per_sec,
            mangadex_user_agent,
        }
    }
}
