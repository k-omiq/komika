# Architecture decisions — closing the open items

**Date:** 2026-07-23 · **Parent:** [`2026-07-23-architecture-review.md`](./2026-07-23-architecture-review.md)

Every gap left open by the three investigation docs, decided. Each entry states
the decision, why, what was rejected, and what it costs. Decisions are binding
for implementation unless explicitly revisited.

**Verified facts this document rests on** (checked live, 2026-07-23):

| Fact | Evidence |
| --- | --- |
| MangaDex `/manga` already returns `tags` inline | `attributes.tags[]` present in a plain `?limit=1` response |
| Tags carry a `group` discriminator | groups observed: `theme`, `genre`, `format`, `content` |
| `/statistics/manga` batches at **100**, truncates silently | 120 requested → HTTP 200, 100 returned |
| **FTS5 is compiled into the running server binary** | 22 `fts5` symbol matches in `/usr/local/bin/komika-server` |
| `work_tag` is admin-curated with **full-replace** semantics | `migrations/0031_work_admin_overrides.sql:10-20` |

---

## A · Catalogue and browse

### AD-1 · Source tags get their own table; `work_tag` stays admin-only

**This corrects an error in the earlier plan.** The browse doc said "ingest
MangaDex tags into `work_tag`". That would have been a bug. `0031_work_admin_overrides.sql:10-12`
defines `work_tag` as an admin override with full-replace semantics — *"When a
work has ANY row here, this IS its genre list."* Bulk-loading 100K works into it
would permanently mask the fallback chain and make every work look
admin-curated, with no way to distinguish a human decision from an import.

**Decision:** add a separate `work_source_tag`:

```sql
CREATE TABLE work_source_tag (
    work_id     TEXT NOT NULL REFERENCES work(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL,          -- 'mangadex'
    tag         TEXT NOT NULL,          -- normalized lower-case slug
    display     TEXT NOT NULL,          -- 'Slice of Life'
    tag_group   TEXT NOT NULL,          -- genre | theme | format | content
    ord         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (work_id, source_type, tag)
);
CREATE INDEX idx_work_source_tag_lookup ON work_source_tag(tag, tag_group, work_id);
```

Precedence, extending the existing chain in `catalog::work_effective_genres`
(`catalog/mod.rs:361-393`): **admin `work_tag` → `work_source_tag` (mangadex) →
Suwayomi-derived genres**.

**Cost: zero extra API calls.** Tags ship inline in the `/manga` payload the
6-hourly catalogue sync already fetches. `MdAttrs` (`mangadex.rs:516`) simply
doesn't parse the field today — this is a parse-and-store change, not a new
crawl. Backfill for existing rows rides the next full sync.

**Rejected:** reusing `work_tag` (destroys admin semantics); a JSON column on
`work` (unindexable, repeats the `LIKE '%"x"%'` mistake the Suwayomi path already
makes at `series_cache.rs:317-402`).

### AD-2 · Facets are built from `tag_group`, not a denylist

The Suwayomi facet list is polluted because it has no type information — its #1
"genre" is **"Japanese" (7,949)**, alongside "Long Strip", "Full Color",
"Content rating: Suggestive". The earlier plan proposed a hand-maintained
denylist. MangaDex's `group` field makes that unnecessary for canonical works.

**Decision:**
- Genre facets = `tag_group IN ('genre','theme')`. `format` and `content` are
  excluded from the genre rail and exposed as their own facets where useful.
- Store `tag` pre-normalized (lower-case, trimmed) and match on it; carry
  `display` for rendering. This kills the 32 case-duplicate groups
  (`Romance`/`romance`/`ROMANCE`) at the source rather than patching the
  comparison.
- Facet counts run the **same** predicate as results — `in_library`, NSFW, and
  readability gates included. The current mismatch (rail says Romance 6,180,
  results give 4,708) is caused by `series_cache.rs:415-419` omitting them.
- Keep a small denylist **only** for legacy Suwayomi-derived genres, which have
  no group data.

