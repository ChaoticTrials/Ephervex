//! A server's live statistics as its Overview shows them.
//!
//! The values come from the server's console and `ops.json`, so they are
//! bounded again here: the interface never lays out more rows than
//! [`SHOWN_ROWS`], whatever the server reported.

use std::fmt::Write as _;

use ephervex_core::{OperatorListing, PlayerRoster};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ModelRc, SharedString, VecModel};

use crate::ui::{OperatorRow, ServerStatisticsView};

/// How many players or operators are listed before the rest is counted.
pub const SHOWN_ROWS: usize = 30;
/// The chart's smallest top value: the tick rate of an unmodified server.
const CHART_FLOOR: f32 = 20.0;
/// The chart's drawing box, matching the viewbox the markup gives its path.
const CHART_WIDTH: f32 = 100.0;
const CHART_HEIGHT: f32 = 40.0;

/// What the backend read for one server.
#[derive(Debug, Clone)]
pub struct ServerStatisticsData {
    pub server: String,
    /// Tick rates the server reported, oldest first.
    pub tick_rates: Vec<f32>,
    /// Whether the server's version has a console command for its tick rate.
    pub reports_tick_rate: bool,
    /// The newest player report, if the server gave one.
    pub players: Option<PlayerRoster>,
    /// The server's operators, or `None` when `ops.json` could not be read.
    pub operators: Option<OperatorListing>,
}

/// The view for what the backend read. Whether the server runs is the
/// markup's to apply, so a stopped server's last run never needs clearing.
pub fn present(data: &ServerStatisticsData) -> ServerStatisticsView {
    let mut view = ServerStatisticsView::default();
    present_tick_rate(&mut view, data);
    present_players(&mut view, data.players.as_ref());
    present_operators(&mut view, data.operators.as_ref());
    view
}

/// Drops what a run's console reported, keeping the operators, which come
/// from a file. A run that is not the current one must never show as live.
pub fn forget_console_values(view: &mut ServerStatisticsView) {
    let operators = std::mem::take(&mut view.operators);
    let operators_note = std::mem::take(&mut view.operators_note);
    let operators_more = std::mem::take(&mut view.operators_more);
    *view = ServerStatisticsView {
        operators,
        operators_note,
        operators_more,
        ..ServerStatisticsView::default()
    };
}

fn present_tick_rate(view: &mut ServerStatisticsView, data: &ServerStatisticsData) {
    let Some(latest) = data.tick_rates.last() else {
        view.tick_note = lookup(if data.reports_tick_rate {
            "gui-tick-rate-waiting"
        } else {
            "gui-tick-rate-unsupported"
        })
        .into();
        return;
    };
    let scale = data
        .tick_rates
        .iter()
        .copied()
        .fold(CHART_FLOOR, f32::max)
        .ceil();
    view.tick_current = lookup_args(
        "gui-tick-rate-value",
        [("value", Arg::from(format!("{latest:.1}")))],
    )
    .into();
    view.tick_scale = format!("{scale:.0}").into();
    view.tick_path = chart_path(&data.tick_rates, scale).into();
}

/// The history as one SVG polyline, oldest at the left. A single sample is a
/// level line, so it still shows.
fn chart_path(rates: &[f32], scale: f32) -> String {
    let y = |rate: f32| CHART_HEIGHT - (rate / scale).clamp(0.0, 1.0) * CHART_HEIGHT;
    if let [only] = rates {
        return format!("M 0 {0:.1} L {CHART_WIDTH} {0:.1}", y(*only));
    }
    let last = rates.len().saturating_sub(1).max(1);
    let mut path = String::new();
    for (index, rate) in rates.iter().enumerate() {
        // Sample counts are far below f32's exact integer range.
        #[allow(clippy::cast_precision_loss)]
        let x = CHART_WIDTH * index as f32 / last as f32;
        path.push_str(if index == 0 { "M " } else { " L " });
        let _ = write!(path, "{x:.1} {:.1}", y(*rate));
    }
    path
}

