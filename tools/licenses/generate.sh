#!/usr/bin/env bash
# Regenerates the third-party license inventory from the dependency graph.
#
# Two steps, kept apart because they answer different questions. `cargo-about`
# reads Cargo.lock and each crate's own license file and decides what Ephervex
# depends on and under what terms; tools/licenses/inventory.py turns that into
# the two committed documents. Neither invents a license: what a crate is
# licensed under comes from the crate.
#
# `--frozen` is `--locked --offline` together, and both halves matter.
# `--locked` means the graph is the committed one rather than whatever a warm
# registry resolved to, and `--offline` means license texts come from the crate
# sources on disk rather than from a remote metadata service whose answers can
# change between two runs. Generation is therefore reproducible from the
# lockfile, which is what makes the staleness check in CI meaningful.
#
# One host dependency:
#
#   * `cargo-about`, which this never installs. See docs/release.md.
#
#   usage: tools/licenses/generate.sh [--check]
set -euo pipefail

fail() {
    printf 'generate-licenses: %s\n' "$*" >&2
    exit 1
}

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)

command -v cargo >/dev/null || fail "cargo is required and was not found"
cargo about --version >/dev/null 2>&1 || fail \
    "cargo-about is required and was not found.
Install the version docs/release.md pins:

  cargo install cargo-about --locked --version <pinned>

This script never installs it, for the same reason packaging never downloads
its host tools: what a release contains is decided in a reviewed file."

command -v python3 >/dev/null || fail "python3 is required and was not found"

RAW=$(mktemp)
cleanup() { rm -f "${RAW}"; }
trap cleanup EXIT

# The crate sources have to be on disk for `--offline` to find their license
# files. This is the one network step, and it fetches exactly what Cargo.lock
# names.
cargo fetch --locked --manifest-path "${REPO_ROOT}/Cargo.toml" >/dev/null

cargo about generate \
    --format json \
    --workspace \
    --frozen \
    --fail \
    -c "${REPO_ROOT}/about.toml" \
    -m "${REPO_ROOT}/Cargo.toml" \
    -o "${RAW}"

python3 "${REPO_ROOT}/tools/licenses/inventory.py" --from-cargo-about "${RAW}" \
    --about-config "${REPO_ROOT}/about.toml" "$@"