### AD-3 · Keyset pagination, not offset — required at 112K

**This is a gap nobody flagged and it would have sunk the launch.** Browse
currently uses `page`/`LIMIT/OFFSET` over 13.6K rows. At 112,602 rows with the
temp B-tree sorts already measured (`work ORDER BY updated_at DESC` → `SCAN` +
`USE TEMP B-TREE`, 112,602 rows), deep offsets degrade linearly: SQLite must
generate and discard every preceding row. Page 3,000 would scan ~60K rows to
return 20.

**Decision:** cursor pagination on a `(sort_key, work_id)` tuple, base64-opaque
to the client. `work_id` is the tiebreaker so the ordering is total and stable
across concurrent writes.

```sql
WHERE (sort_key, work_id) < (:cursor_key, :cursor_id)
ORDER BY sort_key DESC, work_id DESC LIMIT 21   -- 21st row ⇒ hasNextPage
```

Every `orderBy` option needs a matching composite index (AD-4). Keep `page` in
the GraphQL schema as deprecated-but-accepted for one release so the reader can
migrate without a lockstep deploy.

**Rejected:** offset with a cap (arbitrarily truncates a catalogue we just
decided to expose in full); offset plus a covering index (still O(offset)).

### AD-4 · Format from `original_language`, persisted and indexed

`resolve_comic_type()` (`graphql/types.rs:861-905`) derives Manga/Manhwa/Manhua
at map time, so it cannot be filtered in SQL — which is why Format is a
client-side filter returning 2 of 10,583.

**Decision:** persist `work.comic_type` (`TEXT`), populated from
`content_type_override` → `original_language` → existing heuristics, with the
mapping `ja→MANGA`, `ko→MANHWA`, `zh|zh-hk→MANHUA`, else `COMIC`.
`original_language` is populated for 105K of 112.6K works (88,377 ja / 10,578 ko
/ 6,030 zh), so coverage is good. Backfill in the migration; maintain on upsert.
Keep `resolve_comic_type()` as the writer, so there is exactly one definition.

Indexes required by AD-3 (all `(filter, sort_key, id)` shaped):

```sql
CREATE INDEX idx_work_browse_updated ON work(is_nsfw, latest_chapter_at DESC, id DESC);
CREATE INDEX idx_work_browse_title   ON work(is_nsfw, primary_title, id);
CREATE INDEX idx_work_browse_rating  ON work(is_nsfw, rating_bayesian DESC, id DESC);
CREATE INDEX idx_work_type           ON work(comic_type, is_nsfw, id);
CREATE INDEX idx_work_year           ON work(year, is_nsfw, id);
CREATE INDEX idx_work_status         ON work(status, is_nsfw, id);
```

`is_nsfw` leads every index because it is on the hot path for **every**
anonymous request (AD-8).

### AD-5 · Text search moves to FTS5

**FTS5 is confirmed compiled into the running binary**, so this needs no
dependency or image change. Today text search abandons the 112K catalogue
entirely and queries one live Suwayomi source (`mod.rs:2293-2307`), returning
`total: null` and a `hasNextPage: true` alongside `items: []`.

**Decision:** an external-content FTS5 table over `work.primary_title` +
`work_alias.alias`, kept in sync by triggers.

```sql
CREATE VIRTUAL TABLE work_fts USING fts5(
    title, aliases, content='', tokenize='unicode61 remove_diacritics 2'
);
```

`content=''` (contentless) keeps it compact — we only need the rowid → `work_id`
mapping, and 1,250,998 `work_alias_token` rows already exist for the prefix path.
Rank with `bm25()`, then apply the same filter predicates as browse so search and
browse behave identically.

**Rejected:** `LIKE '%q%'` on `work.primary_title` (unindexable leading wildcard,
already flagged as H9 in `PRODUCTION_AUDIT.md:129`); an external search service
(operationally disproportionate for a single 4-core box).

