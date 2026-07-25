# Native embedded-Suwayomi engine — status, 2026-07-27

**Scope.** Honest, evidence-backed state of the embedded Suwayomi engine and the native page-image
byte path on desktop, iOS (device + simulator), Android and web.

**How this was produced.** Six independent audit lanes (`desktop`, `ios`, `android`, `imagefetch`,
`testgap`, `cfshim`) produced **84 findings**. **30** of those — the subset that were candidates for
auto-fixing — went through adversarial refutation (**27 confirmed, 3 refuted**). **The other 54 are
single-pass, unverified audit output** and are labelled as such throughout. Four of six fix groups
completed; a build gate was then run by the orchestrator and is green. Findings in the
`missing-impl`, `test-gap` and `unverified-claim` categories were deliberately excluded from
auto-fixing because they are roadmap work, not code defects — they are still real findings.

---

## 1. Verdict up front

### Desktop (macOS / Linux / Windows) — **DEMO-READY**
The happy path is real and matches the docs: the engine binds loopback only
(`apps/reader/src-tauri/src/suwayomi.rs:233`), JS reaches it only over IPC, the jar SHA-256 is
genuinely verified (`scripts/fetch-suwayomi-jar.sh:53-101`), and the hosted Bearer token is never
forwarded to the local engine (`packages/api/src/composite-backend.ts:185-189`). What is missing is
everything around the happy path. Before this run **nothing built the engine artifacts** — a real
`tauri build` shipped an empty `jre/` and no jar (`tauri.conf.json:9`, now fixed). Still open:
no CI job runs `tauri build` at all (`.github/workflows/native-sidecar.yml:96-156`), no macOS
entitlements/hardened-runtime config for a bundled JVM (`tauri.macos.conf.json:3`), abnormal
termination orphans the JVM (`suwayomi.rs:272`), and Windows pops a console window per JVM spawn
(`suwayomi.rs:257`). This is a build you can demo from a dev checkout, not one you can ship.

### iOS — physical device — **PROTOTYPE**
`apps/reader/src-tauri/src/suwayomi_ios.rs` (843 lines, ~31 KB of unsafe JNI) is a careful,
well-commented first cut of an in-process Zero-JVM launcher, and the cfg gating is correct for every
real iOS triple. But it **has never been compiled by any CI job**, **has never run on hardware**, and
its 6 unit tests have never executed on any machine (`native-sidecar.yml:48-64` is a four-slug
desktop matrix). Three things the plan requires are simply absent: iOS lifecycle/jetsam handling
(`suwayomi_ios.rs:513`), any Cloudflare bypass (`cloudflare.rs` is `#[cfg(desktop)]`,
`lib.rs:16-17`; `suwayomi_ios.rs:293` hard-writes `flareSolverrEnabled = false`), and — before this
run — any post-ready liveness check. The link recipe that makes the build work at all lives only in
a gitignored, hand-edited `gen/apple/project.yml` (mitigated this run, see §5). **The five iOS fixes
applied this run were written with zero compiler feedback** (§6).

### iOS — simulator — **STUB**
`aarch64-apple-ios-sim` and `x86_64-apple-ios` both report `target_abi="sim"`, so `lib.rs:23-28`
routes them to `suwayomi_mobile.rs`, where `start()` only logs (`:69-71`), `suwayomi_status`
hard-codes `state:"degraded"` (`:84-91`), and `suwayomi_gql`/`suwayomi_image` unconditionally `Err`
(`:95-112`). The simulator has never exercised one line of the engine. `docs/plans/n4-ios-build-attempt.md:36-38`
claims "the full native src-tauri crate compiles for aarch64-apple-ios" but cites a
`--target aarch64-apple-ios-sim` check — which routes to the stub and never touched `suwayomi_ios.rs`.

### Android — **NOT STARTED** (engine); the app itself is a working hosted client
There is not one occurrence of `target_os = "android"`, `jniLibs`, `CookieManager` or a foreground
service anywhere under `apps/reader/src-tauri/`. No `tauri.android.conf.json`, no `gen/android`
(`tauri android init` has never been run), no Android JDK build script, no Android CI job. Zero of
work items N3.2–N3.9 exists. The `e2e/android-jdk-probe` "N3.1 PASS" proves the jar's *bytecode*
runs on an aarch64 JDK 21 — via `java -jar` in a glibc Docker container (`run.sh:34-35,128-135`),
i.e. the exact `execve`-a-launcher model the spike itself says API 29+ forbids.

### Web — **PRODUCTION-READY** (unchanged by this work)
`createImageProvider` returns `WebImageProvider` for any non-Tauri build
(`packages/api/src/image-provider.ts:241`), which is the only class that reads `workerBaseUrl`
(`:76`). The Cloudflare Worker (`img.komiq.cc`) remains the web image path and is untouched.

---

## 2. Claim-vs-reality ledger — `docs/plans/native-embedded-suwayomi.md` §0a

The `testgap` lane did this analysis; the table is its output, corrected where the code contradicts
the prose. **Correct the plan's §0a prose accordingly — the wording below is wrong as written, not
merely optimistic.**

| §0a claim | Verdict | Backing artifact |
|---|---|---|
| "reader svelte-check 344/0/0" | **PROVEN** (count stale — actual 401 files, 0 errors) | `ci.yml:56-58` runs `pnpm --filter @komika/reader check`. Bonus: an injected type error in `packages/api/src/offline-queue.ts` surfaced in the reader's svelte-check, so `packages/api` **is** transitively type-checked by CI. |
| "src-tauri 13/0" | **PARTIAL** (gate real, count stale: 20 pre-run, 27 now) | `native-sidecar.yml:119-120` |
| "server 112/0" | **UNPROVEN** | **none.** `ci.yml:111-139` runs `cargo fmt --check`, `cargo clippy`, `cargo build --release` — **no `cargo test`**. 375 `#[test]`/`#[tokio::test]` under `apps/server/src` are never executed by any workflow. |
| Gate C ✅ "cold-start <30 s (proven)" | **UNPROVEN** | The only boot artifact is the `#[ignore]`d `sidecar_boots_and_reports_about_server`, which passes a **60 s** budget (`suwayomi.rs:1191`) and **asserts nothing about elapsed time**. |
| Gate C ✅ "no orphaned java (proven)" | **PARTIAL** | Asserted only for the clean `stop_child` path. Abnormal exit orphans the JVM (`suwayomi.rs:272`); `kill_on_drop` is the only reaping mechanism. |
| Gate C ✅ "status states (proven)" | **PARTIAL** | Tests the pure `transition()`/`set_ready()` setters, not states driven by a real boot/crash. |
| Gate C ✅ "live MangaDex read — proven end-to-end" | **UNPROVEN — both ends are fakes** | Hosted is a mock (`e2e/native-read/harness.entry.ts:makeMockHosted`) **and** the entire Tauri IPC layer is a Node `fetch` shim (`e2e/native-read/tauri-core-stub.mjs:7`) that hard-codes `suwayomi_status → {state:'ready'}` and implements `suwayomi_image` as a plain `fetch(BASE+path)`. **Zero lines of Rust execute.** `validate_image_path`, the 32 MiB cap, the concurrency semaphore and `tauri::ipc::Response` are all unexercised. Assertion E can degrade to `[SKIP]` (`harness.entry.ts:283-285`), so "5/5" can really be 4 asserts + a skip. |
| Gate C 🟡 "kill→degraded+restart … unit-covered accounting" | **UNPROVEN (the "unit-covered" half is false)** | No test referenced `MAX_RESTARTS_PER_MIN`, `storm` or the backoff; the whole supervision loop had zero coverage. (Partly changed this run — see §5.) |
| N2.1 provisioning "curl+harness-verified" | **UNPROVEN** | **none** — no deterministic artifact; exercised only inside network+jar-dependent harnesses. The workflow's own TODO admits it (`native-sidecar.yml:135`). |
| N2.2 fallback ladder "13-assertion check" | **PROVEN AS LOGIC, not in CI** | `e2e/fallback-ladder/run-fallback-check.sh` — executed on this host: 13 PASS, exit 0, <1 s, no engine/network. Runs in **no workflow**. |
| N2.3 offline queue "19-assertion check" | **PROVEN AS LOGIC, not in CI; count wrong (22)** | `e2e/offline-queue/run-offline-check.sh` — executed here: 22 PASS, exit 0. Runs in no workflow. |
| N-CF "7 tests" | **PROVEN** | `cloudflare.rs:617-801` holds exactly 7, run by `native-sidecar.yml:119-120` on macOS/Linux/Windows. |
| N-CF "live `setSettings` acceptance against v2.3.2243" | **UNPROVEN** | **none** — a one-off manual run, no recorded artifact, no test, even though `native-sidecar.yml` already boots a real jar it could assert against. |
| "N3.1 headless acceptance PASS (11/11)" | **PARTIAL / mislabelled** | `e2e/android-jdk-probe/run.sh` is a genuine self-asserting harness, but it needs Docker + the gitignored 174 MB jar + live network, is in no workflow, and `skip()` (`run.sh:50,74-80`) `exit 0`s when Docker is absent. Two of N3.1's three acceptance clauses were never satisfied: no jlink'd arm64 JDK artifact, and **0 GraalJS class-loads observed** (`FINDINGS.md`), which directly contradicts `n3-android-spike.md` §2's "Proves … GraalJS init". |
| "the `native-sidecar.yml` CI matrix" gates this surface | **PARTIAL** | It is a four-slug **desktop** matrix (`native-sidecar.yml:47-62`). No `--target` flag appears anywhere in `.github/workflows/`. |
| "the Tauri bundle wiring (`bundle.resources` + build-jre.sh + fetch-suwayomi-jar.sh)" is landed | **WAS UNPROVEN — fixed this run** | `beforeBuildCommand` was only `pnpm build`; nothing ran the artifact scripts and both dirs are gitignored, so a bundle copied `jre/.gitkeep`. Now chained (`tauri.conf.json:9`). Still no CI job runs `tauri build`. |
| §0a mentions N4.2 / `suwayomi_ios.rs` | **DOES NOT EXIST IN §0a** | `grep -n "N4"` in the plan returns only lines 85 and 95, both about the N4 *spike*. Committed device-only iOS code has **no recorded status** in the plan of record. |
| "Remaining after Wave E spikes: execute the runbook on a display" | **UNDERSTATED** | Every `could_not_verify` item lives only in Markdown prose (`native-embedded-suwayomi.md:70,94`, `n4.1-ios-jvm-findings.md:310`, `device-verify-runbook.md:3,253,310`, `N-CF-SPIKE-FINDINGS.md:272`); nothing executable tracks or expires them. |

### Specific traps, stated plainly

1. **The Gate-C "live MangaDex read" proof executes no Rust.** Both the hosted backend and the whole
   Tauri IPC layer are mocked; `tauri-core-stub.mjs` returns `{state:'ready'}` as a literal. It
   proves the *TypeScript* composite/reconciliation logic, nothing about the native byte path.
