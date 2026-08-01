# Komika performance roadmap — measured baseline → optimal

**Date:** 2026-07-27
**Measured against:** the live prod container `komika-server-1` on `127.0.0.1:8080`
(bypassing the Cloudflare tunnel, so these are **origin** numbers).
**Harness:** `apps/server/bench/loadtest.mjs` — every number below is reproducible with it.

| Companion doc | Contents |
|---|---|
| [`apps/server/bench/README.md`](apps/server/bench/README.md) | How to run the harness, how to read it, **what a clean run does not prove** |
| [`apps/server/bench/results/2026-07-27-baseline.md`](apps/server/bench/results/2026-07-27-baseline.md) | Verbatim measurements + raw JSON |
| [`docs/plans/2026-07-27-performance-investigation.md`](docs/plans/2026-07-27-performance-investigation.md) | Method, corrections to the architecture-only estimate, and the wrong turn |

---

## 1. What the box actually is

| | |
|---|---|
| Host | 4 vCPU / 23 GiB, Oracle VPS — **shared** with two unrelated prod stacks (`clubsite-*`, `torchy-*`) |
| Ingress | `cloudflared` tunnel → `api.komiq.cc` → `localhost:8080` (single process, no LB) |
| Server | Rust, axum + async-graphql, `#[tokio::main]` with **no `worker_threads` override → 4 workers** |
| DB | **SQLite** (not client-server), WAL, `synchronous=NORMAL`, 1.5 GiB mmap. Main pool **8 conns**, cover pool **4** (`apps/server/src/db.rs:57`, `:91`) |
| Reader | Cloudflare Worker (SSR at edge), **not on this box** |
| Suwayomi | capped at **3.0 of 4 CPUs**, 8 GiB (`deploy/docker-compose.yml`) |
| Noisy neighbour | `clubsite-worker` measured at **53% CPU** while komika idled at 0.05% |

## 2. Measured baseline (today)

Closed-loop ramp, 2 s warmup + 8 s measured per step.

| Scenario | p50 @ 1 VU | **Saturation** | p95 at knee | CPU at saturation |
|---|---|---|---|---|
| `/health` (framework floor) | 0.2 ms | **7,360 rps** | 1.9 ms | — |
| `search` (browse rows) | 6.6 ms | **223 rps** | 52 ms | — |
| `facets` (genre aggregate) | 53 ms | — | — | — |
| **`discovery` (home feed)** | **57 ms** | **55 rps** | 339 ms | **~350% of 400%** |
| `mixed` (realistic blend) | 52 ms | **78 rps** | 186 ms | — |

**Today's honest capacity: ~78 origin rps ≈ 390 concurrent users** at 5 s think-time
(home-feed-only traffic: ~55 rps ≈ 275 users).

The saturation signature is textbook: from 4→32 VUs `discovery` throughput is flat at
~55 rps while p50 climbs 82 → 580 ms. Added concurrency buys pure queue delay.

### Where the CPU goes

At saturation the server pins **~3.5 of 4 cores**, i.e. **~63 ms of CPU per home-feed
request**. It is genuinely CPU-bound, not lock-bound. (An earlier reading suggested
otherwise; that sample window only covered the low-VU steps and was wrong.)

Field-by-field isolation of `discovery` — the decisive experiment:

| Selection | Latency |
|---|---|
| `id title` only | **61.3 ms** |
| all flat scalars | 59.0 ms |
| + `rating` (already batched) | 55.4 ms |
| + `scan` | 55.9 ms |
| + `detectedAt` (full fragment) | 71.2 ms |

**~86% of the cost is the base resolver, not the field selection.** Trimming fields
saves nothing. `detectedAt` adds a real but secondary ~10–14 ms (the per-row N+1 at
`graphql/mod.rs:359`). The expensive part is that `discovery` (`graphql/mod.rs:2490`)
re-runs `series_cache::count` + `library` + `recently_added` + `views::trending_keys` +
`series_by_keys` + three `map_series_list` passes **on every single request**.

And that entire result is **viewer-invariant except for one boolean** (`show_nsfw`).
It is recomputed from scratch for every visitor, and cached nowhere.

### Two more findings worth their own line

- **No compression.** Responses carry no `content-encoding`; there is no
  `CompressionLayer` in the middleware stack (`main.rs:1400-1421`). The home feed is
  **64,019 bytes uncompressed / 22,106 gzipped** — 65% of egress through the tunnel is
  avoidable.
- **Deployed schema is behind the working tree.** The running server rejects `sort`,
  `hasChapters`, `types`, `status`, `contentRating` on `search`, and has no `BrowseSort`
  enum. `packages/api/src/operations.ts` already ships them, and its own comment notes
  Browse has **no fallback path**. **Deploy the server before the reader** or Browse
  breaks for everyone.

## 3. The ceiling math

CPU budget ≈ 3,500 ms/s (the 3.5 cores actually reachable). Derived per-request cost:

| Request | CPU/req today | after caching |
|---|---|---|
| `discovery` / `updates` / `facets` | ~63 ms | **~0.5 ms** (framework floor) |
| `search` | ~15.7 ms | ~4 ms (tuned + hot-combo cache) |
| `/health` | ~0.5 ms | — |

Applying the `mixed` weights (discovery 5, search 3, updates 2, facets 1):

