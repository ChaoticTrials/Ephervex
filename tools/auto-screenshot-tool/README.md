# Ephervex documentation screenshots

Run this command from the repository root to regenerate every documented UI state:

```bash
./tools/auto-screenshot-tool/generate.sh
```

The script changes to the repository root and runs the Rust generator. It
captures the compiled `AppWindow` with Slint's headless software renderer at a
fixed scale of 1, with a fixed `en-US` locale and mocked time. It uses the
same system font resolution as the launcher, including its monospace family.
No display server is needed. The Rust tool never attaches the
application controller, opens launcher storage, reads accounts or credentials,
or contacts a service. Its instance, server, offline account, and mod rows are
defined in `src/fixtures.rs` and contain only synthetic values. The CaveStone
pack fields are transcribed from a supplied Modrinth archive; its provenance
is recorded in `SOURCES.md`. Regeneration does not need that archive.

The default command writes both Ephervex Light and Dark captures at 1920x1080
and 2560x1440. Full-window files are stored at
`screenshots/<state>/<theme>-<width>x<height>.png`; crops are stored at
`screenshots/<state>/<region>/<theme>-<width>x<height>.png`. A full run replaces
the default screenshot directory, so removed states and old paths do not remain.
Some states also include a crop around an accessible UI element. A crop is taken
from the rendered window, not rebuilt separately. Light and Dark use the same
fixture object.
The PNG directory, including review contact sheets, is gitignored.

To capture a subset, pass options through the script:

```bash
./tools/auto-screenshot-tool/generate.sh --state instances-overview --theme light --size 1920x1080
```

Repeat `--state`, `--theme`, or `--size` to select more than one. `--output`
chooses a different output directory. `--locale` accepts a bundled locale;
currently that is `en-US`. A 1920x1080-only run is supported. The website
component lives in `web/src/components/`. From `web/`, synchronize selected
PNGs into optimized WebP assets and a matching manifest:

```bash
npm run screenshots:sync
```

Synchronization defaults to 1920x1080. To publish both resolutions, use
`npm run screenshots:sync -- --size 1920x1080 --size 2560x1440`. Unchanged
sources are not recompressed. The source PNGs remain in place. Use the
component in MDX, for example:

```tsx
<ThemeScreenshot state="instances-overview" alt="Ephervex instances" />
<ThemeScreenshot state="instances-overview" region="client-card" alt="CaveStone" />
```

The component renders the light capture in static HTML, then follows
Docusaurus's selected color mode after hydration. It reads the manifest and
uses responsive sources only for resolutions that were synchronized.

Generate review contact sheets with `cd web && npm run screenshots:contact-sheet`.
They are written under `screenshots/review/` and cover every state, crop,
theme, and resolution present in the PNG directory.

The current UI has different layouts at the two window widths, so the tool
renders both sizes natively. The `--derive-1080` option is reserved for a
future state whose layout is identical at both sizes; such a state must opt in
through `same_layout_at_both_sizes` in `src/catalog.rs`. The tool then derives
its 1080p full-window image from its 1440p capture, without rendering that
full window at 1080p. Crops remain native. Both sizes must be requested when
using `--derive-1080`.

Add new screens by adding a named `State` and regions in `src/catalog.rs`, then
populate the existing Slint model properties in `src/fixtures.rs`. The state
catalog is the complete set of screenshots this documentation tool promises to
regenerate. A state that documents an open menu or dropdown sets an
`interaction` in its catalog entry: the tool right-clicks or clicks the element
with that accessible label in the shown window before it captures. Do not add unfinished product screens to satisfy a screenshot.

## Platform requirements

- Linux with Rust 1.85 or newer and the native build dependencies required by
  `ephervex-gui` (including fontconfig, FreeType, and OpenSSL development
  libraries).
- Bash and a fontconfig setup with the fonts used by the launcher.
- No Wayland or X11 display is required.
- For the website, Node.js 20 or newer and npm. Sharp handles WebP conversion
  and contact sheets.

PNG pixels are stable for the same Rust/Slint dependency lockfile, installed
fonts and fontconfig configuration, CPU rendering environment, locale, and
fixtures. Review regenerated images when fonts, Slint, its rasterizer, or the
UI change. For byte-identical results, use the same host or a build image
with the same fonts and fontconfig configuration.
