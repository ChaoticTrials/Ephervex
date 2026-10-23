//! The Diagnostics surface of client and server workspaces: its states, its
//! findings, the fix preview, the failed-run notice, and what a bug report can
//! carry.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AppWindow, BundlePartRow, DiagnosticCheckRow, DiagnosticFindingRow, FixChangeRow, GuiI18n,
    InstanceRow, ServerRow, Theme,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Model as _, ModelRc, PhysicalSize, SharedString, VecModel};

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

fn finding(key: &str, problem: bool, title: &str, fix: &str, route: &str) -> DiagnosticFindingRow {
    DiagnosticFindingRow {
        key: key.into(),
        problem,
        title: title.into(),
        detail: "Required by Create.".into(),
        chain: strings(&["Create", "Flywheel"]),
        files: strings(&[]),
        more_files: SharedString::default(),
        fix_label: if fix.is_empty() {
            SharedString::default()
        } else {
            lookup(fix).into()
        },
        route: route.into(),
        route_label: if route.is_empty() {
            SharedString::default()
        } else {
            lookup("gui-diagnosis-open-mods").into()
        },
        install_label: SharedString::default(),
        install_provider: SharedString::default(),
        install_project: SharedString::default(),
    }
}

fn rows<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

/// Opens Diagnostics on the client or the server workspace with a finished
/// diagnosis: one problem a fix addresses, and one customization.
fn diagnosed_surface(ui: &AppWindow, server_host: bool, running: bool) {
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
        view.diagnosis_state = "result".into();
        view.diagnosis_summary =
            lookup_args("gui-diagnosis-problems", [("count", Arg::from(1))]).into();
        view.diagnosis_detail = "Checked at 12:00.".into();
        view.diagnosis_problems = rows(vec![finding(
            "p0",
            true,
            "Flywheel is turned off",
            "gui-diagnosis-review-fix",
            "mods",
        )]);
        view.diagnosis_customized =
            rows(vec![finding("c0", false, "Custom JVM arguments", "", "")]);
        view.diagnosis_checks = rows(vec![DiagnosticCheckRow {
            label: lookup("gui-diagnosis-check-java").into(),
            value: "21.0.8, chosen automatically.".into(),
            state: "ok".into(),
            state_label: lookup("gui-diagnosis-check-ok").into(),
        }]);
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
        assert!(enabled(&ui, "gui-diagnosis-diagnose"), "{kind}");
        assert!(
            visible(&ui, &lookup("gui-diagnosis-unchecked-title")),
            "opening Diagnostics runs nothing on its own"
        );
        assert!(
            !enabled(&ui, "gui-diagnosis-copy-report"),
            "there is nothing to report before a diagnosis"
        );
    }
}

#[test]
fn diagnosing_asks_for_the_host_the_surface_is_open_on() {
    let ui = window();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    ui.on_diagnose_host(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));
    for (server_host, kind, id) in host_kinds() {
        diagnosed_surface(&ui, server_host, false);
        button(&ui, "gui-diagnosis-diagnose-again").mock_single_click(PointerEventButton::Left);
        assert_eq!(
            asked.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );
    }
}

#[test]
fn problems_and_customizations_say_which_they_are_in_words() {
    let ui = window();
    let reviewed = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&reviewed);
    ui.on_review_fix(move |_, _, key| sink.borrow_mut().push(key.to_string()));
    for (server_host, kind, _) in host_kinds() {
        diagnosed_surface(&ui, server_host, false);
        assert!(
            visible(
                &ui,
                &format!(
                    "{}: Flywheel is turned off",
                    lookup("gui-diagnosis-badge-problem")
                )
            ),
            "{kind}"
        );
        assert!(visible(
            &ui,
            &format!(
                "{}: Custom JVM arguments",
                lookup("gui-diagnosis-badge-customized")
            )
        ));
        assert!(
            visible(
                &ui,
                &lookup_args("gui-diagnosis-requires", [("value", Arg::from("Flywheel"))])
            ),
            "the chain says why the dependency is needed"
        );
        assert!(visible(&ui, &lookup("gui-diagnosis-checked-title")));
        ElementHandle::find_by_accessible_label(
            &ui,
            &format!(
                "{}: Flywheel is turned off",
                lookup("gui-diagnosis-review-fix")
            ),
        )
        .next()
        .expect("the fix names the finding it is for")
        .mock_single_click(PointerEventButton::Left);
        assert_eq!(reviewed.borrow().last().map(String::as_str), Some("p0"));
    }
}

