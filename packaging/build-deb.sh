#!/usr/bin/env bash
# Builds the Ephervex Debian package: both binaries at real paths, its desktop
# entry, its AppStream component, and its icons.
#
# The counterpart to build-appimage.sh and deliberately its shape. The same
# baseline is enforced by the same code, the payload is an explicit file-by-file
# allowlist rather than anything that walks the working tree, and nothing is
# downloaded: a missing host tool is an error naming what to install.
#
# What differs is what the artifact is. An AppImage is one file that dispatches
# its two binaries on the name it was invoked under; a package installs
# /usr/bin/ephervex and /usr/bin/ephervex-gui as two real files already on PATH
# (ADR 0026).
#
# One host dependency must already be present:
#
#   * `dpkg-deb` and `dpkg-shlibdeps`, from dpkg and dpkg-dev.
#
#   usage: packaging/build-deb.sh <output-directory>
set -euo pipefail

fail() {
    printf 'build-deb: %s\n' "$*" >&2
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

# How a release binary is built, and the checks every artifact is held to. One
# implementation, shared with build-appimage.sh.
# shellcheck source=packaging/linux-release-artifact
source "${PACKAGING_DIR}/linux-release-artifact"

HOST_ARCH=$(uname -m)
[[ ${HOST_ARCH} == "${EPHERVEX_BASELINE_ARCH}" ]] ||
    fail "this host is ${HOST_ARCH}; releases are built for ${EPHERVEX_BASELINE_ARCH}"

# Debian names the architecture differently from `uname -m`, and there is one
# supported release architecture, so this is a lookup rather than a guess.
case "${EPHERVEX_BASELINE_ARCH}" in
x86_64) DEB_ARCH=amd64 ;;
*) fail "no Debian architecture is declared for ${EPHERVEX_BASELINE_ARCH}" ;;
esac

# --- host tools --------------------------------------------------------------
for tool in cargo readelf sha256sum dpkg-deb dpkg-shlibdeps; do
    command -v "${tool}" >/dev/null || fail \
        "${tool} is required and was not found.
dpkg-deb comes from dpkg and dpkg-shlibdeps from dpkg-dev. This script never
installs either; see docs/release.md."
done

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
note "glibc floor ${REQUIRED_GLIBC}, at or below the declared baseline ${EPHERVEX_BASELINE_GLIBC}"

enforce_shared_library_allowlist "${PACKAGING_DIR}" "${GUI_BINARY}" "${CLI_BINARY}"
note "shared libraries are the declared set"

enforce_no_build_paths "${GUI_BINARY}" "${CLI_BINARY}"
note "no build-machine home paths are embedded"

# --- assemble ----------------------------------------------------------------
# An explicit allowlist, copied file by file, for the same reason the AppDir is.
VERSION=$("${PACKAGING_DIR}/workspace-version.sh")
mkdir -p "${OUTPUT_DIR}"
OUTPUT_DIR=$(cd -- "${OUTPUT_DIR}" && pwd)
STAGE="${OUTPUT_DIR}/ephervex_${VERSION}_${DEB_ARCH}"
rm -rf "${STAGE}"

note "assembling ${STAGE}"
install -d -m 0755 "${STAGE}/DEBIAN"
install -d -m 0755 "${STAGE}/usr/bin"
install -d -m 0755 "${STAGE}/usr/share/applications"
install -d -m 0755 "${STAGE}/usr/share/metainfo"
install -d -m 0755 "${STAGE}/usr/share/doc/ephervex"

install -m 0755 "${GUI_BINARY}" "${STAGE}/usr/bin/ephervex-gui"
install -m 0755 "${CLI_BINARY}" "${STAGE}/usr/bin/ephervex"
install -m 0644 "${PACKAGING_DIR}/ephervex.desktop" \
    "${STAGE}/usr/share/applications/ephervex.desktop"
# Only the package installs this one. An AppImage registers its link handler on
# request instead, because it has no install step to make the choice at.
install -m 0644 "${PACKAGING_DIR}/ephervex-url-handler.desktop" \
    "${STAGE}/usr/share/applications/ephervex-url-handler.desktop"
install -m 0644 "${PACKAGING_DIR}/ephervex.metainfo.xml" \
    "${STAGE}/usr/share/metainfo/ephervex.metainfo.xml"
install -m 0644 "${REPO_ROOT}/LICENSE" "${STAGE}/usr/share/doc/ephervex/copyright"

ICON_SOURCE="${REPO_ROOT}/assets/icons/hicolor"
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
    icon="${ICON_SOURCE}/${size}/apps/ephervex.png"
    [[ -f ${icon} ]] || fail "missing icon ${icon}"
    install -D -m 0644 "${icon}" \
        "${STAGE}/usr/share/icons/hicolor/${size}/apps/ephervex.png"
