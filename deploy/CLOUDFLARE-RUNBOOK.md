# Komiq deploy runbook — Cloudflare + Oracle Ampere (arm64)

Komiq is a **hosted Mihon/Tachiyomi with a social layer** — the same source-driven
reader experience, but served from our infrastructure with accounts, ratings,
reviews, and per-chapter comments on top. The one architectural fact that shapes
this whole runbook: **chapter page images are NEVER stored.** They stream from
MangaDex@Home through the Cloudflare image Worker and are edge-cached there, so the
size of the catalogue does not drive our disk usage. What we actually store is
**metadata + covers + user/social data** — everything else is proxied on demand.

## Architecture

| Component | Runs on | How |
| --- | --- | --- |
| Reader (the site) | Cloudflare Worker `komika-reader` | adapter-cloudflare edge SSR; backend URLs baked in at build time |
| Image proxy | Cloudflare Worker `komika-img` | proxy + edge cache; hides Suwayomi |
| API server (`komika-server`) | Oracle Ampere VPS (Docker) | axum + async-graphql + SQLite + adaptive scanner |
| Suwayomi + FlareSolverr | VPS (Docker, internal/loopback only) | never exposed to the internet |
| DB backup | Cloudflare R2 | Litestream streams the SQLite WAL |

**Domain plan:**

- `komiq.cc` → reader Worker
- `img.komiq.cc` → image Worker
- `api.komiq.cc` → VPS server, reached via Cloudflare Tunnel
- Suwayomi has **no DNS** — it is never addressable from outside.

## Storage & R2 — what's backed up

- The server uses **TWO SQLite DBs**: the MAIN DB (`komika.sqlite3`) and a SEPARATE
  covers DB (`covers.sqlite3`).
