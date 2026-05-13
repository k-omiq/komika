# Plan — Native embedded Suwayomi (fat client) + hosted server for social & catalogue

> **Goal.** Desktop and mobile apps fetch **content (chapter lists + page images) on-device** via an
> embedded Suwayomi engine, so they never depend on our server to *scrape* chapters. The hosted
> server keeps its role: **(1)** unified social system (ratings, comments, users, library tracking),
> **(2)** a universal, always-updated series **catalogue** (repeatedly scanning Suwayomi extension
> repos), **(3)** content fetching **for web users only** (Suwayomi + Cloudflare Worker images).
> This is the TachiManga model for the client, plus our server for the value-add.

> **Status:** Phase 0 + Licensing done; Phase 1 (desktop sidecar) + the core of Phase 2 built and
> behind `PUBLIC_KOMIKA_NATIVE_ENGINE` (default off). See **§0a Implementation status** below.

---

## 0a. Implementation status (updated; supersedes stale "nothing built yet" notes inline)

**Landed on `feat/native-suwayomi` (all gates green: reader svelte-check 344/0/0, src-tauri 13/0,
server 112/0):** Phase 0 bridge (`workSources`/`source_extension`), AGPL licensing, the Rust sidecar
supervisor (`src-tauri/src/suwayomi.rs`: brokered loopback port, `server.conf` boot with
`kcefEnabled=false`, readiness gate, capped-backoff restart, `suwayomi_gql`/`suwayomi_status`/
`suwayomi_base_url`/`suwayomi_image` commands), the Tauri bundle wiring (`bundle.resources` + jlink-JRE
`build-jre.sh` (61 MB) + SHA-verified `fetch-suwayomi-jar.sh` + tightened IPC-only CSP), the
`native-sidecar.yml` CI matrix, the `LocalSuwayomiBackend` (real v2.3.2243 contract + on-device
extension provisioning + work→engine-id resolution), the `CompositeBackend` local-serving wiring, the
`NativeImageProvider` engine-path branch, and `context.ts` activation.

**Model corrections discovered by the N-GQL-SPIKE (see
`apps/reader/src-tauri/suwayomi/GQL-SCHEMA-FINDINGS.md` for the evidence) — these override the plan
where it conflicts:**
- **No getOrInsert-by-url.** `fetchSourceManga` is a browse/search (needs `type`+`page`), there is no
  `fetchSourceChapters`, and per-manga ops key off the engine's **integer** `MangaType.id`. Resolve a
  work's `sourceKey`(=`MangaType.url`) → id via `mangas(condition:{sourceId,url})` (fast path) then a
  MangaDex `fetchSourceManga(SEARCH,"id:<uuid>")` (exact + persists). This replaces D6/§7's clean
  "one-hop by-key fetch." Non-MangaDex sources use a title-search fallback.
- **Embedded-engine page URLs are relative `/api/...` proxy paths, not CDN URLs** — served over IPC by
  the new `suwayomi_image` command (not `fetch_image`). §8's "MangaDex→`fetch_image`" applies only to
  the hosted-fallback path.
- **MangaDex needs its extension on-device** (`eu.kanade.tachiyomi.extension.all.mangadex` from the
  Keiyoushi store) and its engine source id resolved dynamically by language — none of that is in
  `workSources` (extension is null for MangaDex), so the client supplies it (hardcoded two-tier coords).

**MVP scoping of the live read (deliberate, to preserve progress correctness):** chapter identity +
list + progress stay canonical/hosted (unchanged); only **page image bytes** serve live from the engine
via a number-matched (D7) reconciliation map in the composite (`canonicalChapters` builds it,
`canonicalPages` consults it; engine integer ids never reach `setProgress`). Merging *live/new* chapters
into the list and progress-keyed-by-number are **deferred to N2.3**.

**Gate C (§3.5) scorecard:** ✅ cold-start `<30 s` + status states (proven) · ✅ no orphaned java
(proven) · 🟡 kill→degraded+restart (implemented + unit-covered accounting; live kill not automated) ·
✅ **live MangaDex read** — **proven end-to-end (logic) against a live v2.3.2243 engine** via a
shimmed-`invoke` integration harness that drives the REAL `CompositeBackend` + `LocalSuwayomiBackend`
against a mock hosted (5/5 assertions, reproduced twice): hosted chapter identity preserved; D7
number-match reconciliation serves engine `/api/v1/...` page paths (not the hosted fallback); those
bytes are real JPEG (376 KB, `FF D8 FF`); an unmatched chapter falls back to hosted; the scanlator
tiebreak picks the matching engine chapter. Remaining unverified sliver (**low-risk**): the literal
DOM `<img>` blob paint (`URL.createObjectURL` over the proven bytes — not engine-dependent) and a
real hosted server returning `workSources` for a network-scanned MangaDex work (the mock used the exact
`load_work_sources` shapes; hosted resolvers have 112 passing tests). The native Tauri desktop window
itself can't be automated headless.