#[test]
fn after_a_failed_run_the_likely_cause_leads_and_customizations_fold_away() {
    let ui = window();
    diagnosed_surface(&ui, false, false);
    support::update_host(&ui, 0, |view| {
        view.diagnosis_failure = lookup("gui-run-exited-with-error").into();
    });
    settle(&ui);

    assert!(visible(&ui, &lookup("gui-diagnosis-likely-cause")));
    let customized = format!(
        "{}: Custom JVM arguments",
        lookup("gui-diagnosis-badge-customized")
    );
    assert!(
        !visible(&ui, &customized),
        "a customization does not compete with the likely cause"
    );
    ElementHandle::find_by_accessible_label(
        &ui,
        &format!(
            "{}: {}",
            lookup("gui-diagnosis-show"),
            lookup_args("gui-diagnosis-other-findings", [("count", Arg::from(1))])
        ),
    )
    .next()
    .expect("other findings can be shown")
    .mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert!(visible(&ui, &customized), "and are not hidden completely");
}

/// A missing mod the client can install carries its own install action, one
/// per finding, and the action asks for that mod through the Diagnostics
/// install, whose plan is always confirmed before it installs.
#[test]
fn each_missing_mod_offers_its_own_install_on_a_client() {
    let ui = window();
    let installs: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&installs);
    // The browser's install never hears it: that one skips the confirmation.
    ui.on_install_from_browser(|_, _, _, _| panic!("a Diagnostics install went the browser's way"));
    ui.on_install_from_diagnostics(move |_kind, instance, provider, project| {
        record.borrow_mut().push((
            instance.to_string(),
            provider.to_string(),
            project.to_string(),
        ));
    });
    diagnosed_surface(&ui, false, false);
    let missing = |key: &str, title: &str, project: &str| {
        let mut row = finding(key, true, title, "", "mods");
        row.install_label = lookup("gui-diagnosis-install-latest").into();
        row.install_provider = "modrinth".into();
        row.install_project = project.into();
        row
    };
    support::update_host(&ui, 0, |view| {
        view.diagnosis_problems = rows(vec![
            missing("p0", "LibX is not installed", "qEH6GYul"),
            missing("p1", "Balm is not installed", "MBAkmtvl"),
        ]);
    });
    settle(&ui);

    let buttons: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(
        &ui,
        &format!(
            "{}: Balm is not installed",
            lookup("gui-diagnosis-install-latest")
        ),
    )
    .collect();
    assert_eq!(buttons.len(), 1, "one install action per missing mod");
    buttons[0].mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_eq!(
        installs.borrow().as_slice(),
        [(
            "host".to_owned(),
            "modrinth".to_owned(),
            "MBAkmtvl".to_owned()
        )]
    );
}

/// A server has a catalog install too, and the same confirmation stands before
/// it. Which kind of host asked is what the install reports.
#[test]
fn each_missing_mod_offers_its_own_install_on_a_server() {
    let ui = window();
    let installs: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&installs);
    ui.on_install_from_browser(|_, _, _, _| panic!("a Diagnostics install went the browser's way"));
    ui.on_install_from_diagnostics(move |kind, id, _provider, project| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), project.to_string()));
    });
    diagnosed_surface(&ui, true, false);
    let mut row = finding("p0", true, "LibX is not installed", "", "mods");
    row.install_label = lookup("gui-diagnosis-install-latest").into();
    row.install_provider = "modrinth".into();
    row.install_project = "qEH6GYul".into();
    support::update_host(&ui, 0, |view| view.diagnosis_problems = rows(vec![row]));
    settle(&ui);

    ElementHandle::find_by_accessible_label(
        &ui,
        &format!(
            "{}: LibX is not installed",
            lookup("gui-diagnosis-install-latest")
        ),
    )
    .next()
    .expect("a missing mod on a server offers its install")
    .mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_eq!(
        installs.borrow().as_slice(),
        [(
            "server".to_owned(),
            "server".to_owned(),
            "qEH6GYul".to_owned()
        )]
    );
}

