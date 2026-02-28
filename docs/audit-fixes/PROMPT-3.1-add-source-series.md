# Fix Prompt — Phase 3.1: Wire & harden the Tier-2 "add source series" flow

> **This is a self-contained kickoff prompt for one focused work session.** It bundles four
> audit findings that MUST be fixed together (wiring the mutation without the hardening ships a
> live regression): **C1** (no client binding), **DD2** (not idempotent), **DD1** (cover-pHash not
> fed to the matcher), **N1/N5** (source NSFW flag never set). Full evidence for each ID is in
> [AUDIT_FINDINGS.md](../../AUDIT_FINDINGS.md). Repo root: `/Users/caved/dev/komika`.

Paste everything below the line into a fresh session.

---

You are implementing **one combined change** to the Komika repo at `/Users/caved/dev/komika`.
Read this whole prompt first. All the code facts you need are here — do not re-derive them, but
DO verify each `file:line` against the current code before editing (numbers may have drifted a few
lines; search for the quoted code).

## Workflow rules
- Work on a branch: `git checkout -b audit-fixes/3.1-add-source-series` (never `main`).
- This is the ONE item that is NOT split — implement all of C1+DD2+DD1+N1/N5 in this branch.
- Verify (below) before committing. Add/extend the server unit test.
- Commit locally with `[C1][DD2][DD1][N1]` in the message. **Do not push or open a PR unless asked.**
- Do not deploy or run against production. A local Suwayomi (docker) is available for the NSFW probe (§4).
- If you hit a genuine fork not covered here, prefer the pre-decided fallback in this prompt over stopping.

## Background: what already exists
- The **server resolver `add_source_series` already works** (`apps/server/src/graphql/mod.rs:1500-1610`),
  and the SDL (`addSourceSeries`, `MatchResult`, `mergeQueue`, `MergeCandidate`, `resolveMergeCandidate`)
  is fully defined. The admin **merge-review console already consumes** `mergeQueue`/`resolveMergeCandidate`.
- What's missing/broken: no client can CALL `addSourceSeries` (C1), and the resolver has three latent bugs
  (DD2/DD1/N1) that go live the moment a client can reach it. Hence one change.

---

## Part A — C1: client binding (types → contract → backend → admin UI)

### A1. `packages/types/src/index.ts` — add the `MatchResult` type
No `MatchResult` / `*Result` type exists yet. Mirror the SDL (`schema/komika.graphql:283-290`
`type MatchResult { decision: String! workId: String! matchedWorkId: String score: Float method: String sourceSeriesId: String! }`)
using the file's house style (`export interface`, `Id` for ids, `T | null` for nullables):
```ts
export interface MatchResult {
  decision: string; // 'auto_merge' | 'review' | 'new'
  workId: Id;
  matchedWorkId: string | null;
  score: number | null;
  method: string | null;
  sourceSeriesId: string;
}
```

### A2. `packages/api/src/operations.ts` — add the operation doc
House style is `export const NAME = /* GraphQL */ \`...\``. Add (select every `MatchResult` field):
```ts
export const ADD_SOURCE_SERIES = /* GraphQL */ `
	mutation AddSourceSeries($suwayomiMangaId: ID!) {
		addSourceSeries(suwayomiMangaId: $suwayomiMangaId) {
			decision
			workId
			matchedWorkId
			score
			method
			sourceSeriesId
		}
	}
`;
```
(`operations.ts` is imported as `import * as ops from './operations.js'`; no `index.ts` re-export needed.)

### A3. `packages/api/src/backend.ts` — add the optional method
Mirror the existing optional-admin-method style (`resolveMergeCandidate?`, `mergeQueue?` at ~`:100-127`):
```ts
/** Optional: only the unified Komika API implements it. Runs the Tier-2 dedup add flow. */
addSourceSeries?(suwayomiMangaId: Id): Promise<MatchResult>;
```
Import `MatchResult` from `@komika/types` at the top of the file if not already imported.

### A4. `packages/api/src/graphql-backend.ts` — implement it
Mirror `resolveMergeCandidate` (`:186-197`), using the private `this.gql<T>()`:
```ts
async addSourceSeries(suwayomiMangaId: Id): Promise<MatchResult> {
  const d = await this.gql<{ addSourceSeries: MatchResult }>(ops.ADD_SOURCE_SERIES, { suwayomiMangaId });
  return d.addSourceSeries;
}
```
Import `MatchResult` from `@komika/types` at the top (`:1-16`).

### A5. `apps/admin/src/lib/data.ts` — add a guarded wrapper
Mirror `resolveMergeCandidate` (`:74`) / `loadMergeQueue` (`:64`) which throw if the optional method is absent:
```ts
export async function addSourceSeries(suwayomiMangaId: string) {
  if (!backend.addSourceSeries) throw new Error('Add source series is unavailable on this backend.');
  return backend.addSourceSeries(suwayomiMangaId);
}
```