---

## 0. Decisions locked up front (change these here, not inline)

| # | Decision | Value | Rationale |
|---|----------|-------|-----------|
| D1 | Client content engine | **Embed Suwayomi-Server** (reuse the whole extension ecosystem) rather than reimplement an engine | Aidoku-style rewrite is years of per-source maintenance; Suwayomi already implements every extension |
| D2 | Source of truth for **library membership + reading progress + reviews** | **Hosted server** (existing `canonical_library`, `canonical_progress`, `reviews`) | Powers cross-device sync + the social/activity layer we already built |
| D3 | Source of truth for **catalogue metadata** (title, cover, description, rating, NSFW, "new chapter" signals) | **Hosted server** | Universal, deduped, always-updated; the app must not re-derive it |
| D4 | Source of truth for **chapter list + page URLs** on native | **Embedded Suwayomi** (live from source) | This is the whole point — apps fetch content themselves |
| D5 | Local Suwayomi's own library/progress/history | **Not authoritative** — the local engine is a stateless fetcher; we may add a manga to its library only to drive its downloader | Avoids two conflicting library truths |
| D6 | App-facing series id | Stays the canonical **`w_<work>`** id; the composite backend resolves it to `(source, sourceKey)` internally | UI, social, and catalogue all already speak work ids |
| D7 | Chapter identity join key across server-mirror ↔ local-source | **chapter number** (`f64`), scanlator as a tiebreaker | Server mirror and live source won't share opaque ids |
| D8 | Web build | Unchanged — hosted Komika backend + Worker images | Web can't scrape cross-origin; keep it server-fed |

**Licensing — DECIDED (§12): the whole project ships AGPL-3.0, open-source.** No sign-off from the
Suwayomi maintainers is needed; we just comply (publish source incl. our Suwayomi fork, keep notices).
**Distribution — DECIDED (§12a): no Apple App Store** — Android (F-Droid) + desktop are primary; iOS is
best-effort self-sideload / AltStore. Neither item blocks Phase 0 or desktop work.

---

## 1. Target architecture

```
                         ┌────────────────────────── DEVICE (Tauri app) ──────────────────────────┐
                         │                                                                          │
  ┌───────────┐   social/catalogue/library/progress   ┌──────────────────┐                         │
  │  Hosted   │◀────────────────────────────────────▶ │  CompositeBackend │  (packages/api)         │
  │  komika-  │   GraphQL /graphql (Bearer token)      │   implements      │                         │
  │  server   │                                        │   Backend         │                         │
  │           │                                        └───────┬──────────┘                         │
  │  social   │                                                │ content (series/chapters/pages)     │
  │  catalogue│                                                ▼                                     │
  │  scanner  │                                        ┌──────────────────┐   spawn+supervise        │
  │  (web     │                                        │ LocalSuwayomi     │◀──────────┐             │
  │   fetch)  │                                        │ Backend (JS)      │           │             │
  └───────────┘                                        └───────┬──────────┘   ┌────────┴─────────┐   │
                                                               │ 127.0.0.1:PORT│  Tauri Rust core │   │
                                                               ▼               │  - sidecar mgr   │   │
                                                       ┌──────────────────┐    │  - fetch_image   │   │
                                                       │ Embedded Suwayomi│◀───┘  - port broker   │   │
                                                       │ (JVM, loopback)  │                          │
                                                       │ extensions on-dev│                          │
                                                       └──────────────────┘                          │
                         └──────────────────────────────────────────────────────────────────────────┘

  WEB build: CompositeBackend collapses to the hosted GraphQLBackend + WebImageProvider (Worker). No local engine.
```

**Routing table — which backend answers each `Backend` method** (`packages/api/src/backend.ts`):

| Method | Native (app) | Web |
|--------|--------------|-----|
| `session/login/register/logout` | hosted | hosted |
| `updateProfile/uploadAvatar/myActivity` | hosted | hosted |
| `reviews/myReview/postReview/comments/postComment` | hosted | hosted |
| `discovery/updates/search` (catalogue) | **hosted** (deduped universal catalogue) | hosted |
| `library` (membership) | **hosted** (source of truth, D2) | hosted |
| `mark` | **hosted** (writes membership) + async prime local engine | hosted |
| `setProgress` | **hosted** (source of truth) | hosted |
| `series` (metadata) | **hosted** catalogue metadata, merged with local live chapter count | hosted |
| `chapters` (list) | **local Suwayomi** (live), reconciled to server "new chapter" signals by number (D7) | hosted |
| `pages` (page image URLs) | **local Suwayomi** | hosted |
| image byte resolution | `fetch_image` (direct) or local Suwayomi proxy, per source (§8) | Worker proxy |

