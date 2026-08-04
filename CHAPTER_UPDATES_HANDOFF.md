# Handoff prompt — Komika chapter-updating overhaul, phase E/F tail

Copy everything below the line into a fresh session.

*(Supersedes the original handoff, which was written before any of A–E4 existed and described the
whole overhaul as unimplemented. That version is obsolete — do not follow it.)*

---

You are picking up an overhaul that is **~75% shipped**. The design is done and largely validated by
production measurement. **Do not redesign it. Do not re-measure what §8b/§8c/§8d already measured** —
except for the one baseline explicitly marked invalid below, which you MUST re-measure.

## Step 0 — read before touching anything

1. **`CHAPTER_UPDATES_PLAN.md`** (repo root, ~1,760 lines) — the source of truth. Read in this order:
   * **§1a** — owner requirements, verbatim. Settled; do not re-litigate.
   * **§7z** — "Deferred and carried-over work", especially the table **"Still not started"**. This is
     your work queue.
   * **§8, §8a, §8b, §8c, §8d** — the running correction log. **Twenty-plus plausible conclusions have
     been measured and disproved**, several by the agents who wrote them. Re-deriving them wastes
     hours. §8c and §8d are production measurements taken after the last two deploys.
   * **§7 Phase E, E3, E5** and **§7 Phase F** — your actual targets.
   * **§9** pre-checks and risks, **§10** the metrics table.
2. `CATALOGUE.md` (the work/source_series/chapter model) and `PRODUCTION.md`.
3. The auto-memory index at
   `/home/ubuntu/.claude/projects/-home-ubuntu-komiq-komika/memory/MEMORY.md` — especially
   `concurrent-sessions-share-this-tree`, `never-scan-prod-db-directly`, `host-toolchain-quirks`,
   `deploy-topology`, `updates-feed-root-causes` (read its STATUS block first — most of it is fixed).

## Where the work actually stands (2026-07-31)

| Unit | State |
|---|---|
| A1–A4 (chapter-number contract, oneshot bucket) | **shipped & verified live** |
| B (unified chapter spine) | **shipped & verified live** — `chapter` holds 1,442,150 rows across both sources |
| C1/C2/C3 (release ledger, feed projection, drift reconciler) | **shipped & verified live** — 1,000,753 events, 0 future-dated |
| D (15-min chapter cycle + F9) | **shipped & verified live** |
| E2 (LATEST reopen trigger), E4 (scan-failure honesty) | **shipped & verified live** |
| **A1b, A5, A6, the Phase B query switchover, C1b, the `packages/` fragment fix** | **written, green, NOT COMMITTED, NOT DEPLOYED** |
| **E5, E, E3, F** | **not started — your job** |

Production is at **migration 0092**. Server: **436 tests pass**, `cargo fmt --check` clean, **0
compiler warnings**. Reader: **416 files / 0 errors / 1 warning** (the 1 is pre-existing,
`Cover.svelte:26`).

## Your task, in order

### 1. RE-MEASURE the E5 baseline. It is currently invalid. [do this first]

§7 Phase E5 rests on: **20,352 scans/day, 220 detections, 1.081% hit rate, 93 upstream fetches per
new chapter.** Every one of those was measured **before E4 shipped** — i.e. while
`SuwayomiClient::chapters` silently fell back to a local-cache read on upstream failure and
`record_scan` counted that as a **success**. Failed scans sat in the denominator as
*productive-but-empty*.

Honest data now exists. Measured 2026-07-31, ~9 h after E4 went live: **205 series carry
`consecutive_failures > 0`** (it was **0 library-wide** before — that zero was the tell §4.12 was
written around) and **1 `source_scan_outage`** row.

Re-derive the hit rate, the fetches-per-detection figure and the daily scan count from post-E4 logs
and `series_scan_state`. **The conclusion (polling is the wrong model) is very unlikely to reverse —
but the arithmetic E5's economics rest on will move, and E5 is the gate on everything after it.**
Append the new numbers to §8 as a new subsection; do not overwrite the old ones.

### 2. Evaluate E5, then fork

E5 is a **gate, not a phase**. Its own section says: *"if E5 validates, much of Phase E is never
needed."* After the re-measurement, decide and **tell the owner before building**:
* E5 validates → build E5; Phase E shrinks to a slow safety-net cadence + jitter + the admin override.
* E5 does not validate → build Phase E's tier engine as specified in §7 Phase E.

### 3. Then E3 and/or F

* **E3 — per-source scanners.** Independent of everything. The win is **isolation, not throughput**:
  one stalling source can occupy all three `SCAN_CONCURRENCY` slots and starve the rest. Includes the
  owner's explicit **auto-spawn-when-a-new-source-is-added** requirement.
* **F — `all.mangadex` retirement.** Depended on C, which has shipped, so it is **unblocked**.
  **Destructive** (10,422 `source_series` rows). §9 open pre-check 2 — *are the 463 anchorless
  MangaDex UUIDs still resolvable upstream?* — is **still unanswered and gates step 2**. Answer it
  before writing any deletion code.

## Hard constraints — violating any of these ships a bug

1. **Never read the production DB directly.** Snapshot with SQLite's `.backup()` first (command in
   §10). A long-lived reader pins the WAL and has locked the live app before. A `Bash` timeout does
   **not** kill the child — verify with `ps` and kill orphans.
2. **`SCAN_CONCURRENCY` stays at 3.** Owner decision, after reviewing capacity data. Utilisation is
   7–37%; over-concurrency causes upstream timeouts that cascade into `record_scan_failure` backoff,
   *de-scheduling healthy series*. Bans on small scanlator sites are the worst outcome.
