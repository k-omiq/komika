# Performance investigation — 2026-07-27

**Question asked:** how many concurrent users can the stack handle, and what is the
ceiling if we optimise as hard as is reasonable without sacrificing quality?

**Answer:** ~390 concurrent users today; ~12,000 is reachable in software on this box;
past that the 4-core wall is real. The bottleneck is **one uncached resolver**, not the
hardware in general.

Artifacts produced:

| File | Contents |
|---|---|
| `apps/server/bench/loadtest.mjs` | The load generator |
| `apps/server/bench/README.md` | How to run it, how to read it, what it does *not* prove |
| `apps/server/bench/results/2026-07-27-baseline.md` | Verbatim measurements |
| `PERFORMANCE_ROADMAP.md` | Ceiling math + phased plan |
| this file | Method, dead ends, and how conclusions were reached |

---

## 1. Topology, as measured (not as assumed)

The first pass at this question was answered from architecture alone and got the shape
right but several specifics wrong. What the box actually looks like:

- **4 vCPU / 23 GiB Oracle VPS**, shared with **two unrelated production stacks**
  (`clubsite-*`: caddy/postgres/redis/celery; `torchy-*`: api/postgres/valkey).
  `clubsite-worker` alone was burning **53% CPU** — ~13% of the whole machine — while
  komika idled at 0.05%.
- **Ingress is a `cloudflared` tunnel**, not a local reverse proxy:
  `api.komiq.cc` → `http://localhost:8080`, with `/metrics` explicitly 404'd at the edge.
- **The reader is a Cloudflare Worker**, not on this box. The `reader` compose service is
  `profiles: [selfhost]` and is not running.
- **Suwayomi is capped at 3.0 of 4 CPUs** and 8 GiB, with `MaxRAMPercentage=70`.
  It was **not** the bottleneck under test — it sat at 0.16% CPU.
- **`komika-server` has a 6 GiB memory cap and no CPU cap**, so it can use all 4 cores.
- SQLite, WAL, `synchronous=NORMAL`, 1.5 GiB mmap; main pool **8** connections, cover
  pool **4** (`apps/server/src/db.rs:57`, `:91`).
- `#[tokio::main]` with **no `worker_threads` override** → 4 worker threads.

### Corrections to the initial architecture-only answer

| Initial claim | Reality |
|---|---|
| "Cover stampede → Suwayomi breaks first, capping you at tens of users" | Covers already carry `public, max-age=31536000, immutable` (`main.rs:784`) and are edge-cacheable. Suwayomi was idle. **Not the bottleneck.** |
| "SQLite single-writer will bite under read load" | Not on the read path measured. It remains a real risk for `RecordView` — untested here. |
| "~300–1,000 active users" | Measured **~390**. The estimate was roughly right, for partly the wrong reasons. |

---

## 2. Method

1. **Map the hot path first.** A subagent traced what the reader actually sends and
   which resolvers serve it, so the load test would exercise real operations rather than
   a synthetic endpoint. Key output: traffic ranks `Discovery` > `Search` > series-page
   fan-out > reader fan-out.
2. **Build a harness** (`loadtest.mjs`) speaking those exact operations, including the
   full `SeriesFields` fragment.
3. **Validate every query against the live server** before ramping. This immediately
   caught the schema drift in §4.
4. **Establish the framework floor** (`/health`, no DB) so every later number has a
   ceiling to be compared against.
5. **Ramp each scenario to saturation**, then **isolate cost within the slowest one** by
   varying only the selection set.

### Load-shape choice

Closed-loop ramps (`--vus`) find the saturation knee. Open-loop (`--rate`) is available
and is the honest way to confirm a capacity figure, because a closed loop hides stalls
(coordinated omission). **The baseline was taken closed-loop only** — an open-loop
confirmation run at ~60 rps is still outstanding.

### Safety

Read-only scenarios, a p95 abort at 750 ms, and a 5% error-rate abort. The box was
serving live traffic throughout; the guard exists so a capacity probe can't become an
outage.

---

## 3. The wrong turn (recorded on purpose)

An early CPU sample taken *during a ramp* showed `komika-server` at **117%** of 400% at
peak throughput. The conclusion drawn — and stated — was that the server was **not
CPU-bound** but serialized on a lock, most likely the rate-limiter mutex or the SQLite
pool. That was a satisfying story: it implied a cheap, surgical fix.

It was wrong. The sampler ran for ~20 s against a ~60 s ramp, so its samples covered only
the **1-, 2- and 4-VU steps** — the low-load part of the run. The rising tail
(199% → 208% → 301%) was the ramp climbing, not saturation.

Re-measured properly — 16 VUs held for 25 s, sampling throughout — the server sits at
**~350% of 400%**. It is genuinely CPU-bound.

**Lesson:** when sampling a resource during a stepped ramp, either pin the load at one
step or align the sampling window to the step boundaries. A sampler that silently covers
the wrong window produces a confident, plausible, wrong answer.

A second, smaller version of the same error: one run appeared to show near-zero CPU
because the harness had failed to start at all (`cd` reset the working directory, and the
Node module-resolution error scrolled past above the output being inspected). Always
confirm the load actually ran before interpreting what the server did.

---

## 4. Findings

### 4.1 `discovery` is the bottleneck — and it is not the N+1

`discovery` saturates at **55 rps**, consuming ~3.5 cores, i.e. **~63 ms of CPU per
request**.

The obvious suspect was the per-row `detectedAt` resolver (`graphql/mod.rs:359`), an
un-memoized point query per series — ~80 extra queries per home render. Isolating it by
selection set showed otherwise:

