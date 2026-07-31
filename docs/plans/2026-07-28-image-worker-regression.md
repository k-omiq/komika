# Image-Worker instability — diagnosis & fix plan — 2026-07-28

**Reported:** after the `catalogue-browse-overhaul` deploy (source picker + series
detail reached via non-canonical, Suwayomi-anchored numeric entries), the image Worker
(`img.komiq.cc`) went unstable — images don't return, requests get blocked (4xx/5xx),
and load times spiked.

**Verdict:** the Worker code did **not** change (`apps/worker/src/index.ts` untouched
since Jul 18). The deploy changed *what the Worker fetches* and *how much of the
traffic takes the slow path*. It is a **traffic-mix regression**, not a code defect in
the Worker.

---

## 1. What actually changed vs. the last version

The Suwayomi vs. MangaDex split, numeric reader ids, and the `api.komiq.cc` page/cover
proxy **all already existed** before this deploy. None of the individual mechanisms are
new. What the overhaul changed is the **proportion of traffic** that flows down the
expensive path, and it crossed two capacity thresholds at once.

### The two image paths (both pre-existing)

| | Canonical path (cheap) | Suwayomi path (expensive) |
|---|---|---|
| Reader chooses when | `suwayomiMangaId === null` (`activeSpine`) | numeric `suwayomiMangaId` |
| Resolver | `canonicalPages` (`graphql/mod.rs:3284`) | `pages` → `suwayomi.pages()` (`:3727`) |
| Page host | `*.mangadex.network` (MangaDex@Home — global, purpose-built CDN) | `https://api.komiq.cc/api/v1/manga/.../page/...` (**our single origin VPS**) |
| Cover host | cached `/covers/*.webp` (static) | `api.komiq.cc/api/v1/manga/{id}/thumbnail` (cover pool → Suwayomi) |
| Hops browser→bytes | 2 (browser → `img.komiq.cc` → CDN) | 4 (browser → `img.komiq.cc` → `api.komiq.cc` → loopback Suwayomi `:4567` → source site) |

`abs()` (`suwayomi.rs:344`) builds the Suwayomi URLs against
`SUWAYOMI_PUBLIC_URL=https://api.komiq.cc`.

### Why it *suddenly* got worse

Three things stacked in this one deploy:

1. **Browse now exposes the whole catalogue** (`906b515` "browse the whole catalogue").
   The catalogue is dominated by Suwayomi-library works, not the canonical MangaDex
   spine. Before, discovery surfaced a canonical-heavy set, so most reads defaulted to
   `mangadex.network`. Now most *browsable* works are Suwayomi-anchored.
