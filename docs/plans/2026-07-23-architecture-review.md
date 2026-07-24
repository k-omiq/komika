# Architecture review — browse, sync, and updates

**Date:** 2026-07-23
**Method:** four parallel read-only investigations against the live VPS (container
`komika-server-1`, up 11.5 h at time of snapshot). DB analysed from consistent
snapshots taken via SQLite's backup API from `mode=ro` connections. Live GraphQL
probed on both `localhost:8080` and `api.komiq.cc`. Nothing was modified,
rebuilt, or restarted.

**Companion docs**

| Doc | Scope |
| --- | --- |
| [`2026-07-23-architecture-decisions.md`](./2026-07-23-architecture-decisions.md) | **AD-1…AD-23 — every open item decided. Binding for implementation.** |
| [`2026-07-23-browse-catalogue-filtration.md`](./2026-07-23-browse-catalogue-filtration.md) | Browse filters, series count, the 10,704-vs-112,602 gap |
| [`2026-07-23-sync-scheduler-health.md`](./2026-07-23-sync-scheduler-health.md) | Every background loop, herd behaviour, SQLite write contention |
| [`2026-07-23-updates-and-edge-caching.md`](./2026-07-23-updates-and-edge-caching.md) | Updates page crash, `updated_at` semantics, serving feeds off-VPS |

> **Read order.** These three docs are the *evidence*. The decisions doc is the
> *plan*, and it supersedes them where they differ — it corrects six items,
> listed in its final section. Implementation should follow AD-23's staging.

---

## 1. The single architectural fault

Three separately-reported symptoms share one root cause.

**Komika has two parallel catalogues, and the read path only knows about the
smaller one.**

| | `suwayomi_series` | `work` (canonical) |
| --- | --- | --- |
| Rows | **13,663** | **112,602** |
| Origin | Suwayomi extensions | MangaDex ingest (109,101 `source_series` rows) |
| Powers | Browse, discovery, search, updates, scan scheduler | `canonical_series`, `canonical_chapters`, `canonical_updates` only |
| Genres | populated | **`work_tag` is empty — 0 rows** |
| Indexes | several | **one**, partial and cover-specific |

A canonical migration was clearly *started* — `canonical_series`,
`canonical_updates`, `catalog::latest_english_chapter_at()` and the `w_`-prefixed
id scheme all exist, and the canonical path even computes `updatedAt` correctly.
It was never finished on the read path. The result is a 112,602-row catalogue
that is continuously ingested, cover-cached (20.5 GB of blobs), and then
displayed to nobody.

Every headline complaint falls out of this:

- **"Why only 10,704 series?"** — `search_catalogue` queries `suwayomi_series`
  and never joins `work`. The 100,005 MangaDex-only works are structurally
  unreachable. There is no `QueryRoot` resolver that lists or paginates `work` at
  all.
- **"Filters don't filter."** — the facets that *could* be pushed into SQL
  (status, year, language) live on `work`, which browse never touches. So they
  were implemented client-side over a 20-row page slice instead.
- **"Updates are jumbled."** — the Suwayomi read path stamps `updatedAt` from
  poll time. The canonical path already does this correctly. Only the unfixed
  path is user-visible.

**Implication for sequencing:** the browse fix and the updates fix are the same
project. Both need a canonical read path over `work`. Building them separately
means building it twice. This is the main reason the remediation below is
staged the way it is.

---

## 2. Confirmed state of the system

### Deployment (differs materially from `PRODUCTION.md`)

```
browser ──► komiq.cc          Cloudflare Worker, adapter-cloudflare, edge SSR
              │ 3× POST /graphql (SSR) ─────┐
              ▼                             ├──► cloudflared tunnel ──► VPS :8080
            HTML shell                      │         komika-server-1 (Rust/axum)
              │ hydrate                     │         komika-suwayomi-1 (:4567 loopback)
              └─ 3× POST /graphql ──────────┘
```

- The reader is **not** a static SPA behind nginx. `deploy/nginx.conf` is dead
  code — it ships only inside `reader.Dockerfile`, and that image is not running.
  Its CSP, gzip, and immutable-asset rules apply to nothing.
- Ingress is cloudflared-only. Port 8080 is bound `0.0.0.0` but firewalled off
  from the public IP.
- `PRODUCTION.md` is stale on three counts (static-SPA/nginx claim, Suwayomi
  exposure TODO, `opt-level=z`). Worth correcting so it stops misleading planning.

### Resource envelope

| | Value |
| --- | --- |
| CPU / load | 4 cores, loadavg 0.88–1.12 (~28%) |
| RAM | 23 GiB, 12.5 GiB available |
| Disk | 193 GB, 96 GB used, 98 GB free |
| `komika.sqlite3` | 1.27 GB (+ **77 MB WAL**) |
| `covers.sqlite3` | **20.54 GB**, unbounded, **excluded from backup** |
| Suwayomi | 25–30% CPU, **4.4 GiB**, no memory or CPU limit |
| Docker build cache | 18.5 GB, **17.25 GB reclaimable** |

