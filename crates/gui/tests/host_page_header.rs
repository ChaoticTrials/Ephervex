//! The header of a host's page: what the host is, how it stands, and the
//! actions it offers, all inside the page at the narrowest window that has the list beside it.

#![allow(clippy::expect_used)]

mod support;

use ephervex_gui::ui::{InstanceRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

fn client() -> InstanceRow {
    InstanceRow {
        id: "client".into(),
        name: "Conveyor dupe #412".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.3.0".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        mods: 38,
        paired_server_id: "server".into(),
        paired_server_name: "conveyor-srv".into(),
        state: "Running".into(),
        state_kind: "running".into(),
        can_kill: true,
        can_restart: true,
        ..InstanceRow::default()
    }
}

fn labeled(ui: &ephervex_gui::ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .expect("the page has an element with this label")
}

#[test]
fn the_header_chips_say_what_the_host_is() {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    support::show_client(&ui, client(), "Overview");

    for label in ["1.20.1", "Forge 47.3.0", "Temporary", "conveyor-srv"] {
        labeled(&ui, label);
    }
    let mods = lookup_args("gui-host-chip-mods", [("count", Arg::Count(38))]);
    assert_eq!(mods, "38 mods");
    labeled(&ui, &mods);
}

fn stamped(mut row: InstanceRow, label: &str, detail: &str) {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    row.id = "client".into();
    support::show_client(&ui, row, "Overview");
    labeled(
        &ui,
        &if detail.is_empty() {
            label.to_owned()
        } else {
            format!("{label}. {detail}")
        },
    );
}

#[test]
fn the_stamp_says_what_the_host_is_doing_or_what_it_needs() {
    let idle = InstanceRow {
        state: "Ready".into(),
        state_kind: "idle".into(),
        ..InstanceRow::default()
    };
    stamped(client(), "Running", "in game");
    stamped(
        InstanceRow {
            problems: 2,
            needs_attention: true,
            ..idle.clone()
        },
        "Needs fix",
        "2 problems before it can start",
    );
    stamped(
        InstanceRow {
            case_reproduced: 3,
            ..idle.clone()
        },
        "Reproduced \u{d7}3",
        "seen again",
    );
    stamped(idle, "Not yet", "not reproduced yet");
    stamped(
        InstanceRow {
            state: "Orphaned".into(),
            state_kind: "orphaned".into(),
            ..InstanceRow::default()
        },
        "Orphaned",
        "",
    );
}

#[test]
fn a_host_with_no_mods_still_says_so_in_its_header() {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    let mut row = client();
    row.mods = 0;
    support::show_client(&ui, row, "Overview");

    let zero = lookup_args("gui-host-chip-mods", [("count", Arg::Count(0))]);
    labeled(&ui, &zero);
}

#[test]
fn every_header_action_sits_inside_the_page_at_the_smallest_window() {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    support::show_client(&ui, client(), "Overview");

    // The page ends at the window's edge less the shell's outer padding.
    let page_right = page_right(&ui);
    for label in [
        lookup("gui-open-folder"),
        lookup("gui-restart"),
        lookup("gui-stop-game"),
    ] {
        let button = labeled(&ui, &label);
        let right = button.absolute_position().x + button.size().width;
        assert!(
            right <= page_right + 1.0,
            "{label:?} ends at {right}, beyond the page's {page_right}"
        );
        assert!(
            button.absolute_position().x >= 0.0,
            "{label:?} starts off the window"
        );
    }
}

#[allow(clippy::cast_precision_loss)]
fn page_right(ui: &ephervex_gui::ui::AppWindow) -> f32 {
    ui.window().size().width as f32 - ui.global::<Theme>().get_shell_padding()
}

#[test]
fn the_chip_strip_stays_inside_the_page_with_long_values_and_large_text() {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    ui.global::<Theme>().set_text_scale(1.25);
    let mut row = client();
    row.loader = "NeoForge 21.1.249".into();
    row.paired_server_name = "A very long linked server name that must not push the strip".into();
    row.mods = 1234;
    support::show_client(&ui, row.clone(), "Overview");

    let page_right = page_right(&ui);
    for label in [
        "1.20.1".to_string(),
        row.loader.to_string(),
        "1234 mods".to_string(),
        "Temporary".to_string(),
        row.paired_server_name.to_string(),
    ] {
        let chip = labeled(&ui, &label);
        let right = chip.absolute_position().x + chip.size().width;
        assert!(
            right <= page_right,
            "the chip {label:?} ends at {right}, beyond the page's {page_right}"
        );
    }
}
