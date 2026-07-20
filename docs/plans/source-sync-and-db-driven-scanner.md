# Komika — Extension Source-Sync + DB-Driven Scanner

**Scope:** make Suwayomi-extension series reliably (a) get **discovered** when new ones
appear on a source, (b) stay **updated** for chapters, and (c) scale to a **120k+ series**
library without the per-tick cost growing with catalogue size.

**Working dir:** `/home/ubuntu/komiq/komika` — this checkout IS the live production VPS.

**Status:** **implemented** (server + shared `@komika/api` + admin). `cargo test` green
(210 tests). Not yet built/deployed — see *Deploy* below.

---

## 0. Problem

Three defects, all confirmed in code:

1. **Single-added series never updated.** `addSourceSeries` scanned once at enrol but
   never called `set_in_library`; the scanner only ever looked at `suwayomi.library()`,
   so single-adds were invisible to it forever → never reached the `updates` feed.
2. **New series on a source were never discovered.** Ingest was a one-time POPULAR
   page-walk; nothing revisited a source, so later-added series needed a manual add.
3. **The scanner didn't scale.** Every tick it fetched the **entire** Suwayomi library
   (`mangas(condition:{inLibrary:true})`, unpaginated) and did ~2 serial DB reads per
   series to filter due-ness — O(library) upstream **and** O(library) DB per tick,
   regardless of how little was actually due. Painful at 120k series.

Suwayomi reality that shapes the design: the source **listing** (`fetchSourceManga`
LATEST/POPULAR) returns manga metadata + a chapter *count*, but **not** the chapter list.
So chapter-update detection is fundamentally per-series; discovery and updates are
separate concerns.

---

## 1. Design (three layers)

### A. Enrolment fix + library reconcile
- `addSourceSeries` now calls `set_in_library(mid, true)` before its scan-on-enrol, like
  the bulk/federated paths — [graphql/mod.rs `add_source_series`](../../apps/server/src/graphql/mod.rs).
- Daily **reconcile** (in `sync.rs`) re-asserts `inLibrary=true` for any enrolled Suwayomi
  series not currently in the library, healing historical single-adds and drift. Cheap in
  steady state (only touches the missing set).

### B. Extension subscriptions + source discovery
- New table `extension_subscription` (pkg_name-keyed) + `setExtensionSubscription` admin
  mutation + a **Sync on/off** toggle per installed extension in the admin Sources page
  (wired through shared `@komika/api`). A `subscribed` field is added to the `extensions`
  listing.
- The **source-sync** scheduler (`sync.rs`) walks each subscribed extension's sources'
  **LATEST** listing, auto-enrolling series we don't have via the existing
  `ingest_source_series` path. Stops after 2 consecutive all-known pages or the
  `SOURCE_SYNC_MAX_PAGES` cap (logged). Enabling a subscription fires one immediate pass.

### C. DB-driven scanner (the scaling fix)
The scanner no longer fetches the library per tick. It **selects due work from the DB**:

```sql
SELECT series_id FROM series_scan_state
WHERE next_scan_at IS NULL OR next_scan_at <= ?
ORDER BY next_scan_at ASC                              -- SQLite sorts NULL below all
LIMIT ?                                                --   values → never-scanned first
```
Backed by a new index (`0045`, on `next_scan_at`), tick cost scales with the **due** set,
not catalogue size. `ORDER BY next_scan_at` (not `… IS NOT NULL, …`) lets the index
supply the order — verified by a regression test asserting the plan is
`SCAN … USING INDEX idx_scan_state_next_scan` with **no `USE TEMP B-TREE`**. Due rows sit
at the front of the index (NULLs, then times ≤ now), so `LIMIT` stops the scan early;
future-dated rows are never visited. Idle ticks are ~one cheap indexed read.

**Per due series (`scan_due`):** one combined upstream fetch — `series_and_chapters` (new,
`fetchMangaAndChapters` with both flags → fresh status + chapters in a single call,
falling back to two calls on an older engine) — then `record_scan` **unconditionally**,
even for a paused series. Scanning first is deliberate and fixes two things a naive
"never fetch paused" would break:
- it **baselines a never-observed series**, so even a COMPLETED/HIATUS series gets a real
  chapter count (otherwise a series enrolled already-completed shows 0 chapters forever —
  the bug the old paused-baseline-scan guarded against), and
- it **refreshes status**, so an upstream *reopen* (COMPLETED → ONGOING) auto-resumes
  scanning — parity with the old live-`library()` sweep.

If the series is **still paused afterwards**, it's **parked** (`park_paused`) at
`PAUSED_PARK_HOURS` (14d), *overriding* the steady cadence `record_scan` just set, so it
drops out of the frequent due-set. Net cost of a steady paused series is thus **one fetch
per ~14 days** (also catching the rare late chapter on a "completed" series).

**Cold-start / backlog drain:** a tick processes at most `DUE_BATCH_LIMIT` (5000). When a
tick comes back full, the loop **drains immediately** (next batch, no interval wait) and
only settles back to the hourly cadence once a batch returns short — so a 120k cold start
or post-downtime backlog clears in one continuous pass (paced by `SCAN_CONCURRENCY`)
rather than 5000/hour. Shutdown is checked between batches.

**Completeness invariant:** every enrolled series must have a `series_scan_state` row for
the due-query to find it. Guaranteed by: scan-on-enrol (single/bulk/sync adds create a
full row); `ensure_pending` for federated search (which intentionally doesn't scan-on-enrol
— inserts a "due now" row cheaply); and the daily reconcile's set-based
`backfill_pending_scan_states` for anything pre-existing. Admin overrides
(`setSeriesPaused` unpause, `updateSeriesAdmin`) reset `next_scan_at` so a parked series
can't be stranded by a cleared/loosened override.