done

if command -v desktop-file-validate >/dev/null; then
    for entry in "${STAGE}/usr/share/applications/"*.desktop; do
        desktop-file-validate "${entry}" ||
            fail "$(basename "${entry}") is not a valid desktop entry"
    done
fi
if command -v appstreamcli >/dev/null; then
    appstreamcli validate --no-net \
        "${STAGE}/usr/share/metainfo/ephervex.metainfo.xml" >/dev/null ||
        fail "the AppStream component is not valid"
fi

# --- dependencies, read off the binaries -------------------------------------
# `dpkg-shlibdeps` resolves what the binaries actually need to package
# relationships for whatever distribution is running this. The allowlist in
# linux-shared-libraries still decides what is *allowed* to be needed and has
# already been enforced above; this only decides how to spell it for a package
# manager, so the two cannot disagree about the facts (ADR 0026).
note "resolving package dependencies"
# It insists on being run from a source tree with a debian/control, so it gets a
# throwaway one. Nothing else in this script reads it.
SHLIBDEPS_DIR=$(mktemp -d)
cleanup() { rm -rf "${SHLIBDEPS_DIR}"; }
trap cleanup EXIT
mkdir -p "${SHLIBDEPS_DIR}/debian"
printf 'Source: ephervex\n\nPackage: ephervex\nArchitecture: %s\n' "${DEB_ARCH}" \
    >"${SHLIBDEPS_DIR}/debian/control"
: >"${SHLIBDEPS_DIR}/debian/substvars"

(
    cd "${SHLIBDEPS_DIR}" &&
        dpkg-shlibdeps -Tdebian/substvars -O \
            "${STAGE}/usr/bin/ephervex" "${STAGE}/usr/bin/ephervex-gui"
) >"${SHLIBDEPS_DIR}/depends" 2>"${SHLIBDEPS_DIR}/warnings" || {
    cat "${SHLIBDEPS_DIR}/warnings" >&2
    fail "dpkg-shlibdeps could not resolve the binaries' dependencies"
}

DEPENDS=$(sed -n 's/^shlibs:Depends=//p' "${SHLIBDEPS_DIR}/depends")
[[ -n ${DEPENDS} ]] || fail "dpkg-shlibdeps resolved no dependencies, which cannot be right"

# --- control -----------------------------------------------------------------
# `Installed-Size` is in kibibytes and is what a package manager reports before
# installing, so it is measured rather than estimated.
INSTALLED_SIZE=$(du -ks "${STAGE}" | cut -f1)

cat >"${STAGE}/DEBIAN/control" <<CONTROL
Package: ephervex
Version: ${VERSION}
Architecture: ${DEB_ARCH}
Maintainer: ChaoticTrials <noreply@chaotictrials.de>
Installed-Size: ${INSTALLED_SIZE}
Depends: ${DEPENDS}
Section: games
Priority: optional
Homepage: https://github.com/ChaoticTrials/ephervex
Description: Disposable Minecraft instances for mod development
 Ephervex is a Linux-first Minecraft launcher and developer tool for
 short-lived bug-reproduction environments. It imports a Modrinth, CurseForge,
 or Prism Launcher pack, creates the instance in temporary storage, launches
 the game, and keeps the logs and crash reports afterwards.
 .
 This package installs both the desktop application and the ephervex
 command-line interface.
CONTROL

# --- package -----------------------------------------------------------------
# The name carries the version, because that is what a package manager and an
# apt repository expect to read off a file name. The AppImage's stable name
# exists to keep one download address valid; a package archive indexes versions
# itself, so the two conventions differ for their own reasons.
ARTIFACT="${OUTPUT_DIR}/ephervex_${VERSION}_${DEB_ARCH}.deb"
rm -f "${ARTIFACT}"

note "packaging ${ARTIFACT}"
# `--root-owner-group` so the payload is owned by root regardless of who built
# it, which is what keeps two builds of the same source producing the same
# bytes. `-Zxz` is what the baseline's dpkg reads without argument.
dpkg-deb --root-owner-group -Zxz --build "${STAGE}" "${ARTIFACT}" >/dev/null

[[ -f ${ARTIFACT} ]] || fail "dpkg-deb produced no artifact"

# --- smoke test --------------------------------------------------------------
"${PACKAGING_DIR}/smoke-test-deb.sh" "${ARTIFACT}"

# --- summary -----------------------------------------------------------------
printf '\n'
note "version       ${VERSION}"
note "artifact      ${ARTIFACT}"
note "sha256        $(sha256sum "${ARTIFACT}" | cut -d' ' -f1)"
note "glibc floor   ${REQUIRED_GLIBC} (baseline ${EPHERVEX_BASELINE_GLIBC})"
note "depends       ${DEPENDS}"
