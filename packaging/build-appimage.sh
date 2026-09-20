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
# Neither is downloaded, fetched, installed, or bootstrapped here: a missing one
# is an error rather than an opportunity to reach out to the network. What to
# install is pinned in packaging/host-tools, with the digest to check it against.
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

# How the targeted architecture is spelled by everything that needs a name.
# shellcheck source=packaging/linux-architectures
source "${PACKAGING_DIR}/linux-architectures"
ephervex_require_known_arch "${EPHERVEX_BASELINE_ARCH}"
# What the artifact is named and what appimagetool is told it built. The runtime
# refuses an artifact whose declared architecture is not the machine's, so these
# have to be the architecture the binaries were actually built for.
APPIMAGE_ARCH=$(ephervex_appimage_arch "${EPHERVEX_BASELINE_ARCH}")

# How a release binary is built, and the checks every artifact is held to. One
# implementation, shared with build-deb.sh.
# shellcheck source=packaging/linux-release-artifact
source "${PACKAGING_DIR}/linux-release-artifact"

# Never a cross build. The glibc floor, the shared-library allowlist and the
# smoke test all inspect and run the binaries this produces, and none of that
# happens on a machine that cannot execute them.
HOST_ARCH=$(uname -m)
[[ ${HOST_ARCH} == "${EPHERVEX_BASELINE_ARCH}" ]] || fail \
    "this host is ${HOST_ARCH} and this build targets ${EPHERVEX_BASELINE_ARCH}.
Each release architecture is built on its own, because every check here reads
and runs the built binary. Build this on ${EPHERVEX_BASELINE_ARCH}, or leave
\$EPHERVEX_BASELINE_ARCH unset to target this host."

# --- host tools --------------------------------------------------------------
APPIMAGETOOL=${APPIMAGETOOL:-$(command -v appimagetool || true)}
[[ -n ${APPIMAGETOOL} && -x ${APPIMAGETOOL} ]] || fail \
    "appimagetool was not found.
It is an external host dependency: this script never downloads or installs it.
Install it, or point \$APPIMAGETOOL at it, and run this again.
packaging/host-tools names where it comes from and the SHA-256 this procedure
was verified against, so you can check what you installed before trusting it."

for tool in cargo readelf sha256sum; do
    command -v "${tool}" >/dev/null || fail "${tool} is required and was not found"
done

APPIMAGETOOL_VERSION=$("${APPIMAGETOOL}" --version 2>&1 | head -n 1 || true)
[[ -n ${APPIMAGETOOL_VERSION} ]] || APPIMAGETOOL_VERSION="unknown"

# The AppImage type-2 runtime is the small ELF prepended to the payload. Left to
# itself appimagetool downloads one, which would make packaging a network step
# and let the artifact's launcher change between two builds of the same tree.
# So it is a host dependency like appimagetool itself: supply it through
# $APPIMAGE_RUNTIME_FILE, and packaging/host-tools records where to get it and
# the SHA-256 to check it against.
[[ -n ${APPIMAGE_RUNTIME_FILE:-} ]] || fail \
    "\$APPIMAGE_RUNTIME_FILE is not set.
The AppImage type-2 runtime is a host dependency like appimagetool: this script
never downloads one. Without it appimagetool would fetch a runtime over the
network, which would make packaging a network step and let the artifact's
launcher change between two builds of the same source.
packaging/host-tools records where to get it and the SHA-256 to check it against."
[[ -f ${APPIMAGE_RUNTIME_FILE} ]] ||
    fail "\$APPIMAGE_RUNTIME_FILE is set to ${APPIMAGE_RUNTIME_FILE}, which is not a file"
RUNTIME_ARGS=(--runtime-file "${APPIMAGE_RUNTIME_FILE}")
RUNTIME_DESCRIPTION="${APPIMAGE_RUNTIME_FILE} (sha256 $(sha256sum "${APPIMAGE_RUNTIME_FILE}" | cut -d' ' -f1))"

# --- build -------------------------------------------------------------------
note "building the release binaries"
build_release_binaries "${REPO_ROOT}"