2. **The picker fixes made those works actually open on their Suwayomi source**
   (`720aff8` "show the source picker on Suwayomi-anchored works", `a2d1241` "open a
   chapter from the source that actually has it"). The reader now lands on the numeric
   Suwayomi entry that carries the chapters — i.e. the `api.komiq.cc` path.
3. **The deploy invalidated the entire edge cache** for the newly-dominant works: their
   page/cover URLs are keyed on `api.komiq.cc`, not `mangadex.network`, so day-one is a
   100% cache-miss storm against origin.

Net effect: the share of image bytes flowing through the single `api.komiq.cc` origin
funnel jumped from a **minority** to the **majority**, simultaneously crossing:
- **origin / Suwayomi throughput** (baseline ~78 rps / ~390 users, one 4-core box that
  Suwayomi shares with the scanner — see `2026-07-27-performance-investigation.md`), and
- **the Worker's per-IP rate limit** (below).

A latent, tolerable inefficiency became a visible outage because it stopped being a
corner case.

---

## 2. Symptom → mechanism

**"Doesn't return images / gets blocked"**
- **Cold edge cache (100% miss).** URL host shifted, so pre-deploy `caches.default`
  entries are dead weight; every request reaches origin (`index.ts:99-108`).
- **Worker 429.** `IMG_RATE_LIMITER = 200 req / 60s / IP`, checked *only on a cache
  miss* (`wrangler.toml`, `index.ts:110-117`). A cold-cache reader flipping two ~40-page
  chapters plus a cover grid exceeds 200/min. Keyed on `CF-Connecting-IP`, so a whole
  NAT/office/campus shares **one** bucket.
- **Worker 400 "src host not allowed."** `abs()` passes any absolute `http…` URL through
  unchanged (`suwayomi.rs:347`). Extensions that return absolute source-CDN URLs land on
  hosts absent from `ALLOWED_SOURCE_HOSTS` (`uploads.mangadex.org, mangadex.network,
  api.komiq.cc`) → rejected. Scraper sources that 403/ban → origin 502 → Worker 502.

**"Takes a lot of time"**
- 4 hops instead of 2, and the last two are the slow ones (our origin, then a live
  third-party scrape).
- **The page proxy is unbounded.** `serve_suwayomi_image` → `fetch_image`
  (`main.rs:610-617`, `suwayomi.rs:469`) has **no concurrency semaphore and no short
  timeout** — unlike the cover path, which uses a bounded pool + 8s timeout. A 40-page
  chapter fires 40 uncapped fetches that queue inside Suwayomi behind the scanner's own
  traffic (shared budget).
- Page bytes and GraphQL now contend for the same origin, so they degrade together.

---

## 2b. The CPU-limit failure (sharper root cause — the reported symptom)

Follow-up report: the Worker "partially fetches then breaks", and Cloudflare logs
**"Exceeded CPU Time Limit"**. That is a *compute* error, not the I/O/rate-limit story
above — F0/F1 don't touch it. Disambiguation:

- The reader is a **client-rendered SPA** (`apps/reader/src/routes/+layout.ts` →
  `export const ssr = false`), so `getReaderChapter`/`resolveWork` run in the *browser*,
  not on the reader's edge worker. → the CPU-dying worker is the **image proxy**
  (`img.komiq.cc`), not the reader SSR worker.
- `WebImageProvider.resolvePage` routed **every** page through the Worker — including
  pages already on our own `api.komiq.cc` origin. Covers avoid this
  (`resolveCoverSync` → `isOwnAsset`); pages did not.
- Suwayomi serves **raw full-resolution** pages (multi-MB; `main.rs` — "pages are served
  raw"). The Worker streams every byte through a JS `TransformStream` (`capStream`) **and**
  tees the whole body via `res.clone()` for `caches.default` — per-request CPU that, on a
  multi-MB page, exceeds the Worker's CPU limit and kills the response mid-stream. Small,
  optimised MangaDex@Home images stayed under budget; the source-picker shift to raw
  `api.komiq.cc` pages pushed it over. → "partially fetch, then break".

### F5 — Own-origin pages bypass the image Worker — ✅ DONE (the fix for this symptom)

The Worker exists only to bypass CORS/hotlink on foreign CDNs (MangaDex). An `<img src>`
needs neither for our own origin, which already sends `immutable` cache headers, so
laundering our own multi-MB pages back through it is pure overhead *and* the CPU failure
above.

- `packages/api/src/image-provider.ts`: `resolvePage` now serves an own-origin page
  (`api.komiq.cc/api/v1/manga/.../page/...`) **directly**, mirroring the cover path
  (`isOwnAsset` → `toAbsoluteOwn`). Foreign hosts (`*.mangadex.network`) still route
  through the Worker, where the proxy is genuinely required.
- Reader-only, no server change. Type-checks clean; `apiOrigin` is already wired
  (`context.ts`), page URLs are absolute `api.komiq.cc` (server `abs()`), and pages render
  as plain `<img>` (no CORS, no `crossorigin`).
- Not unit-tested: `packages/api` has no test runner and the provider uses TS parameter
  properties (unsupported by `node --test --experimental-strip-types`); the logic is an
  exact mirror of the production-exercised cover path.

**Trade-off / follow-up:** this removes the Worker's edge cache from the page path.
Browser `immutable` caching still covers same-user re-reads; for cross-user edge caching,
add a **Cloudflare Cache Rule** on `api.komiq.cc/api/v1/manga/*/page/*` (config, no code) —
and/or land **F4** (origin-cache page bytes). The Worker cache was in any case being
poisoned by the mid-stream CPU kills, so nothing working is lost. The remaining "slow on
the rest" is origin/Suwayomi latency → **F2/F4**.

**Optional Worker hardening (defense-in-depth, not required post-F5):** in
`apps/worker/src/index.ts`, skip the JS `capStream` when a trustworthy `Content-Length ≤
MAX` is present and pass `originResp.body` through natively — removes per-byte JS for the
remaining MangaDex path too.

## 3. Fix plan

Ordered by impact-per-effort. Each item is independently shippable.

Deploy note: the Worker (`wrangler.toml`, `apps/worker/`) and the Rust server deploy
separately (see `deploy-topology.md`). Worker/config-only changes (F0, F3) ship without
a server rebuild; F1/F2/F4 are server rebuilds that restart ingest.

### F0 — Immediately relieve the block (Worker config only) — LOW effort

- Raise `IMG_RATE_LIMITER.simple.limit` from **200** to a chapter-aware value (e.g.
  600–1000 / 60s), and/or move the ceiling so shared-IP networks aren't one bucket.
  A single manga page-turn session legitimately needs hundreds of misses on a cold cache.
- Verify `ALLOWED_ORIGINS` still matches the live reader origin (`komiq.cc`,
  `www.komiq.cc`) so no legitimate browser traffic is 403'd.
- **Ship first** — it is the fastest lever on "gets blocked" and needs no server rebuild.

### F1 — Prefer the MangaDex-direct spine over the redundant all.mangadex extension — HIGH impact — ✅ DONE

**Refined rule (per product owner):** `mangadex.network` is used *only* for
MangaDex-direct content (the canonical spine). Every *other* extension (Asura, etc.)
serves its own content via `api.komiq.cc` and is never overridden — there is **no
completeness tradeoff** and no "pick MangaDex instead of a real scanlation source".

**Root of the leak (confirmed by investigation):** the MangaDex Tachiyomi extension
`eu.kanade.tachiyomi.extension.all.mangadex` is catalogued as a *Suwayomi* source
(`source_type='suwayomi'`, numeric `source_key`), fully distinct from the MangaDex-direct
spine (`source_type='mangadex'`, UUID `source_key`). They are the **same MangaDex
content** — one via `mangadex.network` (`canonicalPages`), one via `api.komiq.cc`
(`pages`). The reader offered both as separate translators and defaulted to whichever
had the **most chapters**; since MangaDex often strips the direct spine on a takedown,
the slow extension frequently won the default and funnelled MangaDex reads onto the
origin proxy. Two documented leak paths (both fixed by this one change, because numeric
browse cards recurse into the same `resolveWork` selection):
  1. default/persisted selection preferring the all.mangadex extension over the spine;
  2. the 293 scanner-first browse rows whose `reader_id` is the numeric Suwayomi id
     (`migrations/0069_browse_catalogue.sql:51-53`) — their read still recurses to the
     owning work's canonical selection.

**Implementation** (reader-only — no server rebuild):
- `apps/reader/src/lib/data/translator-select.ts` (new): pure, tested helpers
  `isRedundantMangadexExt()` and `pickDefaultKey()` (mirrors the `chapter-owner.ts`
  split so the selection precedence is unit-testable).
- `apps/reader/src/lib/data/source.ts` `resolveWork()`: an all.mangadex source is flagged
  `redundant` **only when the direct spine carries ≥1 chapter**. Redundant candidates are
  excluded from the default pick and from the picker (no double "MangaDex", no manual
  route onto the slow proxy), but **kept in `candidates`** so a `?ch=` deep link naming
  one of the extension's chapters still resolves via `findChapterOwner` (guards the
  blank-reader regression `a2d1241` fixed).
- `apps/reader/src/lib/data/translator-select.test.ts` (new): 8 tests — prefers spine over
  a higher-count redundant extension, heals stale persisted prefs, does NOT override a
  genuine non-MangaDex source (Solo Leveling → Asura), keeps the extension when the spine
  is empty (takedown).

**Not changed:** non-MangaDex sources (they are the sole source of their content); the
empty-spine takedown case (extension stays the readable MangaDex path).

**Follow-up (293 rows) — ✅ DONE.** These rows linked to `/series/<numeric>` (Suwayomi page:
raw `api.komiq.cc` cover, no source picker) instead of their canonical `/series/w_` page. Root
cause: a MangaDex-anchored work whose mirror carries no dated chapter (a takedown) gets its
`feed_series_updates` row from the scanner/Suwayomi half, which wrote a numeric `reader_id`.
Fix: `reader_id` is now **anchor-derived** (the `w_` id whenever a `mangadex` `source_series`
exists) in both feed derivations (`catalog::refresh_feed_series_updates` +
`scanner::upsert_feed_series_update`, kept identical so the convergence test stays green), and
`browse_catalogue_select` prefers the anchor over the copied feed id. Migration
`0070_reader_id_anchor_heal.sql` heals existing rows in both `feed_series_updates` and
`browse_catalogue` (the runtime rebuild also corrects them at boot). New test
`feed_reader_id_is_canonical_for_a_takedown_anchored_work`; full server suite 387 passed.
Server rebuild.

### F2 — Bound + timeout the page proxy — ✅ DONE

Mirror the cover pool's discipline on the page path so a slow/blocking source can't pile
up unbounded fetches and starve the origin.

- `apps/server/src/suwayomi.rs`: new `fetch_page_now` + `fetch_page_inner` on a **separate**
  page pool (`PAGE_FETCH_CONCURRENCY = 16`, own `page_http` client at `PAGE_TIMEOUT_SECS =
  20`, body streamed under `MAX_PAGE_SOURCE_BYTES = 32 MiB` via `read_capped`). Separate
  from the cover pool so page bursts and cover demand never evict each other.
- Unlike covers (instant fail-fast — a background warmer converges), a page has no
  materializer, so `fetch_page_now` **waits up to `PAGE_ACQUIRE_WAIT_SECS = 6`** for a
  permit before shedding `PageFetchError::Busy` — rides out a transient burst instead of
  breaking a page, but still sheds (never the old 30 s pile-up) under sustained load.
- `apps/server/src/main.rs`: `serve_suwayomi_image`'s page branch now calls `fetch_page_now`;
  `Busy` → `page_busy_response()` (retryable 503 + `Retry-After: 2` + `no-store`; the reader
  renders its per-page "tap to retry" affordance on any non-image response). The orphaned
  unbounded `fetch_image` was removed.
- `cargo check` + `clippy` clean (no new warnings).

### Worker hardening — ✅ DONE (defense-in-depth after F5)

`apps/worker/src/index.ts` `finalizeImage`: stream the body **natively** when the upstream
declared a `Content-Length` (already validated ≤ cap by the caller), instead of piping
every byte through the JS `capStream` TransformStream. That per-byte JS was the Worker's
CPU cost; passing trusted-length bodies through untouched removes the latent CPU cliff for
the remaining MangaDex path too. `capStream` is still used when the length is absent (the
case it exists to guard). Type-checks clean.

### F3 — Close the allowlist / absolute-URL gap — LOW effort, correctness

- Either add the real source-CDN hosts to `ALLOWED_SOURCE_HOSTS`, **or** (preferred)
  make `abs()` route absolute source URLs through the `api.komiq.cc` proxy instead of
  emitting them raw, so they never reach the Worker's host allowlist as a 400.
- Preferred because it keeps the Worker fail-closed and avoids an ever-growing host list.

### F4 — Cache page bytes — ⚠️ REASSESSED: prefer a Cloudflare Cache Rule over an origin blob cache

Original idea: give pages a `suwayomi_cover_blob`-style origin cache. On closer look that's
the **wrong layer**:
- Covers earn an origin blob cache because they are **re-encoded** (expensive to
  regenerate) and **small** (one ~150 KB WebP per manga). Pages are neither: raw
  pass-through bytes, full-resolution, and tens per chapter across the whole catalogue.
  An origin page cache would balloon the SQLite/disk on a VPS already shared with two
  other prod stacks, and needs a real size-bounded LRU + eviction to be safe.
- Page bytes are `immutable` and belong at the **CDN edge**, not origin disk. Before F5,
  the image Worker's `caches.default` provided exactly that (until it CPU-died on big
  pages). After F5, pages load directly from `api.komiq.cc`.

