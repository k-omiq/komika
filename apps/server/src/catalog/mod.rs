//! Canonical catalogue repository (CATALOGUE.md §3).
//!
//! The persistence layer over `work` / `work_alias` / `work_external_id` /
//! `source_series` / `chapter` / `merge_candidate`. Pure sqlx — no network. The
//! MangaDex sync (`crate::mangadex`) writes through `upsert_work_from_mangadex`;
//! the dedup matcher (`crate::dedup`) reads through the `find_*` / `load_match_data`
//! queries. Runtime queries only (matching the rest of the crate), so the build
//! needs no sqlx offline metadata.

pub mod normalize;
pub mod similarity;

use anyhow::Result;
use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

use normalize::normalize_title;

/// A single alt-title with its language tag (from MangaDex `altTitles`).
#[derive(Debug, Clone)]
pub struct Alias {
    pub raw: String,
    pub lang: Option<String>,
}

/// Everything needed to upsert one canonical work. Built from a MangaDex manga, or
/// synthesized for a first-class non-MangaDex work added via a Tier-2 source.
#[derive(Debug, Clone, Default)]
pub struct WorkInput {
    pub primary_title: Option<String>,
    pub primary_lang: Option<String>,
    pub description: Option<String>,
    pub year: Option<i64>,
    pub original_language: Option<String>,
    pub status: Option<String>,
    pub demographic: Option<String>,
    pub content_rating: Option<String>,
    pub is_nsfw: bool,
    pub author: Option<String>,
    pub artist: Option<String>,
    pub cover_phash: Option<String>,
    /// MangaDex cover fileName (e.g. `abc.jpg`); builds the cover URL for reader
    /// browse/read of canonical works (CATALOGUE.md §5).
    pub cover_file_name: Option<String>,
    pub aliases: Vec<Alias>,
    /// `(provider, external_id)` — e.g. `("al", "12345")`, `("mangadex", "<uuid>")`.
    pub external_ids: Vec<(String, String)>,
}

/// One mirrored chapter to upsert under a `source_series`.
#[derive(Debug, Clone, Default)]
pub struct ChapterInput {
    pub external_id: String,
    pub number: Option<String>,
    pub volume: Option<String>,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub published_at: Option<String>,
}

/// The fields the matcher needs to corroborate a candidate work. A complete DTO —
/// a few fields (id/title/language) are carried for callers/future signals even
/// though the current scorer reads only description/author/year/cover/aliases.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct WorkMatchData {
    pub work_id: String,
    pub primary_title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub year: Option<i64>,
    pub original_language: Option<String>,
    pub cover_phash: Option<String>,
    /// Normalized alias keys for this work (includes the primary title).
    pub aliases_norm: Vec<String>,
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}{}", Uuid::new_v4().simple())
}

