# Browse — filtration, series count, and the missing 100K

**Date:** 2026-07-23 · **Parent:** [`2026-07-23-architecture-review.md`](./2026-07-23-architecture-review.md)
**Status:** investigation complete, no code changed.

> **Superseded in part.** This document is the *evidence*. Implementation follows
> [`2026-07-23-architecture-decisions.md`](./2026-07-23-architecture-decisions.md),
> which corrects Stage 3 below: tags go to a new `work_source_tag`, **not**
> `work_tag` (AD-1); facet hygiene comes from MangaDex's `tag_group`, not a
> denylist (AD-2); pagination must be keyset, not offset (AD-3); Format is a
> persisted `work.comic_type` (AD-4); text search is FTS5 (AD-5); ratings live in
> `work_stats` (AD-6).

---

## 1. What the browse page actually sends

```graphql
query Search($query:String!,$page:Int,$genres:[String!],$minRating:Float,$maxRating:Float){
  search(query:$query,page:$page,genres:$genres,minRating:$minRating,maxRating:$maxRating){
    items{...SeriesFields} page hasNextPage total } }
```

`packages/api/src/operations.ts:101-124` → `graphql-backend.ts:117-126` →
`apps/reader/src/lib/data/source.ts:544-568` → `browse/+page.svelte:216,230,283`.

**Only three filter arguments ever leave the browser**: `genres`, `minRating`,
`maxRating` (`+page.svelte:123-129`). Everything else in the filter UI is
cosmetic.

---

## 2. The 10,704 vs 112,602 gap — CONFIRMED

### Measured row counts (snapshot 2026-07-23 06:00, read-only)

| Table / metric | Count |
| --- | ---: |
| `work` (canonical) | **112,602** |
| `source_series` — `mangadex` | **109,101** |
| `source_series` — `suwayomi` | 13,630 |
| works with **only** a MangaDex source | **100,005** |
| `suwayomi_series` | 13,663 (all `in_library=1`) |
| `chapter` | 804,729 (47,557 distinct works) |
| `work_cover` | 109,147 |
| **`work_tag`** | **0** |
| `reviews` | **3** |

### Where 10,704 comes from

`apps/server/src/series_cache.rs:327-333,368-379` — the browse `total` is exactly:

```sql
SELECT COUNT(*) FROM suwayomi_series s
WHERE s.in_library = 1
  AND NOT EXISTS (SELECT 1 FROM source_series ss JOIN work w ON w.id = ss.work_id
                  WHERE ss.source_type='suwayomi' AND ss.source_key = CAST(s.id AS TEXT)
                    AND w.is_nsfw = 1)
```

Reproduced on the snapshot: **10,646** anonymous / 13,643 with NSFW shown. Live
GraphQL returned 10,640 → 10,583 → 10,574 during the session.

**Root cause:** `search_catalogue` reads `suwayomi_series` and **never touches
`work`**. Enumerating every `QueryRoot` resolver (`graphql/mod.rs:1744-3886`),
the only canonical entry points are `canonical_series(workId)`,
`canonical_chapters`, `canonical_updates`, and `canonical_pages` — all single-item
or feed. **No resolver lists or paginates the `work` table.** The 100,005
MangaDex-only works are structurally unreachable from Browse.

Browse does *not* exclude series lacking covers or chapters — that was a
reasonable hypothesis and it is wrong. The only exclusions are `in_library = 1`
(all rows qualify) and the NSFW gate (removes 3,055, ~22%).

### MangaDex ingest is complete, not stalled

`catalogue_sync_state` shows `seed_done=1` for both `catalogue` and `chapters`;
last mangadex `source_series.last_seen` = 2026-07-23T00:35:39. Live
`api.mangadex.org/manga?limit=1` reports 93,763 for the default rating set; our
109,101 exceeds it because pornographic titles are included. The offset-cap
slide (`mangadex.rs:27`), `classify_page` raw-length fix (`:833-841`), and
per-page checkpoint (`:925-933`) all look sound. **No early-exit found.**

### What *is* stalled: Suwayomi ingest

