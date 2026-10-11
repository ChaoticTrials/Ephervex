# Road to v1.0.0 archive publication

This directory contains only `archive-data.json` and publication metadata. The
committed file is the public Forgejo schema v1 export with issue and PR history.
`publication.json` pins its exact SHA-256 and has `mode: "approved"`. Archive
routes are indexable and included in the sitemap. The reserved unavailable
record remains `noindex` and excluded from the sitemap.

Keep private preservation copies such as `snapshot.json` and separately stored
assets outside the Git worktree. Neither belongs in `web/` or Git. The public
JSON intentionally omits Forgejo release records (`releases: []`); future
application pre-releases are handled on GitHub, not in this archive dataset.

## Architecture and stable URLs

`plugins/road-to-v1/index.ts` reads and validates this file during Docusaurus
`loadContent`, then uses `createData` and `addRoute` to produce one overview and
one static page for each issue or pull request. Detail pages receive only their
own entry and the set of known numbers used for cross references. The overview
receives searchable text from all approved public entries once. Nothing is
fetched from Forgejo by the browser or during the website build. Search and
filters run on the overview in memory; results are paged at 30 entries. The
summary counts act as shortcuts to issue, PR, merged, and discussed-thread
filters. Label accents use the validated colors from the public export.

`src/archive/routes.ts` owns the permanent contract:

- `/road-to-v1/`
- `/road-to-v1/forgejo/issues/{number}/`
- `/road-to-v1/forgejo/pulls/{number}/`

Docusaurus `Link` and `useBaseUrl` add the deployment prefix without changing
these canonical paths. **Finalize and freeze these URL contracts before
historical Git commit messages are rewritten for GitHub.** Future redesigns
must keep them. Unknown numbers have no generated route and return 404 on a
normal static host.

The current export has issue #3 but no pull request #3. Because the latter URL
was explicitly reserved in the migration contract, the build prerenders an
honest unavailable-record page at `/road-to-v1/forgejo/pulls/3/`. It does not
invent a PR. The page is `noindex` and is excluded from the sitemap even after
publication approval. If a future reviewed export actually contains PR #3,
the normal PR detail page replaces it automatically.

## Schema v1 contract

The input is the reviewed `archive-data.json` made by
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

## Refresh the public export

1. Keep the private `snapshot.json`, assets, token, and API responses **outside
   this repository**. From `web/`, use
   `python3 scripts/forgejo-archive/forgejo_archive.py export --out "$HOME/ephervex-road-private" --download-assets`,
   followed by `verify --input "$HOME/ephervex-road-private"` and
   `prepare-public --input "$HOME/ephervex-road-private" --out "$HOME/ephervex-road-public-review"`
   with the same script path. Set `FORGEJO_TOKEN` only in your own local shell;
   never put it in a command, chat, file, or build environment. Review every
   issue, PR, comment, review
   body, link, quote, person, hostname, and license. Investigate export warnings
   and missing API capabilities. `prepare-public` produces a review candidate,
   not a safety guarantee.
2. Calculate `sha256sum archive-data.json` on the exact candidate bytes and
   copy only that file here. Set `publication.json` to
   `{"mode":"testing","sha256":"<the 64-character digest>"}` during review.
   A changed data file without matching metadata fails the build. The exporter
   keeps Forgejo releases in the private snapshot and writes `releases: []`
   publicly. After a full review of the final bytes, retain the sign-off record
   and digest outside the website, then set the publication mode to `approved`.
3. Run `npm run archive:test`, `npm run typecheck`, `npm run build`, and
   `npm run archive:check`. Check the generated route list, sitemap, rendered
   pages, and search results before publishing. Each new approved export
   regenerates the site with the same steps. For a nonroot deployment, build
   with `DOCUSAURUS_BASE_URL=/preview/` and run
   `npm run archive:check -- /preview/`.

The build checks for a file named `snapshot.json` or `snapshot.backup.*.json`
under the website source and fails if one appears. Git also ignores those
names. This is a guardrail, not a substitute for reviewing all public text:
the committed public export is shipped in build HTML and search bundles.

## Portable public download

There is currently **no download link or published ZIP**. The bundled exporter's
`package-public` command can build a reproducible ZIP from the reviewed
`archive-data.json` by requiring its exact SHA-256 with
`--approve-sha256`. The ZIP contains the public JSON, readable Markdown
exports, a manifest, and `SHA256SUMS`; it is not a Forgejo server backup.
From `web/`, run:

```bash
sha256sum "$HOME/ephervex-road-public-review/archive-data.json"
python3 scripts/forgejo-archive/forgejo_archive.py package-public \
  --input "$HOME/ephervex-road-public-review" \
  --out "$HOME/ephervex-road-to-v1-public.zip" \
  --approve-sha256 REVIEWED_DATA_SHA256
sha256sum "$HOME/ephervex-road-to-v1-public.zip"
```

Verify the finished ZIP with `sha256sum <zip>` and publish its checksum beside
the asset only after reviewing the package contents and hosting location.
Add a website link only once the URL, approval, package bytes, and published
SHA-256 have all been verified. Never link to `snapshot.json` or automatically
copy the private backup into the site.

Original attachments, Forgejo release records, and release binaries are omitted,
and links to files on
the retiring Forgejo instance may no longer work. The public export may also
omit Forgejo API features unavailable on that server. Keep the private backup
and Git mirror separately for preservation; the public website is a curated
historical record.
