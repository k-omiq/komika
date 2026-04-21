# Fix Prompt — Phase 5 (scanner correctness)

> **Self-contained kickoff prompt for one work session.** Implements all of Phase 5 of the Komika
> audit — the adaptive scan scheduler in `apps/server/src/scanner.rs`. Phases 1–4 are **done and merged
> into `main`**. Evidence for every finding ID is in [AUDIT_FINDINGS.md](../../AUDIT_FINDINGS.md)
> (Domain 7 — Scanner, `SC1`–`SC7`); the phase plan + checklist is
> [AUDIT_FIX_PLAN.md](../../AUDIT_FIX_PLAN.md). Repo root: `/Users/caved/dev/komika`.

Paste everything below the line into a fresh session.

---

You are implementing **Phase 5** of the Komika audit remediation at `/Users/caved/dev/komika`. Read
this whole prompt first. The work is almost entirely in **one file** (`apps/server/src/scanner.rs`)
plus one or two SQLite migrations and a couple of GraphQL touch-points. Do the items as **separate
commits, one per item** on a single phase branch. Before editing, re-verify each `file:line` against
the current code — search for the quoted code; line numbers may have drifted.

## Workflow rules (per item)
- Start from `main`: `git checkout main && git checkout -b audit-fixes/phase5-scanner` (all items on
  this one branch, one commit per item; **never commit on `main`**).
- Re-read the finding in AUDIT_FINDINGS.md before touching its fix.
- Match surrounding style. The scanner has a `#[cfg(test)]` module (`scanner.rs:348`) with pure-function
  tests (`is_overdue`, `avg_interval_hours`, `at()`/`chap()` helpers) — extend it. For anything that
  needs a DB, use the `graphql/mod.rs` test harness pattern (in-memory pool, `sqlx::migrate!`,
  `seed_user`, `exec(...)`). **Prefer writing the failing test first.**
- Verify the server after each item:
  `cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test`
- Commit locally with the ID(s) in the message (e.g. `fix(scanner): baseline first-run count [SC3]`).
  **Do not push or open a PR unless asked.**
- Tick the item's checkbox in AUDIT_FIX_PLAN.md (§ Phase 5) with a one-line note. Keep the checklist
  honest — if you defer or narrow anything, say so on the line.
- If a fix reveals an adjacent issue not in AUDIT_FINDINGS.md, note it in the checklist line rather than
  silently widening scope.

