#!/usr/bin/env bash
# Builds the Ephervex RPM package: both binaries at real paths, its desktop
# entry, its AppStream component, its icons, and its license texts.
#
# The counterpart to build-deb.sh and deliberately its shape. The same baseline
# is enforced by the same code, the payload is an explicit file-by-file
# allowlist rather than anything that walks the working tree, and nothing is
# downloaded.
#
# What differs from the Debian package is only what a package manager expects to
# read: RPM spells the architecture its own way, carries a release number beside
# the version, takes its license texts under /usr/share/licenses rather than
# /usr/share/doc, and reads dependencies off the binaries through its own
# generator instead of dpkg-shlibdeps.
#
# The artifact is an unsigned package. Publishing it to the RPM repository, with
# the signing and indexing that needs, is the release workflow's job and happens
# after this script has finished (ADR 0044); nothing here signs, indexes, or
# uploads.
#
# Two host dependencies must already be present:
#
#   * `rpmbuild`, from rpm;
#   * `rpm`, to read back what was built.
#
#   usage: packaging/build-rpm.sh <output-directory>
set -euo pipefail

fail() {
    printf 'build-rpm: %s\n' "$*" >&2
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

# How a release binary is built, and the checks every artifact is held to. One
# implementation, shared with build-appimage.sh and build-deb.sh.
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

RPM_ARCH=$(ephervex_rpm_arch "${EPHERVEX_BASELINE_ARCH}")

# The package's release number, which RPM carries beside the version and which
# counts rebuilds of one upstream version. Ephervex rebuilds a version only by
# releasing the next one, so it is 1 and stays 1.
RPM_RELEASE=1

# --- host tools --------------------------------------------------------------
for tool in cargo readelf sha256sum rpmbuild rpm; do
    command -v "${tool}" >/dev/null || fail \
        "${tool} is required and was not found.
Both come from the rpm package. This script never installs it; see
docs/release.md."
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

enforce_target_architecture "${GUI_BINARY}" "${CLI_BINARY}"
note "built for ${EPHERVEX_BASELINE_ARCH}, packaged as ${RPM_ARCH}"

enforce_no_build_paths "${GUI_BINARY}" "${CLI_BINARY}"
note "no build-machine home paths are embedded"

# --- assemble ----------------------------------------------------------------
# An explicit allowlist, copied file by file, for the same reason the AppDir and
# the Debian staging tree are.
VERSION=$("${PACKAGING_DIR}/workspace-version.sh")
mkdir -p "${OUTPUT_DIR}"
OUTPUT_DIR=$(cd -- "${OUTPUT_DIR}" && pwd)
STAGE="${OUTPUT_DIR}/ephervex-${VERSION}-${RPM_RELEASE}.${RPM_ARCH}"
rm -rf "${STAGE}"

note "assembling ${STAGE}"
install -d -m 0755 "${STAGE}/usr/bin"
install -d -m 0755 "${STAGE}/usr/share/applications"
install -d -m 0755 "${STAGE}/usr/share/metainfo"
install -d -m 0755 "${STAGE}/usr/share/licenses/ephervex"

install -m 0755 "${GUI_BINARY}" "${STAGE}/usr/bin/ephervex-gui"
install -m 0755 "${CLI_BINARY}" "${STAGE}/usr/bin/ephervex"
install -m 0644 "${PACKAGING_DIR}/ephervex.desktop" \
    "${STAGE}/usr/share/applications/ephervex.desktop"
# Only a package installs this one. An AppImage registers its link handler on
# request instead, because it has no install step to make the choice at.
install -m 0644 "${PACKAGING_DIR}/ephervex-url-handler.desktop" \
    "${STAGE}/usr/share/applications/ephervex-url-handler.desktop"
install -m 0644 "${PACKAGING_DIR}/ephervex.metainfo.xml" \
    "${STAGE}/usr/share/metainfo/ephervex.metainfo.xml"
# RPM's own place for license texts, which is where `rpm -qL ephervex` looks and
# what `--nodocs` installations still keep.
install -m 0644 "${REPO_ROOT}/LICENSE" "${STAGE}/usr/share/licenses/ephervex/LICENSE"
install -m 0644 "${REPO_ROOT}/THIRD-PARTY-LICENSES.md" \
    "${STAGE}/usr/share/licenses/ephervex/THIRD-PARTY-LICENSES.md"

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

# The timestamps the package records for its own files. rpm reads them off the
# staging tree, so without this the only thing that differs between two builds
# of the same source is when they ran, which is exactly what a reproducible
# release procedure must not depend on. Set here rather than left to rpm's
# mtime clamping, which needs a changelog entry this package does not have.
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
find "${STAGE}" -exec touch --no-dereference --date="@${SOURCE_DATE_EPOCH}" {} +

# --- the spec ----------------------------------------------------------------
# Generated rather than kept as a file, because everything in it is already
# recorded somewhere else: the version comes from the workspace manifest, the
# architecture from the table, the payload from the staging tree above. A spec
# checked in beside them would be a fourth place for the same facts to drift.
#
# `%install` copies the tree this script assembled rather than building
# anything. rpmbuild is the archive format and the dependency generator here,
# not the build system.
BUILD_ROOT=$(mktemp -d)
cleanup() { rm -rf "${BUILD_ROOT}"; }
trap cleanup EXIT
mkdir -p "${BUILD_ROOT}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
SPEC="${BUILD_ROOT}/SPECS/ephervex.spec"

# The icon sizes, as %files lines, from the same list that installed them.
ICON_FILES=""
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
    ICON_FILES+="/usr/share/icons/hicolor/${size}/apps/ephervex.png"$'\n'
done

cat >"${SPEC}" <<SPECFILE
# Generated by packaging/build-rpm.sh. Do not edit; edit that script.

# The binaries are already built, checked against the baseline, and staged. Left
# to itself rpmbuild would strip them, split debug symbols out, and rewrite
# build-id links, all of which change the bytes this script just verified.
%define __os_install_post %{nil}
%define debug_package %{nil}
%define _build_id_links none

# The build host's name would otherwise be recorded in the package header, which
# is a path out of the build machine like any other.
%define _buildhost ephervex

Name:           ephervex
Version:        ${VERSION}
Release:        ${RPM_RELEASE}
Summary:        Disposable Minecraft instances for mod development
License:        Apache-2.0
URL:            https://github.com/ChaoticTrials/Ephervex
BuildArch:      ${RPM_ARCH}

%description
Ephervex is a Linux-first Minecraft launcher and developer tool for short-lived
bug-reproduction environments. It imports a Modrinth, CurseForge, or Prism
Launcher pack, creates the instance in temporary storage, launches the game, and
keeps the logs and crash reports afterwards.

This package installs both the desktop application and the ephervex
command-line interface.

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}
cp -a ${STAGE}/. %{buildroot}/