3. **Never set jitter to zero on any tier.** `scanner.rs:164-167` records that without it a
   self-sustaining cohort of ~745 series arrived every 35 minutes, drove a 43% duty cycle and
   **154 GB of Suwayomi egress**. Jitter is load-bearing, not cosmetic.
4. **Do NOT implement "completed = no scans" naively.** The three "none" rows in Phase E's table are
   only safe because of E2, which has shipped — but E2's **60-day backstop park has NOT**. It is
   coupled to `MAX_INTERVAL_HOURS` and must sit *below* `ABSURD_HORIZON_HOURS` (16 days) or
   `reclaim_absurd_schedules` drags every newly-parked series straight back (§8a). That constant move
   belongs to Phase E.
5. **`round(number * 100)` is THE chapter key**, implemented once in
   `chapter_label::ChapterLabel::key()`. Admin chapter-hiding matches on it. Unnumbered chapters use
   the `x:<external_id>` namespace.
6. **Phase F is destructive.** Back up first; never delete an `all.mangadex` `source_series` row until
   that work is *proven* to have a direct MangaDex anchor.
7. **A `CAST` on a COLUMN is not sargable**, and a work-list query and its writer **must share one
   predicate**. Both mistakes were made and caught during B/C — see §8b. The second caused an infinite
   background loop.
8. **This repo IS the live production VPS**, and **other Claude sessions edit it concurrently.** Run
   `git status --short apps/server` and check file mtimes before building. A `docker build` snapshots
   whatever is on disk at COPY time — not what you tested, not any commit.

## Uncommitted work you are building on

Nothing in this overhaul is committed. `git status` shows a large working tree; that is normal here,
not corruption. **A1b, A5, A6, the Phase B switchover, C1b and the `packages/` fix are written and
green but undeployed** — if you build a server image, A1b ships with it, so read what it does first
(`mangadex::backfill_chapter_external_urls`, ~8,779 batches against MangaDex, paced, cursorless,
gated behind `CATALOGUE_SYNC`).

**Deploying the reader ships other sessions' in-flight UI** (`MergeDialog.svelte`, an about page,
support/report routes, Header/Icon changes). Do not `wrangler deploy` without asking the owner.

## Pending owner decisions — ask, do not assume

* **The F3 gate.** `AND sss.last_new_chapter_at IS NOT NULL` still locks **9,851 of 11,797** Suwayomi
  series with chapters out of `/updates`. Removing it is a product-visible change adding ~9,851 cards
  and is in no phase's exit criteria. It must be applied to **both** the rebuild and
  `scanner::upsert_feed_series_update` together, or
  `incremental_write_converges_with_the_periodic_rebuild` will catch the asymmetry. §7z has the detail.
* Browse's `12 ch · Ch. 151` (owner chose "show both") — needs a server column chain, not a reader edit.
* Per-chapter dates on `AggregatedChapter`; the home hero's fourth A6 site (`(app)/+page.svelte:233`,
  blocked on another session's ~541 uncommitted lines).
* The three open questions in §7z (oneshots in `/updates`; 216 `content_rating IS NULL` works;
  which source's chapter title wins).

## Environment and ops

* Node ≥ 22.13 — `PATH="$HOME/.local/node/bin:$PATH"`. System node is 18 and will fail.
* **`pnpm` is broken on this ARM64 host** — use `apps/<app>/node_modules/.bin/…` directly.
* **No `sqlite3` CLI** — use `sudo python3` with the `sqlite3` module.
* Prod DB: `/var/lib/docker/volumes/komika_server-data/_data/komika.sqlite3`. Applied through **0092**.
  Migration numbers are **not contiguous** (other sessions take numbers) — pick a free one.
* Server tests: `cd apps/server && cargo test --bin komika-server` (there is no lib target).
* Deploy: back up the DB, tag the image (`komika-server:rollback-YYYYMMDD`), then
  `sudo docker build -f deploy/server.Dockerfile -t komika-server:latest .` from the repo root and
  `cd deploy && sudo docker compose up -d --no-build server`. **~13 min release build.**
* `sqlx::migrate!` **embeds migrations into the binary at compile time** — there are no `.sql` files in
  the runtime image. Verify what shipped by grepping the extracted binary
  (`docker create` + `docker cp` + `grep -a`; **without `-a`, grep silently reports 0 on a binary**).
  Grep for the migration's *SQL text or description*, not its filename — filenames are not embedded.
* Rollback images: `komika-server:rollback-20260730`, `komika-server:rollback-20260730b`.
  DB backups: `/tmp/predeploy-backup-20260730.sqlite3`, `/tmp/predeploy-backup-2-20260730.sqlite3`.
* Pre-existing CI debt, not yours: ~19 clippy warnings on unmodified code. Count before and after so
  it cannot hide one you introduce.

## How to work

* **Confirm scope with the owner before starting**, and before anything destructive or outward-facing.
* Ship and verify one unit at a time. Verify against §10's metrics table.
* **If a measurement contradicts the plan, say so and append to the §8 log** — never overwrite. That
  log is the single most valuable artifact here.
* Memories and doc line numbers are point-in-time. If one names a file, function or flag, **verify it
  still exists** before relying on it — several were stale within a day.
* **Report outcomes faithfully.** If tests fail, show the output. Do not claim a unit is done until its
  exit criteria are demonstrably met, in production where the criteria say so.
