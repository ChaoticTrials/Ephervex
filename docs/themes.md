# Themes

Ephervex ships a Dark and a Light theme. You can add your own as TOML files,
and they appear in Settings > General > Interface theme next to the built-in
ones. A theme changes colors only. Text size is its own setting, and fonts and
layout cannot be changed by a theme.

## Where theme files go

```text
~/.config/ephervex/themes/<id>.toml
```

Or `$XDG_CONFIG_HOME/ephervex/themes/` when `XDG_CONFIG_HOME` is set. The folder
does not exist until you create it. The settings page shows the exact path.

The file name is the theme's identity. Use lowercase letters, digits, `-` and
`_` (for example `modrinth.toml` or `my-dark_2.toml`); other file names are
skipped. Renaming a file makes it a different theme, so a choice recorded for
the old name falls back to the default until you choose it again.

Themes are read when Ephervex starts and whenever the settings are refreshed,
for example with the Refresh button in the top bar.

## Format

```toml
format_version = 1
name = "My Theme"
parent = "dark"

[colors]
accent = "#1bd96a"
background = "#16181c"
```

| Key | Required | Meaning |
|---|---|---|
| `format_version` | No, defaults to `1` | The theme format. A file with a newer version than this Ephervex reads is skipped. |
| `name` | No, defaults to the file name | What the theme list shows. At most 64 characters are kept. |
| `parent` | No, defaults to `"dark"` | The built-in theme to start from: `"dark"` or `"light"`. |
| `[colors]` | No | The colors to change, by token name. |

Every color you leave out is the parent's. The parent also decides whether the
standard controls (dropdowns, scroll bars, spinners) are drawn dark or light,
so pick the parent closest to your theme.

Colors are written as `"#rrggbb"`, or `"#rrggbbaa"` with an alpha channel, in
hexadecimal. Nothing looser is accepted.

`docs/themes/modrinth.toml` is a complete example that sets every color. Copy it
into the themes folder to try it, or use it as a list of what can be set.

## Colors

| Token | Used for |
|---|---|
| `background` | The window floor behind every page |
| `surface` | Cards, panels, and fields |
| `elevated` | A raised card, such as one under the pointer |
| `overlay` | Popovers, which must read against an `elevated` card |
| `border` | Borders and dividers |
| `bar` | The top bar, the navigation column, and the status bar |
| `well` | The floor of technical views: consoles, logs, crash reports |
| `selection` | A selected row |
| `selection-card` | A selected card |
| `selection-strong` | A selected chip |
| `toggle-on` | The track of a switch that is on |
| `capture-active` | A key binding field waiting for a key |
| `accent` | Links, focus, and the main accent |
| `accent-secondary` | A second accent for secondary highlights |
| `success` | Success states and messages |
| `warning` | Warnings |
| `danger` | Errors and destructive actions |
| `primary-fill` | A primary button. Its caption is `text`, so keep them apart. |
| `primary-fill-hover` | A primary button under the pointer |
| `danger-fill` | A destructive button. Its caption is `on-danger`. |
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
| `shadow` | A popover's shadow; usually a translucent black |
| `scrim` | The wash behind a dialog; usually a translucent black |

Keep text readable: `text`, `text-secondary` and `text-muted` are drawn on
`background`, `surface` and `elevated`, and captions on the button fills.

## When a file has a problem

Problems are listed under the theme setting, one line per file:

- A file is skipped when it is not valid TOML, uses a newer `format_version`,
  names a `parent` that is not a built-in theme, gives a key the wrong type, or
  writes a color any other way than `#rrggbb` or `#rrggbbaa`.
- A color token or key Ephervex does not know is ignored and reported, and the
  rest of the theme is used. Check the spelling when you see one.
- At most 100 theme files of at most 64 KiB each are read.

If the theme you chose can no longer be read, Ephervex paints in the Dark
theme and keeps your choice, so fixing the file brings the theme back.
