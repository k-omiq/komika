# Komika — Spec & Architecture

> "Komika" is a placeholder name. This is the living source of truth for the
> project. It is updated as decisions are made.

## What Komika is

A hosted manga/comics reader — **"Mihon as a service, plus a social layer."**
It indexes **all kinds of comics** (manga, manhwa, manhua, webtoons, Western
comics), auto-serves them from a curated backend, and adds user **reviews,
ratings, and per-chapter comments** on top of a full Mihon-parity reader.

## Clients (one design language across all)

| Client             | Build target                      | Image source                 |
| ------------------ | --------------------------------- | ---------------------------- |
| Web / PWA          | Svelte static SPA                 | Cloudflare Worker proxy      |
| Desktop            | Tauri v2 (wraps the same SPA)     | Direct fetch via Rust core   |
| iOS / Android      | Tauri v2 mobile (same SPA + Rust) | Direct fetch via Rust core   |
| Admin ("manga DB") | Svelte static SPA (web-only)      | n/a                          |

> **Direction (updated): native-first.** The product targets **Desktop + iOS/Android
> (Tauri)**; the public **web app is deprioritized** (kept as an optional build). Two
> consequences: (1) native apps fetch image bytes **directly from the source via the Rust
> `fetch_image` core**, so the Cloudflare Worker **image proxy is off the critical path**
> (there is no B2 image tier — B2/R2's only job now is Litestream backup); (2) **SEO / edge-SSR is moot** — apps are
> store/direct-distributed, not crawled — which removes the biggest earlier "gate". Mobile
> still needs `tauri ios/android init` + Android SDK/NDK. The hosted backend
> (server + Suwayomi + FlareSolverr + scanner + social) stays central; clients are thin.
> Monetization is **donation-only** (no paywalls/ads).

## Stack

- **UI:** Svelte 5 + SvelteKit (`adapter-static`, SPA — **no SSR**, so the web
  build has zero server attack surface). Vite.
- **Native shell:** Tauri v2 (Rust core) for desktop + iOS/Android from the same
  UI. Tiny binaries, system webview, hardened sandbox.
- **Monorepo:** pnpm workspaces.
- **Hard constraints:** low resource, fast, secure against attack,
  **strictly no Next.js** (no long-running Node server / SSR runtime).

## Repo layout

```
komika/
  apps/
    reader/          SvelteKit SPA — the user app
      src-tauri/     Rust core: fetch_image (native direct fetch), hardened CSP
    admin/           SvelteKit SPA — the "manga DB" admin console (web-only)
  packages/
    types/           shared domain types (Series, Chapter, Review, ScanPolicy, …)
    api/             data layer — Backend (GraphQL) + ImageProvider (web/native)
    ui/              design tokens + shared components (tokens are PLACEHOLDERS)
```

## Backend

A **Suwayomi / Tachidesk-style server** runs the Tachiyomi extensions/scrapers.
Key difference from Mihon: **extension management is operator-side only** — users
never install sources or pick extensions. The catalog is **auto-served**.

The unified backend owns: identity, the auto-served catalog, chapter/page
metadata, library + reading-progress sync, series marking, popularity counting,
the social layer, the admin "manga DB", and cache orchestration. It exposes
**GraphQL** (`packages/api/src/graphql-backend.ts` — fully implemented against
`operations.ts`; the UI is built against the stable method signatures).

> **Direction (updated 2026-07-11): catalogue pivot — see [CATALOGUE.md](CATALOGUE.md).**
> The backend moves from pure live-federation to a **hybrid**: MangaDex metadata
> and chapter lists are **mirrored** into the DB (Tier 1, full catalogue + update
> polling via the direct MangaDex API), all other sources come through the
> **curated** Keiyoushi extension repo (Tier 2, admin hand-picks series), and a
> **canonical `work`** model with a multi-step dedup matcher guarantees **one entry
> per series**. Page images stay federated live. This supersedes the "catalog is
> not stored" premise in `apps/server/migrations/0001_init.sql` for metadata.
> NSFW content is gated behind a `show_nsfw` setting (default off).

## Image pipeline (the web/native split)

> **Updated: Workers-only — no object-storage image cache.** The earlier B2 image
> tier was removed (git `6d06784`); images are **never stored**. B2/R2's only job now
> is Litestream backup of the SQLite DB, not image caching.

- **All series** → **not stored**; a **Cloudflare Worker** proxies images on demand
  (pure edge cache, `caches.default`). See `apps/worker/src/index.ts`.
- **Web** build → all images resolved to Worker URLs (browsers can't
  cross-origin-scrape — CORS — so the proxy is mandatory).
- **Native** builds → fetch image bytes **directly** from the source CDN via the
  Rust `fetch_image` command; never touch the Worker.

The seam is `ImageProvider` in `packages/api`. The UI calls `resolvePage()` /
`resolveCover()` and is oblivious to which platform it's on. Chosen at runtime by
`isTauri()` (`__TAURI_INTERNALS__`).

> **Note (CSP / native backend calls):** the reader's Tauri CSP currently allows
> dev backend hosts (`localhost:4567`, `localhost:8787`) in `connect-src`. For
> production, either pin the real API host in the CSP or (preferred) route native
> GraphQL calls through the Rust core so the webview only ever talks IPC.

## Adaptive update scanner (backend behavior)

Each series has a `ScanPolicy`: poll cadence derives from its **average
time-between-chapters** (`avgIntervalHours`), **admin-overridable**
(`overrideIntervalHours`) in the admin console. Once a series is "overdue" it is
re-polled every `pollEveryMinutes` (e.g. 30) until the new chapter appears. The
scanner **pauses** automatically for `COMPLETED` / `HIATUS` / `CANCELLED`. This
drives new-chapter notifications (there is no image cache-fill job — images are
edge-cached on demand only). This adaptive per-series scanning applies to Suwayomi-library
series only, by design; MangaDex-mirrored canonical works are refreshed on a global interval
by the catalogue sync and surface their updates via `canonicalUpdates` (see CATALOGUE.md §5–6).

## Social layer

- **Per-series reviews** with a 1–10 score (aggregated into `RatingSummary`).
- **Per-chapter comment threads.**
- **Spoiler tags** on both.
- **No moderation in v1.**
- Requires real user accounts.

## Reader (full Mihon parity)

Paged LTR / RTL / vertical, continuous webtoon, zoom/pan, tap zones, keyboard
nav, prefetch, and offline downloads on native.

> **Key engineering risk to design deliberately:** continuous/webtoon reading of
> large images in a WebView must virtualize pages and decode on demand (release
> offscreen pages) to keep memory bounded. This is the one place the WebView
> approach needs care; budget for it in the reader work.

## Designs (imported)

The designs were delivered as a zip (`Manga reading website.zip`, branded
**"YOMU"**) and implemented. Source comps live in the scratchpad; the real design
tokens are extracted into `packages/ui/tokens.css`:

- **Type:** Bricolage Grotesque (display) + Manrope (body), self-hosted via
  `@fontsource-variable/*` (CSP-safe, offline).
- **Palette:** dark-first (`#0c0c0d` base), coral/purple/teal format accents,
  gold rating stars, green/blue/amber status colors.

Implemented screens (reader app), faithful to the comps:

| Route            | Screen  | Notes                                                          |
| ---------------- | ------- | -------------------------------------------------------------- |
| `/`              | Home    | auto-rotating hero, format cards, Latest/Trending/Added rows   |
| `/browse`        | Browse  | live search + filter rail (format/genre/status/rating) + sort  |
| `/updates`       | Updates | trending rows, New/Hot tabs, format filter, grid               |
| `/library`       | Library | continue-reading, shelf tabs w/ counts, progress bars          |
| `/series/[slug]` | Series  | hero, rate (1–10 stars), chapters, related, **comments**       |
| `/read/[slug]`   | Reader  | strip + paged modes, auto-hide chrome, settings, EOC, comments |
| `/profile`       | Profile | stats, currently-reading, shelves, fav genres, activity        |
| `/donate`        | Donate  | goal bar, membership tiers, one-time amounts, allocation       |
| `/support`       | Support | search, category cards, FAQ accordion, contact                 |

Shared components: `Header` (+ `SearchOverlay`), `Footer`, `MangaCard`,
`FlagBadge` (SVG JP/KR/CN flags), `Stars`, `Icon`.

> The design's brand name is **YOMU**; "Komika" remains the working project name.

## Backend wiring

The client talks to **one Komika unified GraphQL API** (not Suwayomi's raw schema).
The Komika server federates Suwayomi (catalog/chapters/pages/library/progress) and
adds social + auth + discovery; keeping that boundary server-side keeps the client
clean and stable.

- **Operations** (`packages/api/src/operations.ts`) — domain-shaped GraphQL
  documents, one per `Backend` method. `GraphQLBackend` runs them and returns
  `@komika/types` shapes 1:1, no client mapping.
- **Contract** (`packages/api/src/schema/komika.graphql`) — the SDL the server must
  implement, annotated `[suwayomi]` / `[komika]` per field (incl. `MangaStatus`
  mapping, `fetchChapterPages → Page`, `updateManga`/`updateChapter`).
- **Repository** (`apps/reader/src/lib/data/source.ts`) — screens load through this
  seam via SvelteKit `+page.ts`. It maps live domain data → the screens' view
  shapes when `PUBLIC_KOMIKA_BACKEND=on`, and **falls back to `mock.ts` on any
  failure**, so the app is always renderable. Wired screens: Home, Browse, Updates,
  Library, Profile, Donate, Support, **Series, and Reader** — all through `load` +
  the repository, routed by real backend ids, with mock fallback.
- Default is backend **off** → pure `mock.ts` (verified rendering, 0 console errors).

**Live Suwayomi adapter (real data, verified).** A second `Backend` impl,
`SuwayomiBackend` (`packages/api/src/suwayomi-backend.ts`), talks directly to a
Suwayomi/Tachidesk server's real GraphQL and maps it to `@komika/types` — so the
reader shows real catalog **without** the (unbuilt) Komika server. Selected via
`PUBLIC_KOMIKA_BACKEND_KIND=suwayomi` + `PUBLIC_SUWAYOMI_URL`. It auto-picks an
English source; social/auth return empty (Suwayomi has none).

Local dev backend (a Docker container is running):

```sh
docker start suwayomi        # (created via: docker run -d --name suwayomi -p 4567:4567 ghcr.io/suwayomi/tachidesk:stable)
# MangaDex source installed via the Keiyoushi repo. Suwayomi UI + GraphQL at http://localhost:4567
```

`apps/reader/.env` (gitignored) enables it:
`PUBLIC_KOMIKA_BACKEND=on`, `PUBLIC_KOMIKA_BACKEND_KIND=suwayomi`,
`PUBLIC_SUWAYOMI_URL=http://localhost:4567`, `PUBLIC_KOMIKA_IMG_MODE=direct`.

**Images (wired + verified).** Covers render through a `Cover.svelte` component →
`ImageProvider.resolveCover` → `<img>`. The web provider gained a **direct mode**
(`PUBLIC_KOMIKA_IMG_MODE=direct`) that returns source URLs unchanged when they're
already CORS-safe (Suwayomi proxies source images itself). View shapes carry
`cover`/`id`; `MangaCard` renders the real cover or falls back to the hatch
placeholder. Verified: real MangaDex cover art on Home/Browse (Chainsaw Man,
Mushoku Tensei, Komi Can't Communicate, …), with real chapter counts.

**Ratings & comments (wired + verified, local persistence).** Suwayomi has no
social layer and Komika's own service doesn't exist yet, so `apps/reader/src/lib/
data/social.ts` persists ratings + comment threads to **localStorage**, keyed by
series / chapter, behind a small API. The Series page (1–10 rating + thread) and
Reader (per-chapter 1–5 rating + thread) read/write through it — posts, likes,
and ratings survive reloads. Swappable for `backend.reviews/comments/postReview/
postComment` (multi-user needs the server). Single-device only for now.

## Komika unified server (BUILT + verified)

A **Rust** server at `apps/server` (`komika-server`) implements `komika.graphql`
end-to-end. Chosen for the lowest process footprint (~5–15 MB RSS, no GC) and to
share crates with the Tauri Rust core later. Stack: **axum 0.8 + async-graphql 7 +
async-graphql-axum**, **SQLite via sqlx** (single file, `create_if_missing` +
migrations), **argon2id** password hashing + opaque session tokens, **reqwest** for
Suwayomi federation. Bearer-token auth over `Authorization: Bearer <token>` — matches
the existing `GraphQLBackend` client transport, so no client changes were needed.

Layout (`apps/server`): `src/config.rs`, `src/db.rs`, `src/auth.rs`,
`src/suwayomi.rs` (federation client mirroring the TS `SuwayomiBackend`),
`src/graphql/{types.rs,mod.rs}` (code-first schema = 1:1 mirror of the SDL; Query +
Mutation resolvers), `src/main.rs` (CORS for the reader, GraphiQL at `GET /graphql`,
`/health`). Migrations in `migrations/0001_init.sql`; DB `komika.sqlite3` (gitignored).

- **Federated (→ Suwayomi):** `discovery`, `search`, `series`, `chapters`, `pages`,
  `library`, `mark`, `setProgress` — real MangaDex catalog/covers/pages/chapter counts.
- **Komika-native (→ SQLite):** `register`/`login`/`logout`/`session` (multi-user
  accounts, revocable sessions), `reviews`/`postReview` (1–10, **one per user/series,
  upsert**), `comments`/`postComment` (per-chapter, spoiler flags). `Series.rating`
  is aggregated live from stored reviews (`average`/`count`/`distribution[10]`).
- **Verified live** against the running Suwayomi container: federated discovery/browse
  returns real titles + covers; register→review→aggregate math correct (alice 5 + bob 7
  → avg 6.0, right distribution buckets); upsert updates in place; comments, session
  whoami, anonymous-null, and logout-revokes all confirmed via GraphQL. The **reader
  renders through it** (temporarily pointed at it): Browse shows real covers/counts and
  the ★6.0 aggregate surfaced from SQLite. Zero console/network errors.

Run it: `cd apps/server && cargo run` (defaults: `PORT=8080`, `SUWAYOMI_URL=
http://localhost:4567`, `DATABASE_URL=sqlite://komika.sqlite3`; see `.env.example`).
Point the reader at it: `apps/reader/.env` → `PUBLIC_KOMIKA_BACKEND_KIND=komika`,
`PUBLIC_KOMIKA_API=http://localhost:<port>/graphql` (reader `.env` is otherwise on the
Suwayomi-direct adapter — the persistent Docker container — by default).

**Auth UI wired (BUILT + verified).** The reader has a full sign-in/registration flow
against the server: a runes-based auth store (`apps/reader/src/lib/auth.svelte.ts`)
persists the bearer token to localStorage, injects it into the `backend` seam via the
new optional `Backend.setToken` (GraphQLBackend implements it; the Suwayomi adapter
omits it → `setToken?.()` is a safe no-op), and on load restores + validates the token
via `session()`. A `/login` screen (`routes/(app)/login/+page.svelte`) has Sign in /
Create account tabs, error handling, and `?redirect=` support. The `Header` reflects
auth: a "Sign in" pill when logged out, or an avatar (user initial) with a dropdown
menu (username, Profile, Library, Sign out) when signed in. `initAuth()` runs once from
the root layout. Verified live against the Komika server: login (alice) → avatar +
persisted token; register (carol) → account created + signed in; sign-out clears it;
**session survives a full reload** (token re-validated via `session()`); 0 console errors.

**Social wired to the server (BUILT + verified).** The Series/Reader screens now read/write
the real multi-user social layer through a single seam, `apps/reader/src/lib/data/
social-repo.ts`: when the Komika backend is active it uses `backend.reviews/comments/
postReview/postComment`; otherwise it falls back to the localStorage store (`social.ts`),
preserving the "always renderable" contract for Suwayomi/mock mode.

- **Series page** — the "Reviews" section lists per-series reviews (author, score badge,
  spoiler veil, "You" tag); the 1–10 rating widget upserts the user's review _score_
  (`Stars` gained an `onchange` hook so hydration doesn't post). Reviews couple score+body,
  so a bare rating is a review with an empty body — the server's `postReview` was relaxed
  to allow that; the thread shows only body-bearing reviews. Posting requires a score.
- **Reader page** — per-chapter comment thread via `comments/postComment`, with a spoiler
  toggle + click-to-reveal. The chapter's 1–5 star rating has no backend field, so it stays
  local (documented in-code).
- Both are **auth-gated**: signed out (on the Komika backend) the composer is replaced by a
  "Sign in" prompt, while existing reviews/comments still render (public, read-only).
- **Verified live** (reader pointed at the server, signed in as `carol`): posted a ★9 review
  → appears tagged "You", and on reload the hero aggregate recomputed 6.0 → **7.0 (3)**
  (alice 5 + bob 7 + carol 9); posted a spoiler-flagged chapter comment → veiled, **persists
  across reload from the backend**; signed out → sign-in prompts appear and reviews show with
  no "You" tags. 0 console errors; `check` = 0 errors.

> Likes/replies aren't modelled server-side, so in live mode they're ephemeral client-only
> affordances (reset on reload).

**Library + reading progress wired (BUILT + verified).** The Series "Add to Library" button
and the reader's progress now hit the backend through the repository seam (`source.ts`):
`setLibraryMark(seriesId, marked)` → `backend.mark`, and `saveProgress(chapterId, lastPage,
read)` → `backend.setProgress` (both no-op offline/mock). `SeriesDetailView` gained
`isMarked`, so the button reflects real state on load. The reader marks a chapter **read**
once it reaches the end (strip ≥98% / paged last page, idempotent per chapter) and saves
`lastPageRead` when leaving a chapter; `Stars` etc. unaffected. Verified live (Suwayomi
backend, which proxies these directly): toggling "Add to Library" → "In Library" persisted
(Suwayomi `mangas(inLibrary:true)` = [3] and the Library page lists it); scrolling a chapter
to 100% → Suwayomi records `isRead=true, lastPageRead=36`, and the Series page then shows
Ch.1 **Read** with the CTA advanced to **"Continue Ch. 2"**. 0 console errors; `check` = 0.
Per-user library/progress still proxy to Suwayomi's single global account (real per-user
sync needs the Komika server to own it).

## Admin "manga DB" console (BUILT + verified)

`apps/admin` (was a scaffold) is now a working admin console against the Komika
server. Server side: a `series_admin` table (`migrations/0002_series_admin.sql`)
holds Komika-native per-series overrides (`override_interval_hours`,
`poll_every_minutes`, `paused_override`, `status_override`); an admin-gated
`updateSeriesAdmin(input)` mutation upserts them (whole-state — a null field
clears that override); and `map_series` **folds** the overrides into every
`Series` (status override wins over source; forced pause wins over the
status-derived auto-pause), so the reader sees them too. Admin access is bootstrapped
via `KOMIKA_ADMIN_USERS` (comma usernames promoted at startup + on registration);
`SessionUser.isAdmin` gates the mutation (`require_admin`).

Client: `Backend.updateSeriesAdmin?` (+ `SeriesAdminInput`, `UPDATE_SERIES_ADMIN`
op, SDL). The admin SPA has its own `config`/`context`/`auth.svelte.ts` (login gated
on `isAdmin`, separate `komika-admin-token`), an admin login screen, and a catalog
console (`routes/+page.svelte`): a table of the library (or a search) showing
cover/title/type/status/chapters/scan cadence, with a per-series editor drawer —
status flag, scanner (Auto / Force run / Force pause), interval override, poll cadence.
Saving sends the whole form and swaps the row in place with the recomputed series.

Run it: `apps/admin` dev on port 5273 (launch config "admin"), with
`apps/admin/.env` → `PUBLIC_KOMIKA_API=http://localhost:<server>/graphql`, and the
server started with `KOMIKA_ADMIN_USERS=<name>` + `5273` in `CORS_ORIGINS`.

Verified live: non-admin (alice) and anonymous are rejected ("Admin access
required" / "Not authenticated"); admin login → catalog lists the library; editing
series 3 to COMPLETED + 48h override + 60m poll + force-pause **persisted** (row in
`series_admin`, survives reload, editor pre-fills) and **folded into the public
`series()` query** (status COMPLETED, override 48h/60m, paused). 0 console errors;
`check` = 0 errors (both admin + reader). (Editor scanner uses a heuristic to
show Auto vs forced; when a forced value coincides with the status auto-pause it
reads as Auto — harmless, effective state is correct.)

## Parallel production push (BUILT + verified)

Four independent workstreams built concurrently (non-overlapping file territories),
then integrated + verified together:

- **Adaptive scan scheduler + server hardening** (`apps/server`). A background tokio
  task (`src/scanner.rs`, `SCAN_TICK_SECONDS` default 300) computes each library
  series' rolling avg chapter interval, resolves the effective interval (admin
  override → avg), skips paused/completed/hiatus, re-fetches overdue series from
  Suwayomi, detects new chapters, and persists `series_scan_state`
  (`migrations/0003`) — folded back into `Series.scan` (avg/last/next). New
  `scanStatus` query. Hardening: security response headers (`X-Content-Type-Options`,
  `X-Frame-Options: DENY`, `Referrer-Policy`, `Permissions-Policy`) + a sliding-window
  auth rate limiter on login/register (`AUTH_RATE_LIMIT_MAX`/`_WINDOW_SECS`). Verified
  live: scanner ticked, computed ~695h interval for series 3, `scanStatus` returns
  `{librarySize,overdueCount,lastTickAt,nextDueAt}`, headers present. `cargo build` +
  `fmt` + `clippy -D warnings` + `--release` all clean.
- **Image pipeline** (`apps/worker` + `packages/api/src/image-provider.ts`). A
  Cloudflare Worker: `GET /img?src=<url>` — validates + host-allowlists (empty list
  fails **closed**), hotlink protection, `image/*` Content-Type enforcement, edge
  cache (Cache API) only (no object-storage tier — B2 removed in `6d06784`), streamed
  bodies, immutable cache headers. Client `WebImageProvider` proxy mode builds the
  Worker URL; native builds fetch directly in Rust (host-guarded against SSRF, UA +
  Referer set). `tsc` clean. **Code-complete; needs a Cloudflare account to run live.**
- **Reader polish / a11y / virtualization** (`apps/reader/src`). Long-strip
  **virtualization** (IntersectionObserver windowing — verified ~4 `<img>` mounted of
  38 cells, window moves on scroll, offscreen torn down, aspect-ratio reserves layout);
  loading skeletons + empty + error states across screens (incl. reader "no pages");
  a11y pass (skip-to-content, focus-visible, keyboard nav, reduced-motion, fixed both
  prior warnings) → `check` = **0 errors / 0 warnings**. (Follow-up: 3 faintest text
  tokens in `packages/ui/tokens.css` are below AA contrast.)
- **CI/CD + deploy + e2e** (new `.github/`, `deploy/`, `e2e/`, `PRODUCTION.md`).
  `ci.yml` (web check+build, rust fmt/clippy/build, gated e2e + Lighthouse stubs);
  `deploy/` Dockerfiles + `docker-compose.yml` (suwayomi + server-with-persistent-SQLite
  - nginx reader) — **compose config validates**; standalone Playwright smoke tests;
    `PRODUCTION.md` readiness tracker.

Integration notes: root install needed `allowBuilds: {esbuild,sharp,workerd: false}` in
`pnpm-workspace.yaml` (the worker pulled build-script deps). Server config now takes
`Arc<AppState>` (shared with the scanner). **Next engineering priority: production
hardening & reliability** — data durability (SQLite has no PITR — add Litestream-style
backup or move to Postgres), auth completeness (email verify + password reset),
observability, secrets/TLS/host. The gate on a _public_ launch remains the
legal/distribution/monetization decision.

## One-command self-host deployment (BUILT + verified)

`deploy/` stands up the entire public stack with `./deploy.sh` — "hosted
Mihon/Tachiyomi + social layer + auto-updating catalog":

- **`deploy/docker-compose.yml`** — four services: `suwayomi` (sources + image
  serving), **`flaresolverr`** (Cloudflare-challenge solver, internal-only), `server`
  (komika-server + adaptive scanner), `reader` (nginx static SPA). Parameterized by
  `deploy/.env` (`PUBLIC_HOST`, ports, admin creds, `SEED_SERIES`, `SCAN_TICK_SECONDS`).
- **`deploy/deploy.sh`** — preflight (docker/compose/python), build + up, wait-healthy,
  run bootstrap, print URLs. Subcommands: `up` / `bootstrap` / `down` / `destroy` / `logs`.
- **`deploy/bootstrap.py`** (stdlib only, idempotent) — registers the Keiyoushi repo,
  installs the **MangaDex** source, **enables FlareSolverr** in Suwayomi
  (`setSettings flareSolverrEnabled=true, flareSolverrUrl=http://flaresolverr:8191`),
  creates the Komika admin, and **seeds the library** from `SEED_SERIES` so the scanner
  immediately has series to auto-update. Verified live against the running stack.
- **Public image wiring (server change):** federation stays internal
  (`SUWAYOMI_URL=http://suwayomi:4567`) but image URLs (covers/pages) are handed to the
  browser, so the server now rewrites them to **`SUWAYOMI_PUBLIC_URL`** (a
  browser-reachable host) via a separate `image_base_url` in `SuwayomiClient` (falls back
  to `SUWAYOMI_URL`). Verified: `coverUrl` uses the public base while GraphQL still
  federates internally. `cargo build`/`fmt`/`clippy` clean.
- **Durable data (Litestream):** SQLite is now opened in **WAL mode** (+ `synchronous
NORMAL`, 5s busy_timeout) in `db.rs`. The server image bundles **Litestream**; the
  container entrypoint (`deploy/server-entrypoint.sh`) runs `litestream replicate -exec
komika-server` when `LITESTREAM_*` env is set — streaming the WAL to S3/B2 (point-in-time
  restore, ~seconds RPO) and **auto-restoring on a fresh volume** — else it runs the server
  plain (backup off by default). Config in `deploy/litestream.yml`; opt-in vars in
  `deploy/.env`. Chosen over Postgres deliberately: the store is a low-write social/auth
  layer on a single low-resource node, so Litestream solves durability with ~zero overhead
  and no code/schema change; Postgres would only pay off with multiple server instances.

> FlareSolverr is heavy (headless Chromium) and only needed for CF-protected sources —
> MangaDex is an open API and doesn't use it. Suwayomi's `:4567` is unauthenticated, so a
> real deploy should reverse-proxy it to expose only image routes (or use the Worker image
> path). See `deploy/README.md`.

## Status

- [x] Monorepo + both SvelteKit apps (static SPA)
- [x] Tauri v2 shell on the reader (desktop; mobile init pending SDKs)
- [x] Data-layer seams: `Backend` + `ImageProvider` (web/native)
- [x] Real design tokens + self-hosted fonts
- [x] **All 9 reader screens implemented + shared component library**
- [x] Reader builds + type-checks (0 errors); verified live in the browser
- [x] Typed GraphQL client (`operations.ts` + `GraphQLBackend`) + SDL contract
- [x] Repository seam + `load` wiring (7 screens) with mock fallback
- [x] **Stand up the Komika unified server** (Rust; implements `komika.graphql`, federates Suwayomi + real multi-user social/auth on SQLite) — verified live
- [x] Wire Series + Reader through the repository (needs id-routing + social shapes)
- [x] Login/register UI + auth store (`setToken`, session restore/validate, Header account menu) — verified
- [x] Swap the Series/Reader social sections off `social.ts` → `backend.reviews/comments/postReview/postComment` (via `social-repo.ts`, auth-gated, localStorage fallback) — verified
- [x] **Admin "manga DB" console** (`apps/admin`): admin-gated catalog console — per-series scan-interval override, poll cadence, pause, and status flags, persisted server-side and folded into `Series` — verified
- [x] Real covers via `ImageProvider` (direct mode); verified against Suwayomi
- [x] Ratings + comments wired (local persistence store); verified across reloads
- [x] Series page wired to backend — real detail, genres, chapters, poster cover
- [x] Reader wired to backend — real chapters, pages, page images, chapter nav
- [x] Build the Komika unified server + real multi-user social (server done; reader UI swap pending)
- [ ] Cloudflare Worker image proxy (edge-cache only; no B2 tier)
- [ ] Android SDK/NDK setup for mobile builds

```

```
