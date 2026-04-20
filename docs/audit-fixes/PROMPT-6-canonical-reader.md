# Fix Prompt — Phase 6 (canonical reader path)

> **Self-contained kickoff prompt for one work session.** Implements the canonical-reader-path items of
> the Komika audit: **6.1 (CR2), 6.2 (CR3), 6.3 (CR4)**. Phases 1–5 are done. Item **6.4 (CR6) has its
> own dedicated prompt** — [`PROMPT-6.4-canonical-progress.md`](PROMPT-6.4-canonical-progress.md) — and is
> larger and independent; run it separately (see "Item 6.4" below). Evidence for every finding ID is in
> [AUDIT_FINDINGS.md](../../AUDIT_FINDINGS.md) (Domain — Canonical reader, `CR2`–`CR6`); the phase plan +
> checklist is [AUDIT_FIX_PLAN.md](../../AUDIT_FIX_PLAN.md). Repo root: `/Users/caved/dev/komika`.

Paste everything below the line into a fresh session.

---

You are implementing **Phase 6** (items 6.1–6.3) of the Komika audit remediation at
`/Users/caved/dev/komika`. Read this whole prompt first. These are the "canonical (MangaDex-mirrored,
`w_`-prefixed) works read correctly and deterministically" fixes: two server-side, one reader-side. Do
them as **separate commits, one per item** on a single phase branch. Before editing, re-verify each
`file:line` against the current code — search for the quoted code; line numbers may have drifted.

## Workflow rules (per item)
- Start from `main`: `git checkout main && git checkout -b audit-fixes/phase6-canonical-reader` (all
  items on this one branch, one commit per item; **never commit on `main`**).
- Re-read the finding in AUDIT_FINDINGS.md before touching its fix.
- Match surrounding style.
  - Server: extend the `graphql/mod.rs` test module (in-memory pool, `sqlx::migrate!`, `seed_user`,
    `exec(...)`) or add a pure unit test next to `select_reader_chapters` in `catalog/mod.rs`. The
    existing test `reader_chapters_dedupe_prefer_english_and_order` is the one to extend for CR2.
    **Prefer writing the failing test first.**
  - Reader: `pnpm -C apps/reader run check` (svelte-check, must stay 0 errors / 0 warnings).
- Verify per item:
  - Server items: `cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test`
  - Reader item: `pnpm -C apps/reader run check` (and a quick reasoning check that ordering is preserved).
- Commit locally with the ID in the message (e.g. `fix(catalog): deterministic english chapter pick [CR2]`).
  **Do not push or open a PR unless asked.**
- Tick the item's checkbox in AUDIT_FIX_PLAN.md (§ Phase 6) with a one-line note. Keep the checklist
  honest — if you narrow or defer anything, say so on the line.
- If a fix reveals an adjacent issue not in AUDIT_FINDINGS.md, note it in the checklist line rather than
  silently widening scope.

## Current landmarks (verified 2026-07-14; may drift a few lines)
`apps/server/src/catalog/mod.rs`
- `load_canonical_chapters(pool, work_id)` **:282-295** — the SELECT has **no `ORDER BY`** (CR2 root);
  filters `ss.source_type = 'mangadex' AND c.lang = 'en'`; hands rows to `select_reader_chapters`.
- `chapter_sort_key(&Option<String>)` **:314-319** — number-less/unparseable → `f64::INFINITY` (sorts
  last, server-side).
- `select_reader_chapters(rows)` **:324-347** — dedupes to one row per number string; the English-upgrade
  branch only fires `if is_en && existing.lang != en`, so **two English rows for the same number keep the
  arbitrary DB order** (CR2). Pure function → unit-testable.
- `CanonicalChapter` has `external_id`, `number`, `volume`, `lang`, `title`, `published_at`.

`apps/server/src/graphql/mod.rs`
- `canonical_series` **:823-836** and `canonical_chapters` **:841-857** — both `load_canonical_work(...)`
  then NSFW-gate. `load_canonical_work` returns `Some` even for a backfilled `w_<numeric>` work whose
  `mangadex_id` is `None` (CR3).
- `map_canonical_series(work, count)` **:425** — `match (&work.mangadex_id, &work.cover_file_name)`
  (`mangadex_id: Option<String>`); a `None` yields an empty cover.
- `map_canonical_chapter(work_id, c)` **:473** — `number: f64` via `unwrap_or(0.0)`, so a number-less /
  oneshot chapter is emitted as **`0.0`, indistinguishable from a real chapter `0`** on the wire. The
  `Chapter.number` field is **non-null**, so the client cannot identify number-less rows by value (CR4).

`apps/reader/src/lib/data/source.ts`
- `isCanonicalId(id)` **:30-32** — `id.startsWith('w_')`; routes **any** `w_…` id to the canonical
  resolvers (CR3).
