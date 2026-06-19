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
  proxy until re-cached — **no broken images**.
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
   - Because the Cloudflare Tunnel connects from localhost, set:
     - `TRUSTED_PROXY_CIDRS=127.0.0.1/32`
     - `CORS_ORIGINS=https://komiq.cc`
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

The reader's backend URLs are **baked at build time** (Vite inlines `PUBLIC_*`), so
build with the env inline, then deploy:

```sh
cd apps/reader
PUBLIC_KOMIKA_BACKEND=on PUBLIC_KOMIKA_BACKEND_KIND=komika \
PUBLIC_KOMIKA_API=https://api.komiq.cc/graphql \
PUBLIC_KOMIKA_IMG_MODE=proxy PUBLIC_KOMIKA_IMG_WORKER=https://img.komiq.cc \
pnpm build
wrangler deploy
```

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