CPU and RAM are comfortable. The real constraint is disk: `covers.sqlite3` has
no eviction policy, ~8,700 orphaned blobs (~1.5 GB) with no delete path, and
grows on every cover materialization.

### Correcting the record

Two premises in the original report did not survive investigation, and the
corrections change what should be fixed:

1. **"Every sync is turned on"** — false. `METADATA_BACKFILL` is unset, so
   metadata auto-enrichment has never run this uptime (`main.rs:1008-1010`).
   This is part of why `work_tag` is empty.
2. **"Series update tick isn't properly running"** — the tick runs fine:
   24,712 successful scans in 11.4 h, no backlog (`due now` = 6), zero permanent
   failures, panic-supervised. The defect is *cadence*, not liveness — 91% of
   capacity is burned re-polling a ~1,150-series hot set twice an hour while
   12,700 series get ~230 scans/h between them.
3. **"Updates page doesn't have SSR"** — SSR is enabled and confirmed in the
   deployed bundle. It renders zero content because the load function returns an
   unawaited promise. The observation was right; the mechanism is different, and
   the fix is a one-line `await`.

---

## 3. Remediation, staged by blast radius

Staged so that everything reversible and cheap lands before anything that
requires a Rust rebuild — **a rebuild restarts the server and interrupts
ingest**, so server-side changes should be batched into as few restarts as
possible.

### Tier 0 — no deploy, no rebuild, minutes

| # | Action | Risk | Effect |
| --- | --- | --- | --- |
| T0.1 | **Run `ANALYZE`** on `komika.sqlite3` | Low — but it *writes* `sqlite_stat1`; needs explicit go-ahead | Planner currently has **zero statistics** across 112K works / 805K chapters |
| T0.2 | Cloudflare Browser Cache TTL → *Respect Existing Headers* | None, dashboard-only, instantly reversible | Stops `max-age=0` being rewritten to **4 hours**; restores the 30–300 s freshness the code already asks for |
| T0.3 | `docker builder prune` | Low | Reclaims ~17 GB |
| T0.4 | Start `flaresolverr` (declared in compose, **not running**) | Low | Every Cloudflare-protected source currently fails sync with a DNS error |
| T0.5 | Set memory/CPU limits on `komika-suwayomi-1` | Low, needs container restart | It is the box's dominant unbounded consumer at 4.4 GiB |

T0.1 and T0.2 are the highest leverage-to-risk actions available anywhere in
this review.

### Tier 1 — reader deploy only, no VPS restart

| # | Action | Fixes |
| --- | --- | --- |
| T1.1 | Dedupe `newUpdates` before render | **Live production crash** (duplicate `{#each}` key) |
| T1.2 | `await` the updates load promise on the server | SSR renders real HTML; edge cache stops caching an empty shell |
| T1.3 | Render `totalCount` in the browse header | "20+ series" → real number |
| T1.4 | Disable/hide the rating slider | It collapses the catalogue to 3 results |

T1.1 is the only item in this document that is breaking the product *right now*
for every visitor to `/updates`.

### Tier 2 — one batched Rust rebuild

Correctness and scheduler-health fixes. Detailed per-item in the companion docs.

- Scanner: `BEGIN IMMEDIATE` + bounded retry; stop classifying lock failures as
  upstream failures; add ±10% jitter to `next_scan_at`; cap `MAX_INTERVAL_HOURS`.
- Source-sync: short retry when reconcile fails, instead of a 24 h blackout.
- Boot: stagger the five loops' first tick.
- Feeds: `published_at <= now` guard; NSFW-filter before `LIMIT`, not after.
- `updatedAt` semantics: stop sourcing it from poll time.
- Migration: indexes on `work(updated_at)`, `work(primary_title)`, and a global
  `chapter(published_at DESC)`.
- Supervise the three unsupervised loops (`cover.rs`, `gc.rs`, enrichment).

### Tier 3 — architecture

The items that actually close out the three complaints:

1. **Canonical catalogue read path** — a resolver that lists/paginates `work`
   with real server-side facets. Unblocks 100,005 hidden series *and* makes
   filters real. Prerequisite: ingest MangaDex tags into `work_tag`.
2. **Materialized `feed_updates` table** — replaces a per-request 805K-row
   `GROUP BY` (3.5–4.0 s) with an index range-scan.
3. **Cacheable GET read path** (`GET /feed/updates.json`, ETag +
   `s-maxage` + `stale-while-revalidate`) — POST GraphQL is structurally
   uncacheable at any CDN. This is what makes "updates don't hit the VPS" true.
4. **Cover blob eviction + backup decision** for the 20.5 GB store.

---

## 4. Decisions taken (2026-07-23)