Everything social/identity/catalogue is hosted; only **chapter list + pages + image bytes** are local.

---

## 2. Phase 0 — Server bridge: expose the source mapping (do this first; additive, safe)

The server already stores the mapping in `source_series (work_id, source_type, source_id, source_key,
source_url, is_nsfw)` (migration `0005_canonical_works.sql`). Today it is **not exposed to clients**.
The app cannot fetch natively without it. This phase is pure addition — no behavior change for web.

### 2.1 Record extension coordinates during catalogue scan
The device must install the *exact* extension a `source_series` came from. Extend the scan/catalogue
writer so each `source_series` (or a new `source_extension` table) records, per `source_id`:
- `repo_url` (e.g. the Keiyoushi repo the extension came from),
- `pkg_name` (Suwayomi/Tachiyomi extension package id, e.g. `eu.kanade.tachiyomi.extension.en.mangadex`),
- `apk_name` / extension artifact id, and `version_code` at catalogue time,
- `lang`.

New migration `0017_source_extension.sql`:
```sql
CREATE TABLE source_extension (
    source_id    TEXT PRIMARY KEY,   -- Suwayomi source id (matches source_series.source_id)
    pkg_name     TEXT NOT NULL,
    repo_url     TEXT NOT NULL,
    apk_name     TEXT,
    version_code INTEGER,
    lang         TEXT,
    is_nsfw      INTEGER NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL
);
```
Populate it in the scanner where sources are enumerated (mirror what the operator-side Suwayomi
reports via its `extensions`/`sources` GraphQL). This is the same data the server already sees.

### 2.2 New GraphQL surface
Add to `packages/api/src/schema/komika.graphql` + Rust `graphql/types.rs`/`mod.rs`:
```graphql
type WorkSource {
  sourceType: String!   # "mangadex" | "suwayomi"
  sourceId: String!     # Suwayomi source id
  sourceKey: String!    # manga id/slug within the source
  sourceUrl: String
  isNsfw: Boolean!
  lang: String
  extension: SourceExtension   # null for mangadex-native fetch
}
type SourceExtension {
  pkgName: String!
  repoUrl: String!
  apkName: String
  versionCode: Int
  lang: String
}
extend type Query {
  # All source mappings for a work, best-preferred first (MangaDex, then curated Keiyoushi).
  workSources(workId: ID!): [WorkSource!]!
  # Bulk form for prefetch (library screen): map many works at once.
  workSourcesBatch(workIds: [ID!]!): [WorkSourceGroup!]!
}
type WorkSourceGroup { workId: ID!  sources: [WorkSource!]! }
```
Rust resolver: `SELECT ... FROM source_series JOIN source_extension USING(source_id) WHERE work_id=?
ORDER BY (source_type='mangadex') DESC, last_seen DESC`. NSFW-gate by the viewer's `show_nsfw`
(reuse `filter_nsfw`/`user_show_nsfw`). Add resolver tests mirroring the existing style
(`graphql/mod.rs` tests: authed, NSFW-gated, ordering).

### 2.3 Acceptance (Phase 0)
- `workSources(w_…)` returns ≥1 mapping with usable `(sourceId, sourceKey, pkgName, repoUrl)`.
- Web build unaffected (`svelte-check` 0/0, `cargo test` green, existing behavior identical).
- No client changes yet.

---

## 3. Phase 1 — Desktop sidecar (macOS first, then win/linux)

### 3.1 Bundle the JVM + Suwayomi-Server
- Pin a Suwayomi-Server release (jar) — do **not** float `:stable`. Vendor a specific
  `Suwayomi-Server-vX.Y.Z.jar` and record its SHA-256 in-repo (`apps/reader/src-tauri/suwayomi/VERSION`).
- Ship a **minimal JRE via `jlink`** (only the modules Suwayomi needs) per `(os, arch)`:
  `macos-aarch64`, `macos-x86_64`, `windows-x86_64`, `linux-x86_64`. Target ~40–60 MB compressed.
  Build these in CI (§11), not by hand.
- Lay them out as Tauri **`externalBin`/`resources`**:
  ```
  src-tauri/
    resources/suwayomi/Suwayomi-Server.jar
    resources/jre/<target>/...           # per-target, selected at runtime by cfg!(target_os/arch)
  ```
  Register in `tauri.conf.json → bundle.resources`. (No `externalBin` sidecar binary — we launch
  `<jre>/bin/java -jar Suwayomi-Server.jar`, so it's a resource + a spawned process, not a Tauri sidecar
  shim. This avoids per-target sidecar naming rules.)

### 3.2 Sidecar supervisor (Rust, `src-tauri/src/suwayomi.rs`)
A dedicated module owning the child process lifecycle. Concrete responsibilities:
- **Data dir:** `app_handle.path().app_data_dir()?/suwayomi` (per-OS correct). Pass to Suwayomi via its
  data-root env/flag so its DB, extensions, and downloads live in the app sandbox.
