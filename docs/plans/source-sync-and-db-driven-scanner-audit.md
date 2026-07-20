# Audit — Source-Sync + DB-Driven Scanner

**Target:** the uncommitted, not-yet-deployed feature described in
[`source-sync-and-db-driven-scanner.md`](./source-sync-and-db-driven-scanner.md).
**Method:** 6 parallel adversarial reviews (scanner, sync module, GraphQL/enrolment,
migrations, frontend, build/test). Read-only against the live prod checkout.
**Date:** 2026-07-22.

## Verdict

**Not production-ready.** Build is clean — `cargo fmt --check` ✓, `cargo check
--all-targets` 0 warnings, **210 tests pass / 2 ignored**, `pnpm -r check` ✓ (one
pre-existing unrelated `Cover.svelte` warning). `clippy` is **not installed** on this
toolchain, so no lint pass ran.

But there are three confirmed defects that undermine the feature's own headline
guarantees, all made worse by this being a frequently-rebuilt live VPS.

**Root cause tying the worst findings together:** a failed `scan_due` never advances
`next_scan_at`, so failing rows stay permanently due at the front of the queue.

### Top 3 must-fix
1. **Drain-loop retry-storm + no error backoff** (#1/#4) — track *successful* progress,
   break the drain on a no-progress full batch, give failed scans bounded backoff.
2. **`add_source_series` stranding** (#2) — add the `ensure_pending` fallback so a failed
   enrol-scan still leaves a scan_state row.
3. **Restart-triggered full sync + unbounded reconcile fetch** (#3/#5) — gate the immediate
   pass behind `last_synced_at`; paginate `library()`.

---

## CONFIRMED bugs (ranked)

### 1. CRITICAL — Drain loop retry-storms with no backoff on upstream outage
`apps/server/src/scanner.rs:729-745` + `:357-378` (+ swallowed errors `:370-372`)

The drain continues while `overdue == DUE_BATCH_LIMIT`, but `overdue` counts rows
**selected**, not rows successfully scanned. `scan_due` errors are swallowed in
`for_each_concurrent` and `record_scan` never runs, so the row stays due.

**Failure scenario:** ≥5000 series due (guaranteed at 120k cold-start / post-downtime) +
Suwayomi/FlareSolverr down → every iteration re-selects the same 5000, all fail,
`overdue == 5000`, drain never breaks. Back-to-back 5000-row queries + upstream fetches
with **zero inter-batch delay, indefinitely** — a self-DoS against a dead upstream; only
shutdown exits.

**Fix:** count scans that actually advanced `next_scan_at`; break the drain when a full
batch makes **no progress**; add inter-batch sleep + backoff on repeated all-fail batches.

### 2. HIGH — `add_source_series` strands a single-add if the enrol-time scan fails
`apps/server/src/graphql/mod.rs:5342-5356`, `apps/server/src/scanner.rs:484-491`

`scan_series` fetches chapters (`?` early-return) *before* the only code that INSERTs a
`series_scan_state` row. The scan is best-effort here, and unlike `federated_ingest` there
is **no `ensure_pending` fallback**. A routine chapters-fetch hiccup leaves the series
enrolled + `inLibrary=true` but **with no scan_state row** — and the DB-driven scanner
selects only from that table, so it's invisible. The in-code comment claiming "the
scheduler retries next pass" is **false** under the new model. Only the once-daily
reconcile backfill recovers it (~24h invisible). Directly re-opens spec defect #1.

**Fix:** call `scanner::ensure_pending(...)` right after enrol, as `federated_ingest` does
(idempotent `ON CONFLICT DO NOTHING`).

### 3. HIGH — Every server restart triggers a full, unthrottled source-sync pass
`apps/server/src/sync.rs:75-98` + `apps/server/src/main.rs:965`

`tokio::time::interval` fires immediately at t=0 and `run_loop` has no initial-delay /
`last_synced_at` gate. Every boot runs `sync_all` → full `library()` fetch + LATEST
re-walk of every subscribed source, ignoring the 86400s interval. Rebuilds/restarts are
routine on this VPS — 10 deploys/day = 10 full library fetches + 10 full LATEST sweeps
hammering upstream.

**Fix:** gate the immediate pass on persisted `last_synced_at > interval`, or skip the
first tick.

### 4. HIGH — Permanent starvation of healthy series behind ≥5000 always-failing rows
`apps/server/src/scanner.rs:385-399` + `:423-427`

Numeric ids whose upstream manga was deleted **error** every scan (only *non-numeric* ids
get parked; a 404'd numeric id does not). They keep `next_scan_at` NULL/past forever and
sort to the front every tick. Once ≥5000 dead rows exist (~4% of 120k), they fill every
batch and healthy due series past position 5000 are **never reached** — and the batch is
never short, so the drain never settles (compounds #1).

**Fix:** on scan failure, push `next_scan_at` out with bounded error-backoff (a
`consecutive_failures` column, eventually park dead ids).

### 5. MEDIUM — `reconcile_library` reintroduces the unbounded full-library fetch; runs even with zero subscriptions
`apps/server/src/sync.rs:107`, `:147` → `apps/server/src/suwayomi.rs:566`

`library()` issues `mangas(condition:{inLibrary:true})` **unpaginated**, collecting all
120k series into a Vec/HashSet — the exact O(library) anti-pattern §0.3 removed from the
scanner. It runs *before* the subscription check, so the "no-op when nothing subscribed"
claim in the spec and the `main.rs:965` comment is **false**.

**Fix:** paginate `library()`; correct the "no-op" comments; consider splitting reconcile
into its own honestly-labelled job.

### 6. MEDIUM — Index does NOT deliver the "O(due)" claim; idle ticks are O(catalogue)
query `apps/server/src/scanner.rs:387-389`, index
`apps/server/migrations/0045_scan_state_next_scan_index.sql:9-10`, test `scanner.rs:1517-1541`

Verified empirically (SQLite 3.45.1): `WHERE next_scan_at IS NULL OR next_scan_at <= ?`
plans as a **full unbounded `SCAN … USING INDEX`**, not a bounded `SEARCH`. The `IS NULL
OR` prevents `<=` from terminating the range. Measured: 10 due rows among 100k
future-dated → predicate evaluated **200,010 times**. The early-stop never fires and every
future-dated row *is* visited — the opposite of the spec's "idle ticks are one cheap
indexed read / future-dated rows never visited." The regression test only checks the index
name + absence of `TEMP B-TREE`, which a full scan satisfies — **false confidence**.
(Real-world pain is low: a 120k single-column index walk once/hour is a few ms. The defect
is the overstated guarantee + untested claim.)

**Fix:** store "due now" as a sentinel timestamp instead of NULL → `WHERE next_scan_at <=
?` plans as a bounded `SEARCH`. Or at minimum assert a bounded SEARCH in the test.

### 7. MEDIUM — `update_series_admin` unconditionally nulls `next_scan_at`, flipping series into accelerated "awaiting" poll + queue-jumping
`apps/server/src/graphql/mod.rs:4729-4732` ↔ `apps/server/src/scanner.rs:593-594`

The reset runs on *every* admin edit regardless of field. NULL reads as due-now, so the
next `record_scan` computes `due_now && !new_found → awaiting`, switching the series to the
accelerated `poll_every_minutes` cadence — defeating the exact guard `record_scan`'s own
comment describes. Asymmetry: `triggerScan` and unpause call `scan_series` directly
*without* nulling, preserving the guard; only `updateSeriesAdmin` breaks it. NULLs also
sort ahead of genuinely-due rows, so a burst of edits jumps the queue.

**Fix:** write `next_scan_at = now` (or `min(next_scan_at, now)`), or only reset when
pause/interval fields actually changed.

### 8. MEDIUM — Health snapshot can't reveal a stuck/looping scanner
`apps/server/src/scanner.rs:734-740` → `apps/server/src/graphql/mod.rs:2685-2705`

`ScanHealth` = `{library_size, overdue_count, last_tick_at}`. During the #1 infinite-fail
drain, `last_tick_at` refreshes every iteration (looks alive) and there's no success/error
count or `last_success_at`. Health lies by omission.

**Fix:** record per-tick scanned/failed counts + `last_success_at`; surface
consecutive-failure state.

### 9. MEDIUM — 14-day park is a periodic thundering herd, not the "~2/min" the spec claims
`apps/server/src/scanner.rs:444-458`

On cold start the whole completed/paused set (~48k) is parked at nearly the same `now +
14d`, so they all re-come-due in the same window 14 days later and re-cluster —
self-perpetuating. The spec's "~3.4k/day ≈ 2/min" is the average, not the distribution;
reality is a multi-batch spike every 14 days that also re-arms #1 if upstream is flaky.

**Fix:** jitter the park (`14d ± random`).

### 10. MEDIUM — N+1 DB lookups in the LATEST walk
`apps/server/src/sync.rs:240-251` → `apps/server/src/catalog/mod.rs:1418`

Per manga per page, a separate `find_source_series_by_key`. 20-50 serial round-trips ×
pages × sources × extensions.

**Fix:** one `WHERE source_key IN (...)` per page, diff in memory.

### LOW-severity confirmed
- **Backfill has no `ON CONFLICT`** `apps/server/src/catalog/mod.rs:1663-1673`: a
  concurrent `ensure_pending` insert causes a UNIQUE violation that **rolls back the entire
  multi-row backfill** (logged + swallowed → silently no-ops that pass). Add `ON
  CONFLICT(series_id) DO NOTHING`.
- **`awaiting_since` never cleared on park** `apps/server/src/scanner.rs:447-456`: a
  completed series shows "awaiting since …" forever in the admin console. Cosmetic.
- **Shutdown not honored within a tick** `apps/server/src/scanner.rs:368-374`: one batch
  (5000 × up to 30s / conc 3) can take hours and ignores shutdown mid-flight.
- **Permanently-failing enrol retried every pass** `apps/server/src/sync.rs:253-270`: a
  never-enrollable manga reads as "new" every pass, re-attempted, and resets
  `consecutive_known=0`, pushing the walk deeper each time.
- **`pkg_name TEXT PRIMARY KEY` allows NULLs**
  `apps/server/migrations/0044_extension_subscription.sql:14` (SQLite quirk; latent,
  current writer is non-null). Add `NOT NULL`. Also 0044 lacks `IF NOT EXISTS` (asymmetric
  with 0045).
- **Orphaned subscription state on uninstall**
  `apps/admin/src/routes/sources/+page.svelte:1059-1091`: subscribe → uninstall leaves the
  `extension_subscription` row with no UI to clear it; silently resumes on reinstall.
- **`spawn_extension_sync` has no single-flight guard** `apps/server/src/sync.rs:294-299`:
  rapid re-toggle / overlap with the daily pass runs concurrent walks (idempotent, but
  redundant upstream load).
- **Stale rationale comments** (`apps/server/src/graphql/mod.rs:5328-5334`,
  `apps/server/src/sync.rs:9-13`): both justify `set_in_library` by "the scanner only
  iterates `library()`" — no longer true; will mislead maintainers into thinking library
  membership keeps a series scanned (the scan_state row does).

---

## PLAUSIBLE concerns
- **LATEST walk can stop too early** `apps/server/src/sync.rs:253-256`: Suwayomi LATEST is
  ordered by recent *update*, not recent *add*. A newly-added series with an old update
  timestamp sorts behind 2 pages of known series → the 2-consecutive-known stop skips it,
  and the early-stop (unlike the page cap) is **not logged**. Inherent to the heuristic;
  mitigate with a periodic deeper sweep.
- **Reopen auto-resume latency up to 14 days** — logic is correct (park skipped when status
  ≠ paused), but resume only happens on the next parked scan. Confirm that SLA is intended.
- **sqlx checksum immutability**: 0044/0045 are frozen after first deploy — any later
  whitespace/comment edit aborts boot with a version-mismatch. Corrections must go in 0046+.

---

## Test adequacy

Build is genuinely green (210 pass, 0 warnings, fmt ✓, frontend ✓). **clippy not
installed** — no lint pass ran. The DB query/index/upsert *primitives* are well tested (5
new tests), but the **new orchestration logic is largely untested**:

- **`sync.rs` has ZERO tests** — discovery stop-condition, page cap, `reconcile_library`,
  auto-enrol, immediate-pass all unverified.
- **`scan_due` orchestration untested** — no fake Suwayomi trait, so scan-then-park *as a
  sequence* and reopen COMPLETED→ONGOING auto-resume (the two headline behaviors) are
  verified by reading code only. `effective_status`/`is_paused` (pure, trivially testable)
  have no direct tests.
- **Drain loop untested** — a regression that idles an hour between 5k batches
  (reintroducing the "5000/hour" problem) would pass all 210 tests.
- **Admin `next_scan_at` reset untested** — the completeness-invariant safeguard the spec
  names.

### What the 5 new tests DO cover (credit where due)
- `due_query_takes_null_and_past_orders_nulls_first_and_limits` — NULL sorts first as
  due-now, overdue included, future excluded, LIMIT honored. Does **not** prove LIMIT
  early-terminates the scan (see #6).
- `due_query_is_index_backed_with_no_sort` — plan contains the index, no `TEMP B-TREE`.
  Scoped to plan shape only (see #6).
- `ensure_pending_is_due_now_and_never_clobbers` — fresh row due-now; re-run never disturbs
  a parked row.
- `park_pushes_next_scan_out_and_drops_from_due_set` — park pushes `next_scan_at` out and
  drops from due set. Isolated; does not exercise park-after-a-scan.
- `backfill_covers_only_untracked_suwayomi_series` — the strongest: three-way branch
  (untracked suwayomi → backfilled; tracked → untouched; non-suwayomi → never tracked).

---

## Verified-correct (no defect — checked, for the record)
- Completeness invariant holds via the three enrol paths + daily set-based
  `backfill_pending_scan_states` (idempotent `NOT EXISTS`, `source_type='suwayomi'` only) —
  **except** the enrol-scan-failure hole in #2.
- First-observation baseline for a COMPLETED/paused series is recorded before parking — the
  "0 chapters forever" bug is genuinely fixed.
- `park_paused` correctly overrides `record_scan`'s `next_scan_at` (park writes last); one
  row per series, no double-schedule.
- `ensure_pending` `ON CONFLICT DO NOTHING` never clobbers an existing parked schedule
  (test-covered).
- Admin unpause and `update_series_admin` both reset scheduling so a parked series isn't
  stranded (but see #7 for the over-reset).
- **Auth:** `setExtensionSubscription` is `require_admin`-gated (`graphql/mod.rs:5877`); the
  `subscribed` badge resolver is admin-gated (`:2959`). No new mutation/field un-gated.
- **`subscribed` is not N+1** — one `subscribed_extension_set` HashSet query, then
  `.contains(pkg_name)` per row. Join key `pkg_name` matches migration 0044 PK.
- **Subscription helpers idempotent** — subscribe = `ON CONFLICT(pkg_name) DO NOTHING`
  (preserves `created_at`), unsubscribe = `DELETE`.
- **Frontend Sync toggle is fully wired end-to-end**, NOT dead UI: `+page.svelte`
  `toggleSync` → `data.ts` → `GraphQLBackend.setExtensionSubscription` →
  `ops.SET_EXTENSION_SUBSCRIPTION` → schema. The `?`-optional on the Backend interface is
  harmless (both concrete impls define it; composite forwards to hosted; the one caller
  guards). `subscribed` is required in types + schema + selection set, so never undefined
  at runtime. Toggle reflects load state and maps in the server-returned value (not a blind
  optimistic flip); failures surface a row message and keep prior state.
- **Migrations:** no duplicate/gap numbering (0001…0045, each once); each runs in a
  transaction (clean rollback on partial apply); 0045 index build is sub-second on 120k and
  runs before serving traffic; no table rewrite / no FK cascade risk; ORDER BY / NULL
  ordering matches the index direction and column.
