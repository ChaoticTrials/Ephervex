//! The port a new server is offered: chosen in Settings, and the starting
//! point of the New Server dialog.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::AppWindow;
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, PhysicalSize};

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn general_settings() -> AppWindow {
    let ui = support::test_window();
    support::open_settings(&ui, "General");
    ui.window().set_size(PhysicalSize::new(1500, 2400));
    settle(&ui);
    ui
}

fn press(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.size().width > 0.0)
        .expect("a control with this label")
        .invoke_accessible_default_action();
    settle(ui);
}

fn record(ui: &AppWindow) -> Rc<RefCell<Vec<(bool, String)>>> {
    let calls: Rc<RefCell<Vec<(bool, String)>>> = Rc::default();
    let sink = Rc::clone(&calls);
    ui.on_save_server_port_default(move |custom, port| {
        sink.borrow_mut().push((custom, port.to_string()));
    });
    calls
}

#[test]
fn choosing_a_custom_port_saves_the_choice_with_the_field() {
    let ui = general_settings();
    ui.set_default_server_port("25570".into());
    let calls = record(&ui);

    press(&ui, &lookup("gui-port-custom"));

    assert_eq!(
        calls.borrow().as_slice(),
        [(true, "25570".to_owned())],
        "the choice is saved as soon as it is made"
    );
    assert!(ui.get_default_server_port_custom());
}

#[test]
fn choosing_what_is_already_chosen_saves_nothing() {
    let ui = general_settings();
    let calls = record(&ui);

    press(&ui, &lookup("gui-port-automatic"));

    assert!(calls.borrow().is_empty());
}

#[test]
fn the_new_server_dialog_starts_from_the_stored_default_each_time_it_opens() {
    let ui = support::test_window();
    ui.set_stored_server_port_custom(true);
    ui.set_stored_server_port("25570".into());

    ui.invoke_open_create_server();
    assert!(!ui.get_create_server_automatic_port());
    assert_eq!(ui.get_create_server_port(), "25570");

    // Edited and abandoned, the dialog does not remember the edit.
    ui.set_create_server_automatic_port(true);
    ui.set_create_server_port("30000".into());
    ui.set_modal("".into());
    ui.invoke_open_create_server();
    assert!(!ui.get_create_server_automatic_port());
    assert_eq!(ui.get_create_server_port(), "25570");
}