/// Find the work already claiming `(provider, external_id)`, if any. The
/// highest-precision match key (CATALOGUE.md §4, step 1).
pub async fn find_work_by_external(
    pool: &SqlitePool,
    provider: &str,
    external_id: &str,
) -> Result<Option<String>> {
    let id = sqlx::query_scalar::<_, String>(
        "SELECT work_id FROM work_external_id WHERE provider = ? AND external_id = ?",
    )
    .bind(provider)
    .bind(external_id)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Distinct work ids whose alias index contains this exact normalized title
/// (step 2). More than one means an ambiguous title that step 4 must disambiguate.
pub async fn find_works_by_alias(pool: &SqlitePool, normalized: &str) -> Result<Vec<String>> {
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT work_id FROM work_alias WHERE normalized_title = ?",
    )
    .bind(normalized)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Fuzzy blocking (step 3): distinct work ids whose alias index contains `token`
/// as a substring, capped at `limit`. `token` should be the longest word of the
/// candidate's normalized title, so the block is selective.
pub async fn candidate_work_ids_by_token(
    pool: &SqlitePool,
    token: &str,
    limit: i64,
) -> Result<Vec<String>> {
    if token.len() < 2 {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", token.replace(['%', '_'], ""));
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT work_id FROM work_alias WHERE normalized_title LIKE ? LIMIT ?",
    )
    .bind(pattern)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Load the corroboration fields + normalized aliases for one work.
pub async fn load_match_data(pool: &SqlitePool, work_id: &str) -> Result<Option<WorkMatchData>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        primary_title: Option<String>,
        description: Option<String>,
        author: Option<String>,
        year: Option<i64>,
        original_language: Option<String>,
        cover_phash: Option<String>,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT primary_title, description, author, year, original_language, cover_phash \
         FROM work WHERE id = ?",
    )
    .bind(work_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let aliases_norm = sqlx::query_scalar::<_, String>(
        "SELECT normalized_title FROM work_alias WHERE work_id = ?",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;
    Ok(Some(WorkMatchData {
        work_id: work_id.to_string(),
        primary_title: row.primary_title,
        description: row.description,
        author: row.author,
        year: row.year,
        original_language: row.original_language,
        cover_phash: row.cover_phash,
        aliases_norm,
    }))
}

/// A canonical work resolved for reader browse/read (CATALOGUE.md §6). Carries the
/// MangaDex anchor id + cover fileName so the reader can build cover URLs and reach
/// pages via MangaDex@Home. `mangadex_id` is `None` for a first-class non-MangaDex
/// work (not reader-openable through this path yet).
#[derive(Debug, Clone, Default)]
pub struct CanonicalWork {
    pub work_id: String,
    pub mangadex_id: Option<String>,
    pub primary_title: Option<String>,
    pub description: Option<String>,
    /// Carried for completeness/future surfacing; not in the `Series` shape yet.
    #[allow(dead_code)]
    pub year: Option<i64>,
    pub original_language: Option<String>,
    pub status: Option<String>,
    pub author: Option<String>,
    pub artist: Option<String>,
    pub is_nsfw: bool,
    pub cover_file_name: Option<String>,
    pub alt_titles: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// One mirrored chapter selected for the reader (already deduped to one row per
/// number). `external_id` is the MangaDex chapter uuid — the key used to fetch pages
/// via MangaDex@Home.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CanonicalChapter {
    pub external_id: String,
    pub number: Option<String>,
    /// Selected by the mirror query and carried for completeness; not surfaced yet.
    #[allow(dead_code)]
    pub volume: Option<String>,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub published_at: Option<String>,
}

/// Load a canonical work with its MangaDex anchor + cover fileName + alt titles, for
/// the reader's canonical series path. `None` if the work id is unknown.
pub async fn load_canonical_work(
    pool: &SqlitePool,
    work_id: &str,
) -> Result<Option<CanonicalWork>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        primary_title: Option<String>,
        description: Option<String>,
        year: Option<i64>,
        original_language: Option<String>,
        status: Option<String>,
        author: Option<String>,
        artist: Option<String>,
        is_nsfw: i64,
        cover_file_name: Option<String>,
        created_at: String,
        updated_at: String,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT primary_title, description, year, original_language, status, author, artist, \
                is_nsfw, cover_file_name, created_at, updated_at \
         FROM work WHERE id = ?",
    )
    .bind(work_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };

    // The MangaDex source_series (its source_key is the MangaDex manga uuid).
    let mangadex_id = sqlx::query_scalar::<_, String>(
        "SELECT source_key FROM source_series \
         WHERE work_id = ? AND source_type = 'mangadex' LIMIT 1",
    )
    .bind(work_id)
    .fetch_optional(pool)
    .await?;

    let alt_titles = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT raw_title FROM work_alias WHERE work_id = ? ORDER BY raw_title",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;

    Ok(Some(CanonicalWork {
        work_id: work_id.to_string(),
        mangadex_id,
        primary_title: row.primary_title,
        description: row.description,
        year: row.year,
        original_language: row.original_language,
        status: row.status,
        author: row.author,
        artist: row.artist,
        is_nsfw: row.is_nsfw != 0,
        cover_file_name: row.cover_file_name,
        alt_titles,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }))
}

