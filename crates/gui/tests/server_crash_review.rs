//! What a crashed server's workspace actually offers.
//!
//! The notice is the only path from "it failed to start" to a mod that can be
//! switched off, and both halves of that path are markup: a button on one tab
//! that opens another, and a button that acts on the list it sits beside.
//! Neither is visible from reading the callbacks, so the window is built for
//! real and clicked for real.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, ModRow, ServerRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn a_crashed_servers_overview_leads_to_the_mods_it_points_at() {
    let ui = window();
    crashed_server_workspace(&ui);
    let requests = support::follow_surface_requests(&ui);
    settle(&ui);

    let review = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-crash-review-mods"))
        .next()
        .expect("a crashed server offers a way into its mods");
    review.mock_single_click(PointerEventButton::Left);

    assert_eq!(
        requests.borrow().as_slice(),
        [(1, "Mods".to_owned())],
        "reviewing must land on the panel whose rows carry the marks"
    );
    assert_eq!(support::surface(&ui, 0), "Mods");
}

/// The notice leads to the report it was read from, the log, and Diagnostics,
/// each in the pane it sits in.
#[test]
fn a_crashed_servers_overview_leads_to_the_report_the_log_and_diagnostics() {
    let ui = window();
    let routes = follow_routes(&ui);
    for (action, surface, mode, file) in [
        ("gui-run-open-diagnostics", "Diagnostics", None, None),
        ("gui-diagnosis-open-log", "Logs", Some("live"), None),
        (
            "gui-diagnosis-open-crash-report",
            "Logs",
            Some("crashes"),
            Some(REPORT),
        ),
    ] {
        crashed_server_workspace(&ui);
        routes.clear();
        settle(&ui);

        button(&ui, action).mock_single_click(PointerEventButton::Left);
        assert_eq!(
            routes.surfaces.borrow().as_slice(),
            [(1, surface.to_owned())],
            "{action}"
        );
        assert_eq!(
            routes.modes.borrow().as_slice(),
            mode.map(|mode| (1, mode.to_owned())).as_slice(),
            "{action}"
        );
        assert_eq!(
            routes.files.borrow().as_slice(),
            file.map(|file| (1, file.to_owned())).as_slice(),
            "{action}"
        );
    }
}

/// While the failed-run notice above it offers the same ways to the evidence,
/// the crash notice leaves them to it, so each is offered once.
#[test]
fn the_crash_notice_leaves_its_routes_to_a_failed_run_notice() {
    let ui = window();
    crashed_server_workspace(&ui);
    support::update_host(&ui, 0, |view| {
        view.run_failure_title = lookup("gui-run-failed-server").into();
        view.run_failure = lookup("gui-run-server-exited-with-error").into();
        view.run_crash_report = REPORT.into();
    });
    settle(&ui);

    for action in [
        "gui-run-open-diagnostics",
        "gui-diagnosis-open-log",
        "gui-diagnosis-open-crash-report",
    ] {
        assert_eq!(buttons(&ui, action).len(), 1, "{action} is offered once");
    }
    assert_eq!(buttons(&ui, "gui-crash-review-mods").len(), 1);
}

/// A refused start's notice offers only the EULA, so a crash notice beside it
/// keeps its own ways to the evidence.
#[test]
fn the_crash_notice_keeps_its_routes_beside_a_refused_start() {
    let ui = window();
    crashed_server_workspace(&ui);
    support::update_host(&ui, 0, |view| {
        view.run_failure_title = lookup("gui-run-refused-server").into();
        view.run_eula_refused = true;
    });
    settle(&ui);

    for action in [
        "gui-run-open-diagnostics",
        "gui-diagnosis-open-log",
        "gui-diagnosis-open-crash-report",
    ] {
        assert_eq!(buttons(&ui, action).len(), 1, "{action} is still offered");
    }
}

/// Beside the marked mods, the notice opens the report the marks come from.
#[test]
fn the_mods_panel_notice_opens_the_report_the_marks_come_from() {
    let ui = window();
    let routes = follow_routes(&ui);
    crashed_server_workspace(&ui);
    support::set_surface(&ui, 1, "Mods");
    support::set_mods(&ui, ModelRc::new(VecModel::from(vec![suspect_mod()])));
    settle(&ui);
    routes.clear();

    button(&ui, "gui-diagnosis-open-crash-report").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        routes.surfaces.borrow().as_slice(),
        [(1, "Logs".to_owned())]
    );
    assert_eq!(
        routes.modes.borrow().as_slice(),
        [(1, "crashes".to_owned())]
    );
    assert_eq!(routes.files.borrow().as_slice(), [(1, REPORT.to_owned())]);
}

