#!/usr/bin/env bash
#
# Build an interpreter-only (Zero, NO-JIT — legal on iOS) OpenJDK runtime for iOS arm64,
# capable of hosting the stock Suwayomi-Server jar in-process via the JNI Invocation API.
#
# WHY BUILD, NOT FETCH (N4.1 findings): no clean, general-purpose prebuilt iOS-arm64 Zero
# `libjvm` exists to download. Gluon publishes only GraalVM-AOT / Substrate artifacts
# (download2.gluonhq.com/mobile) which AOT-compile ONE fixed app and CANNOT host an
# arbitrary stock jar in interpreter mode. PojavLauncher's iOS JVMs rely on JIT (jailbreak/
# debugger) and are not interpreter-only. So the only reproducible, license-clean path is to
# build the HotSpot Zero interpreter from OpenJDK's official Mobile Project. See
# docs/plans/n4.1-ios-jvm-findings.md and openjdk/mobile README "Build static image for iOS".
#
# OUTPUT: a static-libs image at ../jre-ios/aarch64-ios/ (gitignored, never committed):
#   lib/zero/libjvm.a           <- the interpreter VM
#   lib/*.a                     <- static JDK native libs (libjava.a, libnio.a, libnet.a,
#                                  libzip.a, ...) linked into the iOS app binary
#   (plus the modules/classes runtime the launcher points java.home at — see N4.2 notes)
# On iOS these are STATIC libraries: the Rust/Xcode link step statically links them into the
# app binary and calls JNI_CreateJavaVM directly (iOS forbids app-loaded .dylibs for this).
#
# REQUIREMENTS (hard):
#   * Apple-Silicon macOS + Xcode (iPhoneOS SDK). Validated on Xcode 26.6 / iOS SDK 26.5.
#   * A Boot JDK whose feature version is N-1, N, or N-2 of openjdk/mobile HEAD. As of the
#     pinned commit, HEAD is JDK 28-dev, so the boot JDK MUST be 26, 27, or 28. The desktop
#     recipe's JDK 21 is REJECTED by configure ("Boot JDK version must be one of: 26 27 28").
#     Point BOOT_JDK at a suitable install (e.g. Temurin 26 GA). This script will fetch a
#     pinned Temurin boot JDK if BOOT_JDK is unset and KOMIKA_FETCH_BOOT_JDK=1.
#   * autoconf (brew install autoconf) — OpenJDK's configure needs it.
#   * The Gluon "mobile-support" zip (prebuilt iOS libffi + cups headers), fetched + verified
#     below. Zero needs libffi to call native functions (it has no template interpreter).
#
# Usage:
#   ./scripts/build-ios-jvm.sh                     # uses $BOOT_JDK (must be JDK 26/27/28)
#   BOOT_JDK=/path/to/jdk-26 ./scripts/build-ios-jvm.sh
#   KOMIKA_FETCH_BOOT_JDK=1 ./scripts/build-ios-jvm.sh   # auto-fetch pinned Temurin 26
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ---- Pins (supply-chain: never float; verify SHA where we can) -----------------------------
# openjdk/mobile commit that has the iOS `static-libs-image` target (JDK-8346233) and builds
# the Zero interpreter static libjvm. Bump deliberately; a bump likely changes the required
# boot-JDK feature version (see REQUIREMENTS above).
#
# !!! PIN WARNING (N4.1 finding) !!!  mainline HEAD is NOT a green iOS build point. openjdk/mobile
# auto-merges jdk:master, and mainline's hotspot os/bsd layer has drifted to macOS-only libproc
# APIs (`#include <libproc.h>`, proc_pidinfo, proc_pidpath) with NO iOS guards, which the iPhoneOS
# SDK lacks. Building the commit below reaches configure + compiles the Zero interpreter, then
# FAILS at `os_bsd.cpp:114 'libproc.h' file not found`. You MUST pin an earlier openjdk/mobile
# commit where the iOS static-libs build was validated (≈2025-01, JDK-25 era, matching the README
# + the 20250106 support zip — boot JDK would then be 24/25) OR carry an iOS port patch series.
# The commit below is the verified-reproducible FAILURE point, not a shippable pin.
MOBILE_REPO="https://github.com/openjdk/mobile.git"
MOBILE_COMMIT="feb2de9b27dee5315d67448b4959beaaa77006a2"   # JDK 28-dev HEAD; reaches Zero compile then fails os/bsd (see warning)

