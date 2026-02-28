# Fix Prompt — Phase 6.4 (CR6): Per-user progress / library / rating for canonical works

> Self-contained kickoff prompt for one work session. Implements finding **CR6**: MangaDex-mirrored
> canonical (`w_`-prefixed) works are currently stateless per user — the reader always resumes at chapter 1,
> the library toggle silently no-ops, and the rating is always empty. Full evidence in
> [AUDIT_FINDINGS.md](../../AUDIT_FINDINGS.md) (CR6). Repo root: `/Users/caved/dev/komika`.

Paste everything below the line into a fresh session.

---

You are implementing per-user reading state for canonical works in the Komika repo at
`/Users/caved/dev/komika`. Read this whole prompt first. All the code facts are here; verify each
`file:line` against current code before editing (numbers may have drifted; search for the quoted code).

## Workflow rules
- Branch: `git checkout -b audit-fixes/6.4-canonical-progress` (never `main`).
- Verify (below) before committing; add server tests. Commit locally with `[CR6]` in the message.
  **Do not push or open a PR unless asked.** Do not deploy.
- Prefer the pre-decided design below over stopping to ask.

## Key context (why this is smaller than it looks)
- Canonical works are addressed by a `w_`-prefixed id (vs numeric Suwayomi ids). Numeric ids route through the
  Suwayomi path (which owns its own progress/library in Suwayomi); canonical ids route through
  `canonicalSeries`/`canonicalChapters`/`canonicalPages` and have **no per-user store** — that's the gap.
- **Ratings need ZERO schema change.** The `reviews` table keys on an opaque `series_id TEXT` (no FK, no numeric
  constraint; `0001_init.sql`), `post_review` binds it directly, and `rating_summary(pool, series_id)`
  (`mod.rs:199-220`) queries by string. A `w_` id already round-trips. The only fix is making the canonical
  series resolver read the aggregate instead of returning `RatingSummary::empty()`.
- **The reader already renders these fields** the moment the server populates them — `mapSeriesView`
  (`source.ts:395-429`) reads `s.isMarked` and `s.rating.*`; resume logic keys off `Chapter.read`
  (`source.ts:398` `asc.find(c=>!c.read) ?? asc[0]`, and `getReaderChapter:549`). So most of this is server-side.

## Scope boundary — READ THIS (do not expand)
**IN scope:** on a canonical **series page** — "Add to Library" persists + reflects state, the 1–10 rating
persists + aggregates; in the **reader** — progress (last page + read) persists and drives resume-at-last-chapter.
**OUT of scope (do NOT build in this change):** making canonical works appear *in the Library screen / profile
shelves*. `getLibrary` (`source.ts:247`) enumerates only `backend.library()` (Suwayomi-global); surfacing
canonical works there needs a new `canonicalLibrary` list query + merge and is a larger, separate task. Note it
as a follow-up. CR6 is the series-page + reader round-trip only.

---

## Step 1 — Migration `apps/server/migrations/0010_canonical_progress.sql`
Highest existing migration is `0009`; migrations run via `sqlx::migrate!("./migrations")` (`db.rs:25`),
forward-only, SQLite, `foreign_keys(true)`. Mirror the `reviews`/`comments` opaque-key + user-FK pattern:
```sql
-- Per-user reading state for canonical (MangaDex-mirrored) works.
CREATE TABLE canonical_progress (
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    chapter_id     TEXT NOT NULL,   -- MangaDex chapter uuid (not an owned FK)
    work_id        TEXT NOT NULL,   -- w_-prefixed work id, for per-series aggregation
    last_page_read INTEGER NOT NULL DEFAULT 0,
    read           INTEGER NOT NULL DEFAULT 0,
    updated_at     TEXT NOT NULL,
    PRIMARY KEY (user_id, chapter_id)
);
CREATE INDEX idx_canonical_progress_work ON canonical_progress(user_id, work_id);

CREATE TABLE canonical_library (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    work_id    TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, work_id)
);
```
(FK to `work(id)` for `work_id` is optional — `work` rows exist for `w_` ids — but keep `chapter_id` FK-less
since the MangaDex uuid isn't in a stable owned table, matching `reviews.series_id`/`comments.target_id`.)

## Step 2 — Route `mark` and `set_progress` on id shape (`apps/server/src/graphql/mod.rs`)
Both currently parse the id as `i64` and fail for `w_`/uuid:
- `mark(ctx, series_id: ID, marked: bool) -> Result<Series>` (`:1101-1110`): `series_id.0.parse::<i64>()` at `:1103`.
- `set_progress(ctx, chapter_id: ID, last_page_read: i32, read: bool) -> Result<bool>` (`:1112-1129`): parse at `:1123`.

Add a canonical branch at the top of each (mirror `post_review`'s user-keyed upsert, `:1131-1174`, which does
`let user = require_user(ctx).await?;` then `INSERT ... ON CONFLICT(...) DO UPDATE SET ...` binding the opaque
id as text — no parse):
- **`mark`**: if `series_id.0.starts_with("w_")` → `require_user`, upsert/delete on `canonical_library`
  (`marked` true → `INSERT ... ON CONFLICT(user_id,work_id) DO NOTHING`; false → `DELETE`), then re-load the
  canonical work and return via the (now async) `map_canonical_series` (Step 3) so `isMarked` reflects the change.
  Else fall through to the existing numeric Suwayomi path unchanged.
- **`set_progress`**: if `chapter_id.0` is not all-digits (canonical chapter ids are MangaDex uuids) →
  `require_user`, `INSERT INTO canonical_progress (user_id, chapter_id, work_id, last_page_read, read, updated_at)
  VALUES (...) ON CONFLICT(user_id, chapter_id) DO UPDATE SET last_page_read=excluded..., read=excluded...,
  updated_at=excluded...`. **Problem:** `set_progress` gets only `chapter_id`, not `work_id`. Resolve by
  `SELECT ss.work_id FROM chapter c JOIN source_series ss ON ss.id = c.source_series_id WHERE c.external_id = ?`
  (the canonical chapter’s uuid is stored as `chapter.external_id`; confirm the exact column via the
  `canonical_chapters`/`load_canonical_chapters` query in `catalog/mod.rs`). If the lookup misses, still store
  progress with `work_id = ''` (or skip the work column) rather than erroring — progress is per-user private.
  Return `Ok(true)`. Else fall through to the numeric path.
- **Anonymous:** `require_user` already errors "Not authenticated" — that's the correct behavior (can't persist
  without a user); the reader gates the composer/library button on auth anyway.

