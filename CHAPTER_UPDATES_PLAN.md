# Chapter updating — diagnosis, architecture, and implementation plan

**Status:** design complete, no code written yet.
**Author:** investigation of 2026-07-30.
**Supersedes:** the layered Rev-1/2/3 draft of the same name. Where this document and any earlier
note disagree, this one wins — several Rev-1 claims were overturned by later measurement and are
preserved in §8 as an audit trail rather than silently deleted.

---

## 1. Executive summary

The reported symptoms were:

1. Suwayomi series never reach `/updates` despite having chapters on their series page.
2. MangaDex series are "mostly missing" from `/updates` despite being scanned.
3. Scan cadence ignores publication status.
4. The `all.mangadex` Suwayomi extension is redundant.
5. MangaDex external chapters are not handled.

**The single most expensive assumption — that the MangaDex chapter mirror has a gap — is false.**
The mirror is complete (§4.1). Every loss is downstream, in two materialized-view queries and in
the scheduling policy. Re-seeding the firehose would have cost days and fixed nothing.

The root cause of symptoms 1, 2 and 4 is one structural fact: **there are two parallel, incompatible
update pipelines**, and the Suwayomi one never writes the canonical `chapter` table — it holds
**0 Suwayomi rows out of 877,824** (§5). Everything else follows from that.

Nine defects were found. Four were not in the original report, and two of those are larger than
anything that was:

| # | Defect | Blast radius | In original report? |
|---|---|---|---|
| F1 | External chapters unopenable (blank reader) | **~35,000 chapters (4% of mirror)** | partially |
| F2 | Oneshot works excluded from the chapter aggregator | **21,422 works (18.5% of catalogue)** | **no** |
| F3 | Suwayomi series locked out of the feed | 9,851 of 11,797 series | yes |
| F4 | Chapter COUNT rendered as chapter NUMBER | all Suwayomi-only cards, ≥3 surfaces | yes |
| F5 | Feed staleness — 6 h rebuild, no incremental MangaDex writer | whole feed | yes (as "missing") |
| F6 | No status/format-aware cadence | 14,098 series | yes |
| F7 | Later sources re-float already-reported chapters | multi-source works | yes |
| F8 | `all.mangadex` duplication | 10,422 of 14,103 rows (74%) | yes |
| F9 | Latent: firehose cursor advances past uncatalogued works | 0 today, permanent when it fires | **no** |
| F10 | "Completed = no scans" would destroy the only reopen-detection path | 598 series would go permanently blind | **no** |
| F11 | **A broken source reports as healthy** — silent fallback to cached chapters | 209 series proven frozen; class is undetectable by design | **no** |
| F12 | Source picker defaults by chapter COUNT, not by newest chapter | every multi-source series' default source | **no** (contradicts an owner requirement) |
| — | **99% of scans find nothing** (1.081% hit rate, 93 fetches per chapter found) | 20,132 of 20,352 daily fetches wasted | **no** (see Phase E5) |

The plan is seven phases (§7). Phase A alone fixes F1, F2 and F4 — the three largest user-visible
defects — and is pure read-path with no lock-profile change. Phase E2 replaces paused-series polling
with a push trigger off each source's LATEST ranking: **~540 scans/day → ~0.4, with reopen detection
14× faster**, at zero additional upstream fetches.

---

## 1a. Owner requirements, verbatim

These are the stated requirements this plan exists to satisfy. Quoted so no phase drifts from them,
with the finding/phase that delivers each.

> **"For series with multiple sources, if x source updates chapter y first, it will be registered in
> the updates page and if more sources update the chapters later, they won't hit the updates page
> since another extension updated it earlier."**

First-source-wins per *chapter*, not per series. → **F7**, delivered by Phase C's `release_event`
ledger as `PRIMARY KEY (work_id, chapter_key)` + `INSERT OR IGNORE`. Today `released_at = MAX(...)`
does the opposite: a later source re-floats the card.

> **"Just because a series was merged doesn't mean we won't be checking all of the sources of that
> said series."**

Merging must not reduce source coverage. → Already true structurally (the scanner is keyed per
Suwayomi series, so each source of a merged work is scanned independently), and **preserved** by
Phase B (every source writes into the one `chapter` spine) and Phase E3 (per-source scanners).
`authoritative_suwayomi_mappings` keeps one mapping per `source_id` so coverage is per-source, not
per-work. **Do not "optimise" a merged work down to one source.**

> **"The moment a new chapter is found by the scanner, it will be sent to the updates page."**

→ **F5**. Requires the incremental feed writer in Phase C; today the MangaDex half waits up to 6 h for
a rebuild, and 369 cards are stale even after a detection.

> **"By chapter, I mean the name of the chapter, not the amount of chapters a series has per source.
> Some sources have redundant half chapters."**

→ **F4**. Never a count. 35,091 genuine half-chapters exist on the Suwayomi side [M], which is why the
chapter key keeps 2 decimal places.

> **"Our updater, home page, update page, series detail, source detail, browse page will show that
> number as the latest chapter number."**

All six surfaces must print the chapter **number**. Known call sites [C]:

| Surface | Location | Status today |
|---|---|---|
| Updates page card | `source.ts:300-305` `Ch. {latestChapter ?? chapterCount}` | **count fallback — must go** |
| Card mapper (home/browse) | `source.ts:243` `Ch. ${s.chapterCount}` | **count, unconditionally — must go** |
| Continue-reading card | `source.ts:1282` `Ch. ${read + 1}` | **read index, not a number — must go** |
| Home hero | `routes/(app)/+page.svelte:117` | via `current.ch` |
| Series detail | `routes/(app)/series/[slug]/+page.svelte:639` | via `continueCh` |
| Reader chapter list / up-next | `routes/read/[slug]/+page.svelte:351,595` | uses `c.n` — already a number |
| Admin updates panel | `apps/admin/src/routes/updates/+page.svelte` | audit |
| Library / profile progress | `library:63`, `profile:151` `Ch. {read} / {total}` | **legitimately a ratio — leave alone** |

Note: there is **no dedicated `source detail` route** in the reader (`routes/(app)/` has browse,
library, login, profile, series, support, updates). Confirm with the owner whether this means the
source picker inside series detail, or a page still to be built.

> **"Some sources name the chapters (chapter 45 → take 45; chapter 45: beginning of the end → take 45),
> just take the number from that and use that. Do not use the number of chapters."**

→ Phase A2. Take the **first** number, not the largest — "Chapter 45: The 100 Kings" is 45. Note this
is a **~0.15% fallback**: `suwayomi_chapter.chapter_number` already agrees with the name on 3,994 of
4,000 sampled rows [M].

> **"If source x was the fastest to update series Y's chapter 100, we will send it to the updates page
> and the series detail page will have the latest newest chapter's source auto-selected."**
>
> Clarified: **"some sources pick up a series from midway. Series Y has 151 chapters, source X has 150
> translated, but another source picked it up halfway and has 70 chapters — but their most recent
> chapter is ch 151. That's what I meant by most updated chapter."**

→ **F12**, new. Auto-select the source with the **highest chapter NUMBER** (furthest ahead), not the
most chapters and not the most recent upload. `pickDefaultKey` sorts by `chapterCount` today. Needs a
mis-merge guard (a 1-chapter source must not win) and must ship with/after Phase B — see F12 for the
date-less-rows consequence. A saved user preference always wins.

## 2. Methodology and confidence levels

Every claim below is tagged:

* **[M]** — measured against a `.backup()` snapshot of the live production DB (1.7 GB, taken
  2026-07-30 06:18 UTC) and/or the live MangaDex API. Reproduction queries in §10.
* **[C]** — read directly from the code, with `file:line`.
* **[I]** — inferred. Explicitly flagged as such; treat as a hypothesis, not a fact.

The production DB was **never read directly** — it was snapshotted with SQLite's `.backup()` API
first, because a long-lived reader pins the WAL and has previously locked the application.

**A measurement trap worth recording:** do not compare per-day `published_at` counts against
upstream, and do not sample MangaDex with `order[publishAt]`. MangaDex stamps external chapters
`publishAt = 2037-12-31T15:00:00`, which sorts them to the top of any `publishAt`-ordered query and
manufactures a phantom ~30% gap. This cost one full round of wrong analysis. Sample with
`order[readableAt]=desc`, which is what MangaDex's own "latest updates" surface uses.

---

## 3. The system as it exists today

```
                    ┌──────────────────── MangaDex direct API ───────────────────┐
                    │  /manga  (catalogue sweep, createdAt→updatedAt windows)    │
                    │  /chapter (firehose, English, all 4 content ratings)       │
                    └───────────────────────────┬───────────────────────────────┘
                                                ▼
                                        work + source_series
                                                │
                                                ▼
                                    chapter  ← 877,824 rows, 100% MangaDex
                                                │
                                                ▼  every 6 h (CATALOGUE_SYNC_INTERVAL_SECS)
                                        feed_updates (63,805)
                                                │
                                                ▼
  ┌── Suwayomi extensions ──┐             feed_series_updates (64,902) ──▶ /updates
  │  daily source sync      │                    ▲
  │  scan scheduler (5 min) │────────────────────┘
  └─────────┬───────────────┘   via series_scan_state.last_new_chapter_at
            ▼
   suwayomi_series (14,111)
   suwayomi_chapter (563,095)   ← never enters `chapter`
   series_scan_state (14,098)
```

**Live configuration [C/M]** (`docker inspect komika-server-1`):

| Variable | Value | Meaning |
|---|---|---|
| `CATALOGUE_SYNC_INTERVAL_SECS` | `21600` (6 h) | Drives catalogue sweep, chapter cycle **and** the whole feed-rebuild chain |
| `SCAN_TICK_SECONDS` | `300` (5 min) | Scan scheduler tick (selects due series only) |
| `SOURCE_SYNC_INTERVAL_SECONDS` | `86400` (1 day) | Extension source sync — **already matches intent** |

Scale [M]: 115,656 works · 113,863 MangaDex source_series · 14,103 Suwayomi source_series ·
877,824 chapters · 563,095 `suwayomi_chapter` rows · 64,902 feed rows.

---

## 4. Findings

### 4.1 The MangaDex mirror is complete — do not re-seed [M]

Two independent tests:

* **Recency.** 300 newest-by-`readableAt` English chapters upstream → **300/300 mirrored, 0 missing.**
  Of their 170 distinct works: 111 visible in our feed, 13 hidden by the NSFW gate, 46 absent —
  and all 46 were absent for the reason in §4.2, not for a mirroring gap.
* **Depth.** 40 random `source_series` rows, our English chapter count vs upstream's per-manga
  `total` → **0/40 short, total deficit 0.**
* **Totals.** 877,824 mirrored vs **877,042** upstream.

The catalogue sweep, the chapter firehose and both seeds are healthy. `catalogue_sync_state` shows
both cursors current to within ~3 minutes of the snapshot.

### 4.2 F1 — External chapters are unopenable (~35,000 chapters) [M]

**This is the largest user-visible defect and it was under-scoped in the original report.**

Sampling 300 random mirrored English chapters and batch-querying them upstream: **12 carry an
`externalUrl`** → extrapolated **~35,000 chapters, 4% of the mirror**. Hosts seen: `mangaplus.shueisha.co.jp`,
`comikey.com`, `namicomi.com`, `bilibilicomics.com`.

`MdChapterAttrs` (`mangadex.rs:876`) parses neither `externalUrl` nor `readableAt`, so we store
neither. The reader therefore requests pages for a chapter that has none and renders blank — this
is a substantial share of the "unstable image Worker" reports.

Three sub-findings that shape the fix:

* **`publishAt = 2037-12-31T15:00:00` is a sentinel, not a date.** It marks one flavour (MangaPlus).
  Because `refresh_feed_updates` bounds `published_at <= now`, **46 works whose every chapter is
  2037-dated are permanently absent from the feed**, and 21 more show a stale older chapter as
  "latest". Upstream has exactly 236 such chapters and we mirror all 236.
* **`pages == 0` is NOT a valid discriminator.** Sampled bilibilicomics external chapters report
  `pages: 45` and `pages: 50`. Only `externalUrl IS NOT NULL` is authoritative.
* **`readableAt >= publishAt` does NOT hold.** Sampled bilibili chapters have `readableAt`
  **two weeks earlier** than `publishAt` (`readableAt 2022-11-02`, `publishAt 2022-11-16`). So a
  naive `COALESCE(readable_at, published_at, …)` silently changes which chapters count as released.

Exemplar — *Magical Girl Tsubame* ch. 1: `publishAt 2037-12-31`, `readableAt 2023-10-07`,
`pages 0`, `externalUrl https://mangaplus.shueisha.co.jp/viewer/1019123`.

### 4.3 F2 — Oneshot works are excluded from the chapter aggregator (21,422 works) [M/C]

**Not in the original report, and the second-largest defect.**

`catalog::work_source_chapters` (`catalog/mod.rs:2971-2978`) filters the MangaDex half with:

```sql
AND c.number IS NOT NULL AND c.number GLOB '*[0-9]*'
```

The comment explains the intent — `CAST('Extra' AS REAL)` yields `0.0`, which would masquerade as a
real chapter 0. The intent is right; the consequence was not measured. **23,254 chapters carry a
NULL or non-numeric number, and 21,422 works have *nothing else*** — their entire chapter list is
`Oneshot`, `Extra`, `?`. Those works therefore show:

* an **empty chapter list** on the series-detail page (nothing to click), and
* a **blank chapter label** on `/updates` — the same cohort as the 21,567 feed rows with
  `latest_chapter IS NULL` measured independently.

A oneshot is a legitimate content type with exactly one chapter. 18.5% of the catalogue is
currently unreadable through the aggregated path. The fix is a distinct non-numeric bucket, not a
looser numeric cast.

### 4.4 F3 — Suwayomi series locked out of the feed (9,851 series) [M/C]

`catalog/mod.rs:834`, the scanner half of `refresh_feed_series_updates`:

```sql
WHERE ss.source_type = 'suwayomi' AND sy.in_library = 1
  AND sy.latest_chapter_at IS NOT NULL
  AND sss.last_new_chapter_at IS NOT NULL      -- ← the lockout
```

`last_new_chapter_at` is stamped only when a scan observes a chapter id absent from the previous
snapshot. The **first** observation is a baseline and deliberately never counts (`scanner.rs:1504`,
`let baseline = first_observation || !have_prior_snapshot;`).

Measured: of **11,797** in-library Suwayomi series that have mirrored chapters, **9,851 have no
detection** and can never enter the feed. **1,030** of those works have no MangaDex anchor either,
so they are invisible on every surface. This is symptom 1, exactly.

The series-detail page is unaffected because it reads `work_source_chapters` live — which is why the
user could see chapters there and not on `/updates`.

### 4.5 F4 — Chapter COUNT rendered as chapter NUMBER [M/C]

`catalog/mod.rs:823` inserts `latest_chapter = NULL, chapter_count = sy.chapter_count`, and the
reader falls back to the count. The code already admits it (`apps/reader/src/lib/data/source.ts:291-294`):

> the mirror half knows the newest chapter's NUMBER ("Ch. 10.5"), the scanner half only the series'
> chapter COUNT ("Ch. 412"). They are different quantities and the server sends whichever it has.

Affected surfaces found [C]:

| Surface | Line | Problem |
|---|---|---|
| Updates feed card | `source.ts:300-305` | `Ch. {latestChapter ?? chapterCount}` |
| Another card mapper | `source.ts:243` | `ch: \`Ch. ${s.chapterCount}\`` — count, unconditionally |
| Continue-reading card | `source.ts:1282` | `ch: \`Ch. ${read + 1}\`` — a read *index*, not a number |

Legitimately ratio-based and **not** to be changed: `library/+page.svelte:63` and
`profile/+page.svelte:151` render `Ch. {read} / {total}` as reading progress.

**The data to fix this already exists.** `suwayomi_chapter` stores `chapter_number REAL` and `name`
for all 563,095 rows, and it agrees with the number embedded in `name` on **3,994 of 4,000** sampled
rows (99.85%). The six disagreements are pathological upstream data (`Ch.99999999 - TEST`,
`Ch.20221017`, `Chapter 13,14`).

Two traps [M]:

* **A naive `MAX(chapter_number)` prints garbage.** `Ch.99999999` and `Ch.20240120` (a date used as
  a chapter number) would label 2 Suwayomi series and 10 MangaDex works `Ch. 100000000`. Needs a
  sanity clamp.
* **`MAX(number)` and newest-release disagree on 1,174 of 11,797 series (10%).** Material, not
  cosmetic. The update event is about the newly *released* chapter, so the label must come from the
  newest-released chapter, with `MAX` only as a tiebreak.
* `chapter_number = 0` is legitimate (`Chapter 0`, `Prologue`, `Preview`) and **`-1` is the Oneshot
  sentinel** — a `<= 0` filter would drop valid chapters.

### 4.6 F5 — Feed staleness, and the write-lock ceiling that constrains the fix [M/C]

`refresh_feed_updates` has exactly two call sites [C]: `main.rs:1284` (boot) and `mangadex.rs:2761`
(after each catalogue sync). At `CATALOGUE_SYNC_INTERVAL_SECS=21600` that is **once per 6 hours**.
The Suwayomi half has an incremental writer (`scanner::touch_feed_series_update`, `scanner.rs:1026`);
**the MangaDex mirror half has none.** A chapter mirrored five minutes ago is invisible for up to 6 h.

`run_chapter_cycle` never got its own timer. `mangadex.rs:2618-2625` says it should:

> this half can be driven on a much tighter schedule than `CATALOGUE_SYNC_INTERVAL_SECS` (6h, which
> is why a brand-new chapter can surface already labelled "5h ago")

**The obvious fix is unsafe on its own.** `catalog/mod.rs:936-942` documents its own measurements:

> the transaction above is already **~12.0 s of SQL** (`DELETE` 4.7 s + mirror INSERT 3.1 s +
> `en_chapter_count` 3.3 s + the rest) plus ~0.6 s of type fill and a ~0.5 s commit, and **Browse's
> rebuild adds ~6 s**. One shared transaction would therefore hold the write lock for ~19 s against
> the pool's **15 s `busy_timeout`** … concurrent writes stop being a wait and start being
> `SQLITE_BUSY` failures.

`refresh_work_fts` (FTS5 over all 115,656 works) adds a third transaction on top. `db.rs:52`
confirms `busy_timeout(15s)`. The chain is split into three transactions *specifically* to keep each
under the ceiling.

So dropping the cycle to 15 min fires a **~20–25 s lock chain 96×/day instead of 4×** — a ~2.5%
write-lock duty cycle, with every scanner write in those windows queueing against a 15 s timeout and
the WAL growing accordingly. **Incremental feed maintenance is a hard prerequisite for the tighter
timer, not a follow-up optimisation.**

**Sub-finding: 369 cards are present but stale [M].** Verified directly — of the 1,947 series the
scanner has ever recorded a detection for, **0 are absent from the feed**. So "updated but missing"
is fully explained by F3 (never detected), F5 (staleness) and F1 (the 2037 sentinel); there is no
fourth cause. But there is a *fourth symptom*: **369 feed rows have a `released_at` more than an hour
behind their series' actual `suwayomi_series.latest_chapter_at`** — the card exists but sorts too low
to be seen.

Cause: `touch_feed_series_update` is gated on `new_found` (`scanner.rs:1027`,
`if !new_found { return; }`). Its own doc comment (`scanner.rs:1009-1021`) lists what the gate
misses, including "an upstream edit to an EXISTING chapter's `upload_date`". When `latest_chapter_at`
advances **without a new chapter id appearing** in the scanner's id-set diff, the incremental writer
never fires and the row stays stale until the next 6 h rebuild. The gap was documented but never
quantified; it is 369 series, ~3% of the 11,802 in-library series that have a `latest_chapter_at`.

Phase C's ledger removes this by construction — `released_at` derives from chapter data rather than
from a detection event.

### 4.7 F6 — No status- or format-aware cadence [M/C]

Status feeds exactly one binary: `paused_for_status()` (`graphql/types.rs:897`) → park 14 days.
Everything else is an inferred publication average clamped to
`[MIN_INTERVAL_HOURS = 6, ACTIVE_MAX_INTERVAL_HOURS = 12]` (`scanner.rs:114-121`).

**The effective cadence today is a flat 12 h, not the 6 h the floor suggests.** This is a common
misreading of the config, so it is worth stating precisely. `resolve_interval`
(`scanner.rs:142-153`) computes `clamp(inferred_avg, 6, 12)`, and when there is no inferred cadence
it substitutes `DEFAULT_INTERVAL_HOURS`, which is **defined as `ACTIVE_MAX_INTERVAL_HOURS = 12.0`**
(`scanner.rs:66`) — the ceiling, not the floor. Measured distribution of `avg_interval_hours` across
all 14,098 scan-state rows [M]:

| Inferred cadence | Series | Effective interval |
|---|---|---|
| **> 12 h** | **9,921 (70%)** | capped **down** to 12 h |
| **0 — no cadence data** | **3,307 (23%)** | defaults to 12 h |
| < 6 h | 775 (5.5%) | floored **up** to 6 h |
| 6–12 h | **95 (0.7%)** | used as inferred |

So **93% of series land on exactly 12 h**; the 6 h floor binds on 5.5%, and only 0.7% use a genuinely
inferred value. The scheduled-gap histogram confirms it — of 6,531 ongoing/unknown series, **4,956
sit in the 11–13 h bucket** (12 h + jitter), 655 in 6–11 h, 215 in 1–6 h, and 173 under 1 h (the
accelerated `awaiting` poll). **Median scheduled gap: 11.94 h.**

Phase E therefore replaces a de-facto flat 12 h, and reaching the 3 h tier requires bypassing **both**
the 6 h floor and the fact that the no-data default *is* the ceiling.

Measured `AVG(next_scan_at − last_scanned_at)` by status, in-library series:

| Status | Series | Measured today | Target |
|---|---|---|---|
| ONGOING | 6,392 | **11.3 h** | 3 h / 24 h / 12 h by format+recency |
| PUBLISHING_FINISHED | 3,386 | 346.1 h | none |
| COMPLETED | 3,191 | 352.2 h | none |
| ON_HIATUS | 558 | 347.5 h | 72 h |
| CANCELLED | 432 | 342.1 h | none, or 72 h on source disagreement |
| UNKNOWN | 139 | 11.9 h | treat as ONGOING |

### 4.8 F7 — Later sources re-float already-reported chapters [C]

`catalog/mod.rs:837`: `released_at = MAX(feed_series_updates.released_at, excluded.released_at)`.
When a second source mirrors chapter N that a first source already reported, the card's clock moves
forward and it jumps back to the top of `/updates`. First-source-wins is not expressible in the
current schema.

### 4.9 F8 — `all.mangadex` is 74% of the Suwayomi library [M]

**10,422 of 14,103** Suwayomi `source_series` rows come from `all.mangadex`, across **62 source ids**
(the extension registers one per language). **9,926** of those works already carry a direct-API
MangaDex anchor — pure duplication, consuming the shared Suwayomi scan budget and minting duplicate
`w_` rows (1,455 titles are held by more than one work).

**496 rows / 463 works have no MangaDex anchor and no other Suwayomi source.** A blind delete
orphans them. The MangaDex UUID is **not stored locally** — `suwayomi_series` has no `url` column;
the UUID exists only in Suwayomi's `MangaType.url` (`/manga/<uuid>`), which the client already
fetches (`suwayomi.rs:188`) and we discard.

**Verdict:** keep the extension *installed* as a read fallback for works whose direct spine is
empty, keep `isRedundantMangadexExt()` (`translator-select.ts:25`), but stop syncing it and delete
only rows whose work is *proven* to have a direct anchor afterwards. The direct API is irreplaceable
regardless: only it provides `createdAt` windowing, `links` (AniList/MAL), full `altTitles`, tags,
`year`, `content_rating`, and page images off `*.mangadex.network` instead of our origin.

### 4.10 F9 — Latent: the firehose cursor advances past uncatalogued works [C]

`mangadex.rs:2334-2337` assigns `last_created` **before** the skip at 2352:

```rust
for c in &chapters {
    if let Some(ts) = chapter_window_ts(c, window) { last_created = Some(ts); }   // ← cursor moves
    ...
    let ssid = match catalog::find_source_series_id(pool, "mangadex", "mangadex", &manga_id).await {
        Ok(Some(id)) => id,
        Ok(None) => continue,          // ← chapter dropped, cursor already advanced
```

