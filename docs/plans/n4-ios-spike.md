# N4-SPIKE — iOS feasibility (research only; UNCOMMITTED)

> **Scope.** Feasibility research for **Phase 4 (iOS)** of `native-embedded-suwayomi.md`.
> No implementation, no dependency changes. This doc gathers evidence and proposes a gating
> prototype; nothing is built until reviewed.
> **Companion refs:** `native-embedded-suwayomi.md` §0a (status), §6 (Phase 4), §8b (Cloudflare
> on-device), §12a (distribution — decided: no App Store). Whole project is AGPL-3.0.

---

## 1. Executive summary + verdict

**Verdict: FEASIBLE, and cheaper than the plan feared — recommend proceeding to a single gating
device prototype. Confidence: high on the two hardest-looking blockers (extensions, Cloudflare),
medium on JVM cold-start/latency, which only a device measurement can settle.**

The plan (§6, §16) frames iOS as the hardest phase, with two suspected blockers: (a) running a JVM
under iOS's no-JIT rule, and (b) executing Android/dex extensions on iOS. Research collapses both:

1. **Extension execution is a non-issue on iOS specifically.** The feared "hardest blocker" (Q2) is
   **already solved by stock Suwayomi-Server** and is not iOS-specific at all. Suwayomi converts each
   Tachiyomi extension **APK → JAR via `dex2jar`** at install time, then loads the resulting *standard
   JVM bytecode* through a `ChildFirstClassLoader`, backed by an `AndroidCompat` shim
   (`CustomContext implements android.content.Context`). There is **no Dalvik/ART on the device** — the
   engine never runs dex; it runs ordinary JVM classes. This path is the *same* path Komika's desktop
   sidecar (Phase 1, already built) uses today. It carries to iOS unchanged the moment a JVM boots.
   dex2jar is pure-Java (no native codegen), so it runs in interpreter mode.

2. **JVM-on-iOS is a solved, shipping technique.** OpenJDK's official **Mobile Project** builds the
   HotSpot **Zero interpreter** (pure C++, zero runtime-generated assembly → no W^X, no JIT) for iOS
   arm64. **TachiManga already ships stock-lineage Suwayomi on the iOS App Store** doing exactly this
   (their server is a `Java`-language fork of `Suwayomi/Suwayomi-Server`, parent confirmed via GitHub
   API). PojavLauncher/Amethyst run a full OpenJDK on iOS for Minecraft, further proving in-process JVM
   viability. So Komika does **not** need a novel port — it needs to adopt the same recipe.

