//! What a crashed server's workspace actually offers.
//!
//! The notice is the only path from "it failed to start" to a mod that can be
//! switched off, and both halves of that path are markup: a button on one tab
//! that opens another, and a button that acts on the list it sits beside.
//! Neither is visible from reading the callbacks, so the window is built for
//! real and clicked for real.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, ModRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn a_crashed_servers_overview_leads_to_the_mods_it_points_at() {
    let ui = window();
    let loaded: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&loaded);
    ui.on_load_server_mods(move |id| record.borrow_mut().push(id.to_string()));

    crashed_server_workspace(&ui);
    settle(&ui);

    let review = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-crash-review-mods"))
        .next()
        .expect("a crashed server offers a way into its mods");
    review.mock_single_click(PointerEventButton::Left);

    assert_eq!(
        ui.get_workspace_tab(),
        "Mods",
        "reviewing must land on the panel whose rows carry the marks"
    );
    assert_eq!(
        loaded.borrow().as_slice(),
        ["repro-server"],
        "the panel it opens has to be asked for its list"
    );
}

#[test]
fn the_mods_panel_offers_to_disable_only_what_the_report_stated() {
    let ui = window();
    let disabled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&disabled);
    ui.on_disable_suggested_mods(move |id| record.borrow_mut().push(id.to_string()));

    crashed_server_workspace(&ui);
    ui.set_workspace_tab("Mods".into());
    ui.set_server_mods(ModelRc::new(VecModel::from(vec![
        suspect_mod(),
        ordinary_mod(),
    ])));
    // One stated suspect. The compat trigger in the list is deliberately not
    // counted: it is an inference, and it stays a row the user ticks by hand.
    ui.set_crash_suggested(1);
    settle(&ui);

    let label = lookup_args("gui-crash-disable-suggested", [("count", Arg::Count(1))]);
    let action = ElementHandle::find_by_accessible_label(&ui, &label)
        .next()
        .expect("the panel offers to act on the suggestion");
    action.mock_single_click(PointerEventButton::Left);

    assert_eq!(disabled.borrow().as_slice(), ["repro-server"]);
    // The rows themselves are on screen, each with its own switch, which is
    // what makes acting on one suspect at a time possible.
    for title in ["Euphoria Patcher", "UtilitiX"] {
        let label = lookup_args("gui-mod-loads", [("value", Arg::from(title.to_owned()))]);
        assert!(
            ElementHandle::find_by_accessible_label(&ui, &label)
                .next()
                .is_some(),
            "{title} has no switch on the server's mods panel"
        );
    }
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-crash-review-mods"))
            .next()
            .is_none(),
        "the panel it would open is the one already being looked at"
    );
}

#[test]
fn a_report_naming_only_mods_already_switched_off_offers_no_action() {
    let ui = window();
    let disabled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&disabled);
    ui.on_disable_suggested_mods(move |id| record.borrow_mut().push(id.to_string()));

    crashed_server_workspace(&ui);
    ui.set_workspace_tab("Mods".into());
    let mut already_off = suspect_mod();
    already_off.enabled = false;
    ui.set_server_mods(ModelRc::new(VecModel::from(vec![already_off])));
    ui.set_crash_suggested(0);
    settle(&ui);

    let label = lookup_args("gui-crash-disable-suggested", [("count", Arg::Count(0))]);
    let action = ElementHandle::find_by_accessible_label(&ui, &label)
        .next()
        .expect("the action is shown, so the count is readable");
    action.mock_single_click(PointerEventButton::Left);

    assert!(
        disabled.borrow().is_empty(),
        "a suggestion with nothing left to act on must not be pressable"
    );
}

#[test]
fn a_server_that_has_not_crashed_shows_no_notice() {
    let ui = window();
    crashed_server_workspace(&ui);
    ui.set_crash_report(SharedString::default());
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-crash-review-mods"))
            .next()
            .is_none()
    );
}

/// The server workspace, on a server whose newest crash report named a mod.
fn crashed_server_workspace(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(1);
    ui.set_workspace("server".into());
    ui.set_workspace_tab("Overview".into());
    ui.set_servers(ModelRc::new(VecModel::from(vec![server()])));
    ui.set_selected_server_index(0);
    ui.set_selected_server_id("repro-server".into());
    ui.set_crash_report("crash-2026-08-30_18.09.07-server.txt".into());
    ui.set_crash_failure(
        "java.lang.NoClassDefFoundError: net/minecraft/client/multiplayer/ClientLevel".into(),
    );
    ui.set_crash_client_code_on_server(true);
    ui.set_crash_suggested(1);
}

/// Renders once, which is what gives every element the geometry a pointer event
/// is resolved against.
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

fn suspect_mod() -> ModRow {
    ModRow {
        suspect_reason: lookup("crash-reason-named").into(),
        suspect_confidence: "high".into(),
        title: "Euphoria Patcher".into(),
        logical_path: "mods/euphoria_patcher-1.6.3-neoforge.jar".into(),
        ..ordinary_mod()
    }
}

fn ordinary_mod() -> ModRow {
    ModRow {
        project_id: "UTILITIX".into(),
        logical_path: "mods/utilitix-1.21.1-1.13.2.jar".into(),
        title: "UtilitiX".into(),
        file_name: "utilitix-1.21.1-1.13.2.jar".into(),
        version: "1.13.2".into(),
        version_known: true,
        provider: "Modrinth".into(),
        origin: "From the pack".into(),
        origin_kind: "pack".into(),
        compatibility: "1.21.1 | NeoForge".into(),
        author: "MelanX".into(),
        icon: slint::Image::default(),
        missing: false,
        enabled: true,
        pack_owned: true,
        can_manage: true,
        can_change_version: true,
        can_update: true,
        can_remove: true,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

fn server() -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        linked_client_id: SharedString::default(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        state_detail: "Not running".into(),
        address: "127.0.0.1:25565".into(),
        path: "/tmp/ephervex/servers/repro-server".into(),
        installed: true,
        can_start: true,
        can_stop: false,
        can_restart: false,
        can_kill: false,
        can_send_command: false,
        can_connect: false,
        connect_reason: SharedString::default(),
        needs_attention: false,
    }
}
