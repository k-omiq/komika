# Sync & scheduler health

**Date:** 2026-07-23 · **Parent:** [`2026-07-23-architecture-review.md`](./2026-07-23-architecture-review.md)
**Snapshot:** 06:00–06:06 UTC. Container up since 2026-07-22T18:35:38 (11.5 h).
**Status:** investigation complete, no code changed, no restarts.

> **Superseded in part.** This document is the *evidence*. Implementation follows
> [`2026-07-23-architecture-decisions.md`](./2026-07-23-architecture-decisions.md):
> writes are serialized through one task **before** `SCAN_CONCURRENCY` is raised
> to 12 (AD-9); the awaiting cohort widens exponentially and is capped at 500
> members (AD-10); failing subscriptions auto-disable after 5 strikes (AD-12);
> `covers.sqlite3` gets a hard 24 GB LRU cap, applied *before* replication
> (AD-21, resolving S7).

---

## 1. Loop inventory

| # | Loop | Spawn | Cadence | Concurrency | On? | Supervised | State |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Series scan tick | `main.rs:960` → `scanner.rs:826` | `SCAN_TICK_SECONDS=300` (default 3600) | `SCAN_CONCURRENCY=3` (`scanner.rs:405`), batch ≤5000 | ✅ | ✅ | **Thrashing** (§2) |
| 2 | Source sync | `main.rs:967` → `sync.rs:74` | 86,400 s | 1 | ✅ | ✅ | **Dark all uptime** (§4, S4) |
| 3 | MangaDex catalogue sync | `main.rs:987` → `mangadex.rs:1290` | 21,600 s, 4 req/s | 1 | ✅ | ✅ | Healthy |
| 4 | Cover-cache drainer | `main.rs:1016` → `cover.rs:580` | 300 s, batch 500 | 1 (AtomicBool) | ✅ | ❌ | Healthy / idle |
| 5 | Comment-media GC | `main.rs:976` → `gc.rs:23` | 3,600 s | 1 | ✅ | ❌ | Idle |
| 6 | Metadata backfill | `main.rs:1002` → `mod.rs:6119` | 300 s, batch 25 | 1 | ❌ **OFF** | ❌ | **Not running** |
| 7 | Bulk source ingest | `ingest.rs:267` | on-demand | `ITEM_CONCURRENCY` | — | ❌ | Idle since 07-20 |
| 8 | On-subscribe kick | `sync.rs:364` | per subscribe | **unbounded** | — | ❌ | Running (mangadex, since 05:54) |
| 9 | Dedup / pHash / notify | — | **no loop** — inline in request/scan paths | — | — | — | `COVER_PHASH=off` |

> **The premise "every sync is turned on" is false.** `METADATA_BACKFILL` is
> unset (`main.rs:1008-1010`); boot log confirms `metadata auto-enrichment
> disabled`. Un-enriched MangaDex-anchored works stay un-enriched forever — this
> is a direct contributor to `work_tag` being empty (see the browse doc, B7).

---

## 2. The scanner is saturated, not stalled

### Throughput

```
76 `scan tick complete` lines over 10.18 h
total_ok = 24,712  ⇒  2,427 successful scans/hour
744 series in 1,190 s ⇒ 1.60 s/series wall (4.8 s/series/worker)
```

### Health signals all look *good* — and that is what misleads

```
series_scan_state rows        13,850     due now                        6
consecutive_failures = 0      13,666     = 1: 2       >= 2: 0
never scanned                      0     null next_scan_at              0
last_scanned <1h               1,443     <1d 5,852    <7d 6,373   >7d 0
awaiting_since NOT NULL        1,164
```

No backlog, no starvation, no permanent failures, panic-supervised, zero ERROR
lines and zero task deaths in 11.5 h. The scheduler keeps up with the schedule it
wrote. **The schedule itself is the defect.**

### Capacity is consumed by a permanent fast-poll cohort

```
awaiting_since NOT NULL (30-min poll)  : 1,164
rows with effective interval < 1 h     : 1,118
distinct series scanned in last hour   : 1,443
successful scans in last hour          : ~2,211
```

**~2,200 of ~2,430 scans/hour (91%) are the same ~1,150 series re-polled twice an
hour.** ~230 scans/hour remain for the other ~12,700 series.

### Coverage and horizon

```
scanned in last 24 h : 7,478 / 13,850 (54%)  — 6,372 untouched >24 h
next_scan_at <= 1 d  : 2,152 (15.5%)
next_scan_at <= 7 d  : 4,582 (33.1%)
next_scan_at <= 14 d : 8,393 (60.6%)  ⇒ 39% scheduled >14 days out
next_scan_at > 1 y   :    44          max: 2033-03-14
avg_interval_hours > 720 h : 3,170    == 0 (→24 h default) : 3,220
```