/// Load the reader-facing chapter list for a canonical work: the mirrored **English**
/// chapters of its MangaDex source_series, deduped to one row per chapter number and
/// ordered ascending by number. Komika serves only English chapters, so non-English
/// rows are excluded here (belt-and-suspenders alongside the English-only sync); a
/// number with several English scanlations is collapsed by `select_reader_chapters`.
pub async fn load_canonical_chapters(
    pool: &SqlitePool,
    work_id: &str,
) -> Result<Vec<CanonicalChapter>> {
    let rows = sqlx::query_as::<_, CanonicalChapter>(
        "SELECT c.external_id, c.number, c.volume, c.lang, c.title, c.published_at \
         FROM chapter c JOIN source_series ss ON ss.id = c.source_series_id \
         WHERE ss.work_id = ? AND ss.source_type = 'mangadex' AND c.lang = 'en'",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;
    Ok(select_reader_chapters(rows))
}

/// NSFW flag of the work owning a mirrored MangaDex chapter (by chapter uuid), for
/// gating `canonicalPages`. `None` if the chapter isn't in the mirror.
pub async fn chapter_owner_is_nsfw(pool: &SqlitePool, external_id: &str) -> Result<Option<bool>> {
    let v = sqlx::query_scalar::<_, i64>(
        "SELECT w.is_nsfw FROM chapter c \
         JOIN source_series ss ON ss.id = c.source_series_id \
         JOIN work w ON w.id = ss.work_id \
         WHERE c.external_id = ? AND ss.source_type = 'mangadex' LIMIT 1",
    )
    .bind(external_id)
    .fetch_optional(pool)
    .await?;
    Ok(v.map(|n| n != 0))
}

/// Parse a MangaDex chapter number string ("10", "10.5", or None) into a sort key.
/// Unparseable / missing numbers sort last.
fn chapter_sort_key(number: &Option<String>) -> f64 {
    number
        .as_deref()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(f64::INFINITY)
}

/// Collapse the raw per-language chapter rows into one row per chapter number,
/// preferring an English translation, ordered ascending by number (number-less rows
/// last). Pure so it's unit-testable without a DB.
fn select_reader_chapters(rows: Vec<CanonicalChapter>) -> Vec<CanonicalChapter> {
    use std::collections::HashMap;
    // Key by number string ("" for number-less); keep the best row per key.
    let mut best: HashMap<String, CanonicalChapter> = HashMap::new();
    for row in rows {
        let key = row.number.clone().unwrap_or_default();
        let is_en = row.lang.as_deref() == Some("en");
        match best.get(&key) {
            Some(existing) => {
                // Upgrade to an English translation when the current pick isn't one.
                if is_en && existing.lang.as_deref() != Some("en") {
                    best.insert(key, row);
                }
            }
            None => {
                best.insert(key, row);
            }
        }
    }
    let mut out: Vec<CanonicalChapter> = best.into_values().collect();
    out.sort_by(|a, b| {
        chapter_sort_key(&a.number)
            .partial_cmp(&chapter_sort_key(&b.number))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.external_id.cmp(&b.external_id))
    });
    out
}