## Current landmarks (verified 2026-07-14; may drift a few lines)
`apps/server/src/scanner.rs`
- `ScanState` struct + `series_scan_state` mirror **:39-46**; `scan_state()` read **:49-59**.
- `ScanAdmin` struct **:62-67** — currently selects only `override_interval_hours`,
  `paused_override`, `status_override`. **It does NOT read `poll_every_minutes`** (that's the SC1 gap).
  `scan_admin()` query **:69-80**.
- `avg_interval_hours()` **:97-120**; `latest_number()` (computed, log-only, never persisted) **:123-128**;
  `is_overdue()` **:138-150**.
- `tick()` **:171-221** — effective-interval resolution **:200-208**, the live overdue gate **:210**
  (`next_scan_at` is **not** read here — gating is recomputed live).
- `scan_series()` **:231-312** — `new_found = count > prior.known_chapter_count` **:245**; next-interval
  recompute + `MAX_INTERVAL_HOURS` clamp (no MIN) **:248-259**; `next_scan_at` **:260-261**;
  `last_new_chapter_at` **:262-266**; the `series_scan_state` upsert **:288-309**.
- `DEFAULT_INTERVAL_HOURS = 24.0` **:32**, `MAX_INTERVAL_HOURS` **:36**. `spawn()` loop **:315-346**.

`apps/server/src/graphql/mod.rs`
- Admin struct reads `poll_every_minutes` **:333**; `map_series` surfaces it (default 30) **:378** and
  surfaces `next_scan_at` **:386**; `setSeriesAdmin` upsert writes it **:1459-1481**; `trigger_scan`
  calls `scan_series(st, &m, Utc::now())` **:1496-1501**; `scanStatus` reads `MIN(next_scan_at)` **:1076**.

`apps/server/migrations/0003_series_scan_state.sql` — the table SC4/SC1 may extend. Latest migration is
`0011`; a new one is `0012_*` (bump if you add more than one).

## Read this before starting (scope + a real caveat)
- **SC2 is out of scope — REFUTED as a defect.** The scanner intentionally covers only the Suwayomi
  library, not MangaDex canonical works (those refresh via `mangadex::spawn_recurring`, Phase 2). Do
  **not** try to "fix" the split. If you touch anything MangaDex-side, you've gone out of bounds.
- **Detection correctness hinges on an unverified assumption:** that Suwayomi's
  `fetchMangaAndChapters(fetchChapters:true)` (behind `suwayomi.chapters()`) forces a source-side
  refresh rather than returning cached rows. Phase 5 improves the *bookkeeping* on top of whatever
  Suwayomi returns; it does not fix a stale upstream. Don't claim end-to-end freshness you can't test —
  note it in the checklist if relevant.

---

## Item 5.2 — SC3: don't flag the back catalogue as "new" on first observation 🟡
*(Do this first — it's the smallest and it's inside the same `new_found` logic SC4 will touch.)*

**Finding SC3:** first-ever scan has `prior = default()` (`known_chapter_count = 0`), so any series with
≥1 chapter gets `new_found = true` and writes `last_new_chapter_at = now`. On a fresh deploy,
`bootstrap.py` seeds the library and the first tick logs the **entire library** as just-updated,
flooding the `updates` feed (ordered by `last_new_chapter_at DESC`).

**Fix:** in `scan_series`, distinguish "no prior row" (first observation) from "prior row with a count."
On first observation, record the baseline `known_chapter_count` **without** setting `last_new_chapter_at`
(leave it `NULL`). Simplest signal: have `scan_state()` / the caller tell you whether a row existed
(e.g. return `Option<ScanState>` and match on `None` = first-run) rather than collapsing to
`unwrap_or_default()` at **:238-240**. Keep steady-state behavior identical for series that already have
a row.

**Test:** first `scan_series` on a series with N chapters → row persisted with
`known_chapter_count = N` and `last_new_chapter_at IS NULL`; a subsequent scan that adds a chapter →
`last_new_chapter_at = now`.

---

## Item 5.3 — SC4: detect add+remove churn, not just count 🟡  *(needs a migration column)*

**Finding SC4:** detection is strictly `count > prior.known_chapter_count`. `latest_number()` is computed
but only logged — never persisted or compared. So: (a) if upstream removes one chapter and adds another
within an interval, count is unchanged → the new chapter is **silently missed**; (b) if upstream removes
chapters, `known_chapter_count` is overwritten downward with no signal.

**Fix:** persist and compare a stronger signal than count. Pick one and note which:
- **Max chapter number** (cheapest): migration `0012` adds `known_max_chapter REAL` to
  `series_scan_state`; set `new_found` when `count > prior_count` **OR** `latest_number(&chapters) >
  prior.known_max_chapter` (with an epsilon for float compare). This finally gives `latest_number` a job.
- **Chapter-key set** (stronger, catches same-number replacements): persist a hash/set of chapter keys
  (id or number) and diff it. More faithful to the finding but heavier — only if the max-number route
  demonstrably misses a case you care about.

Recommend the **max-chapter-number** route unless you can justify the key-set. Handle the downward
`known_chapter_count` case too (log at least a debug/info when count regresses).

**Test:** a scan where count is unchanged but a higher chapter number appears → `new_found = true`; a pure
add still works; persisted `known_max_chapter` round-trips.

---

## Item 5.4 — SC5: minimum interval clamp 🟡

**Finding SC5:** `avg_interval_hours` can return an arbitrarily small positive value (a same-day burst
gives sub-hour gaps). Resolution only guards zero/negative and clamps the **upper** bound. A burst series
(avg ~0.2h) is overdue on essentially every 300s tick → refetched every tick → needless
source/FlareSolverr load.

**Fix:** add `const MIN_INTERVAL_HOURS` (pick a sane floor, e.g. 6.0 — justify it in a comment) and clamp
the effective interval into `[MIN, MAX]` in **both** places interval is finalized: the `tick` resolution
(**:200-208**) and the `scan_series` next-interval recompute (**:254-259**). An **admin
`override_interval_hours`** is an explicit human choice — decide whether MIN applies to it (recommend:
clamp only the *inferred* avg, let a deliberate override go below MIN but still ≥ some hard floor to
protect upstreams; state your choice in the checklist).

**Test:** an avg well below MIN clamps up to MIN; a normal avg passes through; the upper clamp still
holds. Extend the existing interval tests.

---

## Item 5.1 — SC1: implement the `poll_every_minutes` overdue re-poll cadence 🟡  *(the big one)*

**Finding SC1:** `poll_every_minutes` is **dead config** — surfaced in the API (default 30), validated,
and editable in the admin console, but `scanner.rs` never reads it. After an overdue no-new-chapter scan,
`scan_series` sets `next_scan_at = now + full interval` and stamps `last_scanned_at = now`, so a series
overdue for its next chapter isn't re-checked for up to a **full interval** (a week for a weekly series).
The adaptive "tighten the cadence once a series is overdue for a new chapter" promise doesn't exist.
`next_scan_at` isn't even read for gating.

**Fix direction (from the finding):** track an **"awaiting update"** state distinct from steady-state
cadence. Once a series passes its expected cadence with no new chapter, it's "awaiting"; re-poll it at
`poll_every_minutes` (clamped) until `new_found` flips, then return to the steady avg/override cadence.

**Concretely:**
- Add `poll_every_minutes` to `ScanAdmin`'s select (**:71**) so the scanner actually reads it; clamp it
  (a floor in minutes, and don't let it exceed the steady interval).
- Model the awaiting state. Cheapest signal that needs **no** new column: a series is "awaiting" iff
  it has a prior row, is past its steady cadence, and `count`/`known_max_chapter` didn't advance on the
  last scan. If a boolean/timestamp is clearer, add it to migration `0012` (fold into SC4's migration if
  you're doing both — one `0012` with all Phase 5 columns is fine, just make each commit's migration
  self-consistent).
- When awaiting: compute `next_scan_at = last_scanned_at + poll_every_minutes` instead of `+ full
  interval`, and make the `tick` gate honor it. Today `is_overdue` keys off `effective_interval` only —
  the accelerated poll window has to feed into the same gate (either fold `poll_every_minutes` into the
  effective interval when awaiting, or gate on the persisted `next_scan_at`; if you switch to gating on
  `next_scan_at`, that also closes **SC7**, see 5.5 — note the overlap).
- When a new chapter lands, clear the awaiting state and resume steady cadence.

**Test (pure-function where possible):** given a prior row past cadence with no new chapter, the next
scan schedules at `poll_every_minutes`, not the full interval; once a new chapter is detected, the next
schedule reverts to the steady interval. Keep the accelerated cadence clamped (a 1-minute
`poll_every_minutes` must not collapse to per-tick refetch below the tick cadence).

---

## Item 5.5 — SC6 / SC7: scan-state transaction + `next_scan_at`/gating agreement ⚪
*(low severity — one commit; SC7 may already be handled if SC1 switched gating to `next_scan_at`)*

**SC6 (race):** `trigger_scan` (`mod.rs:1496-1501`) and `tick` (`scanner.rs:215`) both call `scan_series`
= read `prior` → fetch → upsert, non-transactional last-writer-wins. An admin force-scan overlapping a
tick can double-count or clobber `known_chapter_count`. → Wrap the read-modify-write in `scan_series` in
a single transaction (`BEGIN … COMMIT` / sqlx `pool.begin()`), reading `prior` and writing the upsert in
the same tx so concurrent scans don't interleave. Values already converge next scan; this removes the
intra-overlap clobber.

**SC7 (display vs gating):** `tick` recomputes overdue live off `effective_interval`; persisted
`next_scan_at` is only read for display (`map_series:386`, `scanStatus MIN`:1076). If an admin changes
the override between scans, the console's "next due" won't match actual gating. → Make them agree: either
gate on the persisted `next_scan_at`, or recompute `next_scan_at` from the current override at display
time. **If item 5.1 already moved gating onto `next_scan_at`, SC7 is largely closed — just confirm and
note it.** Cosmetic; don't over-engineer.

**Test:** SC6 — a transaction test is awkward against SQLite in-memory; at minimum assert `scan_series` is
idempotent under a repeated call (no double-count) and that the upsert reads+writes in one tx (a
focused unit test or a documented reasoning note is acceptable given the low severity — say which).

---

## Definition of done
- SC3: first observation records the baseline count without flagging the back catalogue as new.
- SC4: detection compares max chapter number (or key-set), not just count; churn/regression no longer
  silently missed; new column persisted via migration.
- SC5: inferred interval clamped into `[MIN, MAX]`; burst series no longer refetched every tick.
- SC1: `poll_every_minutes` is live — overdue-awaiting series re-poll at the (clamped) poll cadence and
  revert to steady cadence once a chapter lands. No more dead config.
- SC6/SC7: scan-state read-modify-write is transactional; `next_scan_at` and gating agree.
- `cargo test` green (new tests added per item); `cargo clippy -- -D warnings` clean; `cargo fmt --check`
  clean. Note the final test count.
- Five checklist lines in AUDIT_FIX_PLAN.md (§ Phase 5) ticked with notes. **Phase 5 fully closed.**

## Out of scope
- **SC2** (Suwayomi-only vs MangaDex canonical scanning) — refuted as a defect; intended design.
- Forcing/verifying Suwayomi's source-side refresh semantics (`fetchChapters:true`) — an upstream
  black box; Phase 5 fixes the bookkeeping, not upstream staleness.
- Any reader/admin frontend change beyond what's needed to keep the GraphQL contract honest (the
  `poll_every_minutes` field already exists end-to-end; you're wiring the server to honor it, not adding
  UI).
- Phase 6+ (canonical reader path, social/admin, deploy/ops).
