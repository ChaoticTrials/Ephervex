//! A card's context menu stays with the row it was opened for.
//!
//! Instance and server cards are repeater children, so a list that changes in
//! place hands an existing card another row while its menu is still open. The
//! menu must not go on to act on that other row.

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ephervex_gui::ui::{AppWindow, InstanceRow, MenuRow, ServerRow};
use ephervex_i18n::lookup;
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, ModelRc, SharedString, VecModel};

#[test]
fn an_open_instance_menu_never_acts_on_the_row_that_replaced_its_row() {
    let ui = window();
    ui.set_loading(false);
    ui.set_workspace_open(false);
    let rows = Rc::new(VecModel::from(vec![
        instance("alpha", "Alpha"),
        instance("bravo", "Bravo"),
    ]));
    ui.set_instances(ModelRc::from(Rc::clone(&rows)));
    settle(&ui);

    open_menu(&ui, "Alpha");
    assert!(
        find(&ui, &lookup("gui-delete-instance")).is_some(),
        "the menu opened on the Alpha card"
    );

    swap_rows(&rows);
    settle(&ui);

    activate_last_entry_if_open(&ui, &lookup("gui-delete-instance"));
    assert_ne!(
        ui.get_pending_action_id(),
        "bravo",
        "the menu opened on Alpha must not delete Bravo"
    );
}

/// Guards the two tests around it: driving the menu from the keyboard does
/// reach the delete entry, so a closed menu there is the card's doing.
#[test]
fn the_delete_entry_of_an_untouched_menu_acts_on_its_own_row() {
    let ui = window();
    ui.set_loading(false);
    ui.set_workspace_open(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![
        instance("alpha", "Alpha"),
        instance("bravo", "Bravo"),
    ])));
    settle(&ui);

    open_menu(&ui, "Alpha");
    activate_last_entry_if_open(&ui, &lookup("gui-delete-instance"));

    assert_eq!(ui.get_pending_action_id(), "alpha");
}

#[test]
fn an_open_server_menu_never_acts_on_the_row_that_replaced_its_row() {
    let ui = window();
    ui.set_loading(false);
    let rows = Rc::new(VecModel::from(vec![
        server("alpha", "Alpha"),
        server("bravo", "Bravo"),
    ]));
    ui.set_listed_servers(ModelRc::from(Rc::clone(&rows)));
    settle(&ui);

    open_menu(&ui, "Alpha");
    assert!(
        find(&ui, &lookup("gui-delete-server")).is_some(),
        "the menu opened on the Alpha card"
    );

    swap_rows(&rows);
    settle(&ui);

    activate_last_entry_if_open(&ui, &lookup("gui-delete-server"));
    assert_ne!(
        ui.get_pending_action_id(),
        "bravo",
        "the menu opened on Alpha must not delete Bravo"
    );
}

/// The group list is a menu of its own, reached from the card's menu, and it
/// outlives the menu it was reached from.
#[test]
fn an_open_group_list_never_acts_on_the_row_that_replaced_its_row() {
    let ui = window();
    let groups: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&groups);
    ui.on_set_client_group(move |id, group| {
        record
            .borrow_mut()
            .push((id.to_string(), group.to_string()));
    });
    ui.set_instance_group_entries(ModelRc::new(VecModel::from(vec![MenuRow {
        id: "g-beta".into(),
        label: "Beta".into(),
        ..MenuRow::default()
    }])));
    ui.set_loading(false);
    ui.set_workspace_open(false);
    let rows = Rc::new(VecModel::from(vec![
        instance("alpha", "Alpha"),
        instance("bravo", "Bravo"),
    ]));
    ui.set_instances(ModelRc::from(Rc::clone(&rows)));
    settle(&ui);

    let card = find_card(&ui, "Alpha");
    let origin = LogicalPosition::new(
        card.absolute_position().x + 160.0,
        card.absolute_position().y + card.size().height - 18.0,
    );
    pointer(&ui, origin, PointerEventButton::Right);
    let set_group = find_entry(&ui, &lookup("gui-set-group")).expect("the menu leads to groups");
    pointer(&ui, center_in(origin, &set_group), PointerEventButton::Left);
    assert!(
        find_entry(&ui, "Beta").is_some(),
        "the group list opened on the Alpha card"
    );

    swap_rows(&rows);
    settle(&ui);

    if let Some(beta) = find_entry(&ui, "Beta") {
        pointer(&ui, center_in(origin, &beta), PointerEventButton::Left);
    }
    assert!(
        groups.borrow().iter().all(|(id, _)| id != "bravo"),
        "the group list opened on Alpha must not regroup Bravo"
    );
}

