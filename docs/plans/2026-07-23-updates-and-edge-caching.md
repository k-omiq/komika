# Updates page, `updatedAt` semantics, and serving feeds off-VPS

**Date:** 2026-07-23 · **Parent:** [`2026-07-23-architecture-review.md`](./2026-07-23-architecture-review.md)
**Status:** investigation complete, no code changed.

> **Superseded in part.** This document is the *evidence*. Implementation follows
> [`2026-07-23-architecture-decisions.md`](./2026-07-23-architecture-decisions.md):
> `updatedAt` keeps its current meaning and a new `latestChapterAt` is added
> alongside it, rather than redefining the field in place (AD-14); the feed is
> materialized into `feed_updates` (AD-15); feeds are served from origin-cached
> `GET /feed/*.json` and **KV/R2 is rejected** (AD-16); anonymous and
> authenticated traffic use separate paths rather than a `Vary` cache key (AD-17).

---

## Summary

| Complaint | Verdict |
| --- | --- |
| "Series update tick isn't properly running" | **Partly wrong** — the tick runs and has no backlog. The defect is cadence. See [sync doc](./2026-07-23-sync-scheduler-health.md). |
| "`updated_at` is broken so every series is jumbled" | **Confirmed exactly as described.** Spearman ρ vs. newest chapter = **−0.06**, median offset **480 days**. |
| "Updates page doesn't load / no SSR" | **Confirmed — two independent hard bugs.** SSR renders zero content, *and* a duplicate `{#each}` key is crashing the page in production right now. |
| "Make sure every update doesn't hit the VPS" | **Currently every update hits it twice per view**, and one of the two queries costs 3.5–4.0 s of SQLite. |

---

## 1. `updatedAt` means "when we last polled" — CONFIRMED

### The bug

`apps/server/src/graphql/mod.rs:700`
```rust
updated_at: to_iso(m.last_fetched_at.as_deref()).unwrap_or_default(),
```

`m.last_fetched_at` is Suwayomi's `lastFetchedAt` (`suwayomi.rs:15,52`). The
scheduler calls `series_and_chapters()` (`suwayomi.rs:490-497`) with
**`fetchManga: true`**, forcing Suwayomi to re-fetch upstream and therefore
stamping `lastFetchedAt = now` on **every single scan**.

So `Series.updatedAt` literally means *"when our scanner last polled this
series."* The reader renders it as the "· 4h" label on every card
(`apps/reader/src/lib/data/source.ts:125`).

### Proof (n = 11,386 in-library series with cached chapters)

`Series.updatedAt` minus newest cached chapter's `upload_date`:

```
> 1 year late : 6,421 (56.4%)
30–365 d late : 2,648 (23.3%)
7–30 d late   : 1,259 (11.1%)
1–7 d late    :   932 ( 8.2%)
0–1 d         :   122 ( 1.1%)   ← only 1% are actually correct

median = +480.0 d   p90 = +2,209 d   max = +3,110 d
Spearman ρ(last_fetched_at, newest chapter) = -0.0636   ← zero correlation
```

Live `discovery` POPULAR, ordered by this column:

```
13850  lastFetched 2026-07-23 06:01  newestChapter 2026-07-03  结婚吧。以离婚为前提。
 8252  lastFetched 2026-07-23 06:00  newestChapter 2025-04-20  Rebirth in the Apocalypse…
 7752  lastFetched 2026-07-23 06:00  newestChapter 2021-10-27  Prince, Don't Do This!
                                                              ↑ 4.7 years stale, ranked #18 "most recent"
```

### Every writer

| File:line | What |
| --- | --- |
| `series_cache.rs:40-85` | `put_series()` — `updated_at = now`, `last_fetched_at = <Suwayomi lastFetchedAt>` |
| `series_cache.rs:127-131` | `put_chapters()` — `UPDATE … SET chapter_count=?, updated_at=?` |
| `scanner.rs:643` | `persist_scan()` → `put_series` on **every** scan, including no-change scans |
| `scanner.rs:646` | `persist_scan()` → `put_chapters` on every scan |
| `graphql/mod.rs:1163` | `resolve_series_cached()` cache-miss hydration |
| `graphql/mod.rs:1777` | `discovery()` cold path |
| `graphql/mod.rs:5662, 6039, 6234` | admin bulk-add / ingest / enrol |

Plus `work.updated_at`, which is a *metadata* touch, not chapter arrival:
`catalog/mod.rs:624-634` (`upsert_work_from_mangadex`, `Utc::now()` on every
re-sync), `:705` (`create_work`), `:1037` (`mark_work_nsfw`).

### Orderings that consume it as "recently updated"

