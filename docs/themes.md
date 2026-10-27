# Themes

Ephervex ships two built-in themes, Light and Dark, which are the Workbench look
on a light and on a dark page, and seasonal themes that show themselves on a few
days of the year. You can add your own as TOML files or theme folders, and they
appear in Settings > General > Interface theme next to the built-in ones. A theme
changes colors, and a theme folder can also replace the logo and the window icon. Text size is its own setting, and fonts and layout
cannot be changed by a theme.

## Where theme files go

```text
~/.config/ephervex/themes/<id>.toml
```

Or `$XDG_CONFIG_HOME/ephervex/themes/` when `XDG_CONFIG_HOME` is set. The folder
does not exist until you create it. When a theme has a problem, Show problems
in the theme setting names the exact path.

A theme that brings its own images is a folder instead of a file:

```text
~/.config/ephervex/themes/
└── example-theme/
    ├── theme.toml
    └── branding/
        ├── logo.png
        └── icon.png
```

`theme.toml` has the same format as a standalone file. A folder without a
`theme.toml` is not a theme and is ignored.

The file name, or the folder name for a theme folder, is the theme's identity.
Use lowercase letters, digits, `-` and `_` (for example `modrinth.toml` or
`my-dark_2`); other names are skipped. Renaming a file or folder makes it a
different theme, so a choice recorded for the old name falls back to the
default until you choose it again. If a file and a usable folder have the same
name, the folder is used and the file is reported. A folder that cannot be used
does not hide the file.

Themes are read when Ephervex starts and whenever the settings are refreshed,
for example with the Refresh button above the host list.

## Format

```toml
format_version = 1
name = "My Theme"
parent = "workbench"

[branding]
logo = "logo.png"
icon = "icon.png"

[colors]
accent = "#0d7143"
background = "#e6f4ec"
```

