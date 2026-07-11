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
    let pattern = format!("%{}%", token.replace('%', "").replace('_', ""));
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
            demographic, content_rating, is_nsfw, author, artist, cover_phash, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
           primary_title = excluded.primary_title, primary_lang = excluded.primary_lang, \
           description = excluded.description, year = excluded.year, \
           original_language = excluded.original_language, status = excluded.status, \
           demographic = excluded.demographic, content_rating = excluded.content_rating, \
           is_nsfw = excluded.is_nsfw, author = excluded.author, artist = excluded.artist, \
           cover_phash = COALESCE(excluded.cover_phash, work.cover_phash), \
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
            demographic, content_rating, is_nsfw, author, artist, cover_phash, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

/// Read the sync cursor for a job (`catalogue` | `chapters`). `None` means the job
/// has never completed a cycle → the caller should do a full `createdAt` seed.
pub async fn get_sync_cursor(pool: &SqlitePool, job: &str) -> Result<Option<String>> {
    let v = sqlx::query_scalar::<_, String>(
        "SELECT last_synced_at FROM catalogue_sync_state WHERE job = ?",
    )
    .bind(job)
    .fetch_optional(pool)
    .await?;
    Ok(v)
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
    async fn sync_cursor_round_trips() {
        let pool = pool().await;
        assert_eq!(get_sync_cursor(&pool, "catalogue").await.unwrap(), None);
        set_sync_cursor(&pool, "catalogue", "2026-07-11T00:00:00")
            .await
            .unwrap();
        assert_eq!(
            get_sync_cursor(&pool, "catalogue")
                .await
                .unwrap()
                .as_deref(),
            Some("2026-07-11T00:00:00")
        );
        // Upsert overwrites, and jobs are independent.
        set_sync_cursor(&pool, "catalogue", "2026-07-12T00:00:00")
            .await
            .unwrap();
        assert_eq!(
            get_sync_cursor(&pool, "catalogue")
                .await
                .unwrap()
                .as_deref(),
            Some("2026-07-12T00:00:00")
        );
        assert_eq!(get_sync_cursor(&pool, "chapters").await.unwrap(), None);
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
