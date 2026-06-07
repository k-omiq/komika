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

/// One localized description of a work (S2/H2), keyed by BCP-47-ish language tag.
#[derive(SimpleObject, Clone)]
pub struct LocalizedDescription {
    pub lang: String,
    pub description: String,
}

/// One author/artist credit of a work (S2/H2). `role` is `"author"` or `"artist"`.
#[derive(SimpleObject, Clone)]
pub struct Credit {
    pub role: String,
    pub name: String,
}

/// One cover of a work (F2). `url` is a proxy-ready MangaDex cover URL (the client
/// resolves it through the Worker, like `coverUrl`); `isPrimary` marks the main
/// cover mirrored on `Series.coverUrl`.
#[derive(SimpleObject, Clone)]
pub struct Cover {
    /// The cover's `fileName` leaf (`covers/{mangadexId}/{fileName}`).
    pub file_name: String,
    /// Proxy-ready full cover URL (`uploads.mangadex.org/covers/...`).
    pub url: String,
    /// Proxy-ready 512px thumbnail URL.
    pub thumbnail_url: String,
    pub lang: Option<String>,
    pub volume: Option<String>,
    pub is_primary: bool,
}

#[derive(SimpleObject, Clone)]
#[graphql(complex)]
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

/// The install/pin coordinates for one Suwayomi source's extension (§2.2), joined
/// onto a `WorkSource`. Absent for a MangaDex-native mapping (there is no extension
/// to install) and for any source that hasn't been catalogued with an extension yet.
#[derive(SimpleObject, Clone)]
pub struct SourceExtension {
    pub pkg_name: String,
    pub repo_url: String,
    pub apk_name: Option<String>,
    /// `Int` (i32); the DB column is INTEGER — cast on read.
    pub version_code: Option<i32>,
    pub lang: Option<String>,
}

/// One catalogued source mapping for a canonical work, plus the extension coordinates
/// a native client needs to install and fetch from it (§2.2). `extension` is null for
/// the MangaDex-native mapping (fetched via MangaDex@Home, not an extension).
#[derive(SimpleObject, Clone)]
pub struct WorkSource {
    pub source_type: String,
    pub source_id: String,
    pub source_key: String,
    pub source_url: Option<String>,
    pub is_nsfw: bool,
    /// The extension's language (there is no per-source lang on `source_series`);
    /// null when the source has no catalogued extension.
    pub lang: Option<String>,
    pub extension: Option<SourceExtension>,
}

/// A canonical work's source mappings, keyed by `work_id`, for the batch resolver.
/// A work with no (visible) sources yields an empty `sources` list.
#[derive(SimpleObject, Clone)]
pub struct WorkSourceGroup {
    pub work_id: ID,
    pub sources: Vec<WorkSource>,
}

/// Source provenance for one catalogue `Series` (admin console): the canonical
/// work the series is linked to and every `source_series` mapping on that work,
/// with each source's extension coordinates (`WorkSource.extension.pkgName` is
/// the "extension" column). `workId` is null — and `sources` empty — for a
/// series that hasn't been catalogued into a work yet.
#[derive(SimpleObject, Clone)]
pub struct SeriesSourceGroup {
    pub series_id: ID,
    pub work_id: Option<ID>,
    pub sources: Vec<WorkSource>,
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
    /// Optional editable display name; falls back to `username` when unset.
    pub display_name: Option<String>,
    /// Optional editable "about me" text.
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    /// Whether this user has opted into seeing NSFW-flagged works (CATALOGUE.md §2).
    pub show_nsfw: bool,
    /// Account creation timestamp (ISO 8601) — the profile "joined" date.
    pub joined_at: String,
}

/// One entry in a user's activity feed (a review posted, a comment posted, a
/// series added to their library). Written by the corresponding mutation.
#[derive(SimpleObject, Clone)]
pub struct Activity {
    pub id: ID,
    /// `"review"` | `"comment"` | `"library_add"`.
    pub kind: String,
    /// `"series"` | `"chapter"` — the kind of thing acted on, when known.
    pub target_type: Option<String>,
    /// The series/chapter id the action targeted; the client resolves a title.
    pub target_id: Option<ID>,
    pub created_at: String,
}

