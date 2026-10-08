# Ephervex documentation website

This Docusaurus site uses screenshots of the actual Ephervex Slint window.
The Rust generator uses synthetic fixtures and writes original PNGs locally.
The website uses optimized WebP copies.

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
remain gitignored under `tools/auto-screenshot-tool/screenshots/`.

`npm run screenshots:sync` checks that both themes exist for every selected
state, region, and size. It hashes the source PNGs and reuses unchanged WebPs.
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