- **Port:** bind an **ephemeral loopback port** ourselves (`TcpListener::bind("127.0.0.1:0")`), read the
  assigned port, close it, and pass it to Suwayomi (`TACHIDESK_SERVER_IP=127.0.0.1`,
  `TACHIDESK_SERVER_PORT=<port>`). Never a fixed 4567 (collision-prone; also lets us run alongside a dev
  server). Store the chosen port in an `OnceLock`/state.
- **Launch:** `Command::new(java).args(["-jar", jar, ...]).env(...).spawn()`, capturing stdout/stderr into
  the tauri-log plugin with a `suwayomi` target. JVM flags: `-Xmx` cap (e.g. 256–512 MB), headless
  (`-Djava.awt.headless=true`), disable the WebUI it bundles.
- **Readiness gate:** poll `POST http://127.0.0.1:<port>/api/graphql {query: "{ aboutServer { version } }"}`
  every 250 ms up to a timeout (~30 s cold start). Expose readiness as app state; the JS side awaits it
  before issuing any content call.
- **Crash supervision:** if the child exits unexpectedly, restart with backoff (cap N restarts/минute),
  surface a degraded state to the UI, and fall back to server fetch (§7 fallback) meanwhile.
- **Single instance:** a lockfile in the data dir prevents two app instances double-spawning; second
  instance reuses the running port (or refuses).
- **Graceful shutdown:** on `RunEvent::ExitRequested`/window close, send Suwayomi a shutdown (its GraphQL
  has a shutdown mutation) then `kill()` after a grace period; never orphan the JVM. On mobile, tie to
  app-background/terminate lifecycle.
- **Commands exposed to JS** (`#[tauri::command]`):
  - `suwayomi_base_url() -> Option<String>` (e.g. `http://127.0.0.1:<port>`), `None` until ready.
  - `suwayomi_status() -> { state: "starting"|"ready"|"degraded"|"stopped", version, lastError }`.

### 3.3 Capabilities / CSP
- `capabilities/default.json`: add the permissions for the shell/process, path, and the log plugin as
  used. Keep least-privilege.
- `tauri.conf.json` CSP `connect-src`: the port is dynamic, so we can't hardcode it. Options, pick one:
  (a) bind Suwayomi to a **fixed loopback port range** and whitelist `http://127.0.0.1:<fixed>`; or
  (b) route all local-Suwayomi calls **through a Rust command** (JS never `fetch()`s the JVM directly —
  it calls a `suwayomi_gql(query, vars)` command that proxies to the loopback port). **Prefer (b):** it
  keeps CSP tight (`connect-src 'self' ipc:`), hides the port, and lets the Rust layer enforce
  loopback-only + add the SSRF-safe image handling in one place. It costs one IPC hop per GraphQL call
  (fine for content calls; pages are few).

### 3.4 `LocalSuwayomiBackend` (JS, `packages/api/src/local-suwayomi-backend.ts`)
- A close cousin of the existing `SuwayomiBackend` (`suwayomi-backend.ts`), but:
  - transport is the **Tauri `suwayomi_gql` command** (option b), not `fetch`;
  - it does **not** auto-pick a single source — it fetches a *specific* `(sourceId, sourceKey)` handed to
    it by the composite backend (from `workSources`);
  - it implements only the content methods (`series`, `chapters`, `pages`, and the fetch primitives);
    social/library/progress throw / are unused (routed to hosted).
- Reuse the query set already proven in `suwayomi-backend.ts`
  (`fetchSourceManga`, source `fetchManga`/detail, `fetchChapterList`, `fetchChapterPages`).

### 3.5 Acceptance (Phase 1)
- Cold-start on macOS arm64: sidecar reaches `ready` < 30 s; `suwayomi_status` reflects states.
- Open a MangaDex-catalogued series → `workSources` → local Suwayomi returns a live chapter list and
  page URLs → images render via `fetch_image`.
- Kill the JVM mid-session → app shows degraded, auto-restarts, content still loads via fallback.
- App exit leaves **no orphaned java process** (verified with `pgrep`).

---

## 4. Phase 2 — Extension management on-device

The engine is useless without the right extension installed. Today extension management is
operator-side only (SPEC §"extension management is operator-side only"); on native we must manage it
per device — but **driven by the catalogue**, never a user-facing "install sources" UI (keeps the
"auto-served" product promise).

- **Provisioning:** when the app needs `(sourceId, pkgName, repoUrl, versionCode)` for a work and that
  extension isn't installed/updated locally, install it via Suwayomi's extension GraphQL
  (`fetchExtensionRepo` / `installExtension(pkgName)` / `updateExtension`). Add the repo first
  (`createExtensionRepo(repoUrl)` for the curated Keiyoushi repo + MangaDex).
