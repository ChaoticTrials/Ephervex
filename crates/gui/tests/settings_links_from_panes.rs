//! Workspace pane buttons that lead to a Settings tab.
//!
//! Leaving for Settings closes the workspace, and closing it removes the pane
//! rows at once, which takes the asking pane and its handler with them. Each
//! button here still has to land on its tab.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::Cell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AccountRow, AppWindow, DiagnosticFindingRow, GuiI18n, InstanceRow, ServerRow,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// Adding an account from a pane's Launch as menu opens the accounts.
#[test]
fn launch_as_add_account_opens_the_accounts_settings() {
    let ui = window();
    support::follow_workspace_close(&ui);
    support::show_client(&ui, instance(), "Overview");
    ui.set_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: "alice-id".into(),
        name: "Alice".into(),
        is_default: true,
        ..AccountRow::default()
    }])));
    ui.set_settings_tab("General".into());
    settle(&ui);

    widest(&ui, "gui-launch-as").mock_single_click(PointerEventButton::Left);
    settle(&ui);
    // The menu is a popup that a pointer press here cannot reach, so it is
    // walked the way the keyboard does: past the one account to Add Account.
    for key in [Key::DownArrow, Key::DownArrow, Key::Return] {
        let text = SharedString::from(key);
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text });
        settle(&ui);
    }

    assert_left_for(&ui, "Accounts");
}

/// A client's settings pane leads to the global defaults it cannot override.
#[test]
fn the_global_defaults_button_opens_the_minecraft_settings() {
    let ui = window();
    // Tall enough for the form's last row, which a scrolled-off pane never lays out.
    ui.window().set_size(PhysicalSize::new(1480, 2400));
    support::follow_workspace_close(&ui);
    support::show_client(&ui, instance(), "Settings");
    ui.set_settings_tab("General".into());
    settle(&ui);

    widest(&ui, "gui-open-global-defaults").mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_left_for(&ui, "Minecraft");
}

/// A Java finding opens the Java settings, with the runtimes asked for, from a
/// client's diagnosis and a server's alike.
#[test]
fn a_java_finding_opens_the_java_settings() {
    let ui = window();
    support::follow_workspace_close(&ui);
    let loads = Rc::new(Cell::new(0));
    let count = Rc::clone(&loads);
    ui.on_load_java_runtimes(move || count.set(count.get() + 1));

    for server in [false, true] {
        if server {
            support::show_server(&ui, server_row(), "Diagnostics");
        } else {
            support::show_client(&ui, instance(), "Diagnostics");
        }
        support::update_host(&ui, 0, |view| {
            view.diagnosis_state = "result".into();
            view.diagnosis_summary =
                lookup_args("gui-diagnosis-problems", [("count", Arg::from(1))]).into();
            view.diagnosis_problems = ModelRc::new(VecModel::from(vec![DiagnosticFindingRow {
                key: "p0".into(),
                problem: true,
                title: "Java 17 cannot run Minecraft 1.21.1".into(),
                route: "java-settings".into(),
                route_label: lookup("gui-diagnosis-open-java-settings").into(),
                ..DiagnosticFindingRow::default()
            }]));
        });
        ui.set_settings_tab("General".into());
        settle(&ui);
        let before = loads.get();

        widest(&ui, "gui-diagnosis-open-java-settings").mock_single_click(PointerEventButton::Left);
        settle(&ui);

        assert_left_for(&ui, "Java");
        let host = if server { "server" } else { "client" };
        assert_eq!(
            loads.get(),
            before + 1,
            "a {host}'s finding loads the runtimes"
        );
    }
}

fn assert_left_for(ui: &AppWindow, tab: &str) {
    assert_eq!(ui.get_page(), 4, "the button opens Settings");
    assert_eq!(ui.get_settings_tab(), tab, "the button opens its tab");
    assert!(!ui.get_workspace_open(), "the workspace closes");
}

/// The largest element showing this text, which is the control rather than the
/// text inside it.
fn widest(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup(id))
        .filter(|element| element.size().width > 0.0)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("nothing shows {id}"))
}

fn instance() -> InstanceRow {
    InstanceRow {
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        icon_slot: -1,
        ..InstanceRow::default()
    }
}

fn server_row() -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        minecraft: "1.21.1".into(),
        installed: true,
        ..ServerRow::default()
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
