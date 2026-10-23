# ADR 0056 - Seasonal themes are not part of the product

**Status:** Accepted

Supersedes [ADR 0049](0049-a-seasonal-theme-stands-in-for-a-built-in-theme-during-its-season.md).

## Context

A seasonal theme replaced Dark or Light for a span of calendar days, with its
own palette and branding, a setting to turn it off, and a polling check of the
local date. Its palettes and artwork were designed against the Dark and Light
palettes, so it cannot be carried over to a different built-in look. A season
has to be designed against the look it stands in for.

## Decision

**There are no seasonal themes.** The recorded theme is the theme shown. No
date, setting, or environment variable replaces it, and the interface does not
watch the clock for it.

**The `seasonal_themes` key is not a setting.** A configuration file that still
carries it loads, and the key is kept like any other unknown key under
`[display]`, so it does not stop an older file from starting.

**A later season is a new decision.** It gets its own ADR against the palettes
of the time.

## Consequences

- Users see the theme they chose on every day of the year.
- The settings page has no seasonal toggle.
- Branding comes from a user theme folder (ADR 0047) or is the default logo and
  window icon.