2. **"cold-start <30 s (proven)" is not proven.** The test's budget is 60 s and it makes no timing
   assertion at all.
3. **`ci.yml` runs no `cargo test` for the server.** 375 server tests never execute in CI, so the
   "server 112/0" gate is asserted, not run.

---

## 3. The image-fetch byte path, per platform

**This is the crux.** Two separate questions, answered separately for every platform:

| Platform | Depends on the **Cloudflare Worker** (`img.komiq.cc`)? | Depends on the **hosted `api.komiq.cc` proxy**? |
|---|---|---|
| Desktop (Tauri) | **NO** — structurally unreachable | **YES** — for every Suwayomi-sourced cover and page |
| iOS device | **NO** — structurally unreachable | **YES** |
| iOS simulator | **NO** — structurally unreachable | **YES** (engine stub always `Err`s) |
| Android | **NO** — structurally unreachable | **YES** (engine stub always `Err`s) |
| Web | **YES** — by design; Worker stays for web only | YES |

### Why "no Worker" is structural, not configuration

`createImageProvider` returns `NativeImageProvider` purely on `isTauri()`
(`packages/api/src/image-provider.ts:241`). `NativeImageProvider` never reads `config.workerBaseUrl`;
only `WebImageProvider` does (`:76`), and `createImageProvider` never constructs it under Tauri. The
reader is a pure client-side SPA in the Tauri build (`apps/reader/src/routes/+layout.ts:11-12`,
`ssr = false; prerender = false`), so no Worker URL is ever baked into shipped native HTML. As of
this run the native CSP no longer even allow-lists `img.komiq.cc` (`tauri.conf.json:25`).
**There is no code path from a native build to `img.komiq.cc`. That goal is met.**

### Why "no server" is *not* met

The traced hops, native, today:

```
<img src={blobUrl}>
  └─ NativeImageProvider.resolvePage / resolveCover   (image-provider.ts:213-225)
       ├─ path starts with "/api/"       → invoke('suwayomi_image')  → loopback engine   [UNREACHABLE, see below]
       ├─ path starts with "/covers/" or "/avatars/"  → apiOrigin + path → invoke('fetch_image')
       ├─ host is a MangaDex host        → invoke('fetch_image')     → *.mangadex.network directly
       └─ anything else                  → resolveViaLocalProxy()    → invoke('fetch_image')
             └─ Rust fetch_image (lib.rs:89-136): semaphore permit, manual redirect loop,
                per-hop validate_image_url, .resolve() IP pin, fixed Referer/UA, read_capped
```

The middle branch is the problem. In production the server hands out **absolute**
`https://api.komiq.cc/api/v1/manga/{id}/thumbnail` and
`https://api.komiq.cc/api/v1/manga/{m}/chapter/{c}/page/{p}` URLs — `abs()` prefixes
`image_base_url` (= `SUWAYOMI_PUBLIC_URL`, `https://api.komiq.cc` in prod) onto every Suwayomi
cover/page path (`apps/server/src/suwayomi.rs:344-350`), served publicly at
`apps/server/src/main.rs:584-620`. Those are absolute URLs, so they fall through to `fetch_image`
and are fetched **from our origin**. Every Suwayomi-sourced cover and page on desktop, iOS and
Android still comes from `api.komiq.cc`.

Three independent reasons the embedded-engine byte path never executes in a shipped build:

1. **The flag is off.** `apps/reader/.env:19` is `PUBLIC_KOMIKA_NATIVE_ENGINE=off`, and
   `.env.production` (verified: it lists `PUBLIC_KOMIKA_BACKEND`, `PUBLIC_KOMIKA_API`,
   `PUBLIC_KOMIKA_IMG_MODE`, `PUBLIC_KOMIKA_IMG_WORKER` …) **does not override it**. That is the
   env `beforeBuildCommand: "pnpm build"` loads.
2. **Even with the flag on, `pages()` never consults the engine.**
   `CompositeBackend.pages()` is unconditionally hosted (`composite-backend.ts:244-248`); only
   `canonicalPages()` (`:420-430`) consults the engine, and only for D7-reconciled chapters of
   MangaDex-spine works.
3. **On Android and the iOS simulator the engine command always fails** — `suwayomi_image` returns
   `Err` unconditionally (`suwayomi_mobile.rs:104-112`) and `suwayomi_status` reports `degraded`, so
   `CompositeBackend.localReady()` is false by construction.

Non-MangaDex direct fetching is additionally untested against reality: `fetch_image` has no channel
for per-source `Referer`/`User-Agent`/`Cookie` (`lib.rs:58`), so hotlink-protected sources will 403.
Today that is masked purely because hosted `pages()` returns `api.komiq.cc` proxy URLs.

**Summary: "No Worker" is done. "No server" is not.** Reaching it requires (a) flipping the native
flag per platform once the engine is device-verified, (b) making `pages()` consult the engine the way
`canonicalPages` does, (c) an explicit `origin: 'engine' | 'cdn' | 'hosted'` tag on `Page` instead
of the `startsWith('/api/')` discriminator, and (d) a per-source header channel on `fetch_image`.

---

## 4. Findings — all 84

**Legend.**
`V` = adversarially verified (**one** refuter, not a panel) · `U` = **unverified single-pass audit
output** — plausible, cited, but never challenged.
Disposition: **FIXED** this run · **FIXED (partial)** · **OPEN** · **REFUTED** (with the refuter's
reason) · **DEFERRED-TO-ROADMAP** (`missing-impl` / `test-gap` / `unverified-claim` — deliberately
excluded from auto-fixing because they are roadmap work, not code defects).

Counts: **24 FIXED**, **1 FIXED (partial, incomplete)**, **3 REFUTED**, **56 OPEN or
DEFERRED-TO-ROADMAP**. Of the 84, **30 were adversarially verified**; **54 were not**.

### 4.1 Desktop (18)

| Sev | Finding | `file:line` | Failure scenario | ? | Disposition |
|---|---|---|---|---|---|
| BLOCKER | Nothing builds/bundles the engine artifacts | `apps/reader/src-tauri/tauri.conf.json:9` | Fresh clone → `pnpm desktop:build` → installer builds green → `resolve_config` fails at launch → engine permanently Degraded → the entire Phase-1 feature is absent from the shipped product with no build-time error. | V | **FIXED** — `beforeBuildCommand` now chains `fetch-suwayomi-jar.sh` + `build-jre.sh` |
| HIGH | No macOS entitlements / hardened-runtime / signing for the bundled JVM | `apps/reader/src-tauri/tauri.macos.conf.json:3` | Sign + notarize → notarization rejects the nested unsigned JRE binaries, or the hardened runtime kills `spawn_engine` when HotSpot maps W+X memory → engine boot fails for every macOS user, 30 s stall per retry, forever. | U | **OPEN** — needs a macOS host |
| HIGH | Stale lockfile after any abnormal exit permanently disables the engine | `apps/reader/src-tauri/src/suwayomi.rs:539` | SIGKILL / power loss / WebView crash leaves `komika-suwayomi.lock` → every subsequent launch reports "another Komika instance holds the engine lock" until the user manually deletes a file in an OS-specific app-data dir. | V | **FIXED** — heartbeat lock + stale takeover |
| HIGH | Orphaned `java` survives SIGKILL/logout/crash | `apps/reader/src-tauri/src/suwayomi.rs:272` | Force-quit/logout → the 512 MB-heap JVM keeps running and holds its port + H2 dir; combined with the lockfile the next launch can neither start an engine nor see the orphan. Repeated crashes stack orphans. | U | **OPEN** — needs `PR_SET_PDEATHSIG` / Job Object / kqueue, all per-OS |
| HIGH | Every app quit blocks the UI thread 6 s when the engine never started | `apps/reader/src-tauri/src/lib.rs:272` | Quit a build without the bundled engine → window disappears, process hangs 6 s on the main thread; OS shows beachball/"not responding"; a relaunch in that window hits the still-held lockfile. | V | **FIXED** — `ENGINE_STARTED` short-circuit (residual: a *failed* start still burns the poll; that path lives in `suwayomi.rs`) |
| HIGH | Embedded engine runs with no authentication | `apps/reader/src-tauri/src/suwayomi.rs:231` | Any unprivileged local process scans loopback for an `aboutServer` response, then POSTs `updateExtensionRepos`/`installExternalExtension` at an attacker-hosted jar → arbitrary code inside the Komika-launched JVM with the user's privileges and full app-data access. | V | **FIXED (partial)** — HTTP Basic auth with a per-boot token; the mutation-allowlist half was deliberately skipped (§5) |
| HIGH | Native reader downloads every page of a chapter before showing page 1 | `apps/reader/src/lib/data/source.ts:1833` | A 40-page chapter awaits ~40 engine/proxy downloads (7 sequential batches of 6) before the first byte is painted — tens of seconds of blank screen. | V | **OPEN** — fix agent correctly declined: needs `image-provider.ts` + the read route, outside its owned files (§5) |
| MED | Readiness gate never observes child exit | `apps/reader/src-tauri/src/suwayomi.rs:339` | A JRE missing a class Suwayomi needs → JVM exits in ~1 s → supervisor waits the full 30 s, reports a timeout, sleeps 2 s, repeats forever, with no exit-status diagnostic and the storm cap never engaging. | V | **FIXED** — `select!` on `child.wait()`; boot failures now surface in ~1 s with the real `ExitStatus` |
| MED | Shutdown SIGKILLs the JVM with no SIGTERM | `apps/reader/src-tauri/src/suwayomi.rs:360` | JVM killed mid-write → next launch H2 must recover or refuses with "Database may be already in use" → boot fails, visible only as a 30 s readiness timeout. | V | **FIXED** — SIGTERM-then-KILL ladder on unix; Windows unchanged |
| MED | JVM + CF listener start for every desktop user regardless of the flag | `apps/reader/src-tauri/src/lib.rs:255` | A shipped build with the flag off still spawns a `-Xmx512m` JVM every launch, writes an H2 dir, holds a lockfile and binds a second loopback listener for a subsystem no code path uses. | V | **FIXED** — `engine_autostart_enabled()` + CAS-guarded `start_engine_once()` + a new `suwayomi_start` IPC command |
| MED | Release builds install no logger | `apps/reader/src-tauri/src/lib.rs:241` | A production engine stuck Degraded reports only `lastError: "engine did not become ready within 30s"`; the JVM's actual stack trace was read from the pipe and dropped. Undiagnosable without a debug build. | V | **FIXED** — `tauri_plugin_log` unconditional, rotating file target, Info in release |
| MED | The bundled JRE is unpinned and unverified | `apps/reader/src-tauri/scripts/build-jre.sh:18` | A runner or laptop with `JAVA_HOME` on JDK 17/24 produces a JRE whose module set differs from the one `MODULES` was validated against; the bundle boots on the builder's machine and nothing detects the drift. | V | **FIXED** — hard JDK-21 feature assertion + sorted-tree SHA-256 manifest + opt-in `KOMIKA_JRE_MANIFEST_SHA256` enforcement |
| MED | `KOMIKA_JAVA_BIN`/`KOMIKA_SUWAYOMI_JAR` honoured in release; artifacts never re-verified | `apps/reader/src-tauri/src/suwayomi.rs:485` | Claimed: a per-user install or an env-setting process points `KOMIKA_JAVA_BIN` at arbitrary code. | V | **REFUTED** — code claims verified, threat model isn't: setting the app's env or writing its resource dir already requires code execution as that user (same-principal), and the per-user-install and system-install halves of the scenario are mutually exclusive. Moot at a deeper layer anyway: the app *by design* downloads and loads unsigned third-party extension jars into that JVM (`local-suwayomi-backend.ts:289-297`). The suggested fix is a live regression risk — the env override is the only working escape hatch today (`device-verify-runbook.md:373`, `native-sidecar.yml:130-132`). Residual worth doing: log a loud WARN when an override is honoured in release. |
| MED | Deterministic native checks + chapter-list smoke never run in CI | `.github/workflows/native-sidecar.yml:134` | A refactor of composite-backend's D7 reconciliation or the fallback ladder breaks native reads; every gate stays green because those checks are run by hand. | U | **DEFERRED-TO-ROADMAP** |
| MED | Spawning `java.exe` from a GUI app pops a Windows console window | `apps/reader/src-tauri/src/suwayomi.rs:257` | On Windows a black console window appears beside the app window on launch and on every supervised restart, staying for the JVM's lifetime. | U | **OPEN** — one-line `CREATE_NO_WINDOW` / `javaw.exe`, unverified |
| LOW | Brokered port released before the JVM binds it (TOCTOU) | `apps/reader/src-tauri/src/suwayomi.rs:220` | Another process grabs the ephemeral port between broker and bind → the JVM fails to bind and exits, but the supervisor polls a stranger's HTTP service for 30 s and reports a generic timeout. | V | **FIXED** — `poll_ready` now distinguishes "nothing there yet" from "a stranger owns this port" and names the collision |
| LOW | Desktop CSP still allowed `img.komiq.cc` + dev localhost origins | `apps/reader/src-tauri/tauri.conf.json:25` | No functional failure; the CSP stopped documenting the real dependency set and kept a path open for a regression that reintroduces Worker URLs on desktop without any gate noticing. | V | **FIXED** — Worker and dev origins removed from the shipped CSP; dev origins moved to a new `devCsp` |
| LOW | LAN/self-hosted Suwayomi shows no images: `fetch_image` blocks private addresses | `apps/reader/src-tauri/src/lib.rs:170` | A desktop user pointed at their own LAN Suwayomi gets every page and cover rejected with "host resolves to a non-public address" — a fully blank reader, error visible only in a debug build. | U | **FIXED (partial)** — the new single-origin exemption covers it, but only once the compile-time env is forwarded to Rust (§5 remainder 3) |

