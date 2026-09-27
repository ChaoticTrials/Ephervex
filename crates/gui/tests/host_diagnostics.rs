//! The Diagnostics surface of client and server workspaces: verification,
//! repair, and the reasons Repair waits.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, BundlePartRow, GuiI18n, InstanceRow, ServerRow};
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
    } else {
        let mut row = instance();
        row.can_kill = running;
        row.can_launch = !running;
        client_workspace(ui, row);
    }
    ui.set_host_busy_kind(SharedString::default());
    ui.set_host_busy_id(SharedString::default());
    support::set_surface(ui, 1, "Diagnostics");
    support::update_host(ui, 0, |view| {
        view.isolation_active = false;
        view.integrity_state = "checked".into();
        view.integrity_summary =
            lookup_args("gui-integrity-problems", [("count", Arg::from(1))]).into();
        view.integrity_repair_targets = strings(&["mods/gone.jar"]);
        view.integrity_unrepairable = strings(&[]);
    });
    settle(ui);
}

fn host_kinds() -> [(bool, &'static str, &'static str); 2] {
    [(false, "client", "host"), (true, "server", "server")]
}

#[test]
fn diagnostics_is_a_surface_of_client_and_server_workspaces() {
    let ui = window();
    let requests = support::follow_surface_requests(&ui);
    for (server_host, kind, _) in host_kinds() {
        if server_host {
            server_workspace(&ui, server(false));
        } else {
            client_workspace(&ui, instance());
        }
        settle(&ui);

        tab(&ui, "gui-diagnostics-tab").mock_single_click(PointerEventButton::Left);
        settle(&ui);

        assert_eq!(
            requests.borrow().last(),
            Some(&(1, "Diagnostics".to_owned()))
        );
        assert_eq!(support::surface(&ui, 0), "Diagnostics");
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
    for (server_host, kind, _) in host_kinds() {
        checked_surface(&ui, server_host, false);
        support::update_host(&ui, 0, |view| view.isolation_active = true);
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
        support::update_host(&ui, 0, |view| {
            view.integrity_repair_targets = strings(&[]);
            view.integrity_unrepairable = strings(&["mods/shipped.jar"]);
        });
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
    let requests = support::follow_surface_requests(&ui);
    for (server_host, _, _) in host_kinds() {
        checked_surface(&ui, server_host, false);
        button(&ui, "gui-integrity-open-logs").mock_single_click(PointerEventButton::Left);
        settle(&ui);
        assert_eq!(requests.borrow().last(), Some(&(1, "Logs".to_owned())));
        assert_eq!(support::surface(&ui, 0), "Logs");
    }
}

fn bundle_rows(selected: bool) -> ModelRc<BundlePartRow> {
    ModelRc::new(VecModel::from(vec![
        BundlePartRow {
            name: "logs/latest.log".into(),
            kind: lookup("gui-bundle-kind-game-log").into(),
            size: "2.0 KiB".into(),
            selected,
        },
        BundlePartRow {
            name: "crash-reports/crash-new.txt".into(),
            kind: lookup("gui-bundle-kind-crash-report").into(),
            size: "4.0 KiB".into(),
            selected: false,
        },
    ]))
}

#[test]
fn offered_bundle_parts_are_choices_and_export_waits_for_one() {
    let ui = window();
    let toggles = Rc::new(RefCell::new(Vec::new()));
    let toggle_sink = Rc::clone(&toggles);
    ui.on_toggle_bundle_part(move |kind, id, name, selected| {
        toggle_sink.borrow_mut().push((
            kind.to_string(),
            id.to_string(),
            name.to_string(),
            selected,
        ));
    });
    let exports = Rc::new(RefCell::new(Vec::new()));
    let export_sink = Rc::clone(&exports);
    ui.on_export_bundle(move |kind, id| {
        export_sink
            .borrow_mut()
            .push((kind.to_string(), id.to_string()));
    });
    for (server_host, kind, id) in host_kinds() {
        checked_surface(&ui, server_host, false);
        support::update_host(&ui, 0, |view| {
            view.bundle_parts = bundle_rows(true);
            view.bundle_selected_count = 1;
        });
        settle(&ui);

        assert!(visible(&ui, &lookup("gui-bundle-privacy")), "{kind}");
        let switch = ElementHandle::find_by_accessible_label(&ui, "logs/latest.log")
            .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
            .expect("each offered part has its own switch");
        assert_eq!(switch.accessible_checked(), Some(true));
        switch.invoke_accessible_default_action();
        assert_eq!(
            toggles.borrow().last(),
            Some(&(
                kind.to_owned(),
                id.to_owned(),
                "logs/latest.log".to_owned(),
                false
            ))
        );

        assert!(enabled(&ui, "gui-bundle-export"));
        button(&ui, "gui-bundle-export").mock_single_click(PointerEventButton::Left);
        assert_eq!(
            exports.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );

        support::update_host(&ui, 0, |view| {
            view.bundle_parts = bundle_rows(false);
            view.bundle_selected_count = 0;
        });
        settle(&ui);
        assert!(!enabled(&ui, "gui-bundle-export"));
        assert!(visible(&ui, &lookup("gui-bundle-choose-one")));
    }
}

#[test]
fn an_exported_bundle_says_where_it_went_and_can_be_shown() {
    let ui = window();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_open_bundle_folder(move |path| sink.borrow_mut().push(path.to_string()));
    for (server_host, _, _) in host_kinds() {
        checked_surface(&ui, server_host, false);
        support::update_host(&ui, 0, |view| {
            view.bundle_parts = bundle_rows(true);
            view.bundle_selected_count = 1;
            view.bundle_result = "Saved one file to /home/user/bundle.zip.".into();
            view.bundle_result_path = "/home/user/bundle.zip".into();
        });
        settle(&ui);
        assert!(visible(&ui, "Saved one file to /home/user/bundle.zip."));
        button(&ui, "gui-bundle-show-folder").mock_single_click(PointerEventButton::Left);
        assert_eq!(
            opened.borrow().last().map(String::as_str),
            Some("/home/user/bundle.zip")
        );

        support::update_host(&ui, 0, |view| {
            view.bundle_result = SharedString::default();
            view.bundle_result_path = SharedString::default();
            view.bundle_error = "The disk is full.".into();
        });
        settle(&ui);
        assert!(
            visible(&ui, "The disk is full."),
            "a failed export stays on screen"
        );
    }
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
        restart_stage: SharedString::default(),
        can_restart: false,
        paired_server_id: SharedString::default(),
        paired_server_name: SharedString::default(),
        pair_start_together: false,
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
        restart_stage: SharedString::default(),
        needs_attention: false,
        paired_client_id: SharedString::default(),
        paired_client_name: SharedString::default(),
        pair_start_together: false,
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
