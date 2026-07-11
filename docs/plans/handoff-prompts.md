# Komika — handoff prompts to finish the remaining work

Each prompt is self-contained. Give one to a fresh agent/session. **Prepend the SHARED
CONTEXT to every prompt.** Prompts 1 and 3 are the high-value ones; run Prompt 1 first.

---

## SHARED CONTEXT (prepend to every prompt)

You're working on komiq/komika, a manga reader. Working dir `/home/ubuntu/komiq/komika`.
**This checkout IS the live production VPS — be careful.**

- **Server** `apps/server`: Rust (axum + async-graphql + SQLite/sqlx), Docker container
  `komika-server-1`, compose `deploy/docker-compose.yml`. GraphQL at
  `http://localhost:8080/graphql` (public `https://api.komiq.cc/graphql`), health `/health`.
  - Build/deploy: `cd deploy && docker compose build server && docker compose up -d server`
    (~1–2 min build; recreate = ~seconds downtime; the `/data` volume persists). Verify:
    `curl -s http://localhost:8080/health` → `ok`.
  - Sanity before building: `cd apps/server && cargo check --all-targets -j4` then
    `cargo test --bin komika-server` (**197 passing** as of this handoff; keep green). Run
    cargo from **within `apps/server`** — the session cwd drifts and bare `cargo` from the
    repo root fails to find the manifest.
  - DB `/data/komika.sqlite3` is container-owned (uid 10001), no `sqlite3` in the image, you
    (ubuntu) can't read `/data`. **Schema changes → sqlx migrations; data changes → admin
    GraphQL mutations.** Next migration number is `0043`.
  - GraphQL **introspection is OFF** in prod — use exact field names, don't rely on `__schema`.
- **Admin mutations** (`require_admin`): get a fresh token from the user (their admin
  browser `localStorage['komika-admin-token']`), send `Authorization: Bearer <token>`. Don't
  read it from process memory (a classifier blocks that). Use `http://localhost:8080/graphql`
  (local — never the public api host for admin ops).
- **Reader** `apps/reader`: SvelteKit + `adapter-cloudflare`, deployed as a Cloudflare Worker
  on `komiq.cc` — separate from the server image. `pnpm` is broken here; typecheck via
  `cd apps/reader && ./node_modules/.bin/svelte-check --tsconfig ./tsconfig.json` (NOTE: in
  some sandboxes svelte-check can't load the vite config and errors identically on every
  `.svelte` file — that's an env limitation, not your change; verify by grepping the output
  for errors that are NOT "No Svelte configuration found in vite config"). Deploy:
  `cd apps/reader && pnpm build && pnpm dlx wrangler deploy` — **let the user run the deploy.**
- **Suwayomi + flaresolverr** are separate containers — don't touch. Suwayomi is currently
  flaky (FlareSolverr stalls); live source browses/fetches can be slow or empty.
