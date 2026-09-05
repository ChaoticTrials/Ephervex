#!/usr/bin/env bash
# Prints the version this source tree releases as.
#
# One reader for it, because two things need the same answer and a disagreement
# between them is a mislabeled release: the packaging script reports what it
# built, and the release workflow checks the pushed tag against it. Every crate
# inherits the version from `[workspace.package]`, so the root manifest is where
# it lives.
#
#   usage: packaging/workspace-version.sh
set -euo pipefail

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
MANIFEST="${REPO_ROOT}/Cargo.toml"

VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "${MANIFEST}" | head -n 1)
if [[ -z ${VERSION} ]]; then
    printf 'workspace-version: no version in %s\n' "${MANIFEST}" >&2
    exit 1
fi

printf '%s\n' "${VERSION}"