#[test]
fn a_running_diagnosis_offers_stop_and_holds_diagnose() {
    let ui = window();
    let stopped = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&stopped);
    ui.on_stop_diagnosis(move || *sink.borrow_mut() += 1);
    diagnosed_surface(&ui, false, false);
    support::update_host(&ui, 0, |view| {
        view.diagnosis_state = "working".into();
        view.diagnosis_progress = "mods/big.jar".into();
    });
    settle(&ui);
    assert!(visible(&ui, "mods/big.jar"));
    assert!(!enabled(&ui, "gui-diagnosis-diagnose-again"));
    button(&ui, "gui-diagnosis-stop").mock_single_click(PointerEventButton::Left);
    assert_eq!(*stopped.borrow(), 1);
}

#[test]
fn a_diagnosis_that_could_not_finish_says_so_apart_from_its_findings() {
    let ui = window();
    diagnosed_surface(&ui, true, false);
    support::update_host(&ui, 0, |view| {
        view.diagnosis_state = "failed".into();
        view.diagnosis_error = "The version metadata could not be read.".into();
    });
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-diagnosis-failed")));
    assert!(visible(&ui, "The version metadata could not be read."));
}

fn preview(ui: &AppWindow, blocked: &str) {
    support::update_host(ui, 0, |view| {
        view.fix_state = "ready".into();
        view.fix_title = "Flywheel is turned off".into();
        view.fix_changes = rows(vec![FixChangeRow {
            action: lookup("gui-fix-enable").into(),
            name: "Flywheel".into(),
            detail: "mods/flywheel.jar".into(),
        }]);
        view.fix_notes = strings(&[&lookup("gui-fix-enable-note")]);
        view.fix_blocked = blocked.into();
    });
    settle(ui);
}

#[test]
fn a_preview_is_the_confirmation_and_apply_waits_for_what_is_in_its_way() {
    let ui = window();
    let applied = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&applied);
    ui.on_apply_fix(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));
    for (server_host, kind, id) in host_kinds() {
        diagnosed_surface(&ui, server_host, false);
        preview(&ui, &lookup("gui-fix-blocked-running"));
        assert!(visible(&ui, &lookup("gui-fix-title")), "{kind}");
        assert!(visible(&ui, &lookup("gui-fix-enable-note")));
        assert!(visible(&ui, &lookup("gui-fix-blocked-running")));
        assert!(!enabled(&ui, "gui-fix-apply"));

        preview(&ui, "");
        assert!(enabled(&ui, "gui-fix-apply"));
        button(&ui, "gui-fix-apply").mock_single_click(PointerEventButton::Left);
        assert_eq!(
            applied.borrow().last(),
            Some(&(kind.to_owned(), id.to_owned()))
        );

        support::update_host(&ui, 0, |view| view.fix_state = "applying".into());
        settle(&ui);
        assert!(!enabled(&ui, "gui-fix-apply"));
        assert!(!enabled(&ui, "gui-fix-cancel"));
        support::update_host(&ui, 0, |view| view.fix_state = SharedString::default());
    }
}

#[test]
fn a_failed_run_is_explained_on_the_client_overview_with_a_way_to_diagnostics() {
    let ui = window();
    let requests = support::follow_surface_requests(&ui);
    client_workspace(&ui, instance());
    support::update_host(&ui, 0, |view| {
        view.run_failure_title = lookup("gui-run-crashed").into();
        view.run_failure = lookup("gui-run-exited-with-error").into();
        view.run_likely_cause = lookup_args(
            "gui-run-likely-cause",
            [("cause", Arg::from("Flywheel is turned off"))],
        )
        .into();
        view.run_crash_report = "crash-2026-09-27_12.00.00-client.txt".into();
    });
    settle(&ui);

    assert_eq!(support::surface(&ui, 0), "Overview");
    assert!(visible(&ui, &lookup("gui-run-crashed")));
    assert!(find_button(&ui, "gui-diagnosis-open-crash-report").is_some());
    button(&ui, "gui-run-open-diagnostics").mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert_eq!(
        requests.borrow().last(),
        Some(&(1, "Diagnostics".to_owned()))
    );
}

