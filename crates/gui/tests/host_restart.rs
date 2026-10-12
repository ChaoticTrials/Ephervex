//! Restart in the host header: when it is offered, what it calls, and how a
//! restart under way shows on every view of the host.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::ElementHandle;
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

fn visible(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

fn button(ui: &AppWindow, message: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup(message))
        .filter(|element| element.accessible_enabled().is_some())
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("no button labeled {message}"))
}

fn enabled(ui: &AppWindow, message: &str) -> bool {
    button(ui, message)
        .accessible_enabled()
        .expect("the button reports whether it is enabled")
}

fn running_client(can_restart: bool, stage: &str) -> InstanceRow {
    let mut row = instance();
    row.can_launch = false;
    row.can_kill = true;
    row.state = "Running".into();
    row.state_kind = "running".into();
    row.can_restart = can_restart;
    row.restart_stage = stage.into();
    row
}

fn restarting_server(stage: &str) -> ServerRow {
    let mut row = server(true);
    row.restart_stage = stage.into();
    row
}

#[test]
fn a_running_client_this_run_launched_can_be_restarted_from_its_header() {
    let ui = window();
    let restarts = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&restarts);
    ui.on_client_restart(move |id| sink.borrow_mut().push(id.to_string()));
    client_workspace(&ui, running_client(true, ""));

    assert!(enabled(&ui, "gui-restart"));
    button(&ui, "gui-restart").mock_single_click(PointerEventButton::Left);
    assert_eq!(restarts.borrow().as_slice(), ["host".to_owned()]);
}

#[test]
fn restart_is_not_start_for_a_stopped_client() {
    let ui = window();
    client_workspace(&ui, instance());
    assert!(!enabled(&ui, "gui-restart"));
    assert!(enabled(&ui, "gui-launch"), "starting it is still Launch");
}

#[test]
fn a_client_this_run_did_not_launch_is_not_offered_a_restart() {
    let ui = window();
    client_workspace(&ui, running_client(false, ""));
    assert!(!enabled(&ui, "gui-restart"));
    assert!(enabled(&ui, "gui-stop-game"), "it can still be stopped");
}

#[test]
fn a_server_restart_is_asked_for_from_its_header() {
    let ui = window();
    let restarts = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&restarts);
    ui.on_server_restart(move |id| sink.borrow_mut().push(id.to_string()));
    server_workspace(&ui, server(true));
    button(&ui, "gui-restart").mock_single_click(PointerEventButton::Left);
    assert_eq!(restarts.borrow().as_slice(), ["server".to_owned()]);

    server_workspace(&ui, server(false));
    assert!(
        !enabled(&ui, "gui-restart"),
        "a stopped server is started, not restarted"
    );
}

#[test]
fn a_restart_under_way_shows_its_stage_as_progress_and_holds_the_host_actions() {
    let ui = window();
    for (stage, message) in [
        ("stopping", "gui-restart-stage-stopping"),
        ("starting", "gui-restart-stage-starting"),
    ] {
        client_workspace(&ui, running_client(true, stage));
        assert!(visible(&ui, &lookup(message)), "client {stage}");
        assert!(visible(&ui, &lookup("gui-state-restarting")));
        assert!(!enabled(&ui, "gui-restart"));
        assert!(!enabled(&ui, "gui-stop-game"), "no stop races the restart");

        server_workspace(&ui, restarting_server(stage));
        assert!(visible(&ui, &lookup(message)), "server {stage}");
        assert!(!enabled(&ui, "gui-restart"));
        assert!(!enabled(&ui, "gui-stop"));
    }
}

#[test]
fn a_server_restart_shows_on_its_card_as_well_as_in_its_workspace() {
    let ui = window();
    ui.set_loading(false);
    ui.set_page(1);
    ui.set_workspace_open(false);
    ui.set_global_servers(ModelRc::new(VecModel::from(vec![restarting_server(
        "stopping",
    )])));
    ui.set_selected_global_server_index(0);
    ui.set_selected_global_server_id("server".into());
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-state-restarting")));
}

fn reason(ui: &AppWindow, message: &str) -> String {
    button(ui, message)
        .accessible_description()
        .expect("the button reports a description")
        .to_string()
}

fn server_in_state(state_kind: &str) -> ServerRow {
    let mut row = server(false);
    row.state_kind = state_kind.into();
    row.can_start = false;
    row
}

#[test]
fn a_disabled_restart_says_why_for_each_kind_of_host() {
    let ui = window();

    client_workspace(&ui, instance());
    assert_eq!(
        reason(&ui, "gui-restart"),
        lookup("gui-hint-restart-client-stopped")
    );

    client_workspace(&ui, running_client(false, ""));
    assert_eq!(
        reason(&ui, "gui-restart"),
        lookup("gui-restart-client-unavailable")
    );

    server_workspace(&ui, server(false));
    assert_eq!(
        reason(&ui, "gui-restart"),
        lookup("gui-hint-restart-server-stopped")
    );

    server_workspace(&ui, server_in_state("orphaned"));
    assert_eq!(
        reason(&ui, "gui-restart"),
        lookup("gui-hint-stop-server-uncontrolled")
    );
}

