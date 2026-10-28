//! The column of cards beside a host page: the servers linked to an instance
//! and where the host keeps its files.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, BundlePartRow, HostView, InstanceRow, ServerRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
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
    // Tall enough for every card, since one below the fold is not drawn.
    ui.window().set_size(PhysicalSize::new(width, 2400));
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
fn the_bundle_card_lists_the_files_and_saves_the_chosen_ones() {
    let mut view = client_with_server();
    view.bundle_parts = ModelRc::new(VecModel::from(vec![
        BundlePartRow {
            name: "logs/latest.log".into(),
            kind: "Game log".into(),
            size: "812 KiB".into(),
            selected: true,
            ..BundlePartRow::default()
        },
        BundlePartRow {
            name: "logs/debug.log".into(),
            kind: "Game log".into(),
            size: "1.4 MiB".into(),
            selected: false,
            ..BundlePartRow::default()
        },
    ]));
    view.bundle_selected_count = 1;
    view.bundle_selection = "1 file, 812 KiB".into();
    let ui = window(1600, view);
    assert!(drawn(&ui, "BundleCard"));
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&toggled);
    ui.on_toggle_bundle_part(move |kind, id, name, chosen| {
        sink.borrow_mut()
            .push((kind.to_string(), id.to_string(), name.to_string(), chosen));
    });
    let exported = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&exported);
    ui.on_export_bundle(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));

    press(&ui, "logs/debug.log");
    press(&ui, &lookup("gui-bundle-card-save"));

    assert_eq!(
        toggled.borrow().as_slice(),
        [(
            "client".to_owned(),
            "client".to_owned(),
            "logs/debug.log".to_owned(),
            true
        )]
    );
    assert_eq!(
        exported.borrow().as_slice(),
        [("client".to_owned(), "client".to_owned())]
    );
}

#[test]
fn the_bundle_offer_is_asked_for_when_the_card_appears_and_when_the_host_changes() {
    let ui = support::test_window();
    ui.window().set_size(PhysicalSize::new(1600, 2400));
    let asked = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    ui.on_plan_host_bundle(move |kind, id| {
        sink.borrow_mut().push((kind.to_string(), id.to_string()));
    });
    support::show(
        &ui,
        vec![client_with_server()],
        vec![support::pane(1, 0, "Overview")],
    );
    settle(&ui);
    assert_eq!(
        asked.borrow().last(),
        Some(&("client".to_owned(), "client".to_owned()))
    );

    support::show(
        &ui,
        vec![support::server_view(linked_server())],
        vec![support::pane(1, 0, "Overview")],
    );
    settle(&ui);
    assert_eq!(
        asked.borrow().last(),
        Some(&("server".to_owned(), "server".to_owned()))
    );
}

#[test]
fn the_run_card_starts_and_stops_the_host_the_page_shows() {
    let mut view = client_with_server();
    view.client.can_launch = true;
    let ui = window(1600, view);
    let launched = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&launched);
    ui.on_launch_client(move |id| sink.borrow_mut().push(id.to_string()));
    press(&ui, &lookup("gui-launch"));
    assert_eq!(launched.borrow().as_slice(), ["client"]);
}

#[test]
fn the_run_card_waits_while_the_host_restarts() {
    let mut view = client_with_server();
    view.client.can_kill = true;
    view.client.can_restart = true;
    view.client.restart_stage = "stopping".into();
    let ui = window(1600, view);
    let stop = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-stop-game"))
        .find(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .expect("the Stop button");
    assert_eq!(stop.accessible_enabled(), Some(false));
    let restart = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-restart"))
        .find(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .expect("the Restart button");
    assert_eq!(restart.accessible_enabled(), Some(false));
}

#[test]
fn a_server_page_offers_no_keep_it_on_its_storage_card() {
    let mut server = linked_server();
    server.storage_kind = "temporary".into();
    let ui = window(1600, support::server_view(server));
    assert!(drawn(&ui, "HostStorageCard"));
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-storage-keep"))
            .all(|element| element.size().width == 0.0)
    );
}

#[test]
fn the_header_leaves_the_run_actions_to_the_card_while_the_column_is_beside_the_page() {
    let ui = window(1600, client_with_server());
    let launches = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch"))
        .filter(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .count();
    assert_eq!(launches, 1, "one Launch button, in the Run card");

    let narrow = window(1300, client_with_server());
    let launches = ElementHandle::find_by_accessible_label(&narrow, &lookup("gui-launch"))
        .filter(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .count();
    assert_eq!(launches, 1, "one Launch button, in the header");
}

fn text_shown(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

#[test]
fn an_overview_beside_the_column_leaves_the_linked_servers_to_it() {
    let ui = window(1300, client_with_server());
    assert!(!text_shown(&ui, &lookup("gui-linked-server")));
    assert!(text_shown(&ui, &lookup("gui-folder")));
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
