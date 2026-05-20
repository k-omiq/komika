#!/usr/bin/env bash
#
# Build a minimal JRE for the embedded Suwayomi-Server sidecar via jlink.
#
# Output lands at ../jre/<slug>/ where <slug> matches Rust's std::env::consts pattern
# "<ARCH>-<OS>" (e.g. aarch64-macos) — exactly the path suwayomi.rs probes at runtime
# (<resource_dir>/jre/<slug>/bin/java). Only the host-target JRE is built per invocation;
# each platform's CI build produces and bundles its own.
#
# Requires JDK 21 with jlink. Defaults to Homebrew's openjdk@21; override with JAVA_HOME.
#
# Usage:  ./scripts/build-jre.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@21}"
JLINK="${JAVA_HOME}/bin/jlink"
if [[ ! -x "${JLINK}" ]]; then
  echo "error: jlink not found at ${JLINK}" >&2
  echo "  set JAVA_HOME to a JDK 21 install (has bin/jlink)." >&2
  exit 1
fi

# Derive the target slug the way suwayomi.rs does: Rust ARCH + Rust OS.
uname_m="$(uname -m)"
case "${uname_m}" in
  arm64 | aarch64) ARCH="aarch64" ;;
  x86_64 | amd64)  ARCH="x86_64" ;;
  *) echo "error: unsupported arch '${uname_m}'" >&2; exit 1 ;;
esac
uname_s="$(uname -s)"
case "${uname_s}" in
  Darwin)             OS="macos" ;;
  Linux)              OS="linux" ;;
  MINGW* | MSYS* | CYGWIN* | Windows_NT) OS="windows" ;;
  *) echo "error: unsupported OS '${uname_s}'" >&2; exit 1 ;;
esac
SLUG="${ARCH}-${OS}"
OUT_DIR="${SRC_TAURI_DIR}/jre/${SLUG}"

echo "building minimal JRE"
echo "  JAVA_HOME: ${JAVA_HOME}"
echo "  target:    ${SLUG}"
echo "  output:    ${OUT_DIR}"

# jlink refuses to write into an existing directory.
rm -rf "${OUT_DIR}"
mkdir -p "$(dirname "${OUT_DIR}")"

# Module set for Suwayomi-Server: it runs Tachiyomi extensions and needs desktop (awt is
# loaded even headless), sql (H2), xml, scripting (Nashorn/JS via extensions), crypto,
# HTTP client, logging, management, naming, and instrumentation. This curated set was
# validated to boot the pinned jar (see the #[ignore]d sidecar_boots integration test).
# If a future Suwayomi release needs more, widen here (fallback: --add-modules ALL-MODULE-PATH).
MODULES="java.base,java.desktop,java.sql,java.sql.rowset,java.naming,java.management,java.instrument,java.logging,java.net.http,java.scripting,java.security.jgss,java.security.sasl,java.xml,java.xml.crypto,java.transaction.xa,java.datatransfer,java.compiler,java.rmi,java.prefs,jdk.crypto.ec,jdk.crypto.cryptoki,jdk.unsupported,jdk.zipfs,jdk.net,jdk.httpserver,jdk.management,jdk.jsobject,jdk.dynalink,jdk.charsets,jdk.localedata,jdk.security.auth,jdk.security.jgss"

"${JLINK}" \
  --add-modules "${MODULES}" \
  --strip-debug \
  --no-header-files \
  --no-man-pages \
  --compress=zip-9 \
  --output "${OUT_DIR}"

# jlink writes module legal notices read-only (mode 444). Tauri's bundler copies this
# tree into target/<profile>/jre and, on a later rebuild, `fs::copy` fails to overwrite
# those read-only files ("Permission denied"). Make the whole output user-writable so
# every re-copy succeeds.
chmod -R u+w "${OUT_DIR}"

# Sanity: the launcher the supervisor probes must exist.
JAVA_BIN="${OUT_DIR}/bin/java"
[[ "${OS}" == "windows" ]] && JAVA_BIN="${OUT_DIR}/bin/java.exe"
if [[ ! -x "${JAVA_BIN}" ]]; then
  echo "error: expected launcher missing after jlink: ${JAVA_BIN}" >&2
  exit 1
fi

# Report size and enforce a hard ceiling.
SIZE_KB="$(du -sk "${OUT_DIR}" | awk '{print $1}')"
SIZE_MB=$(( SIZE_KB / 1024 ))
echo "JRE built: ${SIZE_MB} MB (uncompressed on disk) at ${OUT_DIR}"

HARD_CEILING_MB=150
ASPIRATIONAL_MB=60
if (( SIZE_MB > HARD_CEILING_MB )); then
  echo "error: JRE is ${SIZE_MB} MB, exceeds hard ceiling of ${HARD_CEILING_MB} MB" >&2
  exit 1
fi
if (( SIZE_MB > ASPIRATIONAL_MB )); then
  echo "note: ${SIZE_MB} MB exceeds the aspirational ~${ASPIRATIONAL_MB} MB target; future module thinning could shrink it." >&2
fi

echo "ok: minimal JRE for ${SLUG} ready"
