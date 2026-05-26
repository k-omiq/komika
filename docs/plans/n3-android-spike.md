# N3-SPIKE — Android feasibility for the embedded Suwayomi engine

> **Status:** research spike, UNCOMMITTED, review-gated. No implementation until reviewed.
> **Scope:** answers the six N3 questions in `native-embedded-suwayomi.md` §5 (Phase 3 — Android) and
> §8b (Cloudflare on-device) with evidence, then proposes a build plan. iOS (§6 / Phase 4) is out of
> scope except where it constrains an Android decision.
> **Pin under study:** we embed **stock Suwayomi‑Server `v2.3.2243`** (jar SHA in
> `apps/reader/src-tauri/suwayomi/VERSION`, 174 MB desktop). All findings are against that pin and
> against `tachimanga/Tachidesk-Server` as of commit `2026-07-15` (its most recent push).

---

## 0. Executive summary + recommendation

**Recommendation: pursue path (b) — bundle an embeddable JVM on Android that runs our *stock*
`v2.3.2243` jar in‑process — and harvest, not adopt, TachiManga's fork. Confidence: medium.**

The single most decision-shaping fact this spike uncovered: **TachiManga's actually‑shipping mobile
code (`master`) is based on the legacy *pre‑GraphQL* Tachidesk‑Server (REST/Javalin era, `Constants.kt`
default `v0.7.0`), not the modern GraphQL `v2.3.x` tree we built our entire stack against.** The
`v2.3.22xx` tags on the fork are unmodified mirrors of upstream tags (a byte‑for‑byte diff of
`tachimanga:v2.3.2232` vs `Suwayomi:v2.3.2232` is *identical*); the mobile patches live only on `master`,
which is `behind_by: 1026` commits from that tag and exposes **no `suwayomi/tachidesk/graphql`
package** — only REST `controller/`+`impl/`. Our `LocalSuwayomiBackend`, the N‑GQL‑SPIKE contract work,
N1 (desktop sidecar) and N2 (extension provisioning, fallback ladder, offline queue) all speak the
**GraphQL** API. **Adopting TachiManga's fork wholesale (path a) would mean throwing that away and
re‑targeting a stale REST engine + inheriting their product rewrite** (cloud sync, backup, tracking,
migrate, PIP controllers — ~300 changed files, most of it *not* mobile‑enablement).

Meanwhile the technical barriers that originally made path (a) look mandatory are softer than the plan
assumed, **because Android — unlike iOS — permits a full JVM** (JIT allowed; Termux ships OpenJDK), and
because upstream `v2.3.2243` already removed the two worst native dependencies:

- **JS engine:** upstream substitutes Tachiyomi's native `com.squareup.duktape:duktape-android` with
  **pure‑Java `org.graalvm.polyglot:js-community`** (GraalJS), which runs *interpreted* on any stock
  JDK 21+ with **no native `.so`**. Duktape‑the‑native‑lib was the classic Android blocker; it's gone.
- **DEX→JVM conversion:** done in pure JVM bytecode via **`de.femtopedia.dex2jar` 2.4.37** (ASM‑based) —
  runs anywhere a JVM runs; not a native blocker.
- **SQLite:** the one remaining mandatory native lib (`xerial/sqlite-jdbc`) **now ships Android natives**
  (`natives-android` classifier, `Linux-Android/aarch64` → `arm64-v8a`), so it is a *supply‑the‑`.so`*
  problem, not a *port‑the‑engine* problem.

That leaves exactly **two** things stock `v2.3.2243` genuinely cannot do on Android, and both are narrow,
harvestable, MPL‑licensed pieces of TachiManga's fork rather than reasons to fork the whole engine:

1. **AWT/Java2D image code paths** in `AndroidCompat` (`Bitmap`/`BitmapFactory`/`Canvas`/`Paint`) —
   TachiManga rewrote these to be AWT‑free (diff: `android/graphics/Paint.java` +840, `Bitmap.java`
   +209, `BitmapFactory.java` +410). Whether we even hit them depends on which endpoints *we* use (we
   proxy raw page bytes and use hosted covers). **#1 device‑only unknown.**
2. **A WebView↔engine Cloudflare bridge** — stock Suwayomi drives *FlareSolverr*, it has no device
   WebView path on any platform. TachiManga added `android/webkit/CookieManager` + `McCookieManager` +
   `McCookieJar` + `NativeNet`. On Android our existing **N‑CF Rust FlareSolverr‑v1 shim +
   `setSettings(flareSolverrUrl=…)`** design carries over unchanged; only the *cookie‑harvest source*
   changes from "read the Tauri WebView cookie" to "a Kotlin Tauri plugin calling
   `android.webkit.CookieManager.getInstance().getCookie(url)`" (Mihon's mechanism).

