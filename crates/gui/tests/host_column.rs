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

#[test]
fn the_report_card_copies_exactly_the_summary_it_shows() {
    let mut view = client_with_server();
    view.client.summary = "Conveyor dupe\n\nMinecraft: 1.20.1".into();
    let ui = window(1600, view);
    assert!(drawn(&ui, "ReportCard"));
    let copied = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&copied);
    ui.on_copy_text(move |text| sink.borrow_mut().push(text.to_string()));
    press(&ui, &lookup("gui-report-copy"));
    assert_eq!(
        copied.borrow().as_slice(),
        ["Conveyor dupe\n\nMinecraft: 1.20.1"]
    );
}

#[test]
fn a_server_page_has_no_report_card() {
    let ui = window(1600, support::server_view(linked_server()));
    assert!(!drawn(&ui, "ReportCard"));
}

#[test]
fn the_report_card_leads_to_the_diagnostic_bundle() {
    let ui = window(1600, client_with_server());
    let requests = support::follow_surface_requests(&ui);
    press(&ui, &lookup("gui-report-bundle"));
    assert_eq!(
        requests.borrow().last(),
        Some(&(1, "Diagnostics".to_owned()))
    );
}

fn text_shown(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

#[test]
fn an_overview_beside_the_column_leaves_the_linked_servers_to_it() {
    let ui = window(1300, client_with_server());
    assert!(!text_shown(&ui, &lookup("gui-linked-server")));
    assert!(text_shown(&ui, &lookup("gui-reproduction-environment")));
}

#[test]
fn an_overview_in_a_split_keeps_its_linked_servers() {
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
    assert!(text_shown(&ui, &lookup("gui-linked-server")));
}

#[test]
fn the_overview_card_holds_the_text_that_follows_it_apart_from_its_own() {
    let ui = window(1300, client_with_server());
    let below = |label: &str| {
        let element = ElementHandle::find_by_accessible_label(&ui, label)
            .find(|element| element.size().width > 0.0)
            .expect("the text is shown");
        element.absolute_position().y
    };
    let path = ElementHandle::find_by_accessible_label(&ui, "/tmp/ephervex/instances/client")
        .find(|element| element.size().width > 0.0)
        .expect("the card shows the instance's folder");
    let card_bottom = path.absolute_position().y + path.size().height;
    assert!(
        below(&lookup("gui-temporary-lifecycle")) >= card_bottom,
        "the lifecycle note starts at {}, inside the card that ends at {card_bottom}",
        below(&lookup("gui-temporary-lifecycle"))
    );
}