| File:line | Feed |
| --- | --- |
| `series_cache.rs:280` | `library()` → `ORDER BY COALESCE(last_fetched_at, updated_at) DESC` — powers **discovery POPULAR** |
| `series_cache.rs:388` | `search_catalogue()` → same — powers **Browse** |
| `series_cache.rs:296` | `recently_added()` → `COALESCE(created_at, updated_at)` — correct-ish |

### The code already knows

`catalog/mod.rs:506-517` `latest_english_chapter_at()` does this correctly, with
an explicit comment at `:501-505` warning that `work.updated_at` is "last
metadata touch, not last new chapter". It is used at `graphql/mod.rs:1336-1340`
— **for the canonical path only**.

**The asymmetry is the bug.** The canonical (`w_`) path was fixed; the Suwayomi
(numeric) path at `mod.rs:700` was not.

### Even the fixed path is only half right

`graphql/mod.rs:1928-1933` overrides `updated_at` with
`series_scan_state.last_new_chapter_at` — but that is a *detection* time, not the
chapter's publish time. The scanner writes `now` for the whole batch
(`scanner.rs:761-766`), so hundreds of rows share one identical timestamp:

```
745 rows all at 2026-07-23T00:50:50.857726335
```

That destroys intra-batch ordering **and directly manufactures the adjacent
duplicates that crash the page** (§2a).

---

## 2. The updates page — two independent hard bugs

### 2a · P0 · Duplicate `{#each}` key crashes the page in production **right now**

`apps/reader/src/routes/(app)/updates/+page.svelte:93`
```svelte
{#each updates as item (item.title + item.ch)}
```

`getUpdates()` (`source.ts:684`) concatenates **without any dedupe**:
```ts
newUpdates: [...recent, ...canonicalCards],
```
whereas `getHome()` (`source.ts:626,636,637`) wraps every list in
`dedupeCardsByTitle()` (`source.ts:175`).

**Reproduced against the live API:** page 1 of `updates` + page 1 of
`canonicalUpdates` = 40 cards, **39 unique keys**:
```
×2  'I'm Quitting Everything and Selling Cola' Ch. 6   (series 1217 and 3409)
```

