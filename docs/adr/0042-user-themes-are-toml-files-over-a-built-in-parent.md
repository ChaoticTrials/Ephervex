# ADR 0042 - User themes are TOML files over a built-in parent

**Status:** Accepted (its one-file and only-colors decisions amended by
[ADR 0047](0047-a-theme-folder-carries-branding-confined-to-its-own-branding-folder.md))

## Context

ADR 0041 makes a theme a complete color palette, resolved before the markup
reads it, and leaves the shape of a user-written palette to a decision of its
own. Users want to add themes without rebuilding Ephervex, and a theme file is
something people copy from each other, so the format is a public contract from
the first release that reads it.

## Decision

**One theme per file, in the themes folder.** Theme files are
`$XDG_CONFIG_HOME/ephervex/themes/<id>.toml`. The file stem is the theme's
identity and is limited to lowercase ASCII letters, digits, `-` and `_`. The
preference records a user theme as `user:<id>`, so a file can never shadow a
built-in theme or one a later build adds.

**A theme is a built-in parent and the colors it changes.** A file names its
`parent`, which must be a built-in theme, and sets any subset of the `Theme`
color tokens under `[colors]` by their token names. Every token it leaves out
is the parent's, and so is the scheme the shipped widgets paint in. A parent
cannot be another user theme: a chain would make one file depend on others
that can be renamed, deleted, or form a cycle. `name` is what the theme list
shows, and the file stem stands in for it when it is absent.

**The format is versioned in the file.** `format_version` defaults to 1. A file
that declares a newer version is skipped rather than half understood.

**A file is used whole or not at all.** A wrong type, an unknown parent, or a
color that is not `#rrggbb` or `#rrggbbaa` skips the file: a theme with some of
its colors silently missing can be unreadable. A color token or top-level key
this build does not know is ignored instead, so a theme written for a build
with more tokens still loads. Both are reported under the theme setting, per
file, and one bad file never affects another.

**Only colors.** A theme cannot set text sizes, fonts, or measurements
(ADR 0041).

**Read with the settings, not watched.** The folder is read whenever the
settings are refreshed, which includes starting up and pressing Refresh. At most
100 files of at most 64 KiB each are read.

**A missing theme costs its colors, not the choice.** If the chosen theme's
file is gone, the interface paints in the default built-in theme and the
recorded choice is kept, so restoring the file restores the theme.

## Consequences

- Token names are part of the theme format. A token can be added freely; one
  that is renamed or removed breaks every theme that set it.
- A new built-in theme is a valid parent as soon as it exists.
- Nothing in a theme file is executed or followed: it names colors, and the
  only path involved is the folder Ephervex reads.
- The format's documentation and a complete example live in `docs/themes.md`
  and `docs/themes/`, and a test keeps the example valid and complete.