### Can it keep up?

| Target | Requirement |
| --- | --- |
| Full pass at the raw ceiling (2,430/h, 100% dedicated) | 5.7 h |
| Full pass over the non-hot 12,700 at the ~230/h available | **~55 h (2.3 days)** — matches the observed 54%/24 h |
| Hourly polling of all 13,850 | 13,850/h = **5.7× the ceiling**; needs `SCAN_CONCURRENCY` ≈ 18–20, not 3 |

**Verdict: no, not at hourly granularity — but the bottleneck is
`SCAN_CONCURRENCY=3` plus the unbounded adaptive interval, not a dead tick.**

---

## 3. Confirmed thundering herd

`next_scan_at` is written with **zero jitter** (`scanner.rs:758-761`). The only
jitter in the codebase is on the paused-park path (`scanner.rs:572-574`, added
for "audit #9"); the far more frequent steady and awaiting paths were left
un-jittered.

Cohort heads arrive at exactly 35-minute intervals:

```
01:51:11 745 → 01:52:03 277 → 01:56:32 50 → 02:01:11 2 → 02:06:12 3
02:26:08 746 → 02:26:59 276 → 02:31:28 50 → 02:36:08 2 → 02:41:12 6
03:01:01 746 …  03:35:50 746 …  04:10:44 744 …  04:45:32 742 …  05:20:19 745 …  05:55:12 743
```

DB confirms the cluster:

```
next_scan_at bucket   count
2026-07-23T06:1x        740
2026-07-23T06:2x        296
2026-07-23T06:3x         56
2026-07-23T07:0x         31
```

1,163 of 1,164 awaiting rows have `awaiting_since` < 1 day — one synchronized
cohort created by the boot backfill (`source-sync: backfilled scan-state rows
added=3258` at 18:35:38.81), re-polling every 30 min for up to
`AWAITING_MAX_HOURS = 48` (`scanner.rs:103`).

The ~745-row batch blocks the loop ~19.8 min, so the scanner is **busy ~43% of
wall-clock**. This is what drives Suwayomi to 25–30% CPU and **154 GB of network
egress**.

---

## 4. Defects — CONFIRMED

### S1 · P0 — Deferred transaction causes chronic `SQLITE_BUSY_SNAPSHOT`, unretried

`scanner.rs:676` issues a plain `BEGIN` (deferred), reads `series_scan_state`,
then upgrades to a write at `:770`. Under WAL, if another writer commits in
between, SQLite returns `SQLITE_BUSY_SNAPSHOT` **immediately** — `busy_timeout`
(`db.rs:26`) does not and *cannot* retry this class. 25 occurrences in 11.5 h.
The comment at `:664-667` acknowledges the behaviour and accepts it.

`mangadex.rs:38-44` already has the right primitive (`UPSERT_LOCK_RETRIES = 4` +
`is_locked_error`). The scanner never uses it.

**Fix:** `BEGIN IMMEDIATE` + bounded retry, reusing the mangadex helper.

### S2 · P0 — Lock failures misclassified as upstream failures

`scanner.rs:498-509` routes *any* `persist_scan` error into `record_scan_failure`
(`:526`), which bumps `consecutive_failures` and pushes `next_scan_at` out
30 min → 1 h → 2 h (`ERROR_BACKOFF_BASE_MINUTES`, `:360`). A healthy series that
merely lost a write race is penalised as if upstream were dead. Series 207 and
284 sit at `consecutive_failures = 1` for exactly this reason (failed 05:30:20,
again 06:05:12).

**Fix:** classify lock errors separately — retry, don't back off.

### S3 · P0 — Zero jitter → permanent 35-minute herd

See §3. **Fix:** ±10% jitter on every `next_scan_at` write, not just the park path.

### S4 · P0 — Source-sync reconcile has no retry; transient failure = 24 h blackout

```
18:36:35 WARN sync: source-sync: failed to list library for reconcile
         error=error sending request for url (http://suwayomi:4567/api/graphql)
18:36:35 INFO sync: source-sync: reconcile incomplete — not stamping pass (will retry)
```

Suwayomi wasn't accepting connections 57 s after server start.
`sync.rs:164-170` deliberately skips `mark_source_sync_pass`, and the comment
claims "a restart then retries instead of waiting a full interval" — **that is
false**: `run_loop` (`sync.rs:112`) is a fixed `interval(86400)` with
`MissedTickBehavior::Delay`. Nothing re-fires early.