### 4.2 iOS (15)

| Sev | Finding | `file:line` | Failure scenario | ? | Disposition |
|---|---|---|---|---|---|
| BLOCKER | `suwayomi_ios.rs` is compiled by no CI job; its unit tests never run anywhere | `.github/workflows/native-sidecar.yml:48` | Any refactor of lib.rs's shared surface (renaming `crate::image_fetch_semaphore`, changing `tauri::ipc::Response`, bumping tauri/jni-sys) compiles green everywhere while silently breaking the iOS device build — discovered only when someone with a Mac + iPhone attempts a release build. A regression in the iOS copy of the `validate_image_path` SSRF guard would ship unnoticed. | U | **DEFERRED-TO-ROADMAP** — and see §7: the recommended fix must be a **macOS** runner, not the Linux one the audit proposed |
| HIGH | `run_jvm` exits the VM-owning thread while still attached to the JVM | `apps/reader/src-tauri/src/suwayomi_ios.rs:427` | A bundle regression drops the jar from the classpath → `FindClass` returns null → `run_jvm` returns Err → the attached thread exits → the live VM's next safepoint/GC stack-scan walks the freed stack → SIGSEGV in a JVM thread, i.e. an app crash instead of a clean `degraded` + hosted fallback. | V | **FIXED** — single cleanup epilogue calling `DetachCurrentThread` on every error path, body wrapped in `catch_unwind` |
| HIGH | No Cloudflare bypass exists on iOS | `apps/reader/src-tauri/src/suwayomi_ios.rs:293` | A CF-gated source (a large fraction of Keiyoushi extensions) → the engine's `CloudflareInterceptor` has no `flareSolverrUrl` and errors → `CompositeBackend` permanently memoizes the work in `localUnusable` for the session → the device silently reverts to server-fetched content, which is exactly the dependency the native lane exists to remove. | U | **DEFERRED-TO-ROADMAP** (`missing-impl`) |
| HIGH | The entire iOS link recipe lives in a gitignored, hand-edited `gen/apple/project.yml` | `apps/reader/src-tauri/scripts/stage-ios-jvm-runtime.sh:60` | Someone runs `tauri ios init` to regenerate `gen/apple`. The app still links, but the dlsym-only `Java_*`/`JNI_OnLoad_*` members of libjava/libnio/libzip are dropped, so `JNI_CreateJavaVM` fails during java.base bootstrap **on device** with no build-time error — an unexplained permanent `degraded`. | V | **FIXED** — the script now asserts all 13 `-force_load` entries, `DEAD_CODE_STRIPPING: false`, `-lz`/`-liconv`/`-lc++` and both frameworks, and can **print the whole recipe back** so a wiped `project.yml` is recoverable from the repo alone |
| HIGH | Zero iOS lifecycle/jetsam handling | `apps/reader/src-tauri/src/suwayomi_ios.rs:513` | User backgrounds mid-chapter with `suwayomi_image` in flight; iOS suspends the process; reqwest holds 60 s timeouts against a frozen loopback server. On foreground the sockets are dead, IPC promises reject, and — because status is still `ready` — the composite retries and memoizes works `localUnusable`. Under jetsam the user sees a silent cold restart with no diagnostic. | U | **DEFERRED-TO-ROADMAP** (`missing-impl`) — the only shutdown hook is a `RunEvent` iOS never delivers |
| MED | `-Xss1m` contradicts the module's own Zero-stack reasoning | `apps/reader/src-tauri/src/suwayomi_ios.rs:379` | First MangaDex extension install on device: dex2jar / the ASM class-writer recursion runs on a Javalin worker with a 1 MiB Zero stack and throws `StackOverflowError`. The engine reports `ready` but never serves a chapter. | V (refuter notes the SOE itself is plausible-but-unmeasured — source-text + HotSpot/Zero reasoning, no execution possible here) | **FIXED** — `-Xss4m` with the reasoning documented |
| MED | No `-Djava.io.tmpdir` / `-Duser.home` pinned | `apps/reader/src-tauri/src/suwayomi_ios.rs:371` | The extension installer calls `File.createTempFile` with `java.io.tmpdir=/tmp` → `IOException: Read-only file system` → extension provisioning fails permanently → every work memoized `localUnusable` while the engine reports `ready`. | V | **FIXED** — `-Djava.io.tmpdir=<data_dir>/tmp` (created by `prepare_data_dir`) and `-Duser.home=<app-data>` |
| MED | No liveness check after Ready | `apps/reader/src-tauri/src/suwayomi_ios.rs:592` | The engine OOMs mid-session under `-Xmx256m`; every `suwayomi_gql` returns connection-refused; the composite memoizes each work `localUnusable`, so the whole session silently degrades to hosted while `suwayomi_status` still reports `ready` with `lastError: null`. | V | **FIXED** — 30 s `aboutServer` heartbeat, degrade after 3 consecutive misses |
| MED | iOS bundle contents/size unverified; the 218 MB desktop-JRE leak has no in-repo guard | `apps/reader/src-tauri/tauri.ios.conf.json:1` | Someone regenerates `gen/apple` on a machine whose `jre/` was populated by `build-jre.sh`; the iOS copy phase re-includes `jre/aarch64-macos` (macOS mach-o), producing an oversized `.ipa` that either fails codesign or ships 62 MB of unusable macOS executables — a failure already observed once. | V | **FIXED (partial, incomplete)** — `stage-ios-jvm-runtime.sh` now hard-fails on a staged `gen/apple/assets/jre` and enforces a 200 MB runtime / 400 MB payload ceiling, **but** `tauri.ios.conf.json` is unchanged and no post-`tauri ios build` `.app` verifier exists. The orchestrator recorded this finding as UNADDRESSED when `fix:packaging` died; the script guards partially cover it |
| MED | §0a never mentions N4.2; committed device-only code has no recorded status | `docs/plans/native-embedded-suwayomi.md:95` | A reviewer reads §0a and concludes iOS has no engine (or reads the commit message and concludes it works), then either duplicates the work or enables the native flag for an iOS build whose engine has never been observed to reach `ready` on hardware. | U | **DEFERRED-TO-ROADMAP** — *this document is the interim record* |
| MED | Only `-Xmx` capped; metaspace/direct buffers/code cache unbounded | `apps/reader/src-tauri/src/suwayomi_ios.rs:378` | Claimed: a 6-page prefetch burst plus extension class loading crosses the foreground jetsam ceiling on a 4 GB iPhone. | V | **REFUTED** — direct memory defaults to `Runtime.maxMemory()`, i.e. the `-Xmx256m` already present; the VM is an interpreter-only Zero build (`build.rs:54`, `scripts/build-ios-jvm.sh:3,181`) so there is no JIT code cache to grow; per-thread stacks *are* bounded by the `-Xss` that is present. The itemization sums to ~800 MB against a ~2 GB foreground limit, and the ~149 MB jimage is a file-backed mmap that `phys_footprint` excludes. The proposed `-XX:MaxMetaspaceSize=128m` is a live regression risk against Kotlin + per-extension classloaders. Residual: metaspace alone is uncapped — a tuning nit. |
| LOW | Mac Catalyst (`target_abi="macabi"`) resolves to the device JNI module | `apps/reader/src-tauri/src/lib.rs:20` | Someone adds a Catalyst build: cargo pulls the in-process JNI module and `build.rs` feeds it iOS-device `.a` files → mach-o platform-mismatch link failure, or worse, links and aborts at `JNI_CreateJavaVM`, instead of falling back to the stub. | V | **FIXED** — explicit allowlist `all(target_os = "ios", target_abi = "")`. **Sequencing note: `build.rs:47-50` still gates on `abi != "sim"` and must be changed to match, or Catalyst gets the stub module with device `.a` files.** |
| LOW | `rust-version = 1.77.2` but `lib.rs` uses `cfg(target_abi)` (stable in 1.78) | `apps/reader/src-tauri/Cargo.toml:9` | A contributor or CI image pinned to the declared MSRV gets a hard `E0658` on `lib.rs:20` for **every** target, desktop included — not a silently-false predicate. | V | **FIXED** — bumped to `1.78` with the rationale in-file |
| LOW | Engine boot prologue runs on the iOS main thread | `apps/reader/src-tauri/src/suwayomi_ios.rs:546` | On a cold launch where the ephemeral port is repeatedly stolen, the main thread blocks ~600 ms plus filesystem latency during app init, delaying first paint and eating into the `UIApplication` launch-watchdog budget. | V | **FIXED** — `boot()` and the readiness gate moved inside the `suwayomi-jvm` thread; `start()` is now a pure spawn |
| LOW | A panic in the JVM thread closure records no degrade | `apps/reader/src-tauri/src/suwayomi_ios.rs:576` | Claimed: status sits at `starting` for the full 300 s budget with the real cause only on the device console. | V | **REFUTED** — mechanically true but no reachable trigger and no behavioural delta. The `CString::new(...).expect` is fed only by filesystem-derived paths (no interior NUL possible); the JNI fn-pointer `.unwrap()`s are only reached after `JNI_CreateJavaVM` returned `JNI_OK` with non-null `vm`/`env`, and a genuinely mis-staged runtime either fails `check_bundled_runtime()`, returns `rc != JNI_OK`, or hits HotSpot's `os::abort()` (which `catch_unwind` cannot intercept). And the harm is fictional: the only consumer is `LocalSuwayomiBackend.isReady()` (`local-suwayomi-backend.ts:113-122`), which maps `starting`/`degraded`/`stopped` identically to `false`, so JS falls back immediately, not after 300 s; `lastError` is read by **zero** call sites. *(The refuter flagged a genuinely different gap in the same closure: `run_jvm`'s `Ok(())` return — reached when `DestroyJavaVM` returns after all non-daemon engine threads exit — also performs no state transition, so the supervisor can sit at `ready` with a dead VM and a stale port. Not currently filed as its own finding.)* |