/// Exchanges the first two rows in place, which keeps the repeater's cards and
/// gives each the other's item.
fn swap_rows<T: Clone + 'static>(rows: &VecModel<T>) {
    let (first, second) = (
        rows.row_data(0).expect("first row"),
        rows.row_data(1).expect("second row"),
    );
    rows.set_row_data(0, second);
    rows.set_row_data(1, first);
}

fn find(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).next()
}

fn open_menu(ui: &AppWindow, card_label: &str) {
    find(ui, card_label)
        .expect("the list holds the card")
        .mock_single_click(PointerEventButton::Right);
    settle(ui);
}

/// Activates the menu's last entry, which is the delete entry on both cards,
/// when the menu is still showing. A menu that closed with its row offers
/// nothing to activate, which is the other acceptable outcome.
///
/// Driven from the keyboard: Up from no highlight lands on the last entry.
fn activate_last_entry_if_open(ui: &AppWindow, label: &str) {
    if find(ui, label).is_none() {
        return;
    }
    for key in [Key::UpArrow, Key::Return] {
        let text: SharedString = key.into();
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
        settle(ui);
    }
}

fn find_card(ui: &AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, name)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("the list holds the card")
}

/// The row of an open themed menu carrying `label`.
fn find_entry(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|element| {
        element.accessible_role() == Some(AccessibleRole::Button) && element.size().width > 0.0
    })
}

/// The middle of a row of a menu opened at `origin`. A popup's elements report
/// their positions from the popup's own corner.
fn center_in(origin: LogicalPosition, element: &ElementHandle) -> LogicalPosition {
    LogicalPosition::new(
        origin.x + element.absolute_position().x + element.size().width / 2.0,
        origin.y + element.absolute_position().y + element.size().height / 2.0,
    )
}

fn pointer(ui: &AppWindow, position: LogicalPosition, button: PointerEventButton) {
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    ui.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
    settle(ui);
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window() -> AppWindow {
    support::test_window()
}

fn instance(id: &str, name: &str) -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: id.into(),
        name: name.into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/x".into(),
        updated: "Just now".into(),
        linked_servers: 0,
        stoppable_linked_servers: 0,
        unstoppable_linked_servers: 0,
        mods: 0,
        needs_attention: false,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: SharedString::default(),
        connect_server_name: SharedString::default(),
        connect_reason: SharedString::default(),
        can_connect: false,
        creating_server_detail: SharedString::default(),
        creating_server_progress: 0.0,
        creating_server_indeterminate: false,
        restart_stage: SharedString::default(),
        can_restart: false,
        paired_server_id: SharedString::default(),
        paired_server_name: SharedString::default(),
        pair_start_together: false,
        pair_starting: false,
        favorite: false,
        group_id: slint::SharedString::default(),
        group_name: slint::SharedString::default(),
        group_index: 0,
        group_tone: 0,
        icon_slot: -1,
        problems: 0,
        state_line: "".into(),
        last_run: "".into(),
        open: false,
        ..InstanceRow::default()
    }
}

fn server(id: &str, name: &str) -> ServerRow {
    ServerRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: id.into(),
        name: name.into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        linked_client_id: SharedString::default(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        state_detail: "Not running".into(),
        address: "127.0.0.1:25565".into(),
        restart_required: false,
        path: "/tmp/ephervex/servers/x".into(),
        installed: true,
        can_start: true,
        can_stop: false,
        can_restart: false,
        can_kill: false,
        can_send_command: false,
        console_status: SharedString::default(),
        console_completion: 0,
        console_delivered: false,
        can_connect: false,
        connect_reason: SharedString::default(),
        restart_stage: SharedString::default(),
        needs_attention: false,
        paired_client_id: SharedString::default(),
        paired_client_name: SharedString::default(),
        pair_start_together: false,
        pair_starting: false,
        favorite: false,
        icon_slot: -1,
        problems: 0,
        state_line: "".into(),
        last_run: "".into(),
        open: false,
    }
}