- **Only the main DB is Litestream-replicated to R2.** It holds catalogue metadata +
  accounts + sessions + reviews + comments + avatars + comment-media (~4–6 GB at the
  full MangaDex catalogue → comfortably inside R2's free 10 GB tier).
- **Covers live in the separate `covers.sqlite3`, which is NOT replicated** (covers
  are re-derivable from MangaDex). They're still served from our origin at
  `/covers/{id}.webp`, immutable-cached at the edge. Default path: a `covers.sqlite3`
  sibling of `DATABASE_URL` (→ `/data/covers.sqlite3` on the server-data volume);
  override with `COVERS_DATABASE_URL`. On a DR restore the covers DB is empty and the
  server clears the stale cover version pointers on boot, so covers fall back to the
  proxy until re-cached — **no broken images**. ⚠️ It also has **no size cap** and
  shares a disk with the replicated main DB — see *Known risks* below.
- **R2 free tier:** 10 GB storage, 1M Class-A ops/mo, 10M Class-B ops/mo, and
  **EGRESS FREE** (so restores cost nothing). `deploy/litestream.yml` sets
  sync-interval / retention / snapshot-interval to keep ops + storage bounded.
- **Two levers control DB/R2 size:**
  1. `CATALOGUE_SYNC` (default **off**) — off = only your seeded/curated library
     (tiny DB); on = the full ~93.5k MangaDex metadata mirror (~4–6 GB).
  2. Cover caching (`COVER_CACHE`) — covers are served from origin and excluded from
     R2 regardless, so this only affects the `covers.sqlite3` file + boot-volume
     space, **never R2**.

## Prerequisites

- An **Oracle Ampere A1** VPS (Always Free; up to 4 OCPU / 24 GB is ideal).
- Ubuntu 22.04+ on the VPS.
- Your domain's **nameservers on Cloudflare**.
- `wrangler` installed locally:

  ```sh
  npm i -g wrangler && wrangler login
  ```

- Your Cloudflare **Account ID** (dashboard → right sidebar).

## Part A — Cloudflare R2 (backup), do first

1. Create a **private** R2 bucket named `komika-backup`.
2. Create an **R2 API token** with **Object Read & Write** on that bucket.
3. Note the S3 endpoint: `https://<ACCOUNT_ID>.r2.cloudflarestorage.com`.

You'll paste the token's key id + secret and this endpoint into `deploy/.env` in Part B.

## Part B — VPS: server + Suwayomi + FlareSolverr (arm64)

1. Install Docker and add your user to the `docker` group:

   ```sh
   curl -fsSL https://get.docker.com | sh
   sudo usermod -aG docker "$USER"    # re-login for this to take effect
   ```

2. Add a **4 GB swapfile** (the Rust build is memory-hungry on small shapes):

   ```sh
   sudo fallocate -l 4G /swapfile
   sudo chmod 600 /swapfile
   sudo mkswap /swapfile
   sudo swapon /swapfile
   echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
   ```

3. Clone and prepare env:

   ```sh
   git clone https://github.com/gecallidryas/komika.git
   cd komika/deploy
   cp .env.example .env
   ```

4. Edit `deploy/.env`:

   - `PUBLIC_HOST=api.komiq.cc`
   - `KOMIKA_IMG_WORKER_URL=https://img.komiq.cc`
   - a strong `KOMIKA_ADMIN_PASSWORD` (keep it quoted) and `KOMIKA_ADMIN_EMAIL`
   - `SEED_SERIES` — the comma-separated titles to seed the library with
   - `CORS_ORIGINS=https://komiq.cc`
   - `TRUSTED_PROXY_CIDRS=127.0.0.1/32,172.21.0.1/32` — the auth rate-limiter only
     honours `X-Forwarded-For` when the socket peer is in this list. **It is not
     just `127.0.0.1/32`.** The tunnel does connect to `localhost:8080`, but with
     Docker's userland proxy enabled that connection is *re-originated* by
     `docker-proxy` from the bridge **gateway**, so the server's peer is
     `172.21.0.1`. With only the loopback CIDR listed, `X-Forwarded-For` is
     discarded and **every** request keys to that one gateway address — a single
     shared rate-limit bucket, where 10 failed logins lock out login and
     registration for *all* users for 5 minutes. Re-derive the gateway with:

     ```sh
     docker network inspect komika_komika -f '{{json .IPAM.Config}}'
     ```

     Docker allocates the subnet dynamically, so re-check this after any
     `docker compose down` that removed the network — and see
     *Known risks → the bridge gateway is not pinned* below, which is the case
     where this drifts without anyone doing anything. `./deploy.sh up` now
     refuses to start if this value and the live gateway disagree.

     > **Only set this while `8080` is published on `127.0.0.1`** (it is, in
     > `docker-compose.yml`). On a `0.0.0.0` publish, any direct-port client
     > could forge `X-Forwarded-For` and walk past the limiter. The two settings
     > must always move together — `./deploy.sh up` asserts this pairing too.
   - Fill **all five** `LITESTREAM_*` vars:
     - `LITESTREAM_BUCKET=komika-backup`
     - `LITESTREAM_ENDPOINT=https://<ACCOUNT_ID>.r2.cloudflarestorage.com`
     - `LITESTREAM_REGION=auto`
     - `LITESTREAM_ACCESS_KEY_ID=<R2 access key id>`
     - `LITESTREAM_SECRET_ACCESS_KEY=<R2 secret>`

5. Bring it up:

   ```sh
   ./deploy.sh
   ```

   This builds **natively for arm64**, waits for health, then bootstraps
   MangaDex / admin / seed. Confirm the server log shows **continuous backup ENABLED**
   (`[entrypoint] Litestream backup ENABLED`).

6. The server Dockerfile is **arm64-safe**: it resolves the Litestream binary via
   `dpkg --print-architecture`, so no wrong-arch download on Ampere.

7. Lock the firewall down to SSH only:

   ```sh
   sudo ufw allow OpenSSH && sudo ufw enable
   ```

   Do **NOT** expose 8080 / 4567 — the Tunnel reaches them over localhost.

> **Note:** `flaresolverr` is the one image whose arm64 support has been flaky. It's
> only needed for Cloudflare-gated **non-MangaDex** sources, so if you're MangaDex-only
> you can drop the service entirely.

## Part C — Cloudflare Tunnel → api.komiq.cc

The Tunnel connects **outbound** from the VPS to Cloudflare, so it sidesteps Oracle's
ingress firewall completely — the only inbound port you ever need is SSH.

1. Install the **arm64** cloudflared (NOT amd64):

   ```sh
   curl -fsSLo cloudflared.deb https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64.deb
   sudo dpkg -i cloudflared.deb
   ```

2. Authenticate, create the tunnel, route DNS:

   ```sh
   cloudflared tunnel login
   cloudflared tunnel create komiq            # note the UUID it prints
   cloudflared tunnel route dns komiq api.komiq.cc
   ```

3. Write `~/.cloudflared/config.yml`:

   ```yaml
   tunnel: <UUID>
   credentials-file: /home/<user>/.cloudflared/<UUID>.json

   ingress:
     - hostname: api.komiq.cc
       service: http://localhost:8080
     - service: http_status:404
   ```

4. Install and start the service:

   ```sh
   sudo cloudflared service install
   sudo systemctl enable --now cloudflared
   ```

5. Verify:

   ```sh
   curl -fsS https://api.komiq.cc/health
   ```

## Part D — Image Worker → img.komiq.cc

1. In `apps/worker/wrangler.toml`, set the hotlink allowlist (it defaults to `""`,
   which **fails closed** in production — you must set it):

   ```toml
   ALLOWED_ORIGINS = "https://komiq.cc"
   ```

   (`ALLOWED_SOURCE_HOSTS` already ships the MangaDex hosts
   `uploads.mangadex.org,mangadex.network` — leave it unless you add sources.)

2. Deploy:

   ```sh
   cd apps/worker && wrangler deploy
   ```

3. In the Cloudflare dashboard, add a **Custom Domain** `img.komiq.cc` to the
   `komika-img` Worker.

## Part E — Reader Worker → komiq.cc

The reader's backend URLs are **baked at build time** via `$env/static/public` (Vite
inlines them into the bundle). The prod values are **committed** in
`apps/reader/.env.production` — you do **not** pass them inline. Just build and deploy:

```sh
cd apps/reader
pnpm build          # loads .env + .env.production → api.komiq.cc, backend on, img proxy
wrangler deploy
```

> **Why not a Worker `[vars]` block / `$env/dynamic/public`?** adapter-cloudflare
> evaluates `$lib/config.ts` (and the `backend`/`images` singletons it seeds) at edge
> cold-start, where there is **no request context** — a runtime env value is empty there,
> so `apiEndpoint` would freeze to `localhost` and the home/browse feeds render empty even
> though the API is up. `$env/static/public` is inlined at build, so it resolves correctly
> at module scope. To change an endpoint, edit `apps/reader/.env.production` and rebuild.

The public catalog pages (home, browse, series, updates, support) are **edge-SSR**: their
`load` awaits the backend on the server so real content is in the HTML (Svelte SSR only
renders the pending branch of `{#await}`, so streamed promises would ship skeletons forever).
Verify with a plain `curl https://komiq.cc/` — it must contain `/series/…` links, not just
`k-skeleton` placeholders.

Then add the **Custom Domain** `komiq.cc` (and `www.komiq.cc`) to the `komika-reader`
Worker in the dashboard.

## Part F — Admin console → admin.komiq.cc

The admin console (`apps/admin`) is a **static SPA** (`@sveltejs/adapter-static`,
`fallback: index.html`) — no `_worker.js`, unlike the reader. Its
`apps/admin/wrangler.toml` deploys the prebuilt `build/` as static assets with
`not_found_handling = "single-page-application"` so client-side deep links
(e.g. `/sources`) and refreshes serve `index.html` instead of 404-ing.

It reads exactly **one** build-time env var, `PUBLIC_KOMIKA_API` (Vite bakes it in),
so — like the reader — set it **before** `build`:

```sh
cd apps/admin
PUBLIC_KOMIKA_API=https://api.komiq.cc/graphql pnpm build   # emits build/
wrangler deploy
```

Then add the **Custom Domain** `admin.komiq.cc` to the `komika-admin` Worker in the
dashboard (Workers & Pages → komika-admin → Settings → Domains & Routes).

**CORS:** the admin origin must be allowed by the API server. Add it to
`CORS_ORIGINS` in `deploy/.env` (comma-separated, alongside the reader origins) and
restart the server, or every admin GraphQL call is blocked:

```sh
# deploy/.env
CORS_ORIGINS=https://komiq.cc,https://www.komiq.cc,https://admin.komiq.cc
cd deploy && docker compose up -d server
```

**Rebuild-on-change:** because `PUBLIC_KOMIKA_API` is baked at build time, if the API
domain ever changes the admin app must be **rebuilt and redeployed** (same as the
reader/image Workers).

**Auth & hardening:** the console is gated by the admin *account*
(`KOMIKA_ADMIN_USERS` / `KOMIKA_ADMIN_PASSWORD` on the server) — there is no separate
admin auth. For defense-in-depth on a management surface, optionally put
**Cloudflare Access** in front of the `admin.komiq.cc` hostname (Zero Trust → Access
→ Applications) so the login page isn't even reachable without passing Access first.
Don't expose the hostname more broadly than needed.