/// Insert or update a canonical work from a MangaDex manga (identified by its
/// MangaDex id). Reuses the existing work if the MangaDex external id already maps
/// to one; otherwise mints a new work. Aliases + external ids are added
/// idempotently, and a `mangadex` source_series is ensured. Returns the work id.
pub async fn upsert_work_from_mangadex(
    pool: &SqlitePool,
    mangadex_id: &str,
    input: &WorkInput,
) -> Result<String> {
    let mut tx = pool.begin().await?;

    let existing = sqlx::query_scalar::<_, String>(
        "SELECT work_id FROM work_external_id WHERE provider = 'mangadex' AND external_id = ?",
    )
    .bind(mangadex_id)
    .fetch_optional(&mut *tx)
    .await?;
    let work_id = existing.unwrap_or_else(|| new_id("w_"));
    let now = Utc::now().to_rfc3339();

    // cover_phash uses COALESCE so a sync that hasn't computed the hash yet doesn't
    // wipe a previously-computed one.
    sqlx::query(
        "INSERT INTO work \
           (id, primary_title, primary_lang, description, year, original_language, status, \
            demographic, content_rating, is_nsfw, author, artist, cover_phash, cover_file_name, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
           primary_title = excluded.primary_title, primary_lang = excluded.primary_lang, \
           description = excluded.description, year = excluded.year, \
           original_language = excluded.original_language, status = excluded.status, \
           demographic = excluded.demographic, content_rating = excluded.content_rating, \
           is_nsfw = excluded.is_nsfw, author = excluded.author, artist = excluded.artist, \
           cover_phash = COALESCE(excluded.cover_phash, work.cover_phash), \
           cover_file_name = COALESCE(excluded.cover_file_name, work.cover_file_name), \
           updated_at = excluded.updated_at",
    )
    .bind(&work_id)
    .bind(&input.primary_title)
    .bind(&input.primary_lang)
    .bind(&input.description)
    .bind(input.year)
    .bind(&input.original_language)
    .bind(&input.status)
    .bind(&input.demographic)
    .bind(&input.content_rating)
    .bind(input.is_nsfw as i64)
    .bind(&input.author)
    .bind(&input.artist)
    .bind(&input.cover_phash)
    .bind(&input.cover_file_name)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    // Ensure the MangaDex external id maps to this work, plus any cross-catalogue ids.
    sqlx::query(
        "INSERT OR IGNORE INTO work_external_id (work_id, provider, external_id) VALUES (?, 'mangadex', ?)",
    )
    .bind(&work_id)
    .bind(mangadex_id)
    .execute(&mut *tx)
    .await?;
    for (provider, ext) in &input.external_ids {
        if provider.is_empty() || ext.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO work_external_id (work_id, provider, external_id) VALUES (?, ?, ?)",
        )
        .bind(&work_id)
        .bind(provider)
        .bind(ext)
        .execute(&mut *tx)
        .await?;
    }

    insert_aliases(&mut tx, &work_id, &input.aliases).await?;
    ensure_source_series(
        &mut tx,
        &work_id,
        "mangadex",
        "mangadex",
        mangadex_id,
        None,
        input.is_nsfw,
        &now,
    )
    .await?;

    tx.commit().await?;
    Ok(work_id)
}

/// Create a brand-new first-class canonical work (no MangaDex anchor) from an input.
/// Used by the Tier-2 add flow when the matcher decides "new work".
pub async fn create_work(pool: &SqlitePool, input: &WorkInput) -> Result<String> {
    let mut tx = pool.begin().await?;
    let work_id = new_id("w_");
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO work \
           (id, primary_title, primary_lang, description, year, original_language, status, \
            demographic, content_rating, is_nsfw, author, artist, cover_phash, cover_file_name, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&work_id)
    .bind(&input.primary_title)
    .bind(&input.primary_lang)
    .bind(&input.description)
    .bind(input.year)
    .bind(&input.original_language)
    .bind(&input.status)
    .bind(&input.demographic)
    .bind(&input.content_rating)
    .bind(input.is_nsfw as i64)
    .bind(&input.author)
    .bind(&input.artist)
    .bind(&input.cover_phash)
    .bind(&input.cover_file_name)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    for (provider, ext) in &input.external_ids {
        if provider.is_empty() || ext.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO work_external_id (work_id, provider, external_id) VALUES (?, ?, ?)",
        )
        .bind(&work_id)
        .bind(provider)
        .bind(ext)
        .execute(&mut *tx)
        .await?;
    }
    insert_aliases(&mut tx, &work_id, &input.aliases).await?;
    tx.commit().await?;
    Ok(work_id)
}

