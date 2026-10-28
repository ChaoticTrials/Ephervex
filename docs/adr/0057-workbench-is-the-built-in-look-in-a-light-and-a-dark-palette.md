# ADR 0057 - Workbench is the built-in look, in a light and a dark palette

**Status:** Accepted (amends ADR 0041, ADR 0042 and ADR 0049)

## Context

ADR 0041 makes a theme a complete color palette and lets built-in palettes be
added, and ADR 0042 lets a user theme start from a built-in `parent` named
`dark` or `light`. Those names are recorded in configuration files and in theme
files people have shared, so they cannot stop loading.

Ephervex's look is Workbench: a light page with ink outlines, hard shadows, and
pastel fills that carry meaning. It is designed as one look, and any other
variant has to be designed against it rather than carried over from another
palette.

## Decision

**Workbench is the built-in look, in two palettes.** Workbench is the light one,
the default for a new configuration, and is recorded as `workbench`. Workbench
Dark is the same look on a dark page, recorded as `dark`. The theme list offers
the two and then the user's themes.

**`light` stays an accepted theme name and resolves to Workbench.** It is valid
as a recorded `color_scheme` and as a theme file's `parent`, and a recorded
`light` is kept exactly as read when other settings are saved. `dark` resolves
to Workbench Dark in the same places.

**A palette is Workbench's shape.** Every palette sets every color token,
including tokens for the strong outline, the hard shadow, host states, notes,
problem boxes, and consoles drawn dark on a light page. The look's measurements
(outline width, corner radii, shadow offsets, grid size, button heights) are
`Theme` values, not palette values, as ADR 0041 requires.

**A seasonal theme is designed in Workbench's shape too.** It stands in for the
built-in theme the user chose, as ADR 0049 describes, with one palette per
season. Workbench draws no header ribbon, so a season's palettes leave the
ribbon tokens transparent and carry the season in their other colors and in the
season's logo.

**A user theme inherits what it leaves out.** A token a theme file does not set,
including one added after the file was written, is the parent's, so a theme is
never missing a color.

## Consequences

- Token names stay part of the theme file format. This decision adds tokens and
  renames or removes none.
- A theme written with `parent = "dark"` paints over Workbench Dark for every
  token it leaves out, and one with `parent = "light"` over Workbench.
- The scheme the shipped widgets paint in comes from the parent, and a theme
  file cannot set it. A theme with a dark background therefore only gets dark
  scroll bars and lists under `parent = "dark"`.
- Text drawn in an accent, state, or mark tone uses a darker variant of the
  pastel fill of the same hue on a light page, and a lighter one on a dark
  page, so it reads on every surface.