**Also fix regardless:** text search currently **writes** new rows into
`suwayomi_series` as a side effect (ids 13,953+ appeared during a probe). User
reads must not mutate the catalogue. Route discovery-of-new-series through the
ingest path explicitly.

### AD-6 · Ratings live in `work_stats`, not on `work`

**Decision:** a sibling table rather than columns on `work`, because statistics
churn on a completely different cadence from catalogue metadata and would
otherwise dirty `work` rows (and their `updated_at`) on every refresh — which is
exactly the semantics collision AD-14 exists to fix.

```sql
CREATE TABLE work_stats (
    work_id         TEXT PRIMARY KEY REFERENCES work(id) ON DELETE CASCADE,
    rating_average  REAL,
    rating_bayesian REAL,
    follows         INTEGER,
    fetched_at      TEXT NOT NULL
);
CREATE INDEX idx_work_stats_bayesian ON work_stats(rating_bayesian DESC, work_id DESC);
CREATE INDEX idx_work_stats_follows  ON work_stats(follows DESC, work_id DESC);
```

Filter and sort on **`rating_bayesian`** — MangaDex's low-vote-count correction —
so a 10.0 from three voters cannot top a 112K catalogue. Refresh: 1,092 batched
requests ≈ 4.5 min at 4 req/s, daily. **Chunk at exactly 100 and assert
`returned == requested`** — the endpoint truncates silently.

`follows` replaces chapter-count as the "Trending"/"Popular" signal. Local
`reviews` (3 rows) stays a separate, separately-labelled number; the two are
never averaged together.

### AD-7 · Readability is a filter and a badge, never a hidden predicate

Only 47,557 of 112,602 works have cached chapters. Silently excluding the rest
would undo the full-catalogue decision at the query layer.

**Decision:** denormalize `work.has_local_chapters` (maintained on chapter
insert/delete), default the Browse UI to **"Readable now" ON**, and make it a
visible, clearable toggle showing both counts. Users see the full catalogue
exists and opt into it; they are not silently shown 65K unreadable rows by
default either.

### AD-8 · NSFW filtered in SQL before `LIMIT`

`filter_nsfw` (`mod.rs:1789-1791`) runs in Rust *after* `LIMIT 20`, which is why
anonymous `discovery` was observed returning **0 POPULAR items and 1 TRENDING**.

**Decision:** push the predicate into every catalogue query, ahead of `LIMIT`,
with `is_nsfw` leading the composite indexes (AD-4). Facet counts use the same
predicate (AD-2). Anonymous and authenticated results become separate cache keys
(AD-17).

---

## B · Sync and scheduler

### AD-9 · Serialize writes through one writer task; then raise read concurrency

The measured problem is not throughput, it is contention: 35 `database is locked`
warnings in 11.5 h, of which 10 held the lock for the full 15 s timeout. Six
in-process writers contend for SQLite's single writer. **Raising
`SCAN_CONCURRENCY` first would multiply the collisions, not the work done.**

**Decision, in this order:**

1. `BEGIN IMMEDIATE` + bounded retry in `record_scan` (`scanner.rs:676`), reusing
   the `UPSERT_LOCK_RETRIES` helper that already exists at `mangadex.rs:38-44`.
   This alone removes the `SQLITE_BUSY_SNAPSHOT` class, which `busy_timeout`
   structurally cannot retry.
2. Classify lock errors separately from upstream errors (`scanner.rs:498-509`) so
   a lost write race stops being punished with a 30 min → 1 h → 2 h backoff.
3. Funnel **all** background writes through a single bounded `mpsc` writer task.
   Reads stay pooled and parallel. This makes contention structurally impossible
   rather than statistically rarer, and gives one place to batch transactions.
4. **Only then** raise `SCAN_CONCURRENCY` from 3 → 12, and measure. The ceiling
   is Suwayomi (25–30% CPU, 4.4 GiB) long before it is the Rust server (1–2%).