# Gluon-published prebuilt iOS libffi + cups (headers/static libs) referenced by the
# openjdk/mobile README. SHA-256 verified before use.
SUPPORT_URL="https://download2.gluonhq.com/mobile/mobile-support-20250106.zip"
SUPPORT_SHA256="5793dd8700612fe0c5a1cbbfb280464167aea5944b3c01f32b340542976e9440"

# Optional auto-fetch boot JDK (Temurin 26 GA, macOS aarch64). Only used when
# KOMIKA_FETCH_BOOT_JDK=1 and BOOT_JDK is unset.
BOOT_JDK_URL="https://github.com/adoptium/temurin26-binaries/releases/download/jdk-26.0.1%2B8/OpenJDK26U-jdk_aarch64_mac_hotspot_26.0.1_8.tar.gz"
BOOT_JDK_SHA256=""   # set to pin; left blank => download without SHA gate (fill in when known)

CONF_NAME="ios-aarch64-zero-release"
OUT_DIR="${SRC_TAURI_DIR}/jre-ios/aarch64-ios"
WORK_DIR="${KOMIKA_IOS_JVM_WORKDIR:-${SRC_TAURI_DIR}/.ios-jvm-build}"   # large; keep out of git

# ---- Preconditions -------------------------------------------------------------------------
[[ "$(uname -s)" == "Darwin" ]] || { echo "error: iOS JVM must be built on macOS" >&2; exit 1; }
command -v autoconf >/dev/null 2>&1 || { echo "error: autoconf missing (brew install autoconf)" >&2; exit 1; }

SYSROOT="$(xcrun --sdk iphoneos --show-sdk-path)"
[[ -d "${SYSROOT}" ]] || { echo "error: iPhoneOS SDK not found (install Xcode)" >&2; exit 1; }
echo "iPhoneOS sysroot: ${SYSROOT}"

compute_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}
verify_sha256() {
  local file="$1" want="$2" got
  got="$(compute_sha256 "${file}")"
  if [[ "${got}" != "${want}" ]]; then
    echo "error: SHA-256 mismatch for ${file}" >&2
    echo "  expected: ${want}" >&2
    echo "  actual:   ${got}" >&2
    return 1
  fi
}

mkdir -p "${WORK_DIR}"

# ---- Boot JDK ------------------------------------------------------------------------------
if [[ -z "${BOOT_JDK:-}" && "${KOMIKA_FETCH_BOOT_JDK:-0}" == "1" ]]; then
  echo "fetching pinned Temurin 26 boot JDK"
  BOOT_TGZ="${WORK_DIR}/boot-jdk.tar.gz"
  curl -fSL --retry 3 -o "${BOOT_TGZ}" "${BOOT_JDK_URL}"
  [[ -n "${BOOT_JDK_SHA256}" ]] && verify_sha256 "${BOOT_TGZ}" "${BOOT_JDK_SHA256}"
  rm -rf "${WORK_DIR}/boot-jdk"; mkdir -p "${WORK_DIR}/boot-jdk"
  tar xzf "${BOOT_TGZ}" -C "${WORK_DIR}/boot-jdk"
  BOOT_JDK="$(dirname "$(dirname "$(find "${WORK_DIR}/boot-jdk" -name java -path '*/bin/java' | head -1)")")"
fi
[[ -n "${BOOT_JDK:-}" ]] || { echo "error: set BOOT_JDK to a JDK 26/27/28 install, or pass KOMIKA_FETCH_BOOT_JDK=1" >&2; exit 1; }
[[ -x "${BOOT_JDK}/bin/java" ]] || { echo "error: no bin/java under BOOT_JDK=${BOOT_JDK}" >&2; exit 1; }
echo "boot JDK: $("${BOOT_JDK}/bin/java" -version 2>&1 | head -1)"

# ---- iOS libffi + cups support -------------------------------------------------------------
SUPPORT_ZIP="${WORK_DIR}/mobile-support.zip"
if [[ ! -f "${SUPPORT_ZIP}" ]] || ! verify_sha256 "${SUPPORT_ZIP}" "${SUPPORT_SHA256}" 2>/dev/null; then
  echo "fetching iOS libffi/cups support"
  curl -fSL --retry 3 -o "${SUPPORT_ZIP}" "${SUPPORT_URL}"
  verify_sha256 "${SUPPORT_ZIP}" "${SUPPORT_SHA256}"