## DNS records

All records are **proxied** (orange cloud):

| Name | Target | Type |
| --- | --- | --- |
| `komiq.cc` | `komika-reader` Worker | Worker custom domain |
| `img.komiq.cc` | `komika-img` Worker | Worker custom domain |
| `admin.komiq.cc` | `komika-admin` Worker | Worker custom domain |
| `api.komiq.cc` | Tunnel | CNAME (`<UUID>.cfargotunnel.com`) |

**Nothing points at Suwayomi** — it has no DNS and no public port.

## Verify end-to-end

1. Curl the three surfaces:

   ```sh
   curl -fsS https://api.komiq.cc/health
   curl -fsSI https://img.komiq.cc/    # image Worker responds
   curl -fsS https://komiq.cc/ | head  # reader HTML (SSR)
   ```

2. Browser walk:
   - Home → the Latest Updates / Latest Added rows populate.
   - Open a series → detail page loads with cover + chapters.
   - Read a chapter → pages stream through `img.komiq.cc`.
   - Register a new account → post a comment on a chapter.
   - Log in as admin → the "manga DB" console loads.

## Monitoring — the two alerts to add first

**Current state: nothing is watching this box.** The server exposes a well-populated
Prometheus endpoint, but there is **no scraper, no alertmanager, no cron, and no
systemd timer** anywhere on the host. `task::supervise` does restart panicked
background loops, but it announces that only into a container log nobody reads.
This section is deliberately *not* a monitoring stack — it is the minimum two
checks that turn a silent failure into a notification.

**The endpoint:** `http://127.0.0.1:8080/metrics` on the VPS. It is intentionally
`404`'d at the tunnel edge (`/etc/cloudflared/config.yml` has an
`^/metrics$` → `http_status:404` ingress rule ahead of the catch-all), so it is
**not** reachable at `https://api.komiq.cc/metrics`. Scrape it from the host, or
from a container on the `komika` network at `http://server:8080/metrics`.

### Alert 1 — the scanner is failing

```sh
curl -fsS http://127.0.0.1:8080/metrics | grep '^komika_scan_failing '
```

Alert on **`komika_scan_failing > 0` sustained for 30 minutes.** A brief nonzero
value is normal (a source rate-limits, a single series 404s and gets retried);
what matters is the value staying up, which means the catalogue has quietly
stopped updating while every health check still reports green. Healthy baseline
at the time of writing is `0`.

Useful companions on the same scrape, worth graphing before they are worth
alerting on: `komika_scan_due` (backlog of series past their next-scan time — a
monotonic climb is the same failure seen earlier), `komika_scan_state_total`,
`komika_subscriptions_disabled` (extension subscriptions tripped by the circuit
breaker), and `komika_feed_updates_newest_age_seconds` (staleness of the newest
row in the updates feed).

### Alert 2 — Litestream replication has gone stale

**This is the one that actually costs you data.** Litestream fails *open*: if R2
credentials expire, the bucket is deleted, or the endpoint changes, replication
stops but the server keeps serving perfectly. Nothing surfaces it — you would
find out at restore time, which is the worst possible moment. Check the R2
generation's freshness directly:

```sh
docker exec komika-server-1 \
  litestream generations -config /etc/litestream.yml /data/komika.sqlite3
# name  generation        lag    start                 end
# s3    20f8d7e0ae99a99d  -4.1s  2026-...T20:04:34Z    2026-...T20:59:24Z
```

Alert when **`end` is more than 15 minutes behind now**, or when the command
exits non-zero / prints no `s3` row at all. `sync-interval` is `10s`
(`deploy/litestream.yml`), so healthy `lag` is single-digit seconds and a
15-minute threshold will not false-positive on a write burst. `litestream
snapshots …` on the same config shows the last base snapshot (one per day,
`snapshot-interval: 24h`) — a snapshot older than ~26h is the same alarm.

Route both to whatever already pages you; a 5-minute cron that curls/execs and
mails on failure is worth strictly more than the nothing that runs today.

## Known risks (accepted, not yet fixed)

### The covers DB has no size cap

`covers.sqlite3` is **20.5 GB** and growing without bound. It is deliberately
excluded from Litestream (see *Storage & R2* above — covers are re-derivable, and
restore-without-covers is handled in code), so this is **not** a backup problem.
It is a **disk** problem, and the damage lands somewhere other than where it
starts:

- The covers DB shares the `server-data` volume — and therefore the boot disk —
  with the **replicated** main DB (`komika.sqlite3`, currently 1.4 GB).
- **First symptom:** not "covers stop caching". It is `SQLITE_FULL` / "database
  or disk is full" **write errors on the main DB** — failed registrations,
  comments, ratings, and scan-state updates — because the covers cache ate the
  free space out from under it. Litestream then has nothing new to replicate,
  so the backup silently goes stale at the same moment.
- Watch `df -h /` on the VPS. Last measured (2026-07-26): **53% used, 91 GB free**
  of 193 GB — `covers.sqlite3` alone is 20.5 GB and Docker's images + build cache
  + volumes another ~47 GB (`docker system df`). There is real runway, but the
  growth is one-directional and `docker builder prune` reclaims ~3.3 GB today if
  you need slack in a hurry.

The fix is an **LRU cap on the covers cache**, already decided as **AD-21** in
`docs/plans/2026-07-23-architecture-review.md` and **not yet implemented**. It
needs application code, not deploy config. Until it lands, a disk-usage alert on
`/` (warn at 75%, page at 85%) is the cheap stand-in.

### The bridge gateway is not pinned

`TRUSTED_PROXY_CIDRS` hard-codes `172.21.0.1/32` — the `komika_komika` bridge
gateway — because with Docker's userland proxy enabled that is the socket peer the
server actually sees for tunnel traffic. **Nothing pins that address.** The daemon
has no `default-address-pools` configured (`docker info -f '{{json
.DefaultAddressPools}}'` → `null`), so it allocates the first free `/16` out of
`172.17.0.0/16 … 172.31.0.0/16`. Today's occupancy:

| network | subnet |
| --- | --- |
| `bridge` (docker0) | 172.17.0.0/16 |
| `clubsite_default` | 172.18.0.0/16 |
| `torchy_edge` | 172.19.0.0/16 |
| `torchy_default` | 172.20.0.0/16 |
| `komika_komika` | **172.21.0.0/16** |

A `./deploy.sh up -d` does **not** touch the network (it is only created when
absent), so a routine deploy is safe. The address moves when the network is
*deleted and recreated* — `./deploy.sh down` / `destroy`, `docker network prune`,
or **a host reboot**, which is the dangerous one: on reboot the stacks race and
whichever creates its network first takes 172.18, so komika can land anywhere.
`TRUSTED_PROXY_CIDRS` then points at a gateway that no longer exists, `X-Forwarded-For`
is discarded, and every request keys to one address — a single shared rate-limit
bucket where 10 failed logins lock out login and registration for everyone. Nothing
logs it.

**Why it isn't simply pinned in `docker-compose.yml`.** Adding an `ipam:` block
under `networks.komika` changes the network's Compose config hash. Compose stamps
that hash on the network as a label (verify: `docker network inspect komika_komika
-f '{{json .Labels}}'` shows `com.docker.compose.config-hash`) and the only thing
it does on a mismatch is abort — `network %s was found but has incorrect label %s
set to %q (expected: %q)`. There is no recreate path. So pinning the subnet makes
**every** subsequent `up` fail until the network is removed, i.e. it costs a full
`compose down` + stack outage. That is a deliberate maintenance window, not
something to slip into an unrelated deploy.

**What guards it today:** `./deploy.sh up` reads the live gateway and refuses to
start if `TRUSTED_PROXY_CIDRS` doesn't cover it, printing the correct value. That
converts the silent failure into a loud one on the deploy path. It does **not**
cover a bare `docker compose up -d` or an unattended reboot — after any reboot,
re-check by hand:

```sh
docker network inspect komika_komika -f '{{range .IPAM.Config}}{{.Gateway}}{{end}}'
grep TRUSTED_PROXY_CIDRS deploy/.env
```

**To pin it properly** (next planned outage): `cd deploy && ./deploy.sh down`, add

```yaml
networks:
  komika:
    driver: bridge
    ipam:
      config:
        - subnet: 172.21.0.0/16
          gateway: 172.21.0.1
```

then `./deploy.sh up`. The `down` removes the network, so the changed config hash
has nothing to conflict with.

## Rollback

Every change in this deploy is config-only and reverts with `git checkout` + one
`up`; the migrations do not. Read this before starting.

