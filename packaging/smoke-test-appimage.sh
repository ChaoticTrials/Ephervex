#!/usr/bin/env bash
# Checks a built Ephervex AppImage before anyone is asked to run it.
#
# Everything here works on an *extracted* copy, so the check does not depend on
# FUSE being available and can run in a container. The application is exercised
# against throwaway XDG roots, so a smoke test can never read or write the
# developer's own configuration.
#
#   usage: packaging/smoke-test-appimage.sh <artifact.AppImage>
set -euo pipefail

fail() {
    printf 'smoke-test: %s\n' "$*" >&2
    exit 1
}

note() { printf '  - %s\n' "$*"; }

# Checked rather than assumed: `strings` missing would turn the build-machine
# path check below into a silent no-op, which is worse than not running it.
for tool in readelf strings python3; do
    command -v "${tool}" >/dev/null || fail "${tool} is required and was not found"
done

[[ $# -eq 1 ]] || fail "usage: $0 <artifact.AppImage>"
ARTIFACT=$(cd -- "$(dirname -- "$1")" && printf '%s/%s' "$(pwd)" "$(basename -- "$1")")
[[ -x ${ARTIFACT} ]] || fail "${ARTIFACT} is not an executable file"

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
(cd "${WORKSPACE}" && "${ARTIFACT}" --appimage-extract >/dev/null) ||
    fail "the artifact could not be extracted"
ROOT="${WORKSPACE}/squashfs-root"
[[ -d ${ROOT} ]] || fail "extraction produced no squashfs-root"
note "extracted without FUSE"

# --- layout ------------------------------------------------------------------
for required in \
    AppRun \
    ephervex.desktop \
    ephervex.png \
    usr/bin/ephervex \
    usr/bin/ephervex-gui \
    usr/share/applications/ephervex.desktop \
    usr/share/metainfo/ephervex.metainfo.xml \
    usr/share/icons/hicolor/256x256/apps/ephervex.png; do
    [[ -e "${ROOT}/${required}" ]] || fail "the package is missing ${required}"
done
[[ -x "${ROOT}/AppRun" ]] || fail "AppRun is not executable"
note "every expected file is present"

# --- forbidden content -------------------------------------------------------
# What must never ship: repository plumbing, test fixtures, developer tooling,
# build output, and anything that looks like configuration or a credential.
while IFS= read -r found; do
    fail "the package contains forbidden content: ${found#"${ROOT}/"}"
done < <(find "${ROOT}" \
    \( -name '.git' -o -name '.git*' -o -name 'fixtures' -o -name 'tools' \
    -o -name 'target' -o -name '*.toml' -o -name '*.rs' -o -name '.env*' \
    -o -name 'config.toml' -o -name '*.pem' -o -name '*.key' \) -print)
note "no repository, fixture, tooling, or configuration content"

# --- no build-machine paths --------------------------------------------------
enforce_no_build_paths "${ROOT}/usr/bin/ephervex" "${ROOT}/usr/bin/ephervex-gui"
note "no build-machine home paths are embedded"

# --- glibc floor -------------------------------------------------------------
for binary in "${ROOT}/usr/bin/ephervex" "${ROOT}/usr/bin/ephervex-gui"; do
    # `|| true` because the pipeline exits non-zero when it matches nothing, and
    # under `pipefail` that would end this script with no explanation. An empty
    # reading is then a failure in its own right: comparing it against the
    # baseline would come out equal and pass a check that never ran.
    found=$(readelf --version-info "${binary}" 2>/dev/null |
        grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -u -V | tail -n 1 || true)
    [[ -n ${found} ]] || fail "could not read the glibc requirements of $(basename "${binary}")"
    highest=$(printf '%s\n%s\n' "${found}" "${EPHERVEX_BASELINE_GLIBC}" | sort -V | tail -n 1)
    [[ ${highest} == "${EPHERVEX_BASELINE_GLIBC}" ]] ||
        fail "$(basename "${binary}") requires glibc ${found}, above the baseline ${EPHERVEX_BASELINE_GLIBC}"
done
note "glibc requirements are at or below the baseline ${EPHERVEX_BASELINE_GLIBC}"

# --- how the application is reached ------------------------------------------
# Two ways in, and they are not the same test.
#
# A user runs the artifact itself: the type-2 runtime mounts it through FUSE,
# exports the name it was invoked under as $ARGV0, and AppRun dispatches on
# that. A symlink named `ephervex` is therefore the whole command-line
# interface, and it is what docs/release.md tells people to make.
#
# That path needs FUSE, which a build container usually does not have. So the
# extracted copy is exercised unconditionally, with $ARGV0 supplied by hand,
# and the mounted artifact is exercised whenever the machine can.
XDG="${WORKSPACE}/xdg"
mkdir -p "${XDG}"/{config,data,cache,state,tmp}
# Deliberately *not* the XDG parent. Anything that resolves a location from
# $HOME instead of the XDG variables then lands somewhere the check at the end
# can see, rather than blending into the roots it was supposed to use.
SANDBOX_HOME="${WORKSPACE}/home"
mkdir -p "${SANDBOX_HOME}"

sandboxed() {
    env -i \
        HOME="${SANDBOX_HOME}" \
        PATH="/usr/bin:/bin" \
        XDG_CONFIG_HOME="${XDG}/config" \
        XDG_DATA_HOME="${XDG}/data" \
        XDG_CACHE_HOME="${XDG}/cache" \
        XDG_STATE_HOME="${XDG}/state" \
        TMPDIR="${XDG}/tmp" \
        EPHERVEX_LOG=error \
        "$@"
}

# The extracted AppRun, told what it was invoked as. No FUSE involved.
run_cli() {
    sandboxed ARGV0=ephervex "${ROOT}/AppRun" "$@"
}

if [[ -e /dev/fuse ]] && (command -v fusermount3 >/dev/null || command -v fusermount >/dev/null); then
    FUSE_AVAILABLE=yes
else
    FUSE_AVAILABLE=no
fi

# --- the command-line interface, without FUSE --------------------------------
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
# There is a configuration section because the roots resolved; it is `null` only
# when they did not, and the assertion above already rules that out.
assert health["configuration"] is not None
assert health["configuration"]["status"] == "absent", health["configuration"]
# A first run has written nothing, so a root is either not there yet or already
# usable. Anything else is a fault, and each is named rather than lumped into
# one "not writable" answer that would also cover a root nothing could inspect.
for directory in health["directories"]:
    assert directory["state"] in ("absent", "writable"), directory
PY
note "doctor loads the bundled datasets and reports a clean first run"

# Opening the application created the roots that were absent above, so this run
# has to find all of them, and find every one writable. Asserting that only on
# the first run would accept a directory the probe never reached.
HEALTH=$(run_cli --json doctor) || fail "\`ephervex doctor\` failed on a settled installation"
python3 - "${HEALTH}" <<'PY' || fail "the application did not leave every storage root writable"
import json, sys
health = json.loads(sys.argv[1])
for directory in health["directories"]:
    assert directory["state"] == "writable", directory
PY
note "every storage root is created and writable"

# --- no source tree needed ---------------------------------------------------
# Run again from a directory that is not the repository and with the repository
# unreadable from the environment, to prove nothing resolves back into it.
(cd / && run_cli --json info >/dev/null) || fail "the package needs its source tree"
note "nothing resolves back into the source tree"

# --- the artifact as a user runs it ------------------------------------------
# Everything above ran on the extracted copy. This is the mounted one: the
# runtime, the symlink, and $ARGV0 dispatch, which is the path every user takes
# and the only one the extracted checks cannot cover.
if [[ ${FUSE_AVAILABLE} == "yes" ]]; then
    ln -s "${ARTIFACT}" "${WORKSPACE}/ephervex"
    MOUNTED=$(sandboxed "${WORKSPACE}/ephervex" --json info) ||
        fail "the artifact does not run through an \`ephervex\` symlink"
    # The identity JSON is what proves the runtime dispatched to the
    # command-line interface rather than merely exiting zero.
    printf '%s' "${MOUNTED}" | grep -q '"app_id"' ||
        fail "an \`ephervex\` symlink did not reach the command-line interface"
    note "the mounted artifact dispatches to the command-line interface by name"
else
    note "no usable FUSE; skipped the mounted artifact and its symlink dispatch"
fi

# --- the desktop application -------------------------------------------------
# Only when there is a display to start it on; a headless container is a
# reasonable place to build a package and a bad place to assert about windows.
if [[ -n ${DISPLAY:-} || -n ${WAYLAND_DISPLAY:-} ]]; then
    env HOME="${SANDBOX_HOME}" \
        XDG_CONFIG_HOME="${XDG}/config" \
        XDG_DATA_HOME="${XDG}/data" \
        XDG_CACHE_HOME="${XDG}/cache" \
        XDG_STATE_HOME="${XDG}/state" \
        TMPDIR="${XDG}/tmp" \
        EPHERVEX_LOG=error \
        "${ROOT}/AppRun" &
    gui_pid=$!
    sleep 5
    if kill -0 "${gui_pid}" 2>/dev/null; then
        kill "${gui_pid}" 2>/dev/null || true
        wait "${gui_pid}" 2>/dev/null || true
        note "the desktop application starts and stays up"
    else
        wait "${gui_pid}" 2>/dev/null && status=0 || status=$?
        fail "the desktop application exited immediately (status ${status})"
    fi
else
    note "no display; skipped starting the desktop application"
fi

# --- nothing escaped the throwaway roots -------------------------------------
# Two things, because either alone is satisfied by a run that did nothing.
# First: $HOME is not the XDG parent, so a location resolved from $HOME instead
# of the XDG variables shows up here. This is what stands between a future
# change and reading or writing the developer's own configuration.
escaped=$(find "${SANDBOX_HOME}" -mindepth 1 -print 2>/dev/null | head -n 5)
if [[ -n ${escaped} ]]; then
    printf '%s\n' "${escaped}" >&2
    fail "something was written under \$HOME instead of the XDG roots"
fi
# Second: the runs did use the sandbox, rather than writing nothing anywhere.
[[ -d "${XDG}/state/ephervex" || -d "${XDG}/config/ephervex" ]] ||
    fail "the run wrote nothing at all, which means it did not use the sandbox"
note "every write landed inside the throwaway XDG roots, and none under \$HOME"

printf '==> smoke test passed\n'
