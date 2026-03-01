# Fix Prompt — Phase 3 completion (items 3.2–3.5)

> **Self-contained kickoff prompt for one work session.** Finishes Phase 3 of the Komika audit.
> Item **3.1 (the `addSourceSeries` cluster) is already DONE and merged into `main`** — so the
> source-level NSFW flag is now actually being set, which is why the NSFW read-gate (3.2) comes
> next. Evidence for every finding ID is in [AUDIT_FINDINGS.md](../../AUDIT_FINDINGS.md); the
> phase plan + checklist is [AUDIT_FIX_PLAN.md](../../AUDIT_FIX_PLAN.md). Repo root: `/Users/caved/dev/komika`.

Paste everything below the line into a fresh session.

---

You are completing **Phase 3** of the Komika audit remediation at `/Users/caved/dev/komika`.
Read this whole prompt first. Do these as **separate items, one commit each** (3.1 was the only
bundled item and it is already merged). Before editing, re-verify each `file:line` against the
current code — search for the quoted code, numbers may have drifted a few lines.

## Workflow rules (per item)
- All prior audit work (Phases 1, 2, and 3.1) is already on `main`. Start from `main`:
  `git checkout main && git checkout -b audit-fixes/phase3-completion` (do all four items on this one
  branch, one commit per item; never commit on `main`).
- Re-read the finding in AUDIT_FINDINGS.md before touching its fix.
- Match surrounding style. Add/extend a `#[tokio::test]` in `graphql/mod.rs` (harness: in-memory
  pool, `sqlx::migrate!`, `seed_user`, `exec(schema, query, token, ip)` — see the tests module) or a
  `dedup.rs` unit test wherever the logic is testable. Prefer writing the failing test first.
- Verify the server after each item: `cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test`
- Commit locally with the ID(s) in the message (e.g. `fix(nsfw): gate suwayomi read path [N2]`).
  **Do not push or open a PR unless asked.**
- Tick the item's checkbox in AUDIT_FIX_PLAN.md with a one-line note. Keep the checklist honest.
- If a fix reveals an adjacent issue not in AUDIT_FINDINGS.md, note it in the checklist line rather
  than silently widening scope.

## Current landmarks (verified; may drift a few lines)
- `apps/server/src/graphql/mod.rs`
  - `viewer_show_nsfw(ctx) -> bool` **:303**, `filter_nsfw(show_nsfw, items) -> Vec<Series>` **:324**
  - `discovery` **:664**, `updates` **:719**, `search` **:877**
  - `series` **:906**, `chapters` **:913**, `pages` **:920**, `library` **:936**
  - Canonical equivalents already gate (mirror them): `canonical` series/chapters use the
    `is_nsfw && !viewer_show_nsfw` return-not-found pattern; `canonical_updates` pushes the nsfw
    predicate into SQL.
- `apps/server/src/dedup.rs` — `resolve` **:62**, `HIGH=0.85` **:57**, `MID=0.6` **:58**,
  single-token block at **:102** (`longest_token` → `candidate_work_ids_by_token`), method labels
  **:127-131** (`title_corroborated`/`fuzzy`/`external_id`), tie-break loop **:112-121**,
  `score_candidate` corroboration **:~149-170**.
- `apps/server/src/catalog/mod.rs` — `candidate_work_ids_by_token` (single `LIKE '%token%'`),
  `chapter_owner_is_nsfw` (reads `work.is_nsfw`).

---

## Item 3.2 — N2: gate the Suwayomi detail/reader path 🟡

**Finding N2:** `series`/`chapters`/`pages`/`library` apply **no** NSFW guard, while the canonical
path does. Suwayomi ids are sequential integers, so an opted-out viewer can hand-craft an id and
read full detail + chapter list + **page images** of an NSFW series; `library()` returns NSFW series
to anyone. Blast radius was ~0 before 3.1 (nothing was flagged) — **3.1 now sets the flag, so this is
live.** Sequence it first.