### D1 · Full catalogue browsable — all 112,602 works

Browse targets the entire canonical catalogue, not a curated subset. This makes
two Tier-3 items **hard prerequisites** rather than nice-to-haves:

- **`work_tag` must be populated before launch.** It is currently 0 rows.
  Shipping 100,005 genre-less works into a genre-filtered UI would make the
  filters look *more* broken, not less.
- **The scan scheduler must not inherit the catalogue.** 13,850 series already
  saturate it at 43% duty cycle (see the sync doc). Browsability must not imply
  scannability — canonical works are browsed from `work`/`chapter`, and only
  Suwayomi-linked series enter `series_scan_state`. Keep that boundary explicit,
  or the 5.7×-short scanner becomes ~50× short.

Consequence to accept: most of the 112K have no locally cached chapters (47,557
of 112,602 do). Browse should surface readability as a *filter and a badge*, not
by hiding rows.

### D2 · Rating — ingest MangaDex statistics (verified feasible)

Chosen over dropping the facet, because the ingest turns out to be nearly free.
Verified live against `api.mangadex.org` on 2026-07-23:

```
GET /statistics/manga?manga[]=<uuid>&…    → 200
{"statistics":{"<uuid>":{"follows":210015,
                         "rating":{"average":9.6902,"bayesian":9.6840657…},
                         "comments":{…},"unavailableChaptersCount":0}}}
```

- **Batch cap is 100 per request** — measured. Requesting 120 returns HTTP 200
  with only 100 entries, i.e. it **truncates silently**. The ingest must chunk at
  exactly 100 and assert `returned == requested`, or it will drop rows without
  erroring.
- **Full-catalogue pass: 109,101 MangaDex-linked works ÷ 100 = 1,092 requests.**
  At the existing 4 req/s limiter that is **~4.5 minutes**. This is a
  once-a-day-at-most job, not an ongoing burden.
- **`follows` comes free in the same response** and is a far better popularity
  signal than the current chapter-count proxy — it is the natural fix for
  "Trending" degenerating to chapter count (browse doc F3) and for the
  poll-time-ordered POPULAR feed (updates doc U5).

Design: store `rating_average`, `rating_bayesian`, `rating_count`/`follows` on
`work`. **Filter and sort on `bayesian`** — it is MangaDex's own
low-vote-count-corrected value, so a 10.0 from three voters won't top the
catalogue. Keep local `reviews` as a separate, separately-labelled signal; do not
merge the two into one number.

### D3 · `covers.sqlite3` → add to Litestream

Accepted. Two sequencing notes, both cost-driven rather than objections:

1. **Run the orphan sweep first.** ~8,700 orphaned blobs ≈ 1.5 GB (sync doc S7).
   Replicating before GC means paying to store known garbage, and SQLite's
   freelist is 0 so the file will not shrink on its own — plan a `VACUUM INTO` to
   a fresh file rather than expecting in-place reclaim.
2. **Tune snapshot retention explicitly for this DB.** At 20.5 GB, Litestream's
   default snapshot cadence and retention can leave several full copies in R2.
   Give `covers.sqlite3` its own stanza with a long `snapshot-interval` and small
   retention — the data is derived and regenerable, so the goal is
   *avoid a painful re-fetch*, not point-in-time fidelity. R2 has no egress fee
   and storage is ~$0.015/GB/month, so a tuned config is a low-single-digit
   monthly cost; an untuned one is several times that for no benefit.

Note that CF caching of covers is a **read-path** mitigation and does not affect
this decision — the cache is a lossy front for an origin that currently has no
backup. Both are worth having, for different reasons.

### Nothing left open

All remaining architectural gaps were closed on 2026-07-23 in
[`2026-07-23-architecture-decisions.md`](./2026-07-23-architecture-decisions.md).
Highlights that changed the plan:

- **AD-1** — MangaDex tags must *not* go into `work_tag`; it is an admin override
  with full-replace semantics. They get their own `work_source_tag`. Tags ship
  inline in the `/manga` payload already being fetched, so the ingest costs zero
  extra API calls.
- **AD-3** — Browse needs **keyset pagination**. Offset over 112,602 rows with
  the measured temp B-tree sorts degrades linearly; this gap was missed in the
  original analysis and would have surfaced as "browse gets slower the further
  you scroll".
- **AD-9** — Serialize background writes through one task **before** raising
  `SCAN_CONCURRENCY`. Raising it first multiplies lock collisions rather than
  throughput.
- **AD-21** — `covers.sqlite3` gets a hard 24 GB cap with LRU eviction. Backup
  was decided last round; growth was not, and replicating an unbounded store
  means the bill tracks the leak.
- **AD-18** — Ship `/metrics`. Every verification step in these docs currently
  requires a human grepping `docker logs`.

The unbounded-growth issue tracked as S7 in the sync doc is resolved by AD-21.