### 4.3 Android (10)

| Sev | Finding | `file:line` | Failure scenario | ? | Disposition |
|---|---|---|---|---|---|
| BLOCKER | Android has no embedded engine at all | `apps/reader/src-tauri/src/suwayomi_mobile.rs:69` | Install any Android build: `start()` logs and returns; `isReady()` sees `degraded`; every chapter list and page URL comes from the hosted MangaDex mirror. A work whose only source mapping is a non-MangaDex Keiyoushi source has no `canonicalPages` path at all and the reader shows an empty chapter. | U (but *confirmed by direct code reading* — the stub is unambiguous) | **DEFERRED-TO-ROADMAP** (`missing-impl`) — needs N3.2 (`dlopen` a bundled `libjvm.so`; the iOS static-link trick does not work on Android), N3.3 (Kotlin foreground service), N3.4 (`network_security_config` for 127.0.0.1), and an Android SDK/NDK host |
| BLOCKER | No Android JVM artifact, no jar bundling, no `gen/android` | `apps/reader/src-tauri/tauri.conf.json:31` | `tauri android build` produces an APK with no `libjvm.so` in `jniLibs/arm64-v8a` and no `Suwayomi-Server.jar` in assets, so even after N3.2 lands the launcher fails at `dlopen`/classpath resolution. With no ABI filter Gradle also emits an armeabi-v7a slice for which no JVM was ever built. | U | **DEFERRED-TO-ROADMAP** (`packaging`, needs an Android SDK/NDK host) |
| HIGH | `android-jdk-probe` validates the one execution model Android forbids | `apps/reader/src-tauri/e2e/android-jdk-probe/run.sh:128` | A reviewer reads "N3.1 headless acceptance PASS (11/11)" and green-lights N3.2 believing the runtime is de-risked; the first real Android attempt then hits an entirely unexercised class of failures — bionic/`libjvm.so` linkage (R8), foreground-service lifetime (R9), 16 KB page alignment. **All remaining Android risk is 100% live despite a green probe.** | U | **DEFERRED-TO-ROADMAP** (`unverified-claim`) — retitle the probe and add an explicit "does NOT cover" list |
| HIGH | §0a records "N3.1 headless acceptance PASS" although 2 of its 3 acceptance clauses were never met | `docs/plans/native-embedded-suwayomi.md:91` | Planning proceeds to N3.2 assuming N3.1 is closed; the arm64 JDK footprint (the dominant term in the ~120–180 MB APK estimate) is still unknown, and the first JS-heavy source hits an untested GraalJS/Truffle init — spike risk R4, which the probe explicitly did not retire (`FINDINGS.md`: "0 `com.oracle.truffle`/`org.graalvm.polyglot` class-loads here"). | U | **DEFERRED-TO-ROADMAP** — downgrade §0a/§4a to "N3.1 PARTIAL" |
| HIGH | No Cloudflare capability on Android | `apps/reader/src-tauri/src/lib.rs:16` | A source behind a managed challenge returns 403/503 to `fetch_image`; `lib.rs:130-132` turns it into `Err("upstream returned 403 Forbidden")`, `Cover.svelte` sets `broken`, and the user sees a permanent grey placeholder. No solve path, no cookie replay, and on native no Worker fallback. Tauri 2.11.5 documents `cookies()` as "**Android**: Unsupported, always returns an empty Vec" (`webview/mod.rs:2167-2169`), so a Kotlin plugin over `android.webkit.CookieManager` is mandatory. | U | **DEFERRED-TO-ROADMAP** (`missing-impl`) |
| MED | `suwayomi_ios.rs` has no shared abstraction — Android would duplicate ~600 lines verbatim | `apps/reader/src-tauri/src/suwayomi_ios.rs:125` | N3.2 is implemented as a copy of `suwayomi_ios.rs`. A later fix to shared logic (the `acquire_boot_port` TOCTOU handling, or the `validate_image_path` traversal guard at `:658-703`) is applied to one file and silently missed in the other — a **security-relevant divergence in the image-path validator across two shipping platforms**. | U | **DEFERRED-TO-ROADMAP** — extract a `suwayomi_core.rs` **before** N3.2 |
| MED | The `android-jdk-probe` is in no workflow | `.github/workflows/native-sidecar.yml:47` | `suwayomi/VERSION` is bumped past v2.3.2243, the new build switches the default store to sqlite-jdbc or bumps GraalJS; CI stays green, the Android premises silently invert (N3.5 stops being moot, a native `.so` becomes mandatory), and nobody learns until an Android build is attempted months later. | U | **DEFERRED-TO-ROADMAP** (`test-gap`) |
| MED | `fetch_image`'s SSRF guard makes every self-hosted/LAN backend unloadable on Android | `apps/reader/src-tauri/src/lib.rs:159` | Point an Android build at `http://192.168.1.20:8080`: catalogue metadata loads (GraphQL goes through `fetch`, not the guard) but `resolveCover('/covers/…')` → `Err("host resolves to a non-public address: 192.168.1.20")` → every cover is a grey placeholder and every reader page fails, with the reason visible only in native logs. | U | **FIXED (partial)** — same single-origin exemption as the desktop/imagefetch variant; conditional on the compile-time env being forwarded to Rust (§5 remainder 3) |
| LOW | Native CSP still allow-listed the Worker and two dev origins | `apps/reader/src-tauri/tauri.conf.json:25` | A hostile or buggy app on the same Android device binds `127.0.0.1:8080`; injected or compromised frontend script can then reach it and exfiltrate to it within the CSP. No exploit chain claimed — unnecessary attack surface shipped in the production native CSP. | U | **FIXED** (incidental — same `tauri.conf.json:25` edit as the desktop CSP finding) |
| LOW | `isReady()` issues an IPC round-trip per content call | `packages/api/src/local-suwayomi-backend.ts:113` | Opening a 37-page chapter on Android: `canonicalChapters` + `canonicalPages` each pay an IPC round-trip whose result is statically known to be `false`, adding latency to the first paint of every chapter open. | V | **OPEN** — `fix:api-pkg` never ran; `local-suwayomi-backend.ts` is untouched |

### 4.4 Cross-platform — the image byte path (12)

| Sev | Finding | `file:line` | Failure scenario | ? | Disposition |
|---|---|---|---|---|---|
| BLOCKER | A single failed page image collapses the entire chapter to "No pages available" | `apps/reader/src/lib/data/source.ts:1833` | A 40-page chapter on desktop/iOS; page 17's CDN returns 502 (or the hosted proxy answers 502 from `serve_suwayomi_image`, `apps/server/src/main.rs:612-616`). `Promise.all` rejects, `live()` catches and returns `emptyReader`. The user sees "No pages available" for a chapter where 39/40 pages were fine, and refreshing re-runs the same all-or-nothing fetch. | V | **FIXED** — `resolvePageUrls()` uses `Promise.allSettled`, mapping each rejection to `''` after a `console.warn` |
| HIGH | Native reader downloads every page of a chapter into memory before first paint | `apps/reader/src/lib/data/source.ts:1833` | A 200-page webtoon at ~1.5 MB/page on an iPhone: ~300 MB of Blob-backed bytes plus 200 IPC transfers must complete before page 1 paints, in the same process as a `-Xmx256m` Zero JVM → jetsam kill. On desktop, tens of seconds of blank reader. | V | **OPEN** — deliberately skipped (§5) |
| HIGH | `fetch_image`'s SSRF guard blocks the app's own API origin | `apps/reader/src-tauri/src/lib.rs:159` | `pnpm desktop:dev` → every cover calls `fetch_image("http://localhost:8080/covers/x.webp")` → `Err("host resolves to a non-public address: 127.0.0.1")` → grey placeholders across the whole app. A self-hoster at `http://192.168.1.20:8080` gets the identical blackout in the shipped app. | V | **FIXED** — exactly-one-origin exemption (§5) |
| HIGH | Native CSP omits the API origin from `img-src` | `apps/reader/src-tauri/tauri.conf.json:25` | A signed-in user opens any comment thread: every avatar and attached comment image is refused by the renderer ("violates Content Security Policy"), falling back to initials/broken tiles. Web is unaffected — that is a different CSP. | V | **FIXED** — `img-src` now includes `https://api.komiq.cc`; `devCsp` adds `http://localhost:8080` |
| HIGH | `fetch_image` has no channel for per-source `Referer`/`User-Agent`/cookies | `apps/reader/src-tauri/src/lib.rs:58` | An Android build (engine stub) reading from a hotlink-protected scanlation source: pages are absolute CDN URLs, `fetch_image` sends `Referer: https://<cdn-host>/` and a Komika UA, the CDN 403s, and the chapter renders empty. Masked today only because hosted `pages()` returns `api.komiq.cc` proxy URLs — **the direct native path is untested against real sources.** | U | **DEFERRED-TO-ROADMAP** (`missing-impl`) |
| HIGH | Native builds still fetch all Suwayomi-sourced covers and pages from the hosted proxy | `packages/api/src/image-provider.ts:222` | Install the shipped desktop/Android app and read any Suwayomi-sourced series with the server offline (or self-host on a laptop that sleeps): every cover and page 502s or times out, because the "native" client proxies its bytes through our origin exactly like the web build. The engine-backed `/api/…` → `suwayomi_image` path proven in Gate C never executes. | U | **DEFERRED-TO-ROADMAP** (`missing-impl`) — **this is the headline gap against the user's goal**; see §3 |
| MED | `fetch_image` accepts any Content-Type and produces a MIME-less Blob | `apps/reader/src-tauri/src/lib.rs:130` | A source CDN returns a 200 HTML interstitial (rate-limit/captcha) instead of an image. Native buffers up to 32 MiB of HTML, hands it to the DOM as a typeless blob, `<img>` fails to decode, and the user sees a generic broken tile with no diagnostic — while the identical request on web would surface a clean 502 "Upstream is not an image" (`apps/worker/src/index.ts`). | U | **OPEN** |
| MED | In-flight native cover fetches that settle after unmount leak their object URL | `apps/reader/src/lib/components/Cover.svelte:112` | Scroll-fling a discovery grid on iOS: dozens of `Cover`s mount, start a `fetch_image`, and unmount before the semaphore-queued fetch returns. Each late settle leaks one full-cover blob. Minutes of browsing pin tens/hundreds of MB in a jetsam-limited process that also hosts a 256 MB-heap JVM. | V | **FIXED** — the `!alive` branch now calls `images.release(u)` |
| MED | Pinning to `addrs[0]` defeats Happy Eyeballs | `apps/reader/src-tauri/src/lib.rs:164` | An iPhone on a carrier with a broken IPv6 prefix: every `fetch_image` pins the AAAA record, hangs 30 s and errors — covers all grey, every chapter "No pages available" — while Safari on the same phone loads the identical CDN fine. | U | **OPEN** — fix is `resolve_to_addrs(&host, &addrs)` with the full validated list, which keeps the rebinding defence |
| MED | No retry/backoff on the native byte path, and nothing logs the hosted-proxy fallback | `apps/reader/src-tauri/src/lib.rs:109` | The product ships "native, no proxy" and silently regresses: a bad `.env.production`, a degraded engine, or a hosted-only source sends 100% of image traffic back through `api.komiq.cc` and neither the user nor the team sees anything. Separately, one transient DNS blip on one page becomes an empty chapter. | V | **FIXED** — one retry per hop on transient failures with jittered backoff, a whole-call 60 s budget (which also *lowers* the previous 180 s worst case), and a `log::info!(target: "images", route=…, host=…)` line per new route |
| MED | Zero automated coverage of `NativeImageProvider`'s branch selection | `packages/api/src/image-provider.ts:213` | Someone changes the hosted cover URL shape (e.g. drops `SUWAYOMI_PUBLIC_URL` so paths become relative `/api/v1/manga/…`) and native silently starts routing hosted ids into the embedded engine via `isEnginePath` — every cover 404s from the local engine and no test fails. | U | **DEFERRED-TO-ROADMAP** (`test-gap`) — duplicate of `testgap-native-image-path-has-zero-ts-coverage` |
| LOW | IPv6-literal image URLs never reach the SSRF guard, and the test asserting otherwise passes for the wrong reason | `apps/reader/src-tauri/src/lib.rs:148` | A source whose page URLs use a bare IPv6 literal is unfetchable on native with the misleading error "dns lookup failed", because the bracketed `host_str` is fed to DNS. Separately, a future regression in the `::a.b.c.d`/v4-mapped folding at `lib.rs:191-206` would go undetected because no test exercises the guard through `validate_image_url` for a v6 literal. | U | **OPEN** |

