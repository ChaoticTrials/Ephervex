//! What a pointer event on a list card actually reaches.
//!
//! A card carries both its own press area and the star that acts on it. Which
//! one a click lands on is decided by the markup's sibling order and by the
//! geometry Slint gives an element that sets a size without a position, and
//! neither is visible from reading the callbacks alone. So the window is built
//! for real and clicked for real.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

/// One window per test thread. Both card kinds are exercised in the first.
#[test]
fn a_card_star_is_clickable_across_its_whole_face_and_opens_nothing() {
    let ui = window();

    let favorites: Rc<RefCell<Vec<String>>> = Rc::default();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();

    let record = Rc::clone(&favorites);
    ui.on_set_favorite(move |kind, id, favorite| {
        record.borrow_mut().push(format!("{kind}/{id}/{favorite}"));
    });
    let record = Rc::clone(&opened);
    ui.on_open_workspace(move |kind, id, surface| {
        record.borrow_mut().push(format!("{kind}/{id}/{surface}"));
    });

    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_listed_servers(ModelRc::new(VecModel::from(vec![server()])));

    settle(&ui);
    star("Repro Client", &ui).mock_single_click(PointerEventButton::Left);
    assert_eq!(
        favorites.borrow().as_slice(),
        ["client/repro-client/true"],
        "a click on the middle of the star must favorite"
    );

    // The same arrangement on a server card, which shares the card shape.
    star("Repro Server", &ui).mock_single_click(PointerEventButton::Left);
    assert_eq!(
        favorites.borrow().as_slice(),
        ["client/repro-client/true", "server/repro-server/true"]
    );
    assert!(
        opened.borrow().is_empty(),
        "the card's own press area must not sit over its star"
    );
}

/// A single click anywhere else on a card opens its host.
///
/// The press has to reach the card's own area: a list model replaced between
/// the press and the release rebuilds every card, and the release then lands
/// on an element that never saw the press, which reads as no click at all.
#[test]
fn one_click_on_a_card_opens_its_host_on_the_overview() {
    let ui = window();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_workspace(move |kind, id, surface| {
        record.borrow_mut().push(format!("{kind}/{id}/{surface}"));
    });

    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_listed_servers(ModelRc::new(VecModel::from(vec![server()])));
    settle(&ui);

    card("Repro Client", &ui).mock_single_click(PointerEventButton::Left);
    card("Repro Server", &ui).mock_single_click(PointerEventButton::Left);

    assert_eq!(
        opened.borrow().as_slice(),
        [
            "client/repro-client/Overview",
            "server/repro-server/Overview"
        ]
    );
}

/// Opening a host from the list also leaves the settings, so the host is what
/// the middle column shows.
#[test]
fn opening_a_host_from_the_list_closes_the_settings() {
    let ui = window();
    ui.on_open_workspace(|_, _, _| {});
    ui.set_loading(false);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    support::open_settings(&ui, "General");
    settle(&ui);
    assert!(ui.get_settings_open());

    card("Repro Client", &ui).mock_single_click(PointerEventButton::Left);

    assert!(!ui.get_settings_open());
}

/// Renders once, which is what instantiates the cards a repeater holds and
/// gives every element the geometry a pointer event is resolved against.
fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

/// The card of the host called `name`.
fn card(name: &str, ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, name)
        .find(|element| element.size().width > 0.0)
        .expect("the list holds the card")
}

/// The favorite star on the card of the host called `name`.
fn star(name: &str, ui: &AppWindow) -> ElementHandle {
    let label = lookup_args("gui-favorite-host", [("value", Arg::from(name.to_owned()))]);
    ElementHandle::find_by_accessible_label(ui, &label)
        .find(|element| element.size().width > 0.0)
        .expect("the card has a star")
}

/// A laid-out, drawn window on this thread's own testing backend.
///
/// Software-rendered, because the pointer positions a click is resolved against
/// only exist once the window has been laid out and drawn.
fn window() -> AppWindow {
    support::test_window()
}

fn instance() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/repro-client".into(),
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
    }
}

fn server() -> ServerRow {
    ServerRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-server".into(),
        name: "Repro Server".into(),
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
