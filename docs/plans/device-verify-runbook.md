# Device-class verification runbook — native embedded Suwayomi (N-RUNBOOK)

Standing `could_not_verify` closeout for the native-embedded-Suwayomi stack. Everything
in this stack is proven headless (unit tests, the `e2e/native-read` harness against a live
v2.3.2243 engine, the CF-shim protocol/health tests). **Two things can only be checked on a
real machine with a display:**

- **Check A** — page bytes served by the *embedded* engine actually paint in the *real*
  Tauri desktop window (not the hosted fallback, not the shimmed-`invoke` harness).
- **Check B** — a *live* Cloudflare challenge solved in the Tauri WebView, with
  `cf_clearance` replayed through the engine against a real CF-gated Keiyoushi source.

This runbook executes both on **this machine** (macOS arm64, has a display) from zero prior
context. Repo root is `/Users/caved/dev/komika` throughout; every command notes its `cwd`.

Ground truth: `docs/plans/native-embedded-suwayomi.md` §0a (status ledger), §3.5 (Phase-1
acceptance), §8b (Cloudflare); `apps/reader/src-tauri/suwayomi/{N-CF-SPIKE-FINDINGS.md,
GQL-SCHEMA-FINDINGS.md}`; `apps/reader/src-tauri/src/{suwayomi.rs,cloudflare.rs}`.

---

## 0. Preconditions + asset bootstrap

### 0.1 Toolchain (verify present)

```bash
# cwd: anywhere
ls -d /opt/homebrew/opt/openjdk@21 && /opt/homebrew/opt/openjdk@21/bin/java -version   # JDK 21 (for jlink + dev java)
node -v            # >= 22.13
pnpm -v            # 11.9.0 (repo packageManager)
cargo --version    # for apps/server + tauri
which sqlite3      # optional, for DB spot-checks
```

Tauri desktop prerequisites (Xcode command-line tools + Rust) are assumed already installed
if `cargo` builds; if `tauri dev` later fails to compile the Rust core, install the Tauri
macOS prereqs first.

### 0.2 Embedded-engine assets (both gitignored)

The jar (174 MB) and the jlink JRE are **not** committed (`apps/reader/src-tauri/.gitignore`).
They already exist on this machine, but bootstrap is idempotent — re-run to be safe:

```bash
# cwd: /Users/caved/dev/komika/apps/reader/src-tauri
./scripts/fetch-suwayomi-jar.sh     # downloads + SHA-256-verifies Suwayomi-Server.jar (pin: v2.3.2243)
JAVA_HOME=/opt/homebrew/opt/openjdk@21 ./scripts/build-jre.sh   # jlink minimal JRE -> jre/aarch64-macos/
```

Verify (both must print a path, not "ABSENT"):

```bash
# cwd: /Users/caved/dev/komika
ls -l apps/reader/src-tauri/suwayomi/Suwayomi-Server.jar        # ~174 MB
ls -l apps/reader/src-tauri/jre/aarch64-macos/bin/java          # the launcher suwayomi.rs probes
shasum -a 256 apps/reader/src-tauri/suwayomi/Suwayomi-Server.jar
# expected: 821141b32e170d4a02d3cbdfed577ed8f07bd22383ff5f4132ebb5ae40e98dd5  (apps/reader/src-tauri/suwayomi/VERSION)
```

Pin details are in `apps/reader/src-tauri/suwayomi/VERSION` (version / url / sha256). The
`fetch` script fails loudly on any SHA mismatch and never keeps a bad jar.

### 0.3 JS deps

```bash
# cwd: /Users/caved/dev/komika
pnpm install        # workspace install (esbuild/sharp/workerd prebuilt bins, no build scripts)
```

### 0.4 Hosted server (REQUIRED for Check A; optional for Check B)

The reader is a **fat client but not standalone**: catalogue metadata, the `w_` canonical
work ids, `canonicalChapters`, and `workSources` all come from the hosted server
(`apps/server`, Rust/axum on `:8080`). The composite backend fetches the hosted canonical
chapter list, then reconciles it against the *embedded* engine's live chapters by number
(D7) and serves page bytes locally. So Check A needs a hosted server with **at least one
catalogued MangaDex work**.

The catalogue is populated by the opt-in direct-MangaDex sync (`CATALOGUE_SYNC=on`), which
seeds on startup then refreshes incrementally. Migrations (incl. `0005_canonical_works.sql`,
`0017_source_extension.sql`) run automatically at startup. Start it with GraphiQL +
introspection on so you can spot-check:

```bash
# cwd: /Users/caved/dev/komika/apps/server
PORT=8080 \
GRAPHIQL_ENABLED=on \
CATALOGUE_SYNC=on \
CORS_ORIGINS="http://localhost:5173,http://localhost:4173,http://tauri.localhost,tauri://localhost" \
cargo run
```

> The bundled `apps/server/komika.sqlite3` in this tree predates the canonical tables and has
> **no catalogued works**; `CATALOGUE_SYNC=on` populates them live from MangaDex (needs
> network egress, respects a ~5 req/s cap). Wait for the seed before Check A — poll until a
> `w_` work with a `mangadex` source mapping appears:

```bash
# cwd: anywhere — poll until canonicalUpdates returns a work id, then confirm its source mapping
curl -s http://localhost:8080/graphql -H 'content-type: application/json' \
  --data '{"query":"{ canonicalUpdates(page:1){ workId title } }"}' | head -c 800; echo

# take a workId (w_...) from above and confirm it exposes a MangaDex source:
curl -s http://localhost:8080/graphql -H 'content-type: application/json' \
  --data '{"query":"query($w:ID!){ workSources(workId:$w){ sourceType sourceId sourceKey extension{ pkgName repoUrl } } }","variables":{"w":"w_PUT_ID_HERE"}}' | head -c 800; echo
# PASS precondition: at least one row with sourceType:"mangadex" (extension is null for MangaDex — the client
# supplies eu.kanade.tachiyomi.extension.all.mangadex + the Keiyoushi index itself; see local-suwayomi-backend.ts).
```

### 0.5 Reader env — turn the native engine ON, keep the backend on the hosted API

`apps/reader/.env` currently sets `PUBLIC_KOMIKA_BACKEND_KIND=suwayomi`, which routes the
reader to a **direct** Suwayomi adapter and **bypasses the composite backend entirely**
(`apps/reader/src/lib/context.ts`). For Check A the reader must use the hosted (`komika`)
backend AND the native-engine flag. Set `apps/reader/.env` to:

```sh
PUBLIC_KOMIKA_BACKEND=on
PUBLIC_KOMIKA_BACKEND_KIND=komika
PUBLIC_KOMIKA_API=http://localhost:8080/graphql
PUBLIC_KOMIKA_NATIVE_ENGINE=on
PUBLIC_KOMIKA_IMG_MODE=direct
```

Why each matters (`apps/reader/src/lib/config.ts` + `context.ts`):
- `PUBLIC_KOMIKA_NATIVE_ENGINE=on` **and** running under Tauri → `createCompositeBackend(...)`
  with a real `LocalSuwayomiBackend`. Off → plain hosted backend, engine never consulted.
- `PUBLIC_KOMIKA_BACKEND_KIND` must **not** be `suwayomi` (that short-circuits the composite).
- Native image resolution ignores `IMG_MODE`: under Tauri `createImageProvider` always returns
  `NativeImageProvider`, which routes engine `/api/…` paths to `suwayomi_image` regardless.

---

## 1. Build / launch

