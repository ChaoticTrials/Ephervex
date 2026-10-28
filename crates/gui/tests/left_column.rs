//! The column down the left of the window: the brand bar with the version, the
//! row of Home, search, and filters, the empty list, and the account chip.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AccountRow, AppWindow, InstanceRow, MenuRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, VecModel};

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

/// The narrowest window that shows the column with the largest text, where it is tightest.
fn tight() -> AppWindow {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    ui.global::<Theme>().set_text_scale(1.25);
    ui.set_loading(false);
    settle(&ui);
    ui
}

fn control(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| {
            element.accessible_role() == Some(AccessibleRole::Button)
                || element.accessible_role() == Some(AccessibleRole::TextInput)
        })
        .unwrap_or_else(|| panic!("no control labeled {label}"))
}

fn column_right(ui: &AppWindow) -> f32 {
    let theme = ui.global::<Theme>();
    theme.get_shell_padding() + theme.get_list_column_width()
}

#[test]
fn the_brand_bar_shows_the_application_version_under_the_wordmark() {
    let ui = support::test_window();
    ui.set_app_version("0.12.0".into());
    settle(&ui);

    let version = ElementHandle::find_by_accessible_label(&ui, "0.12.0")
        .find(|element| element.size().width > 0.0)
        .expect("the version is shown");
    let wordmark = ElementHandle::find_by_accessible_label(&ui, "EPHERVEX")
        .find(|element| element.size().width > 0.0)
        .expect("the wordmark is shown");
    assert!(
        version.absolute_position().y
            >= wordmark.absolute_position().y + wordmark.size().height - 1.0
    );
}

#[test]
fn home_search_and_filters_share_one_row_inside_the_column() {
    let ui = tight();
    let home = control(&ui, &lookup("gui-home"));
    let search = control(&ui, &lookup("gui-filter-hosts"));
    let filters = control(&ui, &lookup("gui-more-filters"));

    let top = home.absolute_position().y;
    for element in [&search, &filters] {
        assert!((element.absolute_position().y - top).abs() < 1.0, "one row");
    }
    let right = filters.absolute_position().x + filters.size().width;
    assert!(
        right <= column_right(&ui),
        "the filter toggle ends at {right}px"
    );
    assert!(
        home.absolute_position().x + home.size().width <= search.absolute_position().x,
        "Home and the search field do not overlap"
    );
    assert!(search.absolute_position().x + search.size().width <= filters.absolute_position().x);
}

#[test]
fn the_filter_toggle_opens_the_panel_and_carries_the_count_of_choices() {
    let ui = tight();
    ui.set_instance_favorites_only(true);
    ui.set_instance_loader_index(1);
    settle(&ui);

    let counted = lookup_args("gui-more-filters-active", [("value", Arg::from("2"))]);
    control(&ui, &counted).mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert!(ui.get_instance_filters_open());
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "2").any(|element| element.size().width > 0.0),
        "the count is on the button"
    );
}

#[test]
fn the_empty_lists_buttons_stay_inside_the_column() {
    let ui = tight();
    let right = column_right(&ui);
    for key in ["gui-modal-new-instance", "gui-import-modpack"] {
        let button = ElementHandle::find_by_accessible_label(&ui, &lookup(key))
            .find(|element| {
                element.accessible_role() == Some(AccessibleRole::Button)
                    && element.absolute_position().x < right
            })
            .unwrap_or_else(|| panic!("the empty list offers {key}"));
        let edge = button.absolute_position().x + button.size().width;
        assert!(
            edge <= right,
            "{key} reaches {edge}px past the column at {right}px"
        );
    }
}

fn with_account(ui: &AppWindow) {
    ui.set_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: "alice-id".into(),
        name: "Alice".into(),
        kind: "Microsoft".into(),
        is_default: true,
        ..AccountRow::default()
    }])));
    ui.set_default_account_name("Alice".into());
    ui.set_default_account_initial("A".into());
    ui.set_default_account_kind("Microsoft".into());
    ui.set_account_launch_entries(ModelRc::new(VecModel::from(vec![MenuRow {
        id: "alice-id".into(),
        label: "Alice".into(),
        ..MenuRow::default()
    }])));
    settle(ui);
}

#[test]
fn the_account_chip_shows_the_initial_name_and_kind_and_stays_in_the_column() {
    let ui = tight();
    with_account(&ui);

    for text in ["A", "Alice", "Microsoft"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, text)
                .any(|element| element.size().width > 0.0),
            "the chip shows {text}"
        );
    }
    let chip = control(
        &ui,
        &lookup_args("gui-account-current", [("value", Arg::from("Alice"))]),
    );
    assert!(chip.absolute_position().x + chip.size().width <= column_right(&ui));
}