| Stage | Weighted CPU/req | Origin rps | Concurrent users @5 s |
|---|---|---|---|
| **Today** | ~45 ms | **78** | **~390** |
| After P1 (cache viewer-invariant reads) | ~4.6 ms | **~750** | **~3,750** |
| After P2 (search tuning) | ~1.5 ms | **~2,400** | **~12,000** |
| After P3 (edge caching) | most reads never reach origin | **10k+ effective** | **~50,000+** |

P3's number is not an origin-capacity claim — it is "the origin stops being the limit."

---

## 4. Roadmap

### P0 — Free wins, no code (do first)

| Task | Why | Effort |
|---|---|---|
| Kill dev tooling on the box | VS Code + 4 vite servers + a debug `komika_server` build are resident; swap is 3.5/4 GiB used | minutes |
| Move or cap `clubsite-worker` | It burns ~53% CPU — **~13% of the whole box** — against komika's 4 cores | small |
| Deploy the pending server change **before** the reader | Avoids the Browse-breaking schema drift in §2 | — |

### P1 — Cache the viewer-invariant reads ← **the single biggest win (~10×)**

The whole finding in one line: *`discovery` costs 63 ms of CPU, is identical for every
anonymous visitor, and is recomputed every time.*

1. **In-process TTL cache for `discovery` and `genreFacets`.** Key on `show_nsfw`
   (2 entries) — 30–60 s TTL. No new dependency needed; the codebase already uses this
   shape in `browse::COUNT_CACHE` (`browse.rs:232`). Add `moka` only if you want
   proper eviction.
   *Expected: 63 ms → ~0.5 ms on ~70% of traffic.*
2. **Single-flight the refill** so a cold key under load doesn't stampede — `KeyedLocks`
   already exists (`graphql/mod.rs:131`).
3. **Batch `detectedAt`** into `map_series_batch` alongside the already-batched
   `rating_summary` (`graphql/mod.rs:1044`). Kills ~80 point queries per home render.
4. **Memoize `viewer_show_nsfw`** per request (`graphql/mod.rs:783`) — it re-queries
   `users` on every call while `current_user` is correctly memoized.
5. **Add `CompressionLayer`** — 65% egress cut, ~free.

### P2 — Make the remaining hot path cheap

1. **Collapse the series/reader fan-out.** A series page is **6–12 sequential HTTP
   round trips** (`apps/reader/src/lib/data/source.ts:580-708`), including an unbounded
   per-translator `Chapters` N+1 at `source.ts:646`. `canonicalSeries`,
   `canonicalChapters` and `aggregatedChapters` each independently re-run
   `load_work_following_redirect`. **One `seriesPage(workId)` resolver → 1 round trip.**
2. **Tune `search`** against the materialized `browse_catalogue` (~115k rows): confirm
   index coverage for the common filter combos, and cache the top-N combos (the
   `COUNT(*)` is already cached 60 s, the rows are not).
3. **Coalesce the view-write path.** `views::record` is 3 writes + 1 read per chapter
   open (`views.rs:76`), serialized against all reads by SQLite's single writer. Buffer
   in memory, flush in batched transactions.
4. **Fix the rate limiter.** A process-global `std::sync::Mutex<HashMap>` taken **twice**
   per check with an O(n) scan, held inside async context (`graphql/mod.rs:41-107`).
   Shard it or move to an atomic/lock-free counter. Not a bottleneck at 78 rps —
   it will be at 2,000.

### P3 — Push reads off the origin entirely

1. **`Cache-Control` + `ETag` on `/graphql` public reads.** Today there is **none**, so
   no CDN or browser revalidation is possible for any query. This is the change that
   makes P3 possible at all.
2. **Edge-cache browse rows.** They're fetched client-side, so the SvelteKit edge cache
   never sees them (`browse/+page.ts:11-16`) — every browse scroll is an origin hit.
   Either move them into the SSR loader or serve them via a cacheable GET.
3. **Confirm Cloudflare is actually caching covers.** They already carry
   `max-age=31536000, immutable` (`main.rs:784`) — verify the edge hit-ratio rather than
   assuming it.

### P4 — Beyond one box

Past ~2,000 origin rps the 4-core wall is real and no code fixes it.

- **Scale up first** (more vCPU) — simplest, and SQLite keeps working.
- **Scale out** requires solving SQLite-per-node: litestream replicas are **DR, not read
  replicas**. Read-only replicas + a single writer is the natural next shape.
- **Get a second node for HA long before you need it for load.** One box, one process:
  a deploy, an OOM or a kernel hiccup drops 100% of users, and rebuilding the Rust
  server interrupts ingest.

---

## 5. How to verify each step

```bash
export PATH=$HOME/.local/node/bin:$PATH        # repo needs Node >=22.13
cd apps/server/bench

node loadtest.mjs --scenario discovery --vus 1,2,4,8,16,32 --duration 8   # the P1 target
node loadtest.mjs --scenario mixed     --vus 1,2,4,8,16,32 --duration 8   # the headline number
node loadtest.mjs --scenario mixed     --rate 200 --duration 30           # open-loop confirmation
```

The harness aborts a ramp when p95 crosses `--max-p95` (default 750 ms), which is what
makes it safe against this live box. Scenarios are read-only by design — **the
`RecordView` write path and the rate limiter are deliberately not exercised**, so a
clean `mixed` result does not prove the write path scales (see P2.3/P2.4).

**Re-run `discovery` after P1.** If it does not move from 55 rps toward the ~7,000 rps
framework floor, the cache is not being hit and the rest of the roadmap is built on sand.