- **Current deployed state** (Build #2, commit `9fb3e33`): release `opt-level=3`,
  `spawn_blocking` on image/argon2 CPU paths, SQLite pragmas + perf indices (migration 0042),
  catalogue seed hardening (truncation fix + `resyncCatalogue` + transient-400 retry + lock
  retry). **Catalogue is seeded to 106,346 MangaDex works.** A chapter firehose re-seed is
  running silently in the background (no per-page logs; resumable across restarts).
- **Committed to `main` but NOT deployed yet** (they ship on the next `docker compose build`):
  - `379627e` — lazy-cache MangaDex covers on our origin (`serve_cover` fetches+caches on miss,
    302→CDN fallback; uncached cover URLs now point at `/covers/{id}.webp`).
  - `1189436` — `markSourceNsfw(sourceId, isNsfw)` admin mutation (bulk per-source NSFW).
  - `b57c5ea` — NSFW flagging fix (broadened `genre_is_nsfw` + OR-in `source_extension.is_nsfw`
    at ingest) + `rederiveSuwayomiNsfw` admin backfill mutation.
  - `e461c63` + earlier — reader B2 (SSR covers) + committed browse/chapter-sort/Cancelled
    changes, awaiting a `wrangler` deploy.
- Commit to `main` as you go; end commit messages with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. Don't `git push`
  unless asked (deploy is docker build / wrangler, not git pull). Merges in `reconcileCatalogue`
  are IRREVERSIBLE — confirm with the user before running it.

---

## PROMPT 1 — Build #2b: deploy the committed server fixes, backfill NSFW, reconcile

Ship the committed-but-undeployed server work (`379627e`, `1189436`, `b57c5ea`) and run the
admin follow-ups. This is the highest priority — it makes the NSFW leak fix and the lazy cover
cache live.

1. `cd apps/server && cargo check --all-targets -j4 && cargo test --bin komika-server` — expect
   197 passing. If red, STOP and fix before building.
2. `cd deploy && docker compose build server && docker compose up -d server`; wait for
   `curl -s http://localhost:8080/health` → `ok`. (This restarts the container; the background
   chapter re-seed resumes from its checkpoint — expected.) Skim startup logs for errors:
   `docker logs komika-server-1 --since 90s 2>&1 | grep -iE "error|panic|migrat"`.
3. Get a fresh admin token from the user.
4. **Backfill NSFW** — run `mutation { rederiveSuwayomiNsfw }`. It returns the count of works
   flipped 0→1. Then read the per-source breakdown:
   `docker logs komika-server-1 --since 5m 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep "rederiveSuwayomiNsfw"`.
   Report to the user which `source_id`s flipped and how many works each — they should be
   adult sources. We ingested ~8 Suwayomi sources (omegascans `1534451209269193504` is already
   handled via manual marking); this catches the others.
5. **Verify the leak is closed**: query the anonymous home feed and confirm no NSFW leaks:
   `curl -s -X POST http://localhost:8080/graphql -H 'content-type: application/json' -d '{"query":"query{ discovery{ title items{ id title isNsfw } } }"}'`
   — every returned item must have `isNsfw:false` (anonymous = NSFW off). Any `isNsfw:true`
   in the response is still a filter bug; investigate `filter_nsfw`/`canonical_is_nsfw_batch`.
6. **Verify lazy MangaDex covers**: pick a catalogued work id (`w_…`) whose cover isn't cached,
   request `curl -sI http://localhost:8080/covers/<workId>.webp` — expect `200 image/webp`
   (materialized) or a `302` to `uploads.mangadex.org` (fallback). A second request should be
   a fast `200 image/webp`.
7. **Re-run reconcile (IRREVERSIBLE — confirm with the user first)**: `mutation { reconcileCatalogue }`,
   then watch `docker logs komika-server-1 --since 30m 2>&1 | grep -iE "reconcile"` for
   `reconcile: complete merged=X queued=Y skipped=Z`. Expect `merged` MUCH larger than the
   prior run's 838 (the spine is now full at ~106k, so the ~10.5k previously-skipped Suwayomi
   works get re-checked). Report the tally.

Gotchas: `docker compose build` compiles the whole tree — a broken uncommitted change fails
the build (run `cargo check` first). Expect `database is locked` warnings during overlap
(handled + retried). The re-derive uses cached genres from `suwayomi_series`; a source with no
`source_extension` row and no adult genre tags won't flip — that's correct.

---

## PROMPT 2 — Phase 3 server perf: OPT-7 dedup batch, OPT-6 idempotency reorder, OPT-9 cover crawl

Implement the deferred ingest optimizations (audit findings). Each is isolated; add a unit test
where noted; keep 197+ tests green. They ship on the next `docker compose build`.

- **OPT-7 — batch `load_match_data` in the dedup matcher.** `apps/server/src/dedup.rs` ~line
  174: `resolve_ex` calls `catalog::load_match_data(pool, wid)` per candidate in a loop, and
  each does 2 queries (work row + `work_alias` rows) → up to ~300 sequential round-trips per
  resolved item. Replace with one `SELECT ... FROM work WHERE id IN (?)` + one
  `SELECT work_id, normalized_title FROM work_alias WHERE work_id IN (?)`, grouped in memory.
  Same scoring semantics. Add a test asserting batched == per-item scoring. This directly
  speeds `reconcileCatalogue`.
- **OPT-6 — reorder the idempotency check in `ingest_source_series`.**
  `apps/server/src/graphql/mod.rs` ~line 5589: it does ~4 upstream round-trips + a cover
  download + dhash BEFORE the `"existing"` short-circuit inside `add_source_series_core_ex`.
  Call `catalog::find_source_series("suwayomi", m.source_id, source_key)` at the TOP; if
  already linked + in library, return early (skip cover fetch, dhash, and the immediate
  `scan_series`). Also remove the double `put_series` (once in `ingest_source_series`, again in
  `scanner::scan_series`). Add a test: re-enrolling an existing linked series issues no upstream
  fetch.
- **OPT-9 — parallelize + `spawn_blocking` the Suwayomi cover crawl.**
  `apps/server/src/cover.rs` ~line 330 (`crawl_uncached_covers`, second/Suwayomi pass): it's a
  serial `for` loop, unthrottled, running `process_cover` inline on the reactor — this floods
  logs and contends with the DB writer during a seed. Use
  `futures::stream::iter(...).map(...).buffer_unordered(4–6)` and wrap `process_cover` in
  `tokio::task::spawn_blocking`. Note: `futures` may need adding to `Cargo.toml`.

Optional follow-up (bigger): OPT-8 — collapse the per-series `views` (4 queries) and
`user_library` (3 queries on the same row) `ComplexObject` resolvers, or add async-graphql
DataLoaders (none exist today). See `docs/plans/optimisation-and-catalogue-plan.md` Tier 2/4.