fi
SUPPORT_DIR="${WORK_DIR}/support"
rm -rf "${SUPPORT_DIR}"; mkdir -p "${SUPPORT_DIR}"
unzip -q -o "${SUPPORT_ZIP}" -d "${SUPPORT_DIR}"
# The archive nests a top-level "support/" dir; locate the real one containing libffi/.
SUP="$(dirname "$(find "${SUPPORT_DIR}" -type d -name libffi | head -1)")"
[[ -d "${SUP}/libffi/include" && -d "${SUP}/libffi/libs" ]] || { echo "error: libffi not found in support zip" >&2; exit 1; }
echo "support: ${SUP}"

# ---- Clone/checkout openjdk/mobile at the pin ----------------------------------------------
MOBILE_DIR="${WORK_DIR}/mobile"
if [[ ! -d "${MOBILE_DIR}/.git" ]]; then
  echo "cloning openjdk/mobile @ ${MOBILE_COMMIT}"
  git clone "${MOBILE_REPO}" "${MOBILE_DIR}"
fi
git -C "${MOBILE_DIR}" fetch --depth 1 origin "${MOBILE_COMMIT}" 2>/dev/null || git -C "${MOBILE_DIR}" fetch origin
git -C "${MOBILE_DIR}" checkout -q "${MOBILE_COMMIT}"

# ---- Configure (Zero is the default JVM variant for the ios target) ------------------------
echo "configuring ${CONF_NAME}"
( cd "${MOBILE_DIR}" && bash configure \
    --disable-warnings-as-errors \
    --openjdk-target=aarch64-macos-ios \
    --with-boot-jdk="${BOOT_JDK}" \
    --with-libffi-include="${SUP}/libffi/include" \
    --with-libffi-lib="${SUP}/libffi/libs" \
    --with-cups-include="${SUP}/cups-2.3.6" \
    --with-sysroot="${SYSROOT}" )

# ---- Build the static-libs image -----------------------------------------------------------
echo "building static-libs-image (this is a full JDK cross-build; expect tens of minutes)"
( cd "${MOBILE_DIR}" && make CONF="${CONF_NAME}" static-libs-image )

# ---- Collect artifacts into the vendored bundle dir ----------------------------------------
IMG="${MOBILE_DIR}/build/${CONF_NAME}/images/static-libs"
LIBJVM="${IMG}/lib/zero/libjvm.a"
[[ -f "${LIBJVM}" ]] || { echo "error: libjvm.a not produced at ${LIBJVM}" >&2; exit 1; }

rm -rf "${OUT_DIR}"; mkdir -p "${OUT_DIR}"
cp -R "${IMG}/lib" "${OUT_DIR}/lib"
# The class-library runtime (java.home) the JVM boots against: prefer the jlink/jmods image
# from the same build so classes match the static libs. Copied if present.
if [[ -d "${MOBILE_DIR}/build/${CONF_NAME}/images/jdk" ]]; then
  cp -R "${MOBILE_DIR}/build/${CONF_NAME}/images/jdk/lib/modules" "${OUT_DIR}/lib/modules" 2>/dev/null || true
fi

# ---- Sanity: confirm the mach-o is arm64 iOS -----------------------------------------------
echo "=== libjvm.a arch/platform ==="
lipo -info "${LIBJVM}" || true
TMP_OBJ_DIR="$(mktemp -d)"
( cd "${TMP_OBJ_DIR}" && ar -x "${LIBJVM}" 2>/dev/null; OBJ="$(ls *.o 2>/dev/null | head -1)"; \
  [[ -n "${OBJ}" ]] && vtool -show-build "${OBJ}" 2>&1 | grep -iE 'platform|IPHONEOS|minos' || true )
rm -rf "${TMP_OBJ_DIR}"

SIZE_MB=$(( $(du -sk "${OUT_DIR}" | awk '{print $1}') / 1024 ))
echo "ok: iOS Zero JVM static-libs image at ${OUT_DIR} (${SIZE_MB} MB)"
echo "NOTE: on-device in-process boot of Suwayomi-Server.jar via JNI_CreateJavaVM is N4.2"
echo "      (device + JNI link work); it cannot be exercised on the mac host."
