# Komika — Catalogue, Sources & Canonical Dedup

> Living design doc for how Komika sources content, mirrors the catalogue,
> guarantees **one entry per series**, and retires the fabricated social seed data.
> Companion to [SPEC.md](SPEC.md). Decisions here were made 2026-07-11.

## Contents

1. [The pivot](#0-the-pivot-read-this-first)
2. [Two-tier source model](#1-two-tier-source-model)
3. [NSFW gating](#2-nsfw-gating)
4. [Canonical work model](#3-canonical-work-model)
5. [The dedup matcher](#4-the-dedup-matcher)
6. [MangaDex catalogue sync](#5-mangadex-catalogue-sync)
7. [Tier-2 add flow & serving](#6-tier-2-add-flow--serving)
8. [Social layer cleanup — retire fabricated seed data](#7-social-layer-cleanup--retire-fabricated-seed-data)
9. [Build sequence](#8-build-sequence)
10. [MangaDex API limits (reference)](#9-mangadex-api-limits-reference)
11. [Open decisions](#10-open-decisions)

---

## 0. The pivot (read this first)

The backend as shipped is **pure live-federation**: `komika-server` stores only
identity + social, and `apps/server/migrations/0001_init.sql` states plainly
_"Catalog/chapters/pages are NOT stored here — federated live from Suwayomi."_
Series identity is an opaque Suwayomi manga id (TEXT); `altTitles` is hardcoded `[]`
([`suwayomi-backend.ts:301`](packages/api/src/suwayomi-backend.ts)) and never flows through.

The catalogue + dedup direction **deliberately reverses that for metadata**. Komika
moves to a **hybrid**:

| Dimension                                 | Before                     | After                                        |
| ----------------------------------------- | -------------------------- | -------------------------------------------- |
| Catalogue metadata, aliases, external IDs | live-federated             | **stored** (mirror)                          |
| Chapter lists                             | live-federated             | **stored** (enables offline update-checking) |
| Series identity                           | opaque Suwayomi id         | **canonical `work`** (one per series)        |
| Page images / reading                     | live via Suwayomi → Worker | **unchanged**                                |

This is a pivot, not an addition — it supersedes the "store nothing" premise in
`0001_init.sql` for the catalogue dimension. Because the **reading path is untouched**,
the working reader is never at risk while the catalogue subsystem is built.

## 1. Two-tier source model

| Tier  | Source                                     | Ingestion                                                       | Stored?                |
| ----- | ------------------------------------------ | --------------------------------------------------------------- | ---------------------- |
| **1** | **MangaDex**                               | Full catalogue crawl + chapter mirror + update polling          | Yes — mirrored         |
| **2** | **Everything else** (Keiyoushi extensions) | **Curated**: admin browses/searches and hand-picks sites/series | Yes — per added series |

**Why this split.** MangaDex has a clean public API worth deep-integrating. The long
tail of sources is already solved by the Mihon/Tachiyomi extension ecosystem, so we
reuse it rather than reimplement per-source scrapers. Extensions are the state of the
art here — 1,356 community-maintained sources patched within days of a site changing —
and hand-writing/maintaining that many adapters is not viable.

### Tier 1 — MangaDex (mirrored)

- Direct HTTP to `api.mangadex.org` (**not** via Suwayomi) — only the direct API
  exposes `createdAt` windowing, the external-ID `links` field, and full `altTitles`.
- ~93.5k titles as of 2026-07. Metadata + chapter list stored; images still proxied
  by the Worker, never stored.

### Tier 2 — Keiyoushi extensions (curated, not crawled)

- Hardcoded extension repo (standard Mihon/Tachiyomi index, 1,356 extensions),
  loaded by the Suwayomi backend. Already registered in `deploy/bootstrap.py`.
  - Index: `https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.min.json`
  - APKs: `https://raw.githubusercontent.com/keiyoushi/extensions/repo/apk/{apk}`
  - (`keiyoushi.github.io` 404s — use the raw `repo` branch.)
- We do **not** monitor/mirror all sources. The admin adds specific series; each add
  runs the dedup matcher (§4).
- **Survival hedge:** the original Tachiyomi repo was taken down; Keiyoushi is the
  successor. Pin/mirror the index + APKs as a fallback and keep the repo URL
  configurable behind the hardcoded default.

## 2. NSFW gating

A single **`show_nsfw`** user setting (default **off**) hides anything flagged NSFW by
**either** signal:

- **Source-level** — Keiyoushi index `nsfw: 1`.
- **Series-level** — MangaDex `contentRating ∈ {erotica, pornographic}` (optionally
  `suggestive`, configurable).

Persist `is_nsfw` on both `source_series` and the canonical `work` so filtering is a
cheap boolean, not a re-check. Consistent with the donation-only, no-gating posture.

## 3. Canonical work model

**Requirement:** every series has exactly **one** canonical entry; many source-series
(the same series under different alt names across sites) fold into it.

**Spine = MangaDex.** Its ~93.5k mostly-clean works carry rich `altTitles` (many
languages), `description`, and external IDs in the `links` field — AniList (`al`),
MyAnimeList (`mal`), MangaUpdates (`mu`), Kitsu (`kt`), AnimePlanet (`ap`). Every
Tier-2 source-series resolves to a `work_id`; **no match → a new first-class canonical
work** (non-MangaDex works are first-class, not hidden or second-class).

```
                    ┌───────────────────┐
                    │       work        │  the ONE canonical entry
                    │  (canonical id)   │
                    └─────────┬─────────┘
          ┌───────────────────┼───────────────────┐
          │                   │                   │
 ┌────────▼───────┐  ┌────────▼────────┐  ┌────────▼─────────┐
 │  work_alias    │  │ work_external_id│  │  source_series   │──┐
 │ (title, lang)  │  │ (provider, id)  │  │ (mangadex | ext) │  │
 └────────────────┘  └─────────────────┘  └────────┬─────────┘  │
                                                    │            │
                                           ┌────────▼───────┐    │
                                           │    chapter     │    │
                                           │ (num, lang, …) │    │
                                           └────────────────┘    │
                                     merge_candidate ────────────┘
                                     (review queue, score, status)
```

### Schema — new migration `0005`, alongside `series_admin` / `series_scan_state`

| Table              | Purpose                                                                                                                                                        |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `work`             | the ONE canonical entry — title, description, year, original language, status, demographic, `content_rating`, `is_nsfw`, `cover_phash`, provenance, timestamps |
| `work_alias`       | `(work_id, normalized_title, lang, raw_title)` — alias index, fed by MangaDex altTitles                                                                        |
| `work_external_id` | `(work_id, provider, external_id)` **unique** — the gold match key                                                                                             |
| `source_series`    | `(work_id FK, source_type, source_id, source_key, is_nsfw, last_seen)` many-to-one; the Suwayomi manga id lives **here** now                                   |
| `chapter`          | per-`source_series` chapter list (number, volume, lang, title, published_at, external id) — powers stored update-checking                                      |
| `merge_candidate`  | manual-review queue — `(source_series_id, candidate_work_id, score, method, status)`                                                                           |

**Migration of existing FKs.** `reviews` / `comments` / `series_admin` /
`series_scan_state` currently key on `series_id` = Suwayomi id. Backfill one `work` +
`source_series` per existing series, then repoint those FKs at `work_id`. The library
is small today, so this is cheap.

## 4. The dedup matcher

A **record-linkage / entity-resolution** pipeline. Runs **at add-time** for Tier-2
series (Tier 2 is human-curated, so the mid-confidence review step is effectively
free) and can be pointed inward later to catch rare MangaDex-internal duplicates.
Natural home: `apps/server/src/dedup/`.

**Precision ladder — cheap → expensive:**

1. **External-ID exact** — AniList / MAL / MangaUpdates / Kitsu / AnimePlanet. Highest
   precision; **stop on hit**.
2. **Normalized-title exact** vs `work_alias` — romanize (`ja-ro`), lowercase, strip
   punctuation + season/part suffixes; guard against common-title collisions.
3. **Fuzzy title** — trigram / token similarity → a **candidate shortlist** (not a
   decision).
4. **Corroborate candidates** → confidence score:
   - **Description similarity** — cheap **MinHash / shingle overlap** (decided:
     MinHash-only to start; aggregators copy AniList/MAL descriptions verbatim, so
     this catches most). Semantic embeddings deferred until precision demands them.
   - **Cover perceptual-hash** — pHash + Hamming distance. Strongest cheap signal;
     the same series reuses the same art.
   - Author/artist, year proximity, tags/demographic, original language.
5. **Decide by threshold:**
   - **High → auto-merge** into the existing work.
   - **Mid → manual admin review queue** (`merge_candidate`) — "looks like _X_ (0.87),
     confirm / reject / new".
   - **Low → new first-class work.**

Store the match method + score on every merge so it is auditable and reversible.
Strongest cheap signals, in order: **cover pHash**, then **copied-description
overlap** — lean on these before reaching for embeddings.

## 5. MangaDex catalogue sync

New module `apps/server/src/mangadex/` (direct `reqwest` client).

- **Seed crawl** — `GET /manga?includes[]=cover_art&order[createdAt]=asc`. The
  `offset + limit ≤ 10,000` cap means you cannot page to 93k directly; **window by
  `createdAt`**: page to ~offset 9,900, set `createdAtSince` = last item's `createdAt`,
  reset offset, repeat until empty. Upsert → `work` / `work_alias` /
  `work_external_id`; compute cover pHash on ingest.
- **Chapters** — global `/chapter` firehose with `createdAtSince` windowing → `chapter`
  rows, attached by the `manga` relationship. **English-only:** the firehose is filtered
  at the source (`translatedLanguage[]=en`) and the sync skips any stray non-English row,
  so the mirror stores only English chapters (Komika serves English only).
- **Incremental refresh** ✅ — `spawn_recurring` (mirrors the `scanner.rs` task pattern)
  seeds once by `createdAt`, then every `CATALOGUE_SYNC_INTERVAL_SECS` (default 6h) does an
  `updatedAtSince` refresh of both catalogue and chapters. The cursor per job lives in
  `catalogue_sync_state` (migration `0006`) and only advances on success, so a failed cycle
  safely retries the same window.
- **Cover URL** (must be proxied — hotlinking returns a wrong response):
  `https://uploads.mangadex.org/covers/{manga-id}/{fileName}` (+ `.512.jpg` / `.256.jpg`).

See §9 for the rate-limit budget the crawl must respect.

## 6. Tier-2 add flow & serving

- **Add flow** — new GraphQL mutation `addSourceSeries(sourceId, mangaKey)`: fetch via
  the existing `suwayomi.rs` client → run the matcher (§4) → return
  `{ auto-merge | review | new }` → admin confirms in `apps/admin/`. On confirm/new,
  create `source_series` and link/create the `work`. Fold source `nsfw` + MangaDex
  `contentRating` into `is_nsfw`.
- **Serving** — `graphql/mod.rs` `map_series` reads the stored `work` (altTitles
  populated). Pages/images still fetched live.
- **NSFW filtering** ✅ — per-user `show_nsfw` (migration `0007`, `setShowNsfw` mutation,
  surfaced on `SessionUser`; default off). `Series.is_nsfw` is set from the canonical model
  and `discovery` / `search` / `updates` drop NSFW-flagged series unless the viewer opts in
  (we only hide what we positively know is NSFW).
- **Stored chapter deltas** ✅ — the `canonicalUpdates` query serves recently-updated
  mirrored MangaDex works + their latest stored chapter straight from the `chapter` table
  (no live Suwayomi round-trip), NSFW-filtered. Each row now also carries a proxy-ready
  `coverUrl`, and the reader surfaces the feed in its Updates screen with openable cards.
- **Reader navigation into canonical works** ✅ — MangaDex-mirrored works are now
  browseable and readable in the reader (migration `0008` stores the cover `fileName`;
  the sync populates it). A canonical work is addressed by its `work` id, whose `w_`
  prefix distinguishes it from a numeric Suwayomi id, so the reader routes it down a
  parallel path without touching the Suwayomi one. New resolvers `canonicalSeries(workId)`
  / `canonicalChapters(workId)` / `canonicalPages(chapterId)` map `work`/`chapter` onto the
  shared `Series`/`Chapter`/`Page` shapes (so existing reader components are reused):
  chapters come from the stored `chapter` mirror, **English-only** (`lang = 'en'`), deduped
  to one row per number and ordered; pages resolve at read-time via **MangaDex@Home**
  (`MangaDexClient::at_home`, rate-limited under the 40/min + global ~5 req/s budgets).
  `show_nsfw` gating applies to all three. Covers (`uploads.mangadex.org`) and pages
  (`*.mangadex.network`) are proxied by the Worker — its `ALLOWED_SOURCE_HOSTS` must
  include `uploads.mangadex.org` **and** `mangadex.network` (the suffix entry covers every
  `@Home` node). Progress-sync for canonical chapters is not wired yet (there is no
  Suwayomi-side store), so reading them doesn't persist per-user progress.

## 7. Social layer cleanup — retire fabricated seed data

The reader currently ships **fabricated comment/review threads** as offline-fallback
seed data. This must be removed as the real social backend (`comments` / `reviews` /
`postComment` / `postReview`, already present in `0001_init.sql`) becomes the default —
shipping fake usernames, bodies, and like counts in a real product is both misleading
and a bad look.

**What to remove / change:**

| Location                                                                              | Today                                                                        | Cleanup                                        |
| ------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- | ---------------------------------------------- |
| [`mock.ts:503`](apps/reader/src/lib/data/mock.ts) `seriesComments`                    | 4 hardcoded review-thread entries ("Mika R.", "devon_k", …)                  | delete the array + `SeriesComment` interface   |
| [`mock.ts:594`](apps/reader/src/lib/data/mock.ts) `readerComments`                    | hardcoded per-chapter comments ("Aria_reads", …) with fake `likes`/`replies` | delete the array + `ReaderComment` interface   |
| [`social-repo.ts:153`](apps/reader/src/lib/data/social-repo.ts) `loadSeriesSocial`    | seeds `seriesComments` into the local fallback                               | seed with `[]` — honest empty state            |
| [`social-repo.ts:247`](apps/reader/src/lib/data/social-repo.ts) `loadChapterComments` | seeds `readerComments` into the local fallback                               | seed with `[]` — honest empty state            |
| [`social.ts`](apps/reader/src/lib/data/social.ts) `getComments(bucket, key, seed)`    | seed param carries the mock threads                                          | keep the localStorage store; callers pass `[]` |

**Principle:** offline / backend-off mode should render an **honest empty comment
state** ("No comments yet"), not fabricated engagement. The live path
(`socialLive()` → real `backend.comments` / `backend.reviews`) is already correct and
unaffected — this only strips the seed threads from the fallback branch. Note
`mock.ts:1005` ("Members-only comment lounge") is unrelated UI copy on the donate page
and stays.

## 8. Build sequence

Milestones, ordered so the working reader is never blocked. Each is independently
shippable. Status as of 2026-07-11:

1. **M1 — Canonical schema.** ✅ Done. Migration `0005` (§3) + backfill of existing
   series into `work` / `source_series`. No behaviour change.
2. **M2 — MangaDex client + seed crawl** (§5). ✅ Done (`src/mangadex.rs`). Direct API
   client, `createdAt` windowing, global token-bucket limiter, `work`/aliases/external-ID
   upserts. Gated by `CATALOGUE_SYNC` (off by default). **Cover pHash on ingest** ✅ done
   (`src/phash.rs`, 64-bit dHash via the `image` crate) — computed per work during sync
   when `COVER_PHASH=on`, feeding the cover dedup signal.
3. **M3 — Chapter mirror + incremental sync** (§5). ✅ Done. Global `/chapter` firehose
   → `chapter` rows, plus the recurring `updatedAtSince` scheduler (`spawn_recurring` +
   `catalogue_sync_state`, migration `0006`) seeding once then refreshing every interval.
4. **M4 — Dedup matcher** (§4). ✅ Done (`src/dedup.rs`). `resolve()` returns
   `{ auto-merge | review | new }` via the 5-step ladder; unit-tested end-to-end.
5. **M5 — Tier-2 add flow** (§6). ✅ Done. `addSourceSeries` + `resolveMergeCandidate`
   mutations and the `mergeQueue` admin query, plus the **admin review UI** at
   `apps/admin/src/routes/review/` (confirm-merge / keep-separate over the queue,
   threaded through `@komika/api` + `@komika/types`).
6. **M6 — Serve from canonical model** (§6). ✅ Done. `map_series` folds canonical
   `work_alias` into `Series.altTitles`; `show_nsfw` per-user filtering (migration `0007`)
   over `discovery` / `search` / `updates`; and the `canonicalUpdates` query serving stored
   `chapter` deltas from the mirror.
7. **M7 — Social seed cleanup** (§7). ✅ Done. Fabricated seed threads removed;
   empty-state fallbacks.
8. **M8 — Reader navigation into canonical works** (§6). ✅ Done. Cover `fileName`
   stored (migration `0008`); `canonicalSeries` / `canonicalChapters` / `canonicalPages`
   resolvers + MangaDex@Home page fetching; the reader routes `w_`-prefixed work ids down
   a canonical path (Suwayomi path untouched) and the Updates screen surfaces openable
   `canonicalUpdates` cards. Worker allowlist extended for `uploads.mangadex.org` +
   `mangadex.network`.

**Remaining follow-ups (tracked):** per-user reading progress + library marking for
canonical works (today progress-sync is Suwayomi-only); related-series / genres for
canonical works (MangaDex tags aren't mirrored yet).

## 9. MangaDex API limits (reference)

| Scope                                            | Limit                                                                 |
| ------------------------------------------------ | --------------------------------------------------------------------- |
| Global, per IP (`api.mangadex.org`)              | **~5 req/s** → 429; persistent abuse → 403 / DDoS ban                 |
| `GET /at-home/server/{id}` (read-time page URLs) | **40 req/min**                                                        |
| List pagination                                  | `offset + limit ≤ 10,000`; `limit` max 100 (500 for some feeds)       |
| Connection                                       | TLS 1.2+ w/ SNI, valid `User-Agent` **required**, no `Via` header     |
| Images                                           | **Must proxy** (hotlink = wrong response); no CORS for external sites |

**Fleet-wide budget.** The Worker/backend egress IP is shared across all users, so the
5 req/s ceiling is a **fleet** budget, not per-user. The catalogue crawl must run behind
a **global token-bucket limiter**; coalesce/queue rather than fan out.

**Single-replica constraint (M4).** The limiter (`MangaDexClient`'s `TokenBucket`, plus
the dedicated `/at-home` bucket) is **in-process** — it bounds one server process, not the
fleet. Running N server replicas with `CATALOGUE_SYNC=on` (or serving reader `at_home`
page-loads) multiplies the effective rate to N×, which breaches the shared-IP ceiling and
risks a 429/403 ban. Until the budget is moved to a shared limiter (DB/Redis keyed by
egress IP), **exactly one replica may have `CATALOGUE_SYNC=on`**, and if reader page-load
`at_home` traffic is also significant, keep the API tier to a single replica or front it
with a shared limiter. The shipped single-container compose satisfies this by default.

## 10. Decisions (locked 2026-07-11)

1. **Description similarity depth** — **MinHash-only** to start (no ML dependency).
   Semantic embeddings deferred; revisit only if match precision proves insufficient.
2. **pHash placement** — **server-side**, computed on cover ingest during MangaDex
   sync. ✅ Realized as a hand-rolled 64-bit dHash over the `image` crate's decoders
   (`src/phash.rs`) — no extra image-hash dependency. Keeps hashing next to the data.

## Related

- [SPEC.md](SPEC.md) — overall architecture & source of truth
- [PRODUCTION.md](PRODUCTION.md) — ops / deploy
- Memory: two-tier source strategy, canonical-work dedup, image-pipeline-workers-only
