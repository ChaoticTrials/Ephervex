//! The Diagnostics surface of client and server workspaces: verification,
//! repair, and the reasons Repair waits.
#![allow(clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(*value))
            .collect::<Vec<_>>(),
    ))
}

fn visible(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

fn find_button(ui: &AppWindow, message: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup(message))
        .filter(|element| element.accessible_enabled().is_some())
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
}

fn button(ui: &AppWindow, message: &str) -> ElementHandle {
    find_button(ui, message).unwrap_or_else(|| panic!("no button labeled {message}"))
}

fn enabled(ui: &AppWindow, message: &str) -> bool {
    button(ui, message)
        .accessible_enabled()
        .expect("the button reports whether it is enabled")
}

fn tab(ui: &AppWindow, message: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup(message))
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("no workspace tab labeled {message}"))
}

const REPAIR_BLOCKERS: [&str; 2] = [
    "gui-integrity-repair-blocked-running",
    "gui-integrity-repair-blocked-isolation",
];

fn blockers_shown(ui: &AppWindow) -> Vec<&'static str> {
    REPAIR_BLOCKERS
        .into_iter()
        .filter(|message| visible(ui, &lookup(message)))
        .collect()
}

/// Opens Diagnostics on the client or the server workspace with a finished
/// verification that found one file a repair can replace.
fn checked_surface(ui: &AppWindow, server_host: bool, running: bool) {
    if server_host {
        server_workspace(ui, server(running));
        ui.set_integrity_host_kind("server".into());
        ui.set_integrity_host_id("server".into());
    } else {
        let mut row = instance();
        row.can_kill = running;
        row.can_launch = !running;
        client_workspace(ui, row);
        ui.set_integrity_host_kind("client".into());
        ui.set_integrity_host_id("host".into());
    }
    ui.set_host_busy_kind(SharedString::default());
    ui.set_host_busy_id(SharedString::default());
    ui.set_active_workflow_id(SharedString::default());
    ui.set_workspace_tab("Diagnostics".into());
    ui.set_integrity_state("checked".into());
    ui.set_integrity_summary(
        lookup_args("gui-integrity-problems", [("count", Arg::from(1))]).into(),
    );
    ui.set_integrity_repair_targets(strings(&["mods/gone.jar"]));
    ui.set_integrity_unrepairable(strings(&[]));
    settle(ui);
}

fn host_kinds() -> [(bool, &'static str, &'static str); 2] {
    [(false, "client", "host"), (true, "server", "server")]
}

#[test]
fn diagnostics_is_a_surface_of_client_and_server_workspaces() {
    let ui = window();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_open_integrity(move |kind, id| {
        sink.borrow_mut().push((kind.to_string(), id.to_string()));
    });
    for (server_host, kind, id) in host_kinds() {
        if server_host {
            server_workspace(&ui, server(false));
        } else {
            client_workspace(&ui, instance());
        }
        ui.set_workspace_tab("Overview".into());
        settle(&ui);

        tab(&ui, "gui-diagnostics-tab").mock_single_click(PointerEventButton::Left);
        settle(&ui);

        assert_eq!(ui.get_workspace_tab(), "Diagnostics");
        assert_eq!(
            opened.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );
        assert!(enabled(&ui, "gui-integrity-verify"), "{kind}");
    }
}

#[test]
fn verifying_calls_the_host_the_surface_is_open_on() {
    let ui = window();
    let verified = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&verified);
    ui.on_verify_host(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));
    for (server_host, kind, id) in host_kinds() {
        checked_surface(&ui, server_host, false);
        button(&ui, "gui-integrity-verify").mock_single_click(PointerEventButton::Left);
        assert_eq!(
            verified.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );
    }
}

#[test]
fn a_repairable_finding_offers_repair_once_nothing_is_in_its_way() {
    let ui = window();
    let repairs = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&repairs);
    ui.on_repair_host(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));
    for (server_host, kind, id) in host_kinds() {
        checked_surface(&ui, server_host, false);
        assert!(enabled(&ui, "gui-integrity-repair"), "{kind}");
        assert!(blockers_shown(&ui).is_empty());
        assert!(
            visible(&ui, "mods/gone.jar"),
            "the preview names what repair touches"
        );
        button(&ui, "gui-integrity-repair").mock_single_click(PointerEventButton::Left);
        assert_eq!(
            repairs.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );
    }
}

#[test]
fn a_running_host_holds_repair_and_says_to_stop_it() {
    let ui = window();
    for (server_host, kind, _) in host_kinds() {
        checked_surface(&ui, server_host, true);
        assert!(!enabled(&ui, "gui-integrity-repair"), "{kind}");
        assert_eq!(
            blockers_shown(&ui),
            ["gui-integrity-repair-blocked-running"]
        );
        assert!(
            enabled(&ui, "gui-integrity-verify"),
            "reading files is safe while the host runs"
        );
    }
}

