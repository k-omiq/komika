//! DB cache for Suwayomi source-series metadata + chapter lists (S1).
//!
//! The reader's Suwayomi paths (`series`, `chapters`, `updates`, `discovery`) used
//! to live-fetch from Suwayomi on every request (each of which fetches the upstream
//! source over the network — the latency the user hit). These helpers persist the
//! raw Suwayomi shapes into `suwayomi_series` / `suwayomi_chapter` on scan + ingest,
//! and read them back, so subsequent loads are served from SQLite. Only the cover
//! REFERENCE is stored (Worker-proxied) — never cover bytes (memory posture).

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::suwayomi::{ChapterCount, SuwayomiChapter, SuwayomiManga, SuwayomiSourceLang};

/// TTL for cached series METADATA before a cache hit revalidates upstream. Series
/// metadata (title/status/description/cover) changes slowly, so a generous window.
pub const SERIES_TTL_SECS: i64 = 6 * 60 * 60; // 6h
/// TTL for a cached chapter list before a cache hit revalidates upstream. Chapters
/// arrive more often than metadata changes, so a tighter window.
pub const CHAPTERS_TTL_SECS: i64 = 90 * 60; // 90m

/// Whether a cached row's last-fetch timestamp is still within `ttl_secs` of now.
/// A missing or unparseable timestamp is treated as STALE (forces a revalidation),
/// never as fresh — the safe direction for a freshness gate.
pub fn is_fresh(fetched_at: Option<&str>, ttl_secs: i64) -> bool {
    let Some(ts) = fetched_at else { return false };
    let Ok(t) = DateTime::parse_from_rfc3339(ts) else {
        return false;
    };
    Utc::now()
        .signed_duration_since(t.with_timezone(&Utc))
        .num_seconds()
        < ttl_secs
}

/// Upsert one series' display metadata into the cache. `chapter_count` prefers the
/// manga's own count, else keeps whatever is already stored (so a detail fetch
/// without a fresh count doesn't zero it).
pub async fn put_series(pool: &SqlitePool, m: &SuwayomiManga) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let genre = serde_json::to_string(&m.genre).unwrap_or_else(|_| "[]".to_string());
    let lang = m.source.as_ref().and_then(|s| s.lang.clone());
    let count = m.chapters.as_ref().map(|c| c.total_count);
    // `series_fetched_at` records ONLY a metadata fetch (this call) — distinct from
    // `updated_at`, which `put_chapters` also bumps — so the reader's series-metadata
    // TTL revalidates on the right event.
    sqlx::query(
        "INSERT INTO suwayomi_series \
           (id, title, thumbnail_url, author, artist, description, genre, status, \
            in_library, in_library_at, last_fetched_at, source_id, lang, chapter_count, \
            updated_at, created_at, series_fetched_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, COALESCE(?, 0), ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
           title = excluded.title, thumbnail_url = excluded.thumbnail_url, \
           author = excluded.author, artist = excluded.artist, description = excluded.description, \
           genre = excluded.genre, status = excluded.status, in_library = excluded.in_library, \
           in_library_at = excluded.in_library_at, last_fetched_at = excluded.last_fetched_at, \
           source_id = excluded.source_id, lang = excluded.lang, \
           chapter_count = COALESCE(?, suwayomi_series.chapter_count), \
           updated_at = excluded.updated_at, \
           created_at = COALESCE(suwayomi_series.created_at, excluded.created_at), \
           series_fetched_at = excluded.series_fetched_at",
    )
    .bind(m.id)
    .bind(&m.title)
    .bind(&m.thumbnail_url)
    .bind(&m.author)
    .bind(&m.artist)
    .bind(&m.description)
    .bind(&genre)
    .bind(&m.status)
    .bind(m.in_library as i64)
    .bind(&m.in_library_at)
    .bind(&m.last_fetched_at)
    .bind(&m.source_id)
    .bind(&lang)
    .bind(count)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .bind(count)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the cached chapter list for a manga (delete + insert in one tx), and
