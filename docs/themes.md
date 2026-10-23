# Themes

Ephervex ships one built-in theme, Workbench. You can add your own as TOML files
or theme folders, and they appear in Settings > General > Interface theme next
to it. A theme changes colors, and a theme folder can also replace
the logo and the window icon. Text size is its own setting, and fonts and layout
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
for example with the Refresh button in the top bar.

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
are drawn light under every parent; see
[What a theme does not reach](#what-a-theme-does-not-reach).

### Parents

| `parent` | Starts from |
|---|---|
| `"workbench"` | The Workbench theme: a light page with ink outlines and pastel fills |
| `"dark"` | Workbench. The name stays valid so existing themes and recorded choices keep loading. |
| `"light"` | Workbench. The name stays valid so existing themes and recorded choices keep loading. |

`"dark"` and `"light"` are accepted wherever a theme is named, and they paint as
Workbench. A theme that sets all of its colors paints the same under any
parent. No parent produces a dark page or dark scroll bars and lists: every
parent is Workbench's light look.

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
| `bar` | The top bar, the navigation column, and the status bar |
| `header-ribbon-1` | The first sixth of the ribbon along the bottom of the top bar; transparent hides it |
| `header-ribbon-2` | The second sixth of the top bar ribbon |
| `header-ribbon-3` | The third sixth of the top bar ribbon |
| `header-ribbon-4` | The fourth sixth of the top bar ribbon |
| `header-ribbon-5` | The fifth sixth of the top bar ribbon |
| `header-ribbon-6` | The final sixth of the top bar ribbon |
| `well` | The floor of technical views: consoles, logs, crash reports |
| `selection` | A selected row, and the highlighted entry of a menu |
| `selection-card` | A selected card |
| `selection-strong` | A selected chip |
| `toggle-on` | The track of a switch that is on |
| `toggle-knob` | The knob of a switch that is on. Keep it apart from `toggle-on`. |
| `capture-active` | A key binding field waiting for a key |
| `accent` | Links, focus, and the main accent |
| `accent-secondary` | A second accent for secondary highlights |
| `success` | Success states and messages |
| `warning` | Warnings |
| `danger` | Errors and destructive actions |
| `primary-fill` | A primary button. Its caption is `on-primary`, so keep them apart. |
| `primary-fill-hover` | A primary button under the pointer |
| `on-primary` | The caption of a primary button |
| `danger-fill` | A destructive button. Its caption is `on-danger`, so keep them apart. |
| `danger-fill-hover` | A destructive button under the pointer |
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
| `accent-primary` | The fill of the main action and the outline of the selected card |
| `on-accent` | A caption on an accent or state fill, such as `accent-primary` or `state-running` |
| `state-running` | The strip and stamp of a host that is running |
| `state-building` | The strip and stamp of a host that is being built |
| `state-attention` | The strip and stamp of a host that needs a fix |
| `state-stopped` | The strip and stamp of a host that is stopped |
| `state-server` | The mark of a server |
| `note-fill` | The paper of a sticky note and of a warning box |
| `danger-soft` | The fill of a problem box. `danger` text is drawn on it. |
| `grid-line` | The hairline grid over the window floor; usually a faint translucent line |
| `terminal` | The floor of a console or log that is drawn dark on any page |
| `on-terminal` | Text on `terminal` |
| `on-terminal-muted` | Quieter text on `terminal`, such as times and sources |
| `terminal-error` | An error line on `terminal` |

Keep text readable: `text`, `text-secondary` and `text-muted` are drawn on
`background`, `surface`, `field` and `elevated`, and captions on the button fills.

The built-in Workbench theme is held to the same bar. A switch that is on keeps
its knob apart from its track, button captions keep a readable ratio on their
fills, and the scrim dims the page firmly. The tones drawn as text (`accent`,
`accent-secondary`, `success`, `warning`, `danger`, the marks, and the group
tones) are held at 4.5:1 or more on every surface, so they are darker than the
pastel fills of the same hue. `on-accent` is dark on every accent and state fill.
A disabled button has no fill, only a border and a muted caption, so it reads as
inactive rather than as a secondary button.

## Branding

A theme folder can name two images under `[branding]`:

| Key | Replaces |
|---|---|
| `logo` | The logo in the top bar and on the About page |
| `icon` | The window icon, where the desktop takes it from the window |

Both are optional, and each one falls back to the Ephervex logo on its own:
a theme that sets only `logo` keeps the default window icon. Workbench uses the
Ephervex logo and window icon.

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

## What a theme does not reach

Dropdowns, sliders, spinners and menus, including the one a right-click or a
"Launch as" button opens, are drawn from the colors above. Scroll bars and lists
are the toolkit's own controls: they take no colors from a theme and are drawn
light for every theme, whatever its parent. A theme with a dark background
leaves them looking out of place.

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

If the theme you chose can no longer be read, Ephervex paints in the Workbench
theme and keeps your choice, so fixing the file brings the theme back.