Confirmed: `SELECT * FROM sync_state` → **0 rows**. The `set_in_library` drift
heal has not run once this uptime. Next attempt 2026-07-23T18:35.

**Fix:** on `!reconciled`, arm a short retry (~15 min) instead of falling through
to the full interval. Also add a readiness wait on Suwayomi at boot.

### S5 · P0 — Boot-time herd: all five loops fire tick #1 simultaneously

`main.rs:960/967/976/987/1016` spawn back-to-back and `tokio::time::interval`
fires immediately:

```
18:35:38.737788 source-sync
18:35:38.737812 scanner
18:35:38.738090 gc
18:35:38.738100 cover
18:35:38.738124 mangadex     ← all within 336 µs
```

Worse, scanner and cover-cache share an identical 300 s period, so absent drift
they collide forever (`18:35:38.738`, `18:40:38.742`, `18:45:38.74`…). This is
the direct cause of the 18:36:43 and 20:01–20:02 lock bursts.

**Fix:** stagger initial ticks; give each loop a distinct period or phase offset.

### S6 · P1 — Three loops unsupervised

`scanner.rs:835`, `sync.rs:80`, `mangadex.rs:1300` have panic-restart
supervisors. `cover.rs:590`, `gc.rs:24`, `mod.rs:6125` do **not** — one panic
ends cover caching / GC / enrichment silently for the process lifetime. None have
panicked yet (0 `restarting in` lines), so this is latent.

### S7 · P1 — Cover blob store has no GC and no bound

`covers.sqlite3` = **20.54 GB**, freelist 0, `work_cover_blob` = **121,366 rows,
avg 176.5 KB** vs 112,602 works ⇒ **~8,700 orphans ≈ 1.5 GB unreclaimable**.
`cover.rs` has no delete path. Excluded from Litestream backup (`db.rs:38-45`).

Note: MEMORY.md targets 150 KB lossy WebP; actual average is 176 KB.

### S8 · P1 — `MAX_INTERVAL_HOURS = 100 years`

`scanner.rs:50`. 217 rows have `next_scan_at > 2027`; worst is series `1535` with
`avg_interval_hours = 58,309` (6.6 years) inferred from **2 chapters**, parked
until **2033-03-14**. Those series will never be rescanned. `MIN_INTERVAL_HOURS`
has a floor; there is no matching practical ceiling. `PAUSED_PARK_HOURS` (14 d)
is the sane cap.

### S9 · P1 — `METADATA_BACKFILL` off

`main.rs:1008-1010`. See header note.

---

## 5. Defects — SUSPECTED

### S10 · MangaDex subscription is an unbounded growth path into a saturated scanner

`eu.kanade.tachiyomi.extension.all.mangadex` maps to **61** Suwayomi sources
(`source_extension`). The daily pass (`sync.rs:295`, `SOURCE_SYNC_MAX_PAGES=10`)
walks 61 × 10 pages, and every genuinely-new manga goes through
`ingest_source_series` → `scan_series` (`mod.rs:5404`) sequentially.
Order-of-magnitude: up to ~15 k new series per daily pass, each permanently added
to a pool already 43% duty-cycled. The pass has **no wall-clock budget and no
early exit**. `library_size` climbed 13,415 → 13,803 between 05:00 and 06:05.

**Recommend:** cap `SOURCE_SYNC_MAX_PAGES` for `all.*` extensions, or budget the
pass by wall-clock.

### S11 · 77 MB WAL inflates read cost and widens the S1 race window

Zero Litestream `checkpoint` lines in 11.5 h; `_litestream_lock` present.
Litestream's long-lived read txn blocks WAL truncation, and sqlx sets no
`wal_autocheckpoint` (`db.rs:14-33`). A large WAL lengthens the read leg of
`record_scan`'s deferred transaction. Verify `pragma wal_checkpoint` behaviour
before acting.

### S12 · Single 8-connection pool shared by loops and all GraphQL traffic

`db.rs:31`. SQLite has one writer regardless, so the pool doesn't *cause*
contention, but a scan-batch write burst and user requests queue on the same
connections. The **10 plain `(code: 5)` timeouts mean some writer held the lock
for the full 15 s** — worth tracing which. Candidates: cover materialisation and
`series_cache::put_chapters`.

### S13 · No circuit breaker on chronically-failing subscriptions

`extension_subscription.last_error`: `drakescans` → `flaresolverr: Temporary
failure in name resolution` (flaresolverr is declared in compose but **not
running**); `suryascans` → `HTTP error 404`. Both **re-stamp `last_synced_at` on
every failure**, so nothing escalates and they are retried daily forever.