**Dev (use this — it's the only mode that emits engine logs):**

```bash
# cwd: /Users/caved/dev/komika/apps/reader
pnpm tauri dev
```

`tauri dev` runs `beforeDevCommand: pnpm dev` (Vite on `:5173`, the `devUrl`), compiles the
Rust core, and opens the real "Komika" window (`tauri.conf.json`). The Suwayomi supervisor
starts automatically in `.setup()` (`lib.rs` → `suwayomi::start`); it is always spawned, and
the JS flag only gates *use*.

> **Logging caveat:** the `tauri_plugin_log` plugin is initialized **only under
> `cfg!(debug_assertions)`** (`lib.rs`). `tauri dev` is a debug build → logs flow. A release
> `tauri build` **compiles logging out**, so Checks A/B (which rely on reading `suwayomi`-target
> log lines) must be done against `tauri dev` or a debug build. Build the bundle only to
> confirm packaging:

```bash
# cwd: /Users/caved/dev/komika/apps/reader   (optional — produces src-tauri/target/release/bundle/macos/Komika.app)
pnpm tauri build
```

**Where engine logs go (dev):** the default `tauri_plugin_log` targets are **stdout of the
`tauri dev` terminal** (easiest) plus a file under the macOS log dir
`~/Library/Logs/app.komika.reader/` (identifier `app.komika.reader`). All sidecar lines use
the `suwayomi` log target. Tail live:

```bash
# cwd: anywhere — filter the dev terminal, or tail the file:
tail -F ~/Library/Logs/app.komika.reader/*.log | grep -i suwayomi
```

Engine data dir (server.conf, lockfile, extensions DB) is
`~/Library/Application Support/app.komika.reader/suwayomi/` (`app_data_dir()/suwayomi`).

---

## 2. Check A — engine-served page bytes render in the real window

**Goal:** prove the pixels in the reader came from the *embedded* engine over
`suwayomi_image` (IPC), not from the hosted fallback (`canonicalPages` → Worker/`fetch_image`).

### A.1 Confirm the engine reached `ready`

Watch the `tauri dev` terminal (or the tailed log). Real strings emitted by `suwayomi.rs`
(quote these exact substrings when confirming):

- `launching engine on 127.0.0.1:<port>`
- `engine ready (v2.3.2243) on port <port>`  ← the readiness gate passed (`aboutServer` returned)
- every JVM stdout line is re-logged under the `suwayomi` target

Independently confirm state via the IPC command from the app devtools console (Web
Inspector: right-click the window → Inspect Element, or ⌥⌘I):

```js
await window.__TAURI__.core.invoke('suwayomi_status')
// PASS: { state: "ready", version: "v2.3.2243", lastError: null }
await window.__TAURI__.core.invoke('suwayomi_base_url')
// PASS: "http://127.0.0.1:<port>"  (None/null until ready)
```

### A.2 Open a MangaDex-backed work and read

In the window: open the discovery/updates feed, pick a **MangaDex-catalogued** series (one
whose `workSources` returned a `mangadex` row in §0.4), open it, open a chapter, and let the
first pages load. First-open triggers on-device extension provisioning (N2.1): the client
auto-installs `eu.kanade.tachiyomi.extension.all.mangadex` from the Keiyoushi index — expect
a short delay and provisioning GraphQL round-trips on the first MangaDex read only.

> Some MangaDex titles return **0 EN chapters** (licensed — e.g. Solo Leveling, My Dress-Up
> Darling; see GQL-SCHEMA-FINDINGS §C). If a series shows an empty chapter list, pick another.

### A.3 Prove the bytes came from the engine (not the hosted fallback)

The signature is unambiguous: **engine page bytes flow through the `suwayomi_image` IPC
command over relative `/api/v1/manga/<id>/chapter/<engineChapterId>/page/<n>` paths**, which
*only* the embedded engine produces. The hosted fallback path never calls `suwayomi_image`.

Prove it three ways (any one is sufficient; do all three for a clean sign-off):

1. **DevTools Network — no HTTP page fetch.** With the engine path active, page `<img src>`
   is a `blob:` URL built from IPC bytes (`NativeImageProvider.engineToBlobUrl` →
   `URL.createObjectURL`). There is **no** outbound `https://…mangadex…` or Worker
   (`/img?src=`) request for the pages. Filter the Network tab: engine reads show as IPC, not
   as image GETs to a CDN. (Cover/among-fallback images may still be blobs too — focus on the
   reader page images.)

2. **DevTools Console — the reconciliation + fallback lines.** The composite logs `console.warn`
   only on *failure/fallback* (`composite-backend.ts`), so a **clean** engine read is
   **silent** on the console. If you see any of these, the engine path did **not** serve and
   it fell back — treat as a Check-A **fail** and troubleshoot:
   - `[composite] workSources fetch failed; leaving local memo intact:`
   - `[composite] local chapter reconciliation failed; marking work unusable on-device:`
   - `[composite] local canonicalPages failed, using hosted:`
   Confirm the positive path directly in the console:
   ```js
   // The pages the reader is showing should be engine proxy paths BEFORE the image provider turns them into blobs.
   // Inspect a rendered <img>; the resolvePage input (page.sourceUrl) is "/api/v1/manga/.../chapter/.../page/0".
   await window.__TAURI__.core.invoke('suwayomi_image', { path: '/api/v1/manga/3/chapter/1/page/0' })
   // PASS: returns an ArrayBuffer (raw JPEG bytes). A non-/api/ path is rejected by validate_image_path.
   ```

3. **Sidecar log — live engine activity during the read.** While paging, the `suwayomi`-target
   log shows the JVM serving `/api/v1/...` requests (the engine's own access lines, re-logged).
   No such activity = bytes did not come from the engine.

**Check A PASS** = engine `ready` (A.1) + a MangaDex work renders pages + the bytes are blobs
from `suwayomi_image` `/api/v1/...` paths with **no** CDN/Worker page fetch and **no**
`[composite] … fallback` warnings (A.3).

---

## 3. Check B — live Cloudflare solve in the Tauri WebView

**Goal:** a real CF challenge is solved in a Tauri WebView, `cf_clearance` is harvested and
replayed **through the stock engine** (no Suwayomi fork), and a CF-gated source's
browse/read succeeds. This is the plan's headline `could_not_verify` (§0a N-CF, §8b).

**How it's wired (no per-host call needed):** on each engine `Ready` transition the supervisor
POSTs `setSettings(flareSolverrEnabled:true, flareSolverrUrl:<loopback shim>,
flareSolverrAsResponseFallback:false)` (`cloudflare.rs::apply_settings`, called from
`suwayomi.rs`). Thereafter the stock engine's `CloudflareInterceptor` calls our loopback
FlareSolverr-v1 shim whenever it sees a `403/503 + Server: cloudflare` response; the shim
opens the challenge URL in a hidden Tauri WebView (`WebviewSolver`), polls
`cookies_for_url` for `cf_clearance`, and answers in FlareSolverr's JSON shape. The engine
then injects the cookie into its OkHttp jar and unifies its UA — all stock.

### B.1 Confirm the shim is up and wired

At launch the `suwayomi` log must show (exact strings from `cloudflare.rs`):

- `cloudflare shim listening on 127.0.0.1:<shimPort>`  ← `CfShim::start`
- `cloudflare shim wired into engine (http://127.0.0.1:<shimPort>)`  ← `apply_settings` confirmed
  `setSettings`. If instead you see `setSettings did not confirm flareSolverr: …` or
  `cloudflare shim not started: …`, the CF path is inert (CF sources will just server-fallback).

### B.2 Install a real CF-gated Keiyoushi source

**There is no in-app extension-management UI** — provisioning is catalogue-driven only
(`local-suwayomi-backend.ts`), and it only auto-installs sources that a `workSources` result
points at. A generic CF-gated source is not in the hosted catalogue, so install it manually
against the engine's loopback GraphQL. Get the port from A.1 (`suwayomi_base_url`), then
(exact mutations from GQL-SCHEMA-FINDINGS §B):

```bash
# cwd: anywhere — replace 4567 with the real ready port from suwayomi_base_url
PORT=<port>
GQL="http://127.0.0.1:$PORT/api/graphql"

# 1. add the Keiyoushi store (idempotent):
curl -s "$GQL" -H 'content-type: application/json' --data '{"query":"mutation($u:String!){addExtensionStore(input:{indexUrl:$u}){extensionStore{name indexUrl}}}","variables":{"u":"https://raw.githubusercontent.com/keiyoushi/extensions/repo/index.min.json"}}'; echo

# 2. refresh the available list from all stores:
curl -s "$GQL" -H 'content-type: application/json' --data '{"query":"mutation{fetchExtensions(input:{}){extensions{pkgName}}}"}' >/dev/null; echo done-fetch

# 3. install the CF-gated extension (id == pkgName):
curl -s "$GQL" -H 'content-type: application/json' --data '{"query":"mutation($id:String!){updateExtension(input:{id:$id,patch:{install:true}}){extension{pkgName isInstalled versionName}}}","variables":{"id":"eu.kanade.tachiyomi.extension.all.mangafire"}}'; echo
# PASS: { ... "isInstalled": true ... }

# 4. confirm the engine source id (verifies the source registered):
curl -s "$GQL" -H 'content-type: application/json' --data '{"query":"{sources(filter:{name:{includesInsensitive:\"mangafire\"}}){nodes{id name lang baseUrl}}}"}'; echo
```

**Recommended CF-gated targets** (verified present in the live Keiyoushi index today; CF-gating
is a *runtime* property of the site, not recorded in the index, so if one isn't currently
challenging, try the next):

| Source | pkgName | en source id (from index) | baseUrl |
|--------|---------|---------------------------|---------|
| **MangaFire** (primary) | `eu.kanade.tachiyomi.extension.all.mangafire` | `6084907896154116083` | https://mangafire.to |
| Weeb Central | `eu.kanade.tachiyomi.extension.en.weebcentral` | `2131019126180322627` | https://weebcentral.com |
| ManhuaUS | `eu.kanade.tachiyomi.extension.en.manhuaus` | `4005973248538140146` | https://manhuaus.com |

> The N-CF spike did **not** run a live solve (its own `could_not_verify`), so there is no
> "spike source" to reuse — these are fresh picks confirmed to exist in the current index.

### B.3 Trigger a browse that hits Cloudflare

Drive a source browse so the engine issues an HTTP request the CF-gated site answers with a
challenge (`fetchSourceManga`, POPULAR page 1 — the same call the client's `refFor`/resolve
path makes):

```bash
# cwd: anywhere — source = the en id from B.2 step 4 (e.g. MangaFire 6084907896154116083)
curl -s "$GQL" -H 'content-type: application/json' --data '{"query":"mutation($s:LongString!){fetchSourceManga(input:{source:$s,type:POPULAR,page:1}){hasNextPage mangas{id title url}}}","variables":{"s":"6084907896154116083"}}'; echo
```

### B.4 What you should see on screen + in logs

- If the site returns a CF challenge, the engine calls the shim, which builds a **hidden**
  challenge WebView with the fixed UA (`CHALLENGE_UA` in `cloudflare.rs`). For a pure-JS
  ("I'm Under Attack") challenge it solves **without any visible window**.
- If still unsolved after the **8 s** grace (`INTERACTIVE_GRACE`), the shim **shows** the
  window titled **"Verifying your connection…"** for you to complete an interactive
  Turnstile; it keeps polling and closes the window on success.
- Success signature: the `fetchSourceManga` call (B.3) **returns a non-empty `mangas` list**
  (the browse succeeded through the cleared cookie). Re-running B.3 should now be immediate —
  the engine persists `cf_clearance` per host in its cookie store, so it won't re-solve until
  the cookie is rejected.
- **Failure/fallback path (also a valid, expected outcome to observe):** if the WebView solve
  times out or the user dismisses an interactive Turnstile, the shim returns a non-2xx
  `solution.status` (`error_resp`), the engine throws `CloudflareBypassException`, and Komika
  falls back to server-fetch for that source (plan §7). `fetchSourceManga` then returns an
  error / empty — that proves the *fallback contract*, not the *solve*.

**Check B PASS** = shim wired (B.1) + a CF-gated browse (B.3) succeeds with a non-empty result
after the WebView solve (hidden or interactive), demonstrating `cf_clearance` was replayed
through the stock engine. Record which source and whether it was hidden-solvable or required
an interactive Turnstile (both are informative for §8b).

---

## 4. Expected-results checklist

| # | Observation | PASS |
|---|-------------|------|
| A1 | `suwayomi_status` → `state:"ready"`, `version:"v2.3.2243"` | ✅ / ❌ |
| A1 | Log shows `engine ready (v2.3.2243) on port <port>` | ✅ / ❌ |
| A2 | A MangaDex work opens with a non-empty chapter list | ✅ / ❌ |
| A3 | Reader page `<img>` are `blob:` URLs; no CDN/Worker page GET in Network | ✅ / ❌ |
| A3 | `suwayomi_image({path:'/api/v1/...'})` returns an ArrayBuffer | ✅ / ❌ |
| A3 | **No** `[composite] … fallback/unusable` console.warn during the read | ✅ / ❌ |
| B1 | Log: `cloudflare shim listening on 127.0.0.1:<port>` | ✅ / ❌ |
| B1 | Log: `cloudflare shim wired into engine (...)` | ✅ / ❌ |
| B2 | `updateExtension(install:true)` → `isInstalled:true` for the CF source | ✅ / ❌ |
| B3/B4 | CF-gated `fetchSourceManga` returns a non-empty `mangas` list | ✅ / ❌ |
| B4 | (record) hidden-solve vs interactive-Turnstile window shown | note |
| — | App exit leaves **no orphaned java** (`pgrep -fl Suwayomi` empty) | ✅ / ❌ |

---

## 5. Troubleshooting

| Symptom | Cause / Fix |
|---------|-------------|
| Engine never reaches `ready`; log shows a SIGTRAP / Chromium download | `kcefEnabled` trap. The supervisor writes `server.kcefEnabled = false` into `server.conf` every launch (`render_server_conf`); if a stale `bin/kcef` payload was left, delete the data dir: `rm -rf ~/Library/Application\ Support/app.komika.reader/suwayomi` and relaunch. Never set `kcefEnabled=true`. |
| `engine unavailable: no Suwayomi-Server.jar found` / `no java runtime found` | Assets missing. Re-run §0.2. In dev you can also override: `KOMIKA_SUWAYOMI_JAR=<abs>` and `KOMIKA_JAVA_BIN=<abs>` are read first by `resolve_jar`/`resolve_java`. |
| `another Komika instance holds the engine lock` (state stays degraded) | Stale `komika-suwayomi.lock` after a hard crash (the guard can't verify holder liveness). Remove it: `rm ~/Library/Application\ Support/app.komika.reader/suwayomi/komika-suwayomi.lock`, then relaunch. |
| Orphaned JVM after quit | Should not happen (SIGKILL on exit + `kill_on_drop`). Check `pgrep -fl Suwayomi`; kill leftovers: `pkill -f 'suwayomi.tachidesk.config.server.rootDir'`. Reproduces the harness teardown check (`e2e/native-read/run.sh`). |
| Reader shows content but engine is idle / no `suwayomi_image` calls | Composite bypassed. Verify `PUBLIC_KOMIKA_BACKEND_KIND=komika` (not `suwayomi`) and `PUBLIC_KOMIKA_NATIVE_ENGINE=on` in `apps/reader/.env` (§0.5), then restart `tauri dev` (Vite env is read at build). |
| `workSources` returns `[]` for every work | Catalogue not seeded. Ensure `apps/server` ran with `CATALOGUE_SYNC=on` and give the MangaDex seed time (§0.4); re-poll `canonicalUpdates`. |
| Reader can't reach the hosted API (network errors in console) | CORS / CSP. The reader webview origin must be in the server `CORS_ORIGINS` (dev origin `http://localhost:5173`; a bundled `.app` on macOS is `tauri://localhost`). The reader CSP `connect-src` already allows `http://localhost:8080` (`tauri.conf.json`) — keep the API on `:8080`. |
| Port conflict on `:8080` (server) or engine port | Server: set `PORT=<n>` and update `PUBLIC_KOMIKA_API`. Engine: the sidecar brokers an **ephemeral** loopback port itself (`broker_port`), so it never fixed-collides; a manual engine (harness) uses `SUWA_PORT`. |
| First MangaDex read hangs on "install" | On-device provisioning (N2.1) is installing the MangaDex extension from Keiyoushi; needs network egress. Confirm with `extensions(filter:{pkgName:{includesInsensitive:"mangadex"}})` against the engine port. A repo-down failure memoizes the work unusable and falls back to hosted (no hard-fail). |
| CF challenge window never appears AND browse fails immediately | Either the source didn't actually challenge (not CF-gated right now — try the next source in B.2) or `setSettings` didn't confirm (check B.1 for `setSettings did not confirm flareSolverr`). |
| CF window appears but solve never completes | Interactive Turnstile you must click, or a UA mismatch. UA unification is the #1 failure mode (N-CF-SPIKE §C): the WebView UA (`CHALLENGE_UA`) must equal the replayed UA — they're set from the same constant, so if CF still rejects, the site likely needs a real interactive solve or isn't crackable on-device (server-fetch fallback is the intended safety net). |
| Release `.app` produces no `suwayomi` logs | Expected — `tauri_plugin_log` is `cfg!(debug_assertions)`-only (`lib.rs`). Do Checks A/B under `pnpm tauri dev`. |
| jlink JRE rebuild fails with `Permission denied` copying into `target/…/jre` | Known jlink legal-notices gotcha (mode 444). `build-jre.sh` already `chmod -R u+w` the output; if you hand-built a JRE, run `chmod -R u+w apps/reader/src-tauri/jre/aarch64-macos` and re-run `tauri dev`. |

---

## 6. Fast headless sanity (optional pre-flight, no display)

Before the in-app checks, the proven harness re-confirms the composite→local→engine logic
end-to-end against the real jar (mirrors Check A minus the DOM paint):

```bash
# cwd: /Users/caved/dev/komika/apps/reader/src-tauri/e2e/native-read
bash run.sh     # boots the engine, drives the REAL composite+local backends, asserts A–E; expect "5 passed, 0 failed"
```

This is the automated coverage the two manual checks exist to top off (the literal DOM
`<img>` blob paint and the live CF WebView solve — the only pieces a headless run can't reach).
