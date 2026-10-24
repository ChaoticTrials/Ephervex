//! The column of cards beside a host page: the servers linked to an instance
//! and where the host keeps its files.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, HostView, InstanceRow, ServerRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn drawn(ui: &AppWindow, type_name: &str) -> bool {
    ElementHandle::find_by_element_type_name(ui, type_name)
        .next()
        .is_some()
}

fn linked_server() -> ServerRow {
    ServerRow {
        id: "server".into(),
        name: "conveyor-srv".into(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        address: "127.0.0.1:25565".into(),
        can_start: true,
        ..ServerRow::default()
    }
}

fn client_with_server() -> HostView {
    let mut view = support::client_view(InstanceRow {
        id: "client".into(),
        name: "Conveyor dupe #412".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        path: "/tmp/ephervex/instances/client".into(),
        paired_server_id: "server".into(),
        ..InstanceRow::default()
    });
    view.linked_servers = ModelRc::new(VecModel::from(vec![linked_server()]));
    view
}

fn window(width: u32, host: HostView) -> AppWindow {
    let ui = support::test_window();
    ui.window().set_size(PhysicalSize::new(width, 900));
    support::show(&ui, vec![host], vec![support::pane(1, 0, "Overview")]);
    settle(&ui);
    ui
}

fn press(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.size().width > 0.0)
        .unwrap_or_else(|| panic!("a control labeled {label}"))
        .invoke_accessible_default_action();
    settle(ui);
}

#[test]
fn a_wide_window_shows_the_cards_beside_an_instance_page() {
    let ui = window(1600, client_with_server());
    assert!(drawn(&ui, "LinkedServersCard"));
    assert!(drawn(&ui, "HostStorageCard"));

    let padding = ui.global::<Theme>().get_shell_padding();
    let card = ElementHandle::find_by_element_type_name(&ui, "LinkedServersCard")
        .next()
        .expect("the card");
    let right = card.absolute_position().x + card.size().width;
    #[allow(clippy::cast_precision_loss)]
    let edge = ui.window().size().width as f32 - padding;
    assert!(right <= edge + 0.5, "the card ends at {right}, edge {edge}");
}

#[test]
fn a_narrower_window_keeps_the_cards_in_a_drawer_opened_from_the_header() {
    let ui = window(1300, client_with_server());
    assert!(!drawn(&ui, "LinkedServersCard"));

    press(&ui, &lookup("gui-workspace-show-column"));
    assert!(drawn(&ui, "LinkedServersCard"));
    assert!(drawn(&ui, "HostStorageCard"));
}

#[test]
fn a_wide_window_has_no_header_button_for_the_cards() {
    let ui = window(1600, client_with_server());
    let button = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-workspace-show-column"))
        .find(|element| element.size().width > 0.0);
    assert!(button.is_none());
}

#[test]
fn a_split_workspace_has_no_column() {
    let ui = support::test_window();
    ui.window().set_size(PhysicalSize::new(1600, 900));
    support::show(
        &ui,
        vec![client_with_server(), support::server_view(linked_server())],
        vec![
            support::pane(1, 0, "Overview"),
            support::pane(2, 1, "Overview"),
        ],
    );
    settle(&ui);
    assert!(!drawn(&ui, "LinkedServersCard"));
    assert!(!drawn(&ui, "HostStorageCard"));
}

#[test]
fn a_server_page_has_storage_and_no_linked_servers() {
    let ui = window(1600, support::server_view(linked_server()));
    assert!(drawn(&ui, "HostStorageCard"));
    assert!(!drawn(&ui, "LinkedServersCard"));
}

#[test]
fn the_start_button_on_a_linked_server_starts_it() {
    let ui = window(1600, client_with_server());
    let started = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&started);
    ui.on_server_start(move |id| sink.borrow_mut().push(id.to_string()));
    press(
        &ui,
        &lookup_args(
            "gui-linked-server-start",
            [("value", Arg::from("conveyor-srv"))],
        ),
    );
    assert_eq!(started.borrow().as_slice(), ["server"]);
}

#[test]
fn a_client_without_servers_offers_to_create_one() {
    let mut view = client_with_server();
    view.linked_servers = ModelRc::new(VecModel::<ServerRow>::default());
    let ui = window(1600, view);
    press(&ui, &lookup("gui-create-linked-server"));
    assert_eq!(ui.get_modal(), "create-server");
    assert_eq!(ui.get_create_server_client_id(), "client");
}

#[test]
fn the_drawer_closes_when_the_page_shows_another_host() {
    let ui = window(1300, client_with_server());
    press(&ui, &lookup("gui-workspace-show-column"));
    assert!(drawn(&ui, "HostStorageCard"));

    support::show(
        &ui,
        vec![support::server_view(linked_server())],
        vec![support::pane(1, 0, "Overview")],
    );
    settle(&ui);
    assert!(!drawn(&ui, "HostStorageCard"));
}

#[test]
fn a_server_that_cannot_be_joined_says_why_on_its_connect_button() {
    let mut view = client_with_server();
    let mut server = linked_server();
    server.connect_reason = "Start the server first.".into();
    view.linked_servers = ModelRc::new(VecModel::from(vec![server]));
    let ui = window(1600, view);
    let button = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch-client-connect"))
        .find(|element| element.size().width > 0.0)
        .expect("the connect button");
    assert_eq!(button.accessible_enabled(), Some(false));
    assert_eq!(
        button.accessible_description().as_deref(),
        Some("Start the server first.")
    );
}
