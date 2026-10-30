# Road to v1.0.0 archive publication

This directory contains the Forgejo schema v1 export, `publication.json`, and
this README. The publication file pins the export's SHA-256 and currently has
`mode: "approved"`. That setting makes the archive routes indexable and adds
them to the sitemap. It does not verify the export's content or attest to a
manual publication review. The reserved unavailable record remains `noindex`
and is excluded from the sitemap.

Private snapshots, assets, review files, and API responses are not part of this
checkout. Store them outside the Git worktree if retained. The site export
omits Forgejo release records (`releases: []`); application pre-releases are
handled on GitHub.

## Architecture and stable URLs

`plugins/road-to-v1/index.ts` reads and validates this file during Docusaurus
`loadContent`, then uses `createData` and `addRoute` to produce one overview and
one static page for each issue or pull request. Detail pages receive only their
own entry and the set of known numbers used for cross references. The overview
receives searchable text from all exported entries once. Nothing is
fetched from Forgejo by the browser or during the website build. Search and
filters run on the overview in memory; results are paged at 30 entries. The
summary counts act as shortcuts to issue, PR, merged, and discussed-thread
filters. Label accents use the validated colors from the public export.

`src/archive/routes.ts` owns the permanent contract:

- `/road-to-v1/`
- `/road-to-v1/forgejo/issues/{number}/`
- `/road-to-v1/forgejo/pulls/{number}/`

Docusaurus `Link` and `useBaseUrl` add the deployment prefix without changing
these canonical paths. Keep the paths stable when changing the archive or site.
Unknown numbers have no generated route and return 404 on a normal static host.

The current export has issue #3 but no pull request #3. Because the latter URL
was explicitly reserved in the migration contract, the build prerenders an
honest unavailable-record page at `/road-to-v1/forgejo/pulls/3/`. It does not
invent a PR. The page is `noindex` and is excluded from the sitemap even after
publication approval. If a future export contains PR #3,
the normal PR detail page replaces it automatically.

## Schema v1 contract

The input is `archive-data.json` made by
`forgejo_archive.py prepare-public`. `src/archive/schema.ts` defines and checks
the contract at build time: `schemaVersion: 1`; archive metadata and the exact
route contract; arrays of `issues`, `pulls`, `milestones`, `labels`, and
`releases`; this website requires the releases array to be empty. Issue and PR
numbers are unique and positive across both collections. Entries
carry kind, title, state, author, timestamps, Markdown body, label names,
milestone, assignees, and comments. Pull requests additionally carry merge
state and time, branch names, and reviews with review comments. Dates and
optional fields may be `null`. Empty arrays are valid. The build fails on
missing required fields, unsupported versions, bad dates, or duplicate IDs.

Schema v1 does not contain a complete Forgejo event timeline, commit diffs, or
attachments. The interface lists the exported records without implying a
complete event chronology. Markdown references such as `#295`
become archive links only when the exported number exists. Raw HTML is skipped,
unsafe URL schemes and internal `.lan` links are blocked, and historical images
are represented by a missing asset note instead of being fetched.

## Replace the site export

The Forgejo instance is archived and read-only, and the project continues on
GitHub. The committed `archive-data.json` was exported once and then edited:
commit-hash text in issue and comment bodies was translated to the hashes of the
GitHub history, and one comment was changed to drop a merge hash. Running the
exporter again reproduces neither change, so regenerate the file only if you can
repeat those edits. Edit the committed file in place for small corrections and
update the digest in `publication.json`.

1. Choose private export and review directories outside this checkout. From
   `web/`, run `forgejo_archive.py export --out DIR` to write the private
   snapshot (add `--forgejo` and `--repo` if the instance is not at the
   defaults, and `--force` to replace an existing snapshot), then
   `verify --input DIR`, then `prepare-public --input DIR --out REVIEW_DIR`.
   `prepare-public` needs an empty `REVIEW_DIR` and stops on export warnings
   unless `--allow-incomplete` is given after review. The exporter is at
   `scripts/forgejo-archive/forgejo_archive.py`. Supply `FORGEJO_TOKEN` through
   your local shell. Keep the token, private snapshot, assets, and API responses
   out of the repository.
2. Review the public candidate's issue and PR text, comments, reviews, links,
   people, hostnames, licenses, warnings, and missing API capabilities.
   `prepare-public` writes the candidate `archive-data.json`, a
   `REVIEW-BEFORE-PUBLISHING.md` checklist, and `PUBLIC-DATA-SHA256.txt`; it does
   not complete the manual review. The exporter keeps Forgejo releases in the private
   snapshot and writes `releases: []` to the site export.
3. Copy only the candidate's `archive-data.json` into this directory. Calculate
   its SHA-256 and update `publication.json` with the exact digest. Use
   `mode: "testing"` while reviewing. The build rejects a digest mismatch.
   Record the review decision and digest outside the website before setting
   `mode: "approved"`; the flag itself does not prove the review happened.
4. Run `npm run archive:test`, `npm run typecheck`, `npm run build`, and
   `npm run archive:check`. Inspect the generated routes, sitemap, pages, and
   search results. For a nonroot deployment, build with
   `DOCUSAURUS_BASE_URL=/preview/` and run
   `npm run archive:check -- /preview/`. `archive:check` expects specific
   records from the current export (issues #13, #295, and #424, and at least one
   pull request); update `scripts/archive-check.mjs` when replacing the data with
   an export that lacks them. `npm run archive:exporter:test` covers the
   exporter.

The road-to-v1 plugin fails the build if a file named `snapshot.json` or
`snapshot.backup.<stamp>.json` exists directly in `web/` or anywhere under
`archive/`, `src/`, `static/`, `docs/`, `plugins/`, `scripts/`, or `build/`. Git
also ignores those names. This is a guardrail, not a substitute for reviewing all public text:
the site export is shipped in build HTML and search bundles.

## Portable public download

There is currently **no download link or published ZIP**. The exporter's
`package-public` command can build a reproducible ZIP from a reviewed
`archive-data.json` by requiring its exact SHA-256 with `--approve-sha256`. The
ZIP contains `archive-data.json`, a `README.md`, one Markdown file per issue and
pull request, `manifest.json`, and `SHA256SUMS`; it is not a Forgejo server
backup. Package the file the site actually publishes: use a directory that holds
an identical `archive-data.json`, such as `web/archive`, as `archive_review_dir`
and pass the digest from `publication.json`. After setting `archive_zip` to a
path outside this checkout, run from `web/`:

```bash
sha256sum "$archive_review_dir/archive-data.json"
python3 scripts/forgejo-archive/forgejo_archive.py package-public \
  --input "$archive_review_dir" \
  --out "$archive_zip" \
  --approve-sha256 REVIEWED_DATA_SHA256
sha256sum "$archive_zip"
```

Verify the finished ZIP with `sha256sum <zip>` and publish its checksum beside
the asset only after reviewing the package contents and hosting location.
Add a website link only once the URL, approval, package bytes, and published
SHA-256 have all been verified. Never link to `snapshot.json` or automatically
copy the private backup into the site.

Original attachments, Forgejo release records, and release binaries are omitted.
Links to files on the archived Forgejo instance may no longer work, and links
with a `.lan` host are not rendered. The site export may also omit Forgejo API
data that the server did not provide. Keep the private snapshot and backups
separate from the public website.
