//! Themes that stand in for a built-in theme during their season.
//!
//! A season is a span of local calendar days that recurs every year. While
//! one is on and seasonal themes are enabled, a user whose recorded theme is
//! Dark or Light sees the season's theme instead. A user theme is always shown
//! as it is. A seasonal theme cannot be chosen or recorded, so the recorded
//! theme comes back on its own once the season ends, whether Ephervex stayed
//! open or was started again (ADR 0049).
//!
//! The `SEASONS_THEME` environment variable names a season to show on every
//! day, for checking a seasonal theme outside its season.

use std::sync::OnceLock;

use chrono::{Datelike as _, NaiveDate};

use crate::ApplicationTheme;

/// Environment variable naming a seasonal theme to show on every day, such as
/// `SEASONS_THEME=halloween`.
const FORCED_SEASON_ENV_VAR: &str = "SEASONS_THEME";

/// A theme that is only ever shown by its season.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeasonalTheme {
    Halloween,
    Christmas,
    Pride,
}

impl SeasonalTheme {
    /// A stable identifier for frontend state. Never recorded in settings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Halloween => "halloween",
            Self::Christmas => "christmas",
            Self::Pride => "pride",
        }
    }
}

/// The value of [`FORCED_SEASON_ENV_VAR`] names no season.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnknownSeasonName {
    name: String,
}

impl std::fmt::Display for UnknownSeasonName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{FORCED_SEASON_ENV_VAR}={:?} names no seasonal theme, so the date decides. Accepted values: {}",
            self.name,
            THEME_SEASONS
                .iter()
                .map(|season| season.theme.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnknownSeasonName {}

/// The season a [`FORCED_SEASON_ENV_VAR`] value names, ignoring case and
/// surrounding whitespace. An empty value names none.
///
fn parse_forced_season(value: &str) -> Result<Option<SeasonalTheme>, UnknownSeasonName> {
    let name = value.trim();
    if name.is_empty() {
        return Ok(None);
    }
    THEME_SEASONS
        .iter()
        .map(|season| season.theme)
        .find(|theme| theme.as_str().eq_ignore_ascii_case(name))
        .map(Some)
        .ok_or_else(|| UnknownSeasonName {
            name: name.to_owned(),
        })
}

/// The season [`FORCED_SEASON_ENV_VAR`] asks to show on every day, if any.
///
/// Read once per process, so a value that names no season is reported once and
/// the answer cannot change while the interface is open. An unset or empty
/// variable and an unknown value both leave the date in charge.
#[must_use]
pub fn forced_season() -> Option<SeasonalTheme> {
    static FORCED: OnceLock<Option<SeasonalTheme>> = OnceLock::new();
    *FORCED.get_or_init(|| {
        let value = std::env::var_os(FORCED_SEASON_ENV_VAR)?;
        let parsed = value.to_str().map_or_else(
            || {
                Err(UnknownSeasonName {
                    name: value.to_string_lossy().into_owned(),
                })
            },
            parse_forced_season,
        );
        parsed.unwrap_or_else(|error| {
            tracing::warn!("{error}");
            None
        })
    })
}

/// A day of the year, independent of the year.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MonthDay {
    month: u32,
    day: u32,
}

impl MonthDay {
    const fn new(month: u32, day: u32) -> Self {
        Self { month, day }
    }

    fn of(date: NaiveDate) -> Self {
        Self::new(date.month(), date.day())
    }
}

/// A seasonal theme and the days it is shown on.
#[derive(Debug, PartialEq, Eq)]
pub struct ThemeSeason {
    pub theme: SeasonalTheme,
    /// The first day of the season.
    pub first: MonthDay,
    /// The last day of the season, which is part of it. A season that ends
    /// on an earlier day than it starts runs over the turn of the year.
    pub last: MonthDay,
}

impl ThemeSeason {
    /// Whether `date` falls in this season.
    #[must_use]
    pub fn contains(&self, date: NaiveDate) -> bool {
        let day = MonthDay::of(date);
        if self.first <= self.last {
            self.first <= day && day <= self.last
        } else {
            day >= self.first || day <= self.last
        }
    }
}

/// Every season. Seasons do not overlap.
pub const THEME_SEASONS: &[ThemeSeason] = &[
    ThemeSeason {
        theme: SeasonalTheme::Halloween,
        first: MonthDay::new(10, 25),
        last: MonthDay::new(10, 31),
    },
    ThemeSeason {
        theme: SeasonalTheme::Christmas,
        first: MonthDay::new(12, 24),
        last: MonthDay::new(12, 26),
    },
    ThemeSeason {
        theme: SeasonalTheme::Pride,
        first: MonthDay::new(6, 1),
        last: MonthDay::new(6, 30),
    },
];

/// The season `date` falls in, if any.
#[must_use]
pub fn season_on(date: NaiveDate) -> Option<&'static ThemeSeason> {
    THEME_SEASONS.iter().find(|season| season.contains(date))
}

/// The theme the interface shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShownTheme {
    /// The theme the user recorded.
    Recorded(ApplicationTheme),
    /// A season's theme, standing in for the recorded built-in theme.
    Seasonal(SeasonalTheme),
}