The incremental window is forward-only `updatedAtSince`, so a chapter skipped this way is never
re-offered. Compounding it, `mangadex.rs:1403-1406` counts catalogue upsert failures as `warn!` with
**no `out.failed += 1`**, so a work lost to `SQLITE_BUSY` never blocks `seed_done` — unlike the
chapter sweep, which does gate on it (`chapter_seed_may_latch`, `mangadex.rs:2614`).

**Currently firing on 0 chapters** [M] — the catalogue is complete, so `find_source_series_id`
always resolves. It is a permanent, silent loss the moment it does fire. Cheap to fix; fix it while
in the area.

### 4.11 F10 — "Completed = no scans" would destroy the only reopen-detection path [M/C]

**Not in the original report. This finding changed a spec item.**

If a COMPLETED series is never fetched again, nothing can notice it **reopening**
(COMPLETED → ONGOING — revivals, sequels published under the same entry, a source correcting a bad
status). That is only safe if some other job refreshes `status`.

**Nothing does.** The daily source sync explicitly does not: its own header (`sync.rs:1-21`) scopes
it to *reconcile* (backfill `series_scan_state` rows, re-assert `inLibrary=true`) and *discover*
(walk LATEST, auto-enrol series we do not have). It applies metadata only through
`ingest_source_series`, i.e. **only for series we do not already have**. Existing series are never
refreshed.

**The actual reopen path is the scanner's own 14-day park, and the current behaviour is a
deliberate, documented decision that already rejected exactly what the spec asked for**
(`scanner.rs:750-762`):

> A single combined upstream fetch (fresh status + chapters), then record the scan —
> *unconditionally*, even for a paused series. […] it refreshes status so an upstream **reopen
> (COMPLETED → ONGOING) auto-resumes scanning** without waiting for an admin. […] Net cost of a
> steady paused series is thus one fetch per park window (~14d) […] **Zero-cost "never fetch paused"
> was rejected**: it reintroduces the 0-chapter bug and loses reopen detection.

Blast radius [M]. MangaDex-anchored works are safe — `to_work_input` sets
`status: map_status(…)` (`mangadex.rs:1048`) and the catalogue sweep refreshes it every cycle.

| Cohort | Paused series | If "no scans" ships naively |
|---|---|---|
| Has a MangaDex anchor | 6,969 | Safe — catalogue sweep refreshes status |
| **Suwayomi-only** | **598** | **Permanently blind** |

Cost of the status quo being protected: paused series at a ~14-day park are **~540 scans/day, 3.6%**
of scan budget. The resolution is §7-E2 — replace the poll with a trigger, not with nothing.

### 4.12 F11 — A broken source reports as perfectly healthy [M/C]

**Not in the original report. Found by chasing a single HTTP 404. This is a silent-failure class, and
it invalidates `failed = 0` as evidence of health.**

**How it was found.** The per-source probe (E3.5) showed `en.suryascans` returning HTTP 404. The
source had *rebranded* — the extension now presents as **"Genz Toons"** at **`genztoons.org`**
(confirmed via `realUrl`), and the site is live: `/` and individual series paths return **200** to
curl. But `fetchMangaAndChapters` 404s for **every** series probed, including ones whose pages return
200 — the series pages exist, the extension's chapter-list endpoint does not. Consistent with a
platform change during the rebrand against extension v1.4.54.

**What the scan state claimed, minutes before the snapshot:**

| suryascans / Genz Toons | Value |
|---|---|
| Series | 209 (122 ONGOING, 85 UNKNOWN, 2 COMPLETED) |
| `consecutive_failures > 0` | **0** |
| Last scanned | 2026-07-30T06:17 |
| Series with any failures **across the whole 14,098-series library** | **0** |

Zero failures library-wide is not plausible. The cause is `SuwayomiClient::chapters`
(`suwayomi.rs:748-770`):

```rust
match self.gql::<FetchData>(&fetch, ...).await {          // live upstream fetch
    Ok(d)  => Ok(parse_records(...)),
    Err(e) => {
        if e.to_string().contains("No chapters") { return Ok(vec![]); }
        // fallback: plain `chapters(condition: { mangaId: $id })` QUERY
        //           → reads Suwayomi's LOCAL DB CACHE, not upstream
```

`series_and_chapters` (`suwayomi.rs:804-815`) wraps the same pattern. So the 404 fails the mutation,
the fallback silently returns the **cached** chapter list, the call returns `Ok`, and `persist_scan`
sees an unchanged id-set. Net effect: `new_found = false`, `consecutive_failures = 0`,
`next_scan_at` advanced on the normal cadence, admin console green.

**⇒ "source is healthy with no new chapters" and "source is completely broken" are indistinguishable,
permanently and by design.**

The fallback is *correct* for the reader path — never hard-fail a user over a transient upstream
blip. It is wrong for the scanner, whose entire job is detecting change. It also means the
`failed = 0` I cited across 25 ticks as evidence of a healthy scanner proves much less than it
appeared to.

**Aftermath of the 2026-07-30 uninstall.** The source was uninstalled once diagnosed. Consequences to
handle:

* Uninstalling the extension does **not** delete `source_series` rows. **53 works are reachable only
  via suryascans** and become unreadable (117 also have a MangaDex anchor, 111 another Suwayomi
  source, so those are safe). 0 of the 53 are in `feed_series_updates` today — consistent with F3.
* Because of F11 itself, those 209 `series_scan_state` rows may keep *succeeding* off cache rather
  than failing and backing off. They need explicit reconciliation.
* As of this writing Suwayomi still lists the source among its **129 installed sources** — worth
  confirming the uninstall propagated, versus only the subscription being removed.
* Note only **15 of 129** installed sources have any in-library series.

### 4.13 F12 — The source picker defaults by chapter COUNT, not by recency [C]

**Same count-vs-number confusion as F4, in a different place — and it contradicts an explicit owner
requirement.**

`pickDefaultKey` (`apps/reader/src/lib/data/translator-select.ts:50-59`):

```ts
const byMostChapters = [...eligible].sort((a, b) => b.chapterCount - a.chapterCount)[0];
return (preferred ?? byMostChapters ?? eligible[0] ?? ordered[0])?.key;
```

The default source is whichever has **the most chapters**. The owner's requirement, clarified
2026-07-30, is the source that is **furthest ahead — highest chapter NUMBER**, regardless of how many
chapters it carries:

> *"some sources pick up a series from midway. Series Y has 151 chapters, source X has 150 translated,
> but another source picked it up halfway and has 70 chapters — but their most recent chapter is
> ch 151. That's what I meant by most updated chapter."*

**Rank by `MAX(chapter_number)` per source, NOT by count, and NOT by upload recency.** Note this is
also subtly different from "who published the newest chapter first" — that is the F7 rule for the
updates feed. Here the question is "which source can I read the furthest in", which is a property of
the source's chapter *range*.

**This is real, not hypothetical [M].** 50 multi-source works have a furthest-ahead source that is not
their most-chapters source:

| Work | Most-chapters source | Furthest-ahead source |
|---|---|---|
| The Immortal Emperor Luo Wuji Has Returned | 130 ch (max 130) | **46 ch (max 199)** |
| The Reincarnated Armed Escort | 91 ch (max 91) | **62 ch (max 153)** |
| Max Level Returner | 118 ch (max 117) | **57 ch (max 174)** |
| Against the Gods | 312 ch (max 700) | **13 ch (max 758)** |

So today the picker defaults to a source **69 chapters behind** on the first of those.

**Trap the naive rule falls into [M].** Ranking purely by `MAX` picks garbage on mis-merged works:
*One Piece (Official Colored)* has a 764-chapter source (max 763) and a **1-chapter** source at
max 1183 — almost certainly a bad dedup. Of the 50 cases, **4 have a winner with < 3 chapters** and
**10 have a winner with < 10% of the leader's count**; 36 are genuine partial pickups (≥ 25%).

**Rule to implement:**

1. Exclude `redundant` sources (existing `isRedundantMangadexExt` behaviour).
2. **Guard against mis-merges:** ignore candidates with fewer than 3 chapters *or* under ~10% of the
   leading source's chapter count.
3. Among survivors, pick the highest `MAX(chapter_number)`.
4. Tiebreak on who had that chapter first (`release_event.first_seen_at`) — reuses the F7 ledger.
5. A saved `preferredKey` **always** wins over the computed default.

**Data needed.** `Selectable` carries `chapterCount` but no max-chapter and no recency, so this cannot
be fixed in the picker alone. Per-source `MAX(chapter_key)` comes straight from the unified `chapter`
spine after **Phase B**; the tiebreak comes from **Phase C**'s ledger. Implement in C, not as a
separate mechanism.

**Verified safe: selecting a midway source does NOT hide earlier chapters.**
`buildAggregatedChapters` (`source.ts:1537-1584`) lists every aggregated chapter regardless of the
selected source, falling back per-chapter via `pickSource` to any source that carries it. The list
stays complete.

**But there is a real cosmetic cost, and it is not small.** For chapters the selected source lacks,
`date` is deliberately left `''` because `AggregatedChapter` exposes no upload timestamp
(`source.ts:1565-1571`) — so those rows render with **no date**. Median chapters the furthest-ahead
source lacks: **50** (max 1,182). Picking a partial source therefore strips the date off most rows on
exactly the works this rule targets. **Phase B closes this**: once Suwayomi chapters live in the
canonical `chapter` table with `released_at`, the aggregation can carry a real timestamp for every
chapter. **Ship F12 with or after B**, not before, or the fix trades a wrong default for a
date-less chapter list.

### 4.14 Things that are NOT broken [M]

Recorded so nobody re-investigates them:

* **`work_redirect`** — 0 stale rows across `feed_updates`, `feed_series_updates` and
  `browse_catalogue`, and 0 `reader_id`s pointing at a merged-away work.
* **NSFW over-flagging** — **2** safe/suggestive works remain flagged (migration 0053 fixed 2,541
  and is holding); Naruto and One Piece are `is_nsfw = 0`. What *is* true: **30% of the feed
  (19,447 of 64,902) is legitimately erotica/pornographic**, so an anonymous visitor correctly loses
  about a third of `/updates`. A residue of 216 works has `content_rating IS NULL` + `is_nsfw = 1`.
* **`updatesFeed` is not library-filtered** (`graphql/mod.rs:2947`) — global feed, gated only on
  `is_nsfw` and optional `comic_type`. No per-user surprise.
* **`suwayomi_chapter` is fresh and pruned** — `series_cache::put_chapters` (`series_cache.rs:287`)
  rewrites a series' rows wholesale (`DELETE` at 385, re-`INSERT` at 397), guarded by a
  `ChapterDigestRow` equality check (line 107) that skips the write when nothing changed. Stale rows
  do not accumulate.
* **The `strftime` guard drops 0 rows**; `feed_updates.latest_at` parses cleanly.
* **`in_library = 1`** is a no-op filter today — all 14,111 `suwayomi_series` rows have it set.

---

## 5. Root cause

One structural fact explains F3, F4, F5, F7 and F8 together.

There are **two parallel, incompatible update pipelines**, reconciled late by a materialized view:

| | MangaDex | Suwayomi |
|---|---|---|
| Chapter storage | `chapter` — per-chapter canonical rows | `series_scan_state` — an id-set *change-detection artifact*, plus a separate `suwayomi_chapter` cache |
| Reaches the feed via | `feed_updates` (grouped) | `last_new_chapter_at` (a detection flag) |
| Release clock | ISO-8601 TEXT | 13-digit epoch-millis TEXT |
| Chapter number | available | **not carried into the feed** |

`chapter` holds **0 Suwayomi rows out of 877,824** [M]. Because the Suwayomi half stores a *detection
event* rather than *chapters*:

* the feed cannot name a chapter number → the count hack (F4);
* a series with chapters but no detection is invisible (F3);
* first-source-wins is inexpressible (F7);
* two incompatible time encodings had to be reconciled inside the schema (migration 0064's central
  complaint);
* there are two `reader_id` rules, two cover paths and two NSFW derivations to keep in sync.

**Critically, the union already exists on the read path.** `catalog::work_source_chapters`
(`catalog/mod.rs:2941`) unions authoritative Suwayomi caches with the MangaDex mirror, and
`group_aggregated_chapters` (`graphql/mod.rs:1430`) dedupes them by a cross-source key. The updates
feed simply does not use it. **We are not inventing an architecture — we are materializing one the
series-detail page has been proving in production all along.**

---

## 6. Target architecture

```
any source ──emits──▶ ChapterObservation {
                         source_series_id, external_id, raw_number, raw_title,
                         released_at (epoch ms), external_url
                      }
                                 │
                                 ▼
                          chapter            ← ONE canonical spine for all sources
                                               + readable_at, external_url, chapter_key
                                 │
                                 │  INSERT OR IGNORE on first sighting of (work_id, chapter_key)
                                 ▼
                        release_event         ← THE ledger: first-source-wins is a PRIMARY KEY
                                 │
                                 ▼
                     feed_series_updates      ← thin projection, incrementally maintained
                                 │
                                 ▼
                              /updates
```

### 6.1 Reuse the existing chapter key — do **not** invent one

`chapter_override.chapter_key` (migration 0032) is already defined as **`round(number * 100)` as
text**, and `graphql::chapter_key()` (`graphql/mod.rs:1462`) is the canonical helper, matching
`group_aggregated_chapters` (`graphql/mod.rs:1434`).

An earlier draft of this plan proposed `round(number * 1000)`. **That would have broken the admin
override layer** — hiding a spam chapter 10.5 across sources would silently stop matching. Validated
on real data [M]: only **31 Suwayomi rows and 41 MangaDex rows** lose any precision at 2 decimal
places, out of 1.44 M. The existing key is correct; reuse it verbatim.

Non-numeric chapters (F2) get their own key namespace derived from `external_id`, so each oneshot is
a distinct event rather than colliding on `0`.

### 6.2 Why the ledger is strictly better than patching the two halves

1. **First-source-wins becomes `PRIMARY KEY (work_id, chapter_key)` + `INSERT OR IGNORE`** — a
   constraint, not a `MAX()` a later source can defeat. Zero query cost, impossible to regress.
2. **The chapter number is first-class on both sources.** F4 dies at the schema level instead of
   being patched per surface.
3. **"Check every source of a merged series" is satisfied structurally** — each source writes its own
   chapters; the ledger dedupes by key. The requirement needs no special-casing.
4. **One clock.** Epoch-millis INTEGER stored once at ingest; the ISO-vs-millis reconciliation
   disappears.
5. **External chapters are a column, not a sentinel** — `external_url IS NOT NULL` → redirect.
6. **Incremental by construction.** One chapter insert ⇒ ≤1 ledger row ⇒ 1 feed upsert. The ~20–25 s
   wholesale chain demotes from *the* write path to a periodic **drift reconciler**.

### 6.3 Sizing [M]

Distinct `(work_id, chapter_key)` pairs: **813,053** MangaDex + **494,742** Suwayomi ≈ **1.31 M**
before cross-source overlap. Same order as `chapter` itself (877,824). Acceptable for SQLite with a
covering index on `(work_id, chapter_key)` and one on `first_seen_at DESC`.

### 6.4 Release-clock rule (precise, because §4.2 showed the naive version is wrong)

```
released_at := readable_at   when present        -- MangaDex `readableAt`
            := published_at  otherwise           -- Suwayomi upload_date, or pre-backfill rows
is_released  := released_at <= now
```

`publishAt` is **scheduling metadata only** and must never be a release clock — it is a 2037 sentinel
on external chapters and can post-date `readableAt` by weeks. Sort and bound on `readable_at`, which
is what MangaDex's own latest-updates surface orders by.

---

## 7. Implementation plan

Seven phases.

| Phase | Fixes | Depends on | Shippable alone? |
|---|---|---|---|
| **A** Chapter-number contract + external chapters | F1, F2, F4 | — | **yes** |
| **B** Unify the chapter spine | (enables C) | — | yes (lands dark) |
| **C** Release ledger + incremental feed | F7 | B | no |
| **D** Tighten the chapter cycle | F5, F9 | **C** | no |
| **E** Cadence engine | F6 | E2 for its "none" rows | partially |
| **E2** Trigger-based reopen detection | F10 | — | **yes** |
| **E3** Per-source scanners + dynamic supervision | source isolation | — | **yes** |
| **E4** Scan-failure honesty | **F11** | — | **yes — do this first** |
| **E5** LATEST-diff discovery | 99% wasted fetches | E2 proves the mechanism | yes — **evaluate before building E** |
| **F** `all.mangadex` retirement | F8 | **C** | no |