/// Two crashed server Overviews side by side: each notice's actions stay
/// inside their own pane and clear of one another, at the usual and the
/// smallest window and at every text size offered.
#[test]
fn the_crash_notice_actions_stay_inside_their_pane_and_apart() {
    let ui = window();
    for width in [1480_u32, 980] {
        // Tall enough that nothing is scrolled out of the pane.
        ui.window().set_size(PhysicalSize::new(width, 3000));
        // The default text size, then the largest one offered.
        for scale in [1.0, 1.25] {
            ui.global::<Theme>().set_text_scale(scale);
            crashed_server_workspace(&ui);
            let pane = |id: i32, column: i32| {
                let mut pane = support::pane(id, 0, "Overview");
                pane.column = column;
                pane.can_split_beside = false;
                pane.can_close = true;
                pane
            };
            ui.set_workspace_panes(ModelRc::new(VecModel::from(vec![pane(1, 0), pane(2, 1)])));
            settle(&ui);
            let window = ui
                .window()
                .size()
                .to_logical(ui.window().scale_factor())
                .width;
            let context = format!("at {width} wide, text {scale}");
            let (mut left, mut right) = (Vec::new(), Vec::new());
            for message in [
                "gui-crash-review-mods",
                "gui-run-open-diagnostics",
                "gui-diagnosis-open-crash-report",
                "gui-diagnosis-open-log",
            ] {
                let mut placed: Vec<Placed> =
                    buttons(&ui, message).iter().map(Placed::of).collect();
                placed.sort_by(|a, b| a.left.total_cmp(&b.left));
                assert_eq!(placed.len(), 2, "{context}: one {message} per pane");
                right.push(placed.pop().expect("two"));
                left.push(placed.pop().expect("two"));
            }
            let left_edge = left.iter().map(|it| it.right).fold(f32::MIN, f32::max);
            let right_start = right.iter().map(|it| it.left).fold(f32::MAX, f32::min);
            assert!(
                left_edge <= right_start,
                "{context}: the left notice reaches the right pane: {left:?} {right:?}"
            );
            assert!(
                right.iter().all(|it| it.right <= window + 0.5),
                "{context}: the right notice leaves the window: {right:?}"
            );
            for actions in [&left, &right] {
                for (index, action) in actions.iter().enumerate() {
                    for other in actions.iter().skip(index + 1) {
                        assert!(
                            !action.overlaps(other),
                            "{context}: {action:?} and {other:?} overlap"
                        );
                    }
                }
            }
        }
    }
}

/// Where one button is drawn.
#[derive(Debug)]
struct Placed {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl Placed {
    fn of(element: &ElementHandle) -> Self {
        let (position, size) = (element.absolute_position(), element.size());
        Self {
            left: position.x,
            right: position.x + size.width,
            top: position.y,
            bottom: position.y + size.height,
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }
}

#[test]
fn the_mods_panel_offers_to_disable_only_what_the_report_stated() {
    let ui = window();
    let disabled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&disabled);
    ui.on_disable_suggested_mods(move |id| record.borrow_mut().push(id.to_string()));

    crashed_server_workspace(&ui);
    support::set_surface(&ui, 1, "Mods");
    support::set_mods(
        &ui,
        ModelRc::new(VecModel::from(vec![suspect_mod(), ordinary_mod()])),
    );
    // One stated suspect. The compat trigger in the list is deliberately not
    // counted: it is an inference, and it stays a row the user ticks by hand.
    support::update_host(&ui, 0, |view| view.crash_suggested = 1);
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
    support::set_surface(&ui, 1, "Mods");
    let mut already_off = suspect_mod();
    already_off.enabled = false;
    support::set_mods(&ui, ModelRc::new(VecModel::from(vec![already_off])));
    support::update_host(&ui, 0, |view| view.crash_suggested = 0);
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
    support::update_host(&ui, 0, |view| view.crash_report = SharedString::default());
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-crash-review-mods"))
            .next()
            .is_none()
    );
}

const REPORT: &str = "crash-2026-08-30_18.09.07-server.txt";

/// The server workspace, on a server whose newest crash report named a mod.
fn crashed_server_workspace(ui: &AppWindow) {
    let mut view = support::server_view(server());
    view.crash_report = REPORT.into();
    view.crash_failure =
        "java.lang.NoClassDefFoundError: net/minecraft/client/multiplayer/ClientLevel".into();
    view.crash_client_code_on_server = true;
    view.crash_suggested = 1;
    support::show(ui, 1, vec![view], vec![support::pane(1, 0, "Overview")]);
}

/// Where the panes were sent: surfaces, log modes, and files opened, by pane.
#[derive(Default)]
struct Routes {
    surfaces: Rc<RefCell<Vec<(i32, String)>>>,
    modes: Rc<RefCell<Vec<(i32, String)>>>,
    files: Rc<RefCell<Vec<(i32, String)>>>,
}

impl Routes {
    fn clear(&self) {
        self.surfaces.borrow_mut().clear();
        self.modes.borrow_mut().clear();
        self.files.borrow_mut().clear();
    }
}

fn follow_routes(ui: &AppWindow) -> Routes {
    let routes = Routes {
        surfaces: support::follow_surface_requests(ui),
        ..Routes::default()
    };
    let sink = Rc::clone(&routes.modes);
    ui.on_log_show(move |pane, mode| sink.borrow_mut().push((pane, mode.to_string())));
    let sink = Rc::clone(&routes.files);
    ui.on_log_open_file(move |pane, name| sink.borrow_mut().push((pane, name.to_string())));
    routes
}

fn buttons(ui: &AppWindow, message: &str) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup(message))
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .collect()
}

fn button(ui: &AppWindow, message: &str) -> ElementHandle {
    buttons(ui, message)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no button labeled {message}"))
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
        provider_kind: "modrinth".into(),
        origin: "From the pack".into(),
        origin_kind: "pack".into(),
        compatibility: "1.21.1 | NeoForge".into(),
        author: "MelanX".into(),
        icon: slint::Image::default(),
        enabled: true,
        deleted: false,
        pack_owned: true,
        can_manage: true,
        can_change_version: true,
        can_update: true,
        can_remove: true,
        can_delete_file: false,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

fn server() -> ServerRow {
    ServerRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
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
        console_status: SharedString::default(),
        console_completion: 0,
        console_delivered: false,
        can_connect: false,
        connect_reason: SharedString::default(),
        restart_stage: SharedString::default(),
        needs_attention: false,
        paired_client_id: SharedString::default(),
        paired_client_name: SharedString::default(),
        pair_start_together: false,
        pair_starting: false,
        favorite: false,
        icon_slot: -1,
    }
}
