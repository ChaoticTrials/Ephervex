# Ephervex documentation website

This Docusaurus site uses screenshots of the actual Ephervex Slint window.
The Rust generator uses synthetic fixtures and writes original PNGs locally.
The website uses optimized WebP copies.
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
WebP files under `static/img/ephervex/` are also ignored until publication.
A fresh checkout must generate and synchronize images before building.

`npm run screenshots:sync` checks that both themes exist for every selected
state, region, and size. Full-window WebPs live directly under each state;
named crops have a subdirectory. It hashes the source PNGs and reuses unchanged
WebPs.
`npm run screenshots:contact-sheet` writes local review sheets grouped by
resolution and region type. `npm run screenshots:check` validates the published
assets and runs automatically before `npm run build`. An ordinary local build
does not run Rust or recompress images. A clean CI job can run the command
sequence above before publishing `web/build/`.

`ThemeScreenshot` reads the synchronized manifest. It renders a sized
placeholder for server rendering and hydration, then mounts only the image
for Docusaurus's selected color mode. `useBaseUrl` applies the site's
configured base URL. Set `DOCUSAURUS_BASE_URL=/your-path/` for a subpath
deployment. New screenshot states need only a fixture and catalog
entry in the Rust tool; the sync command discovers their output directories.

## Site configuration and docs versions

The production domain is `https://ephervex.chaotictrials.de`, with GitHub Pages
hosting planned. `static/CNAME` records the domain; no deployment workflow is
configured yet. GitHub release downloads are the installation page's source;
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
