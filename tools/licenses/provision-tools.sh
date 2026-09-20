#!/usr/bin/env bash
# Installs the pinned license tools into a directory and prints it.
#
# One implementation, shared by every workflow that needs them, so no two of
# them can end up checking a different digest or running a different version.
# Nothing is built: these are the upstream release binaries, and each is
# refused unless its digest is the one tools/licenses/license-tools records.
#
# The directory it prints goes on PATH; `cargo about` and `cargo deny` then
# work as the cargo subcommands they are.
#
#   usage: eval "$(tools/licenses/provision-tools.sh <directory>)"
#      or: tools/licenses/provision-tools.sh <directory> >/dev/null
set -euo pipefail

fail() {
    printf 'provision-license-tools: %s\n' "$*" >&2
    exit 1
}

[[ $# -eq 1 ]] || fail "usage: $0 <directory>"
DESTINATION=$1

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
# shellcheck source=tools/licenses/license-tools
source "${REPO_ROOT}/tools/licenses/license-tools"

for tool in curl sha256sum tar; do
    command -v "${tool}" >/dev/null || fail "${tool} is required and was not found"
done

mkdir -p "${DESTINATION}"
DESTINATION=$(cd -- "${DESTINATION}" && pwd)
WORKSPACE=$(mktemp -d)
cleanup() { rm -rf "${WORKSPACE}"; }
trap cleanup EXIT

# Fetches one pinned tarball, checks it, and puts the binary it carries in the
# destination. The binary's name is the archive's name up to the version, which
# is how both of these are laid out.
install_tool() {
    local name=$1 url=$2 expected=$3 archive actual extracted
    archive="${WORKSPACE}/${name}.tar.gz"
    curl --proto '=https' --tlsv1.2 -sSfL -o "${archive}" "${url}"
    actual=$(sha256sum "${archive}" | cut -d' ' -f1)
    [[ ${actual} == "${expected}" ]] || fail \
        "${name} is not what tools/licenses/license-tools pins.
  expected ${expected}
  fetched  ${actual}
These are pinned at an exact version, so this is a changed asset rather than a
new release. Check what upstream published before updating the digest."
    tar xzf "${archive}" -C "${WORKSPACE}"
    extracted=$(find "${WORKSPACE}" -type f -name "${name}" -perm -u+x | head -n 1)
    [[ -n ${extracted} ]] || fail "the ${name} archive carries no ${name} binary"
    install -m 0755 "${extracted}" "${DESTINATION}/${name}"
}

install_tool cargo-about "${EPHERVEX_CARGO_ABOUT_URL}" "${EPHERVEX_CARGO_ABOUT_SHA256}"
install_tool cargo-deny "${EPHERVEX_CARGO_DENY_URL}" "${EPHERVEX_CARGO_DENY_SHA256}"

printf '%s\n' "${DESTINATION}"