# `CARGO_TARGET_DIR` is how a build inside the baseline container keeps its
# output off a read-only source mount, so the binaries to package are wherever
# cargo actually put them rather than wherever the repository is.
RELEASE_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}/release"
GUI_BINARY="${RELEASE_DIR}/ephervex-gui"
CLI_BINARY="${RELEASE_DIR}/ephervex"
[[ -x ${GUI_BINARY} ]] || fail "missing ${GUI_BINARY}"
[[ -x ${CLI_BINARY} ]] || fail "missing ${CLI_BINARY}"

# --- the declared baseline, enforced against the artifact --------------------
REQUIRED_GLIBC=0
enforce_glibc_baseline "${GUI_BINARY}"
enforce_glibc_baseline "${CLI_BINARY}"
# The runtime is prepended to the artifact and is the first thing the kernel
# loads on a user's machine, so the baseline covers it like everything else
# that ships. Leaving it out would mean a package could clear the floor and
# still fail to start on the distribution the floor names.
enforce_glibc_baseline "${APPIMAGE_RUNTIME_FILE}" static-ok
note "glibc floor ${REQUIRED_GLIBC}, at or below the declared baseline ${EPHERVEX_BASELINE_GLIBC}"

enforce_shared_library_allowlist "${PACKAGING_DIR}" "${GUI_BINARY}" "${CLI_BINARY}"
note "shared libraries are the declared set"

# The runtime ships too, and an artifact whose launcher is built for another
# architecture is one that cannot start at all.
enforce_target_architecture "${GUI_BINARY}" "${CLI_BINARY}" "${APPIMAGE_RUNTIME_FILE}"
note "built for ${EPHERVEX_BASELINE_ARCH}, runtime included"

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
install -d -m 0755 "${APPDIR}/usr/share/doc/ephervex"

install -m 0755 "${GUI_BINARY}" "${APPDIR}/usr/bin/ephervex-gui"
install -m 0755 "${CLI_BINARY}" "${APPDIR}/usr/bin/ephervex"
install -m 0755 "${PACKAGING_DIR}/AppRun" "${APPDIR}/AppRun"
install -m 0644 "${PACKAGING_DIR}/ephervex.desktop" "${APPDIR}/ephervex.desktop"
install -m 0644 "${PACKAGING_DIR}/ephervex.desktop" \
    "${APPDIR}/usr/share/applications/ephervex.desktop"
install -m 0644 "${PACKAGING_DIR}/ephervex.metainfo.xml" \
    "${APPDIR}/usr/share/metainfo/ephervex.metainfo.xml"
# The licenses travel with the binaries, because that is what satisfies them.
# Apache-2.0 and every permissive license the dependencies use ask for their
# text to be distributed with the software; a screen a user may never open is
# how they find it, not how the condition is met.
install -m 0644 "${REPO_ROOT}/LICENSE" "${APPDIR}/usr/share/doc/ephervex/LICENSE"
install -m 0644 "${REPO_ROOT}/THIRD-PARTY-LICENSES.md" \
    "${APPDIR}/usr/share/doc/ephervex/THIRD-PARTY-LICENSES.md"

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
VERSION=$("${PACKAGING_DIR}/workspace-version.sh")
# The name carries no version. A release is identified by its tag and by what
# the binaries themselves report, so one stable name keeps a download address,
# a symlink, and a desktop integration valid across releases instead of
# breaking every one of them on every publish. `AppRun` documents the same name.
ARTIFACT="${OUTPUT_DIR}/Ephervex-${APPIMAGE_ARCH}.AppImage"
rm -f "${ARTIFACT}"

note "packaging with ${APPIMAGETOOL_VERSION}"
# `SOURCE_DATE_EPOCH` is honored by appimagetool's squashfs step, so a rebuild
# of the same tree does not differ only in timestamps.
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0} \
    ARCH="${APPIMAGE_ARCH}" \
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
note "version       ${VERSION}"
note "artifact      ${ARTIFACT}"
note "sha256        $(sha256sum "${ARTIFACT}" | cut -d' ' -f1)"
note "glibc floor   ${REQUIRED_GLIBC} (baseline ${EPHERVEX_BASELINE_GLIBC})"
note "appimagetool  ${APPIMAGETOOL_VERSION}"
note "runtime       ${RUNTIME_DESCRIPTION}"