fn present_players(view: &mut ServerStatisticsView, players: Option<&PlayerRoster>) {
    let Some(roster) = players else {
        view.players_note = lookup("gui-players-waiting").into();
        return;
    };
    view.players_summary = match roster.max {
        Some(max) => lookup_args(
            "gui-players-count",
            [
                ("online", Arg::from(roster.online)),
                ("max", Arg::from(max)),
            ],
        ),
        None => lookup_args(
            "gui-players-count-unlimited",
            [("online", Arg::from(roster.online))],
        ),
    }
    .into();
    if roster.online == 0 && roster.names.is_empty() {
        view.players_note = lookup("gui-players-none").into();
        return;
    }
    let names: Vec<SharedString> = roster
        .names
        .iter()
        .take(SHOWN_ROWS)
        .map(SharedString::from)
        .collect();
    let hidden = (roster.online as usize).saturating_sub(names.len());
    view.players = ModelRc::new(VecModel::from(names));
    view.players_more = more(hidden);
}

fn present_operators(view: &mut ServerStatisticsView, operators: Option<&OperatorListing>) {
    let Some(listing) = operators else {
        view.operators_note = lookup("gui-operators-unreadable").into();
        return;
    };
    if listing.operators.is_empty() {
        view.operators_note = lookup("gui-operators-none").into();
        return;
    }
    let rows: Vec<OperatorRow> = listing
        .operators
        .iter()
        .take(SHOWN_ROWS)
        .map(|operator| OperatorRow {
            name: operator.name.as_str().into(),
            level: operator.level.map(level_label).unwrap_or_default().into(),
        })
        .collect();
    let hidden = (listing.operators.len() - rows.len()) + listing.omitted;
    view.operators = ModelRc::new(VecModel::from(rows));
    view.operators_more = more(hidden);
}

/// The permission level's name. Levels outside 1 through 4 are shown as the
/// number the file holds.
fn level_label(level: i64) -> String {
    match level {
        1..=4 => lookup(&format!("gui-operator-level-{level}")),
        other => lookup_args("gui-operator-level", [("level", Arg::from(other))]),
    }
}