#[test]
fn a_failure_pointing_at_a_damaged_file_offers_to_verify_the_host_and_dismisses_itself() {
    let ui = window();
    let requests = support::follow_surface_requests(&ui);
    let asked = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&asked);
    ui.on_diagnose_host(move |kind, id| sink.borrow_mut().push((kind.to_string(), id.to_string())));
    let dismissed = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&dismissed);
    ui.on_dismiss_run_failure(move |kind, id| {
        sink.borrow_mut().push((kind.to_string(), id.to_string()));
    });
    let row = instance();
    let id = row.id.to_string();
    client_workspace(&ui, row);
    support::update_host(&ui, 0, |view| {
        view.run_failure_title = lookup("gui-run-failed-client").into();
        view.run_failure = "One of the native library archives could not be read.".into();
    });
    settle(&ui);
    assert!(
        find_button(&ui, "gui-run-verify-host").is_none(),
        "a failure with no stored file to blame offers no verification"
    );

    support::update_host(&ui, 0, |view| view.run_verify_suggested = true);
    settle(&ui);
    button(&ui, "gui-run-verify-host").mock_single_click(PointerEventButton::Left);
    settle(&ui);

    assert_eq!(
        asked.borrow().as_slice(),
        [("client".to_owned(), id.clone())]
    );
    assert_eq!(
        requests.borrow().last(),
        Some(&(1, "Diagnostics".to_owned()))
    );
    assert_eq!(dismissed.borrow().as_slice(), [("client".to_owned(), id)]);
}

/// A server whose run failed, as the controller shows it after a quick
/// diagnosis.
fn failed_server(ui: &AppWindow, crash_report: &str) {
    server_workspace(ui, server(false));
    support::update_host(ui, 0, |view| {
        view.run_failure_title = lookup("gui-run-failed-server").into();
        view.run_failure = lookup("gui-run-server-exited-with-error").into();
        view.run_likely_cause = lookup_args(
            "gui-run-likely-cause",
            [("cause", Arg::from("Flywheel is turned off"))],
        )
        .into();
        view.run_crash_report = crash_report.into();
    });
    settle(ui);
}

/// The failed-run notice itself, found by the title it is labeled with.
fn run_failure_notice(ui: &AppWindow, title: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, title)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Groupbox))
}

/// The surfaces the host at `index` offers, by id.
fn surface_ids(ui: &AppWindow, index: usize) -> Vec<String> {
    support::host(ui, index)
        .surfaces
        .iter()
        .map(|surface| surface.id.to_string())
        .collect()
}

#[test]
fn a_failed_run_is_shown_above_every_surface_but_diagnostics() {
    let ui = window();
    for server_host in [false, true] {
        let title = if server_host {
            failed_server(&ui, "");
            lookup("gui-run-failed-server")
        } else {
            client_workspace(&ui, instance());
            support::update_host(&ui, 0, |view| {
                view.run_failure_title = lookup("gui-run-failed-client").into();
                view.run_failure = lookup("gui-run-exited-with-error").into();
            });
            lookup("gui-run-failed-client")
        };
        let ids = surface_ids(&ui, 0);
        assert!(ids.iter().any(|id| id == "Console" || id == "Logs"));
        for id in ids {
            support::set_surface(&ui, 1, &id);
            settle(&ui);
            assert_eq!(
                run_failure_notice(&ui, &title).is_some(),
                id != "Diagnostics",
                "server: {server_host}, surface: {id}"
            );
        }
    }
}

#[test]
fn a_failed_server_run_offers_diagnostics_without_navigating() {
    let ui = window();
    let requests = support::follow_surface_requests(&ui);
    failed_server(&ui, "");
    support::set_surface(&ui, 1, "Console");
    settle(&ui);

    assert!(run_failure_notice(&ui, &lookup("gui-run-failed-server")).is_some());
    assert!(visible(&ui, &lookup("gui-run-server-exited-with-error")));
    assert!(visible(
        &ui,
        &lookup_args(
            "gui-run-likely-cause",
            [("cause", Arg::from("Flywheel is turned off"))]
        )
    ));
    // Offered only when this run wrote a crash report.
    assert!(find_button(&ui, "gui-diagnosis-open-crash-report").is_none());
    assert!(requests.borrow().is_empty());
    assert_eq!(support::surface(&ui, 0), "Console");

    button(&ui, "gui-run-open-diagnostics").mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert_eq!(
        requests.borrow().last(),
        Some(&(1, "Diagnostics".to_owned()))
    );
    assert_eq!(support::surface(&ui, 0), "Diagnostics");
}

