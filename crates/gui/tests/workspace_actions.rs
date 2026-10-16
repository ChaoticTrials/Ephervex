//! What a workspace offers to do with the thing it is a workspace for.
//!
//! A client workspace names the instance's directory on its overview card, so
//! it has to be able to open it. The button that does is the same one the
//! server workspace carries, which is why it is easy for one of the two to be
//! wired to the other's folder.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::{ComponentHandle, PhysicalSize, SharedString};

/// The client workspace opens the client's own folder.
///
/// Its overview already prints the instance path, and the only Open Folder in
/// that view used to belong to the linked-server card, so the workspace named a
/// directory it could not open and offered the server's instead.
#[test]
fn the_client_workspace_opens_the_clients_own_folder() {
    let ui = window();
    let clients: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let servers: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let client_sink = Rc::clone(&clients);
    ui.on_open_client_folder(move |id| client_sink.borrow_mut().push(id.to_string()));
    let server_sink = Rc::clone(&servers);
    ui.on_open_server_folder(move |id| server_sink.borrow_mut().push(id.to_string()));

    client_workspace(&ui);

    let open_folder = widest(&ui, &lookup("gui-open-folder"))
        .expect("the client workspace offers a way to open the instance folder");
    open_folder.invoke_accessible_default_action();

    assert_eq!(
        clients.borrow().as_slice(),
        ["repro-client".to_owned()],
        "the button opens the instance the workspace is for"
    );
    assert!(
        servers.borrow().is_empty(),
        "and never a server's folder in its place"
    );
}

#[test]
fn server_overview_shortcuts_target_its_files_and_existing_surfaces() {
    let ui = window();
    ui.window().set_size(PhysicalSize::new(900, 900));
    let requests = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&requests);
    ui.on_open_server_location(move |id, location| {
        sink.borrow_mut()
            .push((id.to_string(), location.to_string()));
    });
    support::show_server(
        &ui,
        ServerRow {
            id: "repro-server".into(),
            name: "Repro server".into(),
            ..ServerRow::default()
        },
        "Overview",
    );
    let surfaces = support::follow_surface_requests(&ui);
    settle(&ui);
    widest(&ui, &lookup("gui-open-server-folder"))
        .expect("server folder shortcut")
        .invoke_accessible_default_action();
    assert_eq!(
        requests.borrow().as_slice(),
        [("repro-server".to_owned(), "server".to_owned())]
    );
    widest(&ui, &lookup("gui-server-properties-shortcut"))
        .expect("properties shortcut")
        .invoke_accessible_default_action();
    assert_eq!(surfaces.borrow().as_slice(), [(1, "Properties".to_owned())]);
}

/// A vanilla instance has no build to change, so the row the folder button
/// shares carries nothing else. The way into the folder has to survive that.
#[test]
fn a_vanilla_instance_still_offers_its_folder() {
    let ui = window();
    let clients: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let client_sink = Rc::clone(&clients);
    ui.on_open_client_folder(move |id| client_sink.borrow_mut().push(id.to_string()));

    let mut vanilla = instance();
    vanilla.loader = "Vanilla".into();
    vanilla.loader_kind = "vanilla".into();
    client_workspace_with(&ui, vanilla);

    assert!(
        widest(&ui, &lookup("gui-loader-change-open")).is_none(),
        "there is no build to change on a vanilla instance"
    );
    let open_folder =
        widest(&ui, &lookup("gui-open-folder")).expect("a vanilla instance still opens its folder");
    open_folder.invoke_accessible_default_action();

    assert_eq!(clients.borrow().as_slice(), ["repro-client".to_owned()]);
}

