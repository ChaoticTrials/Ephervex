# ADR 0057 - Workbench is the single built-in palette, and dark and light resolve to it

**Status:** Accepted (amends ADR 0041 and ADR 0042)

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

**Workbench is the only built-in palette.** It is the default for a new
configuration and is recorded as `workbench`. The theme list offers Workbench
and the user's themes.

**`dark` and `light` remain accepted theme names and resolve to Workbench.** They
are valid as a recorded `color_scheme` and as a theme file's `parent`. A recorded
`dark` or `light` is kept exactly as read when other settings are saved, shows
Workbench as the chosen entry, and changes only when the user picks another
theme. Giving either name a palette of its own is a separate decision that changes
only how the name resolves.

**A palette is Workbench's shape.** Workbench sets every color token, including
tokens for the strong outline, the hard shadow, host states, notes, problem
boxes, and consoles drawn dark on a light page. The look's measurements (outline
width, corner radii, shadow offsets, grid size, button heights) are `Theme`
values, not palette values, as ADR 0041 requires.

**A user theme inherits what it leaves out.** A token a theme file does not set,
including one added after the file was written, is the parent's, so a theme is
never missing a color.

## Consequences

- Token names stay part of the theme file format. This decision adds tokens and
  renames or removes none.
- Every parent name resolves to Workbench's colors, so a theme written with
  `parent = "dark"` paints over Workbench for every token it leaves out.
- The scheme the shipped widgets paint in comes from the parent, and a theme
  file cannot set it. Every user theme therefore draws scroll bars and lists
  light, including a theme with a dark background.
- Text drawn in an accent, state, or mark tone uses a darker variant of the
  pastel fill of the same hue, so it reads on every surface.
