# Ephervex documentation screenshots

Run this command from the repository root to regenerate every documented UI state:

```bash
./tools/auto-screenshot-tool/generate.sh
```

The script changes to the repository root and runs the Rust generator. It
captures the compiled `AppWindow` with Slint's headless Skia renderer at a
fixed scale of 1, with the `en-US` locale by default and mocked time. It uses the
same system font resolution as the launcher, including its monospace family.
No display server or GPU is needed. Skia is the `skia` feature of the tool,
which `generate.sh` turns on. It is used instead of Slint's software renderer
because that one ignores item transforms and rounded clips, so tilted or
scaled items and rounded panel corners would not match the application. The
feature is off by default so Skia stays out of the dependency graph the license
inventory and `cargo deny` read; a build without it refuses to generate. The
Rust tool never attaches the application controller, opens launcher storage,
or reads accounts or credentials. The rows it shows (instances, servers, the
CaveStone mod list, search results, snapshots, logs, crash reports, and
diagnosis results) come from the files in `data/`. Most were recorded from real
packs, mods, and servers run in an isolated launcher home;
`data/server_mods.json` was assembled from Modrinth records. `SOURCES.md` lists
what each file holds and how it was produced. Progress bars, toasts, activity
entries, which host is shown as running, and the other presentation-only state
are set in `src/fixtures.rs` and the modules under `src/fixtures/`.

The only network access is fetching project logos from `cdn.modrinth.com`
into `artwork/`, which is gitignored. Each recorded row names its logo by
address and by the SHA-256 of the bytes the catalog served. A logo that cannot
be fetched, or whose bytes differ, is left empty with a warning that names its
address, so a project changing its logo never stops a build; update the digest
in `data/` when the new logo is right. Pass `--offline` to fetch nothing and
leave logos that are not cached empty, or `--artwork-cache DIR` to keep them
elsewhere.

The default command writes Ephervex Light, Dark, Halloween, Christmas, Pride,
and April Fools captures at 1920x1080. Pass `--size 2560x1440` for the larger
window. Each seasonal theme is shown as it stands in for a chosen Light or Dark
(Halloween for Dark, the others for Light), with its logo and the Seasonal
themes note. Full-window files are stored under
`tools/auto-screenshot-tool/screenshots/` at
`<state>/<theme>-<width>x<height>.png`; crops are stored at
`<state>/<region>/<theme>-<width>x<height>.png`. A run without `--state`,
`--theme`, or `--output` clears that directory first, whatever `--size` says, so
removed states and old paths do not remain.
Some states also include a crop around an accessible UI element. A crop is taken
from the rendered window, not rebuilt separately. All themes use the
same fixture object.
Native popup menus are cropped from their known window position because they
do not expose a stable element boundary to the headless renderer. Review those
crops at both window sizes when changing the related layout.
The PNG directory, including review contact sheets, is gitignored.

To capture a subset, pass options through the script:

```bash
./tools/auto-screenshot-tool/generate.sh --state instances-overview --theme light --size 1920x1080
```

Repeat `--state`, `--theme`, or `--size` to select more than one. `--output`
chooses a different output directory. `--locale` accepts a bundled locale;
currently that is `en-US`. The website
component lives in `web/src/components/`. From `web/`, synchronize selected
PNGs into optimized WebP assets and a matching manifest:

```bash
npm run screenshots:sync
```

Synchronization defaults to 1920x1080. To publish both resolutions, generate the
2560x1440 captures first and use
`npm run screenshots:sync -- --size 1920x1080 --size 2560x1440`. Unchanged
sources are not recompressed. The source PNGs remain in place. Use the
component in MDX, for example:

```tsx
<ThemeScreenshot state="instances-overview" alt="Ephervex instances" />
<ThemeScreenshot state="instances-overview" region="client-card" alt="CaveStone" />
```

The component renders the light capture in static HTML, then follows
Docusaurus's selected color mode after hydration, or shows the seasonal
capture while the site shows one of its seasons. `theme="halloween"`,
`"christmas"`, `"pride"`, or `"april-fools"` always shows that capture. It reads the manifest and
uses responsive sources only for resolutions that were synchronized.

Generate review contact sheets with `cd web && npm run screenshots:contact-sheet`.
They are written to `tools/auto-screenshot-tool/screenshots/review/` and cover
every state, crop, theme, and resolution present in the PNG directory. They need
all six themes for every capture in that directory. `npm run screenshots:sync`
has the same requirement, so a run limited with `--theme` cannot be
synchronized.

The current UI lays out differently at the two window widths, so every state is
rendered natively at each size you request. The `--derive-1080` option is reserved for a
future state whose layout is identical at both sizes; such a state must opt in
through `same_layout_at_both_sizes` in `src/catalog.rs`. The tool then derives
its 1080p full-window image from its 1440p capture, without rendering that
full window at 1080p. Crops remain native. Both sizes must be requested when
using `--derive-1080`.

Add new screens by adding a named `State` and regions in `src/catalog.rs`, then
populate the existing Slint model properties in `src/fixtures.rs` or the modules
under `src/fixtures/`. The state
catalog is the complete set of screenshots this documentation tool promises to
regenerate. A state that documents an open menu or dropdown sets an
`interaction` in its catalog entry: the tool right-clicks or clicks the element
with that accessible label, or scrolls it into view, in the shown window before
it captures. Do not add unfinished product screens to satisfy a screenshot.

## Platform requirements

- Linux with Rust 1.85 or newer and the native build dependencies required by
  `ephervex-gui` (including the fontconfig and FreeType development
  libraries).
- Bash and a fontconfig setup with the fonts used by the launcher.
- No Wayland or X11 display is required.
- Network access while building: the Skia build downloads prebuilt Skia
  binaries on first use.
- For the website, Node.js 20 or newer and npm. Sharp handles WebP conversion
  and contact sheets.

PNG pixels are stable for the same Rust/Slint dependency lockfile, installed
fonts and fontconfig configuration, CPU rendering environment, locale, and
fixtures. Review regenerated images when fonts, Slint, its rasterizer, or the
UI change. For byte-identical results, use the same host or a build image
with the same fonts and fontconfig configuration.