**Why (b) over (a):** (b) keeps **one engine (stock `v2.3.2243` GraphQL) across desktop+Android+iOS**,
preserving all of N1/N2 and the entire client stack above the transport; the Android‑specific delta is a
JVM‑bundling + foreground‑service + two shims problem. (a) forks the engine onto a stale REST tree, splits
desktop/mobile onto two contracts, and imports a large third‑party product surface we'd have to maintain.

**When (b) fails, fall back toward (a)-style harvesting, not full adoption:** if AWT proves impossible to
satisfy on an Android JVM *and* our code paths can't avoid it, cherry‑pick TachiManga's MPL AndroidCompat
graphics files onto our `v2.3.2243` build (MPL‑2.0 → AGPL‑3.0 is one‑way compatible; keep per‑file MPL
notices). This is a forward‑port of ~6 files, not a fork.

**GraalVM native‑image is ruled out** (question 2): Suwayomi loads extensions at runtime via a
`URLClassLoader` over dynamically fetched+converted bytecode — the textbook violation of native‑image's
closed‑world assumption. Not viable. Evidence in §2.

**Confidence: medium**, gated on three device‑only unknowns (AWT paths, in‑process JVM cold‑start/perf,
CF WebView replay), each with a headless or emulator pre‑check below.

---

## 1. Evidence per question

### Q1 — TachiManga's fork: license, patch surface, staleness

**License — verified MPL‑2.0, AGPL‑compatible.** `gh api repos/tachimanga/Tachidesk-Server` →
`license.spdx_id = "MPL-2.0"`, `parent/source = Suwayomi/Suwayomi-Server`. Confirmed in‑tree: harvested
files (e.g. `org/tachiyomi/ChildFirstClassLoader.java`) carry `Copyright (C) 2026 Tachimanga … Mozilla
Public License, v. 2.0`. MPL‑2.0 is **file‑level** copyleft and is **one‑way compatible into GPL/AGPL**
(MPL §3.3 + the "Secondary License" GPL‑compatibility clause): we may combine MPL files into our AGPL‑3.0
project provided each MPL file keeps its notice and its source stays available. So *harvesting specific
files is clean*; it does not force the rest of our tree to MPL, and it satisfies AGPL. Repo:
<https://github.com/tachimanga/Tachidesk-Server>, license blob confirms MPL‑2.0.

**Patch surface — enumerated by diffing `Suwayomi:v2.3.2232 … tachimanga:master`** (`gh api …/compare`,
`diverged`, ahead 29 / behind 1026, **300 files changed**). Concrete categories:

| Area | Files (examples) | What it changes vs upstream |
|---|---|---|
| **AWT‑free graphics** | `android/graphics/{Paint(+840),Bitmap(+209),BitmapFactory(+410),Canvas,Rect,NativeRef}.java` | Replaces desktop Java2D/AWT‑backed bitmap/canvas with self‑contained impls (mobile has no X11/fontconfig/`libawt`). |
| **Android event loop** | `android/os/{Handler,Looper,Message(+631),MessageQueue(+1244)}.java`, `os/shadows/{NativeObjRegistry,ShadowPausedMessageQueue}` | Robolectric‑style main‑looper so extensions expecting `Handler`/`Looper` work off‑Android. |
| **WebView compat + CF bridge** | `android/webkit/{WebView(+1235),CookieManager(+305),McCookieManager,WebSettings,WebResourceResponse,WebViewClient,WebChromeClient,MyWebSettings}.java` | Real cookie store + WebView surface the engine can call — the on‑device Cloudflare path. |
| **Native networking** | `org/tachiyomi/{NativeNet,NativeString,NativeChannel}.java`, `network/{McCookieJar,CallNativeNetInterceptor,McLoggingEventListener}.kt`; **vendored `org.jsoup` source** (jsoup rerouted through native HTTP) | Routes OkHttp/jsoup through platform‑native networking + the WebView cookie jar. |
| **Classloading** | `org/tachiyomi/{ChildFirstClassLoader,ChildFirstClassLoader2}.java` (branch `fix/classloader`) | Child‑first `URLClassLoader` (parent‑first only for `java.*/javax.*/sun.*/okhttp3.*`) to avoid extension↔host class conflicts. Mirrors Mihon's `ChildFirstPathClassLoader`. **Confirms a URLClassLoader/HotSpot‑style JVM, not ART.** |
| **JS engine** | `app/cash/quickjs/*` added; `com/squareup/duktape/*` removed | Their tree predates upstream's GraalJS swap; they used QuickJS. (We inherit upstream's GraalJS instead — see Q2.) |
| **iOS SQLite** | `libs/sqlite-jdbc-ios-3.41.0.0.jar`, removes `util/DriverJar.java` | Custom SQLite JDBC for iOS. |
| **Build/runtime** | `libs/{android.jar,cloud-api}.jar`, `buildcopy_m2.sh`, `buildSrc`, `libs.versions.toml`, `Constants.kt` | Mobile build plumbing; version pinned back to `v0.7.0` default. |
| **Product rewrite (NOT mobile‑enablement)** | dozens of REST `controller/`+`impl/` (`Cloud`, `ProtoBackup`, `Track`, `Migrate`, `Stats`, `History`, `Pip`, `Repo`, `Browse`…) + `suwayomi/tachidesk/cloud/*` | TachiManga's app product on the **REST** API. Large, entangled, and irrelevant to us. |

