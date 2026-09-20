#!/usr/bin/env bash
# Prepares a release: sets the version, regenerates what the version changes,
# runs the gate, commits, and tags.
#
# It stops before publishing. Pushing the tag is what starts the release
# workflows and puts artifacts in front of people, so that stays a separate
# deliberate act; this script prints the two commands and ends.
#
# Everything here is a check the release would fail on anyway, run now instead
# of after a tag is already pushed. A tag that disagrees with the workspace
# version, a stale license inventory, or a failing test all stop a release
# workflow partway, and un-pushing a tag is worse than not pushing one.
#
# Safe to run again after a failure. Setting the version twice is a no-op, and
# nothing is committed until every check has passed.
#
#   usage: ./prepare_release.sh <version>
#          ./prepare_release.sh 1.0.0
set -euo pipefail

fail() {
    printf 'prepare-release: %s\n' "$*" >&2
    exit 1
}

step() { printf '\n==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }

[[ $# -eq 1 ]] || fail "usage: $0 <version>
For example: $0 1.0.0"
VERSION=$1

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "${REPO_ROOT}"

MANIFEST="${REPO_ROOT}/Cargo.toml"
TAG="v${VERSION}"

# --- what was asked for ------------------------------------------------------
# Three numbers and nothing else. The release workflows compare the tag against
# the workspace version literally, and the packaging scripts put the version in
# a Debian and an RPM file name, neither of which accepts everything semver
# does.
[[ ${VERSION} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "'${VERSION}' is not a release version. Use three numbers, for example 1.0.0."

CURRENT=$("${REPO_ROOT}/packaging/workspace-version.sh")
if [[ ${CURRENT} != "${VERSION}" ]]; then
    # A version that goes backwards is a typo every time. An unchanged one is a
    # re-run of this script, which is supported.
    [[ $(printf '%s\n%s\n' "${CURRENT}" "${VERSION}" | sort -V | tail -n 1) == "${VERSION}" ]] ||
        fail "this tree is ${CURRENT}, and ${VERSION} is older.
Releases only move forward. Check the version you meant."
fi

# --- the tree this would release --------------------------------------------
git rev-parse --git-dir >/dev/null 2>&1 || fail "this is not a git repository"
git diff --quiet && git diff --cached --quiet ||
    fail "there are uncommitted changes to tracked files.
A release is a commit of exactly the version bump and what it regenerates, so
commit or stash your own work first. \`git status\` shows what is pending."
git rev-parse --verify --quiet "refs/tags/${TAG}" >/dev/null &&
    fail "${TAG} already exists.
A published tag is not re-cut. Release the next version instead."

BRANCH=$(git rev-parse --abbrev-ref HEAD)
note "releasing ${VERSION} from ${BRANCH}, currently ${CURRENT}"

# --- the version -------------------------------------------------------------
step "setting the workspace version"
# Only the `version` under `[workspace.package]`. Every crate inherits from
# there, and a blind substitution would also rewrite dependency versions.
awk -v version="${VERSION}" '
    /^\[/ { in_package = ($0 == "[workspace.package]") }
    in_package && /^version *= *"/ && !done {
        print "version = \"" version "\""
        done = 1
        next
    }
    { print }
' "${MANIFEST}" >"${MANIFEST}.release" && mv "${MANIFEST}.release" "${MANIFEST}"

WRITTEN=$("${REPO_ROOT}/packaging/workspace-version.sh")
[[ ${WRITTEN} == "${VERSION}" ]] ||
    fail "the manifest says ${WRITTEN} after the edit, not ${VERSION}"
note "Cargo.toml is ${VERSION}"

# Cargo.lock records each workspace crate's own version, and the packaging
# scripts build with `--locked`. Without this the release build refuses the
# lockfile it was handed.
step "updating Cargo.lock"
cargo update --workspace --offline >/dev/null 2>&1 ||
    cargo update --workspace >/dev/null
# Checked against a workspace crate by name. A bare version grep would pass on
# any unrelated dependency that happens to carry the same three numbers.
awk -v version="${VERSION}" '
    /^name = "ephervex-core"$/ { found = 1; next }
    found { exit ($0 == "version = \"" version "\"") ? 0 : 1 }
    END { if (!found) exit 1 }
' Cargo.lock || fail "Cargo.lock still records another version for ephervex-core.
Run \`cargo update --workspace\` and check what it reported."
note "the lockfile agrees"

# --- what the version changes ------------------------------------------------
step "regenerating the third-party license inventory"
if ! cargo about --version >/dev/null 2>&1; then
    note "provisioning the pinned license tools"
    PATH="$("${REPO_ROOT}/tools/licenses/provision-tools.sh" license-tools)":"${PATH}"
    export PATH
fi
"${REPO_ROOT}/tools/licenses/generate.sh"

# --- the gate ----------------------------------------------------------------
# The same checks the release workflows run, in the same order, so a failure
# here is the failure a tag would have produced.
step "tooling tests"
python3 -m py_compile tools/minecraft-options/*.py tools/licenses/*.py
python3 -m unittest discover -s tools/minecraft-options
python3 -m unittest discover -s tools/licenses
"${REPO_ROOT}/packaging/test-linux-architectures.sh" >/dev/null
note "the datasets, the license builder, and the architecture table"

step "license policy"
if command -v cargo-deny >/dev/null || cargo deny --version >/dev/null 2>&1; then
    cargo deny check licenses
else
    PATH="$("${REPO_ROOT}/tools/licenses/provision-tools.sh" license-tools)":"${PATH}"
    export PATH
    cargo deny check licenses
fi

step "formatting"
cargo fmt --all --check

step "lints"
cargo clippy --workspace --all-targets -- -D warnings

step "tests"
# Offline by contract: fixtures and loopback servers only.
cargo test --workspace

step "command-line help"
cargo run -q -p ephervex-cli -- --help >/dev/null

# --- commit and tag ----------------------------------------------------------
step "committing"
# Named one by one. A release commit is the version bump and what the version
# regenerates, and nothing else that happens to be lying around.
git add Cargo.toml Cargo.lock crates/core/data/third-party-licenses.json \
    THIRD-PARTY-LICENSES.md
if git diff --cached --quiet; then
    fail "nothing changed, so there is nothing to release.
This tree is already ${VERSION} with an up-to-date license inventory. If you
meant to tag it, run: git tag ${TAG}"
fi
git --no-pager diff --cached --stat
git commit -q -m "chore: release ${VERSION}"
note "$(git log --oneline -1)"

step "tagging"
git tag -a "${TAG}" -m "Ephervex ${VERSION}"
note "${TAG} points at $(git rev-parse --short "${TAG}^{commit}")"

# --- what is left ------------------------------------------------------------
printf '\n'
step "${VERSION} is ready, and nothing has been published"
cat <<NEXT

Pushing the tag is what builds and publishes the release, so it is left to you:

    git push origin ${BRANCH}
    git push origin ${TAG}

To undo everything this did instead:

    git tag -d ${TAG} && git reset --hard HEAD~1

docs/release.md describes what the workflows then do, and the desktop smoke
test to run against the published AppImage before telling anyone it is there.
NEXT
