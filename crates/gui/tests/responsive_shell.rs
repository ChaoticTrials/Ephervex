//! The shell's columns in windows of different widths: beside the page when
//! there is room, otherwise drawers opened from a rail.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use ephervex_gui::ui::{AppWindow, InstanceRow, Theme};
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};

fn window(width: u32) -> AppWindow {
    let ui = support::test_window();
    ui.window().set_size(PhysicalSize::new(width, 900));
    support::open_home(&ui);
    settle(&ui);
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

/// Whether the window draws the element: a drawer that is closed is hidden.
fn drawn(ui: &AppWindow, type_name: &str) -> bool {
    ElementHandle::find_by_element_type_name(ui, type_name)
        .next()
        .is_some()
}

fn left_edge(ui: &AppWindow, type_name: &str) -> f32 {
    ElementHandle::find_by_element_type_name(ui, type_name)
        .next()
        .unwrap_or_else(|| panic!("{type_name} is in the window"))
        .absolute_position()
        .x
}

fn right_edge(ui: &AppWindow, type_name: &str) -> f32 {
    let element = ElementHandle::find_by_element_type_name(ui, type_name)
        .next()
        .unwrap_or_else(|| panic!("{type_name} is in the window"));
    element.absolute_position().x + element.size().width
}

fn rail_button(ui: &AppWindow, key: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup(key))
        .find(|element| element.size().width > 0.0)
}

fn press_rail_button(ui: &AppWindow, key: &str) {
    rail_button(ui, key)
        .unwrap_or_else(|| panic!("the rail offers {key}"))
        .invoke_accessible_default_action();
    settle(ui);
}

fn window_width(ui: &AppWindow) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let width = ui.window().size().width as f32;
    width
}

#[test]
fn the_window_can_be_narrower_than_the_old_minimum() {
    let ui = support::test_window();
    assert_eq!(ui.global::<Theme>().get_window_min_width().round(), 1000.0);
}

#[test]
fn a_wide_window_keeps_both_columns_beside_the_page() {
    let ui = window(1480);
    assert!(rail_button(&ui, "gui-shell-show-list").is_none());
    assert!(rail_button(&ui, "gui-shell-show-side").is_none());

    let padding = ui.global::<Theme>().get_shell_padding();
    assert!((left_edge(&ui, "HostList") - padding).abs() < 1.0);
    assert!(right_edge(&ui, "HostList") <= left_edge(&ui, "HomePage"));
    assert!(right_edge(&ui, "HomePage") <= left_edge(&ui, "RightColumn"));
    assert!((right_edge(&ui, "RightColumn") - (window_width(&ui) - padding)).abs() < 1.0);
}

#[test]
fn a_narrow_window_moves_the_side_column_into_a_drawer() {
    let ui = window(1180);
    assert!(rail_button(&ui, "gui-shell-show-list").is_none());
    assert!(
        rail_button(&ui, "gui-shell-show-side").is_some(),
        "the rail offers the column"
    );
    assert!(!drawn(&ui, "RightColumn"), "the column waits closed");
    assert!(right_edge(&ui, "HostList") <= left_edge(&ui, "HomePage"));

    press_rail_button(&ui, "gui-shell-show-side");
    assert!(right_edge(&ui, "RightColumn") <= window_width(&ui));
    assert!(
        left_edge(&ui, "RightColumn") < right_edge(&ui, "HomePage"),
        "the drawer is over the page"
    );

    press_rail_button(&ui, "gui-shell-hide-side");
    assert!(!drawn(&ui, "RightColumn"), "the same button closes it");
}

#[test]
fn the_narrowest_window_moves_the_list_into_a_drawer_too() {
    let ui = window(1000);
    assert!(rail_button(&ui, "gui-shell-show-list").is_some());
    assert!(!drawn(&ui, "HostList"), "the list waits closed");
    assert!(
        left_edge(&ui, "HomePage") < ui.global::<Theme>().get_list_column_width(),
        "the page took the list's width"
    );

    press_rail_button(&ui, "gui-shell-show-list");
    let padding = ui.global::<Theme>().get_shell_padding();
    assert!((left_edge(&ui, "HostList") - padding).abs() < 1.0);
    assert!(right_edge(&ui, "HostList") > left_edge(&ui, "HomePage"));
}

#[test]
fn opening_one_drawer_closes_the_other() {
    let ui = window(1000);
    press_rail_button(&ui, "gui-shell-show-side");
    assert!(right_edge(&ui, "RightColumn") <= window_width(&ui));

    press_rail_button(&ui, "gui-shell-show-list");
    assert!(!drawn(&ui, "RightColumn"), "the side column closed");
    assert!(drawn(&ui, "HostList"));
}

#[test]
fn an_open_drawer_closes_when_the_window_grows_wide_and_stays_closed_after() {
    let ui = window(1000);
    press_rail_button(&ui, "gui-shell-show-list");

    ui.window().set_size(PhysicalSize::new(1480, 900));
    settle(&ui);
    assert!(rail_button(&ui, "gui-shell-show-list").is_none());

    ui.window().set_size(PhysicalSize::new(1000, 900));
    settle(&ui);
    assert!(
        !drawn(&ui, "HostList"),
        "shrinking again does not bring the drawer back"
    );
}

#[test]
fn a_host_page_has_no_side_rail_and_gets_the_whole_width() {
    let ui = window(1000);
    support::show_client(&ui, InstanceRow::default(), "Overview");
    settle(&ui);

    assert!(rail_button(&ui, "gui-shell-show-side").is_none());
    assert!(rail_button(&ui, "gui-shell-show-list").is_some());
    let padding = ui.global::<Theme>().get_shell_padding();
    let workspace = ElementHandle::find_by_element_type_name(&ui, "HostWorkspace")
        .next()
        .expect("the host page");
    assert!(
        (workspace.absolute_position().x + workspace.size().width - (window_width(&ui) - padding))
            .abs()
            < 1.0,
        "the page runs to the window's padding"
    );
}

fn click(ui: &AppWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    settle(ui);
}

fn key(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(ui);
}

#[test]
fn a_press_on_empty_space_inside_a_drawer_leaves_it_open() {
    let ui = window(1000);
    press_rail_button(&ui, "gui-shell-show-list");
    let theme = ui.global::<Theme>();
    let inside = theme.get_shell_padding() + theme.get_list_column_width() + 8.0;
    click(&ui, inside, 400.0);
    assert!(drawn(&ui, "HostList"), "the drawer stayed open");

    click(&ui, 900.0, 400.0);
    assert!(!drawn(&ui, "HostList"), "a press on the page closes it");
}

#[test]
fn escape_closes_an_open_drawer() {
    let ui = window(1000);
    press_rail_button(&ui, "gui-shell-show-list");
    key(&ui, Key::Escape);
    assert!(!drawn(&ui, "HostList"));

    press_rail_button(&ui, "gui-shell-show-side");
    key(&ui, Key::Escape);
    assert!(!drawn(&ui, "RightColumn"));
}

#[test]
fn a_rail_button_names_what_pressing_it_does() {
    let ui = window(1000);
    press_rail_button(&ui, "gui-shell-show-list");
    assert!(rail_button(&ui, "gui-shell-hide-list").is_some());
    assert!(rail_button(&ui, "gui-shell-show-list").is_none());
}

#[test]
fn opening_a_page_from_the_list_closes_its_drawer() {
    let ui = window(1000);
    press_rail_button(&ui, "gui-shell-show-list");
    support::open_settings(&ui, "Accounts");
    settle(&ui);
    assert!(!drawn(&ui, "HostList"));
}