After implementing, they deploy in the next Build (fold into Prompt 1's build if not yet run,
or a Build #2c).

---

## PROMPT 3 — Reader: B1 browse back-nav cache + deploy

Implement B1, typecheck, and get the reader deployed (it carries B2 SSR covers `e461c63` + the
earlier committed browse/chapter-sort/Cancelled changes, none of which are live yet).

- **B1 — browse search back-nav cache.**
  `apps/reader/src/routes/(app)/browse/+page.svelte` (~1200 lines) keeps search state (`rows`,
  `catalogPage`, `hasNext`, `rowsAreFederated`, `totalCount`, rail filters) as component-local
  `$state`, so series→back remounts, re-runs the whole search, and loses scroll. Add a
  **module-scoped cache** keyed by a query+filters signature: on mount, if the signature
  matches, hydrate the state and skip the initial fetch; else fetch and write back. Preserve the
  existing debounce/cancellation and the already-committed Load More / Cancelled-filter /
  format-tab changes (build ON them, don't revert). Optionally mirror rail filters
  (`status`,`minRating`,`maxRating`,`sort`) into the URL.
- Typecheck: `cd apps/reader && ./node_modules/.bin/svelte-check --tsconfig ./tsconfig.json`
  (filter out the "No Svelte configuration found in vite config" env noise — only real type
  errors matter).
- **Deploy (let the user run it):** `cd apps/reader && pnpm build && pnpm dlx wrangler deploy`.
- Verify after deploy: covers appear in SSR HTML (`curl -s https://komiq.cc/ | grep -c '<img'`
  → >1, from B2); browse → open a series → back restores instantly with scroll intact.

---

## PROMPT 4 — Cloudflare + operational cleanup (mostly user-side)

1. **`/covers/` edge cache rule** (Cloudflare dash → `komiq.cc` → Caching → Cache Rules). The
   Suwayomi-thumbnail rule doesn't cover `/covers/*.webp`, so lazily-cached MangaDex covers
   still hit the VPS on repeat. Add a rule: match
   `(http.host eq "api.komiq.cc" and starts_with(http.request.uri.path, "/covers/"))`,
   Eligible for cache, Edge TTL "Use cache-control header if present" (origin sends 1-year
   immutable). Verify `cf-cache-status: HIT` on a 2nd hit.
2. **Verify the HTML browser-cache TTL (B3).** Earlier the home HTML returned
   `cache-control: max-age=14400` (a Cloudflare *Browser Cache TTL* override, NOT in the repo);
   a later check showed `max-age=0`. Confirm it's set to **"Respect Existing Headers"** (Caching
   → Configuration → Browser Cache TTL) and audit Cache/Transform/Page Rules for any
   cache-control override, so new chapters aren't hidden behind a 4h stale HTML cache.
3. **omegascans not-catalogued series (optional).** The 275 marked were what live `sourceBrowse`
   surfaced; the source may have more we never ingested. To pull them in + flag:
   `mutation { startSourceIngest(sourceId:"1534451209269193504") { id status } }`, watch
   `sourceIngestJobs` until done, then `mutation { markSourceNsfw(sourceId:"1534451209269193504", isNsfw:true) }`.
   (Adds content — bigger action; confirm intent.)
4. **Chapter firehose re-seed decision.** `resyncCatalogue` reset the chapter cursor, so a full
   MangaDex chapter re-mirror is running (huge, silent, resumable). Decide whether to let it run
   (powers the "updates" feed) or curtail it. There's no stop mutation; a restart just resumes
   it. If you want it stopped permanently you'd need a small "mark chapters seed_done" admin
   path (not built).
5. **Optional: the ~115-work seed gap.** The resumed seed picked up at offset ~2,800, so ~115
   works from run-1's 0–2,800 range weren't revisited (~0.1%). A fresh
   `mutation { resyncCatalogue }` re-walks everything (~25 min) to close it; otherwise a future
   incremental catches stragglers. Low priority.

---

## Quick reference — verification snippets

```bash
# health
curl -s http://localhost:8080/health
# anonymous home feed must be all isNsfw:false
curl -s -X POST http://localhost:8080/graphql -H 'content-type: application/json' \
  -d '{"query":"query{ discovery{ title items{ id title isNsfw } } }"}'
# catalogue seed / reconcile / rederive in logs
docker logs komika-server-1 --since 30m 2>&1 | sed 's/\x1b\[[0-9;]*m//g' \
  | grep -iE "catalogue cycle done|reconcile|rederiveSuwayomiNsfw|chapter sweep complete"
# lazy cover
curl -sI http://localhost:8080/covers/<workId>.webp
# covers edge cache (after CF rule)
curl -sI "https://api.komiq.cc/covers/<file>.webp" | grep -i cf-cache-status
```
