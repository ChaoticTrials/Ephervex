# ADR 0041 - A theme is a complete color palette

**Status:** Accepted

## Context

The desktop interface paints every color from a named `Theme` token, and every
page and component reads those tokens rather than writing a color. Which value a
token has depends on the appearance the user chose. A second dark variant, a
high-contrast palette, or a palette a user writes themselves are all plausible
additions, and each one has to be possible without finding every place a
component asked "which appearance is this?".

`Theme` also holds typography and measurements. Several of those are load
bearing: a ListView only virtualizes when every row is the height the list
expects, the card list scrolls a card into view by multiplying its height, and
the split workspace sizes panes from fixed minimums. A value like that is an
invariant of the layout, not a matter of taste.

## Decision

**The semantic tokens are the contract.** Components read `Theme.background`,
`Theme.accent`, `Theme.danger` and the rest, and never learn which palette is
active. No component branches on the appearance.

**A palette is complete.** A `ThemePalette` sets every color token and the
scheme the shipped widgets paint in to match. Built-in palettes are defined
together in one module and are the only place a color is written. A palette
that leaves a token out is a defect, not a fallback to another palette.

**The preference is resolved to a palette in one place.** The user's appearance
preference is stored as a preference, and the frontend resolves it to one
complete palette before `Theme` reads it. A preference that is not itself a
palette, such as following the desktop, is resolved there too, so nothing in
the markup can tell the difference.

**Only colors are themeable.** Text sizes, the monospace family, control and
card dimensions, fixed row heights, the navigation column, and workspace pane
and tab geometry stay in `Theme` beside the palette and are never a palette's
to change. The user's text size preference remains the only thing that scales
text.

## Consequences

- Adding a built-in palette is a palette definition, its place in the
  resolution, and its entry in the preference. Page and component markup does
  not change.
- A palette a user writes fits the same shape: a built-in base with some
  tokens overridden, validated and resolved to a complete palette in Rust
  before `Theme` sees it. Its file format, and how that is versioned, is a
  decision of its own.
- Because only colors can vary, a palette cannot break scrolling, virtualized
  lists, or pane layout, and a bug report's screenshot differs from another
  user's in color only.
- Slint fills a struct field that a literal leaves out with its default, which
  for a color is transparent. Completeness is therefore checked by a test
  rather than by the compiler.
