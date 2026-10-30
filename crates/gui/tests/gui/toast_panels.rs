//! Notifications: where they stand and what they carry.

#![allow(clippy::panic)]

use crate::support;

use ephervex_gui::ui::{AppWindow, ToastRow};
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn window_with(toasts: Vec<ToastRow>) -> AppWindow {
    let ui = support::test_window();
    ui.set_loading(false);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.set_toasts(ModelRc::new(VecModel::from(toasts)));
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
    ui
}

fn toast(id: i32, message: &str, error: bool) -> ToastRow {
    ToastRow {
        id,
        message: message.into(),
        error,
        repeats: 1,
    }
}

fn shown(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.size().width > 0.0)
        .expect("the notification is on screen")
}

#[test]
fn a_notification_stands_in_the_middle_of_the_window_above_its_bottom_edge() {
    let ui = window_with(vec![toast(1, "Conveyor dupe is running", false)]);
    let label = format!(
        "{}: Conveyor dupe is running",
        lookup("gui-notification-complete")
    );
    let panel = shown(&ui, &label);

    let window = ui.window().size();
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (window.width as f32, window.height as f32);
    let middle = panel.absolute_position().x + panel.size().width / 2.0;
    assert!(
        (middle - width / 2.0).abs() < 2.0,
        "centered at {middle} of {width}"
    );
    assert!(
        panel.absolute_position().y + panel.size().height <= height,
        "inside the window"
    );
    assert!(
        panel.absolute_position().y > height / 2.0,
        "along the bottom, not the top"
    );
}

#[test]
fn a_failure_is_announced_as_one_and_a_long_message_wraps_inside_the_window() {
    let long =
        "Could not start Minecraft because the Java runtime this instance names was not found. "
            .repeat(4);
    let ui = window_with(vec![toast(1, &long, true)]);
    let label = format!("{}: {long}", lookup("gui-notification-error"));
    let panel = shown(&ui, &label);
    assert!(panel.size().width <= 560.5, "no wider than the limit");
    assert!(panel.size().height > 60.0, "taller than one line");
}

#[test]
fn several_notifications_stack_upward_without_overlapping() {
    let ui = window_with(vec![toast(1, "First", false), toast(2, "Second", false)]);
    let complete = lookup("gui-notification-complete");
    let first = shown(&ui, &format!("{complete}: First"));
    let second = shown(&ui, &format!("{complete}: Second"));
    assert!(
        first.absolute_position().y + first.size().height <= second.absolute_position().y,
        "the older one sits above the newer"
    );
}
