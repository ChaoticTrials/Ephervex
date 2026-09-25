//! Turning a temporary instance into a permanent one, from the desktop app.
//!
//! The decision has two halves that are easy to get wrong independently: the
//! action must only be offered where it can actually work, and the linked
//! servers it would take along must each be answerable on their own rather than
//! as one yes or no (ADR 0025).

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, PromoteServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    ElementHandle, TestingBackend, TestingBackendOptions, mock_elapsed_time,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn a_temporary_instance_offers_to_become_persistent() {
    let ui = window();
    let asked = record_requests(&ui);
    client_workspace_with(&ui, temporary_instance());

    widest(&ui, &lookup("gui-make-persistent"))
        .expect("a temporary instance offers to be kept")
        .invoke_accessible_default_action();

    assert_eq!(asked.borrow().as_slice(), ["repro-client".to_owned()]);
}

/// An instance that is already persistent has nowhere to go, and the core
/// refuses it. Offering the action anyway would be offering a guaranteed error.
#[test]
fn a_persistent_instance_is_not_offered_the_action() {
    let ui = window();
    client_workspace_with(
        &ui,
        InstanceRow {
            storage: "Persistent".into(),
            storage_kind: "persistent".into(),
            ..temporary_instance()
        },
    );

    assert!(
        widest(&ui, &lookup("gui-make-persistent")).is_none(),
        "there is nothing to promote, so there is nothing to offer"
    );
}

/// Moving an instance out from under a running game is refused by the core, so
/// the button says so before the click rather than after it.
#[test]
fn a_running_instance_cannot_be_moved_out_from_under_itself() {
    let ui = window();
    let asked = record_requests(&ui);
    client_workspace_with(
        &ui,
        InstanceRow {
            can_kill: true,
            can_launch: false,
            state: "Running".into(),
            state_kind: "running".into(),
            ..temporary_instance()
        },
    );

    widest(&ui, &lookup("gui-make-persistent"))
        .expect("the button stays visible, so its state stays readable")
        .invoke_accessible_default_action();

    assert!(
        asked.borrow().is_empty(),
        "a press while the game is running must not reach the backend"
    );
}

/// A disabled control that says nothing is a question the user cannot answer.
/// The reason is always known here and is not guessable from the screen.
#[test]
fn a_running_instance_says_why_it_cannot_be_made_persistent() {
    let ui = window();
    client_workspace_with(
        &ui,
        InstanceRow {
            can_kill: true,
            can_launch: false,
            state: "Running".into(),
            state_kind: "running".into(),
            ..temporary_instance()
        },
    );

    let button = widest(&ui, &lookup("gui-make-persistent")).expect("the button is there");
    assert_eq!(
        button.accessible_enabled(),
        Some(false),
        "it has to read as unavailable, not merely look it"
    );
    assert!(
        !reason_is_on_screen(&ui),
        "the explanation belongs to the hover, not to the resting screen"
    );

    // The pointer is moved onto the button before the press, which is what
    // puts it under the cursor. The press itself is refused; the explanation
    // is what the user gets instead, once the built-in tooltip's hover delay
    // has passed.
    button.mock_single_click(PointerEventButton::Left);
    mock_elapsed_time(std::time::Duration::from_millis(600));
    settle(&ui);

    assert!(
        reason_is_on_screen(&ui),
        "a disabled button that says nothing is a question the user cannot answer"
    );
}

/// Whether the reason the button cannot be pressed is currently drawn.
fn reason_is_on_screen(ui: &AppWindow) -> bool {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-make-persistent-while-running"))
        .any(|element| element.size().width > 0.0)
}

/// Every offered server starts on. A persistent instance whose server vanishes
/// on the next reboot is the surprise the dialog exists to prevent.
#[test]
fn every_temporary_linked_server_is_offered_and_starts_selected() {
    let ui = window();
    promotion_dialog(&ui);

    let submitted = record_submissions(&ui);
    submit(&ui);

    assert_eq!(
        submitted.borrow().as_slice(),
        ["repro-client".to_owned()],
        "confirming submits the instance it was opened for"
    );
    assert_eq!(
        selected(&ui),
        vec!["server-one".to_owned(), "server-two".to_owned()],
        "both servers come along unless the user says otherwise"
    );
}

/// The answer is per server, which is the reason this is a list of switches
/// rather than one checkbox for the set.
#[test]
fn a_server_can_be_left_behind_on_its_own() {
    let ui = window();
    promotion_dialog(&ui);

    ElementHandle::find_by_accessible_label(&ui, "Repro Server One")
        .find(|element| element.size().width > 0.0)
        .expect("each server has its own switch")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        selected(&ui),
        vec!["server-two".to_owned()],
        "unticking one server must not change the answer for the other"
    );
}

/// The dialog as the backend's plan opens it.
fn promotion_dialog(ui: &AppWindow) {
    client_workspace_with(ui, temporary_instance());
    ui.set_promote_instance_name("Repro Client".into());
    ui.set_promote_destination("/home/user/.local/share/ephervex/instances/repro-client".into());
    ui.set_promote_servers(ModelRc::new(VecModel::from(vec![
        promotable("server-one", "Repro Server One"),
        promotable("server-two", "Repro Server Two"),
    ])));
    ui.set_pending_action_id("repro-client".into());
    ui.set_modal("make-persistent".into());
    settle(ui);
}

fn promotable(id: &str, display: &str) -> PromoteServerRow {
    PromoteServerRow {
        id: id.into(),
        display: display.into(),
        selected: true,
    }
}

/// The ids the dialog would submit right now.
fn selected(ui: &AppWindow) -> Vec<String> {
    ui.get_promote_servers()
        .iter()
        .filter(|server| server.selected)
        .map(|server| server.id.to_string())
        .collect()
}

fn submit(ui: &AppWindow) {
    widest(ui, &lookup("gui-make-persistent-confirm"))
        .expect("the dialog offers a way to go ahead")
        .invoke_accessible_default_action();
}

fn record_requests(ui: &AppWindow) -> Rc<RefCell<Vec<String>>> {
    let asked: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&asked);
    ui.on_open_make_persistent(move |id| sink.borrow_mut().push(id.to_string()));
    asked
}

fn record_submissions(ui: &AppWindow) -> Rc<RefCell<Vec<String>>> {
    let asked: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&asked);
    ui.on_submit_make_persistent(move |id| sink.borrow_mut().push(id.to_string()));
    asked
}

fn client_workspace_with(ui: &AppWindow, row: InstanceRow) {
    support::show_client(ui, row, "Overview");
    support::follow_surface_requests(ui);
    settle(ui);
}

fn temporary_instance() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/repro-client".into(),
        updated: "Just now".into(),
        linked_servers: 2,
        mods: 0,
        needs_attention: false,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: SharedString::default(),
        connect_server_name: SharedString::default(),
        can_connect: false,
        creating_server_detail: SharedString::default(),
        creating_server_progress: 0.0,
        creating_server_indeterminate: false,
        restart_stage: SharedString::default(),
        can_restart: false,
    }
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
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.to_string())),
                ("second", Arg::from(second.to_string())),
            ],
        )
        .into()
    });
}