/// sync the series' `chapter_count` to the stored count.
pub async fn put_chapters(
    pool: &SqlitePool,
    manga_id: i64,
    chapters: &[SuwayomiChapter],
) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM suwayomi_chapter WHERE manga_id = ?")
        .bind(manga_id)
        .execute(&mut *tx)
        .await?;
    for c in chapters {
        sqlx::query(
            "INSERT INTO suwayomi_chapter \
               (id, manga_id, name, chapter_number, scanlator, upload_date, \
                is_read, is_bookmarked, is_downloaded, last_page_read, page_count, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(c.id)
        .bind(c.manga_id)
        .bind(&c.name)
        .bind(c.chapter_number)
        .bind(&c.scanlator)
        .bind(&c.upload_date)
        .bind(c.is_read as i64)
        .bind(c.is_bookmarked as i64)
        .bind(c.is_downloaded as i64)
        .bind(c.last_page_read)
        .bind(c.page_count)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }
    // Store the deduped "main" chapter count (distinct whole numbers), NOT the raw row
    // count — a source lists one row per (chapter_number, scanlator) plus ".5" extras,
    // so `chapters.len()` inflates the total (e.g. Tsukimichi: 117 real → 151 rows).
    let count = crate::catalog::main_chapter_count(chapters.iter().map(|c| c.chapter_number));
    sqlx::query("UPDATE suwayomi_series SET chapter_count = ?, updated_at = ? WHERE id = ?")
        .bind(count)
        .bind(&now)
        .bind(manga_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// A cached-series row shape (reconstructed into a `SuwayomiManga`).
#[derive(sqlx::FromRow)]
struct SeriesRow {
    id: i64,
    title: String,
    thumbnail_url: Option<String>,
    author: Option<String>,
    artist: Option<String>,
    description: Option<String>,
    genre: Option<String>,
    status: String,
    in_library: i64,
    in_library_at: Option<String>,
    last_fetched_at: Option<String>,
    source_id: String,
    lang: Option<String>,
    chapter_count: i64,
}

impl From<SeriesRow> for SuwayomiManga {
    fn from(r: SeriesRow) -> Self {
        let genre: Vec<String> = r
            .genre
            .as_deref()
            .and_then(|g| serde_json::from_str(g).ok())
            .unwrap_or_default();
        SuwayomiManga {
            id: r.id,
            title: r.title,
            thumbnail_url: r.thumbnail_url,
            author: r.author,
            artist: r.artist,
            description: r.description,
            genre,
            status: r.status,
            in_library: r.in_library != 0,
            in_library_at: r.in_library_at,
            last_fetched_at: r.last_fetched_at,
            source_id: r.source_id,
            source: Some(SuwayomiSourceLang { lang: r.lang }),
            chapters: Some(ChapterCount {
                total_count: r.chapter_count,
            }),
        }
    }
}

const SERIES_SELECT: &str = "SELECT id, title, thumbnail_url, author, artist, description, genre, \
     status, in_library, in_library_at, last_fetched_at, source_id, lang, chapter_count \
     FROM suwayomi_series";

/// Load one cached series, or `None` on a cache miss.
pub async fn get_series(pool: &SqlitePool, id: i64) -> Result<Option<SuwayomiManga>> {
    let row = sqlx::query_as::<_, SeriesRow>(&format!("{SERIES_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(Into::into))
}

/// Load one cached series together with whether its METADATA is still fresh (fetched
/// within `SERIES_TTL_SECS`). `None` on a cache miss. The reader uses the flag to
/// decide whether to revalidate upstream on a cache hit.
pub async fn get_series_fresh(pool: &SqlitePool, id: i64) -> Result<Option<(SuwayomiManga, bool)>> {
    let row = sqlx::query_as::<_, SeriesRow>(&format!("{SERIES_SELECT} WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else { return Ok(None) };
    let fetched: Option<String> =
        sqlx::query_scalar("SELECT series_fetched_at FROM suwayomi_series WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .flatten();
    let fresh = is_fresh(fetched.as_deref(), SERIES_TTL_SECS);
    Ok(Some((row.into(), fresh)))
}

/// When this manga's cached chapter list was last fetched (the max per-row
/// `updated_at` written by `put_chapters`), or `None` if no chapters are cached.
/// `put_series` never touches chapter rows, so this cleanly tracks chapter fetches.
pub async fn chapters_last_fetched(pool: &SqlitePool, manga_id: i64) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT MAX(updated_at) FROM suwayomi_chapter WHERE manga_id = ?",
        )
        .bind(manga_id)
        .fetch_optional(pool)
        .await?
        .flatten(),
    )
}

/// Load the cached chapter list for a manga (source order: newest first by number).
pub async fn get_chapters(pool: &SqlitePool, manga_id: i64) -> Result<Vec<SuwayomiChapter>> {
    let rows = sqlx::query_as::<_, ChapterRow>(
        "SELECT id, manga_id, name, chapter_number, scanlator, upload_date, \
                is_read, is_bookmarked, is_downloaded, last_page_read, page_count \
         FROM suwayomi_chapter WHERE manga_id = ? ORDER BY chapter_number DESC, id DESC",
    )
    .bind(manga_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

#[derive(sqlx::FromRow)]
struct ChapterRow {
    id: i64,
    manga_id: i64,
    name: String,
    chapter_number: f64,
    scanlator: Option<String>,
    upload_date: Option<String>,
    is_read: i64,
    is_bookmarked: i64,
    is_downloaded: i64,
    last_page_read: i64,
    page_count: i64,
}

impl From<ChapterRow> for SuwayomiChapter {
    fn from(r: ChapterRow) -> Self {
        SuwayomiChapter {
            id: r.id,
            manga_id: r.manga_id,
            name: r.name,
            chapter_number: r.chapter_number,
            scanlator: r.scanlator,
            upload_date: r.upload_date,
            is_read: r.is_read != 0,
            is_bookmarked: r.is_bookmarked != 0,
            is_downloaded: r.is_downloaded != 0,
            last_page_read: r.last_page_read,
            page_count: r.page_count,
        }
    }
}

/// Cached in-library series, most-recently-updated first — serves `discovery`
/// without a live source browse. `limit` caps the result.
pub async fn library(pool: &SqlitePool, limit: i64) -> Result<Vec<SuwayomiManga>> {
    let rows = sqlx::query_as::<_, SeriesRow>(&format!(
        "{SERIES_SELECT} WHERE in_library = 1 \
         ORDER BY COALESCE(last_fetched_at, updated_at) DESC, id DESC LIMIT ?"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Cached catalogue series ordered by when they were FIRST added to our cache
/// (`created_at` desc) — powers the home "Latest Added" row. Distinct from
/// {@link library} (most-recently-updated) and the source "Latest" endpoint
/// (recently-updated upstream): this is "what the catalogue gained most recently".
/// `limit` caps the result.
pub async fn recently_added(pool: &SqlitePool, limit: i64) -> Result<Vec<SuwayomiManga>> {
    let rows = sqlx::query_as::<_, SeriesRow>(&format!(
        "{SERIES_SELECT} WHERE in_library = 1 \
         ORDER BY COALESCE(created_at, updated_at) DESC, id DESC LIMIT ?"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Catalogue-wide search over the persisted cache with genre + rating filters,
/// applied in SQL and paginated (F2) — so `search(genres:["Action"])` returns the
/// FULL Action set (paged), not a slice of the first N. Returns `(total, page)`.
///
/// Genres are stored as a JSON array string in `suwayomi_series.genre`; a genre
/// matches via a `LIKE '%"<genre>"%'` on that column (LIKE is ASCII
/// case-insensitive, and the surrounding quotes make it an exact element match, so
/// "Drama" doesn't match "Melodrama"). `genres` matches ANY. Rating filters against
/// the per-series user-review average (`reviews`, keyed by the Suwayomi id as text).
/// NSFW series are excluded unless `show_nsfw`. For the small cached catalogue a
/// scan is fine; at scale a normalized `series_genre(series_id, genre)` index table
/// would replace the LIKE scan.
#[allow(clippy::too_many_arguments)]
pub async fn search_catalogue(
    pool: &SqlitePool,
    genres: &[String],
    min_rating: Option<f64>,
    max_rating: Option<f64>,
    show_nsfw: bool,
    page: i64,
    page_size: i64,
) -> Result<(i64, Vec<SuwayomiManga>)> {
    // Build the shared WHERE + its bind values.
    let mut where_sql = String::from(
        "s.in_library = 1 \
         AND (? = 1 OR NOT EXISTS ( \
            SELECT 1 FROM source_series ss JOIN work w ON w.id = ss.work_id \
            WHERE ss.source_type = 'suwayomi' AND ss.source_key = CAST(s.id AS TEXT) \
              AND w.is_nsfw = 1))",
    );
    // Genre ANY-match.
    let genre_patterns: Vec<String> = genres
        .iter()
        .map(|g| g.trim())
        .filter(|g| !g.is_empty())
        // Escape LIKE metacharacters in the (source-controlled) genre name so a
        // genre containing `%`/`_` matches literally, not as a wildcard. The
        // matching clause uses `ESCAPE '\'`.
        .map(|g| {
            let esc = g
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%\"{esc}\"%")
        })
        .collect();
    if !genre_patterns.is_empty() {
        let ors = std::iter::repeat_n("s.genre LIKE ? ESCAPE '\\'", genre_patterns.len())
            .collect::<Vec<_>>()
            .join(" OR ");
        where_sql.push_str(&format!(" AND ({ors})"));
    }
    // Rating range against the review average (0 when unrated).
    if min_rating.is_some() {
        where_sql.push_str(" AND COALESCE(r.avg, 0) >= ?");
    }
    if max_rating.is_some() {
        where_sql.push_str(" AND COALESCE(r.avg, 0) <= ?");
    }

    let join = "LEFT JOIN (SELECT series_id, AVG(score) AS avg FROM reviews GROUP BY series_id) r \
                ON r.series_id = CAST(s.id AS TEXT)";

    // total (filtered, catalogue-wide)
    let count_sql = format!("SELECT COUNT(*) FROM suwayomi_series s {join} WHERE {where_sql}");
    let mut cq = sqlx::query_scalar::<_, i64>(&count_sql).bind(show_nsfw as i64);
    for p in &genre_patterns {
        cq = cq.bind(p);
    }
    if let Some(v) = min_rating {
        cq = cq.bind(v);
    }
    if let Some(v) = max_rating {
        cq = cq.bind(v);
    }
    let total: i64 = cq.fetch_one(pool).await?;

    // page
    let offset = (page.max(1) - 1) * page_size;
    let rows_sql = format!(
        "SELECT s.id, s.title, s.thumbnail_url, s.author, s.artist, s.description, s.genre, \
                s.status, s.in_library, s.in_library_at, s.last_fetched_at, s.source_id, s.lang, \
                s.chapter_count \
         FROM suwayomi_series s {join} WHERE {where_sql} \
         ORDER BY COALESCE(s.last_fetched_at, s.updated_at) DESC, s.id DESC LIMIT ? OFFSET ?"
    );
    let mut rq = sqlx::query_as::<_, SeriesRow>(&rows_sql).bind(show_nsfw as i64);
    for p in &genre_patterns {
        rq = rq.bind(p);
    }
    if let Some(v) = min_rating {
        rq = rq.bind(v);
    }
    if let Some(v) = max_rating {
        rq = rq.bind(v);
    }
    let rows = rq.bind(page_size).bind(offset).fetch_all(pool).await?;
    Ok((total, rows.into_iter().map(Into::into).collect()))
}

/// How many series are cached (any). Lets `discovery` decide DB-vs-live.
pub async fn count(pool: &SqlitePool) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM suwayomi_series")
        .fetch_one(pool)
        .await?)
}

/// Distinct genre/tag facets across the cached catalogue with per-genre series
/// counts, most common first (S4). Powers the search UI's genre filter — the FULL
/// set the sources provide, not a hardcoded list. Genres are stored as JSON arrays,
/// so they're parsed + counted in Rust (one scan of the cache).
pub async fn genre_facets(pool: &SqlitePool) -> Result<Vec<(String, i64)>> {
    let genres: Vec<Option<String>> =
        sqlx::query_scalar("SELECT genre FROM suwayomi_series WHERE genre IS NOT NULL")
            .fetch_all(pool)
            .await?;
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for g in genres.into_iter().flatten() {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&g) {
            for name in list {
                let name = name.trim();
                if !name.is_empty() {
                    *counts.entry(name.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
    let mut out: Vec<(String, i64)> = counts.into_iter().collect();
    // Most common first, then alphabetical for stable ties.
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn manga(id: i64, title: &str) -> SuwayomiManga {
        SuwayomiManga {
            id,
            title: title.into(),
            thumbnail_url: Some(format!("/thumb/{id}")),
            author: Some("Author".into()),
            artist: Some("Artist".into()),
            description: Some("Desc".into()),
            genre: vec!["Action".into(), "Comedy".into()],
            status: "ONGOING".into(),
            in_library: true,
            in_library_at: Some("2026-01-01T00:00:00Z".into()),
            last_fetched_at: Some("2026-01-02T00:00:00Z".into()),
            source_id: "src-1".into(),
            source: Some(SuwayomiSourceLang {
                lang: Some("en".into()),
            }),
            chapters: Some(ChapterCount { total_count: 3 }),
        }
    }

    fn chapter(id: i64, manga_id: i64, num: f64) -> SuwayomiChapter {
        SuwayomiChapter {
            id,
            manga_id,
            name: format!("Ch {num}"),
            chapter_number: num,
            scanlator: Some("Scan".into()),
            upload_date: Some("1600000000000".into()),
            is_read: false,
            is_bookmarked: false,
            is_downloaded: false,
            last_page_read: 0,
            page_count: 20,
        }
    }

    #[tokio::test]
    async fn round_trips_series_and_chapters() {
        let pool = pool().await;
        put_series(&pool, &manga(1, "Solo Leveling")).await.unwrap();
        let got = get_series(&pool, 1).await.unwrap().expect("cached");
        assert_eq!(got.title, "Solo Leveling");
        assert_eq!(got.genre, vec!["Action", "Comedy"]);
        assert_eq!(got.source.unwrap().lang.as_deref(), Some("en"));
        assert!(got.in_library);
        assert_eq!(got.thumbnail_url.as_deref(), Some("/thumb/1"));

        // Chapters replace + sync count.
        put_chapters(
            &pool,
            1,
            &[
                chapter(10, 1, 1.0),
                chapter(11, 1, 2.0),
                chapter(12, 1, 3.0),
            ],
        )
        .await
        .unwrap();
        let chs = get_chapters(&pool, 1).await.unwrap();
        assert_eq!(chs.len(), 3);
        assert_eq!(chs[0].chapter_number, 3.0, "newest first");
        let cnt: i64 = sqlx::query_scalar("SELECT chapter_count FROM suwayomi_series WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(cnt, 3, "count synced to stored chapters");

        // Re-put fewer chapters replaces (no accumulation).
        put_chapters(&pool, 1, &[chapter(10, 1, 1.0)])
            .await
            .unwrap();
        assert_eq!(get_chapters(&pool, 1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_catalogue_filters_in_sql_across_whole_cache() {
        let pool = pool().await;
        // 5 Action series + 2 non-Action, all in library.
        for i in 1..=5 {
            put_series(&pool, &manga(i, &format!("Action {i}")))
                .await
                .unwrap(); // Action, Comedy
        }
        for i in 6..=7 {
            let mut m = manga(i, &format!("Drama {i}"));
            m.genre = vec!["Drama".into()];
            put_series(&pool, &m).await.unwrap();
        }

        // Genre filter spans the whole cache with page-2 pagination (page_size 2).
        let (total, page1) = search_catalogue(&pool, &["Action".into()], None, None, true, 1, 2)
            .await
            .unwrap();
        assert_eq!(total, 5, "catalogue-wide Action total, not a slice");
        assert_eq!(page1.len(), 2);
        let (_t, page3) = search_catalogue(&pool, &["Action".into()], None, None, true, 3, 2)
            .await
            .unwrap();
        assert_eq!(page3.len(), 1, "page 3 has the 5th Action series");

        // Case-insensitive + exact element (Drama, not a substring of another).
        let (dt, _) = search_catalogue(&pool, &["drama".into()], None, None, true, 1, 20)
            .await
            .unwrap();
        assert_eq!(dt, 2);

        // Rating filter: seed a review so one series has avg 8; minRating 5 keeps only it.
        sqlx::query(
            "INSERT INTO users (id, username, email, password_hash, created_at) VALUES ('u1','u','u@e','h','t')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO reviews (id, series_id, user_id, score, body, created_at, updated_at) \
             VALUES ('rv1', '1', 'u1', 8, 'ok', 't', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let (rt, rows) = search_catalogue(&pool, &[], Some(5.0), None, true, 1, 20)
            .await
            .unwrap();
        assert_eq!(rt, 1, "only the reviewed (avg 8) series clears minRating 5");
        assert_eq!(rows[0].id, 1);
    }

    #[tokio::test]
    async fn genre_facets_counts_across_catalogue() {
        let pool = pool().await;
        // Two series share "Action"; one has "Comedy".
        put_series(&pool, &manga(1, "A")).await.unwrap(); // Action, Comedy
        let mut m2 = manga(2, "B");
        m2.genre = vec!["Action".into(), "Drama".into()];
        put_series(&pool, &m2).await.unwrap();
        let facets = genre_facets(&pool).await.unwrap();
        // Action (2) first, then Comedy/Drama (1) alphabetically.
        assert_eq!(facets[0], ("Action".to_string(), 2));
        assert!(facets.contains(&("Comedy".to_string(), 1)));
        assert!(facets.contains(&("Drama".to_string(), 1)));
    }

    #[tokio::test]
    async fn recently_added_orders_by_first_add_and_preserves_it() {
        let pool = pool().await;
        for i in 1..=3 {
            put_series(&pool, &manga(i, &format!("S{i}")))
                .await
                .unwrap();
        }
        // A non-library series is excluded from the catalogue "Latest Added" row.
        let mut out = manga(4, "Excluded");
        out.in_library = false;
        put_series(&pool, &out).await.unwrap();

        // Pin distinct first-add times so ordering is deterministic (1 oldest, 3 newest).
        for (id, ts) in [
            (1, "2026-01-01T00:00:00Z"),
            (2, "2026-02-01T00:00:00Z"),
            (3, "2026-03-01T00:00:00Z"),
        ] {
            sqlx::query("UPDATE suwayomi_series SET created_at = ? WHERE id = ?")
                .bind(ts)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }

        let recent = recently_added(&pool, 10).await.unwrap();
        assert_eq!(
            recent.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![3, 2, 1],
            "newest-added first, in-library only"
        );

        // Re-upserting an existing series must NOT reset its first-add time.
        put_series(&pool, &manga(1, "S1 refreshed")).await.unwrap();
        let kept: Option<String> =
            sqlx::query_scalar("SELECT created_at FROM suwayomi_series WHERE id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            kept.as_deref(),
            Some("2026-01-01T00:00:00Z"),
            "created_at preserved on upsert"
        );
    }

    #[tokio::test]
    async fn library_and_count() {
        let pool = pool().await;
        put_series(&pool, &manga(1, "A")).await.unwrap();
        let mut m2 = manga(2, "B");
        m2.in_library = false;
        put_series(&pool, &m2).await.unwrap();
        assert_eq!(count(&pool).await.unwrap(), 2);
        let lib = library(&pool, 10).await.unwrap();
        assert_eq!(lib.len(), 1, "only in-library series");
        assert_eq!(lib[0].id, 1);
    }
}