**Staleness — two different answers, both important:**
- *Vs our pin:* the fork tracks upstream **tags** closely — latest synced tag `v2.3.2232` vs our
  `v2.3.2243` (~11 build numbers / a few weeks); last push `2026-07-15`. So as a *reference*, it's current.
- *Vs the API we need:* the fork's **working `master` is far behind** — `behind_by: 1026`, `Constants.kt`
  default `v0.7.0`, and **no GraphQL package** (`gh api …/contents/…/suwayomi/tachidesk?ref=master` →
  `manga, global, cloud, server, kotlin`; upstream `v2.3.2243` has `graphql, opds, i18n, …`). This is the
  crux: their runnable mobile artifact is a *different, older engine* than the one we integrate with.

### Q2 — Decision (a) reuse patches vs (b) embeddable JVM on stock jar; the classloading blocker

**Extension classloading on ART vs HotSpot — resolved and it favours (b).** Suwayomi does **not** use
Android's `DexClassLoader`. At install it downloads the extension **APK**, validates the signature
(`apksig`), parses it (`apk-parser`), then **converts `classes.dex` → JVM bytecode with `dex2jar`
(`de.femtopedia.dex2jar` 2.4.37, ASM‑based, pure‑JVM)** and loads the resulting jar with a **standard
`URLClassLoader`** (TachiManga hardens this to child‑first). Keiyoushi/Tachiyomi extensions ship `.apk`
(with `classes.dex`); Suwayomi is what turns that into JVM `.jar` on the fly. Consequences:
- The conversion + `URLClassLoader` are **pure JVM**, so they run identically on a *bundled* JVM on
  Android as on desktop — **classloading is not an Android blocker for (b).**
- It also means we **cannot** shortcut to ART/`DexClassLoader`: the extensions' host‑side glue
  (`AndroidCompat`, `eu.kanade.tachiyomi.*`) is compiled against a *desktop* `android.jar` shim, not real
  Android; running on ART would collide with the platform's real `android.*`. A bundled non‑ART JVM +
  AndroidCompat shim is the intended model. Evidence: DeepWiki "Extension Management"
  <https://deepwiki.com/Suwayomi/Suwayomi-Server/4.1-extension-management>; upstream `libs.versions.toml`
  (`dex2jar = "2.4.37"`, `asm` "version locked by Dex2Jar").

