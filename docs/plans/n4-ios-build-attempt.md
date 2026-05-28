# N4 — iOS build attempt (reader shell on device toolchain)

> **What this is:** an opportunistic build attempt run when a physical iPhone was wired up, to
> see how far the iOS toolchain gets *today* — **before** any N4.x implementation. It is **not**
> the N4 gate (booting the stock jar in-process via a Zero JVM); that JVM artifact does not exist
> yet. This proves the **iOS packaging/toolchain path for the reader shell** (partial N4.7) and
> yields real size numbers. Companion: `n4-ios-spike.md` (feasibility), `native-embedded-suwayomi.md`
> §6/§12a. Machine: macOS arm64, Xcode 26.6, Tauri CLI 2.11.4. Date: 2026-07-16.

## Result: iOS shell builds, signs, and **installs on the physical iPhone**. Launch pending one on-device "trust developer" tap.

**Update (device install completed):** after the Apple ID was added to Xcode, the signed device build
succeeded and installed on a physical **iPhone 13 Pro (iOS 26.5)**. Sequence that worked:
- Correct **Team ID is `KYA5U323MC`** (the cert's OU / org "Wtf Wegovy") — *not* `5X397B2JPK`, which is
  the parenthetical in the cert common-name and is **not** the team id. (This cost two failed builds.)
- CLI automatic signing does **not** auto-register a device. Fix: one direct
  `xcodebuild -destination 'platform=iOS,id=<udid>' -allowProvisioningUpdates -allowProvisioningDeviceRegistration build`
  registered the device + minted the profile (that standalone run then fails at the Rust script phase —
  expected, it lacks tauri's CLI socket — but registration sticks). Then a plain
  `tauri ios build --export-method debugging` produced `gen/apple/build/arm64/Komika.ipa`, installed via
  `xcrun devicectl device install app`.
- **Last gate (user, on-device):** first launch is denied — *"profile has not been explicitly trusted by
  the user."* Trust it on the phone: **Settings → General → VPN & Device Management → [the Apple-ID
  developer profile] → Trust**. Then it launches. This is an on-device security action an assistant
  cannot perform.
- **`.ipa` size = 218 MB (still wrong):** the `tauri.ios.conf.json` `resources:[]` override did **not**
  strip the desktop jar/JRE for the device build — the resource-copy phase is baked into the Xcode
  project at the *first* `tauri ios init` and re-init doesn't regenerate it. Real shell is still < 10 MB;
  a clean iOS bundle needs a fresh `gen/apple` generated with the override present (or an explicit build
  phase removal) — folded into N4.2 packaging.

## (Original) Result: iOS shell builds, packages, launches, and runs the frontend on iOS.

### What was proven (verifiable, reproduced)

1. **The full native `src-tauri` crate compiles for `aarch64-apple-ios` (real device arch).**
   `cargo check --target aarch64-apple-ios-sim` is clean, and `tauri ios build` (device target)
   compiled the whole crate + ran the Rust build phase, reaching the code-signing step — i.e. the
   sidecar modules (`suwayomi.rs`, `cloudflare.rs`) that use `tokio::process::Command` and
   `WebviewWindowBuilder` **do compile on iOS** (they would fail at *runtime* under the iOS sandbox,
   not at build). No `#[cfg(desktop)]` gating was needed just to compile.
2. **The SvelteKit frontend builds and packages into an iOS `.app`.** `tauri ios init` scaffolded
   `gen/apple/komika.xcodeproj` (xcodegen); `tauri ios build -t aarch64-sim --no-sign` produced
   `gen/apple/build/arm64-sim/Komika.app`.
3. **The app installs, launches, and the WebView loads the frontend on iOS 26.5.** Installed +
   launched on the iPhone 17 Pro simulator (PID stayed alive, not a crash): WebKit reported
   `firstMeaningfulPaint=3.08s`, `domContentLoaded`, `loadEvent`, all subresources finished, and the
   SvelteKit router ran a client-side navigation. WebContent idle at ~32 MB / 0.3% CPU. The screen is
   blank because it's the reader's **empty state** — no hosted backend was reachable (the build baked
   the dev `PUBLIC_KOMIKA_API=localhost:8080`, not running) — **not** a load failure.

### Sizes (measured)

| Component | Size | Note |
|---|---|---|
| iOS main binary `Komika` (arm64) | **7.2 MB** | Rust core + Tauri, iOS device/sim arch |
| **iOS shell total (binary + frontend, no engine)** | **< 10 MB** | the real Tauri-iOS footprint |
| Desktop jar wrongly bundled | 166 MB | `assets/suwayomi/Suwayomi-Server.jar` — a macOS artifact |
| Desktop JRE wrongly bundled | 62 MB | `assets/jre/aarch64-macos/` — **macOS mach-o**, invalid on iOS |
| `.app` as built (with the above) | 235 MB | inflated; the 228 MB engine payload must be excluded from iOS |

The measured `.app` is 235 MB **only** because `bundle.resources: ["suwayomi/","jre/"]` in
`tauri.conf.json` is global and copies the desktop engine into every target. On iOS those bytes are
useless (macOS binaries) and must be excluded; the real shell is **< 10 MB**, and a functional iOS
build's size will be shell + the (unbuilt) iOS Zero-JVM artifact — sized by the N4 gate, not here.

### The two gates that remain before an on-device install

1. **Code-signing needs an Apple ID signed into Xcode (user action — not automatable here).**
   The keychain has a valid *certificate* (`Apple Development: …@icloud.com`, team `5X397B2JPK`), but
   Xcode has **no Account** for that team and **no provisioning profile** for `app.komika.reader`, so
   automatic signing can't mint a development profile for the wired device. Fixing this means signing
   into the Apple ID in Xcode → Settings → Accounts, which is a **credential action for the repo owner
   to perform**; an assistant must not enter Apple ID credentials. Once signed in, `tauri ios build`
   (with `bundle.iOS.developmentTeam` set) auto-generates the profile and installs to the device.
2. **The desktop jar/JRE must be excluded from the iOS bundle.** They are macOS binaries; Xcode may
   also reject them at codesign. Fold this into N4.2 packaging — e.g. a platform-specific
   `tauri.ios.conf.json` overriding `bundle.resources` to `[]` until the iOS Zero-JVM artifact exists
   to take their place. (An inline `--config` override at build time does **not** work: the
   resource-copy phase is baked into the Xcode project at `tauri ios init` time.)

### Follow-ups this surfaced for N4.x (not blockers to the shell build)

- **`#[cfg(desktop)]`-gate the sidecar for mobile (N4.2).** At runtime on iOS, `.setup()` →
  `suwayomi::start` will try to spawn `java` from the (macOS) JRE, fail, and the supervisor will
  thrash on capped-backoff restart while the JS fallback ladder routes to hosted. Compiles fine, but
  mobile should get a no-op/stub engine transport until the in-process JVM (N4.2) lands.
- **Frontend needs a mobile backend config.** The blank empty-state is expected here; a real device
  build must bake a reachable `PUBLIC_KOMIKA_API` (and the CSP `connect-src` currently allows only
  `localhost:8080`).

### Reproduce

```bash
# cwd: /Users/caved/dev/komika/apps/reader   (Xcode + iOS sim required)
pnpm exec tauri ios init                                    # scaffold gen/apple (gitignored)
pnpm exec tauri ios build -t aarch64-sim --no-sign --ci     # -> gen/apple/build/arm64-sim/Komika.app
SIM=$(xcrun simctl list devices | grep -m1 'iPhone 17 Pro (' | grep -oE '[0-9A-F-]{36}')
xcrun simctl boot "$SIM"; xcrun simctl install "$SIM" gen/apple/build/arm64-sim/Komika.app
xcrun simctl launch "$SIM" app.komika.reader
xcrun simctl io "$SIM" screenshot /tmp/komika-ios.png
```

Device build (after the owner signs into Xcode with their Apple ID):
`TAURI_APPLE_DEVELOPMENT_TEAM=5X397B2JPK pnpm exec tauri ios build --export-method debugging`
then `xcrun devicectl device install app --device <udid> <path-to.ipa>`.

### Working-tree note

`gen/apple/**` is gitignored (Tauri default). The only tracked change left uncommitted is
`tauri.conf.json` adding `bundle.iOS.developmentTeam = "5X397B2JPK"` — kept so the owner can finish
the device install without re-adding it; not committed (iOS impl is review-gated per the Wave-E plan).
