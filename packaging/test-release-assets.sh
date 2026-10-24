#!/usr/bin/env bash
# Checks which files a release stages and what stops a publish before it talks
# to a server.
#
# A release that ships the wrong file under the right name, or a checksum that
# covers something else, is a mislabeled release that nothing downstream
# notices. So the staging script is run against a directory of dummy artifacts
# for every release architecture, and the publish script is run only as far as
# its refusals, which come before any network access.
#
# Needs no toolchain, no build, and no network.
#
#   usage: packaging/test-release-assets.sh
set -euo pipefail

FAILURES=0

fail() {
    printf 'test-release-assets: %s\n' "$*" >&2
    exit 1
}

check() {
    local what=$1 expected=$2 actual=$3
    if [[ ${actual} == "${expected}" ]]; then
        printf '  ok   %s\n' "${what}"
    else
        printf '  FAIL %s: expected %s, got %s\n' "${what}" "${expected}" "${actual}" >&2
        FAILURES=$((FAILURES + 1))
    fi
}

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
PACKAGING_DIR="${REPO_ROOT}/packaging"

# shellcheck source=packaging/linux-baseline
source "${PACKAGING_DIR}/linux-baseline"
# shellcheck source=packaging/linux-architectures
source "${PACKAGING_DIR}/linux-architectures"

VERSION=$("${PACKAGING_DIR}/workspace-version.sh")
SCRATCH=$(mktemp -d)
trap 'rm -rf "${SCRATCH}"' EXIT

# What a release of one architecture consists of, in the order it is staged.
artifacts_for() {
    printf '%s\n' \
        "Ephervex-$(ephervex_appimage_arch "$1").AppImage" \
        "ephervex_${VERSION}_$(ephervex_deb_arch "$1").deb" \
        "ephervex-${VERSION}-1.$(ephervex_rpm_arch "$1").rpm"
}

# Runs a command and prints its exit status, with its output kept in
# ${SCRATCH}/output.
status_of() {
    local status=0
    "$@" >"${SCRATCH}/output" 2>&1 || status=$?
    printf '%s\n' "${status}"
}

for arch in ${EPHERVEX_BASELINE_ARCHITECTURES}; do
    printf '==> staging a %s release\n' "${arch}"
    built="${SCRATCH}/${arch}/built"
    staging="${SCRATCH}/${arch}/staging"
    mkdir -p -- "${built}"
    expected=$(artifacts_for "${arch}")
    while read -r artifact; do
        printf 'contents of %s\n' "${artifact}" >"${built}/${artifact}"
    done <<<"${expected}"
    # What the packaging scripts leave beside the artifacts, which must not ship.
    mkdir -p -- "${built}/AppDir"
    printf 'leftover\n' >"${built}/ephervex-leftover.tar"

    check "staging succeeds" 0 \
        "$(status_of env EPHERVEX_BASELINE_ARCH="${arch}" "${PACKAGING_DIR}/stage-release-assets.sh" "${built}" "${staging}")"

    wanted=$(
        while read -r artifact; do printf '%s\n%s.sha256\n' "${artifact}" "${artifact}"; done <<<"${expected}" | sort
    )
    check "only the artifacts and their checksums are staged" "${wanted}" \
        "$(find "${staging}" -maxdepth 1 -type f -printf '%P\n' | sort)"

    while read -r artifact; do
        check "the checksum beside ${artifact} verifies it" "${artifact}: OK" \
            "$(cd -- "${staging}" && sha256sum --check -- "${artifact}.sha256")"
    done <<<"${expected}"

    printf '==> refusing an incomplete %s build\n' "${arch}"
    first=$(head -n 1 <<<"${expected}")
    rm -- "${built}/${first}"
    check "a missing artifact fails staging" 1 \
        "$(status_of env EPHERVEX_BASELINE_ARCH="${arch}" "${PACKAGING_DIR}/stage-release-assets.sh" "${built}" "${SCRATCH}/${arch}/refused")"
    if ! grep -q -- "${first} was not built" "${SCRATCH}/output"; then
        printf '  FAIL the failure names the missing artifact %s\n' "${first}" >&2
        FAILURES=$((FAILURES + 1))
    fi
done

printf '==> publishing without credentials\n'
check "an empty token fails before anything is staged" 1 \
    "$(status_of env TOKEN= GITHUB_API_URL=http://127.0.0.1:9 GITHUB_REPOSITORY=owner/repo GITHUB_REF_NAME="v${VERSION}" \
        "${PACKAGING_DIR}/publish-forgejo-release.sh" "${SCRATCH}/x86_64/built" "${SCRATCH}/unstaged")"
if ! grep -q 'RELEASE_TOKEN' "${SCRATCH}/output"; then
    printf '  FAIL the failure names the RELEASE_TOKEN secret\n' >&2
    FAILURES=$((FAILURES + 1))
fi
check "nothing is staged without a token" no \
    "$([[ -e ${SCRATCH}/unstaged ]] && echo yes || echo no)"

if ((FAILURES > 0)); then
    fail "${FAILURES} check(s) failed"
fi
printf 'all release asset checks passed\n'
