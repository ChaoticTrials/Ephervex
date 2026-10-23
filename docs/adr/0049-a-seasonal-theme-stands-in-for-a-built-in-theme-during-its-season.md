# ADR 0049 - A seasonal theme stands in for a built-in theme during its season

**Status:** Superseded by [ADR 0056](0056-seasonal-themes-are-not-part-of-the-product.md)

## Context

Ephervex dresses itself up for holidays, starting with a Halloween theme. A
holiday theme is a surprise that appears on its own during its season and goes
away afterward; it is not one more entry in the theme list. The interface
theme is a recorded preference (ADR 0042), and a theme a season shows must
never be mistaken for, or written as, that preference.

## Decision

**A seasonal theme is its own kind of theme.** It is a `SeasonalTheme`, not an
`ApplicationColorScheme`: it has no recorded name, is not offered in the theme
list, and cannot be a user theme's `parent`. Each has one complete palette
(ADR 0041) and may have compiled-in branding (ADR 0047). The same palette is
shown whether the user chose Dark or Light.

**A season is a span of local calendar days.** Each season has a first and last
day of the year, both part of it. A season may run over the turn of the year,
and seasons do not overlap. The table lives in `ephervex_core::seasonal_theme`,
the only place that knows which dates belong to which theme. Frontends ask it
which theme to show for a recorded preference and a date, and never compare
dates themselves.

**A season stands in for the built-in themes only.** While a season is on and
seasonal themes are enabled, a recorded Dark or Light is shown as the season's
theme. A user theme is a deliberate choice of colors and is always shown as it
is. The `seasonal_themes` setting under `[display]`, on by default, turns
seasons off.

**The shown theme is computed, never recorded.** The settings page shows and
saves the recorded theme. The theme a season shows is worked out again each
time the settings are read, from the recorded theme, the setting and the local
date, so a season ends on its own after a restart and turning the setting off
shows the recorded theme straight away.

**A date change while running is noticed by polling.** The desktop frontend
checks the local date once a minute. It reads the settings again only when the
new date changes the theme that should be shown, and at most once per date, so
the theme changes once at each boundary and never switches back and forth.

**A season can be forced for debugging.** The `SEASONS_THEME` environment
variable names a `SeasonalTheme` by its identifier, ignoring case, and that
season is shown on every day in place of the date's. It replaces only the date
check: a user theme is still never replaced, turning seasonal themes off still
shows the recorded theme, and nothing is recorded. The name is looked up in the
season table, so a new season needs no further work. An unset, empty or unknown
value leaves the date in charge, and an unknown one is logged with the accepted
values rather than stopping startup. The variable is read once per process.

## Consequences

- Users of Dark or Light see each season unless they turn seasonal themes off.
- A seasonal theme cannot be kept outside its season.
- Adding a season is a `SeasonalTheme` value, its days, its palette, its
  branding and its name.
- A season's theme reaches the window icon wherever branding does, and not the
  installed desktop icon.
