# Komika — Catalogue Consolidation + Server/Cover Optimisation Plan

**Scope:** two user-defined workstreams (A: catalogue seed/reconcile; B: cover/reader
performance) woven together with a four-dimension server optimisation audit (request
latency, DB/SQLite, ingest throughput, build/CPU).

**Working dir:** `/home/ubuntu/komiq/komika` — **this checkout IS the live production VPS.**

**Status:** planning. Nothing below is applied yet.

---

## 0. Governing constraints (read before touching anything)

1. **Every `docker compose build server` + `up -d server` recompiles the whole tree and
   restarts the container, interrupting any in-flight ingest/sync.** Therefore server
   changes are **batched into exactly two rebuilds**, chosen so neither one interrupts
   the long re-seed:
   - **Build #1** — before the re-seed: seed correctness + universally-safe perf.
   - **Build #2** — *after* the seed finishes, *before* re-reconcile: ingest/reconcile perf.
2. **The re-seed runs for hours** (113,610 works at 4 req/s). Kick it after Build #1, then
   do all **reader** work (Workstream B, deploys via Cloudflare — no server rebuild) while
   it runs. Build #2 happens in the idle window after the seed completes.
3. **Merges are irreversible.** Get explicit user go-ahead before the full re-seed +
   re-reconcile (Phase 0 gate).