#[test]
fn a_restart_under_way_is_the_reason_the_host_actions_wait() {
    let ui = window();
    server_workspace(&ui, restarting_server("stopping"));

    let expected = lookup("gui-restart-stage-stopping");
    assert_eq!(reason(&ui, "gui-restart"), expected);
    assert_eq!(reason(&ui, "gui-stop"), expected);
}

#[test]
fn a_server_that_cannot_be_started_says_what_stands_in_the_way() {
    let ui = window();
    for (state_kind, message) in [
        ("orphaned", "gui-hint-server-uncontrolled-start"),
        ("unknown", "gui-hint-server-uncontrolled-start"),
        ("needs-recovery", "gui-hint-server-needs-recovery"),
    ] {
        server_workspace(&ui, server_in_state(state_kind));
        assert!(!enabled(&ui, "gui-start"), "{state_kind}");
        assert_eq!(reason(&ui, "gui-start"), lookup(message), "{state_kind}");
    }

    let mut row = server_in_state("stopped");
    row.installed = false;
    server_workspace(&ui, row);
    assert_eq!(
        reason(&ui, "gui-start"),
        lookup("gui-hint-server-not-installed")
    );
}

#[test]
fn server_properties_save_says_why_for_each_state_that_blocks_it() {
    let ui = window();
    for (state_kind, message) in [
        ("running", "gui-hint-save-requires-stopped"),
        ("orphaned", "gui-hint-save-server-uncontrolled"),
        ("unknown", "gui-hint-save-server-uncontrolled"),
        ("needs-recovery", "gui-hint-server-needs-recovery"),
        ("stopping", "gui-hint-save-server-stopping"),
        ("failed", "gui-hint-save-server-failed"),
    ] {
        let mut row = server_in_state(state_kind);
        row.can_stop = state_kind == "running";
        support::show_server(&ui, row, "Properties");
        settle(&ui);
        assert!(!enabled(&ui, "gui-save-changes"), "{state_kind}");
        assert_eq!(
            reason(&ui, "gui-save-changes"),
            lookup(message),
            "{state_kind}"
        );
    }

    support::show_server(&ui, server(false), "Properties");
    settle(&ui);
    assert!(enabled(&ui, "gui-save-changes"));
    assert_eq!(reason(&ui, "gui-save-changes"), "");
}

#[test]
fn a_disabled_force_kill_says_why() {
    let ui = window();

    server_workspace(&ui, server(false));
    assert!(!enabled(&ui, "gui-kill"));
    assert_eq!(
        reason(&ui, "gui-kill"),
        lookup("gui-hint-force-kill-server")
    );

    server_workspace(&ui, server_in_state("orphaned"));
    assert_eq!(
        reason(&ui, "gui-kill"),
        lookup("gui-hint-stop-server-uncontrolled")
    );

    server_workspace(&ui, server(true));
    assert_eq!(reason(&ui, "gui-kill"), "");
}

#[test]
fn a_disabled_connect_shows_the_reason_its_server_row_carries() {
    let ui = window();
    let mut row = server(true);
    row.linked_client_id = "client".into();
    row.can_connect = false;
    row.connect_reason = "The client is already running.".into();
    ui.set_loading(false);
    ui.set_page(1);
    ui.set_workspace_open(false);
    ui.set_global_servers(ModelRc::new(VecModel::from(vec![row])));
    ui.set_selected_global_server_index(0);
    ui.set_selected_global_server_id("server".into());
    settle(&ui);

    assert!(!enabled(&ui, "gui-launch-client-connect"));
    assert_eq!(
        reason(&ui, "gui-launch-client-connect"),
        "The client is already running."
    );
}

fn client_workspace(ui: &AppWindow, row: InstanceRow) {
    support::show_client(ui, row, "Overview");
    settle(ui);
}

fn server_workspace(ui: &AppWindow, row: ServerRow) {
    support::show_server(ui, row, "Overview");
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
        stoppable_linked_servers: 0,
        unstoppable_linked_servers: 0,
        mods: 2,
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
        console_status: SharedString::default(),
        console_completion: 0,
        console_delivered: false,
        can_connect: running,
        connect_reason: SharedString::default(),
        restart_stage: SharedString::default(),
        needs_attention: false,
        paired_client_id: SharedString::default(),
        paired_client_name: SharedString::default(),
        pair_start_together: false,
        pair_starting: false,
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
    // Tall enough that the whole surface is on screen: the testing backend
    // finds only what is within the window, and the bundle choice sits below
    // the verification results.
    ui.window().set_size(PhysicalSize::new(1480, 1400));
    ui.show().expect("show");
    ui
}