| Key | Required | Meaning |
|---|---|---|
| `format_version` | No, defaults to `1` | The theme format. A file with a newer version than this Ephervex reads is skipped. |
| `name` | No, defaults to the file name | What the theme list shows. At most 64 characters are kept. |
| `parent` | No, defaults to `"workbench"` | The built-in theme to start from. See [Parents](#parents). |
| `[colors]` | No | The colors to change, by token name. |
| `[branding]` | No | Images that replace the logo and window icon. Theme folders only. |

Every color you leave out is the parent's, including any token added after
your file was written, so a theme never ends up with a missing color. There is
one exception: a theme that sets `accent`, `background` or `text` and leaves out
`toggle-knob`, `field` or `on-primary` respectively gets the color it set, as it
did before those could be set on their own. The standard scroll bars and lists
are drawn light or dark by the parent; see
[What a theme does not reach](#what-a-theme-does-not-reach).

### Parents

| `parent` | Starts from |
|---|---|
| `"workbench"` | The Light theme: a light page with ink outlines and pastel fills |
| `"light"` | The same as `"workbench"`. The name stays valid so existing themes and recorded choices keep loading. |
| `"dark"` | The Dark theme: the same look on a dark page, with light ink outlines and a black hard shadow |

`"workbench"`, `"light"` and `"dark"` are accepted wherever a theme is named. A
theme that sets all of its colors paints the same under any parent, except that
the standard scroll bars and lists follow the parent: dark under `"dark"`, light
under the others.

Colors are written as `"#rrggbb"`, or `"#rrggbbaa"` with an alpha channel, in
hexadecimal. Nothing looser is accepted.

`docs/themes/modrinth.toml` is a complete example that sets every color. Copy it
into the themes folder to try it, or use it as a list of what can be set. It
also holds a commented-out `[branding]` section for use in a theme folder.

## Colors

| Token | Used for |
|---|---|
| `background` | The window floor behind every page |
| `surface` | Cards and panels |
| `field` | Text inputs and dropdowns |
| `elevated` | A raised card, such as one under the pointer |
| `overlay` | Popovers, such as dropdown lists and menus, which must read against an `elevated` card |
| `border` | Borders and dividers |
| `bar` | The track of a switch that is off |
| `header-ribbon-1` | Kept so existing theme files stay valid; the window has no header ribbon, so it paints nothing |
| `header-ribbon-2` | Kept so existing theme files stay valid; the window has no header ribbon, so it paints nothing |
| `header-ribbon-3` | Kept so existing theme files stay valid; the window has no header ribbon, so it paints nothing |
| `header-ribbon-4` | Kept so existing theme files stay valid; the window has no header ribbon, so it paints nothing |
| `header-ribbon-5` | Kept so existing theme files stay valid; the window has no header ribbon, so it paints nothing |
| `header-ribbon-6` | Kept so existing theme files stay valid; the window has no header ribbon, so it paints nothing |
| `well` | The floor of technical views: consoles, logs, crash reports. A disabled field and the cache meter's track are drawn on it too. |
| `selection` | The highlighted entry of a menu, a dropdown list, or the tab bar's More list, and a hovered row of the license list |
| `selection-card` | A selected card |
| `selection-strong` | A selected filter chip. `text` is drawn on it. |
| `toggle-on` | The track of a switch that is on |
| `toggle-knob` | The knob of a switch, on or off. It sits inside a `border-strong` outline, so keep the knob apart from `toggle-on` and `bar` by its own color or by that outline. |
| `capture-active` | A key binding field waiting for a key |
| `accent` | Links, focus, and the main accent |
| `accent-secondary` | A second accent for secondary highlights |
| `success` | Success states and messages |
| `warning` | Warnings, and the filled segments of a cache over its budget |
| `danger` | Errors and destructive actions |
| `primary-fill` | A primary button, and the tile behind the initial on the account chip. Its caption is `on-primary`, so keep them apart. |
| `primary-fill-hover` | A primary button under the pointer. Workbench keeps it equal to `primary-fill`, because the button rises instead. |
| `on-primary` | The caption of a primary button, and the initial on the account chip |
| `danger-fill` | A destructive button. Its caption is `on-danger`, so keep them apart. |
| `danger-fill-hover` | A destructive button under the pointer. Workbench keeps it equal to `danger-fill`. |
| `text` | Primary text |
| `text-secondary` | Descriptions and secondary text |
| `text-muted` | Hints and disabled text |
| `on-danger` | The caption of a destructive button |
| `log-error` | Error lines in a console or log |
| `server` | Marks that identify a server |
| `modrinth` | The Modrinth catalog's mark |
| `curseforge` | The CurseForge catalog's mark |
| `vanilla` | The vanilla game's mark |
| `forge` | The Forge loader's mark |
| `neoforge` | The NeoForge loader's mark |
| `group-tone-1` | The first color instance groups are told apart by |
| `group-tone-2` | The second color instance groups are told apart by |
| `group-tone-3` | The third color instance groups are told apart by |
| `group-tone-4` | The fourth color instance groups are told apart by |
| `group-tone-5` | The fifth color instance groups are told apart by |
| `group-tone-6` | The sixth color instance groups are told apart by |
| `shadow` | A popover's shadow; usually a translucent black |
| `scrim` | The wash behind a dialog; usually a translucent black |
| `shadow-hard` | The hard-edged offset shadow under a card, button, dialog, or panel |
| `border-strong` | The outline around a card, button, chip, or panel |
| `accent-primary` | The fill of the selected navigation, settings and workspace tab items, of an icon button under the pointer or standing for something that is on, and of a slider, and the ring around the selected card. The main button reads `primary-fill`. |
| `on-accent` | A caption on an accent or state fill, such as `accent-primary` or `state-running` |
| `state-running` | The fill of the tag of a host that is running, of a success button and toast, and of the strip down a running client's card and a finished activity's card on Home, and of the filled segments of the cache meter |
| `state-building` | The fill of the tag of a host that is starting, stopping, or restarting, of a lavender button, and of the strip down an activity card in progress |
| `state-attention` | The fill of the tag of a host that failed or needs recovery, of the mark on a problem on Home, and of the strip down a failed activity's card and a card of a host that needs a fix |
| `state-stopped` | The fill of an info button, and of the strip down the card of a host that is stopped |
| `state-server` | The fill of a server tag, and of the strip down a server's card |
| `note-fill` | The fill of the workflow banner and of a favorite star that is on |
| `danger-soft` | The fill of a problem box. `danger` text is drawn on it. |
| `ok-soft` | The fill of something that is fine or in use, such as a healthy diagnosis or the Java runtime in use. Text and the tones are drawn on it. |
| `grid-line` | The hairline grid over the window floor; usually a faint translucent line |
| `terminal` | The floor of a console or log that is drawn dark on any page |
| `on-terminal` | Text on `terminal` |
| `on-terminal-muted` | Quieter text on `terminal`, such as times and sources |
| `terminal-error` | An error line on `terminal` |
| `chip-forge` | The fill of a host card's Forge loader chip. `text` is drawn on it. |
| `chip-neoforge` | The fill of a host card's NeoForge loader chip. `text` is drawn on it. |
| `chip-vanilla` | The fill of a host card's Vanilla loader chip. `text` is drawn on it. |

Keep text readable: `text`, `text-secondary` and `text-muted` are drawn on
`background`, `surface`, `field` and `elevated`, and captions on the button fills.

The built-in Workbench theme is held to the same bar, and so is the example
theme. Every caption is readable on the fill it is drawn on: `text` on
`surface`, `selection`, `selection-strong`, `note-fill` and each `chip-` fill, `on-accent` on `accent-primary` and each `state-` fill,
`on-primary` on `primary-fill` and `primary-fill-hover`, and `on-danger` on `danger-fill` and
`danger-fill-hover`, and `surface` on `danger` and on `border-strong` (the counts on a card's problem badge and on a list section's heading). A switch's knob is told apart from its track by its own
color or by the `border-strong` outline. The scrim dims the page firmly.

The tones drawn as text (`accent`, `accent-secondary`, `success`, `warning`, `danger`, the marks, and the group
tones) are held at 4.5:1 or more on every surface, and on `surface` and `selection`, which is what a tag is drawn on,
so they are darker than the pastel fills of the same hue. `on-accent` is dark on every accent and state fill.
A disabled button keeps its fill and its caption at reduced opacity and does not
rise, so it reads as inactive.

## Branding

A theme folder can name two images under `[branding]`:

| Key | Replaces |
|---|---|
| `logo` | The logo above the host list and on the About page |
| `icon` | The window icon, where the desktop takes it from the window |

Both are optional, and each one falls back to the Ephervex logo on its own:
a theme that sets only `logo` keeps the default window icon. A built-in theme's
branding is compiled into the application and never read from disk. Light and
Dark use the Ephervex logo and window icon, and each seasonal theme uses its own
mascot as both logo and window icon.

Each value is a file name inside the theme's own `branding/` folder, such as
`"logo.png"` or `"icons/app.webp"`. Do not write `branding/` in front of it.

- Supported images are PNG (`.png`), JPEG (`.jpg`, `.jpeg`) and WebP
  (`.webp`). The file's contents must be the format its extension names.
- An image may be at most 4 MiB and at most 1024 by 1024 pixels, and decoding
  it may not need more memory than an image of that size.
- Images keep their aspect ratio. A logo is fitted inside its space, and an
  icon that is not square is centered on a transparent square.
- A value that is empty, absolute (`/...`), contains `..` or `.` as a
  component, a backslash or a colon (so no drive letters, URLs or other URI
  schemes), a doubled `/`, a control character, more than 255 bytes, or more
  than three nested folders is ignored and reported.
- Symbolic links are never followed below the themes folder. The theme
  folder, its `branding/` folder, any folder in between, and the image itself
  must all be real folders and files. Each step is opened inside the one
  before it, so a link swapped in while Ephervex reads the theme is refused
  rather than followed. The themes folder itself may be a link.
- An image is only ever read from its own theme's `branding/` folder, never
  from another theme, from elsewhere on disk, or from the network.

An image that is missing, unreadable, malformed, too large, or refused for any
of the reasons above is listed under Show problems with its path, and the
default is shown in its place.

Only the chosen theme's images are read, when the settings are read or the
theme is changed. Changing the theme changes the logo straight away.

### Window and taskbar icons

The window icon changes with the theme while Ephervex runs where the desktop
takes the icon from the window:

- On X11 the window's icon is replaced, and taskbars that show the window's
  own icon follow it. Docks and taskbars that match the window to Ephervex's
  desktop entry instead, as GNOME Shell does, keep showing the installed
  `ephervex` icon.
- On Wayland the compositor shows the icon from Ephervex's desktop entry, and
  an application cannot change it while it runs, so the window icon stays the
  installed one. The logo inside the window still follows the theme.
- Ephervex is built for Linux only, so there is no macOS Dock or Windows
  taskbar icon to change.

## Seasonal themes

A seasonal theme stands in for Light or Dark while its season lasts. It is not
in the theme list and cannot be chosen; there is one palette per season,
whichever of Light and Dark you chose.

| Theme | Season |
|---|---|
| Halloween | October 25 through October 31 |
| Christmas | December 24 through December 26 |
| Pride Month | June 1 through June 30 |
| April Fools | April 1 |

The season follows the computer's local date. It starts at midnight on its
first day and ends at midnight after its last day, also while Ephervex stays
open: the date is checked once a minute, and the theme changes once at each
boundary.

A theme of your own is never replaced. To keep Light or Dark during a season,
turn off Settings > General > Seasonal themes; the theme you chose is shown
straight away. The setting is only offered while a season is on, because it
changes nothing on any other day.

A season never changes your theme setting. The theme list keeps showing your
choice, the Seasonal themes setting says which theme the season is showing,
and your theme comes back when the season ends, also after a restart.

A seasonal theme replaces the colors and the logo and window icon together, and
is the Workbench look in the season's colors. Halloween is a charcoal page with
bone outlines, pumpkin orange fills, and ghost green for running, with purple
only as an accent. Christmas is a snowy page with pine-green outlines, candy-red
shadows, and green highlights. Pride Month is a warm white page where each fill
is another color of the rainbow. April Fools is a sunny yellow page with navy
outlines, magenta shadows, and clashing hot pink, turquoise, lime, and violet fills.
The icon Ephervex was installed with does not change, as described under
[Window and taskbar icons](#window-and-taskbar-icons). A seasonal theme is not
a `parent` a theme file can name.

### Previewing a seasonal theme

To see a seasonal theme outside its season, start Ephervex with the
`SEASONS_THEME` environment variable set to the theme's name:

```sh
SEASONS_THEME=halloween ephervex-gui
```

Use `SEASONS_THEME=christmas`, `SEASONS_THEME=pride`, or
`SEASONS_THEME=april-fools` to preview another season. The theme is shown on
every day for as long as the variable is set, also across restarts and the
once-a-minute date check. The value is matched without regard to case. An unset
or empty variable leaves the date in charge.
A value that names no seasonal theme is ignored and the date decides; the log
says which values are accepted.

The variable replaces only the date. A theme of your own is still never
replaced, turning off Settings > General > Seasonal themes still shows the theme
you chose, and your theme setting is never changed.

### Adding a seasonal theme

A seasonal theme has four parts:

1. A `SeasonalTheme` value (`crates/core/src/seasonal_theme.rs`) and its first
   and last day in `THEME_SEASONS` there. Seasons must not overlap, and one may
   run over the turn of the year, such as December 24 to January 6.
2. Its palette in `BuiltInPalettes` (`crates/gui/ui/palettes.slint`), which
   sets every color token, read by `seasonal_palette` in
   `crates/gui/src/palette.rs`.
3. Its logo and icon, if it has its own, in `SEASONAL_BRANDING`
   (`crates/gui/src/theme_files.rs`), compiled in from `assets/themes/`.
4. Its name in the localization files, for the note the settings page shows
   while it is active.

`SEASONS_THEME` accepts the new theme's `SeasonalTheme::as_str` name with no
further work, because it looks the name up in `THEME_SEASONS`. Nothing else
checks dates. See ADR 0049 for the rules a season follows.

## What a theme does not reach

Dropdowns, sliders, spinners and menus, including the one a right-click or a
"Launch as" button opens, are drawn from the colors above. Scroll bars and lists
are the toolkit's own controls: they take no colors from a theme and are drawn
light or dark by the theme's parent, and a theme cannot set that. A theme with a
dark background therefore needs `parent = "dark"` to get dark scroll bars and
lists.

## When a file has a problem

The theme setting says how many problems were found, and Show problems lists
them, one line per file:

- A file is skipped when it is not valid TOML, uses a newer `format_version`,
  names a `parent` other than `"workbench"`, `"dark"` or `"light"`, gives a key the wrong type, or
  writes a color any other way than `#rrggbb` or `#rrggbbaa`.
- A color token or key Ephervex does not know is ignored and reported, and the
  rest of the theme is used. Check the spelling when you see one.
- A theme folder that is a symbolic link, or whose `theme.toml` is one, is
  skipped.
- A `[branding]` value of the wrong type skips the file. A branding value that
  names an unsafe or unsupported file, or a branding key Ephervex does not know,
  is ignored and reported, and the rest of the theme is used. `[branding]` in a
  standalone `.toml` file is ignored and reported, because only a theme folder
  has a `branding/` folder.
- At most 100 themes, files and folders together, are read, and a theme file
  may be at most 64 KiB.

If the theme you chose can no longer be read, Ephervex paints in the Light
theme and keeps your choice, so fixing the file brings the theme back.