impl Default for ShownTheme {
    fn default() -> Self {
        Self::Recorded(ApplicationTheme::default())
    }
}

impl ShownTheme {
    /// A stable name for frontend state, distinct for every shown theme. A
    /// recorded theme keeps its recorded name.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Recorded(theme) => theme.to_stored(),
            Self::Seasonal(theme) => format!("season:{}", theme.as_str()),
        }
    }
}

/// The theme shown on `today` for a user who recorded `recorded`.
#[must_use]
pub fn shown_theme(
    recorded: &ApplicationTheme,
    seasonal_themes: bool,
    today: NaiveDate,
) -> ShownTheme {
    shown_theme_with(recorded, seasonal_themes, None, today)
}

/// The theme shown on `today`, with `forced` standing in for the date when it
/// names a season. Every other rule is unchanged: a user theme is never
/// replaced and turning seasonal themes off shows the recorded theme.
#[must_use]
pub fn shown_theme_with(
    recorded: &ApplicationTheme,
    seasonal_themes: bool,
    forced: Option<SeasonalTheme>,
    today: NaiveDate,
) -> ShownTheme {
    let season = (seasonal_themes && matches!(recorded, ApplicationTheme::BuiltIn(_)))
        .then(|| forced.or_else(|| season_on(today).map(|season| season.theme)))
        .flatten();
    season.map_or_else(
        || ShownTheme::Recorded(recorded.clone()),
        ShownTheme::Seasonal,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::ApplicationColorScheme;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("a real date")
    }

    const HALLOWEEN: ShownTheme = ShownTheme::Seasonal(SeasonalTheme::Halloween);
    const CHRISTMAS: ShownTheme = ShownTheme::Seasonal(SeasonalTheme::Christmas);
    const PRIDE: ShownTheme = ShownTheme::Seasonal(SeasonalTheme::Pride);

    fn dark() -> ApplicationTheme {
        ApplicationTheme::BuiltIn(ApplicationColorScheme::Dark)
    }

    fn light() -> ApplicationTheme {
        ApplicationTheme::BuiltIn(ApplicationColorScheme::Light)
    }

    #[test]
    fn halloween_is_shown_over_dark_and_light_from_october_25_through_october_31() {
        for recorded in [dark(), light()] {
            for year in [2026, 2027, 2028] {
                let unchanged = ShownTheme::Recorded(recorded.clone());
                assert_eq!(shown_theme(&recorded, true, date(year, 10, 24)), unchanged);
                for day in 25..=31 {
                    assert_eq!(shown_theme(&recorded, true, date(year, 10, day)), HALLOWEEN);
                }
                assert_eq!(shown_theme(&recorded, true, date(year, 11, 1)), unchanged);
            }
        }
        for outside in [date(2026, 1, 1), date(2026, 9, 25), date(2026, 12, 31)] {
            assert_eq!(
                shown_theme(&dark(), true, outside),
                ShownTheme::Recorded(dark())
            );
        }
    }

    #[test]
    fn christmas_is_shown_over_dark_and_light_from_december_24_through_december_26() {
        for recorded in [dark(), light()] {
            for year in [2026, 2027, 2028] {
                let unchanged = ShownTheme::Recorded(recorded.clone());
                assert_eq!(shown_theme(&recorded, true, date(year, 12, 23)), unchanged);
                for day in 24..=26 {
                    assert_eq!(shown_theme(&recorded, true, date(year, 12, day)), CHRISTMAS);
                }
                assert_eq!(shown_theme(&recorded, true, date(year, 12, 27)), unchanged);
            }
        }
    }

    #[test]
    fn pride_is_shown_over_dark_and_light_throughout_june() {
        for recorded in [dark(), light()] {
            for year in [2026, 2027, 2028] {
                let unchanged = ShownTheme::Recorded(recorded.clone());
                assert_eq!(shown_theme(&recorded, true, date(year, 5, 31)), unchanged);
                for day in 1..=30 {
                    assert_eq!(shown_theme(&recorded, true, date(year, 6, day)), PRIDE);
                }
                assert_eq!(shown_theme(&recorded, true, date(year, 7, 1)), unchanged);
            }
        }
    }

    /// Every evaluation reads only the recorded theme and the date, so the
    /// theme shown after a restart is the one shown just before it, and the
    /// first evaluation after midnight is the first to change.
    #[test]
    fn the_shown_theme_changes_only_when_the_date_crosses_a_season_boundary() {
        let mut previous = shown_theme(&light(), true, date(2026, 10, 1));
        let mut changes = Vec::new();
        for offset in 1..=60 {
            let day = date(2026, 10, 1) + chrono::Days::new(offset);
            let now = shown_theme(&light(), true, day);
            if now != previous {
                changes.push((day, now.clone()));
            }
            assert_eq!(shown_theme(&light(), true, day), now, "{day}");
            previous = now;
        }
        assert_eq!(
            changes,
            [
                (date(2026, 10, 25), HALLOWEEN),
                (date(2026, 11, 1), ShownTheme::Recorded(light()))
            ]
        );
    }

    #[test]
    fn a_user_theme_is_shown_during_the_season() {
        let mine = ApplicationTheme::User("modrinth".to_owned());
        assert_eq!(
            shown_theme(&mine, true, date(2026, 10, 31)),
            ShownTheme::Recorded(mine)
        );
    }

    #[test]
    fn turning_seasonal_themes_off_shows_the_recorded_theme_in_season() {
        for recorded in [dark(), light()] {
            assert_eq!(
                shown_theme(&recorded, false, date(2026, 10, 28)),
                ShownTheme::Recorded(recorded)
            );
        }
    }

    #[test]
    fn every_shown_theme_has_its_own_name_and_a_seasonal_one_cannot_be_recorded() {
        assert_eq!(ShownTheme::Recorded(light()).name(), "light");
        assert_eq!(
            ShownTheme::Recorded(ApplicationTheme::User("halloween".to_owned())).name(),
            "user:halloween"
        );
        assert_eq!(HALLOWEEN.name(), "season:halloween");
        assert_eq!(ApplicationTheme::from_stored(&HALLOWEEN.name()), None);
        assert_eq!(ApplicationTheme::from_stored("halloween"), None);
    }

    #[test]
    fn a_forced_season_is_shown_on_every_day_over_dark_and_light() {
        let forced = Some(SeasonalTheme::Halloween);
        for recorded in [dark(), light()] {
            for outside in [date(2026, 1, 1), date(2026, 10, 24), date(2026, 11, 1)] {
                assert_eq!(
                    shown_theme_with(&recorded, true, forced, outside),
                    HALLOWEEN,
                    "{outside}"
                );
            }
        }
    }

    #[test]
    fn without_a_forced_season_the_date_decides() {
        for (day, expected) in [
            (date(2026, 10, 24), ShownTheme::Recorded(dark())),
            (date(2026, 10, 25), HALLOWEEN),
        ] {
            assert_eq!(shown_theme_with(&dark(), true, None, day), expected);
            assert_eq!(shown_theme(&dark(), true, day), expected);
        }
    }

    #[test]
    fn a_forced_season_does_not_replace_a_user_theme() {
        let mine = ApplicationTheme::User("modrinth".to_owned());
        assert_eq!(
            shown_theme_with(
                &mine,
                true,
                Some(SeasonalTheme::Halloween),
                date(2026, 3, 1)
            ),
            ShownTheme::Recorded(mine)
        );
    }

    #[test]
    fn turning_seasonal_themes_off_shows_the_recorded_theme_despite_a_forced_season() {
        assert_eq!(
            shown_theme_with(
                &light(),
                false,
                Some(SeasonalTheme::Halloween),
                date(2026, 3, 1)
            ),
            ShownTheme::Recorded(light())
        );
    }

    #[test]
    fn a_forced_season_name_is_matched_ignoring_case_and_whitespace() {
        for value in ["halloween", "Halloween", "HALLOWEEN", "  halloween\n"] {
            assert_eq!(
                parse_forced_season(value),
                Ok(Some(SeasonalTheme::Halloween)),
                "{value:?}"
            );
        }
    }

    #[test]
    fn an_empty_forced_season_leaves_the_date_in_charge() {
        assert_eq!(parse_forced_season(""), Ok(None));
        assert_eq!(parse_forced_season("   "), Ok(None));
    }

    #[test]
    fn an_unknown_forced_season_is_an_error_naming_the_accepted_values() {
        let error = parse_forced_season("easter").expect_err("no such season");
        assert_eq!(error.name, "easter");
        let message = error.to_string();
        assert!(message.contains("SEASONS_THEME"), "{message}");
        assert!(message.contains("\"easter\""), "{message}");
        assert!(message.contains("halloween"), "{message}");
    }

    #[test]
    fn every_season_in_the_table_can_be_forced_by_its_identifier() {
        for season in THEME_SEASONS {
            assert_eq!(
                parse_forced_season(season.theme.as_str()),
                Ok(Some(season.theme))
            );
        }
    }

    #[test]
    fn a_season_can_run_over_the_turn_of_the_year() {
        let season = ThemeSeason {
            theme: SeasonalTheme::Halloween,
            first: MonthDay::new(12, 24),
            last: MonthDay::new(1, 6),
        };
        for inside in [date(2026, 12, 24), date(2026, 12, 31), date(2027, 1, 6)] {
            assert!(season.contains(inside), "{inside}");
        }
        for outside in [date(2026, 12, 23), date(2027, 1, 7), date(2027, 6, 1)] {
            assert!(!season.contains(outside), "{outside}");
        }
    }

    #[test]
    fn no_two_seasons_share_a_day() {
        let mut day = date(2028, 1, 1);
        while day.year() == 2028 {
            let matching = THEME_SEASONS
                .iter()
                .filter(|season| season.contains(day))
                .count();
            assert!(matching <= 1, "{day}");
            day = day.succ_opt().expect("next day");
        }
    }
}
