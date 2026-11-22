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
        undoable: false,
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

#[test]
fn only_a_notification_that_offers_undo_shows_the_button_and_it_names_that_notification() {
    let ui = window_with(vec![
        toast(1, "Plain", false),
        ToastRow {
            undoable: true,
            ..toast(2, "Turned off one mod that required it as well.", false)
        },
    ]);
    let undone: std::rc::Rc<std::cell::RefCell<Vec<i32>>> = std::rc::Rc::default();
    let sink = std::rc::Rc::clone(&undone);
    ui.on_undo_toast(move |id| sink.borrow_mut().push(id));

    let buttons: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-undo"))
            .filter(|element| {
                element.size().width > 0.0
                    && element.accessible_role()
                        == Some(i_slint_backend_testing::AccessibleRole::Button)
            })
            .collect();
    assert_eq!(
        buttons.len(),
        1,
        "one Undo, on the notification that offers it"
    );
    buttons[0].invoke_accessible_default_action();

    assert_eq!(undone.borrow().as_slice(), [2]);
}