The deployed Svelte 5 runtime throws this **in production, not just dev** — from
`https://komiq.cc/_app/immutable/chunks/DWzmY1Nv.js`:
```js
function Ce(e,t,n){throw Error(`https://svelte.dev/e/each_key_duplicate`)}
…
e > a.size && Ce(``,``,``)      // items(40) > uniqueKeys(39) ⇒ throw
```

The `{#each}` throws during post-hydration render and the page falls into the
SvelteKit error boundary. **The user sees skeletons, then a broken page.** This
matches the reported symptom precisely.

DB-wide risk: 4 duplicate `(title, chapter_count)` groups among the 819 feed rows,
125 catalogue-wide. Because `ORDER BY last_new_chapter_at DESC` combined with the
batch-stamped timestamp puts same-title mirror rows **adjacent**, a collision
reaching page 1 is likely, not rare.

### 2b · P0 · SSR renders literally zero content

`apps/reader/src/routes/(app)/updates/+page.ts:11-14`
```ts
export const load: PageLoad = ({ setHeaders }) => {
	setHeaders({ 'cache-control': 'public, max-age=0, s-maxage=30' });
	return { updates: getUpdates() };   // ← promise returned UNAWAITED, always
};
```

The home page **already fixed exactly this** — `(app)/+page.ts:22-26`:
```ts
const home = getHome();
return { home: browser ? home : await home };
// :12-16 — "Svelte's SSR only ever renders the pending branch of {#await},
//  so a streamed promise would leave the edge HTML as skeletons forever"
```

The updates route never received that fix. Measured on live `https://komiq.cc`:

| Route | SSR time | HTML | `<img>` in SSR HTML | skeletons |
| --- | --- | --- | --- | --- |
| `/` (awaits) | 3.78 s | 20,998 B | **12** real cards | few |
| `/updates` (streams) | 0.12–0.22 s | 12,574 B | **1** | **55** |

SSR is nominally on and confirmed in the deployed bundle
(`_app/immutable/nodes/11.cDF626tv.js`: `ssr:()=>j, j=!0`) — it just delivers
nothing. **The `s-maxage=30` edge cache is therefore caching a content-free
shell.** The report's "doesn't have SSR or something" was a correct observation
with a different mechanism.

### 2c · The content that does arrive is wrong

- **`canonicalUpdates` page 1 is 100% junk** — every row has
  `latestAt = 2037-12-31T15:00:00+00:00`. MangaDex uses far-future `publishAt`
  for scheduled/withheld chapters. **236 chapter rows carry
  `published_at = 2037-12-31`**, producing **64 works** whose `MAX(...)` is in the
  future. `ORDER BY latest_at DESC LIMIT 20` puts those 64 on **pages 1–3**, and
  the reader only ever asks for page 1. `mod.rs:1946-1978` has **no
  `published_at <= now` guard**. The first real row would be `w_7f18d5a75…` at
  `2026-07-23T00:18:45`.
- **`discovery` returns almost nothing** — observed POPULAR = **0** items,
  TRENDING = **1** on one run. `PAGE_SIZE=20` rows are fetched, then `filter_nsfw`
  (`mod.rs:1789-1791`) is applied **after** the LIMIT, so an anonymous viewer
  loses most of the page. RECENTLY_UPDATED and RECENTLY_ADDED are absent entirely
  (`latest` always empty on the cached path, `mod.rs:1765`).
- **`trendingGroups` and `hotUpdates` are the same array** (`source.ts:682,685`)
  — the "Hot" tab shows identical content to "Trending Today" above it.
- **`newUpdates` is not merged or sorted** — 20 scanner rows then 20 canonical
  rows, so it isn't globally newest-first.

### 2d · Timings

| Query | localhost:8080 | api.komiq.cc | bytes |
| --- | --- | --- | ---: |
| `Discovery` | 0.417 / 0.450 / 0.473 s | 0.342 s | 1,785 |
| `Updates(page:1)` | 0.218 / 0.186 / 0.181 s | 0.380 s | 27,741 |
| **`CanonicalUpdates(page:1)`** | **3.627 / 3.525 / 3.598 s** | **3.654 s** | 6,521 |

---

## 3. "Every update hits the VPS" — CONFIRMED, twice per view

```
browser ──► komiq.cc (CF Worker, edge SSR)
              │ 3× POST /graphql ──► VPS      results DISCARDED (skeleton HTML)
              ▼
            HTML shell (s-maxage=30, zero content)
              │ hydrate
              └─ 3× POST https://api.komiq.cc/graphql ──► VPS   (uncached)
```

**Cache posture, measured:**
```
POST /graphql                  → cf-cache-status: DYNAMIC
                                 no cache-control, no etag, no last-modified
GET  /covers/*.webp            → max-age=31536000, immutable → HIT ✅
GET  /api/v1/manga/N/thumbnail → same, immutable ✅
```

- **GraphQL is POST-only** (`main.rs:1045-1048`), so Cloudflare can never cache
  it. No ETag or Last-Modified anywhere on `/graphql`. Cache-control is set only
  on image/asset routes (`main.rs:475,665,683,833`).
- **No server-side memoization** — no `moka`/`lru`/response cache in
  `apps/server`. `resolve_series_cached` is DB-first, not a response cache.
- **No precomputed feed.** `canonicalUpdates` (`mod.rs:1946-1978`) is computed
  per-request:
  ```
  SEARCH ss USING INDEX idx_source_series_type_key (source_type=?)   ← 109,101 rows
  SEARCH w  USING INDEX sqlite_autoindex_work_1 (id=?)
  SEARCH c  USING INDEX idx_chapter_ss_lang_pubdate (source_series_id=? AND lang=?)
  USE TEMP B-TREE FOR GROUP BY
  USE TEMP B-TREE FOR ORDER BY
  ```
  It groups **804,729 chapter rows across 42,275 works** and sorts them on every
  request to return 20. It grows linearly with the mirror.
- **Cost per `/updates` view: up to 6 GraphQL round-trips**, two of them the
  4-second `canonicalUpdates`, **half of them thrown away** by SSR that renders
  nothing.
- Cloudflare also rewrites `max-age=0` → `public, max-age=14400, s-maxage=30` (a
  dashboard Browser-Cache-TTL rule), so the empty shell is pinned in browsers for
  4 h.

---

## 4. Prioritized defects

| # | Sev | Defect | Location |
| --- | --- | --- | --- |
| U1 | **P0** | Duplicate `{#each}` key throws in production; page dies after hydration | `source.ts:684`, `+page.svelte:93` |
| U2 | **P0** | `canonicalUpdates` page 1 is 100% far-future junk; no `published_at <= now` guard | `mod.rs:1946-1978` |
| U3 | **P0** | Updates SSR returns an unawaited promise → zero content, and the edge caches the empty shell | `updates/+page.ts:13` |
| U4 | P1 | `Series.updatedAt` = last poll time, not last chapter | `mod.rs:700`, `series_cache.rs:40-85` |
| U5 | P1 | "Recently updated" orderings sort by poll time → the jumble | `series_cache.rs:280,388` |
| U6 | P1 | `last_new_chapter_at` batch-stamped `now()`, not real chapter time | `scanner.rs:761-766` |
| U7 | P1 | NSFW filtered **after** LIMIT → feeds under-fill or come back empty | `mod.rs:1789-1791` vs `:17` |
| U8 | P2 | `canonicalUpdates` = 3.5–4.0 s, two temp B-trees over 804,729 rows, every view | `mod.rs:1946-1978` |
| U9 | P2 | No HTTP caching on `/graphql` — POST-only, no ETag, `DYNAMIC` | `main.rs:1045-1048` |
| U10 | P2 | SSR fires 3 discarded queries per edge miss (consequence of U3) | — |
| U11 | P3 | `trendingGroups` and `hotUpdates` are the same array | `source.ts:682,685` |

---

## 5. Plan

### Stage 1 — reader deploy only, ship today

- **U1** — wrap `newUpdates` in `dedupeCardsByTitle()` (already exists and is
  already used by `getHome`), *and* make the `{#each}` key unique regardless
  (include the series id). Belt and braces: the dedupe fixes today's collision,
  the key change makes the class of bug non-fatal.
- **U3** — `return { updates: browser ? u : await u }`, mirroring
  `(app)/+page.ts:22-26`. This alone converts the `s-maxage=30` edge cache from
  caching an empty shell to caching real HTML, collapsing N viewers per 30 s per
  PoP into one origin hit.

These two are independent of every server change and unblock the page immediately.

### Stage 2 — batched Rust rebuild

- **U2** — add `AND c.published_at <= :now` to the `canonicalUpdates` grouping.
- **U7** — apply the NSFW predicate in SQL before `LIMIT`, not in Rust after it.
- **U4 + U5 + U6** — the semantics fix, and the one that needs the most care:
  1. Introduce a distinct field for genuine chapter recency. `updatedAt` is
     already consumed by the reader as a "· 4h" label, so changing its meaning
     in place will silently alter every card.
  2. Source it from the newest chapter's real `published_at`, the way
     `catalog::latest_english_chapter_at()` already does for the canonical path
     — **not** from `last_new_chapter_at`, which is a batch-stamped detection
     time.
  3. Repoint `series_cache.rs:280` and `:388` at it.
  4. Backfill: the correct value is derivable from existing `chapter` /
     `suwayomi_chapter` rows, so this is a migration, not a re-scan.
- Index migration — `work(updated_at)`, `work(primary_title)`, global
  `chapter(published_at DESC)`. Today a global newest-first chapter feed has no
  supporting index and scans 804,729 rows.

### Stage 3 — get updates off the VPS

In order; each step is useful on its own.

1. **Materialize the feed.** Add `feed_updates` (work_id/series_id, title, cover,
   latest_chapter, latest_at), refreshed by the scanner and mangadex sync **when
   a chapter actually lands**. Serve it with an index range-scan instead of an
   805K-row `GROUP BY`. Removes the 3.5–4.0 s outright, and — because it's
   written only on genuine chapter arrival — it is also the natural home for the
   correct `latestAt` from U4.
2. **Add a cacheable GET path.** `GET /feed/updates.json` with `ETag` +
   `Cache-Control: public, s-maxage=60, stale-while-revalidate=300`. POST GraphQL
   is structurally uncacheable at any CDN; this is the step that actually makes
   "updates don't hit the VPS" true. Cloudflare then serves the overwhelming
   majority of requests from the edge.
3. **Optionally publish to KV/R2.** The VPS writes the feed JSON to Cloudflare
   KV/R2 whenever the scanner detects a chapter; the reader Worker reads it at
   the edge. Then updates **never** touch the VPS — at the cost of a second
   write path and a staleness window.

**Do U2 and U7 before caching anything**, or the cache will faithfully serve
far-future junk with a 60 s TTL.

Also required, and outside this doc's code: set the Cloudflare Browser Cache TTL
to *Respect Existing Headers*. With it at 4 h, every caching improvement above is
partly masked by browsers holding stale HTML.

### Verification

- `/updates` renders cards, not skeletons, with JS disabled.
- No `each_key_duplicate` in the browser console across 10 reloads.
- `canonicalUpdates` page 1 contains no `latestAt` in the future.
- `canonicalUpdates` p95 under 100 ms.
- Anonymous `discovery` returns a full 20-item POPULAR row.
- Second request to `/feed/updates.json` from a different IP returns
  `cf-cache-status: HIT`.
- Card timestamps correlate with real chapter dates — re-run the Spearman check
  and expect ρ near 1, not −0.06.
