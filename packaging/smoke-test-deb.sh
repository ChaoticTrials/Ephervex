#!/usr/bin/env bash
# Checks a built Ephervex Debian package before anyone is asked to install it.
#
# Everything works on an *extracted* copy, so the check needs no root, installs
# nothing, and can run in a container. The application is exercised against
# throwaway XDG roots, so a smoke test can never read or write the developer's
# own configuration.
#
# This is not the AppImage's smoke test with different paths. The two artifacts
# have different failure modes: an AppImage is one file that must not read
# anything outside itself, while a package is a tree of installed paths plus the
# control metadata a package manager reads before it will install anything
# (ADR 0026).
#
#   usage: packaging/smoke-test-deb.sh <artifact.deb>
set -euo pipefail

fail() {
    printf 'smoke-test-deb: %s\n' "$*" >&2
    exit 1
}

note() { printf '  - %s\n' "$*"; }

# Checked rather than assumed: `strings` missing would turn the build-machine
# path check into a silent no-op, which is worse than not running it.
for tool in readelf strings python3 dpkg-deb; do
    command -v "${tool}" >/dev/null || fail "${tool} is required and was not found"
done

[[ $# -eq 1 ]] || fail "usage: $0 <artifact.deb>"
ARTIFACT=$(cd -- "$(dirname -- "$1")" && printf '%s/%s' "$(pwd)" "$(basename -- "$1")")
[[ -f ${ARTIFACT} ]] || fail "${ARTIFACT} is not a file"

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=packaging/linux-baseline
source "${REPO_ROOT}/packaging/linux-baseline"
# shellcheck source=packaging/linux-release-artifact
source "${REPO_ROOT}/packaging/linux-release-artifact"

WORKSPACE=$(mktemp -d)
cleanup() { rm -rf "${WORKSPACE}"; }
trap cleanup EXIT

printf '==> smoke testing %s\n' "$(basename "${ARTIFACT}")"

# --- extract -----------------------------------------------------------------
ROOT="${WORKSPACE}/root"
mkdir -p "${ROOT}"
dpkg-deb --extract "${ARTIFACT}" "${ROOT}" || fail "the package payload could not be extracted"
CONTROL=$(dpkg-deb --field "${ARTIFACT}") || fail "the package has no readable control fields"

# --- control metadata --------------------------------------------------------
field() { printf '%s\n' "${CONTROL}" | sed -n "s/^$1: //p" | head -n 1; }

VERSION=$("${REPO_ROOT}/packaging/workspace-version.sh")
[[ $(field Package) == "ephervex" ]] || fail "the package is not named ephervex"
[[ $(field Version) == "${VERSION}" ]] ||
    fail "the package says version $(field Version), the workspace says ${VERSION}"
for name in Architecture Maintainer Description Depends Section Priority; do
    [[ -n $(field "${name}") ]] || fail "the control file has no ${name}"
done
# Nothing is bundled, so the C runtime is always a dependency. Its absence means
# the resolution produced something, but not something real.
printf '%s\n' "${CONTROL}" | grep -q 'libc6' ||
    fail "Depends does not mention libc6, so it was not resolved from the binaries"
note "control metadata names the package, its version, and its dependencies"

# --- the payload is exactly what was meant to ship ---------------------------
# An allowlist by prefix rather than a file count, so adding an icon size is not
# a test failure while a stray file still is.
mapfile -t PAYLOAD < <(cd "${ROOT}" && find . -type f -printf '%P\n' | sort)
[[ ${#PAYLOAD[@]} -gt 0 ]] || fail "the package is empty"
for entry in "${PAYLOAD[@]}"; do
    case "${entry}" in
    usr/bin/ephervex | usr/bin/ephervex-gui) ;;
    usr/share/applications/ephervex.desktop) ;;
    usr/share/applications/ephervex-url-handler.desktop) ;;
    usr/share/metainfo/ephervex.metainfo.xml) ;;
    usr/share/doc/ephervex/copyright) ;;
    usr/share/icons/hicolor/*/apps/ephervex.png) ;;
    *) fail "the package ships ${entry}, which is not part of the declared payload" ;;
    esac
done
note "${#PAYLOAD[@]} files, every one of them declared"

for required in \
    usr/bin/ephervex \
    usr/bin/ephervex-gui \
    usr/share/applications/ephervex.desktop \
    usr/share/metainfo/ephervex.metainfo.xml \
    usr/share/doc/ephervex/copyright \
    usr/share/icons/hicolor/256x256/apps/ephervex.png; do
    [[ -f "${ROOT}/${required}" ]] || fail "the package is missing ${required}"
done
for binary in usr/bin/ephervex usr/bin/ephervex-gui; do
    [[ -x "${ROOT}/${binary}" ]] || fail "${binary} is not executable"
done
note "both binaries are installed, at real paths, executable"

# --- the desktop entry points at what the package installs -------------------
# The AppImage dispatches on the name it was invoked under, so its entry names a
# binary nothing installs. A package installs one, and an entry that named
# something else would put a launcher in the menu that does not start.
DESKTOP="${ROOT}/usr/share/applications/ephervex.desktop"
EXEC=$(sed -n 's/^Exec=//p' "${DESKTOP}" | head -n 1)
[[ -n ${EXEC} ]] || fail "the desktop entry has no Exec"
[[ -x "${ROOT}/usr/bin/${EXEC%% *}" ]] ||
    fail "the desktop entry runs ${EXEC}, which this package does not install"
if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "${DESKTOP}" || fail "the desktop entry is not valid"
fi
note "the desktop entry runs a binary the package installs"

# --- the package claims the link scheme --------------------------------------
# Installing the package is what makes this acceptable, and it is the whole
# reason a link from a web page reaches Ephervex without a further step
# (ADR 0027). An entry that claimed the scheme but named no `%u` would be
# handed nothing, which looks exactly like a link that does not work.
HANDLER="${ROOT}/usr/share/applications/ephervex-url-handler.desktop"
[[ -f ${HANDLER} ]] || fail "the package installs no link handler"
grep -q '^MimeType=x-scheme-handler/ephervex;$' "${HANDLER}" ||
    fail "the link handler does not claim the ephervex scheme"
grep -q '^Exec=.* %u$' "${HANDLER}" || fail "the link handler is never handed the link"
grep -q '^NoDisplay=true$' "${HANDLER}" ||
    fail "the link handler would appear as a second Ephervex in the menu"
HANDLER_EXEC=$(sed -n 's/^Exec=//p' "${HANDLER}" | head -n 1)
[[ -x "${ROOT}/usr/bin/${HANDLER_EXEC%% *}" ]] ||
    fail "the link handler runs ${HANDLER_EXEC}, which this package does not install"
# The application's own entry must stay out of it: it takes no arguments, and
# claiming a scheme there would hand a URL to something that ignores it.
grep -q '^MimeType=' "${DESKTOP}" &&
    fail "the application entry must not claim a scheme; the handler entry does"
note "the package claims ephervex:// and is handed the link"

# --- the declared baseline, on what actually shipped -------------------------
REQUIRED_GLIBC=0
enforce_glibc_baseline "${ROOT}/usr/bin/ephervex"
enforce_glibc_baseline "${ROOT}/usr/bin/ephervex-gui"
note "glibc floor ${REQUIRED_GLIBC}, at or below the declared baseline ${EPHERVEX_BASELINE_GLIBC}"

enforce_shared_library_allowlist "${REPO_ROOT}/packaging" \
    "${ROOT}/usr/bin/ephervex" "${ROOT}/usr/bin/ephervex-gui"
note "shared libraries are the declared set"

enforce_no_build_paths "${ROOT}/usr/bin/ephervex" "${ROOT}/usr/bin/ephervex-gui"
note "no build-machine home paths are embedded"

# --- the command-line interface ----------------------------------------------
XDG="${WORKSPACE}/xdg"
mkdir -p "${XDG}"/{config,data,cache,state,tmp}
# Deliberately *not* the XDG parent. Anything that resolves a location from
# $HOME instead of the XDG variables then lands somewhere the check at the end
# can see, rather than blending into the roots it was supposed to use.
SANDBOX_HOME="${WORKSPACE}/home"
mkdir -p "${SANDBOX_HOME}"

run_cli() {
    env -i \
        HOME="${SANDBOX_HOME}" \
        PATH="/usr/bin:/bin" \
        XDG_CONFIG_HOME="${XDG}/config" \
        XDG_DATA_HOME="${XDG}/data" \
        XDG_CACHE_HOME="${XDG}/cache" \
        XDG_STATE_HOME="${XDG}/state" \
        TMPDIR="${XDG}/tmp" \
        EPHERVEX_LOG=error \
        "${ROOT}/usr/bin/ephervex" "$@"
}

run_cli --help >/dev/null || fail "\`ephervex --help\` failed"
run_cli --version >/dev/null || fail "\`ephervex --version\` failed"
note "the command-line interface answers --help and --version"

INFO=$(run_cli --json info) || fail "\`ephervex info\` failed"
printf '%s' "${INFO}" | grep -q '"app_id"' || fail "\`info --json\` is not the expected JSON"
note "info reports its build identity"

# The bundled datasets are compiled into the binaries, so this is what proves a
# release artifact carries them rather than depending on the source tree.
HEALTH=$(run_cli --json doctor) || fail "\`ephervex doctor\` failed"
python3 - "${HEALTH}" <<'PY' || fail "the health report is not what a first run looks like"
import json, sys
health = json.loads(sys.argv[1])
assert health["bundled_data"]["options_compatibility"]["status"] == "loaded", health["bundled_data"]
assert health["bundled_data"]["options_compatibility"]["entries"] > 0
assert health["bundled_data"]["preference_catalog"]["status"] == "loaded", health["bundled_data"]
assert health["bundled_data"]["preference_catalog"]["entries"] > 0
assert health["paths_error"] is None, health["paths_error"]
for directory in health["directories"]:
    assert directory["state"] in ("absent", "writable"), directory
PY
note "doctor loads the bundled datasets and reports a clean first run"

# --- no source tree needed ---------------------------------------------------
# Run from a directory that is not the repository, to prove nothing resolves
# back into it.
(cd / && run_cli --json info >/dev/null) || fail "the package needs its source tree"
note "nothing resolves back into the source tree"

# --- nothing outside the XDG roots -------------------------------------------
# The same rule the AppImage is held to: the application owns the XDG roots and
# writes nowhere else (ADR 0003). A package installs into system paths, but what
# it does at runtime is unchanged.
escaped=$(find "${SANDBOX_HOME}" -mindepth 1 -print 2>/dev/null | head -n 5 || true)
[[ -z ${escaped} ]] || {
    printf '%s\n' "${escaped}" >&2
    fail "the application wrote outside the XDG roots"
}
note "nothing was written outside the XDG roots"

printf '==> %s looks publishable\n' "$(basename "${ARTIFACT}")"
