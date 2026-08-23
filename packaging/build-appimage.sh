#!/usr/bin/env bash
# Builds the Ephervex Linux release artifact: one AppImage holding both
# binaries, its desktop entry, and its icons.
#
# This script does not provision anything. It builds, assembles, verifies, and
# packages inside whatever environment it is run in, and fails loudly when that
# environment is not the one a release is made in. Two host dependencies must
# already be present, both documented in docs/release.md:
#
#   * a Rust toolchain inside the baseline environment (packaging/linux-baseline
#     names the image whose glibc the release targets);
#   * `appimagetool`, on PATH or named by $APPIMAGETOOL.
#
# Neither is downloaded, fetched, installed, or bootstrapped here. There is no
# URL for either in this file, and a missing one is an error rather than an
# opportunity to reach out to the network.
#
#   usage: packaging/build-appimage.sh <output-directory>
set -euo pipefail

fail() {
    printf 'build-appimage: %s\n' "$*" >&2
    exit 1
}

note() { printf '==> %s\n' "$*"; }

[[ $# -eq 1 ]] || fail "usage: $0 <output-directory>"
OUTPUT_DIR=$1

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
PACKAGING_DIR="${REPO_ROOT}/packaging"

# --- the declared baseline ---------------------------------------------------
# shellcheck source=packaging/linux-baseline
source "${PACKAGING_DIR}/linux-baseline"
: "${EPHERVEX_BASELINE_GLIBC:?the baseline file must declare a glibc version}"
: "${EPHERVEX_BASELINE_ARCH:?the baseline file must declare an architecture}"

HOST_ARCH=$(uname -m)
[[ ${HOST_ARCH} == "${EPHERVEX_BASELINE_ARCH}" ]] ||
    fail "this host is ${HOST_ARCH}; releases are built for ${EPHERVEX_BASELINE_ARCH}"

# --- host tools --------------------------------------------------------------
APPIMAGETOOL=${APPIMAGETOOL:-$(command -v appimagetool || true)}
[[ -n ${APPIMAGETOOL} && -x ${APPIMAGETOOL} ]] || fail \
    "appimagetool was not found.
It is an external host dependency: this script never downloads or installs it.
Install it, or point \$APPIMAGETOOL at it, and run this again.
docs/release.md records the version this procedure was verified against and its
SHA-256, so you can check what you installed before trusting it."

for tool in cargo readelf sha256sum; do
    command -v "${tool}" >/dev/null || fail "${tool} is required and was not found"
done

APPIMAGETOOL_VERSION=$("${APPIMAGETOOL}" --version 2>&1 | head -n 1 || true)
[[ -n ${APPIMAGETOOL_VERSION} ]] || APPIMAGETOOL_VERSION="unknown"

# The AppImage type-2 runtime is the small ELF prepended to the payload. Left to
# itself appimagetool downloads one, which would make packaging a network step
# and let the artifact's launcher change between two builds of the same tree.
# So it is a host dependency like appimagetool itself: supply it through
# $APPIMAGE_RUNTIME_FILE, and docs/release.md records where to get it and the
# SHA-256 to check it against.
[[ -n ${APPIMAGE_RUNTIME_FILE:-} ]] || fail \
    "\$APPIMAGE_RUNTIME_FILE is not set.
The AppImage type-2 runtime is a host dependency like appimagetool: this script
never downloads one. Without it appimagetool would fetch a runtime over the
network, which would make packaging a network step and let the artifact's
launcher change between two builds of the same source.
docs/release.md records where to get it and the SHA-256 to check it against."
[[ -f ${APPIMAGE_RUNTIME_FILE} ]] ||
    fail "\$APPIMAGE_RUNTIME_FILE is set to ${APPIMAGE_RUNTIME_FILE}, which is not a file"
RUNTIME_ARGS=(--runtime-file "${APPIMAGE_RUNTIME_FILE}")
RUNTIME_DESCRIPTION="${APPIMAGE_RUNTIME_FILE} (sha256 $(sha256sum "${APPIMAGE_RUNTIME_FILE}" | cut -d' ' -f1))"

# --- build -------------------------------------------------------------------
note "building the release binaries"
# `--locked` because the artifact's dependency set is part of what a release
# claims to be. Without it a warm registry and a stale Cargo.lock silently ship
# versions that are not the committed ones.
cargo build --locked --release --manifest-path "${REPO_ROOT}/Cargo.toml" \
    -p ephervex-gui -p ephervex-cli

# `CARGO_TARGET_DIR` is how a build inside the baseline container keeps its
# output off a read-only source mount, so the binaries to package are wherever
# cargo actually put them rather than wherever the repository is.
RELEASE_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release"
GUI_BINARY="${RELEASE_DIR}/ephervex-gui"
CLI_BINARY="${RELEASE_DIR}/ephervex"
[[ -x ${GUI_BINARY} ]] || fail "missing ${GUI_BINARY}"
[[ -x ${CLI_BINARY} ]] || fail "missing ${CLI_BINARY}"

# --- the declared baseline, enforced against the artifact --------------------
# Checked on the binaries rather than on the host, so a newer build machine
# cannot raise the supported floor by accident. There is deliberately no flag
# that skips this: a package that does not meet the baseline is not a release.
highest_glibc() {
    readelf --version-info "$1" 2>/dev/null |
        grep -o 'GLIBC_[0-9][0-9.]*' |
        sed 's/^GLIBC_//' |
        sort -u -V |
        tail -n 1
}

version_gt() { [[ $(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1) == "$1" && $1 != "$2" ]]; }

# Pass `static-ok` for a file that may legitimately require no glibc symbol
# versions at all. That is the good case for the AppImage runtime, which is
# normally statically linked; for our own binaries an empty reading means the
# check did not work and must not be mistaken for a clean result.
enforce_glibc_baseline() {
    local binary=$1
    local empty_is_ok=${2:-}
    local found symbol symbol_version
    # `|| true` because the pipeline inside `highest_glibc` exits non-zero when
    # it matches nothing, and under `pipefail` that would end the script here
    # with no message instead of reaching the one written for it.
    found=$(highest_glibc "${binary}" || true)
    if [[ -z ${found} ]]; then
        [[ ${empty_is_ok} == "static-ok" ]] ||
            fail "could not read the glibc requirements of ${binary}"
        note "$(basename "${binary}") requires no glibc symbol versions"
        return
    fi
    if version_gt "${found}" "${REQUIRED_GLIBC}"; then REQUIRED_GLIBC=${found}; fi
    version_gt "${found}" "${EPHERVEX_BASELINE_GLIBC}" || return 0
    printf 'build-appimage: %s requires glibc %s, above the declared baseline %s\n' \
        "$(basename "${binary}")" "${found}" "${EPHERVEX_BASELINE_GLIBC}" >&2
    printf 'the symbols responsible:\n' >&2
    # `-W` keeps long symbol names whole; readelf abbreviates them to
    # `[...]` at its default width, which is useless in a diagnostic.
    readelf -W --dyn-syms "${binary}" 2>/dev/null |
        grep -oE '[A-Za-z_][A-Za-z0-9_]*@GLIBC_[0-9][0-9.]*' |
        sort -u |
        while read -r symbol; do
            symbol_version=${symbol#*@GLIBC_}
            if version_gt "${symbol_version}" "${EPHERVEX_BASELINE_GLIBC}"; then
                printf '  %s\n' "${symbol}" >&2
            fi
        done
    fail "build inside the baseline environment (${EPHERVEX_BASELINE_IMAGE:-see packaging/linux-baseline}); see docs/release.md"
}

REQUIRED_GLIBC=0
enforce_glibc_baseline "${GUI_BINARY}"
enforce_glibc_baseline "${CLI_BINARY}"
# The runtime is prepended to the artifact and is the first thing the kernel
# loads on a user's machine, so the baseline covers it like everything else
# that ships. Leaving it out would mean a package could clear the floor and
# still fail to start on the distribution the floor names.
enforce_glibc_baseline "${APPIMAGE_RUNTIME_FILE}" static-ok
note "glibc floor ${REQUIRED_GLIBC}, at or below the declared baseline ${EPHERVEX_BASELINE_GLIBC}"

# --- the declared shared libraries, enforced against the artifact ------------
# The other half of "will it start on the baseline": nothing is bundled, so
# every library a binary needs is one the host has to provide. A dependency
# that arrives with a crate change fails here rather than on somebody's machine.
ALLOWED_LIBRARIES=()
while IFS= read -r line; do
    line=${line%%#*}
    line=${line//[[:space:]]/}
    [[ -n ${line} ]] && ALLOWED_LIBRARIES+=("${line}")
done <"${PACKAGING_DIR}/linux-shared-libraries"
[[ ${#ALLOWED_LIBRARIES[@]} -gt 0 ]] ||
    fail "packaging/linux-shared-libraries lists no libraries"

for binary in "${GUI_BINARY}" "${CLI_BINARY}"; do
    needed=$(readelf -d "${binary}" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p') ||
        fail "could not read the library requirements of ${binary}"
    # Every dynamically linked binary needs at least the C runtime, so nothing
    # at all means the reading failed rather than that there is nothing to check.
    [[ -n ${needed} ]] || fail "could not read the library requirements of ${binary}"
    while IFS= read -r soname; do
        listed=no
        for allowed in "${ALLOWED_LIBRARIES[@]}"; do
            if [[ ${soname} == "${allowed}" ]]; then
                listed=yes
                break
            fi
        done
        [[ ${listed} == "yes" ]] || fail \
            "$(basename "${binary}") needs ${soname}, which packaging/linux-shared-libraries does not list.
Nothing is bundled, so that is a new requirement on every user's machine. If it
is intended, add it there and to the runtime requirements in docs/release.md."
    done <<<"${needed}"
done
note "shared libraries are the declared set"

# --- assemble ----------------------------------------------------------------
# An explicit allowlist, copied file by file. Nothing walks the working tree, so
# `.git`, developer caches, fixtures, local configuration, and build output
# cannot be swept into the payload by an overlooked glob.
mkdir -p "${OUTPUT_DIR}"
OUTPUT_DIR=$(cd -- "${OUTPUT_DIR}" && pwd)
APPDIR="${OUTPUT_DIR}/Ephervex.AppDir"
rm -rf "${APPDIR}"

note "assembling ${APPDIR}"
install -d -m 0755 "${APPDIR}/usr/bin"
install -d -m 0755 "${APPDIR}/usr/share/applications"
install -d -m 0755 "${APPDIR}/usr/share/metainfo"

install -m 0755 "${GUI_BINARY}" "${APPDIR}/usr/bin/ephervex-gui"
install -m 0755 "${CLI_BINARY}" "${APPDIR}/usr/bin/ephervex"
install -m 0755 "${PACKAGING_DIR}/AppRun" "${APPDIR}/AppRun"
install -m 0644 "${PACKAGING_DIR}/ephervex.desktop" "${APPDIR}/ephervex.desktop"
install -m 0644 "${PACKAGING_DIR}/ephervex.desktop" \
    "${APPDIR}/usr/share/applications/ephervex.desktop"
install -m 0644 "${PACKAGING_DIR}/ephervex.metainfo.xml" \
    "${APPDIR}/usr/share/metainfo/ephervex.metainfo.xml"

ICON_SOURCE="${REPO_ROOT}/assets/icons/hicolor"
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
    icon="${ICON_SOURCE}/${size}/apps/ephervex.png"
    [[ -f ${icon} ]] || fail "missing icon ${icon}"
    install -D -m 0644 "${icon}" \
        "${APPDIR}/usr/share/icons/hicolor/${size}/apps/ephervex.png"
done
# The top-level icon the AppImage runtime and desktop integration look for.
install -m 0644 "${ICON_SOURCE}/256x256/apps/ephervex.png" "${APPDIR}/ephervex.png"

if command -v desktop-file-validate >/dev/null; then
    desktop-file-validate "${APPDIR}/usr/share/applications/ephervex.desktop" ||
        fail "the desktop entry is not valid"
fi
# Packaging runs with `--no-appstream`, so this is the only thing that reads the
# component. `--no-net` keeps it local, like every other step here.
if command -v appstreamcli >/dev/null; then
    appstreamcli validate --no-net \
        "${APPDIR}/usr/share/metainfo/ephervex.metainfo.xml" >/dev/null ||
        fail "the AppStream component is not valid"
fi

# --- package -----------------------------------------------------------------
VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "${REPO_ROOT}/Cargo.toml" | head -n 1)
[[ -n ${VERSION} ]] || fail "could not read the workspace version"
ARTIFACT="${OUTPUT_DIR}/Ephervex-${VERSION}-${EPHERVEX_BASELINE_ARCH}.AppImage"
rm -f "${ARTIFACT}"

note "packaging with ${APPIMAGETOOL_VERSION}"
# `SOURCE_DATE_EPOCH` is honored by appimagetool's squashfs step, so a rebuild
# of the same tree does not differ only in timestamps.
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0} \
    ARCH="${EPHERVEX_BASELINE_ARCH}" \
    "${APPIMAGETOOL}" --no-appstream "${RUNTIME_ARGS[@]}" "${APPDIR}" "${ARTIFACT}" >/dev/null

[[ -f ${ARTIFACT} ]] || fail "appimagetool produced no artifact"
chmod 0755 "${ARTIFACT}"

# The artifact's own ELF header is the runtime that was prepended to it, so this
# holds whatever produced it to the baseline, not just the file this script was
# handed. Cheap, and it is the last thing that can catch a launcher nobody
# checked.
enforce_glibc_baseline "${ARTIFACT}" static-ok

# --- smoke test --------------------------------------------------------------
"${PACKAGING_DIR}/smoke-test-appimage.sh" "${ARTIFACT}"

# --- summary -----------------------------------------------------------------
printf '\n'
note "artifact      ${ARTIFACT}"
note "sha256        $(sha256sum "${ARTIFACT}" | cut -d' ' -f1)"
note "glibc floor   ${REQUIRED_GLIBC} (baseline ${EPHERVEX_BASELINE_GLIBC})"
note "appimagetool  ${APPIMAGETOOL_VERSION}"
note "runtime       ${RUNTIME_DESCRIPTION}"