**Recommended (config, no code — the operator must apply it):** add a **Cloudflare Cache
Rule** on the proxied `api.komiq.cc` zone so page responses are edge-cached, restoring the
cross-user edge cache the Worker used to give — with zero origin storage:

```
When:  (http.host eq "api.komiq.cc"
        and starts_with(http.request.uri.path, "/api/v1/manga/")
        and http.request.uri.path contains "/page/")
Then:  Cache eligibility = Eligible for cache
       Edge TTL = "Use cache-control header if present" (origin already sends
                  Cache-Control: public, max-age=31536000, immutable)
```

The same rule can include `/thumbnail` to edge-cache Suwayomi covers too. With this rule +
F2 (origin protected from bursts) + browser `immutable` caching (same-user re-reads), an
origin blob cache adds cost without meaningful benefit, so it is **not** implemented.

If a CF Cache Rule is undesirable, the fallback is a **bounded LRU** page-blob cache (hard
total-size cap, oldest-evicted) — deliberately deferred pending that decision, given the
disk constraint.

### Sequencing

1. **F0** now (unblocks users, Worker-only).
2. **F1** next (removes the majority of origin-funnel traffic).
3. **F2 + F3** together (safety + correctness on the remaining Suwayomi path).
4. **F4** if origin load is still material after F1.