**(b) embeddable‑JVM options on Android, assessed:**
- **Bundled OpenJDK for Android (aarch64) — recommended.** Android allows JIT for an app's own bundled
  code (Termux ships OpenJDK 17/21). A HotSpot (or Zero) `libjvm.so` from an Android/aarch64 OpenJDK build
  (Termux, PojavLauncher, Gluon's OpenJDK‑Mobile pipeline) can run our stock jar. JIT ⇒ acceptable perf,
  unlike iOS. Bundled OpenJDK provides its own `JNI_CreateJavaVM` (do **not** use the system
  `libart.so`/`libnativehelper` — that's ART, wrong runtime).
- **GraalVM native‑image — ruled out.** Native‑image's closed‑world assumption forbids loading new
  bytecode at runtime via non‑built‑in classloaders; Suwayomi's whole extension model is a runtime
  `URLClassLoader` over just‑converted bytecode. `URLClassLoader` is explicitly a "non‑built‑in
  classloader." Fatal. <https://www.graalvm.org/latest/reference-manual/native-image/metadata/Compatibility/>,
  oracle/graal#461.
- **Termux‑style / microVM / separate‑process JVM — rejected on Android platform grounds.** Apps
  targeting **API ≥ 29 cannot `execve()` a binary from the app's writable/data dir** (W^X). A `java`
  launcher would have to live in `nativeLibraryDir` as a `lib*.so` and be exec'd from there — awkward and
  brittle. **In‑process `JNI_CreateJavaVM` from a `dlopen`'d bundled `libjvm.so`** sidesteps this and
  matches how iOS must work anyway. Evidence: Android 10 behavior changes (no exec from home dir);
  termux‑app#1072; JNI‑create‑JVM notes (calebfenton).

**Native‑lib gap for (b) after the GraalJS/dex2jar wins — only one hard item + one soft item:**
1. **SQLite (`xerial/sqlite-jdbc`) — solvable.** Ships `natives-android` (`Linux-Android/aarch64`); place
   `.so` in `jniLibs/arm64-v8a` (+ set `org.sqlite.lib.path`/extract on first run). Verify the exact
   version Suwayomi's Exposed stack pulls bundles an Android native or can be overridden.
   <https://github.com/xerial/sqlite-jdbc> USAGE.md, PR #662.
2. **AWT/Java2D — soft, device‑only.** `AndroidCompat` cover/thumbnail/bitmap paths may call
   `java.desktop`/AWT, which needs fontconfig/freetype absent on Android. Mitigations: (i) use an Android
   OpenJDK that includes a working headless `java.desktop`; or (ii) confirm our *used* endpoints (raw page
   proxy, hosted covers) never hit AWT; or (iii) harvest TachiManga's AWT‑free `android/graphics/*`.

### Q3 — Foreground‑service model in a Tauri v2 Android shell

- **Tauri v2 Android = a single `Activity` hosting the system WebView**, with native code added via
  **Kotlin plugins** (`app.tauri.plugin.Plugin`, `@TauriPlugin`, `@Command`) invoked from Rust/JS through
  JNI glue Tauri generates. Crucially, **plugin native code can run while the WebView is suspended**
  (JNI on Android / FFI on iOS), which is what a long‑lived engine needs.
  <https://v2.tauri.app/develop/plugins/develop-mobile/>, discussion tauri#10695.
- **Run the engine in‑process, in a foreground service — not a child process.** Given the API‑29 exec ban,
  the engine JVM must be created **in‑process** (`JNI_CreateJavaVM` on the bundled `libjvm.so`) inside an
  **Android foreground `Service` with a persistent notification** so the OS won't reap it when backgrounded.
  `START_STICKY` gives best‑effort OS restart under memory pressure. This mirrors what the community
  `tauri-plugin-background-service` does (Android foreground service + persistent notification;
  Tokio work "freezes after ~30s" when backgrounded unless a foreground service holds it). We'd likely
  write our own thin Kotlin foreground‑service plugin rather than depend on that crate, to own the JVM
  lifecycle. <https://crates.io/crates/tauri-plugin-background-service>; note the leaked‑`MainActivity`
  bug tauri#11609 when a foreground service is mishandled — design the service to detach cleanly.
- **Process‑death/restart:** treat the JVM as recreatable. Our N1 supervisor (`suwayomi.rs`: readiness
  gate, capped‑backoff restart, degraded state → fallback ladder) maps onto the service: on Android the
  "restart" is re‑`JNI_CreateJavaVM` (or service restart), not re‑spawn. Persist nothing that assumes the
  JVM survives backgrounding.
- **Transport = loopback HTTP is fine on‑device.** The in‑process JVM binds `127.0.0.1:<ephemeral>`; the
  WebView/Rust reach it exactly as on desktop. Loopback avoids Android's cleartext‑HTTP block for
  non‑loopback (still, add a `network_security_config` allowlisting `127.0.0.1` to be safe). We keep the
  **IPC‑proxy transport** (`suwayomi_gql`/`suwayomi_image` Tauri commands) so the port is never exposed to
  the WebView and CSP stays `connect-src 'self' ipc:`. Binder is unnecessary — we're in one process.
- **Doze/battery:** foreground service + notification largely exempts active reads; downloads should use
  `WorkManager`‑style constraints. Consider the battery‑optimization‑exemption prompt pattern
  (`tauri-plugin-android-battery-optimization`) only for long background downloads, not for foreground
  reading.
- **Prior art:** **Mihon** runs sources in‑process on ART and uses foreground services + WorkManager for
  library updates/downloads. **TachiManga** runs the engine **in‑process** (iOS has no fork/exec; Android
  same pattern) — consistent with our in‑process recommendation.

### Q4 — Cloudflare on Android (the N‑CF spike said Tauri's cookie API returned empty on Android)

The N‑CF design already has the right shape; only the cookie source changes on Android.

- **Keep the existing on‑device N‑CF machinery unchanged:** the loopback **Rust FlareSolverr‑v1 shim**
  (`src-tauri/src/cloudflare.rs`) + wiring the stock engine via `setSettings(flareSolverrUrl=<shim>)` so
  stock Suwayomi injects `cf_clearance`/UA itself. No engine fork needed for CF (already proven on desktop).
- **Replace the cookie‑harvest source with `android.webkit.CookieManager`.** Mihon's mechanism: WebViews
  persist cookies into a **global** `android.webkit.CookieManager`; after a challenge solve you read
  `CookieManager.getInstance().getCookie(url)` and get `cf_clearance` for that host. Tauri's own cookie
  API came back empty on Android (N‑CF notes) because it doesn't surface the WebView cookie jar. Fix: a
  **small custom Kotlin Tauri plugin** exposing `getCookie(url): String` (and `flush()`), plus a hidden/
  shown `WebView` to *perform* the solve. The Rust shim calls this plugin (via the Tauri command bridge)
  where on desktop it read the Tauri WebView. `android.webkit.CookieManager` reference:
  <https://developer.android.com/reference/android/webkit/CookieManager>.
- **UA unification is mandatory:** the challenge‑solving `WebView`'s User‑Agent and the engine's OkHttp UA
  **must match** or Cloudflare rejects the replay. The Kotlin plugin must report the exact WebView UA back;
  the shim already threads UA into `setSettings`. Interactive challenges (Turnstile/CAPTCHA) require
  *showing* the WebView — a real Activity/view, deferred to device testing.
- No JNI gymnastics required beyond the standard Tauri Kotlin‑plugin `@Command` path; `CookieManager` is a
  plain platform API callable from the plugin's Kotlin.

### Q5 — Packaging (Tauri v2 Android, JVM bundling, APK size, minSdk)

- **`tauri android init`** (Tauri v2 stable) scaffolds a Gradle Android project (single‑Activity WebView
  shell) under `src-tauri/gen/android`; needs Android SDK + **NDK** + a JDK on the build host. Our repo
  already flags this as pending in the plan/SPEC. <https://v2.tauri.app/develop/> (mobile),
  <https://v2.tauri.app/develop/plugins/develop-mobile/>.
- **Bundle the JVM as `jniLibs`, not `assets`.** Native `.so` (the bundled `libjvm.so` + OpenJDK's helper
  natives + `libsqlitejdbc.so`) belong in `jniLibs/arm64-v8a` so they're page‑mapped and (with
  `extractNativeLibs`) exec/`dlopen`‑able from `nativeLibraryDir` (the only exec‑legal location under
  API 29+). The **Java class libraries / `modules` image / our engine jar** go in `assets` (or a resource
  the service copies to app storage on first run). Reading a jar from storage is fine; *executing* native
  code from storage is not.
- **APK size — the elephant.** Desktop engine jar is **174 MB**, but that includes a bundled WebUI and all
  desktop natives we don't ship on Android. Realistic Android footprint estimate (arm64‑only):
  - Minimal Android OpenJDK runtime (`jlink`'d modules + `libjvm.so`): **~40–70 MB**.
  - Suwayomi engine classes minus WebUI + minus non‑arm64 natives: **~30–60 MB**.
  - **GraalJS `js-community` (Truffle) is heavy** — tens of MB of jars — the biggest *new* cost vs desktop
    perception; keep but measure.
  - **Estimated arm64 APK: ~120–180 MB.** For calibration, TachiManga's iOS app installs in the
    **several‑hundred‑MB** range (bundled JVM + JS engine + assets), so ~150 MB for an arm64‑only Android
    build is in family. F‑Droid/APK sideload tolerate this (§12a makes Android primary). *Verify TachiManga's
    APK weight and our own via a real `tauri android build`.*
  - **Strippable:** the bundled WebUI (we never use it), non‑arm64 natives, unused JDK modules (`jlink`),
    KCEF (already `kcefEnabled=false`), desktop‑only libs.
- **minSdk:** driven by (i) the exec/`dlopen` model — **API 29+** is the meaningful floor for the W^X
  behavior we design around; (ii) the bundled OpenJDK's bionic requirements (typically API 24–26+). Target
  **minSdk 26–29**; confirm against the chosen OpenJDK build. `arm64-v8a` primary; `x86_64` only if we want
  emulator CI.

### Q6 — AGPL/MPL compliance per path

- **Whole project is AGPL‑3.0** (decided, §12). Both paths must publish Corresponding Source matching each
  distributed binary (the jar, the bundled JVM build scripts, our shims) and any *modified* engine we ship.
- **Path (b) (recommended):** we ship stock `v2.3.2243` (AGPL) unmodified + our AGPL shims + a bundled
  OpenJDK (GPLv2+CE — **classpath exception**, safe to bundle with our app) + GraalJS (`js-community`,
  UPL/EPL — permissive, compatible) + `xerial/sqlite-jdbc` (Apache‑2.0) + `dex2jar` (Apache‑2.0). If we
  harvest TachiManga's MPL `android/graphics/*` or WebView‑CF files, **each stays MPL‑2.0 with its notice**;
  MPL→AGPL one‑way compatibility means the combined work is distributable under AGPL. Cleanest posture.
- **Path (a):** adopting TachiManga's fork is fine license‑wise (MPL in an AGPL work), **but** it drags in
  their whole product tree; every modified engine file we then ship still needs Corresponding Source, and
  we'd be maintaining a large MPL/AGPL hybrid diverged from upstream. Compliance is heavier, not lighter.
- **OpenJDK bundling caveat:** use a GPLv2‑**with‑Classpath‑Exception** OpenJDK build (Temurin/Zulu/Gluon)
  so the exception covers our app; document the JDK build provenance + source offer per release.
- **Extensions:** unchanged from desktop — they execute third‑party scraper code on‑device (same trust
  model as Mihon); keep the JVM sandboxed to app storage; audit bundled extension licenses (already noted).

---

## 2. Proposed build plan (numbered work items)

Ordering: prove the JVM runs the stock jar on a device *first* (highest‑risk, device‑only), then layer the
service, transport, CF bridge, packaging, and finally decide the AWT question with data.

**N3.1 — Android OpenJDK runtime selection + headless "can the stock jar even boot" probe.**
Pick a GPL+CE arm64 OpenJDK build (Temurin/Zulu/Gluon OpenJDK‑Mobile) at the JDK version GraalJS
`js-community` requires (21+). `jlink` a minimal module set incl. a working headless `java.desktop`.
- *Acceptance:* a documented, size‑budgeted arm64 JDK artifact; a script that `jlink`s it reproducibly.
- *Verifiable headless:* boot the **stock `v2.3.2243` jar under this JDK on a Linux‑aarch64 host/emulator**,
  hit `{ aboutServer { version } }`, get `v2.3.2243`. Proves GraphQL API + dex2jar + Exposed/SQLite +
  GraalJS init on a stock JDK before any device work. *Device‑only:* the same on real hardware (N3.7).

**N3.2 — In‑process JVM launcher via JNI (`JNI_CreateJavaVM` over bundled `libjvm.so`).**
A native (Rust/C) shim that `dlopen`s the bundled `libjvm.so` from `nativeLibraryDir`, creates the VM
in‑process, and invokes `suwayomi.tachidesk.MainKt.main` with our data‑dir + ephemeral‑loopback args.
- *Acceptance:* one process, no `execve`; engine reaches `ready`; clean teardown (VM destroy) with no leak.
- *Verifiable headless:* on an aarch64 emulator, `JNI_CreateJavaVM` + boot + `aboutServer` + destroy loop.
- *Device‑only:* cold‑start latency + memory ceiling on a mid‑range phone.

**N3.3 — Foreground‑service + supervisor mapping.**
Kotlin foreground‑service plugin (persistent notification, `START_STICKY`) owning the N3.2 JVM; map our
N1 `suwayomi.rs` state machine (readiness gate, capped‑backoff restart→degraded→fallback) onto
service/VM recreate. Detach cleanly to avoid the `MainActivity`‑leak bug (tauri#11609).
- *Acceptance:* engine survives backgrounding; kill→degraded→auto‑recreate→content via fallback ladder;
  no orphaned VM on app exit; no leaked Activity on relaunch.
- *Verifiable headless:* unit‑test the state machine (as N1 did). *Device‑only:* real background/Doze survival.

**N3.4 — Transport reuse (loopback + IPC proxy).**
Point `LocalSuwayomiBackend` at the on‑device loopback via the existing `suwayomi_gql`/`suwayomi_image`
commands; add `network_security_config` allowlisting `127.0.0.1`. No JS‑layer changes expected.
- *Acceptance:* `LocalSuwayomiBackend`/`CompositeBackend`/N2 provisioning + fallback + offline queue run
  unmodified against the on‑device engine.
- *Verifiable headless:* the existing shimmed‑`invoke` integration harness re‑pointed at an aarch64‑emulator
  engine. *Device‑only:* end‑to‑end MangaDex read on hardware.

**N3.5 — SQLite Android native.**
Supply `libsqlitejdbc.so` (`natives-android/aarch64`) in `jniLibs/arm64-v8a`; set the lib path so Exposed
finds it. Confirm the version matches Suwayomi's `sqlite-jdbc`.
- *Acceptance:* engine DB opens/migrates on device; no "no native library found" error.
- *Verifiable headless:* emulator boot writes/reads the engine DB. *Device‑only:* n/a (same on device).

**N3.6 — Cloudflare WebView↔shim bridge (Android).**
Kotlin Tauri plugin exposing `solve(url)`/`getCookie(url)`/`userAgent()` over
`android.webkit.CookieManager` + a (hidden, shown‑on‑interactive) `WebView`; feed `{cf_clearance, UA}` to
the existing Rust FlareSolverr‑v1 shim; UA‑unify shim↔engine↔WebView.
- *Acceptance (device‑only):* a CF‑gated Keiyoushi source reads on‑device via WebView solve; falls back to
  server fetch on persistent challenge (§7 ladder).
- *Verifiable headless:* the shim's FlareSolverr‑v1 protocol contract + cookie‑injection path (already have
  7 tests); the WebView solve itself is device‑only.

**N3.7 — AWT decision spike (gates whether we harvest TachiManga graphics).**
Instrument the *actual* endpoints we call (page‑byte proxy, any cover/thumbnail path) and detect any
`java.awt`/`sun.font`/`ImageIO` reachability on the Android JDK.
- *Acceptance:* a written go/no‑go: (i) our paths never touch AWT → ship stock; or (ii) they do → forward‑port
  TachiManga's MPL `android/graphics/{Bitmap,BitmapFactory,Canvas,Paint,Rect,NativeRef}.java` onto
  `v2.3.2243` (keep MPL notices) and re‑verify.
- *Verifiable headless:* reachability check on emulator by exercising each endpoint. *Device‑only:* confirm
  no font/render crash on hardware for a real cover/page.

**N3.8 — Packaging + size budget + CI.**
`tauri android init`; wire jniLibs (JVM + sqlite) vs assets (jar/modules); strip WebUI + non‑arm64 +
unused JDK modules + KCEF; set minSdk (26–29); produce a signed arm64 APK; record size against a budget.
- *Acceptance:* installable signed APK within the size budget; documented minSdk rationale.
- *Verifiable headless:* CI `tauri android build` (arm64) + size gate; boot on emulator + `aboutServer`.
- *Device‑only:* install + full read on a physical device (the §5 acceptance).

**N3.9 — Compliance artifacts.**
Per‑release Corresponding Source incl. the OpenJDK build provenance/scripts, the (unmodified) `v2.3.2243`
jar + SHA, any harvested MPL files with notices, and the shim sources.
- *Acceptance:* `LICENSE`/notices updated; a reviewer can rebuild the shipped APK from published source.
- *Verifiable headless:* license/notice lint; reproducible‑build check of the artifact list.

---

## 3. Risks (likelihood × impact)

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | **AWT/Java2D unavoidable** on an Android JDK *and* our endpoints hit it | Medium | High | N3.7 decides early; fallback = harvest TachiManga's AWT‑free `android/graphics/*` (MPL, ~6 files). |
| R2 | **In‑process JVM cold‑start/perf** too slow or memory ceiling too low on mid‑range phones | Medium | High | JIT allowed on Android (unlike iOS) helps; `-Xmx` cap; measure in N3.2; Zero‑interp fallback only if needed. |
| R3 | **CF WebView replay** fails on device (UA mismatch, headless‑blocked challenge) | Medium | Medium | UA unification in N3.6; show WebView for interactive; server‑fetch fallback (§7) is the safety net. |
| R4 | **GraalJS interpreted mode** too slow / too large for JS‑heavy sources | Low‑Med | Medium | Most sources need no JS; measure; it's still a *working* pure‑JVM path (Duktape‑native is gone). |
| R5 | **APK size** (~120–180 MB) deters users / F‑Droid friction | Low | Medium | arm64‑only, `jlink`, strip WebUI/natives/KCEF; Android sideload tolerates it (§12a). |
| R6 | **Upstream `v2.3.2243` drift** — a future engine bump changes native deps (e.g. GraalJS/sqlite versions) | Low | Medium | We pin; re‑run N3.1/N3.5 probes on each bump; native‑dep audit in CI. |
| R7 | **TachiManga fork unmaintained / rebased away** from what we harvest | Low | Low | We only harvest a few stable MPL files; snapshot them; no runtime dependency on the fork. |
| R8 | **`JNI_CreateJavaVM` from bundled `libjvm.so`** linkage/OEM ROM quirks | Low‑Med | Medium | `dlopen`+`dlsym` the bundled JDK's own symbol (never system `libart`); test across a device matrix in N3.7/N3.8. |
| R9 | **Tauri Android foreground‑service** rough edges (Activity leak tauri#11609, background Tokio freeze) | Medium | Medium | Own the Kotlin service; detach cleanly; don't rely on backgrounded Tokio for the engine. |

---

## 4. Open questions needing a device or a prototype spike

1. **AWT reachability (N3.7):** do *our* used endpoints (page‑byte proxy, covers) actually invoke
   `java.awt`/`ImageIO`/`sun.font` on an Android JDK? Decides ship‑stock vs harvest‑graphics. *Device/emulator.*
2. **In‑process cold‑start + steady memory (N3.2):** `JNI_CreateJavaVM` + boot to `ready` latency and
   `-Xmx` ceiling on a mid‑range phone. *Device‑only.*
3. **CF WebView solve+replay (N3.6):** does a real Turnstile/managed‑challenge solve in an Android WebView
   yield a `cf_clearance` the engine can replay (UA‑matched)? *Device‑only, needs a gated source.*
4. **GraalJS interpreted perf/size (N3.4/N3.8):** boot cost + per‑call cost of `js-community` on a stock
   Android JDK, and its contribution to APK size. *Emulator measurable; confirm on device.*
5. **SQLite native version match (N3.5):** does Suwayomi's pinned `sqlite-jdbc` publish an Android
   `natives-android` classifier at that version, or must we override it? *Headless (dependency check).*
6. **OpenJDK build choice (N3.1):** which arm64 GPL+CE OpenJDK (Temurin vs Zulu vs Gluon Mobile) boots the
   stock jar with a working headless `java.desktop` at the smallest `jlink` size? *Headless bake‑off.*
7. **TachiManga APK weight (Q5 calibration):** confirm the real installed size of TachiManga to sanity‑check
   our ~150 MB estimate. *External measurement.*
8. **Does any Keiyoushi source we care about require ART‑only behavior** the desktop shim doesn't emulate
   (e.g. `Handler`/`Looper` timing)? TachiManga added a Robolectric‑style looper; stock `v2.3.2243` may
   already cover it — verify against our two‑tier source set. *Emulator + real sources.*

---

## 4a. Addendum — N3.1 headless acceptance: PASS (2026-07-16)

The N3.1 headless probe (`apps/reader/src-tauri/e2e/android-jdk-probe/run.sh`, findings in the
adjacent `FINDINGS.md`) ran green: **11/11 assertions, cold-start 3–4 s** — the stock `v2.3.2243`
jar boots and runs the full content path (GraphQL, extension install, forced dex2jar APK→JAR,
MangaDex browse + DB persist) on stock `eclipse-temurin:21` Linux-aarch64, no fork, no changes.
Two premise corrections to this doc:

- **N3.5 is moot for this pin:** the embedded DB is **H2 (pure-Java)** — the jar bundles no
  `sqlite-jdbc` and needs **no native DB `.so`** on Android. (Contingency verified: if Suwayomi ever
  adopts sqlite-jdbc, the Android aarch64 native ships inside the ordinary main jar at
  `org/sqlite/native/Linux-Android/aarch64/`; there is no separate `natives-android` classifier.)
- **The common extension-install path skips dex2jar entirely:** the Keiyoushi `repo` index publishes
  a pre-built `jarUrl`, so store installs download the converted jar (0 `d2j` class-loads); dex2jar
  runs only on the raw-apk path (`installExternalExtension`) — which the probe forced and proved
  (235 converter class-loads). Both paths work on the stock JDK.
- **Early AWT signal (indicative, N3.7 still required):** 0 `java.awt`/`sun.font`/`javax.imageio`
  class-loads across 16.8k total loads for boot + install + convert + browse; image-decode/cover
  endpoints not exercised.

## 5. Sources

- TachiManga fork (MPL‑2.0, parent Suwayomi): <https://github.com/tachimanga/Tachidesk-Server> — verified
  via `gh api` (license, parent, branches `master`+`fix/classloader`, tags mirror upstream, compare diff).
- Suwayomi extension management (DEX→JVM via dex2jar, URLClassLoader):
  <https://deepwiki.com/Suwayomi/Suwayomi-Server/4.1-extension-management>
- Upstream `v2.3.2243` deps (`dex2jar 2.4.37`, GraalJS `js-community` "Substitute for duktape-android",
  `okhttp 5.4.0`, jsoup 1.22.2), and GraphQL package present: `gh api …?ref=v2.3.2243`.
- GraalVM native‑image closed‑world / no runtime classloading:
  <https://www.graalvm.org/latest/reference-manual/native-image/metadata/Compatibility/>, oracle/graal#461.
- xerial sqlite‑jdbc Android natives: <https://github.com/xerial/sqlite-jdbc> (USAGE.md), PR #662.
- Tauri v2 mobile plugins / Kotlin `@Command` / JNI while WebView suspended:
  <https://v2.tauri.app/develop/plugins/develop-mobile/>, discussion tauri#10695.
- `tauri-plugin-background-service` (Android foreground service model, backgrounded‑Tokio freeze):
  <https://crates.io/crates/tauri-plugin-background-service>; Activity‑leak bug tauri#11609.
- Android API‑29 exec‑from‑home‑dir ban (W^X): developer.android.com behavior‑changes‑10; termux‑app#1072.
- `JNI_CreateJavaVM` in‑process on Android (use bundled libjvm, not system libart):
  calebfenton "Creating a Java VM from Android Native Code".
- `android.webkit.CookieManager` (global cookie store, Mihon CF harvest):
  <https://developer.android.com/reference/android/webkit/CookieManager>
- OpenJDK Mobile (iOS Zero interpreter context; Android permits JIT):
  <https://openjdk.org/projects/mobile/ios.html>, InfoQ "Running Java on iOS" (Gluon OpenJDK‑Mobile, 2025).