fn more(hidden: usize) -> SharedString {
    if hidden == 0 {
        return SharedString::default();
    }
    lookup_args("gui-more-entries", [("count", Arg::from(hidden))]).into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use ephervex_core::ListedOperator;
    use slint::Model as _;

    use super::*;

    fn data() -> ServerStatisticsData {
        ServerStatisticsData {
            server: "server".to_owned(),
            tick_rates: Vec::new(),
            reports_tick_rate: true,
            players: None,
            operators: Some(OperatorListing::default()),
        }
    }

    fn texts(model: &ModelRc<SharedString>) -> Vec<String> {
        model.iter().map(|text| text.to_string()).collect()
    }

    #[test]
    fn forgetting_a_runs_console_values_keeps_the_operators() {
        let mut view = present(&ServerStatisticsData {
            tick_rates: vec![20.0],
            players: Some(PlayerRoster {
                names: vec!["Alice".to_owned()],
                online: 1,
                max: Some(20),
            }),
            operators: Some(OperatorListing {
                operators: vec![ListedOperator {
                    name: "Alice".to_owned(),
                    level: Some(4),
                }],
                omitted: 0,
            }),
            ..data()
        });

        forget_console_values(&mut view);

        assert!(view.tick_path.is_empty() && view.tick_current.is_empty());
        assert!(view.players_summary.is_empty() && view.players.row_count() == 0);
        assert_eq!(view.operators.row_count(), 1);
    }

    #[test]
    fn a_server_that_has_not_reported_says_what_it_is_waiting_for() {
        let view = present(&data());
        assert_eq!(view.tick_note, lookup("gui-tick-rate-waiting"));
        assert_eq!(view.players_note, lookup("gui-players-waiting"));
        assert!(view.tick_path.is_empty());
        assert_eq!(view.players.row_count(), 0);
    }

    #[test]
    fn a_version_without_a_tick_command_says_so_instead_of_waiting() {
        let view = present(&ServerStatisticsData {
            reports_tick_rate: false,
            ..data()
        });
        assert_eq!(view.tick_note, lookup("gui-tick-rate-unsupported"));
    }

    #[test]
    fn tick_rates_chart_oldest_first_against_a_scale_of_at_least_twenty() {
        let view = present(&ServerStatisticsData {
            tick_rates: vec![20.0, 10.0, 0.0],
            ..data()
        });
        assert_eq!(view.tick_path, "M 0.0 0.0 L 50.0 20.0 L 100.0 40.0");
        assert_eq!(view.tick_scale, "20");
        assert_eq!(view.tick_current, "0.0 TPS");
        assert!(view.tick_note.is_empty());
    }

    #[test]
    fn a_rate_above_twenty_raises_the_scale() {
        let view = present(&ServerStatisticsData {
            tick_rates: vec![40.0, 20.0],
            ..data()
        });
        assert_eq!(view.tick_scale, "40");
        assert_eq!(view.tick_path, "M 0.0 0.0 L 100.0 20.0");
    }

    #[test]
    fn one_sample_is_a_level_line() {
        let view = present(&ServerStatisticsData {
            tick_rates: vec![20.0],
            ..data()
        });
        assert_eq!(view.tick_path, "M 0 0.0 L 100 0.0");
    }

    #[test]
    fn players_show_a_count_with_their_limit_and_names() {
        let view = present(&ServerStatisticsData {
            players: Some(PlayerRoster {
                names: vec!["Alice".to_owned(), "Bob".to_owned()],
                online: 2,
                max: Some(20),
            }),
            ..data()
        });
        assert_eq!(
            view.players_summary,
            lookup_args(
                "gui-players-count",
                [("online", Arg::from(2_u32)), ("max", Arg::from(20_u32))]
            )
        );
        assert_eq!(texts(&view.players), ["Alice", "Bob"]);
        assert!(view.players_more.is_empty());
    }

    #[test]
    fn an_empty_roster_says_nobody_is_online() {
        let view = present(&ServerStatisticsData {
            players: Some(PlayerRoster::default()),
            ..data()
        });
        assert_eq!(view.players_note, lookup("gui-players-none"));
    }

    #[test]
    fn a_long_roster_lists_a_bounded_number_and_counts_the_rest() {
        let names: Vec<String> = (0..100).map(|index| format!("P{index}")).collect();
        let view = present(&ServerStatisticsData {
            players: Some(PlayerRoster {
                names,
                online: 120,
                max: None,
            }),
            ..data()
        });
        assert_eq!(view.players.row_count(), SHOWN_ROWS);
        assert_eq!(
            view.players_more,
            lookup_args("gui-more-entries", [("count", Arg::from(120 - SHOWN_ROWS))])
        );
    }

    #[test]
    fn operators_show_their_level_and_an_unreadable_file_is_explained() {
        let operators = |level| OperatorListing {
            operators: vec![ListedOperator {
                name: "Alice".to_owned(),
                level,
            }],
            omitted: 0,
        };
        let view = present(&ServerStatisticsData {
            operators: Some(operators(Some(4))),
            ..data()
        });
        let row = view.operators.row_data(0).expect("row");
        assert_eq!(row.name, "Alice");
        assert_eq!(row.level, lookup("gui-operator-level-4"));

        let view = present(&ServerStatisticsData {
            operators: Some(operators(Some(9))),
            ..data()
        });
        assert_eq!(
            view.operators.row_data(0).expect("row").level,
            lookup_args("gui-operator-level", [("level", Arg::from(9_i64))])
        );

        let view = present(&ServerStatisticsData {
            operators: Some(operators(None)),
            ..data()
        });
        assert_eq!(view.operators.row_data(0).expect("row").level, "");

        let view = present(&ServerStatisticsData {
            operators: None,
            ..data()
        });
        assert_eq!(view.operators_note, lookup("gui-operators-unreadable"));
    }

    #[test]
    fn operators_beyond_the_shown_rows_and_the_listing_bound_are_counted() {
        let operators = (0..40)
            .map(|index| ListedOperator {
                name: format!("Op{index}"),
                level: Some(2),
            })
            .collect();
        let view = present(&ServerStatisticsData {
            operators: Some(OperatorListing {
                operators,
                omitted: 5,
            }),
            ..data()
        });
        assert_eq!(view.operators.row_count(), SHOWN_ROWS);
        assert_eq!(
            view.operators_more,
            lookup_args("gui-more-entries", [("count", Arg::from(15_usize))])
        );
    }
}