**B → C → D** is a strict chain (D is unsafe without C's incremental writer — see §4.6).
**E2 must land before E's three "none" rows** (see F10). **F** is destructive and must follow **C**
so the ledger's first-seen history survives the `ON DELETE CASCADE` (§7-C1).

Recommended order: **E4 first** (until scan failures are honest, no health signal can validate any
later phase) → **A** (biggest user-visible win, no risk) → **E2** (cheap, unblocks E) →
**B → C → D** → **E** + **E3** → **F**. E3 is independent but pairs naturally with E, since both
touch the scheduler.

### Phase A — Chapter-number contract + external chapters
*Pure read-path. No schema migration on hot tables, no lock-profile change. Fixes F1, F2, F4.*

**A1. Parse what we already receive.** Add `readable_at`, `external_url`, `pages` to
`MdChapterAttrs` (`mangadex.rs:876`). Migration: add `chapter.readable_at`, `chapter.external_url`.
Backfill `readable_at` from `published_at` for existing rows (correct for all non-external chapters);
backfill `external_url` by re-walking the mirror once against `/chapter?ids[]=` in batches of 100.

**A2. One canonical display helper**, replacing every ad-hoc label:

```
chapter_display(number: Option<f64>, raw: Option<&str>, name: Option<&str>) -> ChapterLabel
```

* MangaDex: `attributes.chapter` when numeric.
* Suwayomi: `suwayomi_chapter.chapter_number` when `> 0` and `< SANE_MAX`.
* Fallback (~0.15% of rows): parse the leading number out of `name` —
  `Chapter 45`, `Chapter 45: Beginning of the End`, `Ch. 45`, `Vol.4 Ch.16`, bare `45`.
  Must take the **first** number, not the largest — `Chapter 45: The 100 Kings` is chapter 45.
* `chapter_number == -1` or an unparseable non-numeric label → `ChapterLabel::Oneshot`.
* Clamp: reject `> SANE_MAX` (proposed 5,000) and fall back to the next-best candidate. Guards
  `Ch.99999999` and `Ch.20240120`.
* **Never a count.**

**A3. Latest-chapter rule.** The label is the number of the **newest-released** chapter, not
`MAX(number)` — they disagree on 10% of series. `MAX` is the tiebreak within one release timestamp.

**A4. Oneshot bucket (F2).** Replace the `c.number GLOB '*[0-9]*'` exclusion in
`work_source_chapters` (`catalog/mod.rs:2978`) with a non-numeric branch keyed by `external_id`, so
21,422 oneshot works get a clickable chapter list and a real label. Keep the `CAST(… AS REAL) = 0.0`
guard the comment describes — the bug is the *exclusion*, not the guard.

**A5. External chapters (F1).** Where `external_url IS NOT NULL`: the reader redirects out instead of
requesting pages, and the chapter is badged in the list. This is the blank-reader fix.

**A6. Reader cleanup.** Delete the `?? chapterCount` fallback (`source.ts:300-305`) and the
unconditional `Ch. ${s.chapterCount}` (`source.ts:243`). Leave the `read / total` progress ratios
alone.

**Exit criteria:** no surface renders a count as a number; the 46 sentinel works appear in `/updates`;
oneshot works have a chapter list; external chapters redirect.

### Phase B — Unify the chapter spine
*Additive. Nothing reads the new rows yet, so it is safe to land dark.*

**B1.** Write Suwayomi chapters into canonical `chapter` alongside MangaDex, from
`series_cache::put_chapters`. Requires a `source_series_id` per Suwayomi series (already exists).
**B2.** Add `chapter.chapter_key` (`round(number*100)`, per §6.1) and index it.
**B3.** Backfill 563,095 `suwayomi_chapter` rows. Chunked, off the hot path, resumable.
**B4.** Keep `suwayomi_chapter` as the live Suwayomi cache — it also serves read state and page
counts. `chapter` becomes the *canonical* spine, not the only copy.

**Exit criteria:** `chapter` contains both sources; `work_source_chapters` can be re-expressed as a
single query over it (verified to return identical results to today's two-branch version).

### Phase C — Release ledger + incremental feed maintenance
*The prerequisite for Phase D. Fixes F7.*

**C1.** `release_event(work_id, chapter_key, first_seen_at INTEGER, first_source_series_id TEXT,
label TEXT, PRIMARY KEY (work_id, chapter_key))`.

* **Must NOT cascade from `source_series`.** `chapter.source_series_id` is
  `ON DELETE CASCADE` and `db.rs:36` sets `.foreign_keys(true)`, so Phase F's deletion of 10,422 rows
  would otherwise cascade away the first-seen history. Hold `first_source_series_id` as a nullable,
  non-enforcing reference.
* **Seed from `min(released_at)` per `(work_id, chapter_key)` — never `now()`.** Seeding with the
  current time dumps the entire back catalogue onto page 1 of `/updates`. **This is the single worst
  deployment hazard in the plan.**
* **Merge-aware.** Dedup collapsing work B into work A must keep the *earliest* `first_seen_at` per
  key, or every merge re-floods the feed.

**C1b. Source auto-selection (F12).** Replace `byMostChapters` in `pickDefaultKey`
(`translator-select.ts:50-59`) with **highest `MAX(chapter_key)` per source** — read from the unified
`chapter` spine (Phase B) — plus the mis-merge guard (drop candidates under 3 chapters or under ~10% of
the leading source's count) and a `release_event.first_seen_at` tiebreak. `redundant` filtering and a
saved `preferredKey` both still apply. **Requires Phase B first**, or the selected partial source
leaves a median of 50 chapter rows with no date. See F12.

**C2.** Incremental writers for the MangaDex half of `feed_series_updates`, mirroring
`scanner::touch_feed_series_update`. Extend the existing convergence proof
(`incremental_write_converges_with_the_periodic_rebuild`, `scanner.rs:3388`) to cover it — that test
already pins the incremental writer to the rebuild's exact field mapping.

**C3.** Demote the wholesale chain to a **drift reconciler**: run it rarely (daily), and have it
*report* divergence rather than being the mechanism that keeps the feed correct.

**Exit criteria:** a new chapter reaches `/updates` without a full rebuild; the reconciler reports
zero drift over 24 h; a second source mirroring an existing chapter does not re-float the card.

### Phase D — Tighten the chapter cycle
*Only after C. Fixes F5.*

Give `run_chapter_cycle` its own interval (~15 min) decoupled from `CATALOGUE_SYNC_INTERVAL_SECS`.
Also fix **F9** here: move the `last_created` assignment *after* the anchor lookup, and add
`out.failed += 1` to the catalogue upsert error arm (`mangadex.rs:1405`).

**Exit criteria:** median feed lag < 20 min; no increase in `SQLITE_BUSY`; WAL size stable.

### Phase E — Cadence engine
*Independent of A–D. Fixes F6.*

One policy function replacing the ad-hoc `resolve_interval` path:

```rust
fn scan_interval_hours(
    status: SeriesStatus,               // effective, admin override applied
    sibling_statuses: &[SeriesStatus],  // every source of the same work
    comic_type: ComicType,
    last_release: Option<DateTime<Utc>>,
    admin: &ScanAdmin,
) -> Option<f64>                        // None = do not scan
```

| Condition | Interval |
|---|---|
| `admin.override_interval_hours` set | that value, clamped `>= 1 h` |
| COMPLETED / PUBLISHING_FINISHED | **none — trigger-driven, see E2** |
| CANCELLED, all sources agree | **none — trigger-driven, see E2** |
| CANCELLED, sources disagree | 72 h |
| HIATUS / ON_HIATUS | **none — trigger-driven, see E2** |
| ONGOING/UNKNOWN · MANHWA\|MANHUA · released ≤ 14 d | **3 h** |
| ONGOING/UNKNOWN · MANHWA\|MANHUA · dormant | 24 h |
| ONGOING/UNKNOWN · MANGA | 12 h |

The three "none" rows are only safe because of E2. **Do not ship them without it** — see F10.

**Modelled steady-state cost: ~15,164 scans/day of tier-driven polling.** This is *not* the whole
picture — see the capacity budget below, which measures actual production load at **~24,200/day**
(the difference is the `awaiting` accelerated poll). Net of `awaiting` re-scoping and E2, the proposal
comes out **cheaper than today**. Dropping the completed-park
(6,577 series at ~352 h ≈ 450/day) makes it net-neutral. Format mix of the 6,531 ongoing/unknown
in-library series: 3,723 MANGA · 532 MANHWA-active · 138 MANHUA-active · 1,650 MANHWA-dormant ·
488 MANHUA-dormant.

**The interval is a TARGET, not a floor — decided 2026-07-30.** `scan_interval_hours` returns the
policy value **verbatim**. The `clamp(inferred_avg, MIN_INTERVAL_HOURS, ACTIVE_MAX_INTERVAL_HOURS)`
path is deleted for policy-driven series, because as §4.7 shows it is what produced a de-facto flat
12 h. Concretely:

* `DEFAULT_INTERVAL_HOURS` (currently *defined as the ceiling*, `scanner.rs:66`) becomes unreachable
  — there is always a policy answer, so there is never a "no data" fallback to make.
* `MIN_INTERVAL_HOURS = 6.0` and `ACTIVE_MAX_INTERVAL_HOURS = 12.0` **stop being policy**. Retain
  only `HARD_MIN_INTERVAL_HOURS = 1.0` and `MAX_INTERVAL_HOURS` as absurdity rails.
* `avg_interval_hours` keeps being computed for admin display and diagnostics, but **no longer
  schedules anything**.

Four things must survive the change:

1. **Jitter is mandatory, not optional — and should become per-tier.** 670 series on an exact 3 h
   target with no spread come due in the same instant. `jitter_interval_hours`
   (`SCHEDULE_JITTER_FRACTION = 0.10`, uniform ±10%, `scanner.rs:157/185`) stays.

   But note what ±10% *means* per tier, because it dominates every other source of variance:

   | Tier | Target | ±10% jitter band | Tick lag (E3: 60 s) |
   |---|---|---|---|
   | Manhwa/manhua active | 3 h | **2 h 42 m – 3 h 18 m** | ≤ 1 min |
   | Manga | 12 h | 10 h 48 m – 13 h 12 m | ≤ 1 min |
   | Manhwa/manhua dormant | 24 h | 21 h 36 m – 26 h 24 m | ≤ 1 min |

   So a "3 h" series really lands anywhere in a **36-minute window** — 36× the scheduling lag. Make
   `SCHEDULE_JITTER_FRACTION` **per-tier**, e.g. 5% on the 3 h tier (±9 min) and 10% on the slower
   ones. **Do not set it to zero on any tier**: `scanner.rs:164-167` records that without jitter a
   self-sustaining cohort of ~745 series arrived every 35 minutes, drove the scanner to a 43% duty
   cycle and Suwayomi to **154 GB of egress**, while the rest of the catalogue starved. Below about
   ±5% the tick granularity makes it meaningless anyway.
2. **Admin override still wins**, clamped `>= 1 h`.
3. **Error backoff still overrides** (exponential, capped at `ERROR_BACKOFF_MAX_HOURS = 24`). A target
   must not defeat backoff on a dead source.
4. **The `awaiting` accelerated poll needs re-scoping.** It re-polls a "genuinely late" series every
   30 min (`resolve_poll_minutes`). Against a 12 h/24 h target that is still worth having; against the
   3 h tier it adds 6× load for marginal gain. Recommend: **disable `awaiting` for the 3 h tier**,
   keep it for the slower ones.

**New failure mode a hard target introduces:** achieved interval = target **+ queue delay**. If demand
exceeds drain capacity the target silently becomes a lie. Must be monitored — see the capacity
budget below, and alert when `due == DUE_BATCH_LIMIT` on consecutive ticks.

Remaining constraints [C]:

* **`MIN_INTERVAL_HOURS = 6.0` currently floors everything** (`scanner.rs:121`), so 3 h is
  unreachable until the target change above lands. It exists to stop a same-day upload burst
  inferring an absurd cadence — a concern that disappears once inference no longer schedules.
* **"Never scan" must not be `next_scan_at = NULL`.** The due-query is a bounded
  `next_scan_at <= ?` index seek and `enrol_paths_never_leave_null_next_scan_at` (`scanner.rs:2882`)
  asserts no enrolled row is NULL. Park far out instead.
* **Prove the COMPLETED→ONGOING reopen path before shipping "no scans".** The daily source sync is
  the only thing that would ever unpark a resumed series (`scanner.rs:754-756` claims it refreshes
  status). If that claim is wrong, a reopened series is dead to us permanently. **This is a blocking
  pre-check, not a nice-to-have.**
* **Policy inputs are per-*work*; scheduling is per-*Suwayomi-series*.** `status`, `comic_type` and
  sibling statuses live on `work`, but the due-query must stay a pure index seek. So the **resolved
  interval is materialized onto `series_scan_state`** by a cheap daily pass, not joined at scan time.
  The dormancy input crosses the 14-day boundary at most once per transition, so daily is sufficient.

#### Capacity budget — is the target reachable? [M/C]

**Scanner topology.** There is exactly **one** scan loop, not a pool. `spawn` (`scanner.rs:1631`)
starts a single supervised `run_loop`; `interval` uses `MissedTickBehavior::Delay`
(`scanner.rs:1674`) so **ticks never overlap** — an overrunning tick delays the next rather than
running concurrently with it. Inside one tick, `tick()` pulls up to `DUE_BATCH_LIMIT = 1000` due
series and processes them with **`for_each_concurrent(SCAN_CONCURRENCY = 3)`**
(`scanner.rs:625`) — so **3 scans in flight at a time**, not one-by-one and not unbounded. An outer
drain loop (`scanner.rs:1689`) repeats `tick()` back-to-back until a batch returns short, so a
backlog is worked off without idling a full interval between batches.

The concurrency is deliberately small because **each scan is a live upstream fetch, not a cached
read**: `series_and_chapters` (`suwayomi.rs:786-792`) issues
`fetchMangaAndChapters(fetchManga: true, fetchChapters: true)` — a *mutation* that forces Suwayomi to
go out to the scanlator site, potentially via FlareSolverr (which stalls, hence the 30 s timeout).

**"3 at a time" is a concurrency cap, not a rate.** The composed schedule is:

```
every SCAN_TICK_SECONDS (= 300 s in production):
    tick():  select <= 1000 due series, run them 3-concurrently until exhausted
    if the batch came back full → immediately tick() again (drain)
    else → idle until the next 300 s boundary
```

**Measured in production, 2026-07-30 (25 consecutive ticks over 2 h of `docker logs`):**

| Metric | Value |
|---|---|
| Tick interval | ~300 s (observed completions 265–341 s apart) |
| Due series per tick (`overdue`) | 71–104, **mean ≈ 84** |
| `ok` / `failed` | 84 / **0** — every tick drains fully, no backlog, no trend |
| Lock contention (`database is locked`) in 2 h | **0** |

So **actual throughput ≈ 84 per 300 s = 0.28 scans/s ≈ 24,200 scans/day.**

**Per-scan latency, measured directly against the live engine** (n = 3, real `fetchMangaAndChapters`
calls): **0.68 s / 3.89 s / 7.35 s**, median **3.89 s**. Latency scales with chapter count (the
7.35 s series has 686 chapters). Small sample — treat as ~2–5 s with wide error bars.

⇒ **Capacity at concurrency 3 ≈ 0.77 scans/s ≈ 2,777/hour ≈ 66,600/day**, and a tick's working set of
84 takes **~109 s of the 300 s window**. **Utilisation ≈ 36%** (busy ~109 s, idle ~190 s).

**Two corrections to earlier drafts of this document:**

1. **Today's real load is ~24,200 scans/day, not the ~13,871 modelled from cadence alone.** The gap
   (~10,500/day) is the `awaiting` accelerated 30-min poll, which the cadence model did not count.
   That makes `awaiting` roughly **43% of all scan traffic** — the single largest consumer, and the
   reason re-scoping it (point 4 above) matters more than the tier values do.
2. **Capacity is ~2,777/hour, not the 1,080–3,600/hour cited earlier.** That earlier range came from
   misreading `scanner.rs:535-536`, which describes observed *due-set sizes*, not a throughput
   ceiling.

**Demand vs capacity (corrected):**

| Scenario | Scans/day | Utilisation of ~66,600/day |
|---|---|---|
| Today (flat 12 h + `awaiting`) | ~24,200 | ~36% |
| **Proposed tiers + `awaiting` re-scoped** | **~22,000** | **~33%** |
| Proposed tiers, `awaiting` unchanged | ~25,400 | ~38% |
| Flat 3 h for *all* ongoing | ~52,200 | ~78% — feasible but tight |
| Original spec: flat 2 h, all ongoing | ~78,400 | **~118% — infeasible** |

**The proposed cadence is cheaper than today**, not 1.08× as earlier stated: the 3 h tier makes
`awaiting` redundant for the fastest series, and E2 removes ~540/day of paused polling. The original
flat-2 h spec genuinely was not reachable — it exceeds capacity, so series would slip their target
indefinitely and the scheduler would live permanently in drain mode.

**If more throughput is ever needed**, raise `SCAN_CONCURRENCY` before touching cadence — it is the
binding constraint, and 3 is conservative for I/O-bound upstream fetches. Do so only with the
cover-route budget in mind: the scanner and the cover route share one Suwayomi client.

**Re-measure before relying on these figures**: n = 3 for latency, and it varies ~10× with chapter
count. A proper measurement would sample ~50 series stratified by chapter count.

**Scope note:** this table governs **only Suwayomi-backed series**. MangaDex-only works have no
`series_scan_state` row — they ride the firehose, and after Phase D they get ~15-minute latency,
better than the 3 h tier and free.

Admin: per-series overrides already exist end-to-end — `series_admin.override_interval_hours` →
GraphQL mutation with validation (`graphql/mod.rs:6334`) → UI (`apps/admin/src/routes/+page.svelte:348`).
Only the resolved-tier display is new.

### Phase E2 — Trigger-based reopen detection (replaces the paused-series poll)
*Resolves F10. Prerequisite for the three "none" rows in E.*

**The idea:** stop polling paused series on a timer. Instead, treat a source's LATEST listing as a
*push signal* — if a COMPLETED/HIATUS/CANCELLED series appears in the **top 30 of its source ranked
by latest**, that source just published something for it, so scan it then.

**This costs zero additional upstream fetches.** The daily source sync already walks each subscribed
extension's LATEST listing (`sync.rs:644`, `browse_source(source_id, FetchType::Latest, page, None)`).
It currently **discards** every already-known series — `ingest_source_series` runs only for series we
do not have. The change is to stop discarding them: for a known series that is paused, enqueue a scan.

**Coverage [M].** Of the 598 at-risk Suwayomi-only paused series:

| | Series | Covered by the trigger? |
|---|---|---|
| On a subscribed extension | **539 (90%)** | yes |
| On an unsubscribed extension (`athreascans`) | 59 (10%) | no |

308 of the 539 are `all.mangadex`, which after Phase F is MangaDex-anchored and covered by the
catalogue sweep regardless — so the post-retirement at-risk cohort is ~290, ~80% trigger-covered.

**Is a top-30 window wide enough? [M] Yes, with large margin.** Measured daily churn — distinct
in-library series receiving a chapter in 24 h, by source:

| Source | Series updated / day | Time to fall out of top 30 |
|---|---|---|
| qiscans | 9 | ~3.3 days |
| asurascans | 8 | ~3.8 days |
| omegascans | 7 | ~4.3 days |
| flamecomics | 4 | ~7.5 days |
| all.mangadex | 64 | ~11 h (irrelevant — being retired, and MangaDex-anchored) |

A daily walk catches a top-30 entry with days to spare on every scanlator source. 30 is well
calibrated; the headroom is free, so do not tighten it.

**Implementation notes:**

* **Make the top-30 window explicit, not page-derived.** `browse_source` returns whatever page size
  the extension chooses (`suwayomi.rs:666-700` — `fetchSourceManga` has no page-size parameter), so
  "2 pages" is not a guarantee of 30 entries. Walk until **≥ 30 distinct entries have been observed**,
  then apply the existing stop rule.
* **`STOP_AFTER_KNOWN_PAGES = 2` (`sync.rs:70`) must not short-circuit the window.** It exists to end
  a *discovery* walk once caught up, and paused series are known by definition. Apply it only *after*
  the 30-entry floor is met.
* **Trigger = enqueue, not scan inline.** Set `next_scan_at` to the due-now sentinel so the existing
  scheduler picks it up on its normal tick. Keeps the source-sync pass fast and reuses all the
  existing backoff/failure handling.
* **Debounce.** A series sitting in the top 30 for three days must trigger once, not three times.
  Gate on `latest_chapter_at` having advanced since the last scan.

**Backstop for the uncovered 10%.** Unsubscribed extensions are never walked, so their paused series
get no trigger. Rather than leave them permanently blind, park them at **60 days** instead of never.
Cost: ~0.4 scans/day for the remainder, versus 540/day for polling everything. Nothing is ever
permanently blind, and >99% of the saving is retained.

**Net effect:** ~540 scans/day → ~0.4, with reopen detection that is *faster* than today (≤1 day
versus ≤14) for the 90% on subscribed extensions.

**Future direction (out of scope, worth recording).** The same signal generalises: LATEST tells us
which series on a source just updated, for **any** status. That is a push signal that could
eventually replace timer-based polling for ONGOING series too — scan on trigger rather than on a
cadence. That is a strictly better long-term architecture than any cadence table, and E2 is its
foundation. Not in scope here because LATEST only covers subscribed extensions and our library spans
more sources than that.

### Phase E3 — Per-source scanners with dynamic supervision
*Independent of A–D. Requested 2026-07-30. The win is **isolation**, not throughput — see the
concurrency note below before sizing anything.*

**Today:** one global scan loop (`scanner.rs:1631`), one due-set across the whole library, one
`SCAN_CONCURRENCY = 3`. A single misbehaving source can occupy all three slots with 30 s-timeout
stalls and starve every other source for the duration of the batch. That is the problem worth fixing.

**Target:** one scheduler loop **per source**, each ticking only its own source's series, each with
its own concurrency cap, tick interval, and health state.

**Sizing [M]** — only **15** distinct `source_id`s have in-library series, and after Phase F it is
**14 sources / ~3,676 series**:

| Source | In-library series |
|---|---|
| all.mangadex | 10,422 *(retired in Phase F)* |
| qiscans | 861 |
| thunderscans | 854 |
| arvenscans | 413 |
| asurascans | 337 |
| infernalvoidscans | 281 |
| omegascans | 276 |
| suryascans | 209 |
| flamecomics | 152 |
| athreascans | 143 |
| hijalascans | 107 |
| drakescans + 3 more | < 40 each |

11 sources hold ≥ 100 series; 3 hold < 10. One lightweight loop each is entirely tractable.

**E3.1 Schema.** `series_scan_state` is keyed by `series_id` and carries **no `source_id`**, so a
per-source due-query would need a join. Denormalise `source_id` onto `series_scan_state` and index
**`(source_id, next_scan_at)`** so each loop keeps the same bounded range-seek the global one has
today (O(due), terminating at the first future-dated row). Backfill from `source_series`.

**E3.2 Per-source config**, with defaults and per-source overrides:

| Setting | Default | Notes |
|---|---|---|
| tick interval | 60 s | Affordable now that a tick's due-set is per-source. Tightens worst-case polling lag from 300 s to 60 s. |
| concurrency | 3 | **Do not raise blindly — see below.** |
| enabled | from `extension_subscription` | Reuse the existing circuit breaker (`consecutive_failures`, `disabled_at`, migration 0049). |

**E3.3 Dynamic supervision — a scanner must appear when a source is added.** This is an explicit
requirement, and the current static `spawn()` cannot satisfy it. Design:

* A **supervisor task** owns a `HashMap<source_id, JoinHandle>` and reconciles it on an interval
  (~60 s) *and* on an explicit kick from the admin `setExtensionSubscription` mutation and from
  `sync::ingest_source_series` (so a brand-new source starts scanning without waiting for the
  interval).
* Reconcile = *set difference*, both directions: spawn a loop for any `source_id` that has
  in-library series and no running loop; signal-and-drop any loop whose source has no series left or
  whose subscription was disabled.
* **Idempotent and restart-safe.** The set is derived from the DB (`SELECT DISTINCT source_id FROM
  series_scan_state`), never from in-memory state, so a process restart rebuilds it exactly. Same
  principle as `next_scan_at` being the timer rather than an in-memory countdown.
* Each child keeps the existing panic-supervision-with-backoff wrapper (`scanner.rs:1640-1657`), so
  one source's loop panicking cannot kill the others or the supervisor.
* Bound the map: refuse to spawn beyond a `MAX_SOURCE_SCANNERS` (say 64) and log loudly, so a bad
  data state cannot spawn thousands of tasks.

**E3.4 Concurrency: raise the count only where it buys something.** The instruction was not to worry
about CPU, and CPU genuinely is not the constraint. The real ones are:

1. **There is no throughput problem to solve.** Measured utilisation is **~36%** at
   `SCAN_CONCURRENCY = 3` (see the capacity budget above), and the proposed cadence *lowers* total
   load. 15 loops × 3 = 45 concurrent would deliver **zero extra throughput at current demand** — it
   only drains *bursts* faster (cold start, post-deploy backlog).
2. **The failure mode isn't "Suwayomi breaks", it's a timeout→backoff cascade.** Suwayomi is a JVM
   service and will accept 45 concurrent GraphQL calls without trouble. But each
   `fetchMangaAndChapters` triggers an *outbound* fetch to a scanlator site. Where a site challenges,
   requests either queue behind a bypass or fail; either way `record_scan_failure` converts the
   result into exponential backoff (30 m → 1 h → 2 h …), so over-concurrency actively *de-schedules*
   healthy series. See E3.5 for the measured Cloudflare picture — it is currently benign, which is
   itself worth knowing.
3. **Extensions self-rate-limit.** Tachiyomi extensions commonly ship a rate-limit interceptor
   (single-digit req/s). Concurrency above that yields no throughput and just fills queues inside
   Suwayomi, inflating latency. **Verify per source before raising.**
4. **Bans are the worst outcome.** These are small scanlator sites. Losing a source to an IP ban is
   far worse than scanning it slowly.

**Recommendation:** keep per-source concurrency at **3** (configurable per source, ceiling ~6), and
add a **global semaphore of ~12–16** across all source loops so aggregate concurrency stays in the
range Suwayomi and FlareSolverr already handle. This delivers the isolation win — a stalled source
consumes only its own budget — without the fan-out that gets us blocked. Revisit only if
`due == DUE_BATCH_LIMIT` starts appearing, which is the actual saturation signal.

**E3.5 Cloudflare gating and real per-source latency — measured 2026-07-30 [M].**

Probed every one of the 15 sources with a real `fetchMangaAndChapters` against a live in-library
ONGOING series:

| Source | Subscribed | Series | Latency | Result |
|---|---|---|---|---|
| asurascans | yes | 337 | **0.12 s** | ok, 7 chapters |
| flamecomics | yes | 152 | 0.30 s | ok, 147 |
| arvenscans | yes | 413 | 0.36 s | ok, 161 |
| hijalascans | yes | 107 | 0.62 s | ok, 50 |
| drakescans | yes | 39 | 0.73 s | ok, 69 |
| qiscans | yes | 861 | 0.77 s | ok, 11 |
| infernalvoidscans | yes | 281 | 0.84 s | ok, 623 |
| thunderscans | yes | 854 | 1.00 s | ok, 124 |
| athreascans | **no** | 143 | 1.24 s | ok, 70 |
| omegascans | yes | 276 | **1.57 s** | ok, 141 |
| anisascans | no | 2 | 0.72 s | ok, 40 |
| galaxydegenscans | no | 1 | 1.43 s | ok, 13 |
| all.mangadex | yes | 10,422 | 0.80 s | ERROR: No chapters found |
| suryascans | yes | 209 | 0.70 s | **ERROR: HTTP 404** |
| arenascans | no | 1 | — | no ongoing series to probe |

**Cloudflare-gated sources: 0 of 15.** Every live source answered in **0.12–1.57 s**, all sub-2 s,
median ≈ 0.75 s. No challenge, no bypass needed.

**FlareSolverr is deployed but switched OFF and completely unused:**

* `server.flareSolverrEnabled = false` in Suwayomi's `server.conf`.
* Its container log shows **0 `POST /v1` solve requests in 48 h** — only a health `GET` every 30 s
  (5,741 log lines of pure noise).
* Consequence: a source that *does* start challenging fails outright with
  `java.io.IOException: Cloudflare bypass currently disabled`
  (`CloudflareInterceptor.kt:52`) — seen twice in 24 h.

**Two cheap actions this surfaces, neither part of the chapter work:**

1. **Enable `server.flareSolverrEnabled = true`.** The container is already running, health-checked,
   and `server.flareSolverrAsResponseFallback = true` is already set — so it engages only as a
   fallback. Today a newly-challenging source silently dies instead; this is latent availability risk
   for zero marginal cost.
2. **`suryascans` (209 series) returned HTTP 404.** Scan logs show `failed = 0` across 25 consecutive
   ticks, so this is probably one deleted series rather than a dead source — but a source that has
   moved domain would look exactly like this. Worth confirming before it silently rots 209 series.

**This also revises the capacity numbers upward.** Latency is highly variable and chapter-count
dependent: the earlier n = 3 sample gave 0.68 / 3.89 / 7.35 s (the slow one had 686 chapters), while
this 14-source sweep gave 0.12–1.57 s. Honest range: **~0.7–4 s median ⇒ capacity 0.75–4.3 scans/s
⇒ 65,000–370,000 scans/day ⇒ utilisation 7–37%** against the measured 24,200/day demand. The precise
ceiling is uncertain; the conclusion is robust — **the scanner is nowhere near saturated at
concurrency 3**, which is exactly why raising it buys nothing.

**Note on the cover route [M].** Earlier drafts warned that the scanner competes with cover fetching
for one Suwayomi budget. That is now largely obsolete: **115,574 of 115,656 works (99.93%) have a
locally cached cover** and `covers.sqlite3` is 20.5 GB, so the cover route almost never reaches
upstream. The pools are also separate — `COVER_FETCH_CONCURRENCY = 12` and
`BG_MATERIALIZE_CONCURRENCY = 4` are semaphores in `suwayomi.rs`, whereas `SCAN_CONCURRENCY = 3` is a
plain `for_each_concurrent` in `scanner.rs` and takes no Suwayomi permit at all. They contend only at
the Suwayomi instance and the network, not at a shared semaphore. Cover latency is therefore a much
weaker constraint on scan concurrency than previously assumed — but page fetches
(`PAGE_FETCH_CONCURRENCY = 16`, i.e. live reading) still share the same instance, and those are
user-facing.

### Phase E4 — Scan-failure honesty
*Fixes F11. Independent of everything else, shippable alone, and it should go EARLY — until it lands,
no health signal from the scanner can be trusted, including the ones used to validate other phases.*

**E4.1 Separate "fetched and empty" from "fetch failed, serving cache."** Give the scanner a
non-falling-back call path, or thread a `Provenance { Upstream, CachedFallback }` flag out of
`SuwayomiClient::chapters` / `series_and_chapters`. Keep the existing fallback for the **reader**
path — it is correct there.

**E4.2 A cached-fallback scan must not count as a success.** In `scan_due`, treat
`CachedFallback` as an upstream failure: route it to `record_scan_failure` so it bumps
`consecutive_failures` and backs off. It must **not** advance `last_scanned_at` as though fresh data
arrived, and must not let `new_found = false` masquerade as "confirmed no new chapters".

**E4.3 Surface source health.** Aggregate consecutive cached-fallbacks per `source_id` and expose it
in the admin console; auto-disable via the existing circuit breaker
(`extension_subscription.consecutive_failures` / `disabled_at`, migration 0049) once a whole source is
failing. A source-wide outage should be one loud alert, not 209 silent ones.

**E4.4 Reconcile the suryascans aftermath.** 209 `series_scan_state` rows point at an uninstalled
source, 53 of whose works have no other source. Decide and implement: park them, mark the works
source-less, or delete the rows — but do it explicitly rather than leaving them cycling forever.

**Expect a visible "regression" when this ships:** those 209 series will start reporting failures and
backing off exponentially. That is the fix working. Land E4.4 with or before E4.2 so the noise is
bounded.

#### E4 as built (2026-07-30)

| Item | Implementation |
|---|---|
| E4.1 | `suwayomi::Provenance { Upstream, CachedFallback }` + `series_with_provenance` / `chapters_with_provenance` / `series_and_chapters_with_provenance`. The old three methods delegate and drop the flag, so the **reader path is byte-for-byte unchanged**. `Provenance::and` takes the weaker of two halves. "No chapters" stays `Upstream` — the mutation reached the source and it answered; genuinely-empty series are counted separately as `zeroChapterSeries`. |
| E4.2 | Migration **0072** adds `series_scan_state.last_failure_kind` / `last_failure_at` + an index on `source_series(source_key, source_id)` for the health join. `scan_due` returns `Err` on `CachedFallback` **before** `persist_scan`, so `last_scanned_at` never advances on a scan that did not happen. `FailureKind::{FetchError, CachedFallback, PersistError}` — `PersistError` is kept distinct because it is *our* write failing and must not condemn a source. `record_scan` / `park_paused` clear both columns on success. `scan_series` (enrol / `triggerScan`) is honest too: the enrol path would otherwise baseline a brand-new series at 0 chapters off an empty cache and call it a success. |
| E4.3 | `catalog::source_scan_health` (one indexed GROUP BY), `source_exclusive_work_counts`, and a `source_scan_outage` table (0072). `check_source_health` runs after any pass that had failures **or** while an outage is open — the second half is what detects *recovery*, which happens on a pass full of successes. A confirmed outage alerts once per 24 h, parks the source's series 7 d out, and trips the subscription breaker via `trip_subscription_breaker` (independent of the sync pass's own strike count). Admin surface: `sourceScanHealth` query → a "Scan health" tab on `/sources` with a count badge. |
| E4.4 | Subsumed by E4.3 — see §8a. |
| Thresholds | `SOURCE_OUTAGE_MIN_STREAK = 3` (≈3.5 h of persistent failure, so a Suwayomi restart or FlareSolverr blip cannot trip it), `SOURCE_OUTAGE_MIN_SERIES = 5`, `SOURCE_OUTAGE_FAILING_RATIO = 0.8`, `SOURCE_OUTAGE_PARK_HOURS = 168`, `SOURCE_OUTAGE_REALERT_HOURS = 24`. Both a floor and a share, because either alone misjudges (a 1-series source with one dead entry; an 861-series source with 12). |
| Tests | 4 new: the failure-kind lifecycle, the outage predicate's floor+share, park idempotence + unpark, the health aggregate's separation of `cached_fallback` from `persist_error`, and the breaker tripping once. Full suite **405 passed / 0 failed** (400 of them mine and pre-existing; 5 came from another session's reports feature, which shipped in the same image). |

**Deployed 2026-07-30 16:14 UTC.** Image rebuilt from the tree the suite was run against; DB backed up to
`/tmp/predeploy-20260730-e4.sqlite3` and the prior image tagged `komika-server:rollback-20260730` first.
Migrations applied through **0080** (0071 FTS-widening, 0072 E4, 0080 another session's reader-reports).

First tick after restart — the exit criterion, met immediately:

```
scan tick complete library_size=14137 overdue=36 ok=34 failed=2
scan: series scan failed; backed off series_id="10724"
  error=suwayomi served CACHED chapters for series 10724: the upstream fetch failed,
        so this scan proves nothing about new chapters
```

**`failed` had been structurally 0 across all 14,098 series; it no longer is.** Two independent
findings in the first minute, and one of them was NOT Genz Toons — series 10724 is on
`arvenscans`/Vortex Scans (414 series), a source whose sampled series answered fine. That is exactly
the class F11 said was undetectable by design: a per-series silent cache-serve on an otherwise healthy
source. Pre-E4 it recorded as "scanned, no new chapters".

Also verified live: migration 0071 works — `work_fts` went 113,741 → **115,665** rows and the reported
"My Brother is a Vicious Dog" (Suwayomi-only, reader id `16635`) now returns from `search`.

### Phase E5 — LATEST-diff discovery: ~11× cheaper AND ~24× faster
*Proposed 2026-07-30 in answer to "can this be cheaper without sacrificing performance?" Answer: yes,
and it is not a trade-off — both axes improve. This **subsumes most of Phase E's polling**, which
becomes a slow safety net rather than the primary discovery mechanism.*

#### The measurement that motivates it [M]

Parsed 24 h of production logs (279 ticks):

| Metric | Value |
|---|---|
| Scans completed | **20,352** |
| New-chapter detections | **220** |
| **Hit rate** | **1.081%** |
| Upstream fetches per new chapter found | **93** |
| Fetches that discovered nothing | **20,132** |

**99% of the scanner's upstream work is wasted.** Every one of those 20,132 fetches is a live
`fetchMangaAndChapters` mutation that makes Suwayomi go out to a scanlator site. This is by far the
largest inefficiency in the system, and no phase A–F addresses it — they all keep the
poll-every-series-on-a-timer model and only tune the timer.

#### Why polling is the wrong model here

A source's **LATEST listing is ordered by the source itself, by most-recent chapter** — it is a *push
signal we are not reading*. One page request reveals what changed across 20–25 series. We currently
discover the same information with 20–25 individual fetches.

Critically, the ordering comes from the **source site, not Suwayomi's cache**, so it is a true
upstream signal. `MANGA_FIELDS` (`suwayomi.rs:150-156`) does include `chapters { totalCount }`, but
that is Suwayomi's *cached* count and will not reveal an unfetched chapter — so the signal to use is
**membership/position change in the ordered id list**, not the returned metadata.

#### Design

1. **Discovery.** Poll page 1 of each source's LATEST every ~15–30 min. Diff the ordered `id` list
   against the previous snapshot. Any series that **entered the page or moved up** has new content
   upstream.
2. **Targeted fetch.** Only for series the diff flagged — one `fetchMangaAndChapters` each, i.e.
   ~220/day instead of 20,352.
3. **Safety net.** Every series still gets a slow baseline poll (every 7 days) to cover sources whose
   LATEST is cached, broken, unsupported (`supportsLatest = false`), or unsubscribed. This is what
   keeps correctness independent of LATEST being trustworthy.
4. Reuse E2's machinery wholesale — E2 is the same idea applied only to paused series. E5 is E2
   generalised to *all* statuses.

#### Economics (arithmetic over measured churn)

| Model | Upstream requests/day | Detection latency |
|---|---|---|
| **Today** | **20,352** | 11.94 h median |
| Phase E tiers alone | ~22,000 | 3 h / 12 h / 24 h by tier |
| **E5: 30-min discovery + 7-day safety net** | **1,873** (720 pages + 220 targeted + 933 baseline) | **≤ 30 min** |
| **E5: 15-min discovery + 7-day safety net** | **2,593** (1,440 + 220 + 933) | **≤ 15 min** |
| E5: 30-min discovery + 14-day safety net | 1,407 | ≤ 30 min |

**Recommended: 15-min discovery + 7-day safety net — ~7.8× cheaper than today and ~48× faster than
today's 11.94 h median.** Even the most conservative variant is ~8× cheaper while being an order of
magnitude more responsive.

#### What this does to Phase E

The 3 h / 12 h / 24 h tiers stop being the discovery mechanism and become the **safety-net cadence**.
That is a real simplification: instead of tuning per-format tiers against a capacity budget, there is
one slow baseline plus a fast push signal. Keep the per-series admin override (owner requirement) and
keep jitter on the baseline. **`awaiting` disappears entirely** — it is currently 43% of all scan
traffic and, by construction, polls the *least* productive series (ones already known to be late)
hardest. A push signal makes it pointless.

Recommendation: **implement E2 first as specified** (small, paused-series only, proves the mechanism on
a low-risk cohort), then evaluate E5 before building Phase E's tier engine. If E5 validates, much of
Phase E is never needed.

#### Risks — this is a proposal, not a measured certainty

The 20,352 / 220 / 1.081% figures are measured. The E5 cost model is arithmetic over measured churn
plus assumptions that must be validated first:

| Risk | Mitigation |
|---|---|
| A source's LATEST is cached/CDN-stale → missed updates | The 7-day safety net bounds staleness; alert when the baseline poll finds a chapter the diff missed (that ratio is the health metric for E5) |
| `supportsLatest = false` on some extensions | Those sources fall back to pure polling — enumerate first (open pre-check 2) |
| Only subscribed extensions are walked (13 of 15 sources) | Same fallback; measured 90% coverage of the at-risk cohort in E2 |
| Position-diff false negatives (a series updates but stays at the same rank) | Only possible if 0 other series moved above it, i.e. it was already rank 1 and updated again — caught by the safety net |
| Unsubscribed/new source has no snapshot yet | First poll establishes the baseline and triggers nothing; second poll onward is live |

#### Smaller efficiency wins found alongside [M]

1. **FlareSolverr: 0 solve requests in 48 h.** A Chrome-based container (typically 300–500 MB RSS)
   doing nothing but answering a health `GET` every 30 s. Either **enable** it
   (`server.flareSolverrEnabled = false` today — cheap latent-availability insurance, and 0 of 15
   sources currently need it) or **remove** it. Running it disabled is the one option with cost and no
   benefit.
2. **129 installed Suwayomi sources, only 15 with in-library series.** 114 unused extensions loaded in
   the JVM. Uninstalling is free memory and a shorter startup.
3. **Firehose N+1: two queries per chapter** — `find_source_series_id` (`mangadex.rs:2346`) plus
   `chapter_row_unchanged` (`:2368`). Negligible in steady state (~750 chapters/day) but it is
   **~1.75 M queries on a full re-seed** of 877 k chapters. Batch by manga id if a re-seed is ever
   needed again.
4. **The 258 MB WAL observed mid-session was transient** — it grows during the ~20–25 s refresh chain
   and checkpoints after. No separate fix needed; Phase C removes the cause. Noted so it is not
   mistaken for a leak. (`db.rs` sets no `wal_autocheckpoint`/`journal_size_limit`, so SQLite defaults
   apply; the long transaction is what blocks the checkpoint.)

### Phase F — `all.mangadex` retirement
*Destructive. After C. Fixes F8.*

1. Persist each all.mangadex series' MangaDex UUID from `MangaType.url` (`/manga/<uuid>`).
2. For the 463 anchorless works, upsert from the direct API by UUID. This also merges them onto an
   existing canonical work when the UUID already maps elsewhere, killing duplicate `w_` rows.
3. **Verify** every one of the 10,422 works now has a `source_type='mangadex'` anchor. Report every
   failure; do not proceed past one.
4. Remove all.mangadex series from the Suwayomi library and delete **only proven-redundant**
   `source_series` rows. Keep the extension installed.
5. Exclude all.mangadex source ids from source-sync enrolment so they cannot return.

**Cover risk to check at step 4:** reader home/discovery serves `/api/v1/manga/{id}/thumbnail` from
`suwayomi_cover_blob`, **not** `/covers/`. Deleting `source_series` rows leaves `suwayomi_series` and
the blobs intact, but the 463 backfilled works flip to the MangaDex `/covers/` path — confirm each
has a cover before and after.

---

## 7z. Deferred and carried-over work (state as of 2026-07-30, after A + E2 + B + C1 shipped)

Everything below is **known, scoped, and deliberately not done**. Recorded here so no later
session has to re-derive that it is missing, and so nothing is mistaken for a regression.

### Deferred by the owner — Phase A's reader half

| Id | What | Exact call sites (verified 2026-07-30, line numbers current) | Consequence of leaving it |
|---|---|---|---|
| **A5** | Redirect out of `external_url IS NOT NULL` chapters instead of requesting pages, and badge them in the chapter list | reader `routes/read/[slug]/+page.svelte`; the empty state is at `:466-482` | ~35,000 chapters (4% of the mirror) stay unopenable behind the "no readable pages · often a licensing gap" message. Per §8a that is a *styled* empty state, not a blank screen — the severity framing in §4.2 was wrong, the fix still is not. |
| **A6** | Delete the count-as-number fallbacks | `source.ts:249` `` ch: `Ch. ${s.chapterCount}` `` (card mapper, unconditional); `source.ts:308-311` `Ch. {latestChapter ?? chapterCount}` (updates card); `source.ts:1376` `` ch: `Ch. ${read + 1}` `` (continue-reading — a progress index, not a number) | The scanner half of the feed still prints a chapter COUNT where a chapter NUMBER belongs. **Note `source.ts:283` is already correct** (`latestChapter` or empty), so the remaining sites are three, not five; the handoff's `:244 / :300-306 / :1365 / toFeatured:499 / toRelated:1478` line numbers are stale. `library:63` and `profile:151` (`Ch. {read} / {total}`) are progress ratios — **leave them alone**. |

### NOT deferred — **unimplemented**, and the doc says otherwise

**A1b, the `readable_at` / `external_url` backfill, does not exist.** Migration 0073's own
comment states the columns "are filled by the resumable backfill in
`mangadex::backfill_chapter_external_urls`". There is no such function anywhere in
`apps/server/src`. The only mechanism populating those columns today is the firehose
re-offering an already-changed chapter, which by construction never reaches the long tail.

Until it is written: the 46 works whose every chapter carries the 2037 sentinel stay absent
from `/updates`, and `release_event` correctly refuses to admit their chapters (they are not
readable yet, so they have not been released — see `seedable_where`). Roughly 8,800 batched
`/chapter?ids[]=` requests, resumable, off the hot path.

### Phase B — one switch left, gated on production

`catalog::work_source_chapters_from_spine` is written, tested, and **not wired in**
(`#[allow(dead_code)]`). The two-branch version stays live until the `catalog::spine` drains
have demonstrably finished on production, because a switch made before then returns FEWER
chapters rather than wrong ones. `the_spine_query_matches_the_two_branch_version` pins the
two together, so the switch is a one-line change, not a re-derivation.

**Gate:** `SELECT COUNT(*) FROM chapter WHERE chapter_key IS NULL` = 0 **and** the Suwayomi
work-list = 0, i.e. the `spine: drained — chapter spine and release ledger complete` log line.

### Phase C — complete (C1 shipped; C1b, C2, C3 built and awaiting deploy)

* **C1** `release_event` — shipped 2026-07-30 and seeded on production. Verified live: **0
  future-dated events**, `MAX(first_seen_at)` a real chapter release rather than `now()`, and
  **0–2 events in the trailing hour** where a `now()` seed would have produced ~1.3 M.
* **C1b** source picker — ranks by furthest-ahead chapter NUMBER with the mis-merge guard
  (<3 chapters, or <10% of the leader). Reader-side; 16/16 tests.
* **C2** `catalog::project_feed_from_ledger` — the feed's `released_at` and `latest_chapter`
  become a projection of `MAX(release_event.first_seen_at)` per work. **This is the F7 fix**,
  and it is structural: a duplicate chapter creates no event, so the `MAX` cannot move, so
  the card cannot re-float. Gated on `ledger::is_complete` so a half-seeded ledger can never
  sink live cards. The MangaDex half also gains its first incremental feed writer, per
  firehose page, deduped through a `HashSet`.
* **C3** `catalog::ledger::reconcile_feed` + migration 0092 — the wholesale chain is demoted
  from *the mechanism* to a **daily drift reporter**. It rebuilds, diffs against a
  pre-rebuild snapshot of `(released_at, latest_chapter)`, and writes a single overwritten
  row to `feed_reconcile_report`. `drifted == 0` is the evidence that the incremental path
  is correct rather than merely present; the sample says where to look when it is not.

**Still true and still deferred:** the projection UPDATEs feed rows, it does not CREATE them,
so a work that has never had a feed row waits for the daily reconciler. That is the residual
Phase D's 15-minute cycle shrinks — and it is why the reconciler counts an added row as
drift rather than pretending it is clean.

### Phase D — complete (built, awaiting deploy)

`run_recurring` now drives **three** cadences instead of one, because tying the cheapest and
most time-critical pass to the most expensive one is what made a brand-new chapter surface
already labelled "5h ago":

| Tick | Interval | Env | What runs |
|---|---|---|---|
| catalogue | 6 h | `CATALOGUE_SYNC_INTERVAL_SECS` | `/manga` sweep |
| **chapters** | **15 min** | `CHAPTER_SYNC_INTERVAL_SECS` | `/chapter` firehose + C2's per-page incremental feed writes |
| reconcile | 24 h | `FEED_RECONCILE_INTERVAL_SECS` | the demoted wholesale chain, as a drift report |

**15 minutes is only safe because of C.** That tick used to drag the ~20–25 s wholesale chain
behind it; firing it 96×/day instead of 4× would push the scanner past the pool's 15 s
`busy_timeout`. All three share `CATALOGUE_SYNC_RUNNING`, so two sweeps can never interleave,
and all three use `MissedTickBehavior::Delay` so a long pass postpones rather than queues.

**F9, both halves, fixed:**

1. **The cursor no longer advances past a record this pass did not land.** `last_created` was
   assigned before the uncatalogued-work skip, and the window is forward-only
   `updatedAtSince` — so a chapter skipped that way was never re-offered. Silent and
   permanent, with no counter to show for it. It is now advanced per-outcome: on upsert, on
   already-mirrored, and on a genuinely-unwanted row; **never** on a skip.
2. **`SweepOutcome.skipped`** is new and holds the seed open, alongside `dropped`/`failed`.
3. **The catalogue's upsert error arm now counts.** It only `warn!`ed, so `failed` was always
   zero for that half — and the latch only consulted `dropped`. Both halves now share the
   same predicate (`catalogue_seed_may_latch` / `chapter_seed_may_latch`).

### The F3 gate — APPROVED by the owner 2026-07-31 (build it)

> **Owner decision (2026-07-31): "f3 gap approved."** Remove the gate; the ~9,851 Suwayomi series
> are to enter `/updates`. The owner further clarified the intent: **when a series is added its
> latest chapters are legitimately "the latest added chapters of a series," so they belong in the
> updates channel** — this is the desired behaviour, not a flood to be feared.

`AND sss.last_new_chapter_at IS NOT NULL` (`catalog/mod.rs`, both the rebuild's scanner half
and `scanner::upsert_feed_series_update`) still locks **9,851 of 11,797** Suwayomi series with
chapters out of the feed: a first observation is a baseline and never stamps that column.

Removing it is a **product-visible change that adds ~9,851 cards to /updates** — now sanctioned.
It must still be built carefully: it is not in Phase C's or Phase D's exit criteria, so it is its own
unit, and it must be applied to **both** writers together.

The change itself is small and now well-supported: drop the NULL test, make the
`series_scan_state` join a `LEFT JOIN` (or those series are excluded by the join anyway), and
let the ledger projection supply the clock. The cards would sort by REAL release time, so the
long tail sinks rather than floods — but that reasoning wants confirming against a snapshot
before it ships, and it must be applied to **both** writers together or
`incremental_write_converges_with_the_periodic_rebuild` will catch the asymmetry.

### Browse's chapter label — APPROVED to build 2026-07-31 (server column chain)

> **Owner decision (2026-07-31): "add the server catalogue to make 12ch, ch.151 possible."**
> Build the `browse_catalogue` latest-chapter-number column and thread it through, so Browse shows
> **both** `"12 ch · Ch. 151"`. Confirms the earlier 2026-07-30 "show both" call.

Owner's decision (2026-07-30): Browse should show **both**, `"12 ch · Ch. 151"` — keeping the
honest catalogue-size count and adding the latest chapter number.

This is **not** a reader-only change, which is what it looks like. The figure is rendered at
`apps/reader/src/routes/(app)/browse/+page.svelte:819`
(`m.ch > 0 ? \`${m.ch} ch\` : 'No chapters yet'`) from `browse_catalogue`'s chapter COUNT, and
there is no latest-chapter-number column anywhere on that path. It needs
`browse_catalogue` → GraphQL → `packages/types` → `source.ts` → the card. Ship it with the
A5/A6 reader pass.

### Still not started — the accurate list as of 2026-07-31 (evening)

**Done and deployed:** A's server half (A1–A4), B, C, D, E2, E4.
**Built this session, green, NOT deployed:** **E5, Phase E, E3** (see §8f), plus the prior stack
A1b, A5, A6, the Phase B query switchover, C1b, and the `packages/` fragment+type fix.

| Unit | State | Gate |
|---|---|---|
| **E5** — LATEST-diff discovery | **BUILT, green, undeployed** (§8f) | Baseline re-measured (§8e), validated, built: `discovery.rs` + migration 0093. |
| **Phase E** — cadence engine (F6) | **BUILT, green, undeployed** (§8f) | `scan_interval_hours` tiers + policy path; `awaiting` off under E5. Deferred: 60-day park + `ABSURD_HORIZON` (item 7) and CANCELLED-disagree tier (item 8). |
| **E3** — per-source scanners | **BUILT, green, undeployed** (§8f) | Supervisor + per-source loops + auto-spawn kick; migration 0094. |
| **F** — `all.mangadex` retirement | **not started; NOW UNBLOCKED** | Depended on C, which shipped. Destructive — 10,422 `source_series` rows. Open pre-check 2 (are the 463 anchorless UUIDs still resolvable upstream?) is still unanswered and gates step 2. |
| **F3 gate** — unlock ~9,851 Suwayomi series | **APPROVED 2026-07-31, not yet built** | Drop `AND sss.last_new_chapter_at IS NOT NULL` on BOTH writers; ledger projection supplies the clock. |
| **Browse "12 ch · Ch. 151"** | **APPROVED 2026-07-31, not yet built** | New `browse_catalogue` latest-chapter-number column → GraphQL → `packages/types` → `source.ts` → card. |

**E5's baseline must be RE-MEASURED before it is evaluated.** The 20,352 scans / 220 detections /
**1.081% hit rate** / 93-fetches-per-detection figures were all measured on 2026-07-30 **before E4
shipped** — i.e. while `SuwayomiClient::chapters` silently fell back to a local-cache read on upstream
failure and `record_scan` counted that as a success. Those scans were in the denominator as
*productive-but-empty* when some were in fact *failed*. Re-derive the hit rate from honest data first;
the conclusion (polling is the wrong model) is very unlikely to reverse, but the arithmetic E5's
economics rest on will move.

**§9 pre-check 6 — "how many other sources are silently serving cached chapters right now?
Unmeasurable until E4 lands" — IS NOW ANSWERED.** Measured 2026-07-31, ~9 h after E4 went live:
**205 series carry `consecutive_failures > 0`**, against **0 library-wide** before E4 (that zero was
the tell §4.12 was written around), and **1 `source_scan_outage` row** — a whole-source outage
detected and parked rather than 209 silent successes.

### Open owner questions — RESOLVED 2026-07-31

* **Oneshots in `/updates`: YES.** Owner: *"oneshots will be shown in updates when they are added.
  When a series gets added, they will be sent to the updates page since it's the latest added
  chapters of a series."* So a oneshot (labelled `Oneshot`, keyed by `external_id`, per Phase A4)
  sorts into the feed like any other newly-added chapter. Do **not** exclude them. This pairs with
  the F3-gate approval above — newly-added series and their chapters belong in the feed.
* **Chapter-title disagreement: NOT A REAL QUESTION (closed).** Owner: *"chapter titles are shown in
  the series detail page, and the chapter list changes/updates based on the selected source, so what
  you're asking is redundant."* The selected source drives the displayed titles, so there is no
  cross-source tiebreak to adjudicate. Leave `group_aggregated_chapters` / the ledger label as-is.
* **216 `content_rating IS NULL` + `is_nsfw = 1` works: LEAVE ALONE** (session decision, under "decide
  everything else yourself"). It is a 216-row residue; 0053/0060 already fixed the material NSFW
  mislabels, and re-deriving from absent metadata risks *introducing* mislabels. Not worth the risk;
  revisit only if a user reports a specific bad label.

---

## 8. Superseded claims (audit trail)

Kept so the same wrong turns are not retaken.

| Claim | Status | Correction |
|---|---|---|
| "MangaDex chapters are missing from the mirror" (original report) | **WRONG** | Mirror is complete: 300/300 recent, 0/40 works short. §4.1 |
| "Chapters for uncatalogued works are dropped permanently — rank 2 cause" | **Real bug, wrong priority** | Firing on 0 chapters today. Latent, not the symptom. §4.10 |
| "External chapters = 236 chapters / 46 works" (my Rev 1) | **WRONG, 150× under-scoped** | ~35,000 chapters, 4% of mirror. §4.2 |
| "`pages == 0` identifies external chapters" (my Rev 1) | **WRONG** | bilibili externals report `pages: 45`. Only `externalUrl`. §4.2 |
| "`readableAt >= publishAt`, so COALESCE is safe" (my Rev 1) | **WRONG** | bilibili `readableAt` precedes `publishAt` by 2 weeks. §6.4 |
| "Populate `latest_chapter` from `MAX(suwayomi_chapter.chapter_number)`" (my Rev 1) | **WRONG — would ship a new bug** | Prints `Ch. 100000000`. Needs clamp + newest-release rule. §4.5 |
| "Use `round(number*1000)` as the ledger key" (my Rev 2) | **WRONG — would break admin overrides** | `round(number*100)` already exists in `chapter_override`. §6.1 |
| "Give the chapter cycle a 15-min timer" as an early phase (my Rev 1) | **UNSAFE as ordered** | Fires a ~20–25 s lock chain 96×/day. Incremental first. §4.6 |
| "~2,500 mainstream works are mis-flagged NSFW" (my prior notes) | **STALE** | Now 2. Migration 0053 fixed and holds. §4.11 |
| ~21.5k blank chapter labels are "mostly genuine oneshots, low impact" (my Rev 1) | **UNDER-SCOPED** | Same cohort is 21,422 works with *no chapter list at all*. §4.3 |
| "The daily source sync refreshes status, so completed = no scans is safe" (my Rev 1 §3) | **WRONG** | It does not; it only touches series we don't already have. Reopen detection is the scanner's park. §4.11 |
| "Completed = no scans" (original spec) | **REPLACED, not implemented as written** | Naively it blinds 598 series permanently. Replaced by E2's LATEST trigger + 60-day backstop, which is *faster* (≤1 day) and cheaper (~0.4 vs 540 scans/day). |

### 8a. Corrections found while implementing (appended 2026-07-30, during Phase E4)

| Claim | Status | Correction |
|---|---|---|
| "Migrations applied through **0071**" (this doc's env notes + the handoff) | **WRONG** | Production was at **0070** (`reader id anchor heal`, applied 2026-07-29 10:25). `0071_work_fts_all_sources.sql` was an **untracked, unapplied** file, and the running binary predated it — its logs still emitted `mangadex: work_fts refreshed works=113741` as a *separate* call, which is exactly the call site the 0071 change deletes. The FTS-widening + merge-dialog stream was uncommitted **and undeployed**. E4 ships with it, by the owner's decision. |
| "`en.suryascans` … now uninstalled" (§4.12) | **WRONG — it never propagated** | The extension is still **installed** and its source is still present, now displaying as **"Genz Toons (EN)"** with `isInstalled: true` (pre-check 5, below). Only our `extension_subscription` row was deleted. Its 209 series therefore kept scanning: the newest `last_scanned_at` was **2 minutes before the snapshot**, and `consecutive_failures > 0` was **still 0 library-wide** at 2026-07-30 12:32. |
| The 209 suryascans series are "silently frozen" on a stale chapter list (§4.12) | **UNDER-STATED** | They are at **zero**. All 209 have `chapter_count = 0` and there are **0** `suwayomi_chapter` rows across the whole source — the cached fallback returns an *empty* vec, and `record_scan` reads "empty, same as last time" as "no new chapters, success". So the works were never readable, not merely stale, which is consistent with §4.4's finding that 0 of the 53 exclusive works reach the feed. |
| "F1: the reader … renders blank" (§4.2) | **WRONG — it is a styled empty state** | `read/[slug]/+page.svelte:466-482` renders "No pages available for this chapter · This chapter has no readable pages yet — often a licensing gap at the source", with next-chapter and back-to-series buttons. So the ~35,000 external chapters are *unopenable behind a plausible-sounding excuse*, not a blank screen. Still the right fix (redirect out), but the severity framing was wrong. Worse, and newly found: `emptyReader` (`source.ts:2201`) is also the catch-all for ANY thrown backend error, so **a backend outage renders identically to a licensing gap** — an F11-shaped dishonesty on the reader side, out of scope here but worth its own fix. |
| E2's "60-day backstop park" is a one-constant change | **WRONG — it is coupled to two other invariants** | Caught by E2's own test run. `PAUSED_PARK_HOURS` is (a) the definition of `MAX_INTERVAL_HOURS` (`scanner.rs:77`), the ceiling `record_scan` clamps an inferred cadence to, and (b) below `ABSURD_HORIZON_HOURS` (16 days) **by design** — that horizon is the read-side net that drags legacy far-future rows back into the due-set (production had 3,578, one parked until 2033). A 60-day park sits PAST the horizon, so `reclaim_absurd_schedules` would pull every newly-parked series straight back and the park would do nothing. Both constants belong to Phase E, which rewrites interval policy wholesale, so E2 ships the TRIGGER (which is what E depends on) and the park moves with E. |
| E4.4 needs its own reconciliation pass for the 209 rows | **SUBSUMED** | Handled by E4.3's whole-source outage instead of a one-off: a confirmed outage parks the source's series (`SOURCE_OUTAGE_PARK_HOURS = 7d`, jittered), so a dead source costs ~30 fetches/day rather than 209, and recovery is automatic — any parked probe that succeeds clears the outage and unparks the cohort. The 53 exclusive works are **reported** (`sourceScanHealth.exclusiveWorks`), not deleted: they have no chapters either way, and deleting `source_series` rows for a source that may come back with an extension update is not reversible. |

### 8b. Corrections found while implementing Phases B and C1 (appended 2026-07-30)

| Claim | Status | Correction |
|---|---|---|
| §10: "Suwayomi series with chapters but no feed row: 9,851 → **0 (Phase A)**" | **WRONG PHASE** | No Phase A subtask (A1–A6) touches it. The gate is `AND sss.last_new_chapter_at IS NOT NULL` in `refresh_feed_series_updates` (`catalog/mod.rs:866`), which **Phase C** rewrites. Phase A's server half is complete and this row is still at 9,851 — that is correct, not a regression. |
| Migration 0073's comment: `readable_at`/`external_url` "are filled by the resumable backfill in `mangadex::backfill_chapter_external_urls`" | **THE FUNCTION DOES NOT EXIST** | Grep finds no definition anywhere in `apps/server/src`. The migration documents a mechanism that was never written, so today the ONLY way those columns get values is the firehose re-offering an updated chapter. A1b is not merely "deferred", it is **unimplemented** — the doc should not imply otherwise. |
| "Just check `EXISTS(… WHERE sc.manga_id = CAST(ss.source_key AS INTEGER))`" (implicit in the B3 sketch) | **PATHOLOGICAL IN ONE DIRECTION** | A `CAST` on the **column** is not sargable. `SELECT id FROM source_series WHERE CAST(source_key AS INTEGER) = ?` defeats the `(source_type, source_id, source_key)` index; the mirror-image query (`suwayomi_chapter` outer, correlated `source_series` inner) ran **12 minutes at 99.8% CPU before being killed**. With the CAST on the *outer row's value* instead — a constant per row, probing `idx_suwayomi_chapter_manga` — the same work-list answers in **0.05 s**. Measured: **0 of 14,137** Suwayomi `source_key`s are non-decimal, so `source_key = ?` bound as TEXT is both correct and indexed. |
| "`chapter` grows by 563 k Suwayomi rows … Low risk" (§9 risk table) | **CONFIRMED, and the boot cost is negligible** | Measured on a prod snapshot: both migration-0090 indexes build in **1.1 s each** over 877,891 rows. Adding the columns is metadata-only. This is not a boot hazard and needs no lazy-index workaround. |
| Ledger seeding is the plan's "single worst deployment hazard" | **CONFIRMED SAFE, measured on a snapshot** | Seeding from `MIN(COALESCE(readable_at, published_at))` produced **836,145 events** (MangaDex half), **0 of them in the future**, `MAX(first_seen_at)` = 2026-07-30T16:07Z against a 19:58Z clock, and **0 events dated within the last hour** — a `now()` flood would have put ~1.3 M there. Page 1 spot-checks as a genuine feed: one row per chapter, real labels including `17.5` and `Oneshot`. |
| `chapter_display` has one labelling rule | **IT HAD A SEAM** | The `Unnumbered` fallback printed the raw number string when it was present, so a chapter whose number was *parseable but insane* was labelled with the corruption itself — `chapter_display(None, Some("-1"), Some("Oneshot"))` returned `Unnumbered("-1")`. Latent on the MangaDex half; it would have become **systematic** in Phase B, because the spine stores every source's number as text and hands it back through the `raw` slot. Fixed: a raw that parses as a number is never used as a label. |
| A resumable drain "just needs a work-list query" | **THE WORK-LIST AND THE WRITE MUST SHARE ONE PREDICATE** | The ledger seed's first version had a work-list that asked only "does this chapter lack an event?" while the insert additionally refused future-dated chapters. A work holding a 2037-sentinel chapter was therefore offered forever, inserted nothing, and was offered again — **an infinite seed loop** that never reaches "complete". Caught by a test. Both now share `seedable_where()`. |
| Browse's "12 ch" → "12 ch · Ch. 151" is a reader-only change | **WRONG — it needs a server column** | The browse card's chapter figure is `browse_catalogue`'s count; there is no latest-chapter-number column anywhere on that path, so the owner's chosen "show both" needs `browse_catalogue` → GraphQL → `types` → `source.ts` → `browse/+page.svelte:819`. Belongs with the A5/A6 reader pass, not with C. |
| Baseline row counts (§4, §6.3) | **DRIFTED UPWARD, as expected** | 2026-07-30 19:18 snapshot: `chapter` **877,891** (was 877,824), `suwayomi_chapter` **564,193** (was 563,095), Suwayomi `source_series` **14,137** (was 14,103), series to materialise **11,836** (was 11,797), orphaned `suwayomi_chapter` rows with no mapping **0**. The catalogue is growing; the ratios in the plan all hold. |
| "Ship and verify one phase at a time. Do not batch phases into one deploy." (handoff) | **OVERRIDDEN BY THE OWNER, 2026-07-30** | Phase A's server half and E2 were complete but uncommitted; the owner chose to ship **A + E2 + B + C1 in one deploy** rather than pay a second ~9-minute build and restart. Recorded so a later regression bisect knows this deploy carries four units, not one. |

### 8c. Measured in production after the C+D deploy (2026-07-30 21:49 UTC)

| Claim | Status | Correction / evidence |
|---|---|---|
| Spine sizing "~1.6 M total" (§9 risk table) | **CLOSE — actual 1,442,150** | 877,891 MangaDex + 564,259 Suwayomi. Suwayomi materialisation: **11,836 series, 0 orphans, 0 left**. |
| Ledger sizing "≈1.31 M before cross-source overlap" (§6.3) | **CONFIRMED, overlap is real** | **1,000,753 events** — ~24% below the pre-overlap estimate, which is the cross-source dedup the `PRIMARY KEY` performs. |
| "Seed from `min(released_at)`, assert `MAX(first_seen_at) <= now`" (the plan's worst hazard) | **HELD, on production** | `spine: drained — chapter spine and release ledger complete events=1000753`; that log line is the SUCCESS branch of `assert_no_future_events`. **0 future events**, **0 events in the trailing hour** where a `now()` seed would have produced ~1.3 M. |
| Phase B exit criterion | **MET on live data** | **0 of 60** real multi-source works diverge between `work_source_chapters_from_spine` and the two-branch version. **0 absurd labels** (`>5000` or negative) across 1.44 M rows. |
| F4 "feed cards labelled with a count: all scanner-half → 0" (§10) | **ACHIEVED** | `latest_chapter IS NULL` on **0 of 64,921** feed rows. The entire scanner half was NULL before, which is exactly what forced the reader's `Ch. {chapterCount}` fallback. |
| F7 "a second source must not re-float the card" | **STRUCTURALLY FIXED** | **64,921 of 64,921 (100%)** feed rows now take `released_at` from `MAX(release_event.first_seen_at)`. A duplicate chapter creates no event, so the MAX cannot move. |
| **"The refresh chain costs ~20–25 s of write lock" (§4.6, `catalog/mod.rs:936-942`)** | **UNDER-STATED — measured 32.3 s** | The first production reconcile reported `duration_ms=32287`. Part of that is the reconciler's own before/after snapshots of 64,921 rows rather than held lock, but the chain has clearly grown past the figure the plan quotes (0068's `en_chapter_count` and C2's projection pass are both newer than that comment). It still produced **22 `database is locked` warnings and 3 scanner `series cache write failed`** in the 4 minutes around it — real contention, and 0 user-facing resolver errors. **This is the argument FOR C3, not against it:** the chain now runs 1×/day instead of 4×, a 4× reduction in exposure, and Phase D's 15-minute chapter tick deliberately does not drag it. |
| C3's reconciler "should report zero drift" | **REPORTS 1, AND IT IS THE RIGHT 1** | First run: `rows_before=64921 rows_after=64922 drifted=1 sample=w_dc3bd…: new row`. That is exactly the documented residual in §7z — the projection UPDATEs feed rows and does not CREATE them, so a brand-new work waits for the reconciler. The instrument is measuring the one gap we know about rather than surfacing a surprise. |
| Phase D's 15-minute chapter cycle | **LIVE** | `recurring sync started interval_secs=21600 chapter_interval_secs=900 reconcile_interval_secs=86400`, and the first chapter tick at t+150 s reported `chapter sweep complete stored=77 unchanged=0 dropped=0 failed=0`. |

### 8d. Found while finishing the deferred items (2026-07-30 22:30, three parallel agents)

| Claim | Status | Correction / evidence |
|---|---|---|
| §7z "the only mechanism populating `readable_at`/`external_url` is the firehose re-offering a chapter" | **CONFIRMED, and the number is ZERO** | `SELECT COUNT(*) FROM chapter WHERE external_url IS NOT NULL` = **0** on the post-C+D snapshot. Not "few" — none. The entire ~35,000-chapter F1 cohort is in the tail A1b now drains. **877,868** rows have `readable_at IS NULL`, and **all of them are `source_type='mangadex'`** (0 Suwayomi stragglers, because `suwayomi_upload_date_to_iso` parsed every upload date) — so the drain terminates rather than spinning on a permanent residue. Measured **8,779 batches**, matching the plan's "~8,800". |
| §4.2's TWO traps for external chapters (`pages == 0` is invalid; `readableAt` can precede `publishAt`) | **THERE IS A THIRD, and it would have been silent** | `/chapter?ids[]=` inherits MangaDex's default `contentRating=safe,suggestive,erotica` filter — the same default that once made every `pornographic` English chapter invisible to the firehose. For a backfill this is worse than for a sweep: a filtered-out id is indistinguishable from "upstream has no such chapter", and the drain *must* write something to terminate, so omitting the parameter would have burned every pornographic work's one chance at a real `readableAt`/`externalUrl` and taken the rows off the work-list **permanently**. All four ratings are now sent and a test pins it. `translatedLanguage[]` is deliberately NOT sent — the ids are already exact, so a language filter can only subtract. |
| §4.2 "46 works … and 21 more show a stale older chapter" | **CONFIRMED as a split, not restated** | The 236 sentinel chapters fall across exactly **67 distinct works** = 46 + 21. |
| Migration 0073's partial index serves the backfill work-list | **ONLY UNDER THE `EXISTS` FORM** | Written as a JOIN, SQLite drives from `source_series` and probes the partial index once per MangaDex `source_series` — **113,876 probes per batch**, worsening as each batch drains the front of the iteration order, across 8,779 batches. Written with `EXISTS`, the plan is `SCAN c USING INDEX idx_chapter_needs_readable_at` + one PK probe per row: linear in rows-still-to-do and shrinking as the drain runs. Same family as §8b's non-sargable-`CAST` entry. |
| §7z "the remaining A6 sites are three, not five" | **WRONG — there is a FOURTH** | `apps/reader/src/routes/(app)/+page.svelte:233` — the home hero prints `Ch. {current.ch}` where `current.ch` is `s.chapterCount` (via `toFeatured`, `source.ts:504`). **Not fixed**: that file currently carries ~541 lines of another session's in-flight changes. The right fix is relabelling it as a count ("98 chapters"), not blanking it. |
| A5 is "a reader change" | **WRONG — it was inert without `packages/`** | `Chapter.externalUrl` and `Chapter.label` were live on the server, but `CHAPTER_FIELDS` in `packages/api/src/operations.ts` never SELECTED them and `packages/types` never declared them. **GraphQL returns only what is asked for**, so the property was genuinely absent at runtime no matter what the reader did. Fixed in this pass across three files (fragment, TS type, local schema copy). |
| "`pageCount == 0` might do as a substitute for `externalUrl`" | **WRONG, verified live** | This backend returns `pageCount: 0` for **ordinary** chapters too (`canonicalChapters` on One Piece: every chapter `pageCount: 0`, `externalUrl: null`). `externalUrl != null` remains the only valid test — now recorded in the schema comment and the TS doc so it cannot be re-derived. |
| Feed labels in production | **595/600 numeric, 5 non-numeric** | Sampled 600 live `/updates` rows: `Oneshot` x4, `Brosquito` x1, and **0 null/blank**. So the count fallback was already dead server-side; deleting the reader's is regression defence. But those 5 rendered **"Ch. Oneshot"** — the bug `chapterChip` fixes. |
| Phase B's switchover is a pure no-op | **ONE REAL SEMANTIC CHANGE** | The two-branch version silently skipped any Suwayomi mapping whose `source_key` failed `parse::<i64>()`; the spine version joins on `source_series.id` and has no such skip. Inert today (§8b: **0 of 14,137** non-decimal keys) but not a no-op *by construction*, and the equivalence test cannot see it. |
| Phase B's switchover has no steady-state cost | **ONE RESIDUAL, bounded** | `put_chapters` writes the spine only when the chapter list actually changed (after the zero-write early-out), and `write_chapters_to_spine` needs a `source_series` mapping. A series whose chapters were cached *before* it gained a mapping is therefore up to **~30 min** late (the drain's `IDLE_RECHECK`) where the two-branch version found it immediately. Fewer-not-wrong, self-healing, and the only case that outlives the drain. |
| A1b will need follow-up wiring to get the 46 sentinel works into /updates | **NO — it resolves itself** | `ledger::seed_batch`'s work-list is cursorless (`NOT EXISTS(release_event …)` + `seedable_where()`), so as A1b writes real past `readableAt` values onto the 236 sentinel chapters, `spine::spawn`'s 30-minute idle recheck admits them automatically. |

### 8f. E5 + Phase E + E3 as BUILT (appended 2026-07-31; written & green, NOT yet deployed)

E5 (discovery), Phase E (tier engine) and E3 (per-source scanners) were built this session per the
owner's decisions ("e5 first, then full phase e", then "build E3 now anyway"). Server suite **450
pass / 0 fail**, `cargo fmt` clean, **0 net new clippy warnings** (still 20, all pre-existing). Held
undeployed. Deviations from §7 as written, recorded so no later session mistakes them for bugs:

| # | Decision | Rationale |
|---|---|---|
| 1 | **E5 discovery is a NEW `discovery.rs` loop, not folded into source-sync.** Migration **0093** adds `source_latest_snapshot(source_id, ordered_ids JSON, captured_at)`. Every `DISCOVERY_INTERVAL_SECS` (default 900) it fetches page 1 of each subscribed source's LATEST, diffs the ordered id list vs the snapshot, and `trigger_due_now`s the movers. | Reuses E2's sink verbatim; page-1-only + 15-min cadence is a different shape from the daily multi-page enrol walk. |
| 2 | **Position-diff rule = "entered the window or moved up in rank."** `moved_up_or_entered(prev, curr)`. First poll of a source baselines and triggers nothing; an empty LATEST does not overwrite a good snapshot. Window capped at 40 (`SNAPSHOT_WINDOW`). | A new chapter is the only thing that moves a series up in an upload-time ordering; passively-displaced series move *down*. The one false negative (rank-1 re-update) is the plan's documented case, caught by the baseline. |
| 3 | **Phase E computes the tier at SCAN time, not via a daily materialisation pass.** `scan_interval_hours(comic_type, last_release, now, override)` runs inside `record_scan_once`; `comic_type` comes from a single indexed lookup (`work_comic_type_word`, served by `idx_source_series_type_key_work`) in `persist_scan`. **No new column, no daily pass, no reconciliation.** | The plan chose materialisation to keep the *due-query* an index seek — but the due-query is untouched; the join is per-actual-scan (~15k/day), not per-due-check. Scan-time is simpler and always fresh; a dormancy transition is caught within one tier interval. |
| 4 | **`awaiting` is disabled ENTIRELY on the policy path**, not just for the 3h tier. | §7 E5's own recommendation ("awaiting disappears entirely" — it is 43% of scan traffic and polls the least-productive series hardest). E5's LATEST-diff is its replacement, and the two ship in the same binary, so the coupling always holds. **If Phase E is ever run without E5, re-enable awaiting.** |
| 5 | **Per-tier jitter**: ±5% on the 3h tier, ±10% elsewhere (`tier_jitter_fraction`). Legacy path unchanged at ±10%. | §7 Phase E point 1. Never zero on any tier. |
| 6 | **Integration keeps `record_scan`'s signature** (now `#[cfg(test)]`, the legacy inferred-cadence + awaiting path the ~40 scheduling tests exercise). Production goes through the new `record_scan_policy(..., comic_type)`. | Zero churn to the existing scheduling tests; Phase E behaviour gets its own 5 tests. The legacy path is still a faithful oracle for the inference maths. |
| 7 | **DEFERRED: the 60-day backstop park + `ABSURD_HORIZON_HOURS` raise.** Paused series still park at 14 days (`PAUSED_PARK_HOURS`), unchanged. | Lower value under E5 (a reopened completed series moves up in LATEST and E5 catches it), and it carries the reclaim-horizon invariant (§8a) that has a history of the 2033-parking bug. The "none" rows stay compliant: 14d park + E2/E5 triggers = never permanently blind. Revisit as an isolated change. |
| 8 | **DROPPED: CANCELLED-sources-disagree → 72h.** All CANCELLED series park + are trigger-driven, like the other paused statuses. | Minor cohort (432 series) and the per-*source* status the rule needs is not cleanly modelled (status is per-*work*). Safe omission — a genuine cancellation reopening is still caught by the LATEST trigger. |
| 9 | **Webtoon rides the fast (manhwa/manhua) tier; Comic rides the manga tier.** | Webtoons serialise on the same fast cadence; `content_type_word` already collapses Webtoon→"MANHWA". |

**Net modelled cost** (E5 + Phase E, awaiting removed, 14d park): ~15k tier scans + ~0.5k park +
~1.4k discovery ≈ **~17k upstream fetches/day vs the honest 22,292 today**, at **≤15-min detection
latency**. Cheaper *and* faster. The dramatic ~2.6k/day figure in §10 would need the deferred slow
(7-day) baseline from item 7's regime; the owner chose the full status/format tiers instead.

**E3 — per-source scanners (migration 0094 adds `series_scan_state.source_id` + index
`(source_id, next_scan_at)`; the global `tick`/`run_loop`/`spawn` are replaced by a supervisor +
per-source loops).** Decisions:

| # | Decision | Rationale |
|---|---|---|
| 10 | **Supervisor owns `HashMap<source_id, Child>`; reconciles every 60s AND on an explicit `kick_supervisor()` Notify.** Desired set = `SELECT DISTINCT source_id …` (always DB-derived, restart-safe) + a permanent `__unassigned__` sweeper. `reconcile_delta` (pure, unit-tested) computes spawn/reap. Per-child panic-restart wrapper; `MAX_SOURCE_SCANNERS = 64`. | E3.3 verbatim. The always-on unassigned sweeper (`source_id IS NULL` on the same index) means a freshly-enrolled or unmappable row is **never orphaned** by the sharding; `heal_null_source_ids` (bounded, `EXISTS`-guarded) then moves mappable rows onto their source loop within a reconcile. |
| 11 | **Per-source concurrency stays 3; a GLOBAL `Semaphore(12)` caps the aggregate across all loops.** `SOURCE_TICK_SECONDS = 60`. | E3.4 + hard constraint 2. Isolation, not throughput: 15×3=45 concurrent outbound fetches would cascade into timeout→backoff and ban risk. |
| 12 | **A source loop is reaped ONLY when it has zero series left — NOT when its subscription is disabled** (§7 E3.3 says "or subscription disabled"). | Dropping a disabled source's loop would orphan its still-present series (only that loop owns them). E4.3's outage park already stops a dead source costing fetches; coverage is preserved, parking is the mechanism. |
| 13 | **`kick_supervisor()` is wired from `set_extension_subscription` (enable) only, not from `ingest_source_series`.** | That mutation IS the "a source was added" moment the owner's auto-spawn requirement names. Discovery-enrolled series are scanned by the unassigned sweeper immediately and assigned to their source loop within one 60s reconcile — so an ingest-path kick would only shave <60s and add churn in bulk loops. |
| 14 | **`scan_health` (the admin display) becomes an aggregate**: the supervisor sets `library_size`/`overdue_count` once per reconcile; the loops set `last_tick_at`/`last_success_at`/`scanned_ok`/`scanned_failed`/stuck-count as most-recent-activity. E4.3's `source_scan_health` remains the authoritative per-source signal. | With N loops there is no single "last tick"; the display stays populated and "is the scanner alive/progressing?" is still answerable, without a per-source health rearchitecture. |
| 15 | **E4.3 library-wide health check runs from the supervisor**, gated on a shared `failures_since_health` counter the loops bump, plus `any_source_outage`. `reclaim_absurd_schedules` also moves to the supervisor (once/reconcile, not once/loop-tick). | Both are library-wide and idempotent; running them per-source-tick would multiply the work ~15×. |

**E3 note:** the win is isolation — a stalling source now consumes only its own loop's budget and the
global 12-permit ceiling, so it cannot occupy all slots and starve the rest. No throughput change
(utilisation was ~36%). The legacy library-wide `due_series_ids` is retained `#[cfg(test)]` as the
scheduling tests' oracle.

### 8e. E5 baseline RE-MEASURED on honest post-E4 data (appended 2026-07-31)

**Why this was mandatory.** The E5 economics in §7 Phase E5 rest on **20,352 scans/day, 220
detections, 1.081 % hit rate, 93 fetches/detection**. All four were measured on 2026-07-30 **before
E4 shipped**, while `SuwayomiClient::chapters` silently fell back to a local-cache read on upstream
failure and `record_scan` counted that as a `ok` success. Failed scans therefore sat in the `ok`
denominator as *productive-but-empty*. Post-E4 those scans are honestly routed to `failed`
(`FailureKind::CachedFallback`), so a clean denominator now exists.

**Source [M/logs].** 146 `scan tick complete` lines + 136 `scan: new chapters detected` lines from
`docker logs komika-server-1`, spanning **2026-07-30 21:50:50 → 2026-07-31 09:55:09 UTC = 12.072 h**
(the whole life of the current C+D image; E4's honesty is compiled into it). One tick per ~298 s, no
sustained draining. Scaled ×1.988 to a day.

| Metric | Old (pre-E4, **invalid**) | Honest (post-E4, 12.07 h ×1.988) |
|---|---|---|
| Successful upstream scans/day (`ok`) | — (conflated) | **21,493** |
| Failed scans/day (`failed`, new honest category) | **counted as `ok`** | **799** |
| Total live upstream fetches/day (`ok`+`failed`) | **20,352** | **22,292** |
| New-chapter detections/day | **220** | **270** |
| **Hit rate** — detections ÷ successful scans | **1.081 %** | **1.258 %** |
| Hit rate — detections ÷ all fetches | — | 1.213 % |
| Upstream fetches per detection | **93** | **82** |
| Ticks with any failure / max failed in one tick | 0 (structural) | **77 of 146 / 15** |
| `stuck` ticks (`ok=0, failed>0`) | — | **0** — no total-source outage in-window |

**Interpretation.**

1. **The conclusion does not reverse — it strengthens.** Honest hit rate is **~1.2–1.3 %**, so
   **~98.8 % of every live `fetchMangaAndChapters` still discovers nothing**. Polling remains the
   wrong model by a wide margin.
2. **The denominator moved the "wrong" way, and that matters.** Removing the dishonest
   cached-fallbacks from the *productive* count lifts the hit rate slightly (1.081 → 1.258 %), but the
   honest **total upstream cost is ~22,292 fetches/day, higher than the invalid 20,352** — the old
   number under-counted the daily fetch volume, not over-counted it. E4's backoff has not yet shrunk
   volume below the old figure because the catalogue grew (14,137 series) and the failing cohort keeps
   being retried on backoff rather than dropped. So E5 has *more* waste to remove than the plan
   assumed, not less.
3. **799 failed/day is a real new cost centre E5 does not touch** — those are the ~205
   `consecutive_failures>0` series (§7z / §9 pre-check 6) being retried on exponential backoff. E4
   makes them visible; E4.3's outage park (not E5) is what bounds them. 77 of 146 ticks carried ≥1
   failure but **0 ticks were `stuck`**, i.e. no whole-source outage fired in this window (the one
   standing `source_scan_outage` predates it and its cohort is parked).

**E5 cost model, re-derived with honest churn** (15-min discovery + 7-day safety net):

`1,440 LATEST pages + 270 targeted fetches + 933 baseline (6,531 ongoing ÷ 7 d) = **2,643/day**`

versus the honest **22,292/day** today ⇒ **~8.4× cheaper** (the plan claimed ~7.8× against the invalid
20,352) and **≤15 min vs 11.94 h median latency** unchanged. Every axis of the E5 case survives the
honest re-measurement, with a slightly *larger* efficiency prize.

**Not re-measured here, and why it's sound to defer:** per-source failure concentration and the
current `consecutive_failures` distribution require a `.backup()` snapshot; §7z already records the
same-day figure (205 series >0, 1 outage, measured 2026-07-31) and the 799 failed/day above
corroborates it (repeat retries on a ~205-series cohort). Re-snapshotting a same-day number was judged
not worth the 1.7 GB backup + WAL-pin risk against the hard "never read prod directly" constraint.

### 8g. §9 pre-check 2 RESOLVED — and Phase F step 4 is BLOCKED (appended 2026-07-31, read-only agent + orchestrator re-verification)

Snapshot 2026-07-31 13:36 UTC (`.backup()` per §10) + local Suwayomi GraphQL for UUIDs + paced
MangaDex `ids[]=` batches. Prod was never read directly; no DB writes, no repo edits.

**Answer to pre-check 2: all 496 anchorless `all.mangadex` UUIDs resolve upstream — resolvable 496 /
not-resolvable 0 / no-UUID-obtainable 0.** All `state: published` (status: completed 272, ongoing 187,
hiatus 24, cancelled 13; contentRating safe 364, suggestive 132; `cover_art` present 496/496; `en` in
`availableTranslatedLanguages` 496/496). Batches sent with **all four `contentRating[]` values** —
omitting them silently drops `pornographic` rows and manufactures false "missing". **Phase F leaves no
upstream-driven residue.**

**Counts re-derived (drift from the 2026-07-30 figures).** `all.mangadex` now sits on **exactly 1**
source id (`2499283573021220255`), not 62 — the 59-language leak was consolidated. **10,479**
`source_series` rows (was 10,422) across **10,431** works; **9,945 (95.34%)** already carry a
`source_type='mangadex'` anchor.

**§4.9's "496 rows / 463 works" conflates two cohorts.** Measured today: *no anchor* = **496 rows /
486 works**; *no anchor AND no other Suwayomi source* = **473 rows / 463 works**. The 463 is correct
only for the narrower cohort.

**§4.9's "a blind delete orphans them" is FALSE.** All 496 anchorless UUIDs already exist in our
catalogue as `source_type='mangadex'` rows on a **different** work (495 distinct twins; in 0 cases the
same work). The 486 anchorless works are duplicate `w_` rows, not uncatalogued content. 0 of 496 carry
more chapters than their twin; 92 are empty on both sides and upstream returns `total: 0` for the
sampled ones, so §4.1 holds.

**Cover risk (§7 Phase F step 4): CLEARED — 0 works lose a cover.** All 495 twins have
`work.cover_file_name`, a `work_cover` row and a non-NULL `cover_cached_version`, with 0
`work_cover_issue` rows; 495/495 return HTTP 200 with real bytes from `/covers/{work_id}.webp` today.

**NEW BLOCKER — step 3's gate does not protect step 4.** Redundancy must be proven at the **UUID**
level, not at "the work has *some* mangadex anchor". Independently reproduced by the orchestrator
against the same snapshot:

| Class | Rows |
|---|---|
| **Proven redundant** — row's UUID == a `source_type='mangadex'` `source_key` on the same work | **9,929** |
| **NOT proven redundant** — work HAS an anchor, but this row points at a *different* MangaDex entry | **54** (51 works) |
| Anchorless (no anchor at all) | **496** |
| **Total** | **10,479** |

Those 54 rows carry **4,178 `chapter` rows**, and ~40 of them carry MORE chapters than their work's own
anchor — a **~3,206-chapter deficit** (the agent counted 3,207 across 41 rows; the orchestrator's
independent rerun gave 3,206 across 40, a tie-handling difference at `n > anchor` — the substance is
identical and confirmed). `chapter.source_series_id` is
`REFERENCES source_series(id) ON DELETE CASCADE` (verified in the snapshot schema), so **step 4 as
worded would silently cascade-delete those chapters** — and step 3 as worded passes all 54 cleanly,
because their work does have an anchor. They are colored/version/fan editions mis-merged onto the base
work: *Peerless Martial God (Version 1)* 412 ch vs anchor 10, *YuruYuri (Fan-Colored)* 349 vs 1,
*Kaguya-sama (Official Colored)* 371 vs 47, *ReLIFE (Book Version)* 240 vs 0.

**Step 3 is also unachievable as literally worded** ("verify every one of the 10,422 works now has a
mangadex anchor"): step 2 *merges* the 486 anchorless works onto their twins, so those works cease to
exist and cannot be verified as having an anchor. The invariant must be restated over rows, after
redirect resolution.

**Steps 3–4 to be rewritten as:**
3. Resolve every `all.mangadex` row's UUID (all 10,479 are obtainable from `MangaType.url`). Delete a
   row ONLY if its UUID equals a `source_type='mangadex'` `source_key` on the same work **after**
   redirect resolution. Report every row that fails; do not proceed past one.
4. For the 54 UUID-mismatch rows: create the missing direct anchor, or split the mis-merged work —
   **do not delete**.

**Other Phase F hazards found in the snapshot:** 18 `work_redirect` rows point AT 11 anchorless works
(merging them without re-pointing `new_id` breaks §4.14's "0 stale rows" baseline); 428
`merge_candidate` rows, 486 `browse_catalogue` rows and 37 `feed_series_updates` rows reference
anchorless works; **22,991 `release_event` rows** cascade off them via `work_id`
(`first_source_series_id` is nullable and non-enforcing — that §9 mitigation is intact); 22,148
`chapter` rows sit on the 496 anchorless rows and 348,434 on the 9,983 anchored ones; 9
`suwayomi_series` rows on all.mangadex have no `source_series` row.

**One deleted UUID exists in the wider cohort, none in the anchorless one:**
`618f87d7-f32c-471b-be12-360595bf1fc7` 404s upstream but is still mirrored locally (1 chapter). It is
one of the 54 mismatch rows, not one of the 496.

**Step 5 note:** `source_extension` registers **61** all.mangadex source ids (60 hold zero rows), so
enrolment exclusion should still cover all 61 even though step 4's blast radius is a single source id.

### 8h. Found while building the F3 gate + Browse label (appended 2026-07-31, Agent 2 + orchestrator spot-checks)

| Claim | Status | Correction / evidence |
|---|---|---|
| §7z "Removing [the F3 gate] is a product-visible change that **adds ~9,851 cards** to /updates" | **WRONG — 6.9× over-stated. It adds 1,422 cards.** | Simulated on the 2026-07-31 13:35 `.backup()` snapshot. The gate admits 1,820 works; without it, 10,966. Of the 9,146 newly-admitted WORKS, **7,724 already have a feed row from the mirror half**, so only **1,422** are new CARDS. The series-level figure the plan quotes is right (9,813 newly-admitted series) — it just does not survive the collapse to one row per work that `feed_series_updates` performs. Orchestrator corroboration: the snapshot holds 64,935 feed rows and the post-change count is 66,357 = **+1,422 exactly**. |
| §7z "the cards would sort by REAL release time, so the long tail sinks rather than floods — but that reasoning wants confirming against a snapshot" | **CONFIRMED, and stronger than claimed** | All **1,422 of 1,422** new cards carry a `MAX(release_event.first_seen_at)` clock (0 fall back to `suwayomi_series.latest_chapter_at`). Post-change ordering: **0 of the top 20** and **3 of the top 100** are new (anonymous/NSFW-filtered: 1 of top 20, 4 of top 100); 8 of top 500, 16 of top 1000. Page 1 is unchanged. |
| The F3 change might FLOAT existing cards via `released_at = MAX(existing, excluded)` — **a risk §7z does not consider** | **CHECKED, ZERO RISK** | All 7,724 newly-admitted works that already had a feed row have `release_event` rows, so the projection resets `released_at` to the ledger MAX regardless of what the conflict clause computed. **0** existing cards move. The F7 structural fix absorbs the change completely. |
| §7z "drop the NULL test, make the `series_scan_state` join a LEFT JOIN (or those series are excluded by the join anyway)" | **CONFIRMED — the LEFT JOIN is the load-bearing half** | ~11.8k of the 14.2k qualifying series either have no `series_scan_state` row or a NULL one. Dropping only the predicate would have changed almost nothing. |
| §7z / §7 "`catalog::work_source_chapters_from_spine` is written, tested, and **NOT wired in** (`#[allow(dead_code)]`)" — Phase B's "one switch left, gated on production" | **STALE. THE SWITCH IS ALREADY FLIPPED, and the gate is met.** | That identifier exists **nowhere** in the codebase and `git log -S` shows it never did (orchestrator re-verified by grep). The live function is `catalog::work_source_chapters` (`catalog/mod.rs:3234`), called from production (`graphql/mod.rs:3452`, `:3539`); the old implementation was renamed `work_source_chapters_two_branch` (`catalog/mod.rs:3139`) and demoted to `#[cfg(test)]` on 2026-07-30. Gate verified satisfied on the snapshot anyway: `chapter WHERE chapter_key IS NULL` = **0**, Suwayomi mappings with cached chapters but no spine rows = **0**. The equivalence test is a full multiset comparison, not a weak check. |
| §7z C2 "The MangaDex half also gains its first incremental feed writer", implicitly covered by `incremental_write_converges_with_the_periodic_rebuild` | **THE TEST COVERS ONLY THE SCANNER HALF** | Every drive in `scanner.rs:4867-5016` goes through `touch_feed_series_update` → `upsert_feed_series_update`. The MangaDex half is an un-extracted inline block in `sync_chapters` (`mangadex.rs:2981-3013`) whose only caller is production; **no test invokes it**. It also runs ONLY `project_feed_from_ledger_for_work`, an `UPDATE … FROM` — so it **never CREATES a feed row**, and a mirror-only work with no row yet stays invisible until the 6-hourly rebuild, which is precisely the F5 staleness the block claims to fix. Best-effort (`warn!` + `continue`), so failure is silent. **Not fixed — carried forward.** |
| `ledger::is_complete` "prevents a half-seeded ledger sinking live cards" | **TRUE, with four bounded caveats** | It requires `spine::remaining == (0,0)` AND zero seedable chapters lacking an event, and `the_projection_stays_inert_until_the_ledger_is_complete` proves inertness. But: (a) "seedable" excludes undated/2037-sentinel chapters by design, so "complete" means complete w.r.t. **datable** chapters; (b) `events > 0 && pending == 0` is vacuously true on a DB with one event and no seedable chapters; (c) the production memo is one-way and `#[cfg(not(test))]`, so **no test covers the memoised path** and a restore cannot flip it back; (d) `spine::remaining` only counts mappings that already hold cache rows. |
| §7z A6 "the remaining sites are three" / §8d "there is a FOURTH … Not fixed" | **THE FOURTH IS NOW FIXED** | `(app)/+page.svelte:233` now renders `{current.ch} chapters` (singular-aware) instead of `Ch. {current.ch}`, per §8d's own prescription to relabel rather than blank. A repo-wide sweep for `Ch. ${` / `Ch. {` now returns **zero** count-as-number sites. |
| A5 "badge them in the chapter list" — believed done | **HALF DONE; NOW COMPLETE** | Only the READER's dropdown badged (`read/[slug]/+page.svelte:368-370`), visible only *after* arrival. The SERIES page — the list people actually browse — carried no badge and `SeriesChapterView` did not even hold the flag. Added `external?: boolean`, populated in both chapter mappers via `chapterExternalUrl`, rendered as an outlined `OFF-SITE` chip. On the aggregated path a chapter the selected translator lacks degrades to no badge (`AggregatedChapter` exposes no `externalUrl`) — fewer-not-wrong. A5 is an in-app **interstitial**, not a hard redirect (better: a redirect strands the back button), and it cannot fire on a chapter that has pages because `source.ts:2247/:2277` force `urls = []` whenever `chapterExternalUrl` is non-null. |
| C1b's guard is "<3 chapters **ahead**, or a **lead** <10% of the leader" (as stated in several handoffs) | **THE PROSE IS WRONG; THE CODE IS RIGHT — do not "fix" it** | `translator-select.ts:122-128` guards on the candidate's **absolute** `chapterCount` (`>= 3`, and `>= 10%` of the leader's COUNT), never on a "lead", and the 10% denominator is a count not a chapter number. Applying the prose version to the production case the tests pin (One Piece Official Colored: 764 chapters reaching 763, vs a 1-chapter source claiming 1183) gives a 55% lead — **the mis-merge would WIN**. Anyone who edits the code to match the prose ships a regression. 16/16 tests verified by execution. |
| Oneshots in /updates — "confirm they reach the feed; fix if not" | **THEY ALREADY DO — no fix needed** | Snapshot: **23,916** `x:<external_id>`-keyed release events; **21,935** labelled `Oneshot` across **21,011** works; **20,725** feed rows carry `latest_chapter='Oneshot'`; **0** feed rows have a NULL or empty `latest_chapter`, so `Ch. undefined` is unreachable. `chapterChip('Oneshot')` → `'Oneshot'` (never `'Ch. Oneshot'`), pinned by `chapter-label.test.ts`. Label vocabulary has a long upstream tail (`oneshot` ×69, `Oneshot (Decensored)` ×63, `[ONESHOT]` ×18, `One-Shot` ×7); all render verbatim, which is correct. |
| Browse's "No chapters yet" label | **WAS A LIE FOR ~11,000 WORKS; now fixed** | ~11k feed works have `en_chapter_count = 0` because their chapters carry a NULL number (oneshots — migration 0069's own note), and the card said "No chapters yet" about them. Now renders `Oneshot`; only when **both** figures are absent does it fall back. |

**Carried forward — NOT fixed, both in files Agent 2 did not own:**

1. **B2 is incomplete on the incremental path.** `scanner::mirror_feed_row_into_browse_catalogue` carries its **own hardcoded** 10-column `MUT` list rather than using `catalog::BROWSE_CATALOGUE_MUTABLE`, so the incremental Browse mirror will not carry `latest_chapter` (orchestrator confirmed: the list holds `en_chapter_count` and no `latest_chapter`). Browse's chapter *number* on a freshly-scanned Suwayomi series therefore lags until the next `refresh_browse_catalogue` (boot + per catalogue-sync cycle). Patch: add `"latest_chapter",` to `MUT` after `"en_chapter_count",`, and add `latest_chapter` to both the INSERT column list and the `SELECT f.…` list.
2. **`graphql::updates` keeps its OWN membership gate** — `AND sss.last_new_chapter_at IS NOT NULL` at `graphql/mod.rs:3071` (orchestrator confirmed it is the only surviving instance; both writers are clean). It is a resolver, not a writer, so it sat outside the owner's "both writers" approval — but it is **not dead code**: the reader's HOME "Latest Updates" row calls `backend.updates()`. **So as built, /updates admits 10,966 works while the home row still admits ~1,820.** Owner decision required. Exact change if consistency is wanted: delete that predicate from the `DETECTED` const, and remove the `INDEXED BY idx_scan_state_detected_series` hint with it (that partial index only carries detected rows).

**Latent, low severity, not fixed:** (a) every `page` argument is `Int! = 1` server-side but `$page: Int` client-side, so an explicit `null` (not `undefined`) 400s the whole query — wants `page ?? 1` in `graphql-backend.ts`; (b) `source.ts:839` casts a possibly-`undefined` `find` result with `as TranslatorOption` — unreachable today, but the cast hides a real hole.

**Migration 0095** (`0095_browse_latest_chapter.sql`) adds `browse_catalogue.latest_chapter`, nullable
(a DEFAULT would make "unknown" indistinguishable from an answer). Unapplied, like 0093/0094.

### 8i. E5 / Phase E / E3 verification pass — the §8f build audited (appended 2026-07-31, Agent 1 + orchestrator re-verification)

**Headline: Phase E was INERT. It would have deployed as a no-op that logs like a live engine.**

| Claim | Status | Correction / evidence |
|---|---|---|
| §8f item 3: "`comic_type` comes from a single indexed lookup (`work_comic_type_word`, served by `idx_source_series_type_key_work`) in `persist_scan`" | 🔴 **BOTH HALVES WRONG — the whole tier engine was a no-op** | `catalog::work_comic_type_word` selected `w.comic_type`, **a column that has never existed**. `work` carries only `content_type_override` (0030); `comic_type` is materialised onto `feed_series_updates` (0064) and `browse_catalogue` (0069). Executed against a fully-migrated pool it returns `SqliteError code 1: no such column: w.comic_type`, and its `.ok()?` swallowed that into `None` — so **every series in the library tiered as `ComicType::Manga` = flat 12 h**. No manhwa/manhua/webtoon ever reached the 3 h tier; no dormant series ever reached 24 h. Every Phase E test passed because they all call `record_scan_policy` with a hand-built `ComicType` and never exercise the lookup. Second error: **`idx_source_series_type_key_work` does not exist in any migration** — the doc comment cites migration 0063 for an index that was never created; the real indexes are `idx_source_series_type_key` (0041) and the covering partial `idx_source_series_suwayomi_key` (0072). *Orchestrator re-verified all three facts independently by grep over `migrations/` and by reading the function body.* Fixed in `scanner::scan_comic_type`: one `(source_type, source_key)` seek + two PRIMARY-KEY probes reading `COALESCE(feed_series_updates.comic_type, browse_catalogue.comic_type)` — the same materialised value the reader's badge shows, and the one the scanner already maintains via `fill_missing_feed_comic_type`. A DB error now logs at `warn` instead of vanishing. Guarded by `the_tier_lookup_resolves_a_real_type`. **`catalog::work_comic_type_word` deleted by the orchestrator** (both agents had finished; it was dead the moment the fix landed). |
| §8f item 10: "the always-on unassigned sweeper … means a freshly-enrolled or unmappable row is **never orphaned** by the sharding" | 🔴 **TRUE FOR NULLs, FALSE AT THE CAP** | A source clamped out by `MAX_SOURCE_SCANNERS` has series with a **non-NULL** `source_id`, which the `source_id IS NULL` sweeper does not match — **nothing scanned them**, and the supervisor's own log said so (`"the excess will not be scanned"`). Which sources were clamped was non-deterministic (`HashSet::difference` order), so the hole moved between reconciles. Latent today (~15 sources vs 64) but silent and unbounded if it ever fired. Fixed by making the assigned set an ascending PREFIX (`ORDER BY source_id ASC LIMIT MAX-1`), so its complement is the single range `source_id > cutoff`, which the sweeper takes (`SourceSel::Unassigned { above }`). Three disjoint exhaustive partitions; `the_cap_never_orphans_a_row` asserts exact set equality (no orphans AND no double-scan). |
| E5's position diff is safe under whatever page size the extension returns | 🔴 **WRONG — a GROWING page fired a phantom detection per newly-visible row** | `browse_source` has no page-size parameter (§7 E2's own note), so page 1 can return 20 entries then 40. "Absent from `prev` ⇒ entered" read that as 20 simultaneous new chapters and issued 20 targeted `fetchMangaAndChapters` that find nothing — **E5's own waste profile, re-introduced at up to 96 ticks/day × 15 sources**. Shrinking pages were safe; *re-growing after a shrink* was not. Fixed: an absent id counts as entered only at `new_rank < prev.len()`. Costs no real detection (a new chapter lands at rank 0–2). |
| E5's comment: "snapshot BEFORE the trigger, so a mid-pass crash re-triggers rather than losing the update" | 🔴 **THE COMMENT INVERTED THE CODE'S ACTUAL SEMANTICS** | Writing the snapshot first makes it the commit point, so an interruption between the two **loses the detection permanently** — the next diff compares against the advanced order and sees nothing move. Not a rare crash window: `run_loop` deliberately races `discovery_pass` against shutdown, so the future is dropped mid-poll on **every SIGTERM inside a pass** — i.e. on every deploy. Now trigger-then-snapshot (at-least-once); a duplicate costs one idempotent UPDATE + one scan versus a chapter waiting out the 12–24 h tier. |
| Phase E's dormancy input | 🔴 **TWO DEFECTS** | (a) `last_release` came from `UploadCadence::latest_upload`, inheriting that function's `>= 2 timestamps && gaps > 0` precondition — so a manhwa on its **first** chapter, exactly the cohort the 3 h tier exists for, reported "never released" and got the 24 h dormant tier (same for any series whose chapters share one upload timestamp, e.g. a bulk import). Replaced with `newest_upload`, identical whenever the cadence exists, so it only ever widens the input. (b) `Duration::num_days()` truncates toward zero, so `num_days() <= 14` really meant "< 15 days" — a free extra day on the fast tier. Now an exact `chrono::Duration` comparison, pinned by `the_dormancy_boundary_is_exact`. |
| §8f item 14: with N loops the health display "stays populated and 'is the scanner alive/progressing?' is still answerable" | 🔴 **THE OPPOSITE — E4's stuck signal was silently disabled** | Every loop wrote `scanned_ok`/`scanned_failed`/`consecutive_stuck_ticks` on **every** tick, including the overwhelming majority that find nothing due (~17 due rows/min library-wide against 16 loops on a 60 s beat). The console therefore read `scanned_ok = 0` almost always, and every empty tick took the `else` branch and **reset `consecutive_stuck_ticks` to 0** — so "looping without progress" could never accumulate. Fixed: `last_tick_at` (liveness) always written, activity fields only on a tick with `due > 0`. Last-writer-wins across loops remains, by item 14's design. |
| §8f item 15: E4.3's health check + `reclaim_absurd_schedules` move to the supervisor, "once/reconcile, not once/loop-tick" | **CORRECT, but the reconcile beat multiplies the health check 5×** | The gate is "a loop reported a failure OR an outage is open". §8e measured 77 of 146 ticks carrying ≥1 failure, and a standing outage keeps `outage_open` true *indefinitely* — so at a 60 s reconcile (vs the old 300 s tick) the whole-library `source_scan_health` GROUP BY plus `park_source_series` / `record_source_outage` / `trip_subscription_breaker` would run ~1,440×/day instead of ~288. Added `HEALTH_CHECK_MIN_SECONDS = 300` and changed the counter drain from `swap(0)` to `load` + `fetch_sub` so a throttled pass does not discard failures and a concurrent increment is not lost. `reclaim_absurd_schedules` is fine at 60 s (bounded at 25 rows, self-extinguishing). |
| E3's per-source loops are staggered | 🔴 **NOT STATED, AND NOT TRUE — a restart re-created a thundering herd** | `interval()` fires its first tick **immediately** and every loop is spawned in one reconcile pass, so ~16 loops ticked on the same instant and, being periodic from that start, **stayed aligned forever**. Exactly the self-sustaining-cohort pattern `jitter_interval_hours` exists to prevent (handoff constraint 3: ~745 series every 35 min, 43% duty cycle, 154 GB egress), one level up; the global semaphore bounds concurrency but not burstiness. Fixed with a uniform per-loop start offset inside one tick period (`interval_at`, logged as `stagger_ms`). |
| E3's global `Semaphore(12)` bounds aggregate outbound concurrency | **CONFIRMED — and the true worst case is 14** | The permit is a named binding (`let _permit = …`) held to the end of the async block enclosing `scan_due`, so it genuinely spans the upstream fetch. 12 scanner permits + 1 serial discovery `browse_source` + 1 source-sync walk = **14 concurrent scanlator-bound fetches**. Cover (12) / page (16) / bg-materialise (4) are independent `suwayomi.rs` semaphores and are ~99.93% cache-served, so they contend at the Suwayomi instance, not at scanlator sites. |
| E3's `kick_supervisor()` could lose a kick that lands during a reconcile | **NO — verified safe** | `Notify::notify_one` stores one permit when there is no waiter, and tokio's `Notified::drop` re-hands a received-but-unconsumed notification onward, so a kick arriving while the select's `notified()` future does not exist is retained and consumed on the next select. Coalescing is harmless (reconcile is idempotent). Bare `notify_one` on a `OnceLock<Notify>` — holds no lock, cannot deadlock. One call site: `graphql/mod.rs:8473` (`setExtensionSubscription`), per item 13. |
| `heal_null_source_ids` could spin (§8b's infinite-loop class) | **NO — it converges, and is now pinned** | The `SELECT` subquery and the `EXISTS` guard share one predicate, and `source_series.source_id` is `NOT NULL DEFAULT ''`, so a healed row can never return to NULL. `heal_is_idempotent_and_terminates` asserts the second pass affects **0** rows; `the_e3_queries_are_index_seeks_not_table_scans` pins that it `SEARCH`es `source_series` via 0072's covering partial index rather than scanning per row, and that both due-queries ride `idx_scan_state_source_next` with no TEMP B-TREE. |

**Verified CORRECT, no change needed:** E5 first-run baseline returns `(0,0)`; an empty `mangas`
returns early without clobbering the snapshot; `trigger_due_now` is a bare `UPDATE … WHERE series_id
= ?` so an out-of-library LATEST entry matches 0 rows (no work created, no panic); `SNAPSHOT_WINDOW =
40` boundary; malformed `ordered_ids` degrades to "no snapshot" via `.ok()`; loop shutdown +
panic-restart with no catch-up burst; `awaiting` genuinely OFF on every production path
(`policy_mode = comic_type.is_some()` and `persist_scan` always passes `Some(_)`); legacy
`record_scan` is `#[cfg(test)]` and unreachable from production; per-tier jitter factor ∈ [0.95,1.05]
/ [0.9,1.1], strictly positive; restart safety (all state DB-derived);
`reclaim_absurd_schedules` still runs and the horizon invariant holds (14 d park + ≤369 h jitter <
384 h `ABSURD_HORIZON_HOURS`).

**Deliberately not changed:** item 7 (60-day backstop park / `ABSURD_HORIZON_HOURS` raise) stays
deferred; items 4, 8, 12, 13 verified as *intentional* and left alone; `consecutive_stuck_ticks`
stays a shared last-writer-wins counter (item 14 explicitly declines the per-source health
rearchitecture — only the bug that made it *always zero* was fixed); the supervisor's two
library-wide COUNTs also went 300 s → 60 s but are index-backed, sub-millisecond over ~14 k rows,
and feed the admin liveness display.

### 8j. Completeness audit — the answer is "no", and the gap is DEPLOYMENT (appended 2026-07-31, read-only audit + orchestrator spot-checks)

**One line: the design is complete and the code is ~95% written, but nothing built after
2026-07-30 21:46 UTC is RUNNING.** Phase F has not been started at all, and the reader half of the
overhaul is entirely unpublished.

**The deployment boundary, measured.** Running image `komika-server:latest` `sha256:479f14d9`, built
**2026-07-30T21:46:55Z**, container up since 21:49:22Z (orchestrator confirmed via `docker ps`).
Verified by `grep -a` of the *extracted* binary for migration SQL text and log literals — filenames
are not embedded, and without `-a` grep silently reports 0 on a binary.

| Layer | Deployed | On disk |
|---|---|---|
| Migrations | **0092** (snapshot has no `source_latest_snapshot`, no `series_scan_state.source_id`, no `browse_catalogue.latest_chapter`) | **0095** |
| Server source NOT deployed | — | `mangadex.rs` 07-30 22:26 (A1b), `config.rs`/`main.rs`/`sync.rs` 10:2x, `graphql/mod.rs` 13:49, `browse.rs` 13:44, `discovery.rs` 13:50 (new file), `catalog/mod.rs` 14:03, `scanner.rs` **14:07** |
| Reader (CF Worker) | **git HEAD (9b792fc)** — proven byte-for-byte: the live hero markup equals `git show HEAD:…/(app)/+page.svelte:117` | 24 files / +2,068 / −481, plus untracked components/routes |

Binary greps confirming absence: `"readable backfill"` = 0, `"discovery pass complete"` /
`DISCOVERY_INTERVAL_SECS` = 0, `"scan supervisor started"` = 0, `"comic-type lookup failed"` = 0,
`"LEFT JOIN series_scan_state sss"` = 0, `"f.en_chapter_count, f.latest_chapter"` = 0. The binary
still carries **5** copies of `last_new_chapter_at IS NOT NULL`; disk has **1**.

**§10 scoreboard: 9 achieved · 2 partial · 2 invalid/unmeasurable · 13 not achieved · 4 not re-run.**
Achieved and verified live: oneshots (F2), F7's structural fix, F9 both halves, F11 (205 failing
series + 1 outage where there were structurally 0), E4/E4.3, E2 reopen latency, feed lag < 20 min,
detected-series-absent-from-feed = 0. Everything gated on E5/Phase E/E3 is **unverifiable until
deploy**, not achieved.

**Two §10 rows are now INVALID as written, not merely unmet:**
- *"Feed cards staler than their series' latest chapter: 369 → 0"* — post-C `released_at` is
  `MAX(release_event.first_seen_at)` (first-source-wins), which **legitimately** precedes the newest
  source's own upload date. The naive probe now reports 578. The correct successor probe — feed rows
  behind their work's ledger MAX — is **1 of 64,935**. ✅
- *"Total scans/day 20,352 → ~2,593"* — the 20,352 baseline was already invalidated by §8e; the
  honest current figure is **22,292/day**, *higher* than the baseline it is measured against.

**§9 pre-checks 1–6 are ALL now closed.** The gating item is no longer a pre-check; it is §8g's
Phase-F blocker.

**INERT-BUT-GREEN RISK REGISTER** (the §8i pattern — code that passes its tests while doing nothing).
Ranked:

1. 🔴 **`discovery::poll_source` / `subscribed_source_ids` — HIGHEST.** All E5 tests exercise only the
   pure `moved_up_or_entered`; **nothing drives an actual pass**. The wiring filters
   `catalog::subscribed_extensions` pkg_names against `SourceInfo.pkg_name` **and** `s.lang == "en"`.
   If either predicate misses (a lang tag of `en-us`, a pkg-name shape change, an empty subscription
   table) the source list is empty and **all of E5 does nothing** — and *silently*: the empty path
   logs at `debug!` and the `info!` fires only when `flagged > 0`. At production log level you would
   see zero output and conclude E5 was healthy. This is Phase E's exact failure shape, one level up.
2. **`catalog::source_latest_snapshot` / `put_source_latest_snapshot` — no tests at all.** A silent
   degradation means every pass re-baselines and triggers NOTHING, forever, with no log.
3. **The MangaDex incremental feed writer** (`mangadex.rs` ~2979-3013) — no test, only caller is
   production, cannot CREATE a row, every failure is `warn!` + `continue`. Green because nothing
   tests it.
4. **C1b's `maxChapterNumber`** — all 16 tests hand-build `Selectable`s with it pre-populated, the
   §8i anti-pattern exactly. The real input depends on `number` surviving in `CHAPTER_FIELDS`; if it
   were dropped, `contenders` empties and `pickDefaultKey` **silently falls back to `byMostChapters`**
   — F12 reverting to the very bug it fixes, with 16/16 still green and nothing logged.
5. **`mirror_feed_row_into_browse_catalogue`'s `MUT` list** is a hand-copied duplicate of
   `catalog::BROWSE_CATALOGUE_MUTABLE` with no test asserting equality. It **already drifted once**
   (§8h). `latest_chapter` is now pinned by the digest; the *next* column added to one list and not
   the other is unguarded again.
6. **`ledger::is_complete`'s memo** is `#[cfg(not(test))]` — no test covers the memoised path.
7. **A5 is system-level inert.** Code, GraphQL field, TS type and fragment are all correct and green —
   and the feature is a no-op, because `chapter.external_url` is populated on **5 of ~1,443,461 rows**.
   Only A1b fills it, and A1b is undeployed. "A5 done" must not be read as "external chapters fixed".

**Contradictions with the plan — corrections:**

| Plan text | Correction |
|---|---|
| §4.13: *"Ship F12 with or after B, not before, or the fix trades a wrong default for a **date-less chapter list**."* | 🔴 **Phase B shipped, but the timestamp was NEVER wired.** `AggregatedChapter` (`packages/types/src/index.ts:695`) is still `{number, title, sources}` — orchestrator confirmed, no date field — and `source.ts` still hard-sets `date = ''` for any chapter the selected source lacks. **So C1b/F12 will ship with exactly the cost §4.13 said to avoid.** This appears in NO status table. |
| Handoff constraint 5: *"`round(number * 100)` is THE chapter key, **implemented once** in `chapter_label::ChapterLabel::key()`."* | **FALSE — implemented twice**: `chapter_label.rs:66` and `graphql/mod.rs:1509` (orchestrator confirmed). The `graphql` copy is the one admin chapter-hiding uses. They agree today; **no test pins them together**. Divergence would silently break admin chapter-hiding and re-key the ledger. |
| §7z: *"**A1b … does not exist.** There is no such function anywhere in `apps/server/src`."* | **STALE — it exists** at `mangadex.rs:2606`, spawned at `main.rs:1392`, with a test at `mangadex.rs:3840`. Actively misleading; should be deleted from §7z. |
| §7z: *"`catalog::work_source_chapters_from_spine` is written, tested, and **not wired in**."* | **STALE** — that identifier exists nowhere; the switch is flipped (also recorded in §8h). |
| §10: *"Suwayomi series with chapters but no feed row: 9,851 → 0 (**Phase A**)"* | **Wrong twice**: wrong phase (§8b corrected it to the F3 gate) and wrong unit (§8h corrected it to **1,422 works**, not 9,851 series). |
| §7 Phase E exit: *"`awaiting` re-scoped off the 3 h tier"* | Superseded by §8f item 4 — `awaiting` is disabled **entirely**. §10 still states the weaker target. |
| §7z "Still not started" lists F, the F3 gate and the Browse label | The F3 gate and Browse label **are now built** (§8h). Only **F** is genuinely not started. |

**Operational traps for deploy day (not bugs — deploy-ordering facts):**
- **`SCAN_TICK_SECONDS=300` becomes a silent no-op** under E3 — `scanner::spawn` takes it as
  `_tick_seconds` and per-source loops use `SOURCE_TICK_SECONDS = 60`. Set `DISCOVERY_INTERVAL_SECS`
  explicitly in `deploy/docker-compose.yml` rather than relying on the 900 s code default.
- **Server-first is strictly safer.** `browse_catalogue.latest_chapter` only exists after 0095;
  `Series.latestChapter` rides `OPTIONAL_SERIES_FIELDS`, so a reader-first deploy degrades gracefully
  but a server-first one cannot break.
- **A5 ships as a visible no-op until A1b's ~8,779-batch drain completes.** Same image, so it
  self-resolves — but do not report "external chapters fixed" on deploy day.
- **C3's reconciler has run exactly once, ever** (`ran_at 2026-07-30T21:54`, `drifted 1`). Its
  "zero drift over 24 h" exit criterion has never been evaluated a second time.
- 🔴 **Aggregate outbound concurrency goes 3 → 12 (worst case 14).** §7 E3.4 sanctions a global
  semaphore of ~12–16, but handoff hard constraint 2 reads *"`SCAN_CONCURRENCY` stays at 3"*.
  Per-source is 3; the **aggregate is 4× today's**. Given "bans on small scanlator sites are the
  worst outcome", this needs explicit owner confirmation rather than being assumed satisfied.

### 8k. The deferred cadence items RE-EXAMINED, and the inertness register closed (appended 2026-07-31, Agents G + I)

**§8f item 7 (60-day backstop park + `ABSURD_HORIZON_HOURS` raise) — STAYS DEFERRED, now
mechanically guarded.** Re-examined on measurement rather than inherited reasoning. E2's 60-day
figure was specified as an improvement over *never polling* paused series; the as-built 14-day park
is **strictly better coverage than the backstop it would replace**, so only a cost argument remains —
and it loses to Phase F. Measured on the 2026-07-31 snapshot: **7,601** paused Suwayomi
`source_series` (~543 fetches/day), of which **6,753 (89%) are `all.mangadex`** and are retired
outright by Phase F — taking the park bill to **~61/day for free**, versus the ~416/day (2.4% of the
~17k/day budget) a 60-day park would save, while quadrupling the safety-net latency of exactly the
**96** series (athreascans 94, suryascans 2) on *unsubscribed* sources that receive no E2/E5 trigger
at all. Wrong lever. Separately: **the reclaim backlog is fully drained** — `MAX(next_scan_at)` is
15.4 days out with **0** rows past the 16-day horizon — so raising `ABSURD_HORIZON_HOURS` would only
widen a net that currently catches nothing.

**The invariant is no longer a doc comment.** It is now a `const _: () = assert!(MAX_SCHEDULED_HOURS
< ABSURD_HORIZON_HOURS)` — it fails the **build**, not the deploy — plus
`every_park_writer_stays_below_the_absurd_horizon` and `park_jitter_is_never_degenerate`. Verified
the guard actually fires: compiling with a 60-day park yields `error[E0080]: evaluation panicked`.

**NEW FINDING — there are THREE park writers, not one, and one has undocumented one-sided jitter:**

| Writer | Base | Jitter | Worst case |
|---|---|---|---|
| `park_paused` | 336 h | ±(336/5)/2 = ±33 h | **369 h** |
| `park_source_series` (E4.3 outage) | 168 h | **+168/5 — ONE-SIDED, +20% not ±10%** (previously undocumented) | **201 h** |
| `record_scan{,_policy}` | ≤ `MAX_INTERVAL_HOURS` = 336 h | ×1.10 | **369.6 h** |

Max **370 h < 384 h** (`ABSURD_HORIZON_HOURS`), margin **14 h**. A 60-day park would require a
horizon above **1,583 h**.

**§8f item 8 (CANCELLED-disagree → 72 h) — the RECORDED RATIONALE IS WRONG; the conclusion survives
for a different reason.** §8f states *"the per-**source** status the rule needs is not cleanly
modelled (status is per-**work**)"*. **That is false.** `suwayomi_series.status` (migration 0022) is
per-source-series, is rewritten from the live `SuwayomiManga.status` on **every** scan via
`series_cache::put_series`, and is the value `scanner::effective_status` already consumes; the
sibling lookup would mirror `scan_comic_type` exactly. It stays dropped because it is **redundant and
net-costly**: of 432 CANCELLED series, **216** have a disagreeing sibling; **209 of those are on
subscribed en sources**, where E5's LATEST diff wakes them in ≤15 min — **~576× faster** than a 72 h
tier — and the remaining **7 are `all.mangadex`** (firehose-covered, Phase-F-retired). The rule would
add ~57 fetches/day to buy a *slower* signal than the one already running for every member of the
cohort.

**§8j risk-register item 1 (E5 silent inertness) — CLOSED, claim CONFIRMED.**
`subscribed_source_ids`' empty paths logged **nothing at all** (not `debug!` as the register said);
the only per-pass trace was a `debug!` an inert pass reached with `sources=0`, indistinguishable from
a healthy quiet pass at production log level. Minor correction: E5 did emit one `info!`
(`"discovery scheduler started"`) at boot — but it fires identically whether E5 works or is dead, so
the operational conclusion is unchanged. Fixed: zero-source/zero-polled passes now `warn!`; every
completed pass `info!`s with `sources` + `candidates`; the two empty-predicate causes are separately
diagnosable ("no enabled subscription" vs "subscribed but matched no English source", the latter
carrying `pkg_matches` so *not installed* is distinguishable from *installed, wrong lang*).

**MUTATION-PROVED, and this is the important part.** Injecting the exact Phase E bug shape (call
`subscribed_extensions`, discard the result → empty source list) makes the new
`a_real_discovery_pass_resolves_sources_diffs_and_wakes_only_the_mover` fail with *"discovery
resolved ZERO sources — E5 is inert"* — while **all 8 pre-existing pure-diff tests stay green**. That
is the demonstration that the old suite was blind to this bug class, not merely an assertion that the
new one is better. Built on an in-process fake Suwayomi GraphQL origin (raw TCP, no new dependency)
because `SuwayomiClient` is a concrete struct, not a trait; the fixture includes a `ja` sibling and an
unsubscribed `en` source as decoys so **both** filter predicates are exercised.

**§8j risk-register item 2 — CLOSED (coverage), with one residual documented.**
`source_latest_snapshot`/`put_source_latest_snapshot` now covered against a real migrated pool: JSON
round-trip incl. order and full i64 range, in-place upsert (no twin row), `Some(vec![])` ≠ `None`,
and malformed→`None` across 4 junk payloads. **Residual:** `.ok()` makes a corrupt `ordered_ids`
indistinguishable from "never seen", so an affected source re-baselines on **every** pass and can
never flag anything — permanently, silently, while each pass still reports success. Same
silent-inertness class as Phase E and the E5 logging gap. A `warn!` before the degradation closes it
(the degradation itself is correct — re-baselining is the safe recovery).

**Chapter key — divergence pinned pending unification.** Confirmed twice-implemented
(`chapter_label.rs:66`, `graphql/mod.rs:1509`), so handoff constraint 5's "implemented once" is
false. `the_key_agrees_exactly_with_the_duplicate_implementation_in_graphql` transcribes the
`graphql` expression verbatim and drives both over 27 hostile inputs — including the binary-fraction
hazards where `1.005` rounds **down** and `2.675`/`8.615` land on an exact `.5` and round **away** —
plus `f64::MAX/MIN/±INFINITY/NAN` cast saturation, the `-1` Suwayomi sentinel, and a check that the
`x:` unnumbered namespace can never parse as an i64.

**New instruments built (§10 required them; §8j found neither existed):**
1. **Per-tier achieved-vs-target drift** — sampled on the scan write path at **zero extra queries**
   (`prior.last_scanned_at` is already in the transaction's read), reported from the supervisor,
   throttled to 900 s, warning above §10's 10% with a 20-sample floor. **Trigger-driven scans are
   excluded** (`next_scan_at == DUE_NOW_SENTINEL`): E2/E5 triggers legitimately arrive early, and
   including them would produce a large negative drift that masks a real positive one.
2. **Batch-saturation alert** — per-loop, warns after **2 consecutive** saturated ticks (a single one
   is an ordinary cold-start drain), per-loop so a wedged source is attributable.

**`SCAN_GLOBAL_CONCURRENCY` lever added** (default 12, clamped `.max(1)` — a 0 would deadlock every
loop). Per-source `SCAN_CONCURRENCY` stays **3** and is deliberately NOT env-configurable ("this is
the safety constant"). Setting the global knob to 3 reproduces pre-E3 aggregate behaviour exactly and
is asserted. `the_concurrency_knob_matches_the_compose_wiring` asserts the literal
`SCAN_GLOBAL_CONCURRENCY: ${SCAN_GLOBAL_CONCURRENCY:-12}` is present in `deploy/docker-compose.yml`,
so the knob cannot silently go inert. Caveat: the knob bounds the 12 *scanner* permits; the true
worst case remains **14** (+1 discovery `browse_source`, +1 source-sync walk).

### 8l. Feed/reader gaps closed — and §8h's flood measurement does NOT transfer to the resolver (appended 2026-07-31, Agent H + orchestrator)

🔴 **CORRECTION TO §8h, and it is product-visible.** §8h measured "0 of the top 20 / 3 of the top
100 are newly admitted" on the ledger-clocked `updatesFeed` **writers**. The `graphql::updates`
**RESOLVER** — which the reader's HOME "Latest Updates" row calls — orders by
`suwayomi_series.latest_chapter_at`, **not** the ledger clock. So that measurement does not transfer,
and after the gate's removal the home row's page 1 visibly changes:

| | Newly admitted |
|---|---|
| Top 20 | **9** (anonymous/NSFW-filtered: 10) |
| Top 100 | **23** (anon: 23) |
| Top 500 / top 1000 | 112 / 261 |

**This is honest, not a flood:** every one of the top-20 newcomers carries a real **2026-07-31**
clock — they are genuinely today's releases (`En and Yukari`, `TOUGH Chapter 2`, `RANDOM`, …) — and
the resolver orders by the same value the card prints, so the ordering cannot lie. The tail still
sinks (visible age at offset 900 improves from 10 days to 7). But "the long tail sinks rather than
floods" does not lead a reader to expect 9 of the top 20 to change, so it is recorded here.
Membership goes 2,060 → 14,169 series (anon 1,984 → 13,726).

**§8h's "the LEFT JOIN is the load-bearing half" is true of the WRITERS and FALSE of the resolver.**
Driving from `suwayomi_series`, **14,169 of 14,182** in-library series already have a
`series_scan_state` row — only 13 do not. On this resolver, dropping the NULL test alone *is*
essentially the whole widening, and the surviving `EXISTS(scan-state row)` costs almost nothing.

**Query plan after removing the gate + the `INDEXED BY` hint.** Dropping the hint was mandatory, not
cosmetic: `idx_scan_state_detected_series` is **partial** (`WHERE last_new_chapter_at IS NOT NULL`)
and cannot serve the widened predicate. All four EQP lines remain covering-index SEARCHes, no SCAN,
no TEMP B-TREE; the scan-state probe becomes *covering* via
`sqlite_autoindex_series_scan_state_1` (the widened predicate is a pure PK equality). **Page 1:
1.6 ms → 0.5 ms.**

**NEW COST, found and then CLOSED.** Removing the gate made the resolver's `total` COUNT go
**13.8 ms → 53.4 ms warm** (anonymous), because the NSFW subquery no longer short-circuits behind the
gate: it now runs over 14,169 rows instead of 2,060. Breakdown: bare `in_library` scan 0.58 ms;
+ enrolled subquery 7.2 ms; + NSFW subquery 53.1 ms.

The `NOT IN` anti-join fix was **REJECTED**: it measures 37.3 ms (less benefit) *and* would break
`NSFW_FILTER`'s documented token-for-token sync with `series_cache::NSFW_GATE_SQL` — two copies of an
NSFW predicate drifting apart is how adult content leaks to anonymous viewers, which is far worse
than a slow COUNT.

**Shipped instead: `total` is memoised behind the existing `browse::COUNT_TTL`**, reusing
`clear_count_cache` (`catalog::refresh_browse_catalogue` is the single invalidation event for both,
so there is ONE memo, not two — two caches would mean two chances to forget to clear one).
`browse::{COUNT_TTL, cached_count_with, store_count_with}` became `pub(crate)`; the cache's contents,
TTL, cap, eviction policy and `NSFW_FILTER` are untouched. Keyed on `show_nsfw` — the one dimension
that varies the result (that COUNT takes exactly one bind) — under a `updates:v1|` prefix disjoint
from Browse's `v3|`. `count_sql` is **passed in** rather than duplicated, so the resolver stays the
single owner of `DETECTED`/`NSFW_FILTER` and `total` can never describe a different row set than the
pages.

| Path | Cost |
|---|---|
| memo miss (first request per audience per 60 s) | 53.4 ms — the floor, unchanged |
| **memo hit** | **0.117 µs/op** (200k ops, `rustc -O`) |
| worst-case DB work | 2 misses / 60 s ≈ **0.18% of one core** |

**This over-corrects rather than merely closing the regression:** the home row previously paid
13.8 ms of COUNT on *every* anonymous request; it now pays ~0.1 µs on every request and 53.4 ms at
most twice a minute. Guarded by
`the_updates_total_memo_is_keyed_by_audience_and_cleared_by_the_rebuild`, which runs with a **real
60 s TTL** (`COUNT_TTL` is `ZERO` under `cfg(test)`, so a test that did not control it could never
observe the memo at all) on a fixture where the audiences genuinely differ — warm, the anonymous
viewer must still get 1 while the opted-in gets 2, so a single un-keyed entry fails it.

**The MangaDex-half incremental feed writer — all three defects fixed.** Extracted to
`mangadex::flush_touched_feed_rows`, backed by new `catalog::publish_mirror_feed_row`. It now runs
the rebuild's whole mirror chain narrowed to one work (`feed_updates` upsert → `feed_series_updates`
upsert → comic-type fill → `en_chapter_count` fill → ledger projection), so **it CREATES a row**
where the old code ran only pass (5), an `UPDATE … FROM`, and created nothing. No longer silent:
`SweepOutcome.feed` counts published works and is logged. **Convergence is structural where
possible, asserted where not** — `feed_updates_select(scope)` and a `work_id` scope on
`fill_comic_types`/`fill_en_chapter_count` mean passes 1/3/4/5 are literally the rebuild's code, not
a copy; only pass (2)'s `ON CONFLICT` is restated, deliberately omitting
`chapter_count`/`detected_at`/`suwayomi_thumbnail` (the scanner owns those — copying
`chapter_count = excluded.…` would write the mirror's NULL over a real Suwayomi count, the F4 trap).
`the_mirror_half_incremental_write_converges_with_the_periodic_rebuild` digests **every**
`feed_series_updates` column plus `typeof(released_at)` across four interleavings, including the
firehose landing on a work the rebuild had already settled (label advances to 14 while the Suwayomi
`chapter_count = 13` survives). Bonus: `feed_updates` is now maintained incrementally too, so
`canonicalUpdates` stops being 6 h stale.

**F12's date is wired at last (§8j's §4.13 finding closed).** `chapter.released_at` →
`WorkChapterRow.released_at` (`COALESCE(readable_at, published_at)`, the ledger's own clock) →
`AggregatedChapter.first_released_at` = **MIN across sources** (first-source-wins, so the date does
not move as the reader switches translator) → schema → `packages/types` → `source.ts`, replacing the
hard-coded `date = ''`. **Named `firstReleasedAt`, NOT `releasedAt`, deliberately:** the client's
unknown-field stripper matches by name across *every* document, and `UpdateFeedRow.releasedAt`
already exists — sharing the name would let one older server silently strip the updates feed's date
for the whole session. Registered in `OPTIONAL_SERIES_FIELDS`, so a reader ahead of the API degrades
to the pre-F12 no-date row.

**Also closed:** the chapter key is now implemented ONCE — `graphql::chapter_key` delegates to
`chapter_label::ChapterLabel::key` and the duplicate arithmetic is deleted (handoff constraint 5 is
true again). `ledger::is_complete` gained a testable `ReadyMemo` with an injectable clock; two tests
prove the latch answers *without re-querying* (by emptying `release_event` under it) and that an
empty DB can never latch it, and `ReadyMemo::forget()` makes an early latch recoverable.
`source_latest_snapshot` now `warn!`s with source id, error and a payload head before degrading to
`None` (§8k's residual — closed). The two reader latents are fixed: `pageArg()` at all 14 call sites
(normalising rather than widening `$page: Int` → `Int!`, so the server's default decides what "no
page" means), and the `as TranslatorOption` cast is gone — its root cause was resolving a candidate
against `ordered` but reading it out of `translators`, which could have **shown one source's
chapters under another source's name**.

**The browse-mirror drift class is now structurally impossible, not merely guarded.**
`catalog::BROWSE_CATALOGUE_MUTABLE` is `pub(crate)`; `scanner.rs` re-exports it rather than copying
it, and the interim `include_str!`-parsing equality test was **deleted** (test count 492 → 491) —
detecting drift was replaced by preventing it.

### 8m. Phase F BUILT and staged — and §7's step 4 would have freed zero scan budget (appended 2026-07-31, Agent F + orchestrator)

Built in a new `apps/server/src/phase_f.rs` (~1,830 lines) + migration **0096**
(`all_mangadex_uuid`), wiring in `main.rs`, step 5 in `sync.rs`. **Nothing destructive was run
against production**; the whole pipeline was exercised end-to-end against *mutable copies* of the
existing snapshot.

🔴 **§7 Phase F step 4 is incomplete in a way that costs the phase its ENTIRE stated benefit.**
The scheduler picks work with `SELECT series_id FROM series_scan_state WHERE next_scan_at <= ?`
(`scanner::due_series_ids_for_source`) and joins **nothing**. **10,479 of production's 14,169
`series_scan_state` rows (74%) are all.mangadex series.** Deleting the `source_series` rows alone
therefore frees **zero** scan budget and leaves 10,479 live schedules scanning series with no
catalogue mapping at all. Phase F now retires the scan-state row in the same transaction, guarded by
"no other `source_series` still claims this Suwayomi key". Without this, §10's "all.mangadex rows
10,422 → ~0" would have been reported as achieved while the scan load it exists to remove stayed
exactly where it was.

**§8g's gate reproduced EXACTLY** on a copy of the 2026-07-31 snapshot: `total 10479, redundant
9929, mismatch 54, anchorless 496, unresolved 0`. Full pipeline result:

| Stage | Result |
|---|---|
| Gate before | passed **9,929** / failed **550** (54 mismatch + 496 anchorless) |
| Merge (step 2) | considered 486 works, acted 486, blocked 0 |
| Split (step 4) | considered **64** rows, acted 64, blocked 0 |
| **Gate after** | **passed 10,479 / failed 0 / chapters_at_risk 0** |
| Delete (dry) | deletable 10,415 · cascading_chapters 366,340 · withheld_split 64 · scan_states 10,415 |
| Delete (applied **on the copy**) | 10,415 deleted, 366,340 chapters cascaded exactly, 64 rows survive, **0 orphans anywhere** |

**Corrections to §8g:**
- **"~40 of the 54 carry MORE chapters" is true but reads as lost content — it is not.** That
  comparison is against the work's *own* anchor, i.e. a **different** MangaDex entry. Against the
  direct mirror of the **same** UUID, **0 of 54** carry more chapters. It is a mis-merge, not unique
  content at risk.
- **The 496/486 split hides a case that changes the design.** **10 anchorless works carry TWO
  all.mangadex rows whose UUIDs are owned by two DIFFERENT twins.** A merge can satisfy only one; the
  other becomes a mismatch row on the survivor. So step 4 handles **64** rows (54 + 10), not 54.
- **"Create the missing direct anchor" is IMPOSSIBLE, not merely worse.** All 54 mismatch UUIDs
  already exist as a `source_type='mangadex'` row on another work *and* in `work_external_id`;
  `UNIQUE(source_type, source_id, source_key)` and `PK(provider, external_id)` are global, so the
  anchor cannot be created here without stealing it. **SPLIT is the only executable option** — one
  reversible `UPDATE` with the undo recorded in `prev_work_id`, deleting nothing. The alternative
  (folding the twin in) would destroy 51 works to paper over a mapping error.
- **`work_redirect` resolution is a no-op on today's data** (0 `source_series` rows sit on a
  redirected work id) but must stay in the gate, because step 2 merges works away.
- Suwayomi holds **10,628** mangas on `2499283573021220255` and **13,439** across all 61 ids, vs
  10,479 `source_series` rows. UUID resolution is **10,479/10,479 with 0 conflicts**.

**Migration home, justified.** `source_series.source_url` cannot be the durable home — it dies with
the row Phase F deletes. `work_external_id` is impossible — its PK is `(provider, external_id)`,
globally unique, and all 496 anchorless + 54 mismatch UUIDs are already registered there to a
*different* work. So 0096 is a standalone ledger with **no FK deliberately** (an FK would either
cascade the audit trail away or block the delete), carrying `disposition`/`prev_work_id` so merge and
split are reversible.

**Safety as staged.** The delete requires `--apply` **and**
`--confirm yes-delete-all-mangadex-rows`, and refuses to run if the gate reports a single failure —
proven by `the_delete_is_inert_without_both_opt_ins_and_refuses_a_dirty_gate` and by a live run on a
copy where a fully-opted-in delete aborted with *"gate reported 550 failing row(s) (26326 chapters at
risk) — refusing to delete"* and touched 0 rows. The 54 splits are additionally withheld from the
delete by an explicit `disposition='split'` carve-out. UUIDs are read from Suwayomi's **local**
GraphQL (a DB read, no upstream fetch, no mutation); a UUID disagreeing with a stored one is reported,
never overwritten.

**Owner run order** (each stage exits when done; migration 0096 applies at startup; **take a predeploy
backup first — the delete is irreversible**):
```
phase-f resolve --apply    # step 1, writes only UUIDs
phase-f report             # expect 9929/54/496, gate 9929/550
phase-f merge --apply      # step 2, 486 works
phase-f split --apply      # step 4, 64 rows
phase-f report             # MUST read: GATE passed=10479 failed=0
phase-f delete             # dry run: deletable=10415, withheld_split=64
phase-f delete --apply --confirm yes-delete-all-mangadex-rows
```

**Still outstanding after this:** removing the series from Suwayomi's own library is an engine-side
mutation, deliberately out of scope; `feed_series_updates`/`browse_catalogue` are left to the
periodic rebuild (same as the admin `mergeWorks` path).

⚠️ **`discovery::poll_source` still polls all.mangadex's LATEST every tick — and this must NOT be
"fixed" before the delete runs.** It is wasted work only *after* the rows are gone. Today those
**10,479 series are still enrolled**, so adding the obvious `phase_f::is_retired_pkg` filter to
`discovery::subscribed_source_ids` now would strip their E5 fast-detection path for the entire
window between this build and the delete. (Coverage would not vanish — they are MangaDex works and
the 15-min firehose covers them, which is *why* they are redundant — but it is a silent behaviour
change to 74% of the scan-state table, made for a saving of ~96 HTTP requests/day.) The exclusion is
therefore a **post-delete follow-up**, and the safest general form is "skip a source with zero
enrolled series", which is correct in either order and needs no coupling to Phase F at all.

### 8n. The Browse chapter label never reached HOME (appended 2026-08-01, post-deploy)

**Symptom.** After the deploy, Browse printed `"115 ch · Ch. 90"` correctly while home printed
`"17 chapters"` with no chapter number at all.

**Measured.** `discovery` (home's POPULAR / TRENDING / RECENTLY_ADDED) returned
`latestChapter: null` for **0 of 50** items; `search` (Browse) returned it for **30 of 30**, and it
differed from the count on 18 of them. The same work — *A Painter Who Draws Dungeons* — came back
`"17"` via search and `null` via discovery. **Same GraphQL fragment, same type, different resolver.**

**Root cause.** `graphql::assemble_series` hardcodes `latest_chapter: None`, and that is CORRECT and
must stay: a `SuwayomiManga` carries `chapters.total_count`, a COUNT, and no label, so deriving one
there is F4 exactly. Browse sidestepped it by reading `browse_catalogue` directly in `browse.rs`.
Every other list path funnels through `graphql::map_series_batch`, which hydrated the sibling field
`latest_chapter_at` from a batch query but never refilled `latest_chapter`.

**Fix.** `suwayomi_latest_chapter_label_batch` — one query per page reading
`browse_catalogue.latest_chapter` (a verbatim copy of `feed_series_updates.latest_chapter`, itself
the ledger projection's own value), joined via `source_series.source_key`. TEXT-to-TEXT against a
bound TEXT parameter, no CAST on a column, so 0072's partial index still serves it as a seek (§8b).
Fill-only, never overwrite. Guarded by
`discovery_rows_carry_the_latest_chapter_label_not_the_count`, whose fixture makes count and label
DISAGREE on purpose (7 chapters, newest numbered 90) so a regression that reaches for the count
returns "7" and fails loudly rather than silently printing a count under a "Ch." label.

**Verified live:** discovery 0/50 → **50/50** carrying a label, **19 genuinely differing** from the
count (*Dusk Divide*: 28 chapters, newest **Ch. 24**). Home now renders `Ch. 17 · 17 chapters`.

**Scope correction to §7z.** The plan specifies this label for **Browse only** (§7z lines 1544-1571,
§8 line 1646 — all say "Browse should show both"). Home was never a stated requirement, yet
`(app)/+page.svelte` already built `[latestCh, count].join(' · ')` and was simply never fed. Note the
two pages order it differently — home renders `Ch. 90 · 7 chapters`, Browse renders `7 ch · Ch. 90`.
Both are deliberate in their own comments; they are now visibly inconsistent and want unifying.

---

## 9. Risks and open questions

**Blocking pre-checks (do before writing code):**

1. ~~**Does the daily source sync refresh `status`?**~~ **RESOLVED — no, it does not.** See F10
   (§4.11). Reopen detection is the scanner's own 14-day park, and "never fetch paused" was already
   considered and rejected. Resolution is Phase E2's trigger, not a longer park and not nothing.
2. **Are the 463 anchorless all.mangadex UUIDs still resolvable upstream?** Some may be deleted or
   licensed. Determines whether Phase F can complete or leaves a permanent residue.
3. ~~**Do all 13 subscribed extensions actually support LATEST?**~~ **RESOLVED 2026-07-30 — yes.**
   Enumerated against the live engine (`sources { supportsLatest extension { pkgName isInstalled } }`):
   **all 15 sources that hold in-library series report `supportsLatest: true`**, including
   `all.mangadex`. 5 of the 129 installed sources do not support LATEST, but none of them carries an
   in-library series. E2's 90% coverage figure stands and is not reduced by this.
4. ~~**Is `suryascans` (209 series) alive?**~~ **RESOLVED — no.** The source rebranded to
   **Genz Toons / `genztoons.org`** and its chapter endpoint 404s for every series; the extension was
   uninstalled 2026-07-30. This is what exposed F11. Remaining work is E4.4 (53 orphaned works).
5. ~~**Did the suryascans uninstall actually propagate to Suwayomi?**~~ **RESOLVED 2026-07-30 — no,
   it did not.** The extension `en.suryascans` v1.4.54 is still installed and its source
   `1061713767402958340` is still present, now displaying as **"Genz Toons (EN)"**, `isInstalled:
   true`. Only the `extension_subscription` row was removed, which stops the *discovery walk* and
   nothing else — the 209 series kept being scanned every cadence and kept recording success. A live
   `fetchMangaAndChapters` probe on two of them still returns **HTTP 404 in ~0.78 s**. Handled by
   E4.3's outage park rather than a manual pass; see §8a.
6. **How many other sources are silently serving cached chapters right now?** Unmeasurable until E4
   lands — which is the argument for landing E4 first. The E3.5 probe is the manual stand-in: it calls
   the raw mutation and therefore sees the truth our client hides.

**Risks:**

| Risk | Severity | Mitigation |
|---|---|---|
| Ledger seeded with `now()` floods `/updates` with 1.3 M back-catalogue events | **Critical** | Seed from `min(released_at)`; assert `MAX(first_seen_at) <= now` and spot-check page 1 on a snapshot before deploying |
| Phase F cascade-deletes chapters + ledger rows | High | Non-cascading `first_source_series_id`; run F only after C; snapshot before |
| Incremental writer diverges from the rebuild | Medium | Extend `incremental_write_converges_with_the_periodic_rebuild`; C3's reconciler reports drift |
| 3 h tier regresses cover latency (shared Suwayomi budget) | Medium | Total load *drops* (~24,200 → ~22,000/day), but it redistributes toward bursty 3 h polling — measure cover p95 before/after and be ready to back out |
| E2 trigger misses a reopen (source drops LATEST support, or churn outruns top-30) | Medium | 60-day backstop park means "delayed", never "permanently blind"; alert if a subscribed source returns an empty LATEST |
| Hard target silently slips under load (achieved = target + queue delay) | Medium | Capacity budget says 17–58% utilisation; alert on `due == DUE_BATCH_LIMIT` for consecutive ticks and track achieved-vs-target drift per tier |
| Backfilling 35 k `external_url`s hits MangaDex rate limits | Low | Batch `ids[]=` 100 at a time, off the hot path, resumable cursor |
| `chapter` grows by 563 k Suwayomi rows | Low | ~1.6 M total; the table is already indexed on `source_series_id` |

**Open questions — ALL RESOLVED 2026-07-31 (see §7z "Open owner questions — RESOLVED"):**

* Oneshots in `/updates`: **YES** — they are the latest added chapters of a newly-added series.
* 216 `content_rating IS NULL` + `is_nsfw = 1` works: **leave alone** (session decision).
* Cross-source chapter-*title* disagreement: **closed as redundant** — the series-detail chapter list
  is driven by the selected source, so there is no tiebreak to adjudicate.

---

## 10. Verification and reproduction

**Baseline to re-run after every phase** (all measured 2026-07-30):

| Metric | Baseline | Target after |
|---|---|---|
| Newest 300 upstream EN chapters mirrored | 300/300 | unchanged (regression guard) |
| …visible in `/updates` | 261 | ≥ 261 |
| …hidden by NSFW gate | 39 | unchanged (correct behaviour) |
| Random works short vs upstream | 0/40 | unchanged |
| Suwayomi series with chapters but no feed row | 9,851 | **0** (Phase A) |
| Feed cards staler than their series' latest chapter | 369 | **0** (Phase C) |
| Detected series absent from the feed | 0 | 0 (regression guard — proves no 4th cause) |
| Series whose scan silently served cached chapters | **unknown — unmeasurable today** | 0 undetected; each one reported (Phase E4) |
| Sources with a whole-source outage alert | no such signal exists | 1 alert per outage, not 209 silent successes |
| Orphaned works from the suryascans uninstall | 53 unresolved | 0 — explicitly parked, remapped, or removed |
| Works with no chapter list (oneshots) | 21,422 | **0** (Phase A) |
| Feed cards labelled with a count | all scanner-half | **0** (Phase A) |
| Unopenable external chapters | ~35,000 | **0 blank** — all redirect (Phase A) |
| Total scans/day (measured from logs, not modelled) | 20,352 | ~2,593 (Phase E5) |
| **Scan hit rate** (detections ÷ scans) | **1.081%** | > 10% |
| Upstream fetches per new chapter found | **93** | < 12 |
| Detection latency (median) | 11.94 h | ≤ 15 min (E5) |
| Scanner utilisation | ~36% | ~5% |
| `awaiting` share of all scan traffic | ~43% | re-scoped off the 3 h tier |
| Median scheduled gap, ongoing | 11.94 h | 3 h / 12 h / 24 h **as a target, ±10% jitter only** |
| Series scheduled at exactly 12 h | 93% | 0% — every series on its tier's target |
| Achieved-vs-target drift | n/a | < 10% per tier (proves the target isn't slipping) |
| Paused-series scans/day | ~540 | **~0.4** (Phase E2) |
| Reopen-detection latency (subscribed sources) | ≤ 14 days | **≤ 1 day** (Phase E2) |
| Series with no reopen detection at all | 0 | **0** — backstop park, never "never" |
| Feed lag (chapter mirrored → visible) | up to 6 h | < 20 min (Phase D) |
| all.mangadex `source_series` rows | 10,422 | ~0, with 0 orphaned works (Phase F) |

**Reproduction — snapshot first, never read prod directly:**

```bash
sudo python3 -c "
import sqlite3
src=sqlite3.connect('file:/var/lib/docker/volumes/komika_server-data/_data/komika.sqlite3?mode=ro',uri=True)
dst=sqlite3.connect('/tmp/snap.sqlite3'); src.backup(dst); dst.close(); src.close()"
sudo chown $USER /tmp/snap.sqlite3
```

**Upstream coverage check — note the `readableAt` ordering, per §2:**

```
https://api.mangadex.org/chapter?limit=100&translatedLanguage[]=en
  &contentRating[]=safe&contentRating[]=suggestive&contentRating[]=erotica&contentRating[]=pornographic
  &order[readableAt]=desc&includes[]=manga
```

**External-chapter census** — sample N random `chapter.external_id` from the snapshot, batch
`?ids[]=` 100 at a time, count `attributes.externalUrl != null`. Do **not** use `pages == 0`.

Host notes: `pnpm` is broken on this ARM64 box (use `node_modules/.bin`); there is no `sqlite3` CLI
(use `sudo python3`); Node must be `~/.local/node/bin` (≥ 22.13), not the system Node 18.