- Client re-sorts by number `a.number - b.number` at **:397** (`mapSeriesView`, used by the series page)
  and **:547** (`getReaderChapter`, the reader). **Both call sites are shared by the canonical *and*
  Suwayomi paths** — `getSeries`/`getReaderChapter` route canonical vs. Suwayomi chapters into the same
  mapper. A number-less/oneshot chapter arrives as `0` and jumps to the front, contradicting the server's
  number-less-last order (CR4). **The sort at :277 is inside `getLibrary` — the Suwayomi-only Library
  screen, NOT the canonical path — leave it untouched; the finding excludes it.**

---

## Item 6.1 — CR2: deterministic English-scanlation selection 🟡 (server)

**Finding CR2:** `load_canonical_chapters` has **no `ORDER BY`**, and `select_reader_chapters` keeps the
first-seen row per number; its English-upgrade branch only replaces a *non*-English pick. MangaDex
commonly has several English scanlation groups per chapter number, so when two rows are both English the
**arbitrary DB row order** decides which `external_id` is retained. `canonicalPages` fetches that
`external_id`, so a reload can serve a *different group's* pages / page count. The final `sort_by` orders
output but doesn't decide the kept representative.

**Fix:** make the kept representative deterministic **even when both candidates are English**. Two changes,
both cheap — do both:
- Add an `ORDER BY` to the query in `load_canonical_chapters` so row order into `select_reader_chapters`
  is stable (e.g. `ORDER BY c.published_at DESC, c.external_id ASC`).
- In `select_reader_chapters`, when a same-number English row competes with an existing English pick,
  break the tie explicitly (prefer **latest `published_at`**, then **lowest `external_id`**) rather than
  keeping first-seen. Don't rely solely on query order — keep the pure function correct on its own so the
  unit test is meaningful.

**Test:** extend `reader_chapters_dedupe_prefer_english_and_order` (or add a sibling) — two English rows
for the same number with different `published_at`/`external_id` → the **same** row is kept regardless of
input order (assert the retained `external_id` across a shuffled input). Keep the existing
English-over-non-English and numeric-ordering assertions passing.

---

## Item 6.2 — CR3: reject backfilled `w_<numeric>` (non-mangadex-anchored) ids 🟡 (server; latent)

**Finding CR3:** backfill mints `w_<numeric-suwayomi-id>` ids (migration `0005`); `isCanonicalId` routes
**any** `w_…` id to the canonical resolvers. Such a work has no MangaDex source → `mangadex_id = None` →
empty cover, and `load_canonical_chapters` filters `source_type = 'mangadex'` → **zero chapters**. There's
**no id collision** (MangaDex works use `w_<uuid-hex>`, backfill uses `w_<numeric>`) and today no feed
emits backfilled ids, so it's **latent** — but if any future feed emits one, `canonicalSeries` returns a
titleless/coverless/chapterless **shell** instead of an error.

**Fix (server-side, the robust option):** in `canonical_series` and `canonical_chapters`, after
`load_canonical_work` succeeds, treat a work with `mangadex_id.is_none()` as **not found** — return
`Err(Error::new("No such work"))`, exactly like the existing missing-work / NSFW-gate returns. This makes
the canonical path mangadex-anchored by contract and turns a silent shell into a clean error. (The
alternative — gating `isCanonicalId` in the reader on a mangadex flag — is client-only and leaves the
resolver exploitable; prefer the server guard. You may add the reader guard too if trivial, but the
server fix is the one that closes it.)

**Test:** seed a `work` + `source_series` with a **non-mangadex** source (or a work with `mangadex_id
NULL`) under a `w_…` id → `canonicalSeries`/`canonicalChapters` return "No such work" (not an empty
shell). A normal mangadex-anchored canonical work still resolves.

---

## Item 6.3 — CR4: number-less chapter ordering (server/reader agreement) ⚪ (reader)

**Finding CR4:** the server orders number-less/oneshot chapters **last** (`chapter_sort_key` →
`f64::INFINITY`), but `map_canonical_chapter` sends them over the wire as `number = 0` (oneshot
`unwrap_or(0.0)`), and the reader **re-sorts** by `a.number - b.number` (`source.ts:397,547`) — so a
oneshot (0.0) sorts to the **front** and can collide with a real chapter numbered "0". Cosmetic, but it
contradicts the server's deliberate order.

**Two constraints that shape the fix (read before choosing):**
1. **The client cannot tell a oneshot from a real ch. 0.** `map_canonical_chapter` collapses number-less
   → `0.0` on a **non-null** `number` field (see landmark). So a *pure* client-side "sentinel-sort
   number-less last" is **not achievable** — no value marks a row as number-less. Don't write
   instructions that assume the client can detect it.
