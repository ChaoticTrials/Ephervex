#!/usr/bin/env bash
# Publishes the built Linux release artifacts as a release on the Forgejo
# instance the workflow runs on.
#
# The artifacts and their checksums are staged by packaging/stage-release-assets.sh.
# The release for the tag is then reused if it exists and created otherwise, and
# every staged file is uploaded to it.
#
# Publishing over an existing release replaces its assets rather than adding to
# them. Forgejo accepts a second attachment under a name it already carries,
# which would leave the release offering two different files under one name and
# a download address unable to say which one it means.
#
#   usage: packaging/publish-forgejo-release.sh <built-directory> <staging-directory>
#
# Reads the environment a Forgejo Actions run provides (GITHUB_API_URL,
# GITHUB_REPOSITORY, GITHUB_REF_NAME) and TOKEN, a personal access token with
# repository write access. The automatic per-run token is not used, so the
# release is attributed to an account rather than to the "Ghost" user.
set -euo pipefail

fail() {
    printf 'publish-forgejo-release: %s\n' "$*" >&2
    exit 1
}

[[ $# -eq 2 ]] || fail "usage: $0 <built-directory> <staging-directory>"
BUILT=$1
STAGING=$2

if [[ -z ${TOKEN:-} ]]; then
    fail "the RELEASE_TOKEN secret is empty or not set.
Create a personal access token with repository WRITE scope and
add it under Settings -> Actions -> Secrets as RELEASE_TOKEN."
fi
for variable in GITHUB_API_URL GITHUB_REPOSITORY GITHUB_REF_NAME; do
    [[ -n ${!variable:-} ]] || fail "${variable} is not set; this runs inside a release workflow."
done

REPO_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
"${REPO_ROOT}/packaging/stage-release-assets.sh" "${BUILT}" "${STAGING}"

API=${GITHUB_API_URL}
REPO=${GITHUB_REPOSITORY}
TAG=${GITHUB_REF_NAME}
AUTH="Authorization: token ${TOKEN}"

# curl that prints the server's error body rather than just a number, and
# fails on any HTTP >= 400.
api() {
    local method=$1 path=$2
    shift 2
    curl -sS --fail-with-body -X "${method}" -H "${AUTH}" "$@" "${API}${path}"
}

# Reuse an existing release for this tag if there is one, else create it.
existing=$(curl -sS -H "${AUTH}" "${API}/repos/${REPO}/releases/tags/${TAG}" || true)
release=$(jq -r '.id // empty' <<<"${existing}")

if [[ -z ${release} ]]; then
    echo "Creating release for ${TAG}"
    created=$(api POST "/repos/${REPO}/releases" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg tag "${TAG}" \
            '{tag_name: $tag, name: $tag,
              body: ("Ephervex " + $tag + " for Linux x86_64.")}')")
    release=$(jq -r '.id' <<<"${created}")
else
    echo "Reusing existing release for ${TAG}"
fi

# Everything staging holds, which is only ever the artifacts and their
# checksums.
assets=$(curl -sS -H "${AUTH}" "${API}/repos/${REPO}/releases/${release}/assets" || echo '[]')
mapfile -t names < <(find "${STAGING}" -maxdepth 1 -type f -printf '%P\n' | sort)
for name in "${names[@]}"; do
    for old in $(jq -r --arg n "${name}" '.[] | select(.name == $n) | .id' <<<"${assets}"); do
        echo "Removing the existing ${name} (asset ${old})"
        api DELETE "/repos/${REPO}/releases/${release}/assets/${old}" >/dev/null
    done
    echo "Uploading ${name}"
    api POST "/repos/${REPO}/releases/${release}/assets?name=${name}" \
        -F "attachment=@${STAGING}/${name}"
done
