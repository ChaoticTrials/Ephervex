#!/usr/bin/env bash
# Copies the release artifacts for one architecture out of the packaging output
# directory and writes a SHA-256 checksum beside each.
#
# The packaging scripts leave the AppDir and the two staging trees they
# assembled in the same output directory, so what ships is copied out by name
# instead of left for a glob to find. The names come from the architecture
# table, which is also what produced them. Both release workflows go through
# this script, so there is one definition of which files make a release and of
# how they are checksummed.
#
# The checksum file sits next to the artifact it covers, so a download can be
# checked without trusting the page it came from.
#
#   usage: packaging/stage-release-assets.sh <built-directory> <staging-directory>
#
# The architecture is EPHERVEX_BASELINE_ARCH (packaging/linux-baseline), the
# same value the packaging scripts built for.
set -euo pipefail

fail() {
    printf 'stage-release-assets: %s\n' "$*" >&2
    exit 1
}

[[ $# -eq 2 ]] || fail "usage: $0 <built-directory> <staging-directory>"
BUILT=$1
STAGING=$2

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=packaging/linux-baseline
source "${REPO_ROOT}/packaging/linux-baseline"
# shellcheck source=packaging/linux-architectures
source "${REPO_ROOT}/packaging/linux-architectures"

VERSION=$("${REPO_ROOT}/packaging/workspace-version.sh")
APPIMAGE_ARCH=$(ephervex_appimage_arch "${EPHERVEX_BASELINE_ARCH}")
DEB_ARCH=$(ephervex_deb_arch "${EPHERVEX_BASELINE_ARCH}")
RPM_ARCH=$(ephervex_rpm_arch "${EPHERVEX_BASELINE_ARCH}")

mkdir -p -- "${STAGING}"
for artifact in \
    "Ephervex-${APPIMAGE_ARCH}.AppImage" \
    "ephervex_${VERSION}_${DEB_ARCH}.deb" \
    "ephervex-${VERSION}-1.${RPM_ARCH}.rpm"; do
    [[ -f ${BUILT}/${artifact} ]] || fail "${artifact} was not built."
    cp -- "${BUILT}/${artifact}" "${STAGING}/${artifact}"
    (cd -- "${STAGING}" && sha256sum -- "${artifact}" >"${artifact}.sha256")
done
ls -l -- "${STAGING}"
