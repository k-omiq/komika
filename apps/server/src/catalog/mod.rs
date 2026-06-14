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

/// One cover of a work (F2). `file_name` is the `covers/{mangadex_id}/{leaf}`
/// path leaf; `is_primary` marks the work's main cover (the one mirrored on
/// `work.cover_file_name`).
#[derive(Debug, Clone)]
pub struct Cover {
    pub file_name: String,
    pub lang: Option<String>,
    pub volume: Option<String>,
    pub is_primary: bool,
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
    /// Every localized description as `(lang, text)` (S2). The singular
    /// `description` above stays the English-preferred primary.
    pub descriptions: Vec<(String, String)>,
    /// Full credit list as `(role, name)` with role `author`/`artist` (S2). The
    /// singular `author`/`artist` above keep the first of each.
    pub credits: Vec<(String, String)>,
    /// Covers to store for this work (F2). Empty leaves any existing covers
    /// untouched; non-empty REPLACES the work's cover set (the sweep passes just
    /// the primary, the enrichment path passes the full `/cover` set).
    pub covers: Vec<Cover>,
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

/// Fuzzy blocking (step 3): distinct work ids whose alias index contains `token` as
/// a whole word, capped at `limit`. `token` is a whole word of the candidate's
/// normalized title (the fuzzy block keys on the top-N longest words), so the block
/// is selective.
///
/// H9: this is an index-usable EXACT-TOKEN lookup against the `work_alias_token`
/// inverted index (one row per work+word), replacing the old leading-wildcard
/// `normalized_title LIKE '%token%'` full-scan of `work_alias`. Word-level recall is
/// preserved because the block always feeds whole normalized words (never substrings)
/// and every alias word is indexed — so a distinctive mid/end-title word like "slime"
/// in "...as a Slime" still matches, now via the index instead of a scan.
pub async fn candidate_work_ids_by_token(
    pool: &SqlitePool,
    token: &str,
    limit: i64,
) -> Result<Vec<String>> {
    if token.len() < 2 {
        return Ok(Vec::new());
    }
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT work_id FROM work_alias_token WHERE token = ? LIMIT ?",
    )
    .bind(token)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Split a normalized title into its indexable word tokens (whitespace-separated,
/// byte-length ≥ 2 to mirror the lookup's `token.len() < 2` guard). The
/// `work_alias_token` inverted index is populated from these on every alias write.
fn alias_word_tokens(normalized_title: &str) -> Vec<&str> {
    normalized_title
        .split_whitespace()
        .filter(|t| t.len() >= 2)
        .collect()
}

/// Upsert the word tokens of one normalized alias into the `work_alias_token`
/// inverted index (H9). Idempotent via `INSERT OR IGNORE` on the UNIQUE(work_id,
/// token) key. Runs inside the caller's alias-write transaction.
async fn insert_alias_tokens(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    work_id: &str,
    normalized_title: &str,
) -> Result<()> {
    for token in alias_word_tokens(normalized_title) {
        sqlx::query("INSERT OR IGNORE INTO work_alias_token (work_id, token) VALUES (?, ?)")
            .bind(work_id)
            .bind(token)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
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
    /// Admin-pinned comic type (`content_type_override`); NULL => derive on read.
    pub content_type_override: Option<String>,
    /// Admin metadata overrides (NULL => use the derived/source value).
    pub title_override: Option<String>,
    pub description_override: Option<String>,
    pub is_nsfw_override: Option<bool>,
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

/// The effective genre/tag list for a canonical work: the admin-curated `work_tag`
/// set when present, else the distinct genres of its linked Suwayomi source series
/// (parsed from the cached JSON `genre` arrays). Empty when neither exists. Best-effort
/// — any query/parse failure just contributes nothing.
pub async fn work_effective_genres(pool: &SqlitePool, work_id: &str) -> Vec<String> {
    let curated = sqlx::query_scalar::<_, String>(
        "SELECT tag FROM work_tag WHERE work_id = ? ORDER BY ord, tag",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if !curated.is_empty() {
        return curated;
    }
    let jsons = sqlx::query_scalar::<_, String>(
        "SELECT sw.genre FROM source_series ss \
         JOIN suwayomi_series sw ON CAST(sw.id AS TEXT) = ss.source_key \
         WHERE ss.work_id = ? AND ss.source_type = 'suwayomi' AND sw.genre IS NOT NULL",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for j in jsons {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&j) {
            for g in list {
                let g = g.trim().to_string();
                if !g.is_empty() && seen.insert(g.clone()) {
                    out.push(g);
                }
            }
        }
    }
    out
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
        content_type_override: Option<String>,
        title_override: Option<String>,
        description_override: Option<String>,
        is_nsfw_override: Option<i64>,
        cover_file_name: Option<String>,
        created_at: String,
        updated_at: String,
    }
    let row = sqlx::query_as::<_, Row>(
        "SELECT primary_title, description, year, original_language, status, author, artist, \
                is_nsfw, content_type_override, title_override, description_override, \
                is_nsfw_override, cover_file_name, created_at, updated_at \
         FROM work WHERE id = ?",
    )
    .bind(work_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };

    // The MangaDex source_series (its source_key is the MangaDex manga uuid).
    // A merge can fold two mangadex works together, leaving >1 mangadex source_series
    // on the target. Order deterministically (oldest first, id as final tiebreak) so
    // the cover/page anchor is stable across loads instead of whatever the planner
    // happens to return first.
    let mangadex_id = sqlx::query_scalar::<_, String>(
        "SELECT source_key FROM source_series \
         WHERE work_id = ? AND source_type = 'mangadex' \
         ORDER BY created_at ASC, id ASC LIMIT 1",
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
        content_type_override: row.content_type_override,
        title_override: row.title_override,
        description_override: row.description_override,
        is_nsfw_override: row.is_nsfw_override.map(|v| v != 0),
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
         WHERE ss.work_id = ? AND ss.source_type = 'mangadex' AND c.lang = 'en' \
         ORDER BY c.published_at DESC, c.external_id ASC",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;
    Ok(select_reader_chapters(rows))
}

/// The publish time of the work's most recent **English** MangaDex chapter — the
/// real "last updated" for a series' details page. `None` when the work has no English
/// chapters mirrored yet (caller falls back to the work-metadata timestamp).
///
/// This is deliberately NOT `work.updated_at`: that column is bumped on every routine
/// metadata re-sync (`Utc::now()` in `upsert_work`), so it reflects "last metadata
/// touch", not "last new chapter" — a work synced 4h ago but last updated 32 days ago
/// would wrongly read "4 hours ago". `published_at` falls back to `created_at` per row
/// only when a chapter has no publish date (mirrors `canonical_updates`).
pub async fn latest_english_chapter_at(pool: &SqlitePool, work_id: &str) -> Result<Option<String>> {
    let at = sqlx::query_scalar::<_, Option<String>>(
        "SELECT MAX(COALESCE(c.published_at, c.created_at)) \
         FROM chapter c JOIN source_series ss ON ss.id = c.source_series_id \
         WHERE ss.work_id = ? AND ss.source_type = 'mangadex' AND c.lang = 'en'",
    )
    .bind(work_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(at)
}

/// NSFW flag of the work owning a mirrored MangaDex chapter (by chapter uuid), for
/// gating `canonicalPages`. `None` if the chapter isn't in the mirror.
pub async fn chapter_owner_is_nsfw(pool: &SqlitePool, external_id: &str) -> Result<Option<bool>> {
    let v = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(w.is_nsfw_override, w.is_nsfw) FROM chapter c \
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

/// True when `candidate` is a better reader representative than `existing` for the
/// same chapter number. Deterministic so a duplicated number always resolves to the
/// same `external_id` regardless of DB row order (CR2): English wins over non-English,
/// then latest `published_at`, then lowest `external_id`. Without this, two English
/// scanlations of one number would keep the arbitrary first-seen row and
/// `canonicalPages` could serve a different group's pages on reload.
fn prefer_reader_chapter(candidate: &CanonicalChapter, existing: &CanonicalChapter) -> bool {
    use std::cmp::Ordering;
    let cand_en = candidate.lang.as_deref() == Some("en");
    let exist_en = existing.lang.as_deref() == Some("en");
    if cand_en != exist_en {
        return cand_en;
    }
    // Same English-ness: prefer the latest publish (a present date beats `None`),
    // then the lowest `external_id` as a stable final tiebreak.
    match candidate.published_at.cmp(&existing.published_at) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => candidate.external_id < existing.external_id,
    }
}

/// Collapse the raw per-language chapter rows into one row per chapter number,
/// preferring an English translation, ordered ascending by number (number-less rows
/// last). Pure so it's unit-testable without a DB.
fn select_reader_chapters(rows: Vec<CanonicalChapter>) -> Vec<CanonicalChapter> {
    use std::collections::HashMap;
    // Key by number string ("" for number-less); keep the best row per key via a
    // deterministic tiebreak (see `prefer_reader_chapter`).
    let mut best: HashMap<String, CanonicalChapter> = HashMap::new();
    for row in rows {
        let key = row.number.clone().unwrap_or_default();
        match best.get(&key) {
            Some(existing) => {
                if prefer_reader_chapter(&row, existing) {
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
    // `metadata_synced_at` is stamped on every MangaDex upsert (H1): it marks that
    // this work's all-language metadata was ATTEMPTED, so the backfill advances
    // past works whose upstream record carries no localized description.
    sqlx::query(
        "INSERT INTO work \
           (id, primary_title, primary_lang, description, year, original_language, status, \
            demographic, content_rating, is_nsfw, author, artist, cover_phash, cover_file_name, created_at, updated_at, metadata_synced_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
           primary_title = excluded.primary_title, primary_lang = excluded.primary_lang, \
           description = excluded.description, year = excluded.year, \
           original_language = excluded.original_language, status = excluded.status, \
           demographic = excluded.demographic, content_rating = excluded.content_rating, \
           is_nsfw = excluded.is_nsfw, author = excluded.author, artist = excluded.artist, \
           cover_phash = COALESCE(excluded.cover_phash, work.cover_phash), \
           cover_file_name = COALESCE(excluded.cover_file_name, work.cover_file_name), \
           updated_at = excluded.updated_at, metadata_synced_at = excluded.metadata_synced_at",
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
    .bind(&now) // metadata_synced_at
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
    insert_descriptions_and_credits(&mut tx, &work_id, input).await?;
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
    insert_descriptions_and_credits(&mut tx, &work_id, input).await?;
    tx.commit().await?;
    Ok(work_id)
}

/// Delete a work and its child rows. Intended to reclaim a freshly-minted work
/// that lost a concurrent dedup claim (H6) — such a work has no `source_series`,
/// `reviews`, or `user_library` rows yet, only the child metadata `create_work`
/// wrote. Explicit child deletes make this correct regardless of the connection's
/// `foreign_keys` pragma (mirrors the `merge_works` cleanup).
pub async fn delete_work_cascade(pool: &SqlitePool, work_id: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    for table in [
        "work_alias",
        "work_alias_token",
        "work_external_id",
        "work_description",
        "work_credit",
        "work_cover",
        "work_tag",
        "chapter_override",
        "merge_candidate",
    ] {
        let col = if table == "merge_candidate" {
            "candidate_work_id"
        } else {
            "work_id"
        };
        sqlx::query(&format!("DELETE FROM {table} WHERE {col} = ?"))
            .bind(work_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM work WHERE id = ?")
        .bind(work_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Upsert the localized descriptions + credit rows (S2). Idempotent: a
/// re-sync overwrites each `(work, lang)` description in place and `INSERT OR
/// IGNORE`s credits (names never mutate, they only accumulate).
async fn insert_descriptions_and_credits(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    work_id: &str,
    input: &WorkInput,
) -> Result<()> {
    for (lang, text) in &input.descriptions {
        if lang.is_empty() || text.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT INTO work_description (work_id, lang, description) VALUES (?, ?, ?) \
             ON CONFLICT(work_id, lang) DO UPDATE SET description = excluded.description",
        )
        .bind(work_id)
        .bind(lang)
        .bind(text)
        .execute(&mut **tx)
        .await?;
    }
    for (role, name) in &input.credits {
        if name.trim().is_empty() || !matches!(role.as_str(), "author" | "artist") {
            continue;
        }
        sqlx::query("INSERT OR IGNORE INTO work_credit (work_id, role, name) VALUES (?, ?, ?)")
            .bind(work_id)
            .bind(role)
            .bind(name.trim())
            .execute(&mut **tx)
            .await?;
    }
    // F2: covers ACCUMULATE (INSERT OR IGNORE — never delete), so the recurring
    // sweep (which only knows the primary) can't wipe an already-enriched full
    // cover set. The enrichment path adds the rest via the same call.
    for c in &input.covers {
        if c.file_name.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO work_cover (work_id, cover_file_name, lang, volume, is_primary) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(work_id)
        .bind(&c.file_name)
        .bind(&c.lang)
        .bind(&c.volume)
        .bind(c.is_primary as i64)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Load a work's cover set (F2), primary first then by volume/file name.
pub async fn load_work_covers(pool: &SqlitePool, work_id: &str) -> Result<Vec<Cover>> {
    let rows = sqlx::query_as::<_, (String, Option<String>, Option<String>, i64)>(
        "SELECT cover_file_name, lang, volume, is_primary FROM work_cover WHERE work_id = ? \
         ORDER BY is_primary DESC, \
                  CAST(COALESCE(NULLIF(volume, ''), '0') AS REAL), cover_file_name",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(file_name, lang, volume, is_primary)| Cover {
            file_name,
            lang,
            volume,
            is_primary: is_primary != 0,
        })
        .collect())
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
        // H9: keep the token inverted index in lockstep with the alias write.
        insert_alias_tokens(tx, work_id, &norm).await?;
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

/// The install coordinates for one Suwayomi source's extension (§2.1). Written by
/// the scanner from the operator-side Suwayomi so a native device can install/pin the
/// exact extension a `source_series` came from. `version_code` is the version at
/// catalogue time — the device keeps its extension at or above it so `source_key`s
/// still resolve.
pub struct SourceExtensionInput {
    pub pkg_name: String,
    pub repo_url: String,
    pub apk_name: Option<String>,
    pub version_code: Option<i64>,
    pub lang: Option<String>,
    pub is_nsfw: bool,
}

/// Upsert one source's extension coordinates, keyed by its Suwayomi `source_id`
/// (the same value carried on `source_series.source_id`). Idempotent — a re-scan
/// overwrites all columns with the freshly-observed values and bumps `updated_at`.
pub async fn upsert_source_extension(
    pool: &SqlitePool,
    source_id: &str,
    input: &SourceExtensionInput,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO source_extension \
           (source_id, pkg_name, repo_url, apk_name, version_code, lang, is_nsfw, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(source_id) DO UPDATE SET \
           pkg_name = excluded.pkg_name, repo_url = excluded.repo_url, \
           apk_name = excluded.apk_name, version_code = excluded.version_code, \
           lang = excluded.lang, is_nsfw = excluded.is_nsfw, updated_at = excluded.updated_at",
    )
    .bind(source_id)
    .bind(&input.pkg_name)
    .bind(&input.repo_url)
    .bind(&input.apk_name)
    .bind(input.version_code)
    .bind(&input.lang)
    .bind(input.is_nsfw as i64)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark the works behind these MangaDex ids as metadata-attempted (H1), so the
/// backfill advances past ids MangaDex didn't return. Idempotent; a no-op for ids
/// with no `mangadex` source_series.
pub async fn mark_metadata_synced(pool: &SqlitePool, mangadex_ids: &[String]) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    for mid in mangadex_ids {
        sqlx::query(
            "UPDATE work SET metadata_synced_at = ? WHERE id IN \
             (SELECT work_id FROM source_series WHERE source_type = 'mangadex' AND source_key = ?)",
        )
        .bind(&now)
        .bind(mid)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Mark the works behind these MangaDex ids as full-cover-fetched (F2), so the
/// cover backfill advances past ids `/cover` returned nothing for. Idempotent.
pub async fn mark_covers_synced(pool: &SqlitePool, mangadex_ids: &[String]) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    for mid in mangadex_ids {
        sqlx::query(
            "UPDATE work SET covers_synced_at = ? WHERE id IN \
             (SELECT work_id FROM source_series WHERE source_type = 'mangadex' AND source_key = ?)",
        )
        .bind(&now)
        .bind(mid)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Escalate a work to NSFW. Only ever sets the flag — never clears it: "unknown =
/// safe", but once any source signals NSFW the work stays NSFW. Idempotent. The
/// gating reads consult `work.is_nsfw` exclusively (never `source_series.is_nsfw`),
/// so a source-level signal must be OR'd in here to have any effect (N4).
pub async fn mark_work_nsfw(pool: &SqlitePool, work_id: &str) -> Result<()> {
    sqlx::query("UPDATE work SET is_nsfw = 1, updated_at = ? WHERE id = ? AND is_nsfw = 0")
        .bind(Utc::now().to_rfc3339())
        .bind(work_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Outcome of folding one work into another (D1 admin merge).
#[derive(Debug, Clone, Default)]
pub struct MergeOutcome {
    pub moved_source_series: u64,
}

/// Fold `source_work` into `target_work` (D1): re-point every mapping + user-data
/// row from source to target, fold the source's identity signals (aliases,
/// external ids, and cover_phash if the target lacks one) into target, then delete
/// the now-empty source (cascade cleans its remaining child rows). Idempotent-ish:
/// a mapping the target already has is dropped rather than duplicated. Returns how
/// many `source_series` rows moved. Errors if either work is missing or they're equal.
pub async fn merge_works(
    pool: &SqlitePool,
    source_work: &str,
    target_work: &str,
) -> Result<MergeOutcome> {
    if source_work == target_work {
        anyhow::bail!("cannot merge a work into itself");
    }
    let mut tx = pool.begin().await?;
    for id in [source_work, target_work] {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM work WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            anyhow::bail!("no such work: {id}");
        }
    }

    // Fold aliases (fresh ids; UNIQUE(work_id,normalized_title,lang) de-dupes).
    let src_aliases = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT normalized_title, raw_title, lang FROM work_alias WHERE work_id = ?",
    )
    .bind(source_work)
    .fetch_all(&mut *tx)
    .await?;
    for (norm, raw, lang) in src_aliases {
        sqlx::query(
            "INSERT OR IGNORE INTO work_alias (id, work_id, normalized_title, raw_title, lang) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(new_id("al_"))
        .bind(target_work)
        .bind(&norm)
        .bind(&raw)
        .bind(&lang)
        .execute(&mut *tx)
        .await?;
        // H9: fold the alias's word tokens into the target's inverted index too.
        insert_alias_tokens(&mut tx, target_work, &norm).await?;
    }
    // Fold external ids.
    sqlx::query(
        "INSERT OR IGNORE INTO work_external_id (work_id, provider, external_id) \
         SELECT ?, provider, external_id FROM work_external_id WHERE work_id = ?",
    )
    .bind(target_work)
    .bind(source_work)
    .execute(&mut *tx)
    .await?;
    // Give the target a cover_phash if it has none (strengthens future dedup).
    sqlx::query(
        "UPDATE work SET cover_phash = (SELECT cover_phash FROM work WHERE id = ?) \
         WHERE id = ? AND cover_phash IS NULL",
    )
    .bind(source_work)
    .bind(target_work)
    .execute(&mut *tx)
    .await?;

    // Re-point mappings + user data. `UPDATE OR IGNORE` skips a row that would
    // collide with one the target already has; the leftover source rows are then
    // removed (source_series explicitly, the rest by the final cascade delete).
    let moved = sqlx::query("UPDATE OR IGNORE source_series SET work_id = ? WHERE work_id = ?")
        .bind(target_work)
        .bind(source_work)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM source_series WHERE work_id = ?")
        .bind(source_work)
        .execute(&mut *tx)
        .await?;

    for (table, col) in [
        ("merge_candidate", "candidate_work_id"),
        // Per-user library membership for canonical works lives in `user_library`
        // (series_id = the `w_` work id); repoint it when folding works.
        ("user_library", "series_id"),
        ("reviews", "series_id"),
    ] {
        sqlx::query(&format!(
            "UPDATE OR IGNORE {table} SET {col} = ? WHERE {col} = ?"
        ))
        .bind(target_work)
        .bind(source_work)
        .execute(&mut *tx)
        .await?;
    }
    // `reviews`/`user_library` have a unique key on (series_id, user_id) /
    // (user_id, series_id), so a user with a row on BOTH merged works had their
    // source row SKIPPED by the `UPDATE OR IGNORE` above (the target's row wins).
    // Delete those losing leftovers so the imminent `work` delete can't orphan a
    // row pointing at a nonexistent work (phantom library entry / stray review).
    for table in ["reviews", "user_library"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE series_id = ?"))
            .bind(source_work)
            .execute(&mut *tx)
            .await?;
    }
    // canonical_progress.work_id isn't part of any unique key → plain repoint.
    sqlx::query("UPDATE canonical_progress SET work_id = ? WHERE work_id = ?")
        .bind(target_work)
        .bind(source_work)
        .execute(&mut *tx)
        .await?;
    // Series comments (polymorphic) re-point to the target series id.
    sqlx::query(
        "UPDATE OR IGNORE comments SET target_id = ? WHERE target_type = 'series' AND target_id = ?",
    )
    .bind(target_work)
    .bind(source_work)
    .execute(&mut *tx)
    .await?;

    // Explicitly remove the source's remaining child rows before deleting it.
    // Production has ON DELETE CASCADE, but being explicit makes the merge correct
    // regardless of the connection's `foreign_keys` pragma (and testable in-memory).
    // work_tag / chapter_override are admin edits on the SOURCE work; the merge folds
    // the source into the target, so drop them with the other source-scoped child rows
    // (the target keeps its own). Explicit so the merge is correct with FKs off too.
    for table in [
        "work_alias",
        "work_alias_token",
        "work_external_id",
        "work_description",
        "work_credit",
        "work_cover",
        "work_tag",
        "chapter_override",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE work_id = ?"))
            .bind(source_work)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("DELETE FROM merge_candidate WHERE candidate_work_id = ?")
        .bind(source_work)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM work WHERE id = ?")
        .bind(source_work)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(MergeOutcome {
        moved_source_series: moved,
    })
}

/// One raw chapter row from any of a work's sources (S2 aggregation input).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkChapterRow {
    pub number: f64,
    pub title: Option<String>,
    pub source_type: String,
    pub source_id: String,
    pub suwayomi_manga_id: Option<String>,
    pub chapter_id: String,
    pub scanlator: Option<String>,
}

/// One reconciled Suwayomi mapping for a work (F1): the authoritative
/// `source_series` per `source_id`.
#[derive(Debug, Clone)]
pub struct AuthMapping {
    pub source_id: String,
    /// The authoritative Suwayomi manga id (= `source_series.source_key`), the
    /// mapping with the most cached chapters for its `source_id`.
    pub source_key: String,
}

/// The AUTHORITATIVE Suwayomi mapping per `source_id` for a work (F1). The dedup
/// matcher can consolidate several DISTINCT same-source manga onto one work (e.g.
/// two "Naruto" entries from the MangaDex extension, one with chapters and one
/// without), leaving redundant `source_series` rows for the same `source_id`. This
/// picks, per `source_id`, the mapping with the MOST cached chapters (tiebreak:
/// most-recent `last_seen`, then lowest `id`), so `aggregatedChapters`,
/// `workSources`/translators, and live `chapters()` all agree on ONE readable id.
pub async fn authoritative_suwayomi_mappings(
    pool: &SqlitePool,
    work_id: &str,
) -> Result<Vec<AuthMapping>> {
    let rows = sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT ss.id, ss.source_id, ss.source_key, \
                (SELECT COUNT(*) FROM suwayomi_chapter sc \
                 WHERE sc.manga_id = CAST(ss.source_key AS INTEGER)) AS cached \
         FROM source_series ss \
         WHERE ss.work_id = ? AND ss.source_type = 'suwayomi' \
         ORDER BY ss.source_id ASC, cached DESC, ss.last_seen DESC, ss.id ASC",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;
    // The ORDER BY puts the winner per source_id first; keep the first per source_id.
    let mut out: Vec<AuthMapping> = Vec::new();
    for (_ss_id, source_id, source_key, _cached) in rows {
        if out.iter().any(|m| m.source_id == source_id) {
            continue; // already have this source's authoritative mapping
        }
        out.push(AuthMapping {
            source_id,
            source_key,
        });
    }
    Ok(out)
}

/// Every chapter across a work's sources (S2/F1): the AUTHORITATIVE Suwayomi source
/// caches (one mapping per source_id — see `authoritative_suwayomi_mappings`) UNION
/// the MangaDex mirror. Raw rows — the caller groups them by number. This is what
/// makes a work whose MangaDex spine has 0 chapters still show an installed source's
/// chapters, while never surfacing a redundant same-source mapping the reader can't
/// read from.
pub async fn work_source_chapters(pool: &SqlitePool, work_id: &str) -> Result<Vec<WorkChapterRow>> {
    let auth = authoritative_suwayomi_mappings(pool, work_id).await?;
    let mut rows: Vec<WorkChapterRow> = Vec::new();
    // Suwayomi chapters from the authoritative mapping of each source.
    for m in &auth {
        let Ok(manga_id) = m.source_key.parse::<i64>() else {
            continue;
        };
        let chapters = sqlx::query_as::<_, (f64, String, i64, Option<String>)>(
            "SELECT chapter_number, name, id, scanlator FROM suwayomi_chapter WHERE manga_id = ?",
        )
        .bind(manga_id)
        .fetch_all(pool)
        .await?;
        for (number, title, id, scanlator) in chapters {
            rows.push(WorkChapterRow {
                number,
                title: Some(title),
                source_type: "suwayomi".into(),
                source_id: m.source_id.clone(),
                suwayomi_manga_id: Some(m.source_key.clone()),
                chapter_id: id.to_string(),
                scanlator,
            });
        }
    }
    // MangaDex mirror (English).
    // `CAST(c.number AS REAL)` yields 0.0 for a non-numeric number string ("Extra",
    // "Oneshot", …), which would masquerade as a real chapter 0. Require at least one
    // digit so those non-numeric labels are excluded rather than folded onto 0.
    let md = sqlx::query_as::<_, WorkChapterRow>(
        "SELECT CAST(c.number AS REAL) AS number, c.title AS title, 'mangadex' AS source_type, \
                ss.source_id AS source_id, NULL AS suwayomi_manga_id, \
                c.external_id AS chapter_id, NULL AS scanlator \
         FROM source_series ss \
         JOIN chapter c ON c.source_series_id = ss.id \
         WHERE ss.work_id = ? AND ss.source_type = 'mangadex' AND c.lang = 'en' \
           AND c.number IS NOT NULL AND c.number GLOB '*[0-9]*'",
    )
    .bind(work_id)
    .fetch_all(pool)
    .await?;
    rows.extend(md);
    Ok(rows)
}

/// The count of "main" chapters to display for a series: the number of DISTINCT
/// chapter numbers after grouping by their integer part (the floor). Grouping folds
/// three kinds of noise into one count that matches what external catalogues headline:
///   - per-scanlator / per-language duplicate rows of the same number collapse;
///   - a sparse ".5" bonus/omake (7.5, 14.5, …) folds into its base chapter (7, 14);
///   - split-part numbering (9.1, 9.2 or 10.1, 10.2 …) folds each real chapter into a
///     single count instead of one-per-part.
///
/// So Tsukimichi — 117 whole chapters carried as 151 scanlator-duplicated rows plus 4
/// ".5" extras — reports 117, while a split-numbered series like "Villainess Level 99"
/// (…9.1, 9.2, 10.1…) still reports its ~22 real chapters rather than the 12 whole
/// numbers that happen to appear. A real "Chapter 0 / Episode 0" first chapter IS
/// counted (common in webtoons/manhwa) by grouping on the floor and keeping group 0;
/// only NEGATIVE / non-finite sentinels are dropped. A source with no non-negative
/// numbers at all falls back to its raw row count so it never collapses to zero.
pub fn main_chapter_count<I: IntoIterator<Item = f64>>(numbers: I) -> i64 {
    use std::collections::HashSet;
    let mut groups: HashSet<i64> = HashSet::new();
    let mut total: i64 = 0;
    for n in numbers {
        total += 1;
        // Non-finite/sentinel and negative rows aren't chapters (but still count toward
        // the raw-row fallback below). A "chapter 0" is real content, so it's kept.
        if !n.is_finite() || n < 0.0 {
            continue;
        }
        groups.insert(n.floor() as i64);
    }
    if groups.is_empty() {
        total
    } else {
        groups.len() as i64
    }
}

/// `main_chapter_count` over reader chapters whose numbers are `Option<String>` (the
/// MangaDex mirror shape). Number-less rows only feed the raw-row fallback.
pub fn main_chapter_count_str(chapters: &[CanonicalChapter]) -> i64 {
    main_chapter_count(chapters.iter().map(|c| {
        c.number
            .as_deref()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(f64::NAN)
    }))
}

/// The aggregate chapter count of a work (S2): the number of main chapters across all
/// its sources (see `main_chapter_count`). This is the count the reader should see — a
/// work whose MangaDex spine has 0 chapters but whose asurascans source has 201
/// reports 201, not 0.
pub async fn aggregate_chapter_count(pool: &SqlitePool, work_id: &str) -> Result<i64> {
    let rows = work_source_chapters(pool, work_id).await?;
    Ok(main_chapter_count(rows.iter().map(|r| r.number)))
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

/// Resolve the `(id, work_id)` of an existing source_series by its natural key —
/// for the idempotency pre-check in the Tier-2 add flow, which needs the linked
/// work to report back without re-running the matcher (DD2).
pub async fn find_source_series(
    pool: &SqlitePool,
    source_type: &str,
    source_id: &str,
    source_key: &str,
) -> Result<Option<(String, String)>> {
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT id, work_id FROM source_series \
         WHERE source_type = ? AND source_id = ? AND source_key = ?",
    )
    .bind(source_type)
    .bind(source_id)
    .bind(source_key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
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

    fn ch_pub(
        external_id: &str,
        number: Option<&str>,
        lang: Option<&str>,
        published_at: Option<&str>,
    ) -> CanonicalChapter {
        CanonicalChapter {
            published_at: published_at.map(Into::into),
            ..ch(external_id, number, lang)
        }
    }

    #[test]
    fn reader_chapters_english_tiebreak_deterministic() {
        // Two English scanlations of the same number (CR2). The kept representative
        // must be the same regardless of input order: latest `published_at` wins.
        let older = ch_pub("md-a", Some("5"), Some("en"), Some("2020-01-01T00:00:00Z"));
        let newer = ch_pub("md-z", Some("5"), Some("en"), Some("2023-01-01T00:00:00Z"));

        for rows in [
            vec![older.clone(), newer.clone()],
            vec![newer.clone(), older.clone()],
        ] {
            let out = select_reader_chapters(rows);
            assert_eq!(out.len(), 1);
            assert_eq!(
                out[0].external_id, "md-z",
                "latest published_at kept regardless of input order"
            );
        }

        // Equal `published_at` → lowest `external_id` breaks the tie, both orders.
        let a = ch_pub("md-a", Some("5"), Some("en"), Some("2021-01-01T00:00:00Z"));
        let b = ch_pub("md-b", Some("5"), Some("en"), Some("2021-01-01T00:00:00Z"));
        for rows in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
            let out = select_reader_chapters(rows);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].external_id, "md-a");
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
    async fn latest_english_chapter_at_ignores_non_english_and_missing() {
        let pool = pool().await;
        let w = upsert_work_from_mangadex(&pool, "md-latest", &slime_input())
            .await
            .unwrap();
        // No English chapter yet → None (caller falls back to work metadata time).
        assert_eq!(latest_english_chapter_at(&pool, &w).await.unwrap(), None);

        let ssid = find_source_series_id(&pool, "mangadex", "mangadex", "md-latest")
            .await
            .unwrap()
            .unwrap();
        for (ext, num, lang, pub_at) in [
            ("en1", "1", "en", Some("2026-01-01T00:00:00Z")),
            ("en2", "2", "en", Some("2026-02-01T00:00:00Z")), // newest English
            ("es3", "3", "es", Some("2026-06-01T00:00:00Z")), // newer, but Spanish → ignored
        ] {
            upsert_chapter(
                &pool,
                &ssid,
                &ChapterInput {
                    external_id: ext.into(),
                    number: Some(num.into()),
                    lang: Some(lang.into()),
                    published_at: pub_at.map(Into::into),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        // The Spanish 2026-06 row is ignored; the latest English publish wins.
        assert_eq!(
            latest_english_chapter_at(&pool, &w)
                .await
                .unwrap()
                .as_deref(),
            Some("2026-02-01T00:00:00Z"),
        );
    }

    #[tokio::test]
    async fn source_extension_upsert_is_idempotent_on_source_id() {
        // §2.1: the source_id is the PK, so a re-scan updates the same row in place
        // (e.g. a bumped version_code) rather than inserting a duplicate.
        let pool = pool().await;
        let sid = "1024";
        upsert_source_extension(
            &pool,
            sid,
            &SourceExtensionInput {
                pkg_name: "eu.kanade.tachiyomi.extension.en.mangadex".into(),
                repo_url: "https://example.test/index.min.json".into(),
                apk_name: Some("tachiyomi-en.mangadex-v1.4.60.apk".into()),
                version_code: Some(60),
                lang: Some("en".into()),
                is_nsfw: false,
            },
        )
        .await
        .unwrap();

        // Re-observe the same source with a higher version_code.
        upsert_source_extension(
            &pool,
            sid,
            &SourceExtensionInput {
                pkg_name: "eu.kanade.tachiyomi.extension.en.mangadex".into(),
                repo_url: "https://example.test/index.min.json".into(),
                apk_name: Some("tachiyomi-en.mangadex-v1.4.61.apk".into()),
                version_code: Some(61),
                lang: Some("en".into()),
                is_nsfw: false,
            },
        )
        .await
        .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_extension")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "upsert is idempotent on the source_id PK");
        let version: i64 =
            sqlx::query_scalar("SELECT version_code FROM source_extension WHERE source_id = ?")
                .bind(sid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(version, 61, "the second upsert updates the row in place");
    }

    #[tokio::test]
    async fn metadata_sync_marker_advances_past_descriptionless_works() {
        // H1: a work whose upstream record has NO description still gets
        // `metadata_synced_at` set on upsert (and via mark_metadata_synced for ids
        // MangaDex never returns), so the backfill selector (metadata_synced_at IS
        // NULL) drains instead of looping on it forever.
        let pool = pool().await;

        // A description-less MangaDex work: upsert must still stamp the marker.
        let no_desc = WorkInput {
            primary_title: Some("Description-less Title".into()),
            aliases: vec![Alias {
                raw: "Description-less Title".into(),
                lang: None,
            }],
            ..Default::default()
        };
        let w = upsert_work_from_mangadex(&pool, "md-nodesc", &no_desc)
            .await
            .unwrap();
        // No work_description row (nothing to insert)...
        let descs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM work_description WHERE work_id = ?")
                .bind(&w)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(descs, 0, "no localized description upstream");
        // ...but the marker IS set, so the backfill selector won't re-pick it.
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM source_series ss JOIN work w ON w.id = ss.work_id \
             WHERE ss.source_type = 'mangadex' AND w.metadata_synced_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 0, "the upsert advanced the backfill cursor");

        // A pre-S2 work with a NULL marker is selectable, then mark_metadata_synced
        // clears it even without MangaDex returning the record.
        sqlx::query("UPDATE work SET metadata_synced_at = NULL WHERE id = ?")
            .bind(&w)
            .execute(&pool)
            .await
            .unwrap();
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM source_series ss JOIN work w ON w.id = ss.work_id \
             WHERE ss.source_type = 'mangadex' AND w.metadata_synced_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 1, "reset marker makes it selectable again");
        mark_metadata_synced(&pool, &["md-nodesc".to_string()])
            .await
            .unwrap();
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM source_series ss JOIN work w ON w.id = ss.work_id \
             WHERE ss.source_type = 'mangadex' AND w.metadata_synced_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            pending, 0,
            "mark_metadata_synced advances a not-returned id"
        );
    }

    #[tokio::test]
    async fn authoritative_mapping_dedupes_same_source_and_aggregation_agrees() {
        // F1: a work with TWO mappings for the same source (the dedup matcher
        // consolidated two distinct manga) — key "366" (0 cached) and key "377"
        // (42 cached) — plus MangaPlus "357" (3). The authoritative mapping per
        // source is the one with the most cached chapters, and aggregation +
        // provenance must reference only those (not the empty 366).
        let pool = pool().await;
        let work = upsert_work_from_mangadex(
            &pool,
            "md-naruto",
            &WorkInput {
                primary_title: Some("Naruto".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // MangaDex-ext source "2499": two mappings (366 empty, 377 full).
        upsert_source_series(&pool, &work, "suwayomi", "2499", "366", None, false)
            .await
            .unwrap();
        upsert_source_series(&pool, &work, "suwayomi", "2499", "377", None, false)
            .await
            .unwrap();
        // MangaPlus source "1998": one mapping (357, 3 chapters).
        upsert_source_series(&pool, &work, "suwayomi", "1998", "357", None, false)
            .await
            .unwrap();
        let now = "2026-01-01T00:00:00Z";
        // Cache 42 chapters under 377, 3 under 357, 0 under 366.
        for (manga, count) in [(377, 42), (357, 3)] {
            for n in 1..=count {
                sqlx::query(
                    "INSERT INTO suwayomi_chapter (id, manga_id, name, chapter_number, page_count, updated_at) \
                     VALUES (?, ?, ?, ?, 0, ?)",
                )
                .bind(manga * 1000 + n)
                .bind(manga)
                .bind(format!("Ch {n}"))
                .bind(n as f64)
                .bind(now)
                .execute(&pool)
                .await
                .unwrap();
            }
        }

        let auth = authoritative_suwayomi_mappings(&pool, &work).await.unwrap();
        // One authoritative per source_id: 377 for 2499, 357 for 1998.
        assert_eq!(auth.len(), 2);
        let by_src: std::collections::HashMap<&str, &str> = auth
            .iter()
            .map(|m| (m.source_id.as_str(), m.source_key.as_str()))
            .collect();
        assert_eq!(
            by_src["2499"], "377",
            "the 42-chapter mapping wins over 366"
        );
        assert_eq!(by_src["1998"], "357");

        // Aggregation only references authoritative ids (never 366).
        let rows = work_source_chapters(&pool, &work).await.unwrap();
        let ids: std::collections::HashSet<&str> = rows
            .iter()
            .filter_map(|r| r.suwayomi_manga_id.as_deref())
            .collect();
        assert!(ids.contains("377") && ids.contains("357"));
        assert!(!ids.contains("366"), "redundant empty mapping is excluded");
        // Aggregate distinct-number count = union(377's 1..42, 357's 1..3) = 42.
        assert_eq!(aggregate_chapter_count(&pool, &work).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn aggregate_chapters_unions_across_sources() {
        // S2: a work whose MangaDex spine has 0 chapters but whose Suwayomi source
        // (asurascans, manga id 333) has chapters must report the aggregate.
        let pool = pool().await;
        let work = upsert_work_from_mangadex(
            &pool,
            "md-solo",
            &WorkInput {
                primary_title: Some("Solo Leveling".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // A Suwayomi source mapping (source_key = the Suwayomi manga id "333").
        upsert_source_series(&pool, &work, "suwayomi", "asura-src", "333", None, false)
            .await
            .unwrap();
        // Cache three asura chapters (as the scanner would), plus a duplicate number
        // from a second (hypothetical) source to prove numbers dedupe.
        upsert_source_series(&pool, &work, "suwayomi", "other-src", "999", None, false)
            .await
            .unwrap();
        let now = "2026-01-01T00:00:00Z";
        for (id, manga, num) in [(1, 333, 1.0), (2, 333, 2.0), (3, 333, 3.0), (4, 999, 1.0)] {
            sqlx::query(
                "INSERT INTO suwayomi_chapter (id, manga_id, name, chapter_number, page_count, updated_at) \
                 VALUES (?, ?, ?, ?, 0, ?)",
            )
            .bind(id)
            .bind(manga)
            .bind(format!("Ch {num}"))
            .bind(num)
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
        }

        // Aggregate count = 3 distinct numbers (1,2,3), not 4 rows.
        assert_eq!(aggregate_chapter_count(&pool, &work).await.unwrap(), 3);
        // Raw rows carry per-source availability (number 1 from BOTH sources).
        let rows = work_source_chapters(&pool, &work).await.unwrap();
        let n1: Vec<&str> = rows
            .iter()
            .filter(|r| r.number == 1.0)
            .map(|r| r.suwayomi_manga_id.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(n1.len(), 2, "chapter 1 available from two sources");
        assert!(n1.contains(&"333") && n1.contains(&"999"));
    }

    #[test]
    fn main_chapter_count_groups_by_chapter_number() {
        // The Tsukimichi shape: 117 whole chapters (1..=117), 4 ".5" bonus chapters
        // (which fold into their base chapter), and 30 numbers each duplicated by a
        // second scanlator → 151 source rows. The displayed count must be 117, NOT 121
        // (counting the .5s) and NOT 151 (raw rows).
        let mut nums: Vec<f64> = (1..=117).map(|n| n as f64).collect();
        nums.extend([7.5, 14.5, 21.5, 28.5]);
        for n in 1..=30 {
            nums.push(n as f64); // a duplicate row for chapters 1..30 (second scanlator)
        }
        assert_eq!(nums.len(), 151, "the raw row count is the inflated 151");
        assert_eq!(main_chapter_count(nums), 117);

        // Split-part numbering (chapter 9 = 9.1 + 9.2, chapter 10 = 10.1 + 10.2 …): each
        // real chapter must count once, NOT collapse to the few whole numbers present.
        // Here chapters 1,2, then 3.1/3.2, 4.1/4.2 → 4 chapters (1,2,3,4), not 2.
        assert_eq!(main_chapter_count([1.0, 2.0, 3.1, 3.2, 4.1, 4.2]), 4);

        // A real "Chapter 0" first chapter is counted (webtoon/manhwa episode 0): chapters
        // 0,1,2 → 3, and a 0.5 teaser folds into group 0 (still one chapter there).
        assert_eq!(main_chapter_count([0.0, 0.5, 1.0, 2.0, f64::NAN]), 3);
        assert_eq!(main_chapter_count([0.5, 0.9]), 1);

        // Negatives / non-finite are sentinels, never chapters.
        assert_eq!(main_chapter_count([-1.0, -5.0, 1.0, 2.0]), 2);

        // Fallback: no non-negative numbers at all → raw row count (never zero-out a
        // series that genuinely has chapters).
        assert_eq!(main_chapter_count([f64::NAN, f64::NAN, f64::NAN]), 3);
        assert_eq!(main_chapter_count([-1.0, -2.0]), 2);
    }

    #[tokio::test]
    async fn merge_works_folds_source_into_target() {
        // D1: fold a federation-only duplicate into the MangaDex-anchored work.
        let pool = pool().await;
        // Target: MangaDex-anchored, enriched.
        let target = upsert_work_from_mangadex(
            &pool,
            "md-naruto",
            &WorkInput {
                primary_title: Some("Naruto".into()),
                author: Some("Kishimoto Masashi".into()),
                aliases: vec![Alias {
                    raw: "Naruto".into(),
                    lang: Some("en".into()),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Source: a federation-only work with its own Suwayomi mapping + an alias
        // the target lacks.
        let source = create_work(
            &pool,
            &WorkInput {
                primary_title: Some("Naruto".into()),
                aliases: vec![Alias {
                    raw: "ナルト".into(),
                    lang: Some("ja".into()),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        upsert_source_series(&pool, &source, "suwayomi", "998", "357", None, false)
            .await
            .unwrap();

        let out = merge_works(&pool, &source, &target).await.unwrap();
        assert_eq!(out.moved_source_series, 1, "the Suwayomi mapping moved");

        // Source work is gone; the Suwayomi mapping now points at the target.
        let src_exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work WHERE id = ?")
            .bind(&source)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(src_exists, 0, "source work deleted");
        let mapped: String =
            sqlx::query_scalar("SELECT work_id FROM source_series WHERE source_key = '357'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(mapped, target);
        // The source's ja alias was folded into the target.
        let ja: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM work_alias WHERE work_id = ? AND raw_title = 'ナルト'",
        )
        .bind(&target)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ja, 1, "source alias folded into target");
        // No orphaned source_series/aliases remain for the deleted work.
        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_alias WHERE work_id = ?")
            .bind(&source)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(orphans, 0);

        // Merging a work into itself is rejected.
        assert!(merge_works(&pool, &target, &target).await.is_err());
        // A missing work is rejected.
        assert!(merge_works(&pool, "w_nope", &target).await.is_err());
    }

    #[tokio::test]
    async fn covers_accumulate_and_load_primary_first() {
        // F2: covers accumulate (a later sweep with just the primary must NOT wipe
        // an enriched full set), and load returns primary first then by volume.
        let pool = pool().await;
        // Enrichment stores the full set.
        let full = WorkInput {
            primary_title: Some("Cover Test".into()),
            covers: vec![
                Cover {
                    file_name: "v2.jpg".into(),
                    lang: Some("ja".into()),
                    volume: Some("2".into()),
                    is_primary: false,
                },
                Cover {
                    file_name: "v1.jpg".into(),
                    lang: Some("ja".into()),
                    volume: Some("1".into()),
                    is_primary: true,
                },
                Cover {
                    file_name: "v10.jpg".into(),
                    lang: Some("ja".into()),
                    volume: Some("10".into()),
                    is_primary: false,
                },
            ],
            ..Default::default()
        };
        let w = upsert_work_from_mangadex(&pool, "md-cov", &full)
            .await
            .unwrap();

        // A later sweep upsert carrying ONLY the primary must not delete v2/v10.
        let sweep = WorkInput {
            primary_title: Some("Cover Test".into()),
            covers: vec![Cover {
                file_name: "v1.jpg".into(),
                lang: Some("ja".into()),
                volume: Some("1".into()),
                is_primary: true,
            }],
            ..Default::default()
        };
        upsert_work_from_mangadex(&pool, "md-cov", &sweep)
            .await
            .unwrap();

        let covers = load_work_covers(&pool, &w).await.unwrap();
        assert_eq!(
            covers.len(),
            3,
            "accumulated set survives a primary-only sweep"
        );
        // Primary first, then volume-ascending numerically (10 after 2).
        assert!(covers[0].is_primary);
        assert_eq!(covers[0].file_name, "v1.jpg");
        let order: Vec<&str> = covers.iter().map(|c| c.file_name.as_str()).collect();
        assert_eq!(order, vec!["v1.jpg", "v2.jpg", "v10.jpg"]);
    }

    #[tokio::test]
    async fn descriptions_and_credits_persist_and_upsert() {
        // S2: multi-lang descriptions + full credit list are stored and a re-sync
        // updates a description in place while credits accumulate without dupes.
        let pool = pool().await;
        let mut input = slime_input();
        input.descriptions = vec![
            ("en".into(), "English blurb.".into()),
            ("ja".into(), "日本語のあらすじ".into()),
        ];
        input.credits = vec![
            ("author".into(), "Fuse".into()),
            ("artist".into(), "Mitz Vah".into()),
        ];
        let w = upsert_work_from_mangadex(&pool, "md-desc", &input)
            .await
            .unwrap();

        let descs: Vec<(String, String)> = sqlx::query_as(
            "SELECT lang, description FROM work_description WHERE work_id = ? ORDER BY lang",
        )
        .bind(&w)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            descs,
            vec![
                ("en".to_string(), "English blurb.".to_string()),
                ("ja".to_string(), "日本語のあらすじ".to_string()),
            ]
        );
        let credits: Vec<(String, String)> = sqlx::query_as(
            "SELECT role, name FROM work_credit WHERE work_id = ? ORDER BY role, name",
        )
        .bind(&w)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            credits,
            vec![
                ("artist".to_string(), "Mitz Vah".to_string()),
                ("author".to_string(), "Fuse".to_string()),
            ]
        );

        // Re-sync: an updated English description overwrites in place; the same
        // credits don't duplicate (composite PK / INSERT OR IGNORE).
        input.descriptions = vec![("en".into(), "Revised English blurb.".into())];
        upsert_work_from_mangadex(&pool, "md-desc", &input)
            .await
            .unwrap();
        let en: String = sqlx::query_scalar(
            "SELECT description FROM work_description WHERE work_id = ? AND lang = 'en'",
        )
        .bind(&w)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(en, "Revised English blurb.");
        let credit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM work_credit WHERE work_id = ?")
                .bind(&w)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(credit_count, 2, "credits don't duplicate on re-sync");
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

    #[tokio::test]
    async fn token_index_backs_block_and_survives_merge_and_delete() {
        // H9: the exact-token block reads `work_alias_token`; folding/deleting a work
        // must fold/clean its token rows (no orphans, no lost recall).
        let pool = pool().await;
        let target = upsert_work_from_mangadex(
            &pool,
            "md-target",
            &WorkInput {
                primary_title: Some("Solo Leveling".into()),
                aliases: vec![Alias {
                    raw: "Solo Leveling".into(),
                    lang: Some("en".into()),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // A distinctive alias word ("npodamun") only the source carries.
        let source = create_work(
            &pool,
            &WorkInput {
                primary_title: Some("Na Honjaman Level Up".into()),
                aliases: vec![Alias {
                    raw: "Nodamun Level".into(),
                    lang: Some("en".into()),
                }],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        // Exact-token block finds each work by a whole normalized word.
        assert_eq!(
            candidate_work_ids_by_token(&pool, "leveling", 10)
                .await
                .unwrap(),
            vec![target.clone()]
        );
        assert_eq!(
            candidate_work_ids_by_token(&pool, "nodamun", 10)
                .await
                .unwrap(),
            vec![source.clone()]
        );
        // Too-short tokens return empty.
        assert!(candidate_work_ids_by_token(&pool, "a", 10)
            .await
            .unwrap()
            .is_empty());

        // Merge folds the source's tokens into the target and leaves no orphans.
        merge_works(&pool, &source, &target).await.unwrap();
        assert_eq!(
            candidate_work_ids_by_token(&pool, "nodamun", 10)
                .await
                .unwrap(),
            vec![target.clone()],
            "source's distinctive token now blocks to the target"
        );
        let orphan: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM work_alias_token WHERE work_id = ?")
                .bind(&source)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(orphan, 0, "no token rows left for the deleted source work");

        // Deleting the target cascades its token rows away entirely.
        delete_work_cascade(&pool, &target).await.unwrap();
        assert!(candidate_work_ids_by_token(&pool, "leveling", 10)
            .await
            .unwrap()
            .is_empty());
    }
}