- **Sync policy:** keep the device's extension versions ≥ the `version_code` the server catalogued with
  (from §2.1) so `sourceKey`s resolve. A lightweight "extension manifest" query
  (`installedExtensions`) reconciled against `workSources` results.
- **Concurrency + caching:** install-once, in-flight dedupe (don't install the same pkg twice), cache
  "installed set" in app state; re-check on app launch and on catalogue version bumps.
- **Failure handling:** if an extension can't install (repo down, incompatible), mark that
  `source_series` unusable on-device and **fall back to server fetch** for that work; log + optional
  one-time toast. Never hard-fail the read.
- **NSFW:** respect the viewer's `show_nsfw`; don't auto-install NSFW-only sources for opted-out users.

**Acceptance:** first open of a work whose source isn't installed transparently installs it and reads;
subsequent opens skip install; a bogus repo falls back to server fetch without a crash.

---

## 5. Phase 3 — Android

- Android runs **ART**, not HotSpot; a desktop server jar is not directly runnable. The proven path is
  **TachiManga's mobile fork of Tachidesk-Server** (`github.com/tachimanga/Tachidesk-Server`), which is
  patched to run on mobile. **Action:** study that fork and either (a) reuse its patches to produce our
  own mobile-runnable Suwayomi artifact, or (b) run an embeddable JVM. Do a **spike** to pick.
- Bundle strategy: the mobile Suwayomi artifact + its runtime as Android assets/`jniLibs`; launch as a
  foreground service (long-running) from the Tauri Android shell; expose the same loopback GraphQL.
- Tauri mobile: `tauri android init` + Android SDK/NDK (already noted in SPEC as pending).
- Everything above the transport (composite backend, extension mgmt, fallback) is reused unchanged.

**Acceptance:** same as Phase 1 acceptance, on a physical Android device; foreground-service survives
backgrounding; battery/among-memory sane.

---

## 6. Phase 4 — iOS (hardest)

- iOS bans third-party **JIT** but permits **AOT/interpreted** execution, so a JVM can run in
  **interpreter-only** mode inside the app. This is exactly how **TachiManga** ships Suwayomi on the App
  Store. **Action:** evaluate TachiManga's approach/fork as the reference; a spike to get *any* JVM
  running the server in-process on a device is the gating milestone before committing.
- **Cloudflare: use the on-device WebView, NOT FlareSolverr (§8b).** TachiManga solves Cloudflare on iOS
  with **WKWebView** ("Bypassing Cloudflare automatically", TachiManga changelog v2.3) — the same
  Tachiyomi/Mihon WebView-interceptor mechanism. FlareSolverr is only the *server-side* stand-in for a
  browser; on a device we already have one. So CF-gated sources **are** fetchable on iOS. The work is the
  **WebView↔engine bridge** in §8b, which stock Suwayomi lacks and TachiManga's fork provides.
- Interpreter-mode JVM is **slower**; validate cold-start and first-page latency are acceptable, and cap
  JVM memory tightly (iOS is aggressive about memory).

**Acceptance:** MangaDex + a non-CF Keiyoushi source read on a physical iPhone; a **Cloudflare-gated
source reads via the WKWebView interceptor (§8b)** on-device (server-fetch only if the WebView solve
fails); app passes App Store review constraints (no JIT, no private API).

---

## 7. Content flow details & fallback (applies to all native platforms)

**Opening a series (native):**
1. UI has a `w_` id. Composite backend fetches **metadata** from hosted `canonicalSeries(workId)` (title,
   cover, description, rating, NSFW).
2. Composite backend fetches `workSources(workId)` → picks the preferred usable source (MangaDex first;
   else curated source), ensuring its extension is installed (§4).
3. Composite backend asks `LocalSuwayomiBackend` for the **live chapter list** for `(sourceId, sourceKey)`.
4. Reconcile: join local chapters to the server's mirrored "new chapter" signals by **chapter number**
   (D7) to drive Updates/unread badges; display the local list for reading.
5. **Reading progress + library** read/write go to the **hosted server** (D2), keyed by `w_` id +
   chapter number.

**Pages:** `LocalSuwayomiBackend.pages()` → page URLs → image bytes via §8.

**Fallback ladder (never hard-fail a read):**
1. Local Suwayomi resolves it → use it.
2. Local engine not ready / extension missing / source errored / WebView CF-solve failed (§8b) →
   **server fetch** (`canonicalChapters`/`canonicalPages` for MangaDex works; a new server "resolve this
   source_series" endpoint for non-MangaDex) → use it.
3. Neither → clear, actionable error state (not a spinner-of-death).

The fallback keeps the app usable during the long tail of Phases 3–4 and for CF sources.

---

## 8. Images on native

- **MangaDex sources:** resolve page URLs to `uploads.mangadex.org` / `*.mangadex.network` and fetch via
  the existing `fetch_image` Rust command (already sets the UA + same-origin Referer MangaDex requires;
  already SSRF-hardened). Keeps web/native parity (SPEC I4).
- **Other (Keiyoushi) sources:** many need per-source headers/cookies/Referer that only the extension
  knows. Prefer letting **local Suwayomi proxy the image itself** (`<loopback>/api/v1/...` image
  endpoint) so the extension's request context is applied, then hand that loopback URL to the image
  provider. Add a `NativeImageProvider` branch: MangaDex → `fetch_image`; else → local Suwayomi proxy
  URL (via the `suwayomi_gql`/command transport, or a dedicated `suwayomi_image(path)` command that
  streams bytes over IPC like `fetch_image` does).
- Update `ImageProvider` (`packages/api/src/image-provider.ts`) with the native/source-aware branch;
  keep the web `WebImageProvider` untouched.

---

## 8b. Cloudflare-protected sources — on-device WebView interceptor (not FlareSolverr)

Many sources sit behind Cloudflare. Our **server/web** path uses **FlareSolverr** (headless Chromium) for
this — fine on a server. On **device**, we do what TachiManga/Mihon do: solve the challenge in the
**platform WebView** and reuse the clearance cookie. FlareSolverr is unnecessary (and impossible on iOS).

**Mechanism (the Tachiyomi/Mihon "CloudflareInterceptor" pattern):**
1. An HTTP interceptor in the engine detects a Cloudflare challenge response (403 + challenge / Turnstile /
   managed challenge) for host `H`.
2. The app loads the challenge URL for `H` in a **real platform WebView** — **WKWebView (iOS)**, Android
   `WebView`, and on desktop the OS webview (WebKitGTK / WebView2 / WKWebView) — headless/hidden when the
   challenge is non-interactive, shown only when a CAPTCHA needs a tap.
3. The WebView executes Cloudflare's JS and receives a **`cf_clearance` cookie** bound to that WebView's
   **User-Agent** (and IP).
4. Harvest `cf_clearance` (+ the exact UA) from the WebView cookie store and **inject them into the
   engine's HTTP client** for subsequent requests to `H`. Persist per-host, refresh on expiry/re-challenge.

**Why it matters here:** stock **Suwayomi-Server drives FlareSolverr, not a device WebView** — so embedding
vanilla Suwayomi still needs a **WebView↔engine bridge** to replace FlareSolverr on-device. This is (almost
certainly) a core part of `tachimanga/Tachidesk-Server`, so "study/reuse TachiManga's fork" (Phases 3–4)
covers both JVM-on-iOS *and* this bridge. Concretely, the bridge must:
- expose a native command the JVM engine can call: "solve challenge for URL `U`, return
  `{ cookies, userAgent }`" — implemented on top of the platform WebView;
- feed the returned cookie/UA back into the engine's OkHttp cookie jar + UA for host `H`;
- unify the WebView UA and the engine UA (they **must match** or Cloudflare rejects the replay);
- handle the interactive case (show the WebView, let the user solve a CAPTCHA, then resume).

**Desktop note:** Tauri's own webview can be reused, or a hidden secondary webview window created for the
challenge; same harvest-cookie flow. **Server note:** keep FlareSolverr for the hosted/web path only —
it never ships in the app.

**Fallback:** if the WebView bridge fails for a host (persistent challenge, headless-blocked), fall back to
server fetch for that `source_series` (§7 ladder) as a safety net — but this is the exception, not the
iOS rule.

---

## 9. Offline & downloads

- Suwayomi has a **download manager**; on-device it can download chapters to the app data dir. Expose an
  app "download" action that (a) records the chapter in the **hosted** library/progress model and (b)
  triggers a local Suwayomi download for offline.
- Offline reads serve from the local download; **progress is queued** (local durable queue) and synced to
  the hosted server when connectivity returns (reconcile by `w_` id + chapter number).
- Cache eviction policy + a storage-usage screen (avatars are tiny; downloads are the real disk cost).

---

## 10. Client composite backend (concrete)

- New `packages/api/src/composite-backend.ts`: `class CompositeBackend implements Backend`, constructed
  with `{ hosted: GraphQLBackend, local: LocalSuwayomiBackend | null, resolveSources }`.
- Method bodies follow the §1 routing table verbatim. Content methods first check
  `await isLocalReady()`; if not ready → hosted fallback.
- `packages/api/src/context.ts` (reader) selects the backend:
  - `isTauri()` → `CompositeBackend(hosted, local)`; images → `NativeImageProvider`.
  - web → today's `GraphQLBackend` + `WebImageProvider` (unchanged).
- Keep `setToken` wired to the hosted backend only (local engine is tokenless/loopback).
- Type-safety: `LocalSuwayomiBackend` implements only the content subset; composite never calls social on
  it. Add a narrow `ContentBackend` interface so this is enforced at compile time.

---

## 11. Build, CI, packaging

- **JRE build job** (matrix `os×arch`): `jlink` a minimal runtime, cache by JDK version, attach as CI
  artifacts consumed by the Tauri bundle step. Record sizes; fail if a JRE exceeds a budget.
- **Suwayomi jar:** vendored + SHA-256 verified in CI (supply-chain: never fetch `:stable` at build).
- **Desktop bundling:** `tauri build` per target; macOS **codesign + notarize** the app *and* the bundled
  `java`/dylibs (notarization rejects unsigned nested executables); Windows Authenticode; Linux AppImage.
- **Android:** SDK/NDK in CI, `tauri android build`, signing config.
- **iOS:** `tauri ios build`; **no App Store** (§12a) → sign with a **$99 dev/ad-hoc profile** for
  dev+testers, and produce a sideload-friendly `.ipa` (self-build / AltStore / EU marketplaces for users);
  still respect the on-device constraints (no JIT entitlement, no private APIs); interpreter-JVM artifact
  codesigned.
- Add a CI **smoke matrix**: boot the sidecar headless, hit `aboutServer`, fetch one known chapter list,
  assert non-empty — per desktop target (mobile smoke is device/emulator-gated, keep manual initially).
- Binary-size budgets tracked per platform (JRE is the elephant).

---

## 12. Licensing — DECISION: whole project ships AGPL-3.0, open-source

**Decided.** Suwayomi-Server is AGPL-3.0; rather than fight it, **Komika (apps + server + packages) is
released under AGPL-3.0 with public source.** This is the intended, permission-free way to build on AGPL
software — the license *is* the grant; **no sign-off from the Suwayomi maintainers is needed**, we just
comply. (Closed-source-and-hide-it was rejected: it's copyright infringement, trivially discoverable from
the bundled JVM+jar, and self-defeating.)

**Obligations we take on (make these real, per release):**
1. **License the repo AGPL-3.0** (root `LICENSE`), including our own original code (reader UI, composite
   backend, komika-server). AGPL is "sticky" — plan a **contributor CLA** from day one if we ever want to
   keep relicensing options open.
2. **Publish Corresponding Source** that matches each distributed binary, with build/install scripts
   (jlink JRE steps, vendored Suwayomi jar + SHA, Tauri bundle) — "source is on GitHub" isn't enough; it
   must correspond to the shipped build.
3. **Publish our modified-Suwayomi fork's source** (we *will* fork it for the on-device WebView-Cloudflare
   bridge, §8b) — AGPL §13 requires this for anything we ship *or* run network-facing.
4. **Keep all copyright/license notices + attribution** for Suwayomi, FlareSolverr (MIT — server/web only,
   never in the app), and each bundled Tachiyomi/Keiyoushi extension (audit their licenses/ToS).
5. **Dependency audit:** confirm every bundled component is AGPL-compatible (our stack — Tauri MIT/Apache,
   Svelte MIT, Rust crates MIT/Apache, our code — is permissive → combines into AGPL fine).

**Server side (AGPL §13 nuance):** komika-server calling *unmodified* Suwayomi's API over HTTP is
arm's-length aggregation (separate processes) and does not by itself force komika-server to be AGPL — but
we're AGPL-ing the whole project anyway, so this is moot. What §13 *does* bind: any **modified** Suwayomi
we run server-side must offer its Corresponding Source to network users (→ obligation 3).

**Distribution (this is where "no App Store" lands) — see §12a.**

## 12a. Distribution channels (decided: no Apple App Store)

"No App Store" only costs us on **iOS**; Android + desktop are frictionless and have FOSS-native homes.

| Platform | Channel | Friction |
|----------|---------|----------|
| **Android** | Direct APK + **F-Droid** (natural home for an AGPL app; the whole Mihon/Tachiyomi family lives there) | Low — sideload APK is normal on Android |
| **Desktop** | Direct download (`.dmg`/`.msi`/AppImage) + optionally Flatpak / Homebrew / winget | Low |
| **iOS** | **Self-sideload** (build-from-source with the user's own Apple ID) · **AltStore** (automates free-account 7-day re-signing) · **EU only:** alternative marketplaces / web distribution under the DMA (e.g. AltStore PAL) | **High** — inherent to AGPL-vs-App-Store |

**iOS dev/test vs public distribution (don't conflate):**
- **Dev/test is easy:** a paid **Apple Developer ($99/yr)** *development/ad-hoc* provisioning profile installs
  on up to **100 registered devices/yr** (~1-year builds) — perfect for us + a tester group. A **free**
  Apple ID also works but builds **expire after 7 days**.
- **Public distribution does NOT scale via dev profiles:** the 100-device cap + UDID registration + annual
  re-sign make them unusable as a real channel. **Do NOT use the Enterprise program ($299/yr)** to work
  around it — Apple forbids public distribution on it and will terminate it.
- **The open-source route sidesteps the cap:** because the source is public (AGPL), each iOS user signs
  *their own* build with *their own* Apple ID (directly or via AltStore), so our 100-device limit never
  applies and we never touch App Review (avoiding the AGPL × Apple-ToS conflict entirely).

**Consequence for the plan:** treat **Android (F-Droid) + desktop as primary, frictionless targets**;
iOS is a **best-effort sideload target**, developed/tested via a $99 dev profile and delivered to users via
self-build / AltStore / EU marketplaces. This is an accepted trade-off, not a gap to close.

---

## 13. Security & privacy

- Local Suwayomi's GraphQL admin API is **unauthenticated** (compose already binds it to loopback for
  this reason). On device: bind **127.0.0.1 only**, never `0.0.0.0`; prefer the **IPC-proxy transport**
  (§3.3b) so the port is never reachable from the webview/network at all.
- `fetch_image` SSRF guard already blocks loopback/LAN/metadata targets — keep it; ensure the *new* local
  Suwayomi image proxy path is exempted deliberately and narrowly (it's our own loopback), not by
  loosening the general guard.
- Extensions execute third-party scraper code on-device (same trust model as Mihon). Document it; keep
  the JVM sandboxed to the app data dir; no arbitrary FS/network beyond what the source needs.
- Never send the hosted **Bearer token** to the local engine or vice-versa.
- CSP stays tight (`connect-src 'self' ipc:` with the IPC-proxy transport).

---

## 14. Testing & verification (per layer)

1. **Server bridge:** unit + resolver tests for `workSources`/`workSourcesBatch` (auth, NSFW gate,
   ordering, extension coords present). `cargo test` green; `svelte-check` 0/0.
2. **Sidecar supervisor:** Rust integration test that spawns the JVM (behind a `#[ignore]` "needs java"
   gate like the existing live-Suwayomi test), asserts readiness, port, graceful shutdown, restart.
3. **LocalSuwayomiBackend:** contract tests against a booted sidecar (fixture source) for
   series/chapters/pages shapes.
4. **Composite routing:** unit tests proving each method hits the right backend and the fallback ladder
   fires (mock local as not-ready / erroring).
5. **Extension mgmt:** install/idempotency/failure-fallback tests.
6. **Progress/library sync:** write on device → read via hosted `session`/`library`/`myActivity`
   reflects it; offline queue drains on reconnect.
7. **Per-platform smoke:** the §3.5/§5/§6 acceptance checklists on real hardware.
8. **Parity:** a known series renders the same chapters/pages on web (server path) and native (local
   path) modulo source freshness.

---

## 15. Phase gate summary (ship order)

| Phase | Deliverable | Gate to next |
|-------|-------------|--------------|
| **0** | Server exposes `workSources` + `source_extension` | resolver tests green; web untouched |
| **L** | **Licensing — DECIDED: AGPL-3.0 open-source (§12), no App Store (§12a)** | ✅ done — set up `LICENSE` + notices alongside Phase 0 |
| **1** | Desktop sidecar + composite backend + content read on macOS | §3.5 acceptance |
| **2** | On-device extension provisioning + fallback ladder | §4 acceptance |
| **3** | Android (TachiManga-fork spike → bundle) | §5 acceptance on device |
| **4** | iOS (interpreter-JVM spike → bundle) + CF→server fallback | §6 acceptance on device |
| **∞** | Offline/downloads, storage UI, polish | — |

---

## 16. Open risks / decisions to make explicitly

- **AGPL (§12) — DECIDED (AGPL-3.0, open-source).** Residual work is compliance mechanics (publish
  Corresponding Source per release incl. the Suwayomi fork; CLA; dependency-compat audit), not a blocker.
- **iOS distribution (§12a) — DECIDED (no App Store).** Residual: sideload UX (AltStore/self-build) and
  the EU-marketplace option; Android/desktop unaffected.
- **iOS interpreter-JVM feasibility/perf.** De-risk with a spike before committing to Phase 4 scope.
- **Android ART vs JVM.** Spike TachiManga's fork; it may or may not be reusable under its license.
- **Cloudflare on-device.** Solved via the platform WebView interceptor (§8b), *not* FlareSolverr — but
  this requires a WebView↔engine bridge (stock Suwayomi expects FlareSolverr). De-risk by studying
  TachiManga's fork, which already does it; server-fetch remains the per-host safety net.
- **App size.** JRE per target is tens of MB; confirm acceptable, consider per-arch thinning.
- **Extension version drift** between server catalogue and device — mitigated by §2.1 version pinning +
  §4 sync, but needs monitoring.
- **Maintenance surface** grows (per-platform JVM packaging, extension repo changes). Budget for it.
```
