#!/usr/bin/env bash
# Checks a built Ephervex RPM package before anyone is asked to install it.
#
# Everything works on an *extracted* copy, so the check needs no root, installs
# nothing, and can run in the build container. The application is exercised
# against throwaway XDG roots, so a smoke test can never read or write the
# developer's own configuration.
#
# The Debian package's test with different paths would be the wrong test. What
# an RPM gets wrong is its own: the architecture and release fields a package
# manager matches on, the license text under /usr/share/licenses that
# `rpm -qL` reads, and dependencies produced by RPM's generator rather than by
# dpkg-shlibdeps. What it shares with the other artifacts is the baseline, which
# is one implementation in packaging/linux-release-artifact.
#
#   usage: packaging/smoke-test-rpm.sh <artifact.rpm>
set -euo pipefail

fail() {
    printf 'smoke-test-rpm: %s\n' "$*" >&2
    exit 1
}

note() { printf '  - %s\n' "$*"; }

# Checked rather than assumed: `strings` missing would turn the build-machine
# path check into a silent no-op, which is worse than not running it.
for tool in readelf strings python3 rpm rpm2cpio cpio; do
    command -v "${tool}" >/dev/null || fail "${tool} is required and was not found"
done

[[ $# -eq 1 ]] || fail "usage: $0 <artifact.rpm>"
ARTIFACT=$(cd -- "$(dirname -- "$1")" && printf '%s/%s' "$(pwd)" "$(basename -- "$1")")
[[ -f ${ARTIFACT} ]] || fail "${ARTIFACT} is not a file"

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=packaging/linux-baseline
source "${REPO_ROOT}/packaging/linux-baseline"
# shellcheck source=packaging/linux-architectures
source "${REPO_ROOT}/packaging/linux-architectures"
ephervex_require_known_arch "${EPHERVEX_BASELINE_ARCH}"
# shellcheck source=packaging/linux-release-artifact
source "${REPO_ROOT}/packaging/linux-release-artifact"

WORKSPACE=$(mktemp -d)
cleanup() { rm -rf "${WORKSPACE}"; }
trap cleanup EXIT

printf '==> smoke testing %s\n' "$(basename "${ARTIFACT}")"

# --- extract -----------------------------------------------------------------
ROOT="${WORKSPACE}/root"
mkdir -p "${ROOT}"
(cd "${ROOT}" && rpm2cpio "${ARTIFACT}" | cpio --quiet -idm) ||
    fail "the package payload could not be extracted"

# --- header metadata ---------------------------------------------------------
field() { rpm -qp --queryformat "%{$1}" "${ARTIFACT}" 2>/dev/null; }

VERSION=$("${REPO_ROOT}/packaging/workspace-version.sh")
RPM_ARCH=$(ephervex_rpm_arch "${EPHERVEX_BASELINE_ARCH}")
[[ $(field NAME) == "ephervex" ]] || fail "the package is not named ephervex"
[[ $(field VERSION) == "${VERSION}" ]] ||
    fail "the package says version $(field VERSION), the workspace says ${VERSION}"
[[ -n $(field RELEASE) ]] || fail "the package has no release number"
[[ $(field LICENSE) == "Apache-2.0" ]] ||
    fail "the package declares the license as $(field LICENSE)"
for name in SUMMARY DESCRIPTION URL; do
    [[ -n $(field "${name}") ]] || fail "the package header has no ${name}"
done
# The build host's name is a path out of the build machine like any other, and
# rpm records one unless it is told what to say.
[[ $(field BUILDHOST) == "ephervex" ]] ||
    fail "the package records the build host as $(field BUILDHOST)"
note "the header names the package, its version, its release, and its license"

# --- the architecture the header claims --------------------------------------
# What a package manager matches on before it will install anything, so a field
# that disagrees with the binaries is either an install that cannot happen or
# one that happens and then does not run.
[[ $(field ARCH) == "${RPM_ARCH}" ]] ||
    fail "the package declares $(field ARCH), but this build targets ${RPM_ARCH}"
[[ $(basename "${ARTIFACT}") == "ephervex-${VERSION}-$(field RELEASE).${RPM_ARCH}.rpm" ]] ||
    fail "$(basename "${ARTIFACT}") is not the name a ${RPM_ARCH} package carries"
note "declared ${RPM_ARCH}"

# --- dependencies, produced by RPM's own generator ---------------------------
# Nothing is bundled, so the C runtime is always required. Its absence means the
# generator produced something, but not something real.
REQUIRES=$(rpm -qp --requires "${ARTIFACT}" 2>/dev/null)
printf '%s\n' "${REQUIRES}" | grep -q '^libc\.so\.6' ||
    fail "the package does not require libc.so.6, so nothing was read off the binaries"
note "dependencies were resolved from the binaries"

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
    usr/share/licenses/ephervex/*) ;;
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
    usr/share/licenses/ephervex/LICENSE \
    usr/share/licenses/ephervex/THIRD-PARTY-LICENSES.md \
    usr/share/icons/hicolor/256x256/apps/ephervex.png; do
    [[ -f "${ROOT}/${required}" ]] || fail "the package is missing ${required}"
done
# Not merely present: a generated document that came out empty would satisfy
# the check above while distributing none of the terms it exists to carry.
grep -q 'Third-party licenses' "${ROOT}/usr/share/licenses/ephervex/THIRD-PARTY-LICENSES.md" ||
    fail "the installed third-party license document is not the generated one"
for binary in usr/bin/ephervex usr/bin/ephervex-gui; do
    [[ -x "${ROOT}/${binary}" ]] || fail "${binary} is not executable"
done
note "both binaries are installed, at real paths, executable"

# --- the license texts are marked as such ------------------------------------
# Apache-2.0 attribution is satisfied by shipping the text, and a `--nodocs`
# installation keeps only what the header marks as a license.
LICENSE_FILES=$(rpm -qpL "${ARTIFACT}" 2>/dev/null)
printf '%s\n' "${LICENSE_FILES}" | grep -q '^/usr/share/licenses/ephervex/LICENSE$' ||
    fail "the project's own license is not marked as a license file"
printf '%s\n' "${LICENSE_FILES}" |
    grep -q '^/usr/share/licenses/ephervex/THIRD-PARTY-LICENSES.md$' ||
    fail "the dependencies' licenses are not marked as license files"
note "the license texts survive a --nodocs installation"

# --- the desktop entry points at what the package installs -------------------
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
# (ADR 0027).
HANDLER="${ROOT}/usr/share/applications/ephervex-url-handler.desktop"
[[ -f ${HANDLER} ]] || fail "the package installs no link handler"
grep -q '^MimeType=x-scheme-handler/ephervex;$' "${HANDLER}" ||
    fail "the link handler does not claim the ephervex scheme"
grep -q '^Exec=.* %u$' "${HANDLER}" || fail "the link handler is never handed the link"
grep -q '^NoDisplay=true$' "${HANDLER}" ||
    fail "the link handler would appear as a second Ephervex in the menu"
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

enforce_target_architecture "${ROOT}/usr/bin/ephervex" "${ROOT}/usr/bin/ephervex-gui"
note "built for ${EPHERVEX_BASELINE_ARCH}"

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
(cd / && run_cli --json info >/dev/null) || fail "the package needs its source tree"
note "nothing resolves back into the source tree"

# --- nothing outside the XDG roots -------------------------------------------
# The same rule every artifact is held to: the application owns the XDG roots
# and writes nowhere else (ADR 0003).
escaped=$(find "${SANDBOX_HOME}" -mindepth 1 -print 2>/dev/null | head -n 5 || true)
[[ -z ${escaped} ]] || {
    printf '%s\n' "${escaped}" >&2
    fail "the application wrote outside the XDG roots"
}
note "nothing was written outside the XDG roots"

printf '==> %s looks publishable\n' "$(basename "${ARTIFACT}")"
