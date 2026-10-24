//! Deleting an instance from the desktop app while its linked servers run.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, PhysicalSize};

/// Confirming a dialog that said the servers are stopped is the consent to stop them.
#[test]
fn a_running_linked_server_is_stopped_by_the_confirmed_deletion() {
    let ui = window();
    let requests = record_deletions(&ui);
    deletion_dialog(&ui, 1, 0);

    assert!(
        widest(&ui, &lookup("gui-delete-files")).is_none(),
        "the plain deletion is refused while the server runs"
    );
    widest(&ui, &lookup("gui-stop-and-delete-files"))
        .expect("the dialog offers to stop the server")
        .invoke_accessible_default_action();

    assert_eq!(
        requests.borrow().as_slice(),
        [("repro-client".to_owned(), true, true)]
    );
}

#[test]
fn stopped_linked_servers_are_deleted_without_a_stop() {
    let ui = window();
    let requests = record_deletions(&ui);
    deletion_dialog(&ui, 0, 0);

    assert!(widest(&ui, &lookup("gui-stop-and-keep-files")).is_none());
    widest(&ui, &lookup("gui-remove-from-ephervex"))
        .expect("the dialog offers to keep files")
        .invoke_accessible_default_action();

    assert_eq!(
        requests.borrow().as_slice(),
        [("repro-client".to_owned(), false, false)]
    );
}

/// The dialog warns before a deletion the core will refuse, and leaves the
/// decision to the core, which checks the live state.
#[test]
fn a_linked_server_ephervex_cannot_stop_is_named_before_the_deletion() {
    let ui = window();
    let requests = record_deletions(&ui);
    deletion_dialog(&ui, 0, 1);

    let reason = lookup_args(
        "gui-delete-instance-unstoppable-servers",
        [("count", Arg::Count(1))],
    );
    assert!(widest(&ui, &reason).is_some(), "the dialog says why");
    widest(&ui, &lookup("gui-delete-files"))
        .expect("the plain deletion is offered")
        .invoke_accessible_default_action();

    assert_eq!(
        requests.borrow().as_slice(),
        [("repro-client".to_owned(), true, false)]
    );
}

/// The core refuses before stopping anything, so the dialog does not offer a stop.
#[test]
fn a_server_ephervex_cannot_stop_keeps_the_others_running() {
    let ui = window();
    let requests = record_deletions(&ui);
    deletion_dialog(&ui, 1, 1);

    assert!(widest(&ui, &lookup("gui-stop-and-delete-files")).is_none());
    widest(&ui, &lookup("gui-delete-files"))
        .expect("the plain deletion is offered")
        .invoke_accessible_default_action();

    assert_eq!(
        requests.borrow().as_slice(),
        [("repro-client".to_owned(), true, false)]
    );
}

fn deletion_dialog(ui: &AppWindow, stoppable_linked_servers: i32, unstoppable_linked_servers: i32) {
    ui.set_pending_action_id("repro-client".into());
    ui.set_pending_action_name("Repro Client".into());
    ui.set_pending_action_linked_servers(1);
    ui.set_pending_action_stoppable_linked_servers(stoppable_linked_servers);
    ui.set_pending_action_unstoppable_linked_servers(unstoppable_linked_servers);
    ui.set_modal("delete-instance".into());
    settle(ui);
}

type Deletion = (String, bool, bool);

fn record_deletions(ui: &AppWindow) -> Rc<RefCell<Vec<Deletion>>> {
    let requests: Rc<RefCell<Vec<Deletion>>> = Rc::default();
    let sink = Rc::clone(&requests);
    ui.on_delete_client(move |id, files, stop| {
        sink.borrow_mut().push((id.to_string(), files, stop));
    });
    requests
}

fn widest(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window() -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}

fn install_i18n(ui: &AppWindow) {
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::Count(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.to_string()))]).into()
    });
}

#[test]
fn both_outcomes_are_offered_as_cards_with_their_consequences() {
    let ui = window();
    deletion_dialog(&ui, 0, 0);

    for key in [
        "gui-delete-keep-title",
        "gui-delete-keep-detail",
        "gui-delete-files-title",
        "gui-delete-files-detail",
    ] {
        assert!(widest(&ui, &lookup(key)).is_some(), "{key}");
    }
}

#[test]
fn the_cancel_button_stays_inside_the_dialog_whatever_notes_it_carries() {
    let ui = window();
    for (stoppable, unstoppable) in [(0, 0), (1, 0), (0, 1)] {
        deletion_dialog(&ui, stoppable, unstoppable);
        let cancel = widest(&ui, &lookup("gui-cancel")).expect("Cancel");
        let delete = widest(&ui, &lookup("gui-delete-files-title")).expect("the card");
        assert!(
            cancel.absolute_position().y > delete.absolute_position().y,
            "Cancel sits under the cards"
        );
        let window_height = f64::from(ui.window().size().height);
        assert!(
            f64::from(cancel.absolute_position().y + cancel.size().height) < window_height,
            "Cancel is inside the window"
        );
    }
}