### 4.5 Cross-platform — CI and test gaps (13)

Every finding in this lane is **DEFERRED-TO-ROADMAP** by construction (`test-gap` / `unverified-claim`
were excluded from auto-fixing), and every one is **unverified single-pass audit output** — though the
lane executed several of its own claims on this host, which is noted per row.

| Sev | Finding | `file:line` | Failure scenario | Disposition |
|---|---|---|---|---|
| BLOCKER | `suwayomi_ios.rs` (843 lines of unsafe JNI) + its 6 unit tests are compiled by zero CI jobs | `.github/workflows/native-sidecar.yml:115` | Any refactor of a shared helper (renaming `image_fetch_semaphore()`, a `jni-sys` signature change on a dep bump) silently breaks `suwayomi_ios.rs`. All four CI legs stay green, `cargo test --lib` stays green, and the breakage surfaces only when someone with a Mac runs `tauri ios build` — i.e. potentially never. | **DEFERRED** — **and the lane's own proposed fix is wrong**: see §7, this must be a `macos-latest` job |
| HIGH | Gate C's ✅ "live MangaDex read proven end-to-end" executes zero lines of Rust | `apps/reader/src-tauri/e2e/native-read/tauri-core-stub.mjs:7` | `validate_image_path` rejects a path shape the engine actually emits (e.g. a query string on `/api/v1/manga/3/chapter/1/page/0?…`), or the 6-permit semaphore deadlocks under the reader's `Promise.all`-every-page prefetch. The harness passes 5/5 because it never calls the Rust command; the app shows a blank chapter on device. | **DEFERRED** |
| HIGH | The two fully-offline Wave-D checks run in no workflow despite being CI-ready today | `.github/workflows/native-sidecar.yml:18` | A refactor of `CompositeBackend.canonicalChapters` drops the `localUnusable` memo, so every series open re-attempts extension provisioning against a dead repo. svelte-check still passes (it is type-correct), `native-sidecar` does not run on a `packages/api` change, and the regression ships — though a 1-second check in the repo would have caught it. *(Both were executed on this host: 13 PASS and 22 PASS, exit 0, <1 s each.)* | **DEFERRED** |
| HIGH | `ci.yml` has no `cargo test` for `apps/server` | `.github/workflows/ci.yml:137` | A change to the `workSources`/`load_work_sources` resolver breaks its unit tests. `cargo clippy` and `cargo build --release` stay green, CI merges, and the native client's engine-id resolution silently regresses — the exact hosted contract §0a says is covered by those 112 tests. **375 server tests never run in CI.** | **DEFERRED** — one `cargo test --all-targets` step; toolchain and rust-cache are already warm |
| HIGH | §0a's 🟡 "unit-covered accounting" for kill→degraded+restart is false | `apps/reader/src-tauri/src/suwayomi.rs:443` | An off-by-one or mis-ordered `restarts.clear()` makes the storm branch unreachable, so a crash-looping JVM is respawned every 2 s forever — draining battery and pegging CPU — with no test anywhere that would notice. | **DEFERRED** — partially mitigated this run: boot failures now return in ~1 s so the existing 60 s storm cap can actually trip, and `backoff_grows_and_stays_bounded` was added, but the window accounting is still not extracted into a pure testable function |
| HIGH | `image-provider.ts` — the headline native-image goal — has no automated coverage of any kind | `packages/api/src/image-provider.ts:240` | A change to the `/api/` prefix test sends an engine-relative path to `fetch_image` instead of `suwayomi_image`; the SSRF guard rejects the relative URL, every page in every chapter fails, and nothing in the repo fails first — the reader just renders "No pages available" on device. *(grep confirms no test or harness anywhere references `NativeImageProvider`/`createImageProvider`.)* | **DEFERRED** |
| MED | `suwayomi_mobile.rs` (the Android + iOS-simulator stub) is also compiled by no CI job | `apps/reader/src-tauri/src/suwayomi_mobile.rs:1` | A new argument is added to the desktop `suwayomi_image` command and the stub is not updated. Desktop CI is green on all four legs; the first `tauri android build` or simulator build fails to compile `invoke_handler!`, blocking the Android lane at exactly the moment someone starts it. | **DEFERRED** |
| MED | The CI sidecar boot smoke has two silent-pass paths | `.github/workflows/native-sidecar.yml:132` | The step is edited so a path var resolves empty; the `#[ignore]`d test `return`s early, reports green, and the boot gate silently stops testing anything for months. | **DEFERRED** — make the skips `panic!` under `CI`, and use `--exact` so a rename fails the step instead of matching nothing |
| MED | Gate C's "✅ cold-start <30 s (proven)" is backed by a 60 s budget and no timing assertion | `apps/reader/src-tauri/src/suwayomi.rs:869` (now `:1191`) | A jar bump or jlink module-set change pushes cold start from 4 s to 45 s. CI stays green, §0a keeps claiming <30 s, users see a 45-second dead engine on every launch with status stuck at `starting`. | **DEFERRED** |
| MED | N2.1 "curl+harness-verified" extension provisioning has no repeatable check | `.github/workflows/native-sidecar.yml:135` | The dedupe map keying changes so two concurrent series opens both trigger `installExtension` and the engine 409s the second. Nothing in CI installs an extension, so this only surfaces as an intermittent "no chapters" on a real device. The workflow's own TODO admits the gap. | **DEFERRED** |
| MED | N-CF's "live `setSettings` acceptance" is a one-off manual run with no artifact | `docs/plans/native-embedded-suwayomi.md:68` | A future jar bump renames or removes the `flareSolverrUrl` settings field. The 7 shim tests stay green, the boot smoke stays green, and the CF bypass silently stops being wired to the engine — discovered only when a gated source starts returning challenge HTML to users. | **DEFERRED** — costs nothing: the `#[ignore]`d boot test already has a live engine to assert against |
| LOW | Every `could_not_verify` item lives only in Markdown | `docs/plans/native-embedded-suwayomi.md:94` | Six months on, §0a still says "execute the runbook on a display". Nobody can tell from CI whether that is still true, which items were quietly discharged by a manual run, or which have been invalidated by a jar bump — so the release decision is made on stale prose. | **DEFERRED** — convert each to an `#[ignore = "needs a physical device: see device-verify-runbook.md §N"]` test and print the ignored roster into `$GITHUB_STEP_SUMMARY` |
| LOW | §0a's headline gate counts are stale in three places | `docs/plans/native-embedded-suwayomi.md:17` | Someone deletes half the src-tauri tests during a refactor. The count drops from 20 to 10 and still reads as "better than the documented 13", so the loss is invisible in review and in §0a. | **DEFERRED** |

### 4.6 Cloudflare shim — desktop-only implementation, absent on mobile (16)

All 16 are **unverified single-pass audit output** and all are **OPEN** / **DEFERRED-TO-ROADMAP** —
no fix group owned `cloudflare.rs`. The lane pulled the authoritative engine source for the pin
(`Suwayomi-Server` v2.3.2243 `CloudflareInterceptor.kt`) and diffed it against the shim, so the
protocol claims below are grounded in the real contract even though no refuter reviewed them.

**What the audit confirms is correct:** the command set (`request.get`/`request.post` only — the
engine never calls `sessions.*`, so the shim ignoring `session`/`session_ttl_minutes` is *not* a
divergence), the envelope keys, the `awaitSuccess()` HTTP-200 requirement, the `"not detected"`
sentinel, and the non-2xx→`CloudflareBypassException` fallback contract. Also positive: the
`komika-cf-N` challenge windows get **no** Tauri capabilities (`capabilities/default.json` scopes to
`windows: ["main"]`), so an untrusted challenge origin has no IPC reach to `fetch_image`/`suwayomi_gql`;
and the 7 shim tests **do** run in CI on all three desktop OSes, unlike the iOS module.