**Rejected:** bumping `busy_timeout` (does nothing for `BUSY_SNAPSHOT`);
`WAL2`/`BEGIN CONCURRENT` (not available in the shipped SQLite); separate
databases per writer (splits the transactional boundary the dedup path needs).

### AD-10 · Awaiting cohort widens exponentially and is size-capped

91% of scan capacity re-polls ~1,150 series every 30 min for up to 48 h,
starving 12,700 others down to ~230 scans/h.

**Decision:** widen the awaiting interval 30 m → 1 h → 2 h → 4 h per consecutive
no-change poll, resetting on a real chapter. Cap the cohort at **500** concurrent
members, admitting by most-recent-activity. Apply ±10% jitter (AD-11). Expected
effect: awaiting drops from ~2,200 to under ~600 scans/h, roughly tripling
capacity for the long tail without touching concurrency.

### AD-11 · Jitter everywhere; hard interval ceiling of 14 days

Zero jitter on `next_scan_at` (`scanner.rs:758-761`) produces the measured,
self-sustaining 35-minute herd (745 → 292 → 55 → 5). The only existing jitter is
on the park path.

**Decision:** ±10% jitter on **every** `next_scan_at` write. Cap
`MAX_INTERVAL_HOURS` at `PAUSED_PARK_HOURS` (14 d), replacing the current
100 years — 217 rows are scheduled past 2027, one until **2033-03-14**, and would
otherwise never be rescanned. Backfill those rows in the migration. Stagger the
five loops' first tick (`main.rs:960-1016`, currently all within 336 µs) and
de-align the scanner/cover 300 s collision.

### AD-12 · Circuit breaker on failing subscriptions

`drakescans` and `suryascans` fail every pass and **re-stamp `last_synced_at` as
if successful**, so nothing ever escalates.

**Decision:** stop stamping success on failure. After **5** consecutive failures,
auto-disable the subscription, set `last_error`, and surface it in the admin Bugs
panel that already exists for cover issues. Manual re-enable clears the counter.

### AD-13 · Canonical works never enter `series_scan_state`

Restating as binding: browsability ≠ scannability. 13,850 series already saturate
the loop at 43% duty cycle; admitting 112K would make a 5.7×-short scheduler
roughly 50× short. Canonical works are browsed from `work`/`chapter` and
refreshed by the MangaDex catalogue sync. Only Suwayomi-linked series are
scanned. Enforce with an assertion at the `series_scan_state` insert site, not
just convention.

---

## C · Updates and caching

### AD-14 · Add `latestChapterAt`; leave `updatedAt` meaning what it says

`Series.updatedAt` currently means "when our scanner last polled this"
(ρ = −0.06 against real chapter recency, median 480 days off).

**Decision:** do **not** redefine `updatedAt` in place — the reader already
renders it as a "· 4h" label, and changing its meaning silently alters every card
with no compile-time signal. Instead:

- `updatedAt` keeps its honest meaning: last metadata touch.
- Add **`latestChapterAt`**, sourced from the newest chapter's real
  `published_at`, exactly as `catalog::latest_english_chapter_at()`
  (`catalog/mod.rs:506-517`) already does for the canonical path.
- Repoint the reader, `series_cache.rs:280`, and `:388` at `latestChapterAt`.
- **Not** `last_new_chapter_at` — that is a batch-stamped *detection* time (745
  rows share one identical timestamp), which destroys intra-batch ordering and is
  what manufactures the duplicate-key crash.
- Backfill is derivable from existing `chapter`/`suwayomi_chapter` rows: a
  migration, not a re-scan.

### AD-15 · `feed_updates` materialized table

Replaces a per-request `GROUP BY` over 804,729 chapter rows with two temp
B-trees (measured 3.5–4.0 s, on every view of both `/` and `/updates`).

