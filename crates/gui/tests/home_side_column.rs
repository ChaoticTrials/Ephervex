//! The column beside Home: the storage card with its counts and the shared
//! download cache, the tips, and where the column is and is not.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use ephervex_gui::ui::{AppWindow, InstanceRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn home() -> AppWindow {
    let ui = support::test_window();
    support::open_home(&ui);
    settle(&ui);
    ui
}

fn shown(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

fn elements(ui: &AppWindow, type_name: &str) -> usize {
    ElementHandle::find_by_element_type_name(ui, type_name)
        .filter(|element| element.size().width > 0.0)
        .count()
}

fn measured(ui: &AppWindow, used: &str, total: &str, fill: f32) {
    ui.set_artifact_cache_used(used.into());
    ui.set_artifact_cache_total(total.into());
    ui.set_artifact_cache_fill(fill);
    ui.set_artifact_cache_usage_known(true);
    settle(ui);
}

fn usage(used: &str, total: &str) -> String {
    lookup_args(
        "gui-home-cache-usage",
        [("first", Arg::from(used)), ("second", Arg::from(total))],
    )
}

#[test]
fn the_column_stands_beside_home_and_nowhere_else() {
    let ui = home();
    assert_eq!(elements(&ui, "RightColumn"), 1);

    support::open_settings(&ui, "General");
    settle(&ui);
    assert_eq!(
        elements(&ui, "RightColumn"),
        0,
        "the settings take its width"
    );

    support::show_client(&ui, InstanceRow::default(), "Overview");
    settle(&ui);
    assert_eq!(elements(&ui, "RightColumn"), 0, "so does a host");

    support::open_home(&ui);
    settle(&ui);
    assert_eq!(elements(&ui, "RightColumn"), 1);
}

/// The centre keeps the column's width and its gap on every page without one.
#[test]
fn a_host_is_wider_than_home_by_the_column_and_its_gap() {
    let ui = home();
    let with_column = ElementHandle::find_by_element_type_name(&ui, "HomePage")
        .next()
        .expect("Home")
        .size()
        .width;

    support::show_client(&ui, InstanceRow::default(), "Overview");
    settle(&ui);
    let workspace_width = ElementHandle::find_by_element_type_name(&ui, "HostWorkspace")
        .map(|pane| pane.size().width)
        .fold(0.0_f32, f32::max);

    let theme = ui.global::<Theme>();
    let reserved = theme.get_side_column_width() + theme.get_shell_gap();
    assert!(
        (workspace_width - with_column - reserved).abs() < 1.0,
        "Home is {with_column}px, a host {workspace_width}px, the column {reserved}px"
    );
}

#[test]
fn the_tiles_show_how_many_hosts_there_are_of_each_kind() {
    let ui = home();
    ui.set_home_temporary_count(5);
    ui.set_home_persistent_count(1);
    ui.set_home_server_count(4);
    settle(&ui);

    for (count, key) in [
        (5, "gui-home-storage-temporary"),
        (1, "gui-home-storage-persistent"),
        (4, "gui-home-storage-servers"),
    ] {
        assert!(
            shown(&ui, &format!("{count} {}", lookup(key))),
            "a tile counts {count} {}",
            lookup(key)
        );
    }
}

#[test]
fn a_cache_that_has_not_been_measured_shows_no_numbers() {
    let ui = home();
    assert!(shown(&ui, &lookup("gui-artifact-cache-measuring")));
    assert_eq!(elements(&ui, "SegmentedMeter"), 0);

    ui.set_artifact_cache_unreadable(true);
    settle(&ui);
    assert!(shown(&ui, &lookup("gui-home-cache-unreadable")));
    assert!(!shown(&ui, &lookup("gui-artifact-cache-measuring")));
    assert_eq!(elements(&ui, "SegmentedMeter"), 0);
}

#[test]
fn a_measured_cache_shows_its_use_of_the_budget() {
    let ui = home();
    measured(&ui, "3.1 GiB", "10 GiB", 0.31);

    assert!(shown(&ui, &usage("3.1 GiB", "10 GiB")));
    assert_eq!(elements(&ui, "SegmentedMeter"), 1);
    assert!(!shown(&ui, &lookup("gui-artifact-cache-over-budget")));

    ui.set_artifact_cache_usage_exact(false);
    settle(&ui);
    let approximate = lookup_args(
        "gui-home-cache-usage-approximate",
        [
            ("first", Arg::from("3.1 GiB")),
            ("second", Arg::from("10 GiB")),
        ],
    );
    assert!(
        shown(&ui, &approximate),
        "an unfinished walk is a lower bound"
    );
}

#[test]
fn a_cache_over_its_budget_says_so() {
    let ui = home();
    measured(&ui, "12.4 GiB", "10 GiB", 1.0);
    ui.set_artifact_cache_over_budget(true);
    settle(&ui);

    assert!(shown(&ui, &usage("12.4 GiB", "10 GiB")));
    assert!(shown(&ui, &lookup("gui-artifact-cache-over-budget")));
    assert_eq!(elements(&ui, "SegmentedMeter"), 1);
}

#[test]
fn an_unlimited_cache_has_no_meter() {
    let ui = home();
    measured(&ui, "3.1 GiB", "Unlimited", 0.0);
    ui.set_artifact_cache_unlimited(true);
    settle(&ui);

    let text = lookup_args(
        "gui-home-cache-unlimited",
        [("value", Arg::from("3.1 GiB"))],
    );
    assert!(shown(&ui, &text));
    assert_eq!(elements(&ui, "SegmentedMeter"), 0);
}

#[test]
fn a_failure_after_a_good_measurement_shows_only_the_failure() {
    let ui = home();
    measured(&ui, "3.1 GiB", "10 GiB", 0.31);
    ui.set_artifact_cache_over_budget(true);
    ui.set_artifact_cache_unreadable(true);
    settle(&ui);

    assert!(shown(&ui, &lookup("gui-home-cache-unreadable")));
    assert!(!shown(&ui, &usage("3.1 GiB", "10 GiB")));
    assert!(!shown(&ui, &lookup("gui-artifact-cache-over-budget")));
    assert_eq!(elements(&ui, "SegmentedMeter"), 0);
}

#[test]
fn an_unlimited_cache_that_could_not_be_fully_read_says_at_least() {
    let ui = home();
    measured(&ui, "3.1 GiB", "Unlimited", 0.0);
    ui.set_artifact_cache_unlimited(true);
    ui.set_artifact_cache_usage_exact(false);
    settle(&ui);

    let text = lookup_args(
        "gui-home-cache-unlimited-approximate",
        [("value", Arg::from("3.1 GiB"))],
    );
    assert!(shown(&ui, &text));
    assert!(!shown(
        &ui,
        &lookup_args(
            "gui-home-cache-unlimited",
            [("value", Arg::from("3.1 GiB"))]
        )
    ));
}

#[test]
fn the_tips_name_controls_that_exist() {
    let ui = home();
    assert!(shown(&ui, &lookup("gui-home-tips")));
    assert!(shown(&ui, &lookup("gui-home-tip-open")));
    let show_beside = lookup("gui-pair-show-beside");
    let pair = lookup_args(
        "gui-home-tip-pair",
        [("value", Arg::from(show_beside.clone()))],
    );
    assert!(
        shown(&ui, &pair),
        "the tip names Show beside by its own label"
    );
    assert!(
        !pair
            .to_lowercase()
            .contains(&lookup("gui-workspace-split").to_lowercase()),
        "Split view puts the same host in two panes, so the tip does not claim it"
    );
    assert!(
        !lookup("gui-home-tip-open")
            .to_lowercase()
            .contains("quick view")
    );
}

#[test]
fn no_card_shadow_reaches_past_the_column_at_the_smallest_window() {
    let ui = home();
    support::set_minimum_size(&ui);
    ui.set_artifact_cache_over_budget(true);
    measured(&ui, "12.4 GiB", "10 GiB", 1.0);

    let theme = ui.global::<Theme>();
    let reach = theme.get_shadow_room();
    let column = ElementHandle::find_by_element_type_name(&ui, "RightColumn")
        .next()
        .expect("the column is on screen");
    let right = column.absolute_position().x + column.size().width;
    let (_, window_height) = support::minimum_size(&ui);
    #[allow(clippy::cast_precision_loss)]
    let window_height = window_height as f32;
    for card in ["StorageCard", "TipsCard"] {
        let element = ElementHandle::find_by_element_type_name(&ui, card)
            .find(|element| element.size().width > 0.0)
            .unwrap_or_else(|| panic!("no {card}"));
        let edge = element.absolute_position().x + element.size().width + reach;
        assert!(
            edge <= right + 0.5,
            "a {card}'s shadow reaches {edge}px past {right}px"
        );
        let bottom = element.absolute_position().y + element.size().height + reach;
        assert!(bottom <= window_height, "a {card} is cut off at {bottom}px");
    }
}