`record_source_extensions` moved off the scan tick into the daily reconcile (extension
coordinates change rarely).

---

## 2. Cadence / tunables (config.rs, all env-overridable)

| Concern | Env | Default | Note |
|---|---|---|---|
| Scan tick (due-evaluation) | `SCAN_TICK_SECONDS` | **3600** (1h) | was 300s; per-series cadence still adaptive via `next_scan_at` |
| Source-sync interval | `SOURCE_SYNC_INTERVAL_SECONDS` | **86400** (1d) | discovery + reconcile; no-op when nothing subscribed |
| LATEST pages per source | `SOURCE_SYNC_MAX_PAGES` | 10 | bounds a capped walk (logged) |

In-code constants (scanner.rs): `DUE_BATCH_LIMIT = 5000` (per-batch cap; a full batch
triggers immediate continued draining, oldest-due first), `PAUSED_PARK_HOURS = 24*14`,
`SCAN_CONCURRENCY = 3`.

**Tradeoff:** with a 1h tick, the "awaiting an overdue chapter" accelerated re-poll can't
fire faster than hourly (its 15-min floor is below the tick). Acceptable given the compute
priority; lower `SCAN_TICK_SECONDS` if faster overdue polling is wanted (it's cheap now).

**Behaviour change:** paused/completed series are now genuinely re-scanned ~every 14 days
(a real upstream fetch, then re-parked) instead of never — self-healing: it catches an
upstream reopen and the rare late chapter on a "completed" series, and guarantees a
chapter-count baseline even for a series enrolled already-completed. Load is negligible: a
paused series costs one fetch per 14d (e.g. ~40% of 120k completed ⇒ ~3.4k/day ≈ 2/min,
far under the `SCAN_CONCURRENCY`-paced budget), versus the old design's full-library
metadata fetch *every* tick.

---

## 3. Files changed

**Migrations:** `0044_extension_subscription.sql`, `0045_scan_state_next_scan_index.sql`.

**Server (Rust):**
- `graphql/mod.rs` — `set_in_library` in `add_source_series`; `setExtensionSubscription`
  mutation; `subscribed` on the extensions listing; `ensure_pending` in `federated_ingest`;
  `next_scan_at` reset in `update_series_admin`.
- `graphql/types.rs` — `subscribed` on `ExtensionInfo`.
- `scanner.rs` — DB-driven `tick`; `due_series_ids`, `scan_state_count`, `scan_due`,
  `park_paused`, `ensure_pending`, `persist_scan` (shared by `scan_series`/`scan_due`);
  `record_source_extensions` → `pub(crate)`; `scan_state` gated `#[cfg(test)]`; drain-loop
  in `run_loop`; 5 new tests (due-selection, ensure_pending, park, backfill, index plan).
- `suwayomi.rs` — `series_and_chapters` (combined fetch, with fallback).
- `catalog/mod.rs` — subscription helpers; `suwayomi_source_keys`;
  `backfill_pending_scan_states`.
- `sync.rs` — new module: scheduler, `sync_all`/`sync_extension`/`sync_source_latest`,
  `reconcile_library` (+ backfill + `record_source_extensions`), `spawn_extension_sync`.
- `config.rs`, `main.rs` — new config + `sync::spawn`.

**Shared `@komika/api` + `@komika/types`:** `ExtensionInfo.subscribed`,
`setExtensionSubscription` (schema, operations, backend interface + graphql/composite impls).

**Admin (`apps/admin`):** Sync toggle in `routes/sources/+page.svelte`;
`setExtensionSubscription` in `lib/data.ts`.

---

## 4. Deploy

- **Server rebuild restarts + interrupts in-flight ingest** — land all server changes in
  one build. Migrations `0044`/`0045` run at startup (`db::init`). On first boot after
  deploy, the source-sync's immediate first pass reconciles the library and backfills
  scan-state rows; the scanner then drains due work over subsequent ticks (cold-start
  backlog is bounded by `DUE_BATCH_LIMIT` per tick, oldest-due first).
- **Admin frontend deploys separately** (`apps/admin` + `packages/*`) — that's where the
  Sync toggle lives.
- **Tests:** `cd apps/server && cargo test` (210) + `pnpm -r check` (clean; one pre-existing
  reader `Cover.svelte` warning, unrelated). clippy unavailable on this toolchain.

## 5. Verification
1. **Enrolment fix:** `addSourceSeries` → series appears in `suwayomi.library()` and gets a
   `series_scan_state` row; a later tick re-scans it.
2. **Discovery:** subscribe an extension → a LATEST-listed series we lack gets enrolled on a
   pass (new `source_series` + scan-state row).
3. **Scaling:** confirm a tick issues the indexed due-query (not a full `library()` fetch)
   and idle ticks are cheap. Locked by test `due_query_is_index_backed_with_no_sort`
   (`EXPLAIN QUERY PLAN` = `SCAN … USING INDEX idx_scan_state_next_scan`, no temp b-tree).
4. **Pause/park:** a COMPLETED series is scanned once (baselines its chapter count), then
   parked (~14d `next_scan_at`); admin unpause / `updateSeriesAdmin` resets `next_scan_at`
   so it re-scans next tick. A series that reopens upstream auto-resumes on its next
   (parked) scan.
5. **Cold-start drain:** with a large backlog (all due), confirm the scheduler drains in a
   continuous pass (batches of `DUE_BATCH_LIMIT`) instead of one batch per hour.
6. **Updates feed:** an in-library series that gains a chapter surfaces in `updates`.