### A6. `apps/admin/src/routes/+page.svelte` — add the "Add" action
The catalog root page already lists `Series[]` (search box + library) and each row exposes `s.id`
(= the Suwayomi manga id; `suwayomi-backend.ts:300` sets `id: String(m.id)`). The per-row action cell is
`<span class="col-actions">` with the existing `<button class="edit" onclick={() => openEditor(s)}>Edit</button>`
(`:196-198`). Add an "Add" button next to Edit that calls `addSourceSeries(s.id)` and surfaces
`result.decision` (a toast or inline text: e.g. "auto-merged into <workId>" / "queued for review" / "added as new").
Handle the thrown-error case (show the message). No new route or nav entry is needed.
- **Scope note / don't chase:** `loadCatalog` (`data.ts:21`) falls back to `library()` then `search('')`; whether
  that list surfaces not-yet-added series depends on server `search` behavior. If during verification the list
  mostly shows already-added series, that's fine for wiring — the mutation is idempotent after Part B, so
  re-adding is a no-op. Note it as a follow-up; do NOT build a new source-browse UI in this change.

---

## Part B — DD2: make `add_source_series` idempotent (server)
File: `apps/server/src/graphql/mod.rs`, resolver at `:1500-1610`.

**The bug:** on a repeat call with the same `suwayomiMangaId`, `create_work` runs for `New`/`Review`
*before* any existence check → mints an orphan `work` every time; `upsert_source_series`'s
`ON CONFLICT(source_type,source_id,source_key) DO UPDATE` keeps the ORIGINAL `work_id`
(`catalog/mod.rs:522-537`), so the new work is orphaned; and `Review` calls `insert_merge_candidate`
again (`catalog/mod.rs:640`, plain INSERT, no conflict handling) → duplicate pending rows.

**The fix:** `find_source_series_id` already exists and is unused (`catalog/mod.rs:590`,
`pub async fn find_source_series_id(pool, source_type, source_id, source_key) -> Result<Option<String>>`).
At the TOP of the resolver, right after fetching `m` and computing `mid`:
```rust
// Idempotency: if this source series is already linked, return the existing linkage untouched.
if let Some(ssid) = crate::catalog::find_source_series_id(&st.pool, "suwayomi", &m.source_id, &mid.to_string())
    .await.map_err(gql_err)? {
    // fetch the existing work_id for this source_series to report it back
    // (add a small helper or SELECT work_id FROM source_series WHERE id = ?)
    return Ok(MatchResult { decision: "existing".into(), work_id: <existing>, matched_work_id: None,
                            score: None, method: None, source_series_id: ssid });
}
```
Only run the matcher + `create_work` when no existing `source_series`. (A `SELECT work_id FROM source_series
WHERE id = ?` is fine; or add `find_source_series` returning `(id, work_id)`.) Do NOT change `ensure_source_series`'s
conflict clause — the pre-check is the correct layer.

---

## Part C — DD1: feed cover-pHash into the matcher (server)
**Actionable half only.** The `Candidate` is built with `cover_phash: None` and `external_ids: Vec::new()`
(`mod.rs:1513-1519`). Suwayomi carries **no** external tracker IDs (`SuwayomiManga`/`MANGA_FIELDS` have none),
so `external_ids` stays empty — **leave it, add a one-line comment saying why; do not chase AniList/MAL**.
The pHash half is fully actionable and is the doc's "strongest cheap signal":

1. **Add an image-fetch helper to `SuwayomiClient`** (`apps/server/src/suwayomi.rs`). It has a private
   `http: reqwest::Client` (`:97`) and a public `abs(url: Option<&str>) -> String` (`:128`) but only GraphQL
   methods. Mirror MangaDex's `cover_phash` (`mangadex.rs:209-218`):
   ```rust
   pub async fn cover_bytes(&self, thumbnail_url: Option<&str>) -> Option<Vec<u8>> {
       let url = self.abs(thumbnail_url);
       let res = self.http.get(url).send().await.ok()?;
       if !res.status().is_success() { return None; }
       Some(res.bytes().await.ok()?.to_vec())
   }
   ```
2. **In `add_source_series`**, before building `Candidate`, compute the hash with the SAME function the
   MangaDex path uses — `crate::phash::dhash(&bytes) -> Option<String>` (`phash.rs:14`):
   ```rust
   let cover_phash = match st.suwayomi.cover_bytes(m.thumbnail_url.as_deref()).await {
       Some(bytes) => crate::phash::dhash(&bytes),
       None => None,
   };
   ```
3. Set `cover_phash: cover_phash.clone()` on the `Candidate` (feeds `score_candidate`'s
   `phash_similarity`, `dedup.rs:152`) **and** `cover_phash` on the `make_work()` `WorkInput`
   (`WorkInput.cover_phash: Option<String>`, `catalog/mod.rs:29-49`) so a newly-created work stores its own hash.

