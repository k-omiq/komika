//! GraphQL types — a 1:1 code-first mirror of `packages/api/src/schema/komika.graphql`.

use async_graphql::{Enum, InputObject, SimpleObject, ID};

use crate::suwayomi::SuwayomiChapter;

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum ComicType {
    Manga,
    Manhwa,
    Manhua,
    Webtoon,
    Comic,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum SeriesStatus {
    Ongoing,
    Completed,
    Hiatus,
    Cancelled,
    Unknown,
}

#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum DiscoveryFeedKind {
    Popular,
    Trending,
    RecentlyUpdated,
    RecentlyAdded,
    Genre,
}

#[derive(SimpleObject, Clone)]
pub struct RatingSummary {
    pub average: f64,
    pub count: i32,
    /// Length 10; index 0 => score 1.
    pub distribution: Vec<i32>,
}

impl RatingSummary {
    pub fn empty() -> Self {
        Self {
            average: 0.0,
            count: 0,
            distribution: vec![0; 10],
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct ScanPolicy {
    pub avg_interval_hours: f64,
    pub override_interval_hours: Option<f64>,
    pub poll_every_minutes: i32,
    /// Effective paused state (folded: forced override, else auto-by-status).
    pub paused: bool,
    /// Raw admin overrides, so the admin console can distinguish an explicit
    /// choice from the status default even when they coincide. `null` = no
    /// override (scanner auto-decides).
    pub status_override: Option<SeriesStatus>,
    pub paused_override: Option<bool>,
    /// Raw admin poll-interval override (minutes). `null` = no override, so the
    /// effective `poll_every_minutes` above is the folded default. The admin
    /// console decodes its field from this so an unset poll stays unset.
    pub poll_every_minutes_override: Option<i32>,
    pub last_scanned_at: Option<String>,
    pub next_scan_at: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct Series {
    pub id: ID,
    pub title: String,
    pub alt_titles: Vec<String>,
    pub author: Option<String>,
    pub artist: Option<String>,
    pub description: Option<String>,
    pub genres: Vec<String>,
    #[graphql(name = "type")]
    pub r#type: ComicType,
    pub status: SeriesStatus,
    pub cover_url: String,
    pub source_id: String,
    pub chapter_count: i32,
    pub is_marked: bool,
    /// NSFW per the canonical model (CATALOGUE.md §2); false until the series is
    /// catalogued. Drives `show_nsfw` filtering of discovery/search/updates feeds.
    pub is_nsfw: bool,
    pub rating: RatingSummary,
    pub scan: ScanPolicy,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(SimpleObject, Clone)]
pub struct Chapter {
    pub id: ID,
    pub series_id: ID,
    pub number: f64,
    pub title: Option<String>,
    pub page_count: i32,
    pub uploaded_at: Option<String>,
    pub scanlator: Option<String>,
    pub read: bool,
    pub last_page_read: i32,
    pub bookmarked: bool,
    pub is_downloaded: bool,
}

#[derive(SimpleObject, Clone)]
pub struct Page {
    pub index: i32,
    pub source_url: String,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

#[derive(SimpleObject, Clone)]
pub struct DiscoveryFeed {
    pub kind: DiscoveryFeedKind,
    pub title: String,
    pub genre: Option<String>,
    pub items: Vec<Series>,
}

#[derive(SimpleObject, Clone)]
pub struct UserRef {
    pub id: ID,
    pub username: String,
    pub avatar_url: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct Review {
    pub id: ID,
    pub series_id: ID,
    pub author: UserRef,
    pub score: i32,
    pub body: String,
    pub has_spoiler: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A comment on a polymorphic target: a chapter thread (`target_type = "chapter"`)
/// or a series discussion (`target_type = "series"`).
#[derive(SimpleObject, Clone)]
pub struct Comment {
    pub id: ID,
    pub target_type: String,
    pub target_id: ID,
    pub author: UserRef,
    pub body: String,
    pub has_spoiler: bool,
    pub created_at: String,
}

#[derive(SimpleObject, Clone)]
pub struct SessionUser {
    pub id: ID,
    pub username: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    /// Whether this user has opted into seeing NSFW-flagged works (CATALOGUE.md §2).
    pub show_nsfw: bool,
}

#[derive(SimpleObject, Clone)]
pub struct Session {
    pub token: String,
    pub user: SessionUser,
}

/// Aggregate health of the background scan scheduler, for the admin console.
#[derive(SimpleObject, Clone)]
pub struct ScanStatus {
    /// Number of series in the Suwayomi library at the last tick.
    pub library_size: i32,
    /// Series found overdue (and re-scanned) at the last tick.
    pub overdue_count: i32,
    /// ISO 8601 timestamp of the last completed scan tick, if any.
    pub last_tick_at: Option<String>,
    /// Earliest upcoming `next_scan_at` across the library, if known.
    pub next_due_at: Option<String>,
}

/// A user as seen in the admin user-management console.
#[derive(SimpleObject, Clone)]
pub struct AdminUser {
    pub id: ID,
    pub username: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    pub is_banned: bool,
    pub created_at: String,
}

#[derive(SimpleObject, Clone)]
pub struct AdminUserPage {
    pub items: Vec<AdminUser>,
    pub page: i32,
    pub has_next_page: bool,
    pub total: Option<i32>,
}

#[derive(SimpleObject, Clone)]
pub struct SeriesPage {
    pub items: Vec<Series>,
    pub page: i32,
    pub has_next_page: bool,
    pub total: Option<i32>,
}

#[derive(SimpleObject, Clone)]
pub struct ReviewPage {
    pub items: Vec<Review>,
    pub page: i32,
    pub has_next_page: bool,
    pub total: Option<i32>,
}

#[derive(SimpleObject, Clone)]
pub struct CommentPage {
    pub items: Vec<Comment>,
    pub page: i32,
    pub has_next_page: bool,
    pub total: Option<i32>,
}

#[derive(InputObject)]
pub struct PostReviewInput {
    pub series_id: ID,
    pub score: i32,
    pub body: String,
    pub has_spoiler: bool,
}

#[derive(InputObject)]
pub struct PostCommentInput {
    /// `"chapter"` or `"series"`.
    pub target_type: String,
    pub target_id: ID,
    pub body: String,
    pub has_spoiler: bool,
}

#[derive(InputObject)]
pub struct RegisterInput {
    pub username: String,
    pub email: String,
    pub password: String,
}

/// Admin "manga DB" overrides. Every field is the desired whole state for the
/// series (a null override clears it), so the console sends the full form on save.
#[derive(InputObject)]
pub struct SeriesAdminInput {
    pub series_id: ID,
    pub override_interval_hours: Option<f64>,
    pub poll_every_minutes: Option<i32>,
    /// Force paused on/off; null lets the scanner auto-decide by status.
    pub paused: Option<bool>,
    /// Override the source status; null uses the source-derived status.
    pub status: Option<SeriesStatus>,
}

/// Serialize a `SeriesStatus` to its SDL name (for storage).
pub fn status_word(s: SeriesStatus) -> &'static str {
    match s {
        SeriesStatus::Ongoing => "ONGOING",
        SeriesStatus::Completed => "COMPLETED",
        SeriesStatus::Hiatus => "HIATUS",
        SeriesStatus::Cancelled => "CANCELLED",
        SeriesStatus::Unknown => "UNKNOWN",
    }
}

// ---- mapping helpers -------------------------------------------------------

/// Map a Suwayomi status string onto the Komika `SeriesStatus`.
pub fn status_from(s: &str) -> SeriesStatus {
    match s {
        "ONGOING" => SeriesStatus::Ongoing,
        "COMPLETED" | "PUBLISHING_FINISHED" | "LICENSED" => SeriesStatus::Completed,
        "CANCELLED" => SeriesStatus::Cancelled,
        "ON_HIATUS" => SeriesStatus::Hiatus,
        _ => SeriesStatus::Unknown,
    }
}

/// The scanner auto-pauses for these (effective) Komika statuses.
pub fn paused_for_status(status: SeriesStatus) -> bool {
    matches!(
        status,
        SeriesStatus::Completed | SeriesStatus::Hiatus | SeriesStatus::Cancelled
    )
}

/// Parse a stored Komika status-override string back into the enum.
pub fn komika_status(s: &str) -> Option<SeriesStatus> {
    match s {
        "ONGOING" => Some(SeriesStatus::Ongoing),
        "COMPLETED" => Some(SeriesStatus::Completed),
        "HIATUS" => Some(SeriesStatus::Hiatus),
        "CANCELLED" => Some(SeriesStatus::Cancelled),
        "UNKNOWN" => Some(SeriesStatus::Unknown),
        _ => None,
    }
}

/// Best-effort comic type from the source language.
pub fn type_from_lang(lang: Option<&str>) -> ComicType {
    match lang {
        Some(l) if l.to_lowercase().starts_with("ko") => ComicType::Manhwa,
        Some(l) if l.to_lowercase().starts_with("zh") => ComicType::Manhua,
        _ => ComicType::Manga,
    }
}

/// Coerce a Suwayomi epoch timestamp (seconds or millis, as a string) to ISO 8601.
pub fn to_iso(v: Option<&str>) -> Option<String> {
    let s = v?;
    let n: i64 = s.parse().ok()?;
    if n <= 0 {
        return None;
    }
    let ms = if n > 1_000_000_000_000 { n } else { n * 1000 };
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

/// Map a Suwayomi chapter onto the Komika `Chapter` (no DB lookup needed).
pub fn map_chapter(c: SuwayomiChapter) -> Chapter {
    Chapter {
        id: ID(c.id.to_string()),
        series_id: ID(c.manga_id.to_string()),
        number: c.chapter_number,
        title: Some(c.name),
        page_count: c.page_count as i32,
        uploaded_at: to_iso(c.upload_date.as_deref()),
        scanlator: c.scanlator,
        read: c.is_read,
        last_page_read: c.last_page_read as i32,
        bookmarked: c.is_bookmarked,
        is_downloaded: c.is_downloaded,
    }
}