#[test]
fn every_action_on_the_server_failure_notice_dismisses_it() {
    let report = "crash-2026-09-27_12.00.00-server.txt";
    let ui = window();
    let requests = support::follow_surface_requests(&ui);
    let shown = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&shown);
    ui.on_log_show(move |pane, mode| sink.borrow_mut().push((pane, mode.to_string())));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_log_open_file(move |pane, name| sink.borrow_mut().push((pane, name.to_string())));
    let dismissed = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&dismissed);
    ui.on_dismiss_run_failure(move |kind, id| {
        sink.borrow_mut().push((kind.to_string(), id.to_string()));
    });
    for (action, surface, mode) in [
        ("gui-run-open-diagnostics", Some("Diagnostics"), None),
        ("gui-diagnosis-open-log", Some("Logs"), Some("live")),
        (
            "gui-diagnosis-open-crash-report",
            Some("Logs"),
            Some("crashes"),
        ),
        ("gui-run-dismiss", None, None),
    ] {
        requests.borrow_mut().clear();
        shown.borrow_mut().clear();
        opened.borrow_mut().clear();
        dismissed.borrow_mut().clear();
        failed_server(&ui, report);
        support::set_surface(&ui, 1, "Console");
        settle(&ui);

        button(&ui, action).mock_single_click(PointerEventButton::Left);
        settle(&ui);
        assert_eq!(
            requests
                .borrow()
                .last()
                .map(|(pane, id)| (*pane, id.as_str())),
            surface.map(|surface| (1, surface)),
            "{action}"
        );
        assert_eq!(
            shown
                .borrow()
                .last()
                .map(|(pane, mode)| (*pane, mode.as_str())),
            mode.map(|mode| (1, mode)),
            "{action}"
        );
        let expected_open: &[(i32, String)] = if action == "gui-diagnosis-open-crash-report" {
            &[(1, report.to_owned())]
        } else {
            &[]
        };
        assert_eq!(opened.borrow().as_slice(), expected_open, "{action}");
        assert_eq!(
            dismissed.borrow().as_slice(),
            [("server".to_owned(), "server".to_owned())],
            "{action}"
        );
    }
}

/// Two server Overviews side by side on the smallest window: each notice's
/// actions stay inside their own pane.
#[test]
fn the_server_failure_notice_stays_inside_its_pane_at_the_smallest_split() {
    let ui = window();
    support::set_minimum_width(&ui, 3000);
    for scale in [1.0, 1.5] {
        ui.global::<Theme>().set_text_scale(scale);
        failed_server(&ui, "crash-2026-09-27_12.00.00-server.txt");
        let pane = |id: i32, column: i32| {
            let mut pane = support::pane(id, 0, "Overview");
            pane.column = column;
            pane.can_split_beside = false;
            pane.can_close = true;
            pane
        };
        ui.set_workspace_panes(rows(vec![pane(1, 0), pane(2, 1)]));
        settle(&ui);
        let width = ui
            .window()
            .size()
            .to_logical(ui.window().scale_factor())
            .width;
        for message in [
            "gui-run-open-diagnostics",
            "gui-diagnosis-open-crash-report",
            "gui-diagnosis-open-log",
            "gui-run-dismiss",
        ] {
            let mut placed: Vec<(f32, f32)> =
                ElementHandle::find_by_accessible_label(&ui, &lookup(message))
                    .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
                    .map(|element| {
                        let x = element.absolute_position().x;
                        (x, x + element.size().width)
                    })
                    .collect();
            placed.sort_by(|left, right| left.0.total_cmp(&right.0));
            assert_eq!(placed.len(), 2, "at {scale}: one {message} per pane");
            assert!(
                placed[0].1 <= placed[1].0,
                "at {scale}: {message} in the left pane reaches the right one: {placed:?}"
            );
            assert!(
                placed[1].1 <= width + 0.5,
                "at {scale}: {message} leaves the window: {placed:?}"
            );
        }
    }
}

#[test]
fn diagnostics_links_to_the_logs_surface() {
    let ui = window();
    let requests = support::follow_surface_requests(&ui);
    for (server_host, _, _) in host_kinds() {
        diagnosed_surface(&ui, server_host, false);
        button(&ui, "gui-integrity-open-logs").mock_single_click(PointerEventButton::Left);
        settle(&ui);
        assert_eq!(requests.borrow().last(), Some(&(1, "Logs".to_owned())));
        assert_eq!(support::surface(&ui, 0), "Logs");
    }
}