---

## Part D — N1/N5: fetch & propagate the source-level NSFW flag (server)
**The bug:** `make_work().is_nsfw` is hardcoded `false` (`mod.rs:1534`) and `upsert_source_series(..., false)`
(`mod.rs:1588`); the Suwayomi query never requests any nsfw field. CATALOGUE.md §2 requires NSFW = source flag
OR contentRating; only the MangaDex-contentRating half exists (`mangadex.rs:395`:
`matches!(content_rating.as_deref(), Some("erotica") | Some("pornographic"))`).

**Unknown, pre-decided path (do NOT stop on this):** whether Suwayomi's GraphQL exposes an nsfw flag is not
verifiable from this repo — the repo comment `suwayomi-backend.ts:313` even claims "Suwayomi has none."
Upstream Suwayomi/Tachidesk `SourceType` *does* expose `isNsfw: Boolean!` (source-level, not manga-level), but
treat that as unconfirmed. Do this:

1. **Probe the live schema** (a local Suwayomi container is available; see `SPEC.md`/`deploy/`): run an
   introspection or trial query `{ source(id:"...") { isNsfw } }` (or add `source { lang isNsfw }` to a manga
   query) against `http://localhost:4567/api/graphql`. Determine whether `source.isNsfw` resolves.
2. **If it resolves:** extend `MANGA_FIELDS` (`suwayomi.rs:12-18`) to `source { lang isNsfw }`, add `is_nsfw:
   Option<bool>` to `SuwayomiSourceLang` (rename to `SuwayomiSource` if clearer), and set
   `let source_nsfw = m.source.as_ref().and_then(|s| s.is_nsfw).unwrap_or(false);`.
3. **If it does NOT resolve (fallback — deterministic, so you never stall):** derive from the already-fetched
   `genre: Vec<String>` — `let source_nsfw = m.genre.iter().any(|g| { let g = g.to_ascii_lowercase();
   ["hentai","erotica","smut","pornographic","adult"].iter().any(|k| g.contains(k)) });`. Add a code comment
   that this is a heuristic fallback because the source lacks an explicit flag.
4. **Propagate either way:** OR the source signal into BOTH `make_work().is_nsfw` (replace the hardcoded `false`)
   and the `upsert_source_series(..., source_nsfw)` arg (replace the hardcoded `false`, `mod.rs:1588`). Per N4,
   the gate reads `work.is_nsfw`, so the work value is the one that matters — set both for consistency.
5. Record in your checklist note which path (2 or 3) you took and why.

---

## Verification (run before committing)
- Server: `cd apps/server && cargo build && cargo fmt --check && cargo clippy -- -D warnings && cargo test`
- Add/extend a `#[tokio::test]` in `graphql/mod.rs` (mirror the harness at `:1699+`: in-memory pool,
  `sqlx::migrate!`, seed an admin, `exec(schema, query, token, ip)`). Cover:
  - **DD2:** call `addSourceSeries` twice with the same id → second returns `decision:"existing"`, and assert
    `SELECT COUNT(*) FROM work` and `FROM merge_candidate` did NOT grow on the second call.
  - **N1:** an NSFW-genre (or source-nsfw) series produces a `work` with `is_nsfw = 1`.
  - (DD1 pHash needs network/an image; assert the plumbing compiles + `cover_bytes`/`dhash` are wired — a unit
    test can pass a tiny in-memory PNG to `phash::dhash` directly.)
- Contract/admin: `cd packages/api && npx tsc --noEmit` (or the repo check script); `cd apps/admin && pnpm check`.
- Manual (optional, local stack): sign in to admin → catalog → click "Add" on a row → see the decision surfaced;
  re-click → idempotent (no duplicate review-queue entry).

## Definition of done
- A client (admin catalog page) can invoke `addSourceSeries`; the result decision is shown.
- Re-adding the same series is idempotent (no orphan `work`, no duplicate `merge_candidate`).
- The dedup candidate carries a real cover-pHash; a new work stores its hash.
- `work.is_nsfw` is set from a real source signal (explicit flag or genre fallback), never hardcoded false.
- Tests green; `cargo clippy -D warnings` clean; `tsc`/`pnpm check` clean.
- Update the checklist line for 3.1 in `AUDIT_FIX_PLAN.md` with a note on the NSFW path taken and any follow-up
  (e.g. the admin-list-surfaces-addable-series caveat).

## Explicitly OUT of scope (do not expand into these)
- External-ID (AniList/MAL) matching — Suwayomi provides none; leave `external_ids` empty.
- A new source-browse/discovery UI in admin — attach to existing catalog rows only.
- Changing `ensure_source_series`'s conflict clause — the idempotency pre-check is the right layer.
- The `series`/`chapters`/`pages` NSFW read-gate (that's finding **N2**, a separate item — Phase 3.2).