4. **DB `/data/komika.sqlite3` is container-owned (uid 10001); no `sqlite3` in the
   container.** All schema changes go through **sqlx migrations** (`apps/server/migrations/`,
   run at startup via `sqlx::migrate!` — [db.rs:25](../../apps/server/src/db.rs#L25)); all
   data resets go through **admin GraphQL mutations**. Next migration number is `0042`.
5. **Tests must stay green:** `cd apps/server && cargo test --bin komika-server` (191 today,
   binary crate — no `--lib`). Gate every build on `cargo check --all-targets -j4` first —
   a broken uncommitted change fails the whole image build.
6. **Commit to `main` as you go** (one logical change per commit). Don't `git push` unless
   asked — deploy is `docker build` from the tree, not `git pull`. Reader deploys via
   `wrangler` — let the user run it. End commit messages with:
   `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

### Deploy runbook (per server build)
```bash
cd /home/ubuntu/komiq/komika/apps/server && cargo check --all-targets -j4   # sanity
cargo test --bin komika-server                                              # keep 191 green
cd /home/ubuntu/komiq/komika/deploy
docker compose build server        # ~1–2 min warm
docker compose up -d server        # recreate; /data volume persists; ~secs downtime
curl -s http://localhost:8080/health   # -> ok
docker logs komika-server-1 --since 5m 2>&1 | grep -iE "mangadex|catalogue|reconcile|cover"
```

---

## Phase 0 — Prep, measurement, authorization (NO rebuild)

| ID | Task | Owner | Blocks |
|----|------|-------|--------|
| P0.1 | **Confirm irreversible re-seed + re-reconcile** with user (merges can't be undone; exact-title merges are policy-safe). | User | Phase 1+ |
| P0.2 | **B0 — Cloudflare Cache Rule for covers** (below). Biggest cover win; independent of server. | User | Cover HIT verification |
| P0.3 | Get a **fresh admin token** (`localStorage['komika-admin-token']` from the user's admin browser) for `require_admin` mutations. Don't read from process memory (classifier blocks it). | User | A2/A3, reconcile |
| P0.4 | **Baseline:** `cargo check --all-targets` + `cargo test --bin komika-server` (record 191 pass). | Me | — |
| P0.5 | **Benchmark the `opt-level` change (OPT-1) locally** — timing test `cover::tests::opt_level_hot_path_bench` (`#[ignore]`d), run under `--release` at `z` then `3`. No deploy. Validates OPT-1 before it ships in Build #1. | Me | OPT-1 decision |

**P0.5 measured result (2026-07-20, release profile, `lto=true`):** decisive — keep `opt-level=3`.

| Hot path | `opt-level="z"` | `opt-level=3` | Speedup |
|----------|-----------------|---------------|---------|
| `process_cover(700×1000)` | 561.4 ms median | 136.1 ms median | **4.1× (−76%)** |
| `phash::dhash(512×728)` | 12.57 ms median | 1.90 ms median | **6.6× (−85%)** |

Run: `cargo test --release --bin komika-server opt_level_hot_path_bench -- --ignored --nocapture`.
`Cargo.toml` is already set to `opt-level=3` in the working tree (ships with Build #1). The
`process_cover` cost (still 136 ms even at `-O3`) reinforces OPT-2b — it must run in `spawn_blocking`.

**P0.2 — Cloudflare Cache Rule (user, Dashboard → `komiq.cc` → Caching → Cache Rules):**
- Name: `Cache Suwayomi covers`
- Match: `(http.host eq "api.komiq.cc" and starts_with(http.request.uri.path, "/api/v1/manga/") and ends_with(http.request.uri.path, "/thumbnail"))`
- Then: Eligible for cache; Edge TTL: *Use cache-control header if present*; Browser TTL: respect origin.
- Do **not** also route covers through the `img.komiq.cc` Worker (redundant with this rule).
- Confirm origin sends a long `cache-control` on `/thumbnail` (it does today: `webp_cover_response`
  emits `public, max-age=31536000, immutable`). If ever not, that's a one-line server tweak.

---

## Phase 1 — Server Build #1: seed correctness + safe perf (ONE rebuild → then re-seed)

Bundle everything below into a single image build. Each item is its own commit; ship together.
Rationale for grouping: all are either seed-critical or behaviourally trivial (profile flag,
`spawn_blocking` wrappers, additive indices, per-connection PRAGMAs) — low risk of perturbing
the seed we're about to run.

### Correctness (Workstream A)

- **A1 — Harden `sync_catalogue` against premature truncation.**
  [mangadex.rs:822](../../apps/server/src/mangadex.rs#L822) (`if mangas.is_empty()`) and
  [mangadex.rs:837](../../apps/server/src/mangadex.rs#L837) (`if page_len < PAGE_LIMIT`) both
  latch `done = true` and end the whole sweep. A single transient short/empty page (rate-limit
  blip, 5xx, filtered page) at offset ~1,198 ended the seed and latched `seed_done`.
  - `list_manga` **already returns `total`** for the window — use `offset >= total` as the
    authoritative completion signal for a window. Treat an unexpected empty/short page *before*
    `offset >= total` as a **retryable anomaly**: retry the same offset a bounded number of
    times; only conclude "genuine end" when `total` is reached (or the window slides at
    `WINDOW_OFFSET_CAP`). Do **not** set `seed_done` on a short page that falls short of `total`.
  - **Mid-sweep cursor persistence:** a `set_seed_progress` checkpoint already exists but only
    fires on **window-slide** ([mangadex.rs, post-loop](../../apps/server/src/mangadex.rs#L888)).
    Add a per-page (or every-N-pages) checkpoint of the `createdAt` cursor within a window so a
    mid-window restart resumes near where it died instead of restarting the window at
    `since=None`. Helpers: `catalog::{set_seed_progress,set_sync_cursor,get_sync_state,SyncState}`
    ([catalog/mod.rs:1395+](../../apps/server/src/catalog/mod.rs#L1395)).
  - **Tests:** add a unit/integration test that a short page below `total` does **not** end the
    sweep, and that a resumed cursor picks up mid-window. Keep 191 green.

- **A2 — `resyncCatalogue` admin mutation.** Mirror `persist_catalogue` /
  `materialize_catalogue_covers` / `reconcile_catalogue` in `impl MutationRoot`
  ([graphql/mod.rs](../../apps/server/src/graphql/mod.rs)); `require_admin`. Resets the
  `catalogue` **and** `chapters` rows of `catalogue_sync_state` (`seed_done=0`, cursor cleared)
  and kicks a sync cycle. Needed because `seed_done` can't be reset via raw SQL (no `sqlite3`
  in container). Add a test asserting the reset flips `seed_done` back to false.

### Universally-safe perf (from the audit)

- **OPT-1 — `opt-level = "z"` → `3`** in `[profile.release]`
  ([Cargo.toml:43](../../apps/server/Cargo.toml#L43)). All hot paths are pure-Rust image codecs
  (VP8L WebP encode, Lanczos3 resize, JPEG/PNG decode, dHash); `z` suppresses autovectorisation
  and inlining → typically 1.5–3× slower than `-O3`. Binary grows a few hundred KB–~2 MB
  (irrelevant on a VPS). Keep `lto`/`codegen-units=1`/`strip`/`panic="unwind"`. Ship only if
  P0.5 confirms a real speedup (expected).

- **OPT-2 — Close `spawn_blocking` gaps** (CPU work running inline on the Tokio reactor; the
  avatar/comment handlers already do this right — these are the outliers):
  - **2a — Argon2 login/register/change-pw:** [graphql/mod.rs:4302](../../apps/server/src/graphql/mod.rs#L4302),
    [:4398](../../apps/server/src/graphql/mod.rs#L4398), [:6117](../../apps/server/src/graphql/mod.rs#L6117)
    (10–50 ms CPU/call; a login flood freezes worker threads). Move owned strings into
    `tokio::task::spawn_blocking`.
  - **2b — `serve_suwayomi_cover`:** [main.rs:480](../../apps/server/src/main.rs#L480) — the
    in-flight (uncommitted) cover path runs `process_cover` (decode + up to 5× resize+encode)
    inline. Wrap in `spawn_blocking`. **Fold this into the commit that lands the uncommitted
    cover work** (see A3-commit note below).
  - **2c — Enrolment `dhash`:** [graphql/mod.rs:5003](../../apps/server/src/graphql/mod.rs#L5003),
    [:5554](../../apps/server/src/graphql/mod.rs#L5554), [:5718](../../apps/server/src/graphql/mod.rs#L5718)
    — wrap for consistency (lower urgency; not per-request).

- **OPT-3 — SQLite PRAGMAs + busy_timeout** on both pools
  ([db.rs:14](../../apps/server/src/db.rs#L14) main, [:37](../../apps/server/src/db.rs#L37) covers).
  None of `mmap_size`/`cache_size`/`temp_store` are set today. Add:
  `.pragma("mmap_size","268435456")` (256 MB), `.pragma("cache_size","-16000")` (~16 MB),
  `.pragma("temp_store","MEMORY")`; and raise `busy_timeout` 5s → 15s (matches the "SQLite
  single-writer, expect `database is locked` under sync+drainer+scanner overlap" gotcha).
  All per-connection, **Litestream-safe** (no effect on WAL shipping).

- **OPT-4/5 (+ optional composite) — Additive indices, new migration `0042_perf_indices.sql`
  on the main (Litestream-replicated) DB:**
  - `series_scan_state.last_new_chapter_at` — the "Latest Updates" home feed
    ([graphql/mod.rs:1790](../../apps/server/src/graphql/mod.rs#L1790)) full-scans + filesorts
    every federated series today (table is PK-only):
    `CREATE INDEX idx_scan_state_new_chapter ON series_scan_state(last_new_chapter_at DESC) WHERE last_new_chapter_at IS NOT NULL;`
  - `work.cover_cached_version` — the cover drainer
    ([cover.rs:184](../../apps/server/src/cover.rs#L184)) full-scans `work` each tick:
    `CREATE INDEX idx_work_cover_pending ON work(cover_cached_version) WHERE cover_cached_version IS NULL;`
  - *(optional, cheap)* `canonical_updates` MAX-per-group
    ([graphql/mod.rs:1859](../../apps/server/src/graphql/mod.rs#L1859)):
    `CREATE INDEX idx_chapter_ss_pubdate ON chapter(source_series_id, published_at, created_at);`

### Commit the in-flight cover work

- **A3-commit — Commit the uncommitted, already-deployed cover cache** (`db.rs` `init_covers`
  `suwayomi_cover_blob`, `cover.rs` `get/put_suwayomi_cover`, `main.rs`
  `thumbnail_manga_id`/`serve_suwayomi_cover`/`webp_cover_response`/`raw_image_response`).
  **Land OPT-2b in the same commit** so the cover path ships wrapped in `spawn_blocking`.

### Gate → build → seed (A3)

1. `cargo check --all-targets` + `cargo test --bin komika-server` green.
2. `docker compose build server && docker compose up -d server`; `curl /health` → ok.
3. **Trigger `resyncCatalogue`** (admin token). Watch logs — do **not** assume:
   `docker logs komika-server-1 --since 30m 2>&1 | grep -iE "catalogue page|cycle done"`
   Expect `offset=` climbing toward ~113k and finally `cycle done upserted≈113k incremental=false`.
4. Confirm restart-resume: (optional) restart the container mid-seed and verify it resumes near
   the last cursor, not from `createdAt=0`.

---

## Phase 2 — Reader / cover performance (runs DURING the seed; deploys via wrangler, no server rebuild)

Reader is a separate Cloudflare Worker (`apps/reader`, SvelteKit + `adapter-cloudflare`).
`pnpm` is broken in this env — typecheck via `apps/reader/node_modules/.bin/svelte-check`.
**Let the user run the deploy** (`cd apps/reader && pnpm build && pnpm dlx wrangler deploy`).
These changes **ride along with the already-committed-but-undeployed reader changes**
(chapter-sort fix, browse Load More, Cancelled filter, format-tab removal) — build on them,
don't revert.

- **B1 — Browse search back-nav cache.**
  [browse/+page.svelte](../../apps/reader/src/routes/(app)/browse/+page.svelte) keeps search
  state (`rows`, `catalogPage`, `hasNext`, `rowsAreFederated`, `totalCount`, rail filters) as
  component-local `$state`, so series→back remounts, re-runs the whole search, and loses scroll.
  Add a **module-scoped cache keyed by query+filters signature**: on mount, if the signature
  matches, hydrate state and skip the initial fetch; else fetch and write back. Preserve the
  existing debounce/cancellation. *(Optional: mirror rail filters `status`/`minRating`/
  `maxRating`/`sort` into the URL.)*

- **B2 — SSR the cover `<img>`.**
  [Cover.svelte](../../apps/reader/src/lib/components/Cover.svelte) resolves the URL in a
  browser-only `$effect`, so SSR HTML has no cover `<img>` (hurts LCP).
  `WebImageProvider.resolveCover` ([packages/api/src/image-provider.ts](../../packages/api/src/image-provider.ts))
  is a **synchronous** string transform — resolve it during SSR so `<img src>` is in server HTML;
  add `loading="lazy"` below the fold. **Preserve the Tauri path:** `NativeImageProvider.resolveCover`
  is genuinely async (blob URL) — keep the `$effect`/promise path when `isTauri()`, render
  eagerly only on web.

- **B3 — Investigate HTML `Cache-Control` (REPORT before changing).**
  Live HTML returns `cache-control: public, max-age=14400` despite loads setting
  `max-age=0, s-maxage=…`. Find the override — a `_headers` file (`apps/reader` or
  `.svelte-kit/cloudflare/_headers`), a CF Transform Rule, or a stray `setHeaders`. Determine if
  intentional and **report to the user** (4h HTML caching can serve stale content after new
  chapters). Only change with user sign-off.

---

## Phase 3 — Server Build #2: ingest/reconcile perf (after seed completes, before re-reconcile)

The seed is done → this rebuild interrupts nothing. These optimisations directly speed the
re-reconcile that immediately follows and general ingest.

- **OPT-7 — Batch `load_match_data` in the dedup loop.**
  [dedup.rs:174](../../apps/server/src/dedup.rs#L174) scores candidates one query-pair at a time;
  with up to ~150 candidates × 2 queries each = **~300 sequential round-trips per resolved item**.
  Replace with one `SELECT ... FROM work WHERE id IN (?)` + one
  `SELECT work_id, normalized_title FROM work_alias WHERE work_id IN (?)`, grouped in memory.
  **Directly cuts re-reconcile time over the ~10.5k skipped works.** Pure refactor, same
  semantics — add a test that batched scoring matches the per-item path.

- **OPT-6 — Reorder idempotency check ahead of the fetches in `ingest_source_series`.**
  [graphql/mod.rs:5541](../../apps/server/src/graphql/mod.rs#L5541) does ~4 upstream round-trips
  + cover download + dhash **before** the `"existing"` short-circuit at
  [:5909](../../apps/server/src/graphql/mod.rs#L5909). Call
  `catalog::find_source_series("suwayomi", …)` at the **top**; if already linked + `in_library`,
  return early. Also remove the **double `put_series`** (once at
  [:5552](../../apps/server/src/graphql/mod.rs#L5552), again inside `scan_series`
  [scanner.rs:406](../../apps/server/src/scanner.rs#L406)). Add a test that a re-enrol of an
  existing linked series issues no upstream fetch.

- **OPT-9 — Parallelise + `spawn_blocking` the Suwayomi cover crawl.**
  [cover.rs:330](../../apps/server/src/cover.rs#L330) second pass is serial **and** unthrottled
  (no rate limiter forces serial). Use `futures::stream::iter(...).buffer_unordered(4–6)` and
  offload `process_cover` to `spawn_blocking`. Deliberately deferred to Build #2 so the added
  concurrency doesn't pile onto the engine *during* the seed.

- **OPT-8a — Collapse per-series lazy resolvers (cheap version).**
  `views` runs 4 sequential queries each ([views.rs:116](../../apps/server/src/views.rs#L116) —
  fold the two window-sums into one `SUM(CASE WHEN hour_ts > ? …)`); `is_marked` /
  `library_status` / `is_favorite` hit the **same** `user_library` row 3× — merge into one
  `SELECT`. Cuts ~7N avoidable round-trips per N-series feed. (Structural DataLoader = Phase 4.)

### Gate → build → re-reconcile (A3 cont.)

1. Tests green → `docker compose build server && up -d server` → `/health` ok.
2. **Re-run `reconcileCatalogue`** (admin token). Expect a **much larger `merged`** count than
   the prior run (merged 838 / queued 356 / skipped 10,509) now that the spine is ~full — it
   re-checks the 10,509 previously skipped Suwayomi works.
   `docker logs komika-server-1 --since 30m 2>&1 | grep -iE "reconcile"`

---

## Phase 4 — Optional follow-ups (separate later rebuild / batch)

Lower blast radius; schedule independently, none block Definition of Done.

- **OPT-8b — async-graphql `DataLoader`** for `user_library` and view-counts (no DataLoader
  exists anywhere today). Highest-leverage *structural* read-latency fix; supersedes OPT-8a's
  query-collapse for feed selections.
- **Tier-3 ingest micro-wins:** `tokio::join!` the two Suwayomi home-feed fetches +
  batch the insert loop ([graphql/mod.rs:1668](../../apps/server/src/graphql/mod.rs#L1668));
  `buffer_unordered` the per-work `/cover` fetches in `enrich_works`
  ([graphql/mod.rs:5671](../../apps/server/src/graphql/mod.rs#L5671)) and `sync_catalogue`
  cover-pHash ([mangadex.rs:817](../../apps/server/src/mangadex.rs#L817)) — token bucket still
  caps 5/s, just hides RTT.
- **Dedup CPU:** precompute candidate shingles once instead of re-shingling per comparison
  (dedup.rs `score_candidate` / `similarity.rs`).
- **Scan tick:** small bounded `buffer_unordered(3–4)` over *due* series only
  ([scanner.rs:348](../../apps/server/src/scanner.rs#L348)) — keep modest (FlareSolverr stalls).

**Explicitly NOT changing** (audit-confirmed good): reqwest client reuse, MangaDex `TokenBucket`
+ backoff, ingest page-walk fan-out, transactional writes, WAL+NORMAL baseline,
`panic="unwind"` (needed by the `CatchPanic` layer).

---

## Verification matrix

```bash
# Cover: small WebP (already true)
curl -s -o /dev/null -w "size:%{size_download} type:%{content_type}\n" "https://api.komiq.cc/api/v1/manga/258/thumbnail"

# CDN caching — expect HIT on 2nd hit AFTER P0.2 Cache Rule
curl -sI "https://api.komiq.cc/api/v1/manga/258/thumbnail" | grep -i cf-cache-status

# Total home cover weight (was 40.7 MB → target ~1 MB)
curl -s -X POST https://api.komiq.cc/graphql -H 'content-type: application/json' \
  -d '{"query":"query{discovery{items{coverUrl}}}"}' \
  | python3 -c "import json,sys;d=json.load(sys.stdin);print('\n'.join(i['coverUrl'] for f in d['data']['discovery'] for i in f['items']))" \
  | sort -u | xargs -P8 -I{} curl -s -o /dev/null -w "%{size_download}\n" "{}" \
  | python3 -c "import sys;s=[int(x) for x in sys.stdin if x.strip()];print(f'{len(s)} covers, {sum(s)/1e6:.1f} MB')"

# Covers present in SSR HTML — expect >1 after B2
curl -s https://komiq.cc/ | grep -c '<img'

# Catalogue seed progress + reconcile tally (server logs)
docker logs komika-server-1 --since 30m 2>&1 | grep -iE "catalogue page|cycle done|reconcile"
```

---

## Definition of done

- Full ~113k MangaDex spine seeds without premature truncation **and survives a mid-seed restart**.
- A fresh `reconcileCatalogue` consolidates the bulk of the ~10.5k previously-skipped Suwayomi works.
- Covers small WebP **and** edge-cached (`cf-cache-status: HIT`); home cover weight ~1 MB.
- Covers appear in SSR HTML; browse back-nav restores instantly.
- Audit wins shipped: `opt-level=3`, `spawn_blocking` gaps closed, PRAGMAs + perf indices,
  dedup/ingest batching.
- `cargo test --bin komika-server` green (≥191); work committed to `main`.

## Risk register

| Risk | Mitigation |
|------|------------|
| Re-seed/re-reconcile merges are irreversible | P0.1 user gate; exact-title merges are policy-safe |
| Broken uncommitted change fails whole image build | `cargo check --all-targets` before every build |
| `database is locked` under sync+drainer+scanner overlap | OPT-3 busy_timeout 15s + WAL pragmas; already logged+retried |
| Bundling perf with seed-correctness in Build #1 obscures a seed regression | Build #1 limited to trivially-safe perf; behavioural ingest/dedup changes isolated to Build #2 |
| OPT-9 cover concurrency hammering the engine | Deferred to Build #2 (post-seed); bounded `buffer_unordered(4–6)` |
| Reader deploy ships unrelated committed changes | Expected — they're intended to ship; verify with `svelte-check` first |