2. **:397 (`mapSeriesView`) and :547 (`getReaderChapter`) are shared canonical + Suwayomi call sites.**
   Dropping the sort globally also drops it for the Suwayomi path, whose ordering guarantee is *not*
   verified here. Scope any change so Suwayomi ordering is preserved, and do **not** touch
   `:277`/`getLibrary` (Suwayomi-only, out of scope).

**Fix — pick one and note which in the checklist:**
- **(a) Reader, canonical-only — stop re-sorting on the canonical branch (recommended, lowest-risk).**
  The canonical server already returns chapters number-less-last; consume that order and skip
  `.sort((a,b) => a.number - b.number)` **only** when the chapters came from `canonicalChapters`. Both
  call sites already know they're canonical — `getSeries:435-441` branches on `isCanonicalId`, and
  `getReaderChapter:540-547` has the `canonical` flag — so thread that through (sort before calling
  `mapSeriesView`, or pass a "preserve order" flag) and leave the Suwayomi path exactly as-is. Note: (a)
  fixes the jump-to-front but **not** the exact ch-0-vs-oneshot collision (both are still `0.0`).
- **(b) Add a number-less wire signal (server + reader — larger, more faithful).** Emit `null` for a
  number-less chapter from `map_canonical_chapter` (make `Chapter.number` nullable end-to-end) so oneshots
  are *distinguishable* from ch. 0, then sort `null`-last uniformly. This also resolves the collision, but
  it touches the shared `Chapter` contract — the finding rated CR4 cosmetic and preferred keeping it
  client-side, so (b) is only worth it if you specifically want the collision gone.

Whichever you choose, the resume logic (`asc.find(c => !c.read) ?? asc[0]` at `:398`, and `asc.find(...) ??
asc[0]` at `:549`) must still pick the right "first unread" under the corrected order.

**Verify:** `pnpm -C apps/reader run check` clean (0/0); reason through a series with numbered chapters
**plus** a oneshot — the oneshot lands **last** (matching the server) and the Suwayomi Library/series paths
are unchanged. If you take (a), state in the checklist that the exact ch-0-vs-oneshot collision is a
follow-up (only (b) resolves it, and it needs the contract change).

---

## Item 6.4 — CR6: per-user progress / library / rating for canonical works 🟡 → **separate prompt**

**Do not implement CR6 from this prompt.** It has a dedicated, pre-designed kickoff:
[`docs/audit-fixes/PROMPT-6.4-canonical-progress.md`](PROMPT-6.4-canonical-progress.md). It's larger
(one migration + two tables for progress/library; ratings reuse the `reviews` table with **zero** schema
change; the only reader edit relaxes the `saveProgress` `/^\d+$/` guard at `source.ts:481`) and stands
alone. Run it as its own session/branch (`audit-fixes/6.4-canonical-progress`). Its scope boundary — the
Library *screen* merge (`getLibrary`) is explicitly a deferred follow-up, **not** part of CR6 — is baked
into that prompt; respect it.

If you're doing "all of Phase 6" in sequence: finish 6.1–6.3 here, land them, then open the 6.4 prompt
fresh. Keeping 6.4 separate is deliberate (it's the only item touching the reader's write path and a
migration).

---

## Definition of done (this prompt = 6.1–6.3)
- CR2: the retained English scanlation per number is deterministic across reloads — `ORDER BY` in the
  query **and** an explicit tiebreak in `select_reader_chapters`; canonicalPages serves a stable
  `external_id`.
- CR3: `canonicalSeries`/`canonicalChapters` return "No such work" for a non-mangadex-anchored (`w_<numeric>`
  / `mangadex_id = None`) work instead of an empty shell.
- CR4: reader chapter order agrees with the server (number-less/oneshot last) on the canonical path, with
  the Suwayomi path unchanged. (The exact ch-0-vs-oneshot *collision* is only resolved if you take option
  (b); with option (a) it's an explicitly-noted follow-up.)
- `cargo test` green (new tests for CR2 + CR3); `cargo clippy -- -D warnings` clean; `cargo fmt --check`
  clean; `pnpm -C apps/reader run check` 0/0.
- Three checklist lines in AUDIT_FIX_PLAN.md (§ Phase 6, items 6.1–6.3) ticked with notes.
- **6.4 (CR6) is out of scope here** — closed by its own prompt. Phase 6 is "fully closed" only once 6.4
  also lands; note that on the 6.4 line.

## Out of scope
- **CR6 / item 6.4** — use its dedicated prompt; do not touch the reader write path or add a migration
  here.
- **CR5** — that's the Worker `ALLOWED_SOURCE_HOSTS` finding, already fixed in Phase 4 [I1][CR5]
  (fail-closed + shipped default). Don't revisit.
- Any change to the shared `Series`/`Chapter`/`Page` contract or the Suwayomi (numeric-id) path — these
  fixes are confined to the canonical path.
