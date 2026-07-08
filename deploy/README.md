# Komika deploy

Stand up the **full public stack** — a hosted Mihon/Tachiyomi with a public social
layer (ratings, reviews, per-chapter comments), an auto-served catalog, and an
**adaptive scanner that keeps every library series updated** so users never have to
manually refresh.

## One command

```sh
cd deploy
cp .env.example .env      # edit: PUBLIC_HOST, KOMIKA_ADMIN_PASSWORD, SEED_SERIES…
./deploy.sh
```

`deploy.sh` builds the images, starts everything, waits for health, then **bootstraps**
the stack: registers the Keiyoushi extension repo, installs the **MangaDex** source,
turns on the **FlareSolverr Cloudflare bypass**, creates the admin account, and seeds
the library. When it finishes it prints the URLs.

```
Reader      http://<PUBLIC_HOST>:8081
API         http://<PUBLIC_HOST>:8080/graphql   (GraphiQL on GET)
Suwayomi    http://<PUBLIC_HOST>:4567            (source management / image serving)
admin login: <KOMIKA_ADMIN_USERS> / KOMIKA_ADMIN_PASSWORD
```

Other commands: `./deploy.sh logs`, `./deploy.sh bootstrap` (re-run bootstrap only),
`./deploy.sh down` (stop, keep data), `./deploy.sh destroy` (stop + delete volumes).

## The four services

| Service          | What it does                                                                                                                                                                                     |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **suwayomi**     | Runs the source extensions (MangaDex, …): catalog, chapters, **and page/cover images**.                                                                                                          |
| **flaresolverr** | Headless-browser sidecar that solves Cloudflare "checking your browser" challenges. Internal-only; Suwayomi calls it at `http://flaresolverr:8191`.                                              |
| **server**       | `komika-server` — federates Suwayomi + adds accounts/social/admin, and runs the **adaptive scan scheduler** (auto-updates the library on a cadence derived from each series' chapter frequency). |
| **reader**       | Static SvelteKit SPA (nginx), pointed at the server.                                                                                                                                             |

## How it's wired (important gotcha)

- **GraphQL federation is internal:** the server reaches Suwayomi at
  `SUWAYOMI_URL=http://suwayomi:4567` over the private compose network.
- **Image URLs are public:** page/cover images are `<img src>`s the _browser_ loads, so
  they can't use the internal `suwayomi:4567` hostname. The server rewrites them to
  **`SUWAYOMI_PUBLIC_URL`** (`http://<PUBLIC_HOST>:4567` by default). That's why Suwayomi's
  port is published — browsers fetch images straight from it (direct mode). Set
  `PUBLIC_HOST` to your domain/IP in `.env` for a real deploy.
- The reader's backend config is baked in **at build time** (Vite inlines `PUBLIC_*`), so
  the compose `reader` build args carry `PUBLIC_KOMIKA_API` / `PUBLIC_KOMIKA_IMG_MODE`.
  Rebuilding is automatic via `deploy.sh`.
- **Persistence:** the SQLite DB (accounts, reviews, comments, admin overrides) lives on
  the `server-data` volume at `/data`; Suwayomi's library + downloaded images on
  `suwayomi-data`. Both survive `down`; `destroy` deletes them.

## Auto-updating catalog

`SEED_SERIES` in `.env` (comma-separated titles) is added to the library on bootstrap.
The scanner (`SCAN_TICK_SECONDS`, default 300) then re-checks each library series on a
cadence derived from its average time-between-chapters, refetches overdue ones from the
source, and records new chapters — so the Updates/Library screens stay current with no
user action. Admins can add/adjust more series and per-series scan policy in the
**admin "manga DB" console**.

## Backups & restore (Litestream)

The server DB (`/data/komika.sqlite3` — accounts, sessions, reviews, comments,
admin overrides, scan-state) is backed up with **Litestream**: it streams the SQLite
WAL to an S3-compatible bucket continuously (point-in-time restore, ~seconds RPO) and
**restores the latest snapshot automatically** when the server starts on a fresh volume.

- **Off by default.** Leave the `LITESTREAM_*` vars blank in `.env` and the server runs
  without backup (fine for local/testing). Set them to enable it — no code change.
- **Enable it:** create a bucket (Backblaze B2 recommended — reuse the image-cache
  account) + an app key, then fill `LITESTREAM_BUCKET` / `LITESTREAM_ENDPOINT` /
  `LITESTREAM_REGION` / `LITESTREAM_ACCESS_KEY_ID` / `LITESTREAM_SECRET_ACCESS_KEY` in
  `deploy/.env` and `./deploy.sh` (or `docker compose up -d --build server`). WAL mode
  is already on in the DB, and the server runs under `litestream replicate -exec` inside
  its container.
- **Disaster recovery** is automatic: on a brand-new host/volume with the vars set, the
  server pulls the newest snapshot before it starts. To restore manually to a file:
  ```sh
  docker compose exec server litestream restore -o /tmp/komika.sqlite3 /data/komika.sqlite3
  ```
- Litestream config: `deploy/litestream.yml`; the container entrypoint is
  `deploy/server-entrypoint.sh` (runs the server plain when Litestream is unconfigured).

## Notes for a real (internet-facing) deploy

- **TLS + reverse proxy.** Put Caddy/Traefik/nginx (or Cloudflare) in front and terminate
  TLS. Ideally serve everything under one domain and route: `/` → reader,
  `/graphql`+`/health` → server, and the image routes → Suwayomi. Then set `PUBLIC_HOST`
  to that domain and `SUWAYOMI_PUBLIC_URL` to the image path.
- **Lock down Suwayomi.** Its API/UI on `:4567` is unauthenticated. For direct-mode images
  you must expose the image routes (`/api/v1/manga/*/thumbnail`, `.../chapter/*/page/*`),
  but you should **not** expose the rest of the Suwayomi admin API publicly — restrict it
  at the reverse proxy (allow only the image paths), or move to the Cloudflare Worker image
  path (`apps/worker`, `PUBLIC_KOMIKA_IMG_MODE=proxy`) which caches and hides Suwayomi.
- **FlareSolverr cost.** It runs a real headless Chromium — hundreds of MB RAM, spiky CPU,
  and it breaks when Cloudflare updates. It's only needed for CF-protected sources
  (MangaDex is an open API and doesn't use it). Drop the service if you only run API-based
  sources.
- **Data durability.** Continuous backup is built in — see **Backups & restore** above;
  just set the `LITESTREAM_*` vars. (The `server-data` volume holds argon2 password hashes
  - session tokens + all social data, so keep the bucket private.)
- **Secrets.** Nothing sensitive in the image or compose file; `deploy/.env` is
  git-ignored. Use your platform's secret store for `KOMIKA_ADMIN_PASSWORD` and any keys.

## Build images individually

```sh
# from repo root
docker build -f deploy/server.Dockerfile -t komika-server .
docker build -f deploy/reader.Dockerfile -t komika-reader \
  --build-arg PUBLIC_KOMIKA_API=https://komika.example.com/graphql .
```
