# Ephervex documentation website

This Docusaurus site uses screenshots of the actual Ephervex Slint window.
The Rust generator uses synthetic fixtures and writes original PNGs locally.
The website uses optimized WebP copies.

The installation page is a standalone route at `/install`. The v1 docs have a
Getting Started guide at `/docs/v1/getting-started` with a link to the
installation page. The page suggests a Linux architecture when the browser
exposes one, but the visitor can always choose the other architecture. It
reads the latest GitHub release to link versioned packages and falls back to
the releases list when release metadata is unavailable.

The navbar logo, favicon, and social image in `static/img/` are symlinks to
artwork under the repository's `assets/` directory. Build from a full
repository checkout so Docusaurus can copy those files into `build/img/`.

From the repository root, regenerate and synchronize the default 1080p assets:

```bash
./tools/auto-screenshot-tool/generate.sh
cd web
npm ci
npm run screenshots:sync
npm run typecheck
npm run build
```

To publish both 1080p and 1440p, run
`npm run screenshots:sync -- --size 1920x1080 --size 2560x1440` instead of
the default sync command. The Rust generator keeps both sizes configurable.
The WebP files under `static/img/ephervex/` and their manifest under
`src/components/` are the site assets. The original PNGs and contact sheets
remain gitignored under `tools/auto-screenshot-tool/screenshots/`. Generated
WebP files under `static/img/ephervex/` are also ignored. A fresh checkout must
generate and synchronize images before building.

`npm run screenshots:sync` checks that both themes exist for every selected
state, region, and size. Full-window WebPs live directly under each state;
named crops have a subdirectory. It hashes the source PNGs and reuses unchanged
WebPs.
`npm run screenshots:contact-sheet` writes local review sheets grouped by
resolution and region type. `npm run screenshots:check` validates the published
assets and runs automatically before `npm run build`. An ordinary local build
does not regenerate or recompress images. The Pages workflow regenerates the
screenshots before publishing `web/build/`.

The CLI reference under `docs/reference/cli/` is generated from the CLI's
`--help` output and every `help <path>` command. It is gitignored. The
`prebuild` and `prestart` scripts run `npm run cli:reference:sync`, which builds
the CLI and writes the pages before Docusaurus reads them. Run the sync command
directly if you need the pages without starting or building the site.
`npm run cli:reference:check` verifies an existing generated copy. Set
`EPHERVEX_CLI_BIN` to an already built Ephervex CLI binary when generating
the reference for a specific build.

`ThemeScreenshot` reads the synchronized manifest. It renders a sized
placeholder for server rendering and hydration, then mounts only the image
for Docusaurus's selected color mode. `useBaseUrl` applies the site's
configured base URL. Set `DOCUSAURUS_BASE_URL=/your-path/` for a subpath
deployment. New screenshot states need only a fixture and catalog
entry in the Rust tool; the sync command discovers their output directories.
The `/screenshots` page lists every state in `src/components/screenshot-catalog.ts`;
a state missing there appears under "Other". Selecting an image opens a viewer
with previous and next buttons, the arrow keys, and swipe on touch screens.

## Site configuration and docs versions

The production domain is `https://ephervex.chaotictrials.de` and `static/CNAME`
records it. Configure the GitHub repository's Pages source as **GitHub Actions**
and point the domain's DNS at GitHub Pages. `.github/workflows/pages.yml` builds
and uploads `web/build/`, then deploys it with `actions/deploy-pages`; deployment
does not create a commit.

Pages deploys after a successful GitHub Release workflow or a manually published
release. A push to `main` that changes `web/`, `tools/auto-screenshot-tool/`,
`assets/`, or the Pages workflow also deploys when a published full release
exists. The latest published full release supplies the application source and
Cargo lockfile. On `main` pushes, the workflow applies the current website,
screenshot tool, and artwork to that released source. If a capture needs an
unreleased application change, the build fails and the prior Pages deployment
stays in place. Manual workflow dispatch can retry the current `main` source.
Each build regenerates both screenshot sizes and WebP assets without committing
generated files. GitHub release downloads are the installation page's source;
see `.github/workflows/release.yml` for artifact names and architectures.

Edit `docs/` for v1, which includes all pre-v1 releases and the v1 series.
The current docs version is labeled `v1` and served at `/docs/v1/`. There is
one editable copy until the next major version needs separate documentation.
This follows [Docusaurus's current-version configuration](https://docusaurus.io/docs/versioning#configuring-versioning-behavior).

When v2 documentation starts:

1. Run `npm run docusaurus -- docs:version v1` to archive v1 in
   `versioned_docs/version-v1/` and `versioned_sidebars/`.
2. In `docusaurus.config.ts`, change `versions.current` to label `v2` and
   path `v2`, and add `versions.v1` with label `v1`, path `v1`, and
   banner `none`. Keep `lastVersion: 'current'` when v2 becomes the default.
3. Update the homepage and footer links to `/docs/v2/`, then edit `docs/`
   for v2. Corrections to v1 go in `versioned_docs/version-v1/`.
4. Commit the generated version files with the configuration changes.

The version dropdown lists the current version and archived versions. Explicit
version paths keep v1 links stable after v2 is added. While v2 is unpublished,
set `lastVersion: 'v1'` and `includeCurrentVersion: false`, and retain v1 in
homepage and footer links.
