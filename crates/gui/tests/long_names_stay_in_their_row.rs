//! What a long instance or server name does to the layout around it.
//!
//! A name is user-authored and has no length anybody can rely on, while the
//! columns beside it are fixed. The failure is quiet: a `Rectangle` does not
//! clip, so a caption that does not fit is drawn straight through the border it
//! belongs to instead of being cut off where anyone would notice it in review.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, InstanceRow, ServerRow};
use i_slint_backend_testing::{AccessibleRole, ElementHandle, mock_elapsed_time};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

const LONG_NAME: &str =
    "Garden of Glass (Questbook Edition) 1.16.5 Forge Reproduction for the Crash on World Load";
const LONG_SERVER: &str = "Garden of Glass (Questbook Edition) Development Server";

/// The card's own actions have a fixed column, so a title that asks for more
/// than the rest either pushes them off the card or is drawn over them.
#[test]
fn a_card_title_stays_inside_the_card() {
    let ui = window();
    instances(&ui, &[connectable()]);

    let card = widest(&ui, LONG_NAME).expect("the host has a card");
    let right = card.absolute_position().x + card.size().width;
    let title = card_title(&ui, LONG_NAME);

    assert!(
        title.absolute_position().x + title.size().width <= right,
        "the title reaches {}px on a card that ends at {right}px",
        title.absolute_position().x + title.size().width
    );
}

/// A server's card has the same column and the same limit.
#[test]
fn a_server_card_title_stays_inside_the_card() {
    let ui = window();
    servers(&ui, &[long_named_server()]);

    let card = widest(&ui, LONG_SERVER).expect("the server has a card");
    let right = card.absolute_position().x + card.size().width;
    let title = card_title(&ui, LONG_SERVER);

    assert!(
        title.absolute_position().x + title.size().width <= right,
        "the title reaches {}px on a card that ends at {right}px",
        title.absolute_position().x + title.size().width
    );
}

/// Eliding keeps the layout honest and takes the text away. What is cut has to
/// stay reachable, and hovering the thing that was cut is where a person looks.
#[test]
fn a_shortened_card_title_says_the_whole_name_on_hover() {
    let ui = window();
    instances(&ui, &[connectable()]);
    let before = bearing(&ui, LONG_NAME);

    hover(&card_title(&ui, LONG_NAME));
    mock_elapsed_time(std::time::Duration::from_millis(600));
    settle(&ui);

    assert_eq!(
        bearing(&ui, LONG_NAME),
        before + 1,
        "hovering a cut title has to put the whole name on screen"
    );
}

/// What a server card cut from the server's name stays reachable on hover.
#[test]
fn a_shortened_server_card_title_says_the_whole_name_on_hover() {
    let ui = window();
    servers(&ui, &[long_named_server()]);
    let before = bearing(&ui, LONG_SERVER);

    hover(&card_title(&ui, LONG_SERVER));
    mock_elapsed_time(std::time::Duration::from_millis(600));
    settle(&ui);

    assert_eq!(
        bearing(&ui, LONG_SERVER),
        before + 1,
        "hovering a cut title has to put the whole name on screen"
    );
}

/// The title owns a touch area so that it can be hovered, which puts it over
/// the card's own. A press that lands there still has to reach the card.
#[test]
fn a_press_on_the_title_still_opens_the_card() {
    let ui = window();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&opened);
    ui.on_open_workspace(move |kind, id, surface| {
        sink.borrow_mut().push(format!("{kind}/{id}/{surface}"));
    });
    instances(&ui, &[connectable()]);

    card_title(&ui, LONG_NAME).mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_eq!(
        opened.borrow().as_slice(),
        ["client/repro-client/Overview".to_owned()],
        "the strip of card under its own title has to keep opening it"
    );
}

/// How many things on screen carry this text. A note that opens is one more.
fn bearing(ui: &AppWindow, text: &str) -> usize {
    ElementHandle::find_by_accessible_label(ui, text)
        .filter(|element| element.size().width > 0.0)
        .count()
}

/// Moves the pointer onto an element, which is what shows its note.
fn hover(element: &ElementHandle) {
    element.mock_single_click(PointerEventButton::Left);
}

/// The card's own copy of the name, as opposed to the card itself.
fn card_title(ui: &AppWindow, name: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, name)
        .filter(|element| element.accessible_role() != Some(AccessibleRole::Button))
        .find(|element| element.size().width > 0.0)
        .expect("the card shows the name it is labeled with")
}

fn servers(ui: &AppWindow, rows: &[ServerRow]) {
    ui.set_loading(false);
    ui.set_listed_servers(ModelRc::new(VecModel::from(rows.to_vec())));
    settle(ui);
}

fn long_named_server() -> ServerRow {
    ServerRow {
        snapshot_deletion_note: SharedString::default(),
        id: "repro-server".into(),
        name: LONG_SERVER.into(),
        minecraft: "1.16.5".into(),
        loader: "Forge 36.2.0".into(),
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
        path: "/tmp/ephervex/servers/repro-server".into(),
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

fn instances(ui: &AppWindow, rows: &[InstanceRow]) {
    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(rows.to_vec())));
    settle(ui);
}

fn connectable() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-client".into(),
        name: LONG_NAME.into(),
        minecraft: "1.16.5".into(),
        loader: "Forge 36.2.0".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Imported from a modpack".into(),
        path: "/tmp/ephervex/instances/repro-client".into(),
        updated: "Just now".into(),
        linked_servers: 1,
        stoppable_linked_servers: 0,
        unstoppable_linked_servers: 0,
        mods: 84,
        needs_attention: false,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: "repro-server".into(),
        connect_server_name: LONG_SERVER.into(),
        connect_reason: SharedString::default(),
        can_connect: true,
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
    support::test_window()
}