#[test]
fn an_isolation_session_holds_repair_with_its_own_reason() {
    let ui = window();
    for (server_host, kind, id) in host_kinds() {
        checked_surface(&ui, server_host, false);
        ui.set_isolation_host_kind(kind.into());
        ui.set_isolation_host_id(id.into());
        ui.set_active_workflow_id("mod-isolation".into());
        settle(&ui);
        assert!(!enabled(&ui, "gui-integrity-repair"), "{kind}");
        assert_eq!(
            blockers_shown(&ui),
            ["gui-integrity-repair-blocked-isolation"]
        );
    }
}

#[test]
fn a_repair_in_progress_shows_progress_rather_than_a_blocker() {
    let ui = window();
    for (server_host, kind, id) in host_kinds() {
        checked_surface(&ui, server_host, false);
        ui.set_host_busy_kind(kind.into());
        ui.set_host_busy_id(id.into());
        ui.set_host_busy_text("Downloading files".into());
        settle(&ui);

        assert!(!enabled(&ui, "gui-integrity-repair"), "{kind}");
        assert!(!enabled(&ui, "gui-integrity-verify"));
        assert!(blockers_shown(&ui).is_empty());
        assert!(visible(&ui, "Downloading files"));
        assert!(
            enabled(&ui, "gui-integrity-stop"),
            "the repair can be stopped"
        );
    }
}

#[test]
fn findings_nothing_can_fetch_offer_no_repair_and_say_why() {
    let ui = window();
    for (server_host, kind, _) in host_kinds() {
        checked_surface(&ui, server_host, false);
        ui.set_integrity_repair_targets(strings(&[]));
        ui.set_integrity_unrepairable(strings(&["mods/shipped.jar"]));
        settle(&ui);
        assert!(find_button(&ui, "gui-integrity-repair").is_none(), "{kind}");
        assert!(visible(
            &ui,
            &lookup_args("gui-integrity-unrepairable", [("count", Arg::from(1))])
        ));
    }
}

#[test]
fn shared_launch_files_are_described_for_what_each_host_kind_supports() {
    let ui = window();
    checked_surface(&ui, false, false);
    assert!(visible(&ui, &lookup("gui-integrity-shared-detail")));
    checked_surface(&ui, true, false);
    assert!(visible(&ui, &lookup("gui-integrity-shared-servers")));
}

#[test]
fn diagnostics_links_to_the_logs_surface() {
    let ui = window();
    let logs = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&logs);
    ui.on_open_live_log(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));
    for (server_host, kind, id) in host_kinds() {
        checked_surface(&ui, server_host, false);
        button(&ui, "gui-integrity-open-logs").mock_single_click(PointerEventButton::Left);
        settle(&ui);
        assert_eq!(ui.get_workspace_tab(), "Logs");
        assert_eq!(
            logs.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );
    }
}

fn client_workspace(ui: &AppWindow, row: InstanceRow) {
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace("instance".into());
    ui.set_instances(ModelRc::new(VecModel::from(vec![row])));
    ui.set_selected_instance_index(0);
    ui.set_selected_instance_id("host".into());
    settle(ui);
}

fn server_workspace(ui: &AppWindow, row: ServerRow) {
    ui.set_loading(false);
    ui.set_page(1);
    ui.set_workspace("server".into());
    ui.set_servers(ModelRc::new(VecModel::from(vec![row])));
    ui.set_selected_server_index(0);
    ui.set_selected_server_id("server".into());
    settle(ui);
}

fn instance() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: SharedString::default(),
        id: "host".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/host".into(),
        updated: "Just now".into(),
        linked_servers: 0,
        mods: 2,
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
    }
}

fn server(running: bool) -> ServerRow {
    ServerRow {
        snapshot_deletion_note: SharedString::default(),
        id: "server".into(),
        name: "Repro Server".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        linked_client_id: SharedString::default(),
        state: if running { "Running" } else { "Stopped" }.into(),
        state_kind: if running { "running" } else { "stopped" }.into(),
        state_detail: if running {
            "Accepting connections"
        } else {
            "Not running"
        }
        .into(),
        address: "127.0.0.1:25565".into(),
        path: "/tmp/ephervex/servers/server".into(),
        installed: true,
        can_start: !running,
        can_stop: running,
        can_restart: running,
        can_kill: running,
        can_send_command: running,
        can_connect: running,
        connect_reason: SharedString::default(),
        needs_attention: false,
    }
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
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::from(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}