```sql
CREATE TABLE feed_updates (
    work_id        TEXT PRIMARY KEY REFERENCES work(id) ON DELETE CASCADE,
    latest_at      TEXT NOT NULL,      -- real published_at, always <= now
    latest_chapter TEXT,
    is_nsfw        INTEGER NOT NULL DEFAULT 0,
    lang           TEXT NOT NULL DEFAULT 'en'
);
CREATE INDEX idx_feed_updates_order ON feed_updates(is_nsfw, lang, latest_at DESC, work_id DESC);
```

Written **only when a chapter actually lands** (scanner + mangadex sync), which
makes it the natural home for AD-14's correct timestamp. Write goes through the
AD-9 writer task. Rebuildable from `chapter` at any time, so it needs no backup
guarantees.

**Guard `published_at <= now` at write time.** 236 chapters carry
`published_at = 2037-12-31` (MangaDex uses far-future dates for scheduled
releases), producing 64 works that currently fill pages 1–3 of `canonicalUpdates`
— which is 100% junk today.

### AD-16 · Origin-cached GET endpoints; no KV write path

**Decision:** serve public feeds from `GET /feed/{updates,discovery,browse}.json`
on the origin, with `ETag` and
`Cache-Control: public, s-maxage=60, stale-while-revalidate=300`, cached by a
Cloudflare cache rule. POST GraphQL stays as-is for authenticated and
mutation traffic and is never cached.

**Rejected: publishing to KV/R2.** It would technically mean updates *never*
touch the VPS, but it adds a second write path, a second failure mode, and a
staleness window with no invalidation story — for a workload whose entire origin
cost after AD-15 is an index range-scan. With `s-maxage=60` and CF's edge, origin
load is already reduced by orders of magnitude. Revisit only if measured origin
load justifies it.

**Sequencing: AD-15 and the `published_at` guard must land before any caching**,
or the cache will faithfully serve far-future junk with a 60 s TTL.

### AD-17 · Cache keys split anonymous from authenticated

NSFW gating makes the same URL return different content per viewer.

**Decision:** anonymous feeds are fully cacheable and are the only ones served
from `/feed/*.json`. Authenticated viewers with `show_nsfw` go through GraphQL
uncached. `Vary` is deliberately avoided — a cookie-varying cache key on
Cloudflare fragments the cache to near-uselessness. Two paths, one cacheable, is
simpler and faster than one clever path.

---

## D · Cross-cutting items no doc had claimed

### AD-18 · Ship a `/metrics` endpoint

`PRODUCTION.md:36-40` lists metrics, error tracking, and uptime monitoring as
unchecked, and there is currently **no way to observe latency or error rate other
than reading container logs**. Every fix in these docs has a verification step
that presently requires a human grepping `docker logs`.

**Decision:** Prometheus text-format `/metrics`, bound to loopback and not routed
through the tunnel. Minimum viable set: GraphQL resolver latency histogram, SQLite
busy/lock counter, scan tick duration and batch size, per-loop last-success
timestamp, feed cache hit/miss. This is a prerequisite for honestly closing
AD-9's "measure, then raise concurrency".

### AD-19 · Request-scoped DataLoader for `map_series`

`PRODUCTION_AUDIT.md:63` flags systemic N+1 across every feed; no DataLoader
exists anywhere in `graphql/mod.rs`. `library_ids` needed a bespoke fix
(commit `8fa19cc`) for the same underlying shape.

**Decision:** add `async-graphql`'s `DataLoader` for the batchable lookups on the
feed path — covers, stats, source links, effective genres. Do it **with** the
canonical browse resolver (AD-3), not after: a new resolver over 112K rows
without batching would reintroduce the problem at a larger scale.

### AD-20 · Single-flight on cache miss

`PRODUCTION_AUDIT.md:222` flags stampede-on-miss; the TTL half is implemented
(`series_cache.rs:18,21`), the single-flight half is not.

**Decision:** an in-process keyed single-flight map around
`resolve_series_cached` and the feed builders, so N concurrent misses for the
same key produce one upstream fetch. Cheap, self-contained, and directly reduces
the write bursts feeding AD-9.

### AD-21 · Cover store: LRU eviction with a hard size cap