/// Two panes side by side on the smallest window: each pane is at its minimum
/// width, and nothing in the left one may reach into the right one.
#[test]
fn every_action_stays_inside_its_pane_at_the_smallest_split() {
    let ui = window();
    // Only as wide as the window may be, and tall enough that a 1.5x surface
    // is still on screen for the testing backend to find.
    support::set_minimum_width(&ui, 3000);
    for scale in [1.0, 1.5] {
        ui.global::<Theme>().set_text_scale(scale);
        for (server_host, kind, _) in host_kinds() {
            diagnosed_surface(&ui, server_host, false);
            preview(&ui, "");
            let pane = |id: i32, column: i32| {
                let mut pane = support::pane(id, 0, "Diagnostics");
                pane.column = column;
                pane.can_split_beside = false;
                pane.can_close = true;
                pane
            };
            ui.set_workspace_panes(rows(vec![pane(1, 0), pane(2, 1)]));
            settle(&ui);
            let width = ui
                .window()
                .size()
                .to_logical(ui.window().scale_factor())
                .width;
            for message in [
                lookup("gui-diagnosis-diagnose-again"),
                format!(
                    "{}: Flywheel is turned off",
                    lookup("gui-diagnosis-review-fix")
                ),
                lookup("gui-fix-apply"),
                lookup("gui-diagnosis-copy-report"),
                lookup("gui-diagnosis-bundle-show"),
            ] {
                let mut placed: Vec<(f32, f32)> =
                    ElementHandle::find_by_accessible_label(&ui, &message)
                        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
                        .map(|element| {
                            let x = element.absolute_position().x;
                            (x, x + element.size().width)
                        })
                        .collect();
                placed.sort_by(|left, right| left.0.total_cmp(&right.0));
                assert_eq!(placed.len(), 2, "{kind} at {scale}: one {message} per pane");
                assert!(
                    placed[0].1 <= placed[1].0,
                    "{kind} at {scale}: {message} in the left pane reaches the right one: {placed:?}"
                );
                assert!(
                    placed[1].1 <= width + 0.5,
                    "{kind} at {scale}: {message} leaves the window: {placed:?}"
                );
            }
        }
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

/// The bundle sits behind its own disclosure: a readable report comes first.
fn open_bundle(ui: &AppWindow) {
    assert!(
        !visible(ui, &lookup("gui-bundle-privacy")),
        "the bundle is not the primary result"
    );
    button(ui, "gui-diagnosis-bundle-show").mock_single_click(PointerEventButton::Left);
    settle(ui);
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
        diagnosed_surface(&ui, server_host, false);
        support::update_host(&ui, 0, |view| {
            view.bundle_parts = bundle_rows(true);
            view.bundle_selected_count = 1;
        });
        settle(&ui);
        open_bundle(&ui);

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
        button(&ui, "gui-diagnosis-bundle-hide").mock_single_click(PointerEventButton::Left);
        settle(&ui);
    }
}

#[test]
fn an_exported_bundle_says_where_it_went_and_can_be_shown() {
    let ui = window();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_open_bundle_folder(move |path| sink.borrow_mut().push(path.to_string()));
    for (server_host, _, _) in host_kinds() {
        diagnosed_surface(&ui, server_host, false);
        support::update_host(&ui, 0, |view| {
            view.bundle_parts = bundle_rows(true);
            view.bundle_selected_count = 1;
            view.bundle_result = "Saved one file to /home/user/bundle.zip.".into();
            view.bundle_result_path = "/home/user/bundle.zip".into();
        });
        settle(&ui);
        open_bundle(&ui);
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
        button(&ui, "gui-diagnosis-bundle-hide").mock_single_click(PointerEventButton::Left);
        settle(&ui);
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
        favorite: false,
        group_id: slint::SharedString::default(),
        group_name: slint::SharedString::default(),
        group_index: 0,
        group_tone: 0,
        icon_slot: -1,
        problems: 0,
        state_line: "".into(),
        last_run: "".into(),
        open: false,
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
        restart_required: false,
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
        favorite: false,
        icon_slot: -1,
        problems: 0,
        state_line: "".into(),
        last_run: "".into(),
        open: false,
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
    // the findings.
    ui.window().set_size(PhysicalSize::new(1800, 1400));
    ui.show().expect("show");
    ui
}