| Selection | Latency |
|---|---|
| `id title` only | **61.3 ms** |
| full fragment | 71.2 ms |

**Selecting nothing but `id` and `title` still costs 61 of the 71 ms.** ~86% of the cost
is the base resolver, which per request re-runs `series_cache::count` +
`library` + `recently_added` + `views::trending_keys` + `series_by_keys` plus three
`map_series_list` passes (`graphql/mod.rs:2490`).

And that whole result is **viewer-invariant except for the `show_nsfw` boolean** — it is
recomputed from scratch for every anonymous visitor and cached nowhere. That is the ~10×
win, and it is a cache, not a rewrite.

`detectedAt` is still worth batching (~10–14 ms, and it is in the fragment every screen
selects) — but as P1.3, not P1.1.

### 4.2 Nothing caches GraphQL responses

There is **no `moka`, no `lru`, no `dashmap`** in the server, and no in-memory cache of
any resolver result. The only in-process caches are:

- `browse::COUNT_CACHE` — 60 s TTL, caches the `COUNT(*)` **but not the rows**
  (`browse.rs:232`).
- `series_cache::CHAPTER_FETCH_MEMO` — stores fetch *timestamps*, not data.
- `RequestUserCache` / `RequestLibraryCache` — per-request, die with the request.

`series_cache.rs` is a SQLite mirror of upstream Suwayomi (6 h / 90 m TTLs), **not** a
response cache.

At the HTTP layer: **no `Cache-Control` on `/graphql` at all, no ETag, no
Last-Modified**. So no CDN or browser revalidation is possible for any query. The only
caching in front of GraphQL is SvelteKit edge `s-maxage` on three loaders (home 60 s,
series 60 s, browse facets 300 s) — and browse *rows* are fetched client-side, so the
edge never sees them.

### 4.3 No compression

Home feed: **64,019 bytes uncompressed, 22,106 gzipped**. No `content-encoding` on
responses; no `CompressionLayer` in the stack (`main.rs:1400-1421`). ~65% of tunnel
egress is avoidable for near-zero effort.

### 4.4 The series page is 6–12 round trips

`resolveWork()` (`apps/reader/src/lib/data/source.ts:580-708`) issues three dependent
waves, including an **unbounded per-translator `Chapters` N+1 over HTTP**
(`source.ts:646`). Server-side, `canonicalSeries`, `canonicalChapters` and
`aggregatedChapters` each independently re-run `load_work_following_redirect` and
overlapping chapter loads. The reader route then runs the *same* fan-out again
(`source.ts:1894`) with no edge caching at all.

Not load-tested (the harness covers home/browse), but it is structurally the most
expensive page in the product.

### 4.5 Contention points that will bite later

Neither is a bottleneck at 78 rps; both will be at 2,000:

- **Rate limiter** — a process-global `std::sync::Mutex<HashMap>` taken **twice** per
  check with an O(n) scan, held inside async context (`graphql/mod.rs:41-107`).
- **`views::record`** — 3 writes + 1 read per chapter open (`views.rs:76`), serialized
  against all reads by SQLite's single writer.

### 4.6 Schema drift — deploy ordering hazard

The deployed server rejects `sort`, `hasChapters`, `types`, `status` and `contentRating`
on `search`, and has no `BrowseSort` enum. `packages/api/src/operations.ts` already ships
them, and its own comment states Browse has **no fallback path**.

**Deploying the reader before the server breaks Browse for every user.** This is
unrelated to performance but was surfaced by validating the load-test queries — a useful
side effect of testing against the real schema.

---

## 5. Ceiling math

CPU budget ≈ 3,500 ms/s (the 3.5 cores actually reachable). Per-request cost derived from
`CPU% ÷ rps`:

| Request | today | after caching |
|---|---|---|
| `discovery` / `updates` / `facets` | ~63 ms | ~0.5 ms (framework floor) |
| `search` | ~15.7 ms | ~4 ms |
| `/health` | ~0.5 ms | — |

Applying `mixed` weights (discovery 5, search 3, updates 2, facets 1):

| Stage | Weighted CPU/req | Origin rps | Users @5 s |
|---|---|---|---|
| Today | ~45 ms | **78** | ~390 |
| After P1 (cache viewer-invariant reads) | ~4.6 ms | ~750 | ~3,750 |
| After P2 (search tuning, fan-out collapse) | ~1.5 ms | ~2,400 | ~12,000 |
| After P3 (edge caching) | — | origin stops being the limit | ~50,000+ |

**These are projections, not measurements.** They assume cached reads fall to roughly the
framework floor. The acceptance test is simple: after P1, re-run `discovery`. If it does
not move off 55 rps toward 7,360, the cache is not being hit and the rest of the plan
rests on a bad assumption.

Caveats that bound the top end regardless: the `cloudflared` tunnel is a single process
of unmeasured throughput; the box is shared with two other prod stacks; and think-time is
assumed at 5 s rather than derived from logs.

---

## 6. What was not done

- **No open-loop confirmation run.** Capacity figures are closed-loop only.
- **No authenticated load.** The per-viewer library N+1 never fired; real signed-in
  traffic is heavier than these numbers.
- **No write-path load.** `RecordView` and the rate limiter are untested.
- **No test through the tunnel.** All numbers are origin-local.
- **No series/reader-page scenario**, despite §4.4 identifying it as structurally worst.
- **No code changes.** Nothing in this investigation was committed or deployed; the only
  files added are the harness and these documents.