`source_ingest_job` for source `2499283573021220255` — which supplies **10,015 of
13,663 `suwayomi_series` (73%)** — is in state `failed` at `pages_done=500`,
`items_seen=10000`, error `HTTP error 400` from `fetchSourceManga`, started
2026-07-19T13:52, finished 18:19, **never retried since**. `ingest.rs:42
MAX_PAGES=1_000` was not the limiter; a single page-fetch error fails the whole
job with no resume (`ingest.rs:302-304` documents this as intended: "the admin
can restart").

### Observed drift — SUSPECTED

Browse `total` fell monotonically (10,646 → 10,574) over 20 minutes while the
catalogue is nominally growing. Mechanism: dedup/ingest links Suwayomi series to
canonical works and ORs in the NSFW flag (`graphql/mod.rs:6588-6591`
`mark_work_nsfw`); each new link removes that series from anonymous browse via
the `NOT EXISTS … w.is_nsfw=1` clause. 3,055 rows are already suppressed this
way. Not instrumented on the live writer, hence SUSPECTED.

**Side effect worth noting:** a text `search` **persists new rows** into
`suwayomi_series` via the Suwayomi fetch — ids 13,953+ appeared during a
"naruto" probe. User searches mutate the catalogue.

---

## 3. Why filters don't filter — CONFIRMED

### F1 · Format, Status, and Sort are client-side over a 20-row slice

`browse/+page.svelte:316-336`:
```js
if (types.length && !types.includes(m.type)) return false;
if (status !== 'any' && m.status !== status) return false;
… return [...list].sort(sorters[sort]);
```

`rows` holds only page 1 — `PAGE_SIZE = 20` (`graphql/mod.rs:17`). The `search`
resolver (`mod.rs:2255-2308`) has **no `type`, `status`, `sort`, or `year`
argument at all**. Server `ORDER BY` is hard-coded
`COALESCE(s.last_fetched_at, s.updated_at) DESC, s.id DESC`
(`series_cache.rs:388`).

Measured: Format = Manga yields **2 results out of 10,583** (page-1 distribution
is MANHWA 14 / MANHUA 4 / MANGA 2). Load-more makes it worse — it appends the
next *unfiltered* 20, so the client filter's denominator grows 20 at a time.

The UI documents this as intended (`+page.svelte:113-117,314-315`). The defect is
that it is *presented to the user* as a catalogue filter.

### F2 · `type` has no persisted column

`graphql/types.rs:861-905` `resolve_comic_type()` derives Manga/Manhwa/Manhua at
map time from `content_type_override` → `original_language` → genre heuristics →
title script. Nothing in `suwayomi_series` stores it, so Format **cannot** be
pushed into SQL without a schema change. `work.original_language` is the natural
key and *is* populated (88,377 ja / 10,578 ko / 6,030 zh) — but browse never
joins `work`.

### F3 · The rating slider is a kill-switch — worst single defect

`series_cache.rs:356-362` filters on `COALESCE(r.avg, 0)` where `r` is
`AVG(score) FROM reviews GROUP BY series_id` — **local user reviews**, not any
source or MangaDex score. The DB has **3 reviews across 3 series**.

| Filter | `total` |
| --- | ---: |
| none | 10,574 |
| `minRating: 0.5` (smallest step above 0) | **3** |
| `minRating: 1` | **3** |
| `maxRating: 9` | 10,618 |

The min handle collapses the catalogue at one notch; the max handle does nothing
(`COALESCE`→0 keeps every unrated series). Every card also renders `0.0`, which
additionally makes the "Top rated" client sort a no-op and degenerates
"Trending" to chapter-count.

### F4 · Genre filtering is correct — the one facet that works

`series_cache.rs:317-402`. Genres are `LIKE '%"<g>"%' ESCAPE '\'` OR'd together,
AND'ed with rating and NSFW, applied **pre-pagination across the whole cache**.
Measured: Action 3,910 / Romance 4,708 / Comedy 4,793 of 10,574. Effective
combination is `(genre1 OR genre2 …) AND rating AND nsfw`, then client-side
`AND (type1 OR type2) AND status`.

### F5 · Facet counts don't match results

`series_cache.rs:415-419`: `SELECT genre FROM suwayomi_series WHERE genre IS NOT
NULL` — **no `in_library` filter, no NSFW gate**, and counting is
case-*sensitive* in Rust (`:421-430`) while the SQL match is case-*insensitive*.

- Rail shows "Romance 6180"; selecting it returns 4,708.
- 322 facets with **32 case-duplicate groups** (`Romance`/`romance`,
  `Slice of Life`/`Slice of life`/`Slice Of Life`) — picking the small-count
  variant returns the big set and vice-versa.
- NSFW-only genres leak to anonymous viewers.
- The list is polluted with non-genres: the **#1 facet is "Japanese" (7,949)**,
  plus "Content rating: Suggestive", "Full Color", "Long Strip", "Web Comic",
  "Adaptation".

### F6 · Text search bypasses the catalogue entirely

`graphql/mod.rs:2293-2307` — a non-empty query skips the DB and calls
`st.suwayomi.fetch_source(FetchType::Search, page, q)` (`suwayomi.rs:355-404`),
browsing **one resolved default source** live. Filters are then applied to that
20-item page (`mod.rs:1018-1055`), `total` is returned as `None` (`:2306`), and
`hasNextPage` is the source's raw flag.

Reproduced live:
```
search(query:"naruto")                      → 20 items, one sourceId, total: null
search(query:"naruto", genres:["Romance"])  → items: [], hasNextPage: true, total: null
```
The UI simultaneously renders "0+ series", the "No matches found" empty state,
**and** a working Load-more button. The 112,602-work catalogue is never searched
by text.

### F7 · Federated path silently ignores server filters

`+page.svelte:141-248` — when signed in *and* the query is non-empty,
`getFederatedSearch(q)` is used and genre/rating are re-applied client-side
(`:321-325`). The cache signature deliberately omits filters (`:158-159`), so a
filter change on that path neither refetches nor invalidates.

### F8 · No year facet

No UI control and no resolver argument, despite `work.year` populated for 85,477
rows (range 1812–2027).

---

## 4. "20+ series" — CONFIRMED, and trivially fixable

`browse/+page.svelte:582-587`:
```svelte
`${results.length}${hasNext ? '+' : ''} series`
```

`results.length` is the client-filtered *current page* array — hence literally
"20+".

The real total **is already fetched**: `SeriesPage.total` (`graphql/types.rs:407-412`),
computed catalogue-wide pre-pagination (`series_cache.rs:368-379`), requested in
`operations.ts:120`, plumbed through `source.ts:557` into `totalCount`
(`+page.svelte:120,235`) — and rendered *only* in the load-more footer as
"Showing 20 of 10574" (`:658-660`). The header just doesn't use it. It is not
capped.

**Caveat for the fix:** `total` is `null` for any text query (`mod.rs:2306`), so
the header needs a fallback for that path.

---

## 5. Prioritized defects

| # | Sev | Defect | Location |
| --- | --- | --- | --- |
| B1 | **P0** | Browse reads `suwayomi_series` only; 100,005 MangaDex-only works unreachable; no resolver lists `work` | `series_cache.rs:317-402` |
| B2 | **P0** | Rating filter reads a 3-row `reviews` table; `minRating ≥ 0.5` → 3 results | `series_cache.rs:356-362`, `mod.rs:1041-1051` |
| B3 | P1 | Format/Status/Sort are fake — no server args, applied to 20 rows | `+page.svelte:316-336` vs `mod.rs:2255-2262` |
| B4 | P1 | Header shows `results.length + '+'`; real `totalCount` already in scope | `+page.svelte:586` |
| B5 | P1 | Text search hits one live source, filters post-fetch, returns `total:null` + bogus `hasNextPage` | `mod.rs:2293-2307`, `suwayomi.rs:355-362` |
| B6 | P2 | Facet counts ignore `in_library`/NSFW, case-split 32 groups, include non-genres | `series_cache.rs:415-430` |
| B7 | P2 | `work_tag` empty → MangaDex works have no genres at all | `catalog/mod.rs:361-393` |
| B8 | P2 | Dominant source (73% of catalogue) ingest `failed` since 2026-07-19, no retry | `ingest.rs:302-304` |
| B9 | P3 | No year facet despite `work.year` on 85,477 rows | — |

---

## 6. Plan

### Stage 1 — reader-only deploy, no rebuild

- **B4** — render `totalCount` in the header; fall back to the current
  `n+` form when `total` is null (text-search path).
- **B2 (mitigation)** — hide or disable the rating slider until the MangaDex
  statistics ingest lands (Stage 3, step 2). A filter that silently deletes
  99.97% of results is worse than no filter. This is temporary: the facet comes
  back on real data, it is not being dropped.
- **B3 (honesty)** — until server-side facets exist, label Format/Status/Sort as
  operating on loaded results, or disable them. Do not present page-slice
  filtering as catalogue filtering.

### Stage 2 — batched into the Tier-2 rebuild

- **B6** — apply `in_library` + NSFW gates to `genre_facets`; normalise case in
  SQL (`LOWER(genre)`) so counts and matching agree; add a denylist for
  origin/format tags ("Japanese", "Long Strip", "Content rating: *", "Full
  Color", "Web Comic", "Adaptation").
- **B3 (partial)** — `status` is SQL-able against `suwayomi_series.status`
  today; add the resolver argument. Add an `orderBy` argument to replace the
  hard-coded `ORDER BY` at `series_cache.rs:388`.
- **B8** — add resume-from-checkpoint to `source_ingest_job` so a single upstream
  400 doesn't kill a 10,000-item job permanently; re-run the failed job.
- Index migration (see parent doc Tier 2) — `work(primary_title)`,
  `work(updated_at)` land here and are prerequisites for Stage 3.

### Stage 3 — canonical catalogue read path

This is the item that actually answers "why only 10,704".

**Corpus decision (2026-07-23): the full 112,602 works are browsable.** Not a
curated subset. That makes steps 1 and 2 below hard prerequisites — with the full
corpus exposed, any gap in tags or ratings is visible across 100K rows rather
than hidden behind a curation filter.

1. **Ingest MangaDex tags into `work_tag`** (currently 0 rows) — *blocking*.
   Without this the catalogue ships 100,005 genre-less, unfilterable rows and the
   genre facet looks more broken than it does today. Also re-enable
   `METADATA_BACKFILL`, which is off (sync doc S9).
2. **Ingest MangaDex statistics into `work`** — *blocking for B2*. Verified live:
   `GET /statistics/manga?manga[]=…` returns `rating.average`, `rating.bayesian`
   and `follows`. **Batch cap is 100 and it truncates silently** (120 requested →
   HTTP 200 with 100 entries), so chunk at exactly 100 and assert
   `returned == requested`. Full pass = 109,101 ÷ 100 = **1,092 requests ≈ 4.5 min**
   at the existing 4 req/s limiter.
3. **Add `catalogueSearch`** — a resolver over `work` + `work_cover` +
   `source_series` with genuine server-side arguments: `genres`, `status`,
   `year`, `originalLanguage` (→ Format, per F2), `contentRating`, `minRating`,
   `orderBy`, `nsfw`. Pre-pagination filtering, real `total`.
4. **Point Browse at it**, keeping the Suwayomi path for the "available to read
   now" filter. Since only 47,557 of 112,602 works have cached chapters, surface
   readability as a **filter and a badge**, not by hiding rows — otherwise the
   full-catalogue decision is silently undone at the query layer.
5. **B5** — route text search through the same resolver
   (`work_alias_token` already has a covering `(token, work_id)` index and
   1,250,998 rows) instead of a single live source.
6. **B2 (real fix)** — filter and sort on **`rating_bayesian`**, MangaDex's own
   low-vote-count-corrected value, so a 10.0 from three voters cannot top the
   catalogue. Keep the 3-row local `reviews` average as a separate, separately
   labelled signal; do not merge them into one number. `follows` (free in the
   same response) replaces the chapter-count proxy behind "Trending".

> **Scheduler boundary — do not skip.** Browsability must not imply
> scannability. 13,850 series already saturate the scan loop at 43% duty cycle
> (sync doc §2). Canonical works are browsed from `work`/`chapter`; only
> Suwayomi-linked series enter `series_scan_state`. If the full catalogue leaks
> into the scan pool, a scheduler that is 5.7× short becomes roughly 50× short.

### Verification

- `catalogueSearch(genres:["Romance"])` returns a `total` in the thousands and
  the header shows it.
- Format = Manga returns a five-figure count, not 2.
- Rating slider at min+1 notch does not collapse the result set.
- Facet count for Romance equals the result count for Romance.
- `search(query:"naruto")` returns results drawn from `work`, with a non-null
  `total`, and no Load-more button on an empty result.