3. **Cloudflare-on-device ports directly.** iOS `WKHTTPCookieStore.getAllCookies` is a real, working
   API (unlike Tauri-**Android**'s empty cookie API noted in §0a/N-CF), so the N-CF loopback
   FlareSolverr-shim + platform-WebView solver design (already built for desktop) reads `cf_clearance`
   back out of `WKWebView` and replays it through the engine. TachiManga confirms the pattern works on
   iOS App Store builds ("automatically bypassing Cloudflare", Mobile-Safari UA in WebView).

4. **§6 ↔ §12a reconciliation.** §6's acceptance line "app passes App Store review constraints" is
   **moot** given §12a's decided *no App Store*. But the sub-constraint it bundled — **no JIT** — is
   **not** an App-Review policy; it is a kernel-enforced platform rule (W^X, no distributable
   `get-task-allow`). It therefore **still binds on every realistic Komika channel** (self-build,
   AltStore, EU AltStore-PAL). **Interpreter-only remains mandatory.** Avoiding the App Store *does*
   dissolve the AGPL×Apple-ToS conflict (that conflict is about Apple's ToS adding restrictions atop
   the GPL, not about JIT). Net binding constraints: **no-JIT (always)** + **code-signing (every
   channel)**; App-Review and private-API rules stop binding for self-sideload.

**Effort classification:** *Recipe reuse, not fork, not from-scratch.* TachiManga's server is
source-available but its iOS deltas are squashed into opaque "update"/"buildNNN" commits, so it is a
**reference, not a drop-in dependency** (and its app is proprietary). The reusable, openly-documented
pieces are all upstream: OpenJDK-mobile (Zero) + stock Suwayomi's existing dex2jar/AndroidCompat +
WebKit. The single gating milestone (§6) — *any JVM running the server in-process on a device* — is a
days-scale spike, not a phase.

---

## 2. Evidence per question

### Q1 — Interpreter-only JVM on iOS (how TachiManga does it; latency & memory)

**How:** iOS forbids apps from generating executable machine code at runtime (no writeable+executable
pages, no distributable JIT). OpenJDK's answer is the **Zero interpreter** — a HotSpot variant written
in pure C++ with *no* platform assembly and *no* template interpreter, so it never emits runtime code
and is legal on iOS. This is the OpenJDK **Mobile Project** path (official downstream of `openjdk/jdk`).

- OpenJDK Mobile / iOS details: <https://openjdk.org/projects/mobile/ios.html> ·
  <https://openjdk.org/projects/mobile/> · <https://github.com/openjdk/mobile> ·
  <https://openjdk-mobile.github.io/>
- Zero-Assembler project: <https://openjdk.org/projects/zero/>
- 2025 tooling maturity (Gluon publishing OpenJDK-mobile build pipelines for iOS; explicitly notes
  "Apple does not allow runtime-generated assembly … rules out a JIT compiler," and the strategy of
  **Zero + AOT (Project Leyden) methods** to claw back speed):
  <https://www.infoq.com/news/2025/11/java-on-ios/>
- Prior art that a full JVM boots in-process on iOS: PojavLauncher_iOS, Amethyst-iOS
  (<https://github.com/AngelAuraMC/Amethyst-iOS>), and the 2017 "OpenJDK 8 on iOS, on the App Store"
  writeup (<https://medium.com/@thebaselab/how-i-got-openjdk-8-running-on-ios-and-releasing-on-app-store-56a7619c6452>).

**TachiManga evidence (concrete):** `github.com/tachimanga/Tachidesk-Server` — GitHub API confirms
`language: Java`, `parent: Suwayomi/Suwayomi-Server`, actively pushed (2026-07-15). Branches: `master`
(the shipping fork; commits squashed to `update` / `vX.Y buildNNN`) and **`fix/classloader`** (tracks
upstream Suwayomi + `ChildFirstClassLoader (#1873)` / "Fix child first classloader" — i.e. the
extension-isolation classloader, directly relevant to loading converted extension jars). TachiManga is
live on the App Store: <https://apps.apple.com/ng/app/tachimanga/id6447486175>. This is existence-proof
that *stock-lineage Suwayomi + interpreter JVM* passes even the strict App-Store bar — a bar Komika
doesn't need to clear.

**Latency penalty (could-not-verify precisely — no published Suwayomi-on-Zero numbers):** Zero is an
interpreter-only VM; as a rule of thumb interpreter-only Java runs ~**3–10×** slower than JIT'd HotSpot
on compute-bound work, but Suwayomi's per-request work is **I/O-bound** (HTTP scrape + HTML/JSON parse),
so wall-clock is dominated by network, not interpretation. Cold start is the real cost: classloading +
dex2jar + Kotlin-runtime init are interpreted. Desktop cold start is `<30 s` (Komika Gate C, §0a); on
Zero this will be **worse and must be measured** — this is the primary unknown the gate exists to
resolve. Project-Leyden AOT caching (per the InfoQ piece) is the mitigation lever if it's too slow.

**Memory (jetsam vs `-Xmx512m`):** iOS kills a process that exceeds a per-process "jetsam" resident
ceiling. Concrete data points found: iPhone 12 mini (4 GB RAM) fatal at **`ActiveHard 2098 MB`**
(<https://developer.apple.com/forums/thread/688973>); community teardown of an iPhone 14 jetsam report ≈
**2.05 GB**; 2 GB devices are the danger zone, 4 GB+ comfortable
(<https://github.com/PojavLauncherTeam/PojavLauncher_iOS/issues/97>). Apple's own guidance:
<https://developer.apple.com/documentation/xcode/identifying-high-memory-use-with-jetsam-event-reports>.
**Implication:** desktop's `-Xmx512m` heap + JVM/interpreter overhead + WebView (WKWebView is a separate
process, so its memory doesn't count against the app's jetsam limit — a plus) fits within a ~2 GB
foreground ceiling on any ≥4 GB iPhone. **Cap tighter on iOS** (e.g. `-Xmx256m`, plus `-Xss` tuning)
and treat pre-A-series-with-4 GB devices as unsupported. **App Extensions** (share/widget) have a far
lower ceiling (~tens of MB) — never run the engine in an extension, only in the main app.

### Q2 — Extension loading on iOS (the feared blocker — dissolved)

**Finding:** No iOS-specific extension work is required. Stock Suwayomi's install pipeline (confirmed
via DeepWiki architecture docs) is: **download APK → verify signature (`apksig`) → parse
(`apk-parser`) → `dex2jar` DEX→JVM-bytecode → load JAR into a (child-first) ClassLoader**, with an
`AndroidCompat` layer (`CustomContext implements android.content.Context`, `SharedPreferences`,
`Bitmap`/`BitmapFactory` shims — see recent upstream commits `Bitmap: Allow pixel-based access`,
`BitmapFactory: Support basic options`, `Update dex2jar to v2.4.34`). Extensions therefore run as
**ordinary JVM classes**; **Dalvik/ART is never involved on the device.**

- Extension management: <https://deepwiki.com/Suwayomi/Suwayomi-Server/4.1-extension-management> ·
  system: <https://deepwiki.com/Suwayomi/Suwayomi-Server/4-extension-system> ·
  architecture: <https://deepwiki.com/Suwayomi/Suwayomi-Server/2-architecture>

**Why this ports to iOS for free:** dex2jar and the classloader are pure Java/Kotlin — no native code
generation — so they execute under Zero exactly as on desktop. Komika's Phase-1/2 on-device extension
provisioning (N2.1, already built) drives the *same* GraphQL install path. The `.apk` that TachiManga
users add via repos is fed through this same converter; TachiManga keeping `.apk` as the extension
format (per their repo docs) is consistent with reusing stock Suwayomi's converter rather than
re-implementing it. **Residual risk (low):** classloader edge cases under interpreter mode /
child-first isolation — which is precisely what upstream's `ChildFirstClassLoader` and TachiManga's
`fix/classloader` branch address; watch these.

### Q3 — Tauri v2 iOS shell; in-process JVM thread; loopback servers; background

- **`tauri ios init` state:** Tauri 2.0 stable ships iOS support; the iOS app is a Swift/Xcode shell
  hosting **WKWebView** for the frontend with **Rust compiled in-process** for commands/plugins. Refs:
  <https://v2.tauri.app/blog/tauri-20/> · <https://v2.tauri.app/develop/> ·
  2026 iOS guide <https://viadreams.cc/en/blog/tauri-guide/>.
- **Long-running in-process JVM thread:** viable — Rust runs in-process, and JNI (`libjvm` via the
  Invocation API) can create a JVM on a dedicated native thread that lives for the app's foreground
  lifetime. No separate "service" process is needed or possible. Community/plugin evidence for
  long-lived background work on Tauri-iOS: `tauri-plugin-background-service`
  (<https://docs.rs/tauri-plugin-background-service> / <https://crates.io/crates/tauri-plugin-background-service>)
  and discussion <https://github.com/tauri-apps/tauri/discussions/11688>.
- **No background service concept:** correct and load-bearing. iOS suspends the app shortly after
  backgrounding; there is **no** always-on service. The JVM engine can only run while foregrounded
  (plus a short `beginBackgroundTask` window ≈ 30 s, or `BGTaskScheduler`/`BGProcessingTask` slices
  that iOS grants opportunistically — not guaranteed, not real-time). **Implication for downloads
  (§9):** on-device Suwayomi downloads must be **chunked + resumable + checkpointed**, expect to be
  suspended mid-chapter, and resume on next foreground; do **not** promise reliable
  background/overnight downloading. `BGProcessingTask` (needs `UIBackgroundModes` +
  `BGTaskSchedulerPermittedIdentifiers` in Info.plist) can opportunistically extend it, not replace it.
- **Loopback HTTP server in-app:** allowed. Binding `127.0.0.1:<ephemeral>` inside the app is permitted
  on iOS (no `NSLocalNetworkUsageDescription` prompt for pure-loopback — that prompt is for LAN/mDNS,
  which we must avoid). This matches §13's loopback-only posture. **But** the plan's preferred desktop
  transport is the **IPC-proxy** (`suwayomi_gql` command, §3.3b) rather than the WebView `fetch`ing the
  port — that carries to iOS and is *better* there (tight CSP, port never exposed). Recommend the same
  on iOS: JVM binds loopback for its own internal needs, JS↔engine goes over Tauri IPC.

### Q4 — Cloudflare via WKWebView (cookie readability, UA consistency)

- **Cookie read confirmed:** `WKHTTPCookieStore` exposes `getAllCookies(_:)`, `setCookie`, `delete`
  (iOS 11+). <https://developer.apple.com/documentation/webkit/wkhttpcookiestore>. So after the WebView
  solves the challenge, the shim can harvest `cf_clearance` + host cookies and inject them into the
  engine's OkHttp jar (exactly the N-CF design). **This is the key contrast with Tauri-Android**, whose
  cookie API returns empty (noted §0a/N-CF) — **iOS does not have that problem.**
- **Gotchas:** cookie completion handlers can silently not fire unless a **shared `WKProcessPool`** is
  used across WebViews; `getAllCookies` is async and must be called on the main thread
  (<https://medium.com/appssemble/wkwebview-and-wkcookiestore-in-ios-11-5b423e0829f8> ·
  <https://developer.apple.com/forums/thread/131931>). Bake a single shared process pool into the
  solver WebView.
- **UA consistency:** Cloudflare binds `cf_clearance` to the exact UA that solved it, so the engine
  must send the **same** UA as `WKWebView`. TachiManga's changelog explicitly "uses Mobile Safari's
  user agent by default" and added a "Clear cookies" WebView button
  (<https://tachimanga.app/docs/changelogs.html>) — evidence the UA-match discipline is real and
  workable. **Action:** read `WKWebView`'s effective UA (`evaluateJavaScript("navigator.userAgent")`)
  and pin it into the engine's HTTP client per host, rather than assuming a static string. §8b's
  "WebView UA and engine UA must match" requirement holds on iOS and is satisfiable.

### Q5 — Distribution constraints given §12a (no App Store)

**Does no-JIT still bind without the App Store? YES.** JIT on iOS requires the `get-task-allow`
entitlement *plus* a debugger/JIT-enabler attaching at launch — it is **not** available to
independently-distributed apps:

- AltStore AltJIT needs a desktop AltServer on the same Wi-Fi each launch; SideStore/StikDebug need
  per-launch pairing; iOS 26.x "broke JIT once again." Refs:
  <https://faq.altstore.io/altstore-classic/enabling-jit> ·
  <https://faq.altstore.io/altstore-classic/enabling-jit/altjit> ·
  <https://docs.sidestore.io/docs/advanced/jit> · <https://github.com/StephenDev0/StikDebug>.
- TrollStore *can* launch with JIT, **but** only on exploit-eligible iOS versions (roughly ≤ 17.0,
  device/version-gated) — not a general audience channel: <https://zeejb.com/updates/trollstore/>.
- **EU DMA marketplaces (AltStore PAL / web distribution):** apps are still **Apple-notarized** and run
  with **standard entitlements → no JIT**. The DMA opened *distribution*, not the JIT rule.

**Conclusion: interpreter-only (Zero) is mandatory for Komika across every channel we'd actually use.**
JIT is only reachable in a developer's own debug builds on their own registered devices — useful for
*profiling the gate*, never for shipping.

**Signing / notarization per channel:**

| Channel | Signing | Lifetime / limits | Notarization | JIT | AGPL fit |
|---|---|---|---|---|---|
| **Self-build** (user's own free Apple ID) | personal team | **7-day** re-sign, 3 apps/device | none | no | clean — user builds from public source |
| **$99 dev / ad-hoc** (dev+testers) | paid team, UDID-registered | ~1 yr, **100 devices/yr** | none | dev builds only, own devices | clean (private dev/test) |
| **AltStore (free)** | personal team, auto re-sign via AltServer | 7-day auto-refresh | none | AltJIT (needs desktop, not shipped) | clean |
| **AltStore PAL (EU DMA)** | marketplace dist. cert | ~1 yr | **Apple notarization required** | no | *needs legal check* — notarization + Core-Technology-Fee terms are lighter than App Review but still Apple ToS |
| **TestFlight** | Apple | 90-day | **App Review** | no | **AVOID** — reintroduces App-Store AGPL conflict |
| **TrollStore / jailbreak** | ad-hoc/self | permanent on eligible iOS | none | yes (version-gated) | clean but niche |

**AGPL × App Store — does avoiding it dissolve the tension? YES.** The FSF's App-Store GPL enforcement
turns on Apple's **Usage Rules ToS** layering extra restrictions (per-device install caps, DRM) atop
the GPL's grant — a conflict for *App-Store distribution*
(<https://www.fsf.org/blogs/licensing/more-about-the-app-store-gpl-enforcement> ·
<https://en.wikipedia.org/wiki/GNU_General_Public_License>). Distributing the **`.ipa` directly / via
self-build / AltStore** carries no such conflicting ToS, so **AGPL is clean** on Komika's chosen
channels — consistent with §12a's rationale. **One caveat to flag:** the EU **AltStore PAL** path still
routes through Apple **notarization**; whether that notarization + marketplace terms re-introduce a
GPL-style restriction is a **legal question, not a technical one** — treat PAL as "nice-to-have, pending
legal review," and keep **self-build + AltStore(free)** as the guaranteed-clean primary iOS channels.

### Q6 — Effort / verdict / cheapest gate

**Effort = recipe reuse (medium), not fork (high) and not from-scratch port (very high).** Ordered by
where the work actually is:
1. Build **OpenJDK-mobile (Zero) for iOS arm64** and get it booting in-process (the gate). — *the real
   risk*
2. Wire it under **Tauri iOS** as a JNI-launched engine thread + loopback + IPC transport. — mechanical
3. Port the **N-CF WebView bridge** to `WKWebView` cookie harvest + UA pin. — design already exists
4. Packaging/signing for self-build + AltStore. — mechanical
5. Extensions, composite backend, offline queue, fallback ladder — **already built, carry unchanged**
   (§0a: "client layers above the JVM transport carry over unchanged").

TachiManga is a **reference** (proprietary app + obfuscated server deltas), not a dependency to fork.
Don't fork it; adopt its *technique* from the open upstreams (OpenJDK-mobile, stock Suwayomi, WebKit).

**Recommendation (high confidence): green-light the gating prototype below before scoping Phase 4.**
Do not write any Phase-4 product code until the gate produces a measured cold-start + per-request
latency on real hardware. If the gate's cold start is unacceptable even with AOT caching, iOS drops to
"engine-optional: server-fetch fallback only" (the §7 ladder already makes the app usable without a
local engine) — that is the graceful degradation, not a dead end.

---

## 3. IF feasible — work items (gated on the prototype in §5 passing)

> All items are **behind the existing `PUBLIC_KOMIKA_NATIVE_ENGINE` flag** and the iOS build target;
> none touch web/desktop/Android behavior. "Device-only" = must be verified on physical iPhone
> hardware (no headless CI equivalent); "Headless" = automatable in CI or on a Mac without a device.

- **N4.1 — OpenJDK-mobile (Zero) iOS arm64 runtime artifact.** Produce a reproducible build of an
  interpreter-only JVM (`libjvm` + minimal class library, ideally `jlink`-trimmed like desktop §3.1)
  for iOS arm64, vendored + SHA-pinned like the desktop jar/JRE.
  *Acceptance:* `HelloWorld` and `java -version`-equivalent run in-process on a physical device;
  artifact size recorded against a budget. *Verify: **device-only** (boot) + **headless** (build/size).*
- **N4.2 — In-process engine launcher (Tauri iOS).** JNI Invocation-API launch of Suwayomi v2.3.2243
  on a dedicated native thread from the Rust core; loopback bind; readiness gate; graceful stop tied to
  iOS lifecycle (`applicationWillResignActive`/`DidEnterBackground` → pause/checkpoint; foreground →
  resume). Reuse `suwayomi.rs` supervisor semantics.
  *Acceptance:* `suwayomi_status` reaches `ready`; `aboutServer` answers over the IPC transport;
  backgrounding does not orphan or crash the JVM; foreground resumes. *Verify: **device-only.***
- **N4.3 — Cold-start + latency budget gate.** Instrument N4.2 to log cold-start ms and per-request ms
  for a MangaDex chapter-list + first-page fetch; evaluate `-Xmx`/`-Xss` caps against jetsam; try
  Project-Leyden/AOT class caching if cold start > budget.
  *Acceptance:* documented numbers on ≥2 device tiers (e.g. iPhone 12-class 4 GB and a current 8 GB);
  a go/no-go against an agreed budget (proposed: cold start ≤ 20 s warm-cache, ≤ 45 s first-ever;
  per-page added latency ≤ 500 ms over network). *Verify: **device-only.***
- **N4.4 — Extension provisioning on device.** Confirm the existing N2.1 install path (dex2jar +
  ChildFirstClassLoader) works under Zero for MangaDex + one non-CF Keiyoushi source.
  *Acceptance:* first open of an un-provisioned work installs + reads; classloader isolation holds; a
  bogus repo falls back to server (§7). *Verify: **device-only** (install/read) + **headless** (unit
  contract already exists).*
- **N4.5 — WKWebView Cloudflare bridge (N-CF port).** Implement the solver against `WKWebView` with a
  shared `WKProcessPool`, `getAllCookies` harvest, and UA read-back pinned into the engine per host;
  reuse the loopback FlareSolverr-v1 shim protocol unchanged.
  *Acceptance:* a live CF-gated source solves in the (hidden, then shown-for-CAPTCHA) WebView and the
  engine replays `cf_clearance` successfully; UA matches; failure falls back to server-fetch.
  *Verify: **device-only** (needs a real display + live challenge) + **headless** (shim protocol tests
  already exist under `apps/reader/src-tauri/e2e/`).*
- **N4.6 — iOS image path.** Confirm `NativeImageProvider` engine-proxy branch streams bytes over IPC
  on iOS (the `suwayomi_image` command equivalent); MangaDex `fetch_image` path parity.
  *Acceptance:* pages paint from engine `/api/v1/...` proxy bytes on device. *Verify: **device-only.***
- **N4.7 — Packaging + signing (self-build + AltStore).** `tauri ios build` → signable `.ipa`;
  document the self-build-from-source flow (user's Apple ID) and AltStore re-sign; interpreter-JVM
  artifact codesigned; no `get-task-allow`/JIT entitlement; audit for private-API use (matters if EU
  PAL/notarization is later pursued). *Acceptance:* a tester installs via $99 profile and via
  AltStore-free; app launches and reads. *Verify: **device-only** (install) + **headless** (build).*
- **N4.8 — Offline/download lifecycle for iOS suspension.** Make on-device downloads resumable +
  checkpointed across suspension; wire `BGProcessingTask` opportunistically; never promise unattended
  background downloads. *Acceptance:* a download interrupted by backgrounding resumes on foreground
  without corruption. *Verify: **device-only.***

---

## 4. Risks

- **R1 — Cold-start under Zero (medium/high).** No published Suwayomi-on-Zero benchmark; interpreter
  classloading + Kotlin runtime init + dex2jar could push cold start well past desktop's 30 s. *Mitigate:*
  measure early (N4.3), AOT/Leyden class caching, pre-warm on app launch, keep engine alive across
  foreground sessions. *Fallback:* iOS ships "engine-optional" using the §7 server-fetch ladder.
- **R2 — TachiManga is a closed reference (medium).** Their server iOS deltas are squashed/obfuscated
  and the app is proprietary; we cannot copy their code, only re-derive from open upstreams. Budget
  independent OpenJDK-mobile build/debug effort.
- **R3 — Zero classloader edge cases (low/medium).** Child-first isolation of dynamically-converted
  extension jars under interpreter mode; upstream `ChildFirstClassLoader` mitigates but must be verified
  on device (N4.4).
- **R4 — Memory on 4 GB devices (low).** ~2 GB foreground jetsam ceiling; safe with `-Xmx256m` and
  WKWebView out-of-process, but heavy sources or large images could spike. *Mitigate:* tight heap caps,
  stream images, don't decode full-res in-process.
- **R5 — iOS lifecycle vs downloads (medium).** No real background service; interrupted downloads are
  the norm. *Mitigate:* N4.8 checkpoint/resume; set user expectations.
- **R6 — EU PAL / notarization AGPL ambiguity (low, legal).** Keep self-build + AltStore(free) as the
  clean primary; treat PAL as pending legal review.
- **R7 — Apple platform churn (ongoing).** iOS releases periodically break sideload/JIT tooling; since
  Komika is interpreter-only and doesn't depend on JIT tooling, exposure is limited to the signing/
  install channels, not the runtime.
- **R8 — Extension-signature/`apksig` under Zero (low).** Signature verification is pure-Java; expected
  fine, but validate in N4.4.

---

## 5. The gating prototype (smallest experiment)

**Goal (verbatim from §6):** *"a spike to get ANY JVM running the server in-process on a device is the
gating milestone."* Nothing more — no Tauri, no WebView bridge, no UI.

**Exact experiment (two steps, cheapest-first):**
1. **Pre-gate (hours, no device dependency risk):** Build **OpenJDK-mobile Zero for iOS arm64**; run a
   trivial in-process `System.out`/`HelloWorld` on a **physical iPhone** via a throwaway Xcode host app
   (or the OpenJDK-mobile sample harness). Proves the toolchain + on-device boot.
2. **The gate (days):** In the same throwaway host, `System.load` the JVM, launch the **stock
   Suwayomi-Server v2.3.2243 jar** in-process on a background thread with `-Djava.awt.headless=true`,
   `kcefEnabled=false`, `-Xmx256m`, bound to `127.0.0.1:<ephemeral>`. From native code (or a loopback
   HTTP hit) issue `POST /api/graphql { aboutServer { version } }` and then one **MangaDex
   `fetchSourceManga` + chapter-list** call (installing the MangaDex extension via the engine's own
   GraphQL first, to also exercise dex2jar on-device). **Log:** cold-start ms, first-GraphQL ms,
   chapter-list ms, peak RSS, and whether jetsam fires.

**Pass criteria:** `aboutServer` returns a version; a real chapter list comes back; peak RSS stays under
the device's jetsam ceiling; cold start is within a "tolerable with optimization" range (record the raw
number — the go/no-go threshold is decided from the data, but sanity bar ≈ ≤ 60 s first-ever boot).

**What it needs (hardware/accounts):**
- 1 × Apple-Silicon **Mac** with current **Xcode** (for the OpenJDK-mobile build + on-device deploy).
- 1 × physical **iPhone**, A-series, **≥ 4 GB RAM** (iPhone 12 or newer recommended; avoid 2–3 GB
  devices for the first measurement). A second, newer 8 GB device is ideal for a two-tier latency read.
- A **free Apple ID** suffices for a 7-day dev-signed build (a **$99 Apple Developer** account makes
  iteration far less painful — 1-year builds, no 7-day churn — and is already assumed by §12a for
  dev/test). No paid account needed to *prove feasibility*.
- The already-vendored **Suwayomi-Server v2.3.2243 jar** + the MangaDex extension coordinates the client
  already hardcodes (§0a).

**Explicitly NOT in the gate:** Tauri integration, the WKWebView CF bridge, the composite backend, UI,
signing-for-distribution, downloads. Those are N4.x, unlocked only if the gate passes.

---

## 6. Open questions

- **OQ1:** What is the *actual* measured Suwayomi cold-start and per-request latency under Zero on iOS?
  (Only the gate answers this. Everything downstream hinges on it.)
- **OQ2:** Does TachiManga use OpenJDK-mobile specifically, a custom Zero build, or an AOT-assisted
  variant? (Circumstantial evidence is strong — `Java` fork of Suwayomi, on the App Store, so
  interpreter-only — but no dev statement was found; their commits are squashed. Not required to
  proceed, but would de-risk N4.1.)
- **OQ3:** How much does Project-Leyden / AOT class caching actually buy for cold start on Zero, and is
  it mature enough on the iOS mobile branch in 2026? (InfoQ signals it's the intended lever.)
- **OQ4:** Precise per-device jetsam MB table for current iPhones (only scattered points found:
  4 GB → ~2.05–2.10 GB). Establish a supported-device floor.
- **OQ5:** Legal — do EU AltStore-PAL notarization + Core-Technology-Fee terms re-introduce an
  AGPL-incompatible restriction, or is PAL AGPL-clean like direct sideload?
- **OQ6:** Does dynamic extension classloading (dex2jar output + `ChildFirstClassLoader`) have any
  interpreter-mode-only failure modes on iOS? (Validate in N4.4; upstream `fix/classloader` suggests
  active attention in this area.)
- **OQ7:** WKWebView UA read-back stability across iOS versions, and interactive-Turnstile UX when the
  challenge needs a user tap (same open item flagged for N-CF on desktop).

---

## 7. Plan reconciliation notes (for the reviewer)

- **§6 acceptance line "app passes App Store review constraints (no JIT, no private API)" should be
  amended:** given §12a (no App Store), *App-Review* and *private-API* constraints **stop binding** for
  self-sideload/AltStore; **no-JIT still binds** (it's a kernel rule, not a review rule) → keep
  interpreter-only. Private-API avoidance re-enters only if EU **notarization** (PAL/TestFlight) is
  pursued.
- **§6/§16 "iOS extension execution" is over-weighted as a blocker:** it is solved by stock Suwayomi's
  dex2jar/AndroidCompat and is not iOS-specific. Re-rank it from "hardest blocker" to "low risk, verify
  in gate."
- **§8b ports cleanly to iOS** — `WKHTTPCookieStore.getAllCookies` exists (unlike Tauri-Android). The
  N-CF shim design is reusable; only the WebView driver changes.
- **§9 downloads must be re-scoped for iOS suspension** (no background service) — add resumable/
  checkpointed semantics (N4.8).