%files
%license /usr/share/licenses/ephervex/LICENSE
%license /usr/share/licenses/ephervex/THIRD-PARTY-LICENSES.md
/usr/bin/ephervex
/usr/bin/ephervex-gui
/usr/share/applications/ephervex.desktop
/usr/share/applications/ephervex-url-handler.desktop
/usr/share/metainfo/ephervex.metainfo.xml
${ICON_FILES}
SPECFILE

# --- package -----------------------------------------------------------------
# The name carries the version and the release, because that is what a package
# manager expects to read off a file name. The AppImage's stable name exists to
# keep one download address valid; the two conventions differ for their own
# reasons.
ARTIFACT="${OUTPUT_DIR}/ephervex-${VERSION}-${RPM_RELEASE}.${RPM_ARCH}.rpm"
rm -f "${ARTIFACT}"

note "packaging ${ARTIFACT}"
# `SOURCE_DATE_EPOCH` is what the package records as its build time; the file
# timestamps were normalized above.
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH}" \
    rpmbuild -bb \
    --define "_topdir ${BUILD_ROOT}" \
    --define "_rpmdir ${BUILD_ROOT}/RPMS" \
    --define "use_source_date_epoch_as_buildtime 1" \
    --target "${RPM_ARCH}" \
    "${SPEC}" >"${BUILD_ROOT}/rpmbuild.log" 2>&1 || {
    cat "${BUILD_ROOT}/rpmbuild.log" >&2
    fail "rpmbuild could not build the package"
}

BUILT="${BUILD_ROOT}/RPMS/${RPM_ARCH}/ephervex-${VERSION}-${RPM_RELEASE}.${RPM_ARCH}.rpm"
[[ -f ${BUILT} ]] || {
    cat "${BUILD_ROOT}/rpmbuild.log" >&2
    fail "rpmbuild produced no artifact at ${BUILT}"
}
install -m 0644 "${BUILT}" "${ARTIFACT}"

# Read off the built package rather than predicted, for the same reason
# build-deb.sh reads its dependencies off the binaries: the package manager's
# own generator decides, and reporting anything else would be reporting a guess.
REQUIRES=$(rpm -qp --requires "${ARTIFACT}" 2>/dev/null | sort -u | tr '\n' ' ')
[[ -n ${REQUIRES} ]] || fail "the package declares no dependencies, which cannot be right"

# --- smoke test --------------------------------------------------------------
"${PACKAGING_DIR}/smoke-test-rpm.sh" "${ARTIFACT}"

# --- summary -----------------------------------------------------------------
printf '\n'
note "version       ${VERSION}-${RPM_RELEASE}"
note "artifact      ${ARTIFACT}"
note "sha256        $(sha256sum "${ARTIFACT}" | cut -d' ' -f1)"
note "glibc floor   ${REQUIRED_GLIBC} (baseline ${EPHERVEX_BASELINE_GLIBC})"
note "requires      ${REQUIRES}"