## Step 3 — Populate canonical read state instead of hardcoding (`graphql/mod.rs`)
Currently hardcoded:
- `map_canonical_series(work, chapter_count) -> Series` (`:391-433`) — **sync, no pool/user**. Hardcodes
  `is_marked: false` (`:417`) and `rating: RatingSummary::empty()` (`:419`).
- `map_canonical_chapter(work_id, c) -> Chapter` (`:439-458`) — hardcodes `read: false` (`:454`),
  `last_page_read: 0` (`:455`).

Change:
1. Make `map_canonical_series` **async**, taking `pool: &SqlitePool` and `user_id: Option<&str>`:
   - `rating` ← `rating_summary(pool, &work.work_id).await` (reuse as-is; already string-keyed).
   - `is_marked` ← `false` if `user_id` is None, else `SELECT EXISTS(SELECT 1 FROM canonical_library WHERE
     user_id=? AND work_id=?)`.
   - Update the call site in `canonical_series` (`:789`) to `.await` and pass `current_user(ctx).await`'s id
     (Option). `ctx` is in scope; `current_user` returns `Option<User>` (`:170-176`).
2. For chapters, do the per-user lookup **in `canonical_chapters`** (`:795-811`), not per-row: after loading the
   chapters, if there's a user, run one `SELECT chapter_id, last_page_read, read FROM canonical_progress WHERE
   user_id=? AND work_id=?`, build a `HashMap<chapter_id, (last_page_read, read)>`, and have
   `map_canonical_chapter` take that state (or set the fields after mapping). Anonymous → all unread.
   This mirrors the acceptable per-series-lookup cost the codebase already pays in `map_series`.

## Step 4 — Reader: relax the progress guard (`apps/reader/src/lib/data/source.ts`)
- `saveProgress` (`:473-487`) has `if (!/^\d+$/.test(chapterId)) return;` at **`:481`** — this drops all canonical
  (uuid) progress writes. Relax it so uuid chapter ids are sent to `backend.setProgress` (widen the guard to also
  accept the MangaDex-uuid shape, e.g. allow when the id contains a hyphen / is non-empty-non-numeric). Keep
  guarding truly empty/invalid ids.
- `setLibraryMark` (`:461-470`) has **no** id-shape guard and already calls `backend.mark(seriesId, marked)` for
  any id → **no reader change needed** once the server routes `w_` (Step 2). (Contract already accepts string
  ids: `MARK`/`SET_PROGRESS` ops use `ID!`, `Id = string`; `CANONICAL_*` fragments already select
  `read`/`lastPageRead`/`isMarked`/`rating`. **No `operations.ts` / `graphql-backend.ts` / types change.**)
- Confirm the series page passes the `w_` id as `seriesId` to the reviews/rating path so canonical ratings post
  through the existing `postReview` (opaque id) — the audit already noted a canonical review is accepted; this
  should work once `map_canonical_series` surfaces the aggregate.

## Verification
- Server: `cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test`
- Add `#[tokio::test]`s (harness at `graphql/mod.rs:1699+`): seed a user + a canonical `work`/`chapter` row, then:
  - `mark("w_...", true)` → `canonical_library` has the row; `canonicalSeries` returns `isMarked: true`;
    `mark(..., false)` removes it.
  - `set_progress(<uuid>, 12, true)` → `canonical_progress` upserts; `canonicalChapters` returns that chapter with
    `read: true, lastPageRead: 12`; a second call updates in place (no duplicate row).
  - `postReview("w_...", 9, "")` then `canonicalSeries` → `rating.average`/`count` reflect it (proves the reuse).
- Reader: `cd apps/reader && pnpm check`. Optional manual (local stack, signed in): open a canonical series →
  Add to Library persists across reload; read a chapter partway → reopening resumes at that chapter, not ch.1.

## Definition of done
- Canonical series: Add-to-Library persists + reflects; rating posts + aggregates (via reused `reviews`).
- Canonical reader: progress persists; resume lands on the last in-progress chapter, not chapter 1.
- One new migration; `map_canonical_series` async + user-aware; `map_canonical_chapter` user-aware;
  `mark`/`set_progress` route on id shape; one reader guard relaxed. No contract/types change.
- Tests green; clippy clean; `pnpm check` clean.
- Update the CR6 checklist line in `AUDIT_FIX_PLAN.md`, explicitly noting that Library-screen inclusion is a
  deferred follow-up.

## Explicitly OUT of scope
- Canonical works in the Library screen / profile shelves (`getLibrary` merge) — separate, larger task.
- Any change to the numeric Suwayomi progress/library path.
- New GraphQL operations or type changes — the string-id contract already carries `w_`/uuid ids.