**What is not reversible.** `./deploy.sh up` applies migrations `0056`–`0059`
(~35 s: `0058` rebuilds indexes and re-`ANALYZE`s ~20 tables, `0059` is a
112k-row `UPDATE` on `work`). `sqlx` has no down-migrations. Rolling the *config*
back does not roll the schema back, and an older server binary against a newer
schema is untested. If you need the schema back, that is a Litestream restore
(*Disaster recovery* above), not a rollback.

**Expect a 2–3 minute API outage on the deploy itself**, not a rolling restart.
`mem_limit`, `logging:` and the `@sha256:` image pins all change each service's
config hash, so Compose recreates all three containers. The server is gated on
`depends_on: suwayomi: service_healthy`, so the clock is: Suwayomi JVM cold boot
(healthcheck `start_period` 40 s, `interval` 20 s) **then** ~35 s of migrations
before the listener binds. `api.komiq.cc` returns 502 for that whole window.

**To roll the config back:**

```sh
cd deploy
git checkout -- docker-compose.yml deploy.sh README.md CLOUDFLARE-RUNBOOK.md .env.example
cp .env.bak-preaudit .env          # restores TRUSTED_PROXY_CIDRS=127.0.0.1/32
./deploy.sh up
```

`.env.bak-preaudit` is the pre-change file (mode 600, gitignored by the `.env.*`
rule). It differs from the live `.env` in exactly one value —
`TRUSTED_PROXY_CIDRS` — plus comments.

**The one coupling that matters.** Reverting `docker-compose.yml` restores the
`0.0.0.0` + `[::]` publish of `8080`. `TRUSTED_PROXY_CIDRS` **must** be blanked or
reverted at the same time: on a world-bound port, any direct client can set
`X-Forwarded-For` and pick its own rate-limit bucket, skipping the auth limiter
entirely. Using `.env.bak-preaudit` handles this, because the pre-change value
(`127.0.0.1/32`) never matches the real peer and so trusts nothing. Do **not**
revert only the compose file. `./deploy.sh up` now blocks this combination, but
`docker compose up -d` does not.

**Partial rollbacks** (each independent, all safe to revert alone):

| revert | how | consequence |
| --- | --- | --- |
| healthcheck → `/health` | edit `docker-compose.yml` | loses pool-liveness detection; a wedged SQLite pool reads healthy again |
| `mem_limit` | delete the lines | back to unbounded; a runaway can make the kernel pick an OOM victim from clubsite/torchy |
| `logging:` | delete the blocks | unbounded container logs (the unbounded `clubsite-worker` log on this host is already 310 MB) |
| digest pins | restore `:stable` / `:latest` | next `compose pull` can swap in an unreviewed image, including the headless Chromium |
| `profiles: [selfhost]` | delete the line | the `reader` container starts again on `up` and publishes `0.0.0.0:8081` |
| `/etc/docker/daemon.json` | `rm` it | no effect until the next daemon restart either way |

## Mobile (.ipa) at production

1. Rebuild the Tauri iOS app with the prod backend baked in:

   ```sh
   cd apps/reader
   PUBLIC_KOMIKA_API=https://api.komiq.cc/graphql \
   PUBLIC_KOMIKA_IMG_MODE=proxy PUBLIC_KOMIKA_IMG_WORKER=https://img.komiq.cc \
   pnpm tauri ios build
   ```

2. Add the prod origins to the CSP `connect-src` in
   `apps/reader/src-tauri/tauri.conf.json` — it currently only allows
   `http://localhost:8080`. Add:

   ```
   https://api.komiq.cc https://img.komiq.cc
   ```

   (Without this, the packaged app's requests to prod are blocked by CSP.)

## Gotchas

- **Oracle ingress firewall** is avoided entirely via the Tunnel — only SSH inbound is
  ever needed. Don't fight Oracle's security lists for 8080/4567; keep them closed.
- **Boot volume:** recommend **100 GB** (Oracle Always Free gives 200 GB block
  storage). Comics aren't stored, so this space is for **metadata + covers + Docker
  layers**, not page images.
- **`CATALOGUE_SYNC` is single-replica.** Its MangaDex rate limiter is in-process, so
  it bounds one server process, not the fleet. Run `CATALOGUE_SYNC=on` on **exactly
  one** server or you'll multiply the request rate against MangaDex's per-IP ceiling
  and risk a 429/403 ban.
- **Reclaim build-cache space** after builds with `docker builder prune`.
