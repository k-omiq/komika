#!/usr/bin/env bash
#
# N4.2: stage the iOS in-process JVM runtime into the generated Xcode project.
#
# WHY THIS EXISTS: the statically-linked HotSpot (Zero) derives java.home ITSELF as
# "<app executable dir>/lib" (os_bsd.cpp __IOS__ branch + is_vm_statically_linked()),
# and the boot jimage path (<java.home>/lib/modules) is fixed BEFORE -D options are
# parsed, so -Djava.home cannot relocate it. The runtime therefore CANNOT live under
# tauri's iOS resource dir (Komika.app/assets/...) — it must sit at the APP BUNDLE
# ROOT as Komika.app/lib/. That is achieved with an XcodeGen folder reference
# (gen/apple/project.yml: sources entry "jvm-runtime/lib"), and this script builds
# the folder it references:
#
#   gen/apple/jvm-runtime/lib/
#     lib/modules      <- the class-library jimage (~149 MB)
#     lib/tzdb.dat
#     lib/security/
#     lib/jfr/
#     lib/jvm.cfg
#     conf/
#     release
#
# (i.e. bundle path Komika.app/lib/lib/modules == <java.home>/lib/modules with
# java.home = Komika.app/lib. The *.a static libs are NOT staged — they are
# link-time inputs, consumed via build.rs.)
#
# gen/apple is gitignored and regenerable (`tauri ios init`), and a regeneration
# wipes BOTH this staging and the hand-added project.yml entries. Re-run this
# script after any regeneration; it verifies project.yml carries the required
# entries and fails with guidance if not. APFS clones (cp -c) keep it instant.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_TAURI_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

JRE="${SRC_TAURI_DIR}/jre-ios/aarch64-ios"
APPLE="${SRC_TAURI_DIR}/gen/apple"
STAGE="${APPLE}/jvm-runtime/lib"

[[ -f "${JRE}/lib/modules" ]] || {
  echo "error: ${JRE}/lib/modules missing — build the N4.1 runtime first (scripts/build-ios-jvm.sh)" >&2
  exit 1
}
[[ -d "${APPLE}" ]] || {
  echo "error: ${APPLE} missing — run 'pnpm exec tauri ios init' first" >&2
  exit 1
}

rm -rf "${STAGE}"
mkdir -p "${STAGE}/lib"
cp -c "${JRE}/lib/modules" "${STAGE}/lib/modules"
cp -c "${JRE}/lib/tzdb.dat" "${STAGE}/lib/tzdb.dat"
cp -c "${JRE}/lib/jvm.cfg" "${STAGE}/lib/jvm.cfg"
cp -Rc "${JRE}/lib/security" "${STAGE}/lib/security"
cp -Rc "${JRE}/lib/jfr" "${STAGE}/lib/jfr"
cp -Rc "${JRE}/conf" "${STAGE}/conf"
cp -c "${JRE}/release" "${STAGE}/release"

# The project.yml entries this staging depends on (hand-maintained because
# gen/apple is generated + gitignored; `tauri ios init` regenerates WITHOUT them):
#   sources:      - path: jvm-runtime/lib   (folder reference -> Komika.app/lib)
#   OTHER_LDFLAGS: -Wl,-force_load,<libapp.a> -lz -liconv -lc++
#   dependencies: CoreFoundation.framework, Foundation.framework
#   DEAD_CODE_STRIPPING: false
MISSING=0
for needle in "jvm-runtime/lib" "force_load" "CoreFoundation.framework"; do
  grep -q "${needle}" "${APPLE}/project.yml" || {
    echo "error: gen/apple/project.yml lacks '${needle}' — re-apply the N4.2 project.yml edits (see docs/plans/n4-ios-spike.md and this script's header), then run 'xcodegen generate' in gen/apple" >&2
    MISSING=1
  }
done
[[ "${MISSING}" == "0" ]] || exit 1

SIZE_MB=$(( $(du -sk "${STAGE}" | awk '{print $1}') / 1024 ))
echo "ok: staged iOS JVM runtime at ${STAGE} (${SIZE_MB} MB; bundle path Komika.app/lib)"
