#!/usr/bin/env bash
# Fetches the host tools a release build needs and refuses any whose SHA-256
# does not match packaging/host-tools.
#
# Upstream publishes appimagetool and the AppImage runtime under rolling tags,
# so a pinned digest goes stale without anything in this repository changing.
# Every place that fetches them goes through this script, so there is one
# definition of what is fetched, what it is checked against, and what a
# mismatch says.
#
#   provision-host-tools.sh provision <directory>
#       Fetches the pins for the baseline architecture into <directory>,
#       extracts appimagetool, and prints the environment assignments
#       build-appimage.sh reads (APPIMAGETOOL and APPIMAGE_RUNTIME_FILE) on
#       standard output. Progress and errors go to standard error, so a
#       workflow can append the output to its environment file.
#
#   provision-host-tools.sh verify
#       Fetches the pins for every release architecture into a scratch
#       directory and checks them, so a stale pin is found before a tag exists.
#       Nothing is kept or extracted.
set -euo pipefail

fail() {
    printf 'provision-host-tools: %s\n' "$*" >&2
    exit 1
}

note() { printf '    %s\n' "$*" >&2; }

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=packaging/linux-baseline
source "${REPO_ROOT}/packaging/linux-baseline"
# shellcheck source=packaging/host-tools
source "${REPO_ROOT}/packaging/host-tools"

# Prints the path of the fetched file. Each pin is one "<url> <sha256>" pair
# and neither field can contain a space.
fetch() {
    local pin=$1 arch=$2 directory=$3 url expected file actual
    read -r url expected < <("${pin}" "${arch}")
    file="${directory}/${url##*/}"
    curl --proto '=https' --tlsv1.2 -sSfL -o "${file}" "${url}" ||
        fail "could not fetch ${url}"
    actual=$(sha256sum "${file}" | cut -d' ' -f1)
    [[ ${actual} == "${expected}" ]] ||
        fail "${url##*/} is not what packaging/host-tools pins for ${arch}.
  expected ${expected}
  fetched  ${actual}
Upstream replaced it under its rolling tag. Check the new binary, then update
packaging/host-tools deliberately."
    chmod +x "${file}"
    note "${arch}: ${url##*/} matches"
    printf '%s\n' "${file}"
}

case "${1:-}" in
provision)
    [[ $# -eq 2 ]] || fail "usage: $0 provision <directory>"
    mkdir -p -- "$2"
    directory=$(cd -- "$2" && pwd)
    appimagetool=$(fetch ephervex_appimagetool "${EPHERVEX_BASELINE_ARCH}" "${directory}")
    runtime=$(fetch ephervex_appimage_runtime "${EPHERVEX_BASELINE_ARCH}" "${directory}")
    # appimagetool ships as an AppImage, and a container has no FUSE to mount
    # one with, so it is extracted once and run from the AppDir.
    (cd -- "${directory}" && "${appimagetool}" --appimage-extract >/dev/null)
    printf 'APPIMAGETOOL=%s/squashfs-root/AppRun\n' "${directory}"
    printf 'APPIMAGE_RUNTIME_FILE=%s\n' "${runtime}"
    ;;
verify)
    [[ $# -eq 1 ]] || fail "usage: $0 verify"
    scratch=$(mktemp -d)
    trap 'rm -rf "${scratch}"' EXIT
    for arch in ${EPHERVEX_BASELINE_ARCHITECTURES}; do
        mkdir -p -- "${scratch}/${arch}"
        fetch ephervex_appimagetool "${arch}" "${scratch}/${arch}" >/dev/null
        fetch ephervex_appimage_runtime "${arch}" "${scratch}/${arch}" >/dev/null
    done
    ;;
*)
    fail "usage: $0 provision <directory> | verify"
    ;;
esac