---

## 4. Verification

After each change, re-run the standard post-deploy checks (`deploy-verification-checks`)
plus:

- `curl -I 'https://img.komiq.cc/img?src=<encoded api.komiq.cc page URL>'` → 200,
  `cache-control: public, max-age=604800, immutable`; second hit served from edge.
- Confirm no 400 "src host not allowed" for real reader page/cover URLs (F3).
- Load a Suwayomi-anchored series + a cover-heavy browse page in a real browser; watch
  for 429s in the network panel (F0) and check page-turn latency (F1/F2).
- Server: watch for `suwayomi cover: pool saturated` and the new page-proxy saturation
  logs; they should be near-zero in steady state.

---

## Evidence index

| Claim | Location |
|---|---|
| Worker unchanged | `apps/worker/src/index.ts` (last touched Jul 18) |
| Canonical vs Suwayomi page resolvers | `apps/server/src/graphql/mod.rs:3284`, `:3727` |
| Cover fallback → api.komiq.cc | `graphql/mod.rs:1986`, `:3030` |
| `abs()` passes absolute URLs through | `apps/server/src/suwayomi.rs:344-350` |
| Unbounded page proxy | `apps/server/src/main.rs:610-617`, `suwayomi.rs:469` |
| Bounded cover pool (the pattern to copy) | `main.rs:635-697` |
| Rate limiter 200/60s per-IP, miss-only | `apps/worker/wrangler.toml`, `index.ts:110-117` |
| Host allowlist | `apps/worker/wrangler.toml` `ALLOWED_SOURCE_HOSTS` |
| Origin capacity baseline | `docs/plans/2026-07-27-performance-investigation.md` |
| Prod image config | `apps/reader/.env.production`, `SUWAYOMI_PUBLIC_URL` |