/// Turning the pack-mod opt-in on and leaving the Settings tab destroys the
/// switch; coming back builds a new one from the host's recorded value. The
/// switch shows that value, the one the instance read back after the change,
/// rather than a position of its own.
#[test]
fn the_pack_mod_opt_in_survives_leaving_its_tab() {
    let ui = window();
    let asked: Rc<RefCell<Vec<(String, bool)>>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_set_allow_pack_mod_version_changes(move |instance, allow| {
        record.borrow_mut().push((instance.to_string(), allow));
    });

    // Tall enough that the opt-in, below the Java runtime section, lies
    // inside the pane.
    ui.window().set_size(PhysicalSize::new(1480, 2400));
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Settings");
    settle(&ui);

    let toggle = opt_in(&ui).expect("the Settings tab offers the opt-in");
    toggle.invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(
        asked.borrow().as_slice(),
        [("repro-client".to_owned(), true)],
        "turning it on has to reach the launcher"
    );
    assert_eq!(
        opt_in(&ui).and_then(|toggle| toggle.accessible_checked()),
        Some(false),
        "until the instance records it, the switch shows what is recorded"
    );
    support::update_host(&ui, 0, |view| view.allow_pack_mod_changes = true);
    settle(&ui);

    support::set_surface(&ui, 1, "Overview");
    settle(&ui);
    support::set_surface(&ui, 1, "Settings");
    settle(&ui);

    let toggle = opt_in(&ui).expect("the opt-in is still there");
    assert_eq!(
        toggle.accessible_checked(),
        Some(true),
        "coming back to the tab must not turn it off again"
    );
}

/// The switch itself, not the label beside it, which carries the same text.
fn opt_in(ui: &AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-allow-pack-mod-changes-toggle")).find(
        |element| {
            element.size().width > 0.0
                && element.accessible_role() == Some(AccessibleRole::Checkbox)
        },
    )
}

/// The header names the Java a host would run with once its status is read,
/// and pressing it opens the host's runtime settings: Settings on a client,
/// Properties on a server.
#[test]
fn the_header_java_pill_opens_the_hosts_runtime_settings() {
    let ui = window();
    client_workspace(&ui);
    let note = lookup_args(
        "gui-java-pill-note",
        [
            ("first", Arg::from("chosen automatically")),
            ("second", Arg::from("Healthy")),
        ],
    );
    let label = lookup_args(
        "gui-java-pill",
        [
            ("first", Arg::from("Java 21.0.8 (Eclipse Adoptium)")),
            ("second", Arg::from(note.as_str())),
        ],
    );
    assert!(
        widest(&ui, &label).is_none(),
        "nothing is claimed before the status is read"
    );

    support::update_host(&ui, 0, |view| {
        view.java_summary = "Java 21.0.8 (Eclipse Adoptium)".into();
        view.java_tone = "healthy".into();
        view.java_tone_label = "Healthy".into();
        view.java_origin = "chosen automatically".into();
    });
    settle(&ui);
    widest(&ui, &label)
        .expect("the header shows the runtime")
        .invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(support::surface(&ui, 0), "Settings");

    support::show_server(
        &ui,
        ServerRow {
            id: "repro-server".into(),
            ..ServerRow::default()
        },
        "Overview",
    );
    support::follow_surface_requests(&ui);
    support::update_host(&ui, 0, |view| {
        view.java_summary = "Java 21.0.8 (Eclipse Adoptium)".into();
        view.java_tone = "healthy".into();
        view.java_tone_label = "Healthy".into();
        view.java_origin = "chosen automatically".into();
    });
    settle(&ui);
    widest(&ui, &label)
        .expect("a server's header shows it too")
        .invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(support::surface(&ui, 0), "Properties");
}

fn client_workspace(ui: &AppWindow) {
    client_workspace_with(ui, instance());
}

fn client_workspace_with(ui: &AppWindow, row: InstanceRow) {
    support::show_client(ui, row, "Overview");
    support::follow_surface_requests(ui);
    settle(ui);
}

fn instance() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
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
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}

fn install_i18n(ui: &AppWindow) {
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::Count(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.to_string()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.to_string())),
                ("second", Arg::from(second.to_string())),
            ],
        )
        .into()
    });
}
