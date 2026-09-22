//! The isolation surface and its persistent host-workspace actions.
#![allow(clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AppWindow, GuiI18n, InstanceRow, IsolationCandidateRow, ServerRow, WorkflowActionRow,
};
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

fn button(ui: &AppWindow, message: &str) -> ElementHandle {
    labeled_button(ui, &lookup(message))
}

fn labeled_button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.accessible_enabled().is_some())
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("no button labeled {label}"))
}

fn tab(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("no workspace tab labeled {label}"))
}

#[test]
fn setup_requires_each_reduced_confidence_acknowledgement() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Isolation".into());
    ui.set_isolation_phase("setup".into());
    ui.set_isolation_summary("Two candidate roots can be tested.".into());
    ui.set_isolation_candidates(ModelRc::new(VecModel::from(vec![
        IsolationCandidateRow {
            path: "mods/a.jar".into(),
            name: "A".into(),
            selected: false,
        },
        IsolationCandidateRow {
            path: "mods/b.jar".into(),
            name: "B".into(),
            selected: true,
        },
    ])));
    ui.set_isolation_unrecorded(strings(&["mods/unrecorded.jar"]));
    ui.set_isolation_unidentifiable(strings(&["mods/unidentified.jar"]));
    ui.set_isolation_missing(strings(&["mods/b.jar: required-library"]));
    settle(&ui);

    assert!(visible(&ui, "A"));
    assert!(visible(&ui, "mods/b.jar"));
    assert!(visible(&ui, "mods/unrecorded.jar"));
    assert!(visible(&ui, "mods/unidentified.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-unrecorded-detail")));
    assert!(visible(&ui, &lookup("gui-isolation-accept-unrecorded")));
    assert!(visible(&ui, &lookup("gui-isolation-accept-missing")));
    assert!(
        !button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );

    ui.set_isolation_accept_unrecorded(true);
    assert!(
        !button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );
    ui.set_isolation_accept_missing(true);
    assert!(
        button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );
}

#[test]
fn canceling_setup_stays_on_the_isolation_tab() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Isolation".into());
    ui.set_isolation_phase("setup".into());
    settle(&ui);

    let discarded = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&discarded);
    ui.on_discard_isolation_setup(move |kind, id| {
        sink.borrow_mut().push((kind.to_string(), id.to_string()));
    });
    let canceled = Rc::new(RefCell::new(false));
    let active_cancel = Rc::clone(&canceled);
    ui.on_cancel_isolation(move |_, _| *active_cancel.borrow_mut() = true);
    button(&ui, "gui-cancel").mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_eq!(
        discarded.borrow().as_slice(),
        &[("client".to_owned(), "host".to_owned())]
    );
    assert!(!*canceled.borrow(), "a setup has no session to restore");
    assert_eq!(ui.get_workspace_tab(), "Isolation");
}

#[test]
fn detailed_round_names_the_exact_configuration_and_records_either_answer() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Isolation".into());
    ui.set_isolation_phase("round".into());
    ui.set_isolation_round(4);
    ui.set_isolation_enabled(strings(&["mods/a.jar", "mods/shared.jar"]));
    ui.set_isolation_disabled(strings(&["mods/b.jar"]));
    ui.set_isolation_host_kind("client".into());
    ui.set_isolation_host_id("host".into());
    settle(&ui);

    assert!(visible(&ui, "mods/a.jar"));
    assert!(visible(&ui, "mods/shared.jar"));
    assert!(visible(&ui, "mods/b.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-test-instruction")));

    let answer = Rc::new(RefCell::new(String::new()));
    let recorded = Rc::clone(&answer);
    ui.on_answer_isolation(move |_, _, value| *recorded.borrow_mut() = value.into());
    button(&ui, "gui-isolation-issue-gone").mock_single_click(PointerEventButton::Left);
    assert_eq!(&*answer.borrow(), "gone");
    button(&ui, "gui-isolation-issue-present").mock_single_click(PointerEventButton::Left);
    assert_eq!(&*answer.borrow(), "present");
}