---

## 6. Lock-contention evidence

59 WARN lines in 11.5 h, of which **35 are `database is locked`**: 25 ×
`(code: 517)` = `SQLITE_BUSY_SNAPSHOT`, 10 × `(code: 5)` = plain `SQLITE_BUSY`
after the 15 s timeout expired.

```
18:36:43              6409                        (code: 5)   ← boot herd (S5)
20:01:08–20:02:22     10934/10939/10935/10949/10953/10993     ← burst of 6
22:01:21 / 22:32:40 / 00:04:50 / 00:35:24 / 02:46:08   177    ← ×5, same series
05:30:20              284 + 207
06:05:12              284 + 207 again
```

Six concurrent write origins, all in-process, all contending for SQLite's single
writer: scan scheduler, source-sync, MangaDex sync, cover drainer, GC sweep, and
all user mutations.

---

## 7. Plan

### Tier 0 — no rebuild

- Start **flaresolverr** (S13) — every Cloudflare-protected source is failing.
- Set memory/CPU limits on `komika-suwayomi-1` (4.4 GiB, unbounded).
- Run **`ANALYZE`** — the planner has zero statistics (see parent doc; note this
  writes `sqlite_stat1` and needs a go-ahead).
- `docker builder prune` — ~17 GB.

### Tier 2 — batched Rust rebuild, in dependency order

1. **S1 + S2** together — `BEGIN IMMEDIATE` + bounded retry, and split lock
   errors out of the upstream-failure path. These are one change; doing S1 alone
   leaves the misclassification, doing S2 alone leaves the races.
2. **S3 + S5** — ±10% jitter on all `next_scan_at` writes; stagger boot ticks and
   de-align the scanner/cover 300 s period. Expect the 35-min herd to dissolve
   into a flat arrival rate and the lock bursts to drop sharply.
3. **S4** — short retry on failed reconcile + Suwayomi readiness wait at boot.
4. **S8** — cap `MAX_INTERVAL_HOURS` at `PAUSED_PARK_HOURS` (14 d) and backfill
   the 217 rows scheduled past 2027.
5. **S6** — wrap `cover.rs`, `gc.rs`, and enrichment in the existing supervisor.
6. **S13** — circuit-breaker: stop re-stamping `last_synced_at` on failure;
   escalate to auto-disable after N consecutive failures.
7. **S10** — wall-clock budget on the source-sync pass.

### Tier 3 — capacity and storage

- **Raise `SCAN_CONCURRENCY`** — but only *after* S1/S2/S3 land. Raising it now
  multiplies the lock contention rather than the throughput. Target ~8–12 first
  and measure lock-warning rate before going further; Suwayomi at 25–30% CPU is
  the real ceiling, not the Rust server at 2%.
- **Fix the awaiting cohort** (91% of capacity): the 30-min re-poll for up to 48 h
  is too aggressive for 1,150 series. Consider exponential widening of the
  awaiting interval, or capping the cohort size.
- **S7** — cover blob durability and growth. **Decision (2026-07-23):
  `covers.sqlite3` is added to Litestream.** Sequence matters:
  1. **Orphan sweep first** — ~8,700 rows ≈ 1.5 GB. Replicating before GC pays to
     store known garbage.
  2. **`VACUUM INTO` a fresh file** — the freelist is 0, so deletes alone will
     not shrink the 20.5 GB file; in-place reclaim will not happen.
  3. **Then add a dedicated Litestream stanza** with a long `snapshot-interval`
     and small retention. At 20.5 GB, default cadence/retention can leave several
     full copies in R2. The data is derived and regenerable, so the goal is to
     avoid a painful multi-day re-fetch, not point-in-time fidelity. R2 has no
     egress fee and storage is ~$0.015/GB/month, so a tuned config is a
     low-single-digit monthly cost and an untuned one is several times that for
     no benefit.
  4. **Add the missing delete path** (`cover.rs` has none) so the store stops
     growing unboundedly — otherwise the replication bill tracks the leak.

  Note: CF caching of covers is a *read-path* mitigation and is unrelated to
  durability. Both are worth having, for different reasons.
- **S9** — turn `METADATA_BACKFILL` on (needed by the browse Stage-3 work).

### Verification

- `database is locked` WARN count over a 12 h window drops from 35 toward 0.
- `scan tick complete` sizes flatten — no 745-row spike on a 35-minute period.
- `sync_state` has rows; `set_in_library` drift heal runs.
- No `next_scan_at` beyond `now + 14 d`.
- 24 h coverage rises above 54% without raising Suwayomi CPU proportionally.