async fn insert_aliases(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    work_id: &str,
    aliases: &[Alias],
) -> Result<()> {
    for a in aliases {
        let norm = normalize_title(&a.raw);
        if norm.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO work_alias (id, work_id, normalized_title, raw_title, lang) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(new_id("al_"))
        .bind(work_id)
        .bind(&norm)
        .bind(&a.raw)
        .bind(&a.lang)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn ensure_source_series(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    work_id: &str,
    source_type: &str,
    source_id: &str,
    source_key: &str,
    source_url: Option<&str>,
    is_nsfw: bool,
    now: &str,
) -> Result<String> {
    sqlx::query(
        "INSERT INTO source_series \
           (id, work_id, source_type, source_id, source_key, source_url, is_nsfw, last_seen, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(source_type, source_id, source_key) DO UPDATE SET \
           last_seen = excluded.last_seen, is_nsfw = excluded.is_nsfw",
    )
    .bind(new_id("ss_"))
    .bind(work_id)
    .bind(source_type)
    .bind(source_id)
    .bind(source_key)
    .bind(source_url)
    .bind(is_nsfw as i64)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    let id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM source_series WHERE source_type = ? AND source_id = ? AND source_key = ?",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(source_key)
    .fetch_one(&mut **tx)
    .await?;
    Ok(id)
}

/// Public, non-transactional variant of `ensure_source_series` for the add flow.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_source_series(
    pool: &SqlitePool,
    work_id: &str,
    source_type: &str,
    source_id: &str,
    source_key: &str,
    source_url: Option<&str>,
    is_nsfw: bool,
) -> Result<String> {
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    let id = ensure_source_series(
        &mut tx,
        work_id,
        source_type,
        source_id,
        source_key,
        source_url,
        is_nsfw,
        &now,
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Resolve the id of an existing source_series by its natural key.
pub async fn find_source_series_id(
    pool: &SqlitePool,
    source_type: &str,
    source_id: &str,
    source_key: &str,
) -> Result<Option<String>> {
    let id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM source_series WHERE source_type = ? AND source_id = ? AND source_key = ?",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(source_key)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// Upsert one mirrored chapter under a source_series (idempotent on external id).
pub async fn upsert_chapter(
    pool: &SqlitePool,
    source_series_id: &str,
    ch: &ChapterInput,
) -> Result<()> {
    if ch.external_id.is_empty() {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO chapter \
           (id, source_series_id, external_id, number, volume, lang, title, published_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(source_series_id, external_id) DO UPDATE SET \
           number = excluded.number, volume = excluded.volume, lang = excluded.lang, \
           title = excluded.title, published_at = excluded.published_at",
    )
    .bind(new_id("ch_"))
    .bind(source_series_id)
    .bind(&ch.external_id)
    .bind(&ch.number)
    .bind(&ch.volume)
    .bind(&ch.lang)
    .bind(&ch.title)
    .bind(&ch.published_at)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Enqueue a mid-confidence match for manual admin review. Returns the row id.
pub async fn insert_merge_candidate(
    pool: &SqlitePool,
    source_series_id: &str,
    candidate_work_id: &str,
    score: f64,
    method: &str,
) -> Result<String> {
    let id = new_id("mc_");
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO merge_candidate \
           (id, source_series_id, candidate_work_id, score, method, status, created_at) \
         VALUES (?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(&id)
    .bind(source_series_id)
    .bind(candidate_work_id)
    .bind(score)
    .bind(method)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(id)
}

/// A sync job's persisted state.
#[derive(Debug, Clone)]
pub struct SyncState {
    /// While `seed_done` is false this is a provisional `createdAt` resume point;
    /// afterwards it's the incremental `updatedAtSince` cursor.
    pub cursor: String,
    /// Whether the initial full `createdAt` seed has completed at least once.
    pub seed_done: bool,
}

/// Read a job's full sync state (cursor + whether its initial seed has finished).
/// `None` means the job has never run → do a fresh full `createdAt` seed.
pub async fn get_sync_state(pool: &SqlitePool, job: &str) -> Result<Option<SyncState>> {
    let row = sqlx::query_as::<_, (String, i64)>(
        "SELECT last_synced_at, seed_done FROM catalogue_sync_state WHERE job = ?",
    )
    .bind(job)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(cursor, seed_done)| SyncState {
        cursor,
        seed_done: seed_done != 0,
    }))
}

/// Persist provisional seed progress (leaving `seed_done = 0`) so an interrupted
/// full seed resumes from `since` instead of restarting at `createdAt`=0 (M6).
pub async fn set_seed_progress(pool: &SqlitePool, job: &str, since: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO catalogue_sync_state (job, last_synced_at, updated_at, seed_done) \
         VALUES (?, ?, ?, 0) \
         ON CONFLICT(job) DO UPDATE SET last_synced_at = excluded.last_synced_at, \
           updated_at = excluded.updated_at",
    )
    .bind(job)
    .bind(since)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a job's initial full seed complete and set its incremental cursor to
/// `since` (the wall-clock at the completed cycle's start).
pub async fn mark_seed_done(pool: &SqlitePool, job: &str, since: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO catalogue_sync_state (job, last_synced_at, updated_at, seed_done) \
         VALUES (?, ?, ?, 1) \
         ON CONFLICT(job) DO UPDATE SET last_synced_at = excluded.last_synced_at, \
           updated_at = excluded.updated_at, seed_done = 1",
    )
    .bind(job)
    .bind(since)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Persist the sync cursor for a job. `since` is the MangaDex `since` timestamp to use
/// as `updatedAtSince` on the next incremental cycle (typically the wall-clock at the
/// start of the cycle just completed, so anything updated during it is caught next time).
pub async fn set_sync_cursor(pool: &SqlitePool, job: &str, since: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO catalogue_sync_state (job, last_synced_at, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(job) DO UPDATE SET last_synced_at = excluded.last_synced_at, \
           updated_at = excluded.updated_at",
    )
    .bind(job)
    .bind(since)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn slime_input() -> WorkInput {
        WorkInput {
            primary_title: Some("That Time I Got Reincarnated as a Slime".into()),
            description: Some("Mikami Satoru is reincarnated as a slime.".into()),
            year: Some(2015),
            is_nsfw: false,
            aliases: vec![
                Alias {
                    raw: "That Time I Got Reincarnated as a Slime".into(),
                    lang: Some("en".into()),
                },
                Alias {
                    raw: "Tensei Shitara Slime Datta Ken".into(),
                    lang: Some("ja-ro".into()),
                },
            ],
            external_ids: vec![("al".into(), "101517".into())],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_indexes_aliases_and_externals() {
        let pool = pool().await;
        let a = upsert_work_from_mangadex(&pool, "md-uuid-1", &slime_input())
            .await
            .unwrap();
        // Second upsert of the same MangaDex id reuses the same work.
        let b = upsert_work_from_mangadex(&pool, "md-uuid-1", &slime_input())
            .await
            .unwrap();
        assert_eq!(a, b);

        // External-id lookups resolve (both the mangadex id and the AniList id).
        assert_eq!(
            find_work_by_external(&pool, "mangadex", "md-uuid-1")
                .await
                .unwrap(),
            Some(a.clone())
        );
        assert_eq!(
            find_work_by_external(&pool, "al", "101517").await.unwrap(),
            Some(a.clone())
        );

        // Alias index resolves the romaji alt-title.
        let norm = normalize_title("Tensei Shitara Slime Datta Ken");
        assert_eq!(
            find_works_by_alias(&pool, &norm).await.unwrap(),
            vec![a.clone()]
        );

        // A mangadex source_series was ensured and chapters attach to it.
        let ssid = find_source_series_id(&pool, "mangadex", "mangadex", "md-uuid-1")
            .await
            .unwrap()
            .expect("source_series exists");
        upsert_chapter(
            &pool,
            &ssid,
            &ChapterInput {
                external_id: "ch-1".into(),
                number: Some("1".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        upsert_chapter(
            &pool,
            &ssid,
            &ChapterInput {
                external_id: "ch-1".into(),
                number: Some("1".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chapter WHERE source_series_id = ?")
                .bind(&ssid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "chapter upsert is idempotent on external id");
    }

    #[tokio::test]
    async fn sync_state_seed_progress_and_completion() {
        let pool = pool().await;
        // Never run → no state → a fresh createdAt seed.
        assert!(get_sync_state(&pool, "catalogue").await.unwrap().is_none());

        // A provisional seed checkpoint keeps seed_done = false so the next cycle
        // resumes the createdAt seed from this cursor (M6).
        set_seed_progress(&pool, "catalogue", "2026-07-11T00:00:00")
            .await
            .unwrap();
        let s = get_sync_state(&pool, "catalogue")
            .await
            .unwrap()
            .expect("state present");
        assert_eq!(s.cursor, "2026-07-11T00:00:00");
        assert!(!s.seed_done, "still seeding after a provisional checkpoint");

        // A later checkpoint advances the cursor, still seeding.
        set_seed_progress(&pool, "catalogue", "2026-07-11T06:00:00")
            .await
            .unwrap();
        assert!(
            !get_sync_state(&pool, "catalogue")
                .await
                .unwrap()
                .unwrap()
                .seed_done
        );

        // Completing the seed flips seed_done and sets the incremental cursor.
        mark_seed_done(&pool, "catalogue", "2026-07-12T00:00:00")
            .await
            .unwrap();
        let s = get_sync_state(&pool, "catalogue").await.unwrap().unwrap();
        assert_eq!(s.cursor, "2026-07-12T00:00:00");
        assert!(s.seed_done);

        // Incremental cursor advances without clearing seed_done.
        set_sync_cursor(&pool, "catalogue", "2026-07-13T00:00:00")
            .await
            .unwrap();
        let s = get_sync_state(&pool, "catalogue").await.unwrap().unwrap();
        assert_eq!(s.cursor, "2026-07-13T00:00:00");
        assert!(s.seed_done, "incremental cursor keeps seed_done");

        // Jobs are independent.
        assert!(get_sync_state(&pool, "chapters").await.unwrap().is_none());
    }

    fn ch(external_id: &str, number: Option<&str>, lang: Option<&str>) -> CanonicalChapter {
        CanonicalChapter {
            external_id: external_id.into(),
            number: number.map(Into::into),
            volume: None,
            lang: lang.map(Into::into),
            title: None,
            published_at: None,
        }
    }

    #[test]
    fn reader_chapters_dedupe_prefer_english_and_order() {
        let rows = vec![
            ch("es-2", Some("2"), Some("es")),
            ch("en-1", Some("1"), Some("en")),
            ch("es-1", Some("1"), Some("es")),
            ch("en-10", Some("10"), Some("en")),
            ch("en-2", Some("2"), Some("en")),
            ch("oneshot", None, Some("en")),
        ];
        let out = select_reader_chapters(rows);
        // One row per number, ascending numerically (10 after 2, not lexically), the
        // number-less oneshot last.
        let ids: Vec<&str> = out.iter().map(|c| c.external_id.as_str()).collect();
        assert_eq!(ids, vec!["en-1", "en-2", "en-10", "oneshot"]);
        // Number 1 resolved to the English row even though a Spanish one was seen first.
        assert_eq!(out[0].lang.as_deref(), Some("en"));
    }

    #[tokio::test]
    async fn canonical_work_and_chapters_round_trip() {
        let pool = pool().await;
        let w = upsert_work_from_mangadex(
            &pool,
            "md-uuid-1",
            &WorkInput {
                cover_file_name: Some("cover.jpg".into()),
                ..slime_input()
            },
        )
        .await
        .unwrap();
        let cw = load_canonical_work(&pool, &w).await.unwrap().unwrap();
        assert_eq!(cw.mangadex_id.as_deref(), Some("md-uuid-1"));
        assert_eq!(cw.cover_file_name.as_deref(), Some("cover.jpg"));
        assert!(cw.alt_titles.iter().any(|t| t.contains("Slime")));

        let ssid = find_source_series_id(&pool, "mangadex", "mangadex", "md-uuid-1")
            .await
            .unwrap()
            .unwrap();
        for (ext, num, lang) in [
            ("c2", "2", "en"),
            ("c1", "1", "en"),
            ("c1es", "1", "es"), // duplicate number in another language → excluded
            ("c3es", "3", "es"), // English-absent number → excluded entirely
        ] {
            upsert_chapter(
                &pool,
                &ssid,
                &ChapterInput {
                    external_id: ext.into(),
                    number: Some(num.into()),
                    lang: Some(lang.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let chs = load_canonical_chapters(&pool, &w).await.unwrap();
        let ids: Vec<&str> = chs.iter().map(|c| c.external_id.as_str()).collect();
        // English-only: the Spanish rows (including the English-absent "3") are dropped.
        assert_eq!(
            ids,
            vec!["c1", "c2"],
            "English-only, deduped by number, ordered"
        );
        // NSFW-owner lookup resolves through the chapter uuid.
        assert_eq!(
            chapter_owner_is_nsfw(&pool, "c1").await.unwrap(),
            Some(false)
        );
        assert_eq!(chapter_owner_is_nsfw(&pool, "nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn match_data_and_token_blocking() {
        let pool = pool().await;
        let w = upsert_work_from_mangadex(&pool, "md-uuid-1", &slime_input())
            .await
            .unwrap();
        let md = load_match_data(&pool, &w).await.unwrap().unwrap();
        assert!(md.aliases_norm.iter().any(|a| a.contains("slime")));
        let ids = candidate_work_ids_by_token(&pool, "slime", 10)
            .await
            .unwrap();
        assert_eq!(ids, vec![w]);
    }
}