#[derive(InputObject)]
pub struct UpdateProfileInput {
    /// New display name; `null` or blank clears it (falls back to username).
    pub display_name: Option<String>,
    /// New bio; `null` or blank clears it.
    pub bio: Option<String>,
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

// ---- Sources & Extensions admin surface (EXT-1) ------------------------------

/// Which listing of a source to browse (mirrors Suwayomi's `FetchSourceMangaType`).
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum SourceBrowseType {
    Popular,
    Latest,
    Search,
}

impl From<SourceBrowseType> for crate::suwayomi::FetchType {
    fn from(t: SourceBrowseType) -> Self {
        match t {
            SourceBrowseType::Popular => crate::suwayomi::FetchType::Popular,
            SourceBrowseType::Latest => crate::suwayomi::FetchType::Latest,
            SourceBrowseType::Search => crate::suwayomi::FetchType::Search,
        }
    }
}

/// One Keiyoushi/Mihon extension as known to the Suwayomi engine — the admin
/// management view (installed or not). Mirrors `suwayomi::ExtensionListEntry`.
#[derive(SimpleObject, Clone)]
pub struct ExtensionInfo {
    pub pkg_name: String,
    pub name: String,
    pub lang: String,
    pub version_name: String,
    pub is_installed: bool,
    pub has_update: bool,
    pub is_nsfw: bool,
    pub icon_url: Option<String>,
    /// The store/repo the extension came from, when reported.
    pub repo: Option<String>,
}

impl From<crate::suwayomi::ExtensionListEntry> for ExtensionInfo {
    fn from(e: crate::suwayomi::ExtensionListEntry) -> Self {
        ExtensionInfo {
            pkg_name: e.pkg_name,
            name: e.name,
            lang: e.lang,
            version_name: e.version_name,
            is_installed: e.is_installed,
            has_update: e.has_update,
            is_nsfw: e.is_nsfw,
            icon_url: e.icon_url,
            repo: e.repo,
        }
    }
}

/// One installed Suwayomi source — the admin picker feeding `sourceBrowse(sourceId)`.
#[derive(SimpleObject, Clone)]
pub struct SourceInfo {
    /// The Suwayomi source id (the `sourceBrowse` input).
    pub id: ID,
    /// User-facing display name (per-language variants, e.g. "MangaDex (EN)").
    pub name: String,
    pub lang: String,
    pub is_nsfw: bool,
    pub icon_url: Option<String>,
    /// The owning extension's pkgName, when reported.
    pub pkg_name: Option<String>,
}

/// One manga returned by browsing a source (admin bulk-ingest picker). The id is
/// Suwayomi's internal manga id — exactly what `bulkAddSourceSeries` consumes.
#[derive(SimpleObject, Clone)]
pub struct SourceBrowseEntry {
    pub suwayomi_manga_id: ID,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub in_library: bool,
}

/// A page of source-browse results.
#[derive(SimpleObject)]
pub struct SourceBrowsePage {
    pub items: Vec<SourceBrowseEntry>,
    pub page: i32,
    pub has_next_page: bool,
}

/// One "add all from source" background ingest job (S1). Mirrors
/// `ingest::IngestJob` / the `source_ingest_job` row.
#[derive(SimpleObject, Clone)]
pub struct SourceIngestJob {
    pub id: ID,
    pub source_id: String,
    /// `running` | `completed` | `cancelled` | `failed`.
    pub state: String,
    pub pages_done: i32,
    pub items_seen: i32,
    pub succeeded: i32,
    pub failed: i32,
    pub new_works: i32,
    pub auto_merged: i32,
    pub queued_for_review: i32,
    pub already_existing: i32,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl From<crate::ingest::IngestJob> for SourceIngestJob {
    fn from(j: crate::ingest::IngestJob) -> Self {
        SourceIngestJob {
            id: ID(j.id),
            source_id: j.source_id,
            state: j.state,
            pages_done: j.pages_done as i32,
            items_seen: j.items_seen as i32,
            succeeded: j.succeeded as i32,
            failed: j.failed as i32,
            new_works: j.new_works as i32,
            auto_merged: j.auto_merged as i32,
            queued_for_review: j.queued_for_review as i32,
            already_existing: j.already_existing as i32,
            error: j.error,
            started_at: j.started_at,
            finished_at: j.finished_at,
        }
    }
}

/// Per-id outcome of `bulkAddSourceSeries`: the dedup `MatchResult` on success,
/// or the error that made this one id fail (other ids still proceed).
#[derive(SimpleObject)]
pub struct BulkAddEntry {
    pub suwayomi_manga_id: ID,
    pub result: Option<super::MatchResult>,
    pub error: Option<String>,
}

/// Result of a bulk catalogue ingest: per-id entries plus a decision summary.
#[derive(SimpleObject)]
pub struct BulkAddResult {
    pub entries: Vec<BulkAddEntry>,
    pub total: i32,
    pub succeeded: i32,
    pub failed: i32,
    /// Decision counts across the succeeded entries.
    pub new_works: i32,
    pub auto_merged: i32,
    pub queued_for_review: i32,
    pub already_existing: i32,
}

// ---- Federated multi-extension search + translators (S3) ---------------------

/// One source/extension that carries a canonical work — a "translator" in the
/// reader UI (S3). For a Suwayomi source, `suwayomiMangaId` (= the source
/// mapping's `source_key`) is exactly the id the reader passes to
/// `chapters(seriesId:)` to fetch THIS translator's chapters; for the MangaDex
/// spine (`sourceType = "mangadex"`), chapters come from
/// `canonicalChapters(workId)` instead and `suwayomiMangaId` is null.
#[derive(SimpleObject, Clone)]
pub struct Translator {
    /// `"mangadex"` (the canonical spine) or `"suwayomi"` (an installed extension).
    pub source_type: String,
    pub source_id: String,
    /// The source's display name (e.g. "MangaDex (EN)", "Manga Plus"); null when
    /// the source isn't currently installed on the engine.
    pub source_name: Option<String>,
    pub lang: Option<String>,
    /// The Suwayomi manga id to fetch this translator's chapters with
    /// (`chapters(seriesId:)`). Null for the MangaDex spine mapping.
    pub suwayomi_manga_id: Option<ID>,
    pub extension_pkg_name: Option<String>,
    pub extension_icon_url: Option<String>,
}

/// One consolidated federated-search hit: a canonical work as a `Series`, plus
/// the per-source translator list gathered across every installed extension (S3).
#[derive(SimpleObject, Clone)]
pub struct FederatedSeries {
    pub series: Series,
    pub translators: Vec<Translator>,
}

/// A page of federated search results (S3).
#[derive(SimpleObject)]
pub struct FederatedSearchPage {
    pub items: Vec<FederatedSeries>,
    pub page: i32,
    pub has_next_page: bool,
    /// How many installed sources were actually queried in the fan-out (diagnostic).
    pub sources_queried: i32,
}

/// One genre/tag facet for the search filter UI (S4): a genre name and how many
/// cached series carry it. The full set the sources provide, not a hardcoded list.
#[derive(SimpleObject, Clone)]
pub struct GenreFacet {
    pub genre: String,
    pub count: i32,
}

/// One source that provides a given chapter number of a work (S2 aggregation).
/// `suwayomiMangaId` (for a Suwayomi source) is the id the reader passes to
/// `chapters(seriesId:)` to read this source's copy; `chapterId` is the specific
/// chapter to open (a Suwayomi chapter id, or a MangaDex chapter uuid).
#[derive(SimpleObject, Clone)]
pub struct ChapterSource {
    pub source_type: String,
    pub source_id: String,
    pub suwayomi_manga_id: Option<ID>,
    pub chapter_id: ID,
    pub scanlator: Option<String>,
}

/// One aggregated chapter of a work — a chapter NUMBER available across one or
/// more sources (S2). Deduped by number; `sources` keeps per-source availability
/// so the reader can pick a translator.
#[derive(SimpleObject, Clone)]
pub struct AggregatedChapter {
    pub number: f64,
    pub title: Option<String>,
    pub sources: Vec<ChapterSource>,
}

/// Result of folding one canonical work into another (D1 admin merge).
#[derive(SimpleObject, Clone)]
pub struct MergeWorksResult {
    /// The surviving (target) work id.
    pub target_work_id: ID,
    /// How many `source_series` mappings were re-pointed onto the target.
    pub moved_source_series: i32,
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