#[test]
fn the_account_chip_opens_a_menu_that_switches_and_manages() {
    let ui = tight();
    with_account(&ui);
    let chosen: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&chosen);
    ui.on_set_default_account(move |id| record.borrow_mut().push(id.to_string()));

    control(
        &ui,
        &lookup_args("gui-account-current", [("value", Arg::from("Alice"))]),
    )
    .mock_single_click(PointerEventButton::Left);
    settle(&ui);
    let entry = |label: &str| {
        ElementHandle::find_by_accessible_label(&ui, label).find(|element| {
            element.accessible_role() == Some(AccessibleRole::Button)
                && element.size().width > 0.0
                && element.accessible_enabled().is_some()
        })
    };
    entry("Alice")
        .expect("the menu lists the account")
        .invoke_accessible_default_action();
    assert_eq!(chosen.borrow().as_slice(), ["alice-id"]);

    control(
        &ui,
        &lookup_args("gui-account-current", [("value", Arg::from("Alice"))]),
    )
    .mock_single_click(PointerEventButton::Left);
    settle(&ui);
    entry(&lookup("gui-account-manage"))
        .expect("and a way to manage them")
        .invoke_accessible_default_action();
    assert!(ui.get_settings_open());
    assert_eq!(ui.get_settings_tab(), "Accounts");
}

#[test]
fn a_client_card_says_when_it_last_ran_once() {
    let ui = support::test_window();
    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![InstanceRow {
        id: "repro".into(),
        name: "Repro".into(),
        state_kind: "ready".into(),
        state_line: lookup("gui-state-line-idle").into(),
        last_run: "2 hours ago".into(),
        icon_slot: -1,
        ..InstanceRow::default()
    }])));
    settle(&ui);

    let count = |text: &str| {
        ElementHandle::find_by_accessible_label(&ui, text)
            .filter(|element| element.size().width > 0.0)
            .count()
    };
    assert_eq!(count("2 hours ago"), 1);
    assert_eq!(count("Ready"), 1);
    assert_eq!(count("Last run 2 hours ago"), 0);
}

fn right_edge(element: &ElementHandle) -> f32 {
    element.absolute_position().x + element.size().width
}

/// The darkness of the pixel at `(x, y)` of the window as drawn.
fn darkness(ui: &AppWindow, x: f32, y: f32) -> u32 {
    let buffer = ui.window().take_snapshot().expect("render");
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixel = buffer.as_slice()[y as usize * buffer.width() as usize + x as usize];
    765 - (u32::from(pixel.r) + u32::from(pixel.g) + u32::from(pixel.b))
}

#[test]
fn the_refresh_search_filter_and_settings_controls_share_a_right_edge() {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    ui.set_loading(false);
    settle(&ui);

    let refresh = right_edge(&control(&ui, &lookup("gui-refresh")));
    let filter = right_edge(&control(&ui, &lookup("gui-more-filters")));
    let settings = right_edge(&control(&ui, &lookup("gui-settings")));
    assert!(
        (refresh - filter).abs() < 0.5,
        "refresh {refresh}, filter {filter}"
    );
    assert!(
        (settings - filter).abs() < 0.5,
        "settings {settings}, filter {filter}"
    );
}

#[test]
fn the_refresh_search_filter_and_settings_controls_draw_a_hard_shadow() {
    let ui = support::test_window();
    support::set_narrowest_with_list(&ui);
    ui.set_loading(false);
    settle(&ui);

    for label in [
        lookup("gui-refresh"),
        lookup("gui-more-filters"),
        lookup("gui-settings"),
        lookup("gui-filter-hosts"),
    ] {
        // A text field reports its inner input too; the field is the widest.
        let element = ElementHandle::find_by_accessible_label(&ui, &label)
            .max_by(|a, b| a.size().width.total_cmp(&b.size().width))
            .expect("the control is on the window");
        let (x, y) = (right_edge(&element), element.absolute_position().y);
        let middle = y + element.size().height / 2.0;
        let shadow = darkness(&ui, x + 1.5, middle);
        // Past the three pixels of shadow and short of the neighbor 8px away.
        let beyond = darkness(&ui, x + 5.5, middle);
        assert!(
            shadow > beyond + 100,
            "{label:?} draws no shadow to its right ({shadow} against {beyond})"
        );
    }
}