**Fix:**
- `series(id)` / `chapters(seriesId)` / `pages(chapterId)`: after resolving the underlying work's
  `is_nsfw`, if `is_nsfw && !viewer_show_nsfw(ctx).await` → return not-found exactly like the
  canonical resolvers (same error/shape). For `chapters`/`pages` the nsfw flag lives on the owning
  series/work — resolve it (Suwayomi `series` metadata, or the mirrored `source_series`/`work` row if
  that's the lookup path) and gate.
- `library()`: wrap the returned `Vec<Series>` in `filter_nsfw(viewer_show_nsfw(ctx).await, items)`.
- **Test:** an NSFW-flagged Suwayomi series → `series`/`chapters`/`pages` return not-found for an
  opted-out viewer and succeed for an opted-in one; `library` omits it for the opted-out viewer.
  (You can flag a series via the 3.1 add flow or by seeding an nsfw `work`/`source_series` row.)

---

## Item 3.3 — DD3 / DD4: dedup precision 🟡

**DD3 (common-title auto-merge):** the only guard against a wrong auto-merge on a shared title is the
0.6 score cap. With a thin/overlapping description, description-Jaccard + boosters can push a
same-titled-but-different work past `HIGH=0.85` and auto-merge without review. → **Require cover-pHash
corroboration for an exact-title auto-merge, OR route ultra-common normalized titles always to
Review** (never auto-merge). Pick one; the pHash-corroboration route is cleaner now that 3.1 feeds a
real cover hash. Add a test: exact-title match with description overlap but **no** pHash corroboration
must land in `Review`, not `AutoMerge`.

**DD4 (fuzzy recall):** the fuzzy block keys on only the single longest token (one `LIKE '%token%'`);
when the discriminating token isn't the longest, the block returns nothing → `Decision::New` → a
silent duplicate work. → **Block on the top-N longest tokens (union the candidate id sets)**, or a
trigram-indexed shortlist. Update `candidate_work_ids_by_token` (or add a top-N variant) and the
caller in `dedup.rs:102`. Add a test: a candidate whose discriminating token is not the longest still
finds the existing work (→ Review/AutoMerge, not New).

*(These two are one item — both are dedup-precision. One commit is fine, or split if cleaner; note
which in the checklist.)*

---

## Item 3.4 — N3 / N4: NSFW count skew + fold source flag into `work.is_nsfw` ⚪

**N3 (count skew):** `updates`/`search`/`discovery` compute `total`/`hasNextPage` from a raw
`COUNT(*)`/id-count with no nsfw predicate, then `filter_nsfw` drops rows **after** the page slice →
for opted-out viewers `total` overstates and a page can return `< PAGE_SIZE` while
`hasNextPage=true`. Cosmetic. → Push the nsfw predicate **into SQL** (mirror `canonical_updates`'s
query) for all three counts + page queries, and drop the post-slice `filter_nsfw` there.

**N4 (source flag write-only):** every gating read uses `work.is_nsfw`; `source_series.is_nsfw` is
never read, so a flag set only on `source_series` would still leak. → Ensure the source signal is
**OR'd into `work.is_nsfw`** (not just `source_series`). Check whether 3.1 already does this on the
work; if `upsert_source_series` sets `ss.is_nsfw` but the work row can be created/updated without the
OR, close that gap (e.g. in `create_work`/`upsert_work_from_mangadex` or a follow-up update).
- **Test:** an opted-out viewer's `updates.total` matches the number of items actually returned
  across pages (no skew); a work whose only nsfw signal came from the source is gated.

---

## Item 3.5 — DD5 / DD6 / DD7: dedup labels/determinism cleanup ⚪

All low-severity, one commit:
- **DD5:** `catalog/similarity.rs` computes exact shingle-Jaccard, but §4/§10 call it "MinHash." Either
  rename the function/comments to shingle-Jaccard (cheapest, honest) or implement real MinHash
  signatures. Recommend **rename** unless there's a perf reason.
- **DD6:** `dedup.rs:127-131` emits `title_corroborated`/`fuzzy`/`external_id`; migration `0005`
  documents `external_id/title_exact/fuzzy/description/cover`. Align the emitted `method` labels to the
  documented enum (or update the migration comment to match the code — pick one, note it).
- **DD7:** the Review candidate is chosen by iterating a `HashSet` (first-seen best on ties) → the
  surfaced `work_id` is arbitrary across runs when several works share the exact title and none
  corroborate. Add a **deterministic tiebreak** (e.g. lowest `work_id`, or oldest `created_at`) in the
  candidate pick at `dedup.rs:112-121`. Add a test asserting a stable pick across two runs.

---

## Definition of done
- N2: Suwayomi `series`/`chapters`/`pages`/`library` gate NSFW for opted-out viewers, matching the
  canonical path. DD3/DD4: no wrong auto-merge on a bare common title; no silent duplicate when the
  discriminating token isn't longest. N3/N4: no count skew; `work.is_nsfw` reflects the source signal.
  DD5/DD6/DD7: labels honest, Review pick deterministic.
- `cargo test` green; `cargo clippy -D warnings` clean; `cargo fmt --check` clean.
- Four checklist lines in AUDIT_FIX_PLAN.md ticked with notes. Phase 3 fully closed.

## Out of scope
- The reader-side canonical work (Phase 6) and the per-user canonical progress (6.4 has its own
  prompt). Don't touch the reader here.
- Re-litigating 3.1's genre-heuristic NSFW decision — that path is settled.