#[test]
fn isolation_state_survives_logs_and_mods_navigation() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Isolation".into());
    ui.set_isolation_phase("round".into());
    ui.set_isolation_round(3);
    ui.set_isolation_enabled(strings(&["mods/persistent.jar"]));
    settle(&ui);

    tab(&ui, &lookup("gui-instance-tab-logs")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(ui.get_workspace_tab(), "Logs");
    assert_eq!(ui.get_isolation_round(), 3);

    tab(&ui, &lookup("gui-isolation-tab")).invoke_accessible_default_action();
    settle(&ui);
    assert!(visible(&ui, "mods/persistent.jar"));

    tab(&ui, &lookup("gui-instance-tab-mods")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(ui.get_workspace_tab(), "Mods");

    tab(&ui, &lookup("gui-isolation-tab")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(ui.get_isolation_round(), 3);
    assert!(visible(&ui, "mods/persistent.jar"));
}

#[test]
fn persistent_round_actions_answer_and_open_the_detailed_surface() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Logs".into());
    ui.set_isolation_host_kind("client".into());
    ui.set_isolation_host_id("host".into());
    ui.set_active_workflow_id("mod-isolation".into());
    ui.set_active_workflow_title(lookup("gui-isolation-active").into());
    ui.set_active_workflow_summary("Waiting for an answer to round 2.".into());
    ui.set_active_workflow_actions(ModelRc::new(VecModel::from(vec![
        workflow_action("details", "gui-isolation-detailed-view", false),
        workflow_action("gone", "gui-isolation-issue-gone", true),
        workflow_action("present", "gui-isolation-issue-present", true),
        workflow_action("undo", "gui-isolation-undo", true),
    ])));
    settle(&ui);

    let recorded = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&recorded);
    ui.on_answer_isolation(move |kind, id, answer| {
        sink.borrow_mut()
            .push((kind.to_string(), id.to_string(), answer.to_string()));
    });
    button(&ui, "gui-isolation-issue-gone").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        recorded.borrow().as_slice(),
        &[("client".to_owned(), "host".to_owned(), "gone".to_owned())]
    );

    button(&ui, "gui-isolation-detailed-view").mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert_eq!(ui.get_workspace_tab(), "Isolation");
}

#[test]
fn contradiction_and_result_views_offer_the_safe_next_actions() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Isolation".into());
    ui.set_isolation_phase("contradiction".into());
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-isolation-contradiction")));
    assert!(
        button(&ui, "gui-isolation-undo")
            .accessible_enabled()
            .expect("the undo button reports whether it is enabled")
    );

    ui.set_isolation_phase("result".into());
    ui.set_isolation_summary("One candidate root remains.".into());
    ui.set_isolation_reproducing(strings(&["mods/a.jar"]));
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-isolation-finished")));
    assert!(visible(&ui, "mods/a.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-finished-detail")));
}

#[test]
fn host_actions_remain_available_for_client_and_server_workspaces() {
    let ui = window();
    client_workspace(&ui);
    ui.set_workspace_tab("Isolation".into());
    settle(&ui);

    let client_launches = Rc::new(RefCell::new(Vec::new()));
    let launch_sink = Rc::clone(&client_launches);
    ui.on_launch_client(move |id| launch_sink.borrow_mut().push(id.to_string()));
    button(&ui, "gui-launch").mock_single_click(PointerEventButton::Left);
    assert_eq!(client_launches.borrow().as_slice(), ["host".to_owned()]);

    server_workspace(&ui, server(false));
    ui.set_workspace_tab("Isolation".into());
    settle(&ui);
    let starts = Rc::new(RefCell::new(Vec::new()));
    let start_sink = Rc::clone(&starts);
    ui.on_server_start(move |id| start_sink.borrow_mut().push(id.to_string()));
    button(&ui, "gui-start").mock_single_click(PointerEventButton::Left);
    assert_eq!(starts.borrow().as_slice(), ["server".to_owned()]);

    server_workspace(&ui, server(true));
    ui.set_workspace_tab("Isolation".into());
    settle(&ui);
    let stops = Rc::new(RefCell::new(Vec::new()));
    let stop_sink = Rc::clone(&stops);
    ui.on_server_stop(move |id| stop_sink.borrow_mut().push(id.to_string()));
    let restarts = Rc::new(RefCell::new(Vec::new()));
    let restart_sink = Rc::clone(&restarts);
    ui.on_server_restart(move |id| restart_sink.borrow_mut().push(id.to_string()));
    button(&ui, "gui-stop").mock_single_click(PointerEventButton::Left);
    button(&ui, "gui-restart").mock_single_click(PointerEventButton::Left);
    assert_eq!(stops.borrow().as_slice(), ["server".to_owned()]);
    assert_eq!(restarts.borrow().as_slice(), ["server".to_owned()]);
}

fn workflow_action(id: &str, message: &str, requires_stopped: bool) -> WorkflowActionRow {
    WorkflowActionRow {
        id: id.into(),
        label: lookup(message).into(),
        tone: "secondary".into(),
        enabled: true,
        requires_stopped,
    }
}

fn client_workspace(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace("instance".into());
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_selected_instance_index(0);
    ui.set_selected_instance_id("host".into());
    ui.set_isolation_host_kind("client".into());
    ui.set_isolation_host_id("host".into());
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