Backup was decided; **growth was not**. `covers.sqlite3` is 20.5 GB with a
freelist of 0, no delete path in `cover.rs`, and ~8,700 orphans. Backing up an
unbounded store means the replication bill tracks the leak.

**Decision:**
- Track `last_served_at` per blob.
- Hard cap at **24 GB**, evicting least-recently-served above the mark. Covers
  are derived and re-fetchable, and CF already fronts reads, so eviction is
  cheap — a miss costs one origin re-fetch.
- Orphan sweep, then `VACUUM INTO` a fresh file (the freelist will not reclaim in
  place), **then** attach Litestream with a long `snapshot-interval` and small
  retention.
- Cap first, replicate second. Replicating before capping pays to store garbage.

### AD-22 · Correct `PRODUCTION.md`

It is wrong on three counts that actively mislead planning: it claims the reader
is a static SPA served by nginx (it is a Cloudflare Worker with edge SSR;
`deploy/nginx.conf` is dead code), lists Suwayomi exposure and TLS as open TODOs
(both resolved), and states `opt-level=z` (now 3). Fix as part of this work,
not "later".

### AD-23 · Rollout order and rollback

Ordered so each stage is independently valuable and independently revertible.

| Stage | Contents | Restarts server? | Rollback |
| --- | --- | --- | --- |
| **0** | `ANALYZE`; CF Browser-Cache-TTL → Respect Existing Headers; `docker builder prune`; start flaresolverr; cap Suwayomi mem/CPU | No (except Suwayomi) | Drop `sqlite_stat1`; revert CF toggle |
| **1** | Reader-only: dedupe + unique `{#each}` key; `await` updates SSR; header `totalCount`; disable rating slider | No | Redeploy previous Worker |
| **2** | Rust batch A — AD-9(1,2), AD-11, AD-12, AD-14 + backfill, AD-15, `published_at` guard, AD-8, AD-20, AD-18 | **Yes, once** | Revert image; migrations are additive |
| **3** | AD-16 GET feeds + CF cache rule | No (config) | Remove cache rule; reader falls back to GraphQL |
| **4** | Rust batch B — AD-1, AD-6 ingest, AD-4, AD-5 FTS5, AD-3 cursor, AD-19, canonical browse resolver | **Yes, once** | `page` kept accepted for one release (AD-3), so the reader is not lockstep |
| **5** | AD-9(3,4) writer task + concurrency raise, AD-10, AD-21 cover cap, AD-13 assertion, AD-22 | **Yes, once** | Concurrency is env-tunable; revert without redeploy |

**Rules.** All migrations additive — no destructive `ALTER`/`DROP` in any stage,
so an image revert never strands the schema. Three server restarts total, batched
deliberately: **a rebuild restarts the server and interrupts ingest**, so restarts
are a budgeted resource. Stage 2 must precede Stage 3 (don't cache junk). Stage 4
must precede raising concurrency in Stage 5 (measure with AD-18 first).

---

## Corrections to earlier docs

| Was | Now | Why |
| --- | --- | --- |
| "Ingest MangaDex tags into `work_tag`" | AD-1: separate `work_source_tag` | `work_tag` is an admin override with full-replace semantics; bulk-loading it destroys that |
| "Hand-maintained genre denylist" | AD-2: `tag_group` from MangaDex | The API already classifies tags; a denylist was solving a problem the data doesn't have |
| Tag ingest implied new crawling | AD-1: zero extra calls | Tags ship inline in the `/manga` payload already being fetched |
| "Raise `SCAN_CONCURRENCY` to 8–12, measure" | AD-9: serialize writes **first**, then 12 | Raising it before fixing contention multiplies lock collisions |
| Pagination unaddressed | AD-3: keyset cursors | Offset over 112K rows with temp B-tree sorts degrades linearly; a gap in the original analysis |
| "Consider KV/R2 for feeds" | AD-16: rejected | Second write path and staleness window, for a workload that is an index scan after AD-15 |