| Sev | Finding | `file:line` | Failure scenario | Disposition |
|---|---|---|---|---|
| HIGH | Solver returns the already-rejected `cf_clearance` from the shared WebView jar | `apps/reader/src-tauri/src/cloudflare.rs:489` | User solves a challenge Monday; Tuesday the ISP rotates their IP, invalidating the clearance. The engine 403s and calls `/v1`; the shim finds the stale `cf_clearance` on poll #1 and returns it as a fresh solve; the engine replays and 403s again; repeat. The user sees a spinner while the app hammers the source in a hot loop and never opens a challenge window. | **OPEN** |
| HIGH | Leading dot stripped from cookie domains → engine marks `cf_clearance` host-only | `apps/reader/src-tauri/src/cloudflare.rs:520` | Source gates `example.org` but serves pages from `img1.example.org`. The shim returns `domain: "example.org"`; the engine's `if (!cookie.domain.startsWith('.')) it.hostOnlyDomain(...)` stores it host-only; the page GET to `img1.example.org` carries no clearance, 403s, re-enters the interceptor, re-solves, and again produces a host-only cookie. Every distinct image host costs a full solve. *(Root cause proven in-registry: `cookie-0.18.1/src/lib.rs:777-785` — `domain()` strips the leading dot, so `to_fs_cookies` can never emit one.)* | **OPEN** |
| HIGH | 45 s solve cap and 8 s interactive grace exceed the 30 s GraphQL transport timeout | `apps/reader/src-tauri/src/cloudflare.rs:51` | CF issues an interactive Turnstile. The window is hidden 8 s, shown, the user takes 15 s to click, clearance lands at ~28–40 s. Past 30 s the IPC call has already failed (`suwayomi.rs:35` `HTTP_TIMEOUT`), the work is memoed `localUnusable` (`composite-backend.ts:404`) and every page is served hosted (`:429`) — **while the engine sits holding a perfectly good `cf_clearance` nobody will use until restart.** | **OPEN** — the timeout ladder must be made monotonic: shim wall cap < engine `callTimeout` < transport timeout |
| HIGH | Loopback `/v1` has no token: a local cookie oracle | `apps/reader/src-tauri/src/cloudflare.rs:318` | Any unprivileged local process (or a malicious npm/pip postinstall) scans loopback for the `komika-cf-shim` banner, POSTs `{"cmd":"request.get","url":"https://<source-with-user-login>/"}` and receives that site's **full cookie set, including session cookies**, in `solution.cookies`. Repeat per target host. Separately it can POST an attacker URL and, 8 s later, display a credential-phishing page in a window that looks like Komiq's. | **OPEN** — **highest-severity unfixed security finding.** Fix is cheap: a 128-bit token as a path segment (`flareSolverrUrl = http://127.0.0.1:<port>/<token>`, since the engine appends `/v1`), constant-time compared; plus restricting solvable URLs to hosts with an installed source, and filtering `to_fs_cookies` to `cf_clearance` + inbound seeds |
| HIGH | Unbounded, untimed body read on the serving thread | `apps/reader/src-tauri/src/cloudflare.rs:341` | A local process sends `POST /v1` with `Content-Length: 10000000000` and trickles a byte a minute. The serving thread parks in `read_to_string` forever: every real solve queues behind it (CF bypass silently dead), memory grows toward 10 GB, and at quit `Drop → shutdown() → join()` never returns, so Komiq hangs on exit and must be force-killed. A merely-buggy peer reproduces the hang without malice. *(Confirmed against `tiny_http` 0.12 in-registry: the body is read on the consumer's thread; `unblock()` cannot interrupt a thread parked in `read_to_string`.)* | **OPEN** |
| HIGH | Hardcoded spoofed Chrome/124 macOS UA on a WebKit/WebView2 engine | `apps/reader/src-tauri/src/cloudflare.rs:61` | On Linux the WebKitGTK challenge window presents `Chrome/124` with no `Sec-CH-UA`. CF's managed challenge flags the inconsistency and either never issues `cf_clearance` (45 s timeout → hosted-only) or issues one that fails on replay. Meanwhile the single attempt has already flipped the **whole engine's** UA to a Chrome version two years stale, for every source. | **OPEN** |
| HIGH | iOS and Android have no CF bypass at all | `apps/reader/src-tauri/src/lib.rs:16` | On an iPhone with the native engine on, the first CF-gated response throws `IOException("Cloudflare bypass currently disabled")`; the app quietly serves every page from `api.komiq.cc` and nobody — user or telemetry — can tell the embedded engine was bypassed. If the hosted server is down or the user is offline, the read fails outright with no explanation. **Permanent, per CF-gated source.** | **DEFERRED-TO-ROADMAP** (`missing-impl`) — duplicate of `ios-cloudflare-unbuilt` + `android-no-cloudflare-bridge`; interim ask is at minimum a log/telemetry event whenever `localUnusable` is set, so the hosted-fallback rate is measurable |
| MED | `setSettings` is fire-and-forget after Ready | `apps/reader/src-tauri/src/suwayomi.rs:408` | The reader restores the last-read chapter on launch. The engine flips Ready, the reader immediately issues `chapters(ref)` for a CF-gated source, and the interceptor throws because the spawned `setSettings` has not round-tripped yet. That work is `localUnusable` for the whole session even though the shim is wired 40 ms later. | **OPEN** — await `apply_settings` **before** transitioning to Ready |
| MED | A cookie with no domain serializes as `"domain": null`, which the engine's non-nullable Kotlin field rejects | `apps/reader/src-tauri/src/cloudflare.rs:73` | On Linux the challenge page sets an analytics cookie whose soup record has no domain (`wry` webkitgtk/mod.rs:938 sets a domain only `if let Some(...)`); the shim emits `"domain": null`; the engine's decoder throws; a *successful* solve becomes an `IOException` and the work is hosted-only. | **OPEN** |
| MED | The solve blocks Tauri's shared async-runtime workers | `apps/reader/src-tauri/src/cloudflare.rs:467` | On a 2-core laptop a solve begins: `recv_timeout` parks worker 1 for up to 10 s and the 400 ms cookie polls park worker 2 repeatedly. Concurrent `suwayomi_gql`/`fetch_image` futures cannot be polled, so covers stop loading and the reader freezes for the solve's duration even for unrelated non-CF content. The module docs still describe a `block_on` design that no longer exists. | **OPEN** |
| MED | No cap on concurrent solves | `apps/reader/src-tauri/src/cloudflare.rs:351` | A local process fires 200 concurrent `/v1` requests with distinct URLs; the shim builds 200 hidden WebViews, each rendering a remote page and polling cookies every 400 ms, until the app OOMs or the window server refuses. A friendly caller reproduces this on a chapter whose pages span many CF-gated hosts. | **OPEN** |
| MED | Inbound seed cookies are discarded | `apps/reader/src-tauri/src/cloudflare.rs:413` | A user logged into a members-only source: the engine forwards its `session=` cookie as a seed, the shim navigates a cookie-less WebView, which is redirected to `/login` instead of the challenge page, so `cf_clearance` never appears. The shim burns the full 45 s and the work is hosted-only. The in-file comment records a limitation that no longer holds. | **OPEN** |
| MED | The "protocol verified" suite never exercises the real `/v1` socket nor the engine's cookie contract | `apps/reader/src-tauri/src/cloudflare.rs:712` | Any of the dot-stripping, null-domain or unbounded-body findings above is introduced or persists with a fully green `cargo test --lib` on macOS, Linux and Windows, because no test in the suite can observe them. | **DEFERRED-TO-ROADMAP** (`test-gap`) |
| LOW | The loopback listener opens for every desktop user even when the native-engine flag is off | `apps/reader/src-tauri/src/suwayomi.rs:594` | A user who never enabled the native engine still exposes `komika-cf-shim` on loopback as a cookie oracle / window-spoofing primitive, for a feature they are not using. | **OPEN** — partially mitigated: `engine_autostart_enabled()` now gates `suwayomi::start()`, and the shim starts from inside it, so a flag-off build no longer opens the listener *by autostart*. Not independently re-verified |
| LOW | `notify_waiters()` only cancels solves already parked in the select | `apps/reader/src-tauri/src/cloudflare.rs:277` | The user quits at the moment the engine dispatches a solve; the task misses the notification and keeps a hidden WebView polling for 45 s while the app tears down, delaying exit or leaving a stranded window. | **OPEN** — needs a level-triggered `watch`/`CancellationToken` |
| LOW | `request.post` is answered by a GET navigation; `postData` is parsed then discarded | `apps/reader/src-tauri/src/cloudflare.rs:97` | A source whose search/page-list endpoint is POST-only sits behind CF. The engine sends `request.post`; the shim GETs the URL, receives a 405 page with no challenge, polls fruitlessly for 45 s and errors — the source is permanently hosted-only on desktop too. | **OPEN** |

---

## 5. What was fixed this run

Four of six fix groups completed. Every group was forbidden from running cargo; each verified its
file parses via `rustfmt` and (where possible) compiled pure-logic helpers standalone. The gate
(§7) then compiled and tested everything except the iOS module.

### `fix:ios-jni` — `apps/reader/src-tauri/src/suwayomi_ios.rs` (5 applied, 0 skipped)
- **Detach leak.** The whole post-`JNI_CreateJavaVM` body was extracted verbatim into
  `unsafe fn invoke_main(env) -> Result<(), String>` (all five early returns unchanged), called
  inside `catch_unwind(AssertUnwindSafe(...))`, with every `Err` path funnelling through one
  epilogue that calls `DetachCurrentThread` before returning. The VM-owning pthread never exits
  while attached. The success path still falls through to `DestroyJavaVM`.
- **Sandbox-writable dirs.** `-Djava.io.tmpdir=<data_dir>/tmp` (created by `prepare_data_dir`) and
  `-Duser.home=<app-data>` so neither falls back to the unwritable `/tmp` or `/var/mobile`.
  Option construction was split into a testable `jvm_options(jar, data_dir)`.
- **`-Xss1m` → `-Xss4m`**, documented next to the existing 16 MiB `JVM_THREAD_STACK` with the
  Zero-per-frame reasoning and a note that peak thread count is an N4.3 measurement input.
- **Post-ready heartbeat.** A 30 s `{ aboutServer { version } }` POST (10 s per-request timeout)
  started right after `set_ready`; exits immediately if state leaves Ready; after 3 consecutive
  misses (~90 s) calls `degrade("engine stopped responding")` so JS sees a terminal state instead
  of a permanently-lying `ready`.
- **Off-main boot.** `boot()` and the readiness gate moved inside the `suwayomi-jvm` thread;
  `start()` (which Tauri calls on the iOS main thread) now only checks managed state and spawns.
- **Deliberate non-changes:** no `DeleteLocalRef`/`PushLocalFrame` was added — `jni-sys` 0.3.1 is an
  iOS-only dep and is not vendored on this host, so the field names could not be verified, and
  `invoke_main` creates no refs the pre-existing code did not. `-Xmx256m` left alone: raising the
  heap on a jetsam-limited device is an N4.3 measurement decision, and the heartbeat now makes an
  OOM observable instead of silent.

### `fix:lib-rs` — `apps/reader/src-tauri/src/lib.rs` (6 applied, 0 skipped)
- **SSRF exemption — the security-relevant one, stated precisely.** `validate_image_url` now skips
  **only** the `is_blocked_ip` address check when the target's host+port is an **exact,
  case-insensitive match for the one configured API origin**. Not a range, not a subnet, not a
  suffix match, not a wildcard, and **there is no IPC command that lets the webview set it** — the
  origin comes from `KOMIKA_API_ORIGIN` (process env) or the compile-time `PUBLIC_KOMIKA_API`.
  Scheme allowlist, DNS resolution, per-hop re-validation and the `.resolve()` IP pin are all
  untouched. **When neither var is set, behaviour is byte-identical to before — nothing is exempt.**
- **Retry + route telemetry.** One retry per hop on transport errors or transient statuses
  (5xx/429/408) with linear jittered backoff, all attempts bounded by a new whole-call 60 s
  deadline (which *lowers* the previous 180 s worst case for a held permit); plus
  `log::info!(target: "images", route=hosted|cdn host=…)` on first sighting of each route.
- **Release logging.** `tauri_plugin_log` registered unconditionally — Debug in dev, Info in
  release, Stdout + `LogDir{file_name:"komika"}`, `KeepSome(3)`, 2 MiB max — so the supervisor's
  stdout/stderr pumps land on disk in production.
- **Flag-gated autostart.** `engine_autostart_enabled()` (runtime `KOMIKA_NATIVE_ENGINE`, else
  compile-time `PUBLIC_KOMIKA_NATIVE_ENGINE`, else the historical `true` so no existing build loses
  its engine), a CAS-guarded `start_engine_once()`, and a new `suwayomi_start` IPC command for lazy
  bring-up.
- **Fast quit.** `static ENGINE_STARTED: AtomicBool`; `RunEvent::Exit` returns immediately when
  false.
- **Catalyst.** Device-module selection is now the explicit allowlist
  `all(target_os = "ios", target_abi = "")`.

**Cross-file remainders the integrator must sequence (all outside that agent's ownership):**
1. `build.rs:47-50` still gates the `jre-ios/aarch64-ios` link flags on `abi != "sim"`; `lib.rs` now
   uses `target_abi == ""`. **The two must match** or Catalyst gets the stub module with device
   `.a` files. Device and both sim triples are unaffected either way.
2. `apps/reader/src/lib/context.ts` should `invoke('suwayomi_start')` when
   `config.nativeEngine && isTauri()`. Until it does nothing breaks — autostart defaults to ON when
   no flag is visible to Rust.
3. **For the compile-time halves to work in shipped builds**, `build.rs` (or the tauri build
   invocation) must forward the reader's env, e.g.
   `println!("cargo:rustc-env=PUBLIC_KOMIKA_API={..}")` and likewise for
   `PUBLIC_KOMIKA_NATIVE_ENGINE`. **Without it, `tauri dev` still needs
   `KOMIKA_API_ORIGIN=http://localhost:8080` exported to get images**, and the route telemetry
   labels everything `route=cdn` (the `host=` field is always present, so the destination is still
   unambiguous).
4. `suwayomi::start()`'s early-bail paths still leave the state `Degraded` with no supervision
   task, so a quit **after a failed start** still burns the 6 s poll.

### `fix:desktop-sup` — `apps/reader/src-tauri/src/suwayomi.rs` (5 applied, 2 deliberately skipped)
- **Engine authentication — the other security-relevant fix.** `render_server_conf()` now writes
  `server.authMode = "BASIC_AUTH"` + `server.authUsername`/`authPassword`. The names are **not
  guessed**: the agent read Suwayomi v2.3.2243 `ServerConfig.kt` (~363-401/607), `AuthMode.kt` and
  `JavalinSetup.kt` (~216-244) — `BASIC_AUTH` is the exact enum constant config4k parses, and the
  Javalin gate 401s every request except CORS preflight / page-icon / web-manifest, which covers
  `/api/graphql` **and** the `/api/v1/...` image routes. Only the modern keys are written; the
  deprecated `basicAuth*` trio is omitted. The password is a per-process 128-bit `OnceLock` token
  drawn from `RandomState`'s OS-seeded SipHash keys (**no new crate**). `build_client()` attaches
  `Authorization: Basic …` as a *sensitive* default header on the supervisor's single
  `reqwest::Client`, so `poll_ready`, `suwayomi_gql`, `suwayomi_image` and
  `cloudflare::apply_settings` are all authenticated with **zero call-site changes**. *Note:
  `suwayomi_base_url` is still exported to JS but unused; anything fetching it directly from the
  WebView will now get 401 — the IPC commands are the supported path.*
- **Heartbeat lockfile.** `LockGuard` keeps its `File` and gains `refresh()`; a 15 s heartbeat task
  advances the mtime. `acquire_lock()` takes over a lockfile whose mtime has stood still for 60 s —
  the signature of a SIGKILLed holder — and **never** steals one whose metadata is unreadable or
  future-dated. `start()` no longer treats a held lock as fatal: `run_engine()` retries every 5 s,
  reports Degraded once, and proceeds as soon as it wins. Net behaviour change: **a post-crash
  relaunch is Degraded for up to ~65 s and then self-heals, instead of being bricked until the user
  deletes a file by hand.**
- **Child-exit race.** `boot_engine()` races `poll_ready` against `child.wait()` in a biased
  `select!`; a JVM that dies during startup fails in ~1 s carrying its real `ExitStatus`, and both
  failure paths funnel through `stop_child()` so nothing is orphaned. Because failed boots are now
  fast, the existing >5-restarts-in-60 s storm cap can finally trip on boot failures.
- **Graceful shutdown.** `request_graceful_exit()` sends SIGTERM via `kill(1)` on unix (std has no
  signal API and the crate has no `libc` dep) so the JVM's shutdown hooks run and H2 closes cleanly,
  then falls back to the unchanged `start_kill` ladder. Worst-case quit cost unchanged (3 s + 3 s).
- **Port-broker collision.** `poll_ready()` distinguishes "nothing there yet" from "a stranger owns
  this port" (a 2xx body with no `data`/`errors` key) and names the collision; both messages now
  carry the port.
- **Skipped 1 — the mutation-allowlist half of the auth fix.** *Its reasoning, verbatim in
  substance:* a mutation allowlist/denylist in `suwayomi_gql` covering repo/extension-install
  mutations **would break the app's own provisioning** — `packages/api/src/local-suwayomi-backend.ts`
  and `graphql-backend.ts` legitimately call `addExtensionRepo` / `installExtension` /
  `updateExtension(patch:{install:true})` through that exact command. **Any denylist that stops an
  attacker also stops Komika.** Authentication is the enforceable control here; a
  WebView-XSS-scoped allowlist needs a JS-side operation whitelist in `packages/api`, outside that
  agent's file list.
- **Skipped 2 — a separate consecutive-failure counter.** Now that boot failures return in ~1 s
  rather than ~31 s, the existing `MAX_RESTARTS_PER_MIN` window engages on exactly the storm the
  finding describes; a parallel counter would be new machinery for no additional coverage.
- **Not fixed, out of blast radius:** a stop signalled *during* boot still drops the boot future and
  relies on `kill_on_drop` (SIGKILL), bypassing the new SIGTERM path.

### `fix:reader-ui` — `source.ts` + `Cover.svelte` (2 applied, 1 deliberately skipped)
- **`Promise.allSettled`.** New `resolvePageUrls(domainPages)` (`source.ts:1804-1822`) returns
  fulfilled URLs as-is and maps each rejection to `''` after a `console.warn` naming the 1-based
  page number; both `getReaderChapter` call sites use it. A 40-page chapter with one bad page now
  renders 39 pages instead of "No pages available".
- **Cover blob release.** The native `.then`'s `!alive` branch now calls `images.release(u)` instead
  of silently dropping the just-minted object URL.
- **Skipped — lazy per-page resolution.** *Its reasoning:* it cannot be fixed inside those two
  files. It needs (a) `packages/api/src/image-provider.ts` to expose an on-demand/abortable resolve
  (today `NativeImageProvider.resolvePage` unconditionally pulls the bytes and mints a blob), and
  (b) `routes/read/[slug]/+page.svelte` to resolve per-page on visibility while keeping its
  blob-revoke bookkeeping (~lines 96-112) correct for URLs that arrive later. A `source.ts`-only
  variant is impossible: `ReaderView.pages[].url` is a plain string materialized in
  `buildReaderView`, and `load` data from a non-`.svelte.ts` module is not reactive, so there is no
  way to push later URLs into the already-rendered `{#each pages}` — resolving only the first N
  would permanently blank the tail of every chapter.
- **Two behavioural caveats for the integrator:** a page whose resolve now fails renders the
  route's *generic placeholder* tile, not the "tap to retry" tile, because the retry branch
  (`+page.svelte` ~499/~529) gates on `p.url` being truthy. And `data.pages.length === 0` is what
  suppresses the view-count ping (`+page.svelte:59-61`) — a chapter where *every* page failed now
  has a non-empty array and will count as a view.

### `fix:packaging` — died mid-group on a quota limit (6 of 7 applied)
Its edits **are present and coherent** (`Cargo.toml`, `tauri.conf.json`, `scripts/build-jre.sh`,
`scripts/stage-ios-jvm-runtime.sh`; both scripts pass `bash -n`):
- `beforeBuildCommand` now chains `fetch-suwayomi-jar.sh` + `build-jre.sh` before `pnpm build`.
- `build-jre.sh` no-ops for `TAURI_ENV_PLATFORM=ios|android`, hard-fails unless `java -version`
  reports feature 21, and emits a `jre/<slug>.manifest` with a sorted-tree SHA-256 that
  `KOMIKA_JRE_MANIFEST_SHA256` can turn into a build failure.
- `stage-ios-jvm-runtime.sh` now asserts **all 13** `-force_load` libs, `DEAD_CODE_STRIPPING: false`,
  `-lz`/`-liconv`/`-lc++` and both frameworks (previously it grepped for three substrings), can
  **print the entire recipe** so a wiped `project.yml` is recoverable from the repo alone, hard-fails
  if `gen/apple/assets/jre` exists (the 218 MB desktop-JRE leak), and enforces 200 MB runtime /
  400 MB payload ceilings.
- CSP: `img.komiq.cc` and the dev origins removed from the shipped native CSP; `api.komiq.cc` added
  to `img-src`; dev origins moved into a new `devCsp`.
- `rust-version` bumped `1.77.2` → `1.78` with an in-file note that pre-1.78 `cfg(target_abi)` is a
  hard `E0658` on **every** target, desktop included.

**Left undone:** `tauri.ios.conf.json` is unchanged and there is no post-`tauri ios build` `.app`
verifier, so `ios-bundle-size-and-desktop-jre-leak-ungated` is only partially addressed.

### `fix:api-pkg` — never ran
`packages/api/src/local-suwayomi-backend.ts` is untouched; `android-isready-ipc-per-call` (LOW) is
**OPEN**.

---

## 6. Trust caveats — read this before acting on anything above

**(a) The iOS fixes are compiler-unverified, and there is no way to verify them here.**
`suwayomi_ios.rs` is `#[cfg(target_os = "ios")]` + non-simulator ABI. **No CI job and no command on
this host compiles it.** The five iOS fixes were therefore written with **zero compiler feedback**.
The file was confirmed to *parse* cleanly (`rustfmt --edition 2021 --check`, zero parse errors) — but
**parsing is not type-checking**. The fix agent hand-verified types, borrows and moves and reported
`compile_risk: low-to-moderate`; the only genuinely new JNI symbol is
`JNIInvokeInterface_::DetachCurrentThread` (Option-wrapped like the rest), plus
`std::panic::{catch_unwind, AssertUnwindSafe}`, `reqwest::RequestBuilder::timeout` and
`tokio::time::sleep`. **Treat the iOS module as unbuilt until a macOS runner says otherwise.**

**(b) One refuter per finding, not a panel — and only for 30 of 84.**
The adversarial pass gave each of the 30 auto-fix candidates a **single** refuter. 27 confirmed, 3
refuted. **The other 54 findings never faced a challenge at all** and are marked `U` in §4. Several
of them are high-severity (`imagefetch-hosted-proxy-still-load-bearing-on-native`,
`imagefetch-no-per-source-request-headers`, `desktop-macos-no-jvm-entitlements`, every `cfshim`
finding). They are cited and internally consistent, but they are one agent's reading.

**(c) Two fix groups did not finish.**
`fix:packaging` died on a quota limit with its 7th finding unaddressed (`tauri.ios.conf.json`
unchanged). `fix:api-pkg` never ran. Nothing was left half-edited — both scripts pass `bash -n` and
the gate is green — but the coverage is incomplete.

**(d) Other things explicitly not verified.**
- No fix was run on a device, a simulator, or macOS. `needs_macos_verification: true` on both the
  `ios-jni` and `lib-rs` groups.
- No fix group owned `cloudflare.rs`; all 16 shim findings are untouched code, including the
  unauthenticated loopback cookie oracle.
- The Basic-auth change has **not** been exercised against a real engine. The ignored integration
  test (`KOMIKA_JAVA_BIN=… KOMIKA_SUWAYOMI_JAR=… cargo test -- --ignored sidecar_boots`) is the
  cheap way to confirm the engine boots with `authMode` set and that `poll_ready` authenticates —
  it needs a real JRE + jar but **not** macOS.
- The dirty working tree contains substantial **unrelated** in-progress server browse/catalogue work
  (`apps/server/**`, `packages/api/src/graphql-backend.ts`, several reader routes). None of it was
  audited, fixed, or reviewed here, and none of the numbers in this document describe it.
- `desktop-eager-whole-chapter-image-fetch` / `imagefetch-eager-whole-chapter-byte-prefetch` remain
  **open and are the largest known native UX defect**: a 200-page chapter still buffers every page
  before first paint.

---

## 7. Build gate

Run by the orchestrator (the dedicated gate agent died on a quota limit). **Baseline:
`cargo check --all-targets` was GREEN before any edits**, so the deltas below are attributable.

| Check | Result |
|---|---|
| `cargo check --all-targets` | **GREEN** — finished dev profile, 0 errors, 0 warnings |
| `cargo clippy --all-targets -- -D warnings` | **GREEN** — 0 warnings. *The clippy component had to be installed first; it was not present on this host.* |
| `cargo test` | **GREEN** — **27 passed, 0 failed, 1 ignored** (up from 20 pre-run; the fix agents added 7 tests: `lock_heartbeat_clears_abandonment`, `stale_lockfile_is_taken_over`, `backoff_grows_and_stays_bounded`, `retries_only_transient_statuses`, `engine_password_is_stable_and_unguessable`, `image_path_guard_*`, `parses_api_origin_from_a_graphql_url`) |
| reader `svelte-check` | **GREEN** — 0 errors, 1 warning; the warning (`Cover.svelte:26`, `state_referenced_locally` on the `syncResolve(src)` initializer) is **pre-existing**, confirmed both by diff context and independently by the `reader-ui` fix agent |
| `cargo check --target aarch64-apple-ios` | **IMPOSSIBLE ON THIS HOST** — see below |
| `suwayomi_ios.rs` parse check | `rustfmt --edition 2021 --check`: **zero parse errors**. The 1591-line rustfmt diff is the repo-wide 2-space-vs-rustfmt style, not a defect (`lib.rs` shows 5794 and `suwayomi.rs` 1978 diff lines on the same check). **Parsing is not type-checking.** |

### The iOS cross-check is impossible on Linux — and the audit predicted the wrong reason

The `testgap` lane recommended a **Linux** cross-check job, reasoning that "`cargo check` never
links, so no Xcode/NDK is required", and confirmed all three targets are installable via `rustup` on
this aarch64 Linux host. **That conclusion is refuted.** The `aarch64-apple-ios` std was installed
successfully, but `cargo check --target aarch64-apple-ios` **fails inside a dependency build
script**: `objc2-exception-helper` v0.1.1 compiles `src/try_catch.m`, and `cc-rs` needs both `clang`
and `xcrun --show-sdk-path` to locate the iPhoneOS SDK. Neither exists on Linux. Linking is not the
only thing that needs a toolchain — **build scripts still run**.

**Corrected CI recommendation:** a `macos-latest` job running
`cargo check --target aarch64-apple-ios` (plus `-sim`). That is still cheap, free on GitHub-hosted
runners for public repos, and fully automatable — **it simply cannot be a Linux job.** An
`aarch64-linux-android` cross-check may still work on Linux and should be tried separately.

---

## 8. Device testing: unblocked vs still blocked

### Now unblocked (no new hardware needed)

| Action | Why it is now possible |
|---|---|
| Produce a desktop bundle that actually contains an engine | `beforeBuildCommand` chains `fetch-suwayomi-jar.sh` + `build-jre.sh`, so `pnpm desktop:build` no longer ships an empty `jre/`. **Untested end-to-end — no CI job runs `tauri build`.** |
| Run `tauri dev` with working images | Export `KOMIKA_API_ORIGIN=http://localhost:8080`. (Once §5 remainder 3 lands, the compile-time `PUBLIC_KOMIKA_API` covers this automatically.) |
| Diagnose a Degraded engine in a release build | `tauri_plugin_log` now writes to `LogDir/komika`, `KeepSome(3)`, 2 MiB max; the `images` target logs the route per fetch. |
| Recover from a crash without deleting a lockfile by hand | The heartbeat lock self-heals in ~65 s. |
| Confirm the engine boots under Basic auth | `KOMIKA_JAVA_BIN=… KOMIKA_SUWAYOMI_JAR=… cargo test -- --ignored sidecar_boots` on **any** desktop OS. No macOS needed. |
| Land the cheap CI wins | Add `cargo test --all-targets` to `ci.yml`'s `rust` job (375 server tests); add an `ubuntu-latest` job running `run-fallback-check.sh` + `run-offline-check.sh` (both verified to pass here in <1 s, no engine or network); add `packages/api/**` to `native-sidecar.yml`'s path filter. |
| Add the missing native-image-routing check | The `fallback-ladder.check.ts` pattern (esbuild + node, stubbed `invoke`) needs no device — assert which command name each representative URL dispatches to, plus a case pinning that no input ever yields a `workerBaseUrl` request. |

### Still blocked, and the exact human action that unblocks each

| Blocked | Required human action |
|---|---|
| **Any statement that the iOS module compiles.** Five fixes are compiler-unverified; the module has never been type-checked. | **A macOS machine with Xcode** (or a `macos-latest` GitHub runner). Run `cargo check --target aarch64-apple-ios --all-targets`, then `cargo clippy --target aarch64-apple-ios -- -D warnings`. **This is the single highest-value unblock in the whole document** and it is CI-automatable. |
| iOS `.ipa` build, codesign, bundle-content verification, the `tauri.ios.conf.json` fix | macOS + Xcode + an Apple developer identity. Then re-run `stage-ios-jvm-runtime.sh` and confirm the new `gen/apple/assets/jre` guard and the 200/400 MB ceilings behave. |
| **Whether the iOS engine ever reaches `ready` on hardware** — cold start, RSS, jetsam headroom, `java.io.tmpdir`/`user.home` actually landing on the pinned paths, extension install surviving `-Xss4m`, the new heartbeat degrading correctly on a killed VM | **A physical iPhone (≥4 GB) plus the trust tap:** install the signed build, then on the device go **Settings → General → VPN & Device Management → [your developer profile] → Trust**. Without that tap the app will not launch and none of the N4.3 measurements can be taken. |
| macOS notarization / hardened-runtime survival of a bundled JVM | macOS + a Developer ID. Add the entitlements plist (`allow-jit`, `allow-unsigned-executable-memory`, `disable-library-validation`), and either move the JRE under `Contents/Frameworks` or sign every nested Mach-O before the outer bundle. |
| Windows console-window fix verification; orphan-JVM reaping (`PR_SET_PDEATHSIG` / Job Object / kqueue) | A Windows host and a Linux host; each mechanism is per-OS and none can be tested from the others. |
| **Anything Android** — N3.2 `dlopen`/`dlsym` of a bundled `libjvm.so`, the foreground service, `jniLibs` packaging, 16 KB page alignment, bionic linkage, ART coexistence | **An Android SDK + NDK host and a physical Android device.** None of it exists in this environment. Note the probe that "de-risked" this validated `execve` on glibc — the model API 29+ forbids — so **all** Android runtime risk is still live. |
| **The live Cloudflare solve** — `cf_clearance` actually replayed through the engine, per-OS webview UA-override/cookie-read behaviour, the interactive-Turnstile UX | **A machine with a real display** (the challenge window cannot be driven headless) plus a genuinely CF-gated source. This is the standing `could_not_verify` in `device-verify-runbook.md`. It should be attempted only **after** the dot-stripped-domain and stale-clearance-reuse findings are fixed, or the solve will appear to fail for the wrong reason. |

---

## 9. If you read only one thing

`img.komiq.cc` is already structurally unreachable from every native build
(`packages/api/src/image-provider.ts:241`). **That half of the goal is done.** The other half is
not: the server hands out absolute `https://api.komiq.cc/api/v1/...` cover and page URLs
(`apps/server/src/suwayomi.rs:344-350`), and the native provider fetches them through `fetch_image`,
so **every Suwayomi-sourced cover and page on desktop, iOS and Android still comes from our
origin.** The embedded-engine byte path is unreachable in any shipped build for three independent
reasons (flag off in `apps/reader/.env:19` and not overridden in `.env.production`;
`CompositeBackend.pages()` unconditionally hosted at `composite-backend.ts:244-248`; the mobile
engine command always `Err`s at `suwayomi_mobile.rs:104-112`). **Do not describe the native apps as
server-independent.**
