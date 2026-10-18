//! The isolation surface and its persistent host-workspace actions.
#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{
    AppWindow, GuiI18n, InstanceRow, IsolationCandidateRow, ServerRow, WorkflowActionRow,
};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, ElementQuery, TestingBackend, TestingBackendOptions,
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
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "setup".into());
    support::update_host(&ui, 0, |view| {
        view.isolation_summary = "Two candidate roots can be tested.".into();
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_candidates = ModelRc::new(VecModel::from(vec![
            IsolationCandidateRow {
                path: "mods/a.jar".into(),
                display_path: "a.jar".into(),
                name: "A".into(),
                selected: false,
            },
            IsolationCandidateRow {
                path: "mods/b.jar".into(),
                display_path: "b.jar".into(),
                name: "B".into(),
                selected: true,
            },
        ]));
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_unrecorded = strings(&["mods/unrecorded.jar"]);
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_unidentifiable = strings(&["mods/unidentified.jar"]);
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_missing = strings(&["mods/b.jar: required-library"]);
    });
    settle(&ui);

    assert!(visible(&ui, "A"));
    assert!(visible(&ui, "b.jar"));
    assert!(!visible(&ui, "mods/b.jar"));
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

    support::update_host(&ui, 0, |view| view.isolation_accept_unrecorded = true);
    assert!(
        !button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );
    support::update_host(&ui, 0, |view| view.isolation_accept_missing = true);
    assert!(
        button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );
}

/// Opens a setup on the client or the server workspace, the two surfaces that
/// share the Isolation setup.
fn setup_on(ui: &AppWindow, server_host: bool, running: bool) {
    if server_host {
        server_workspace(ui, server(running));
    } else {
        let mut row = instance();
        row.can_kill = running;
        row.can_launch = !running;
        client_workspace_with(ui, row);
    }
    support::set_surface(ui, 1, "Isolation");
    support::update_host(ui, 0, |view| view.isolation_phase = "setup".into());
    support::update_host(ui, 0, |view| {
        view.isolation_summary = "261 candidate roots can be tested.".into();
    });
    support::update_host(ui, 0, |view| view.isolation_missing = strings(&[]));
    support::update_host(ui, 0, |view| view.isolation_unrecorded = strings(&[]));
    support::update_host(ui, 0, |view| view.isolation_accept_missing = false);
    support::update_host(ui, 0, |view| view.isolation_accept_unrecorded = false);
    ui.set_host_busy_kind("".into());
    ui.set_host_busy_id("".into());
    settle(ui);
}

fn start_enabled(ui: &AppWindow) -> bool {
    button(ui, "gui-isolation-start")
        .accessible_enabled()
        .expect("the start button reports whether it is enabled")
}

fn blockers_shown(ui: &AppWindow) -> Vec<&'static str> {
    [
        "gui-isolation-start-blocked-running",
        "gui-isolation-start-blocked-missing",
        "gui-isolation-start-blocked-unrecorded",
    ]
    .into_iter()
    .filter(|message| visible(ui, &lookup(message)))
    .collect()
}

/// Every text shown between the given acknowledgement and the Start row, where
/// the setup tells the user what Start is waiting for.
fn notes_beside_start(ui: &AppWindow, acknowledgement: &str) -> Vec<String> {
    let label = lookup(acknowledgement);
    let top = ElementHandle::find_by_accessible_label(ui, &label)
        .map(|element| element.absolute_position().y + element.size().height)
        .fold(f32::MIN, f32::max);
    let start = button(ui, "gui-isolation-start");
    let cancel = button(ui, "gui-cancel");
    let (left, right) = (
        cancel.absolute_position().x,
        start.absolute_position().x + start.size().width,
    );
    ElementQuery::from_root(ui)
        .match_accessible_role(AccessibleRole::Text)
        .find_all()
        .into_iter()
        .filter(|text| {
            let (position, size) = (text.absolute_position(), text.size());
            size.width > 0.0
                && position.y >= top
                && position.y + size.height <= start.absolute_position().y
                && position.x >= left
                && position.x + size.width <= right
        })
        .filter_map(|text| text.accessible_label().map(|label| label.to_string()))
        .filter(|label| !label.is_empty())
        .collect()
}

/// The switch for an acknowledgement, which must be on screen and usable.
fn acknowledgement(ui: &AppWindow, message: &str) -> ElementHandle {
    let label = lookup(message);
    let switch = ElementHandle::find_by_accessible_label(ui, &label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Checkbox))
        .unwrap_or_else(|| panic!("no visible switch for {message}"));
    let start = button(ui, "gui-isolation-start");
    let panel_right = start.absolute_position().x + start.size().width;
    assert!(
        switch.absolute_position().x + switch.size().width <= panel_right,
        "the switch for {message} is outside the setup panel"
    );
    assert!(
        switch.absolute_position().y + switch.size().height <= start.absolute_position().y,
        "the switch for {message} is not above Start"
    );
    assert_eq!(switch.accessible_enabled(), Some(true));
    switch
}

#[test]
fn start_is_enabled_for_a_setup_with_nothing_to_acknowledge() {
    let ui = window();
    for server_host in [false, true] {
        setup_on(&ui, server_host, false);
        assert!(start_enabled(&ui), "server: {server_host}");
        assert!(blockers_shown(&ui).is_empty());
    }
}

#[test]
fn unrecorded_dependency_information_waits_for_a_visible_acknowledgement() {
    let ui = window();
    for server_host in [false, true] {
        setup_on(&ui, server_host, false);
        let many: Vec<String> = (0..60).map(|index| format!("mod-{index}.jar")).collect();
        support::update_host(&ui, 0, |view| {
            view.isolation_unrecorded =
                strings(&many.iter().map(String::as_str).collect::<Vec<_>>());
        });
        settle(&ui);

        assert!(!start_enabled(&ui), "server: {server_host}");
        assert_eq!(
            blockers_shown(&ui),
            ["gui-isolation-start-blocked-unrecorded"]
        );
        acknowledgement(&ui, "gui-isolation-accept-unrecorded").invoke_accessible_default_action();
        settle(&ui);

        assert!(support::host(&ui, 0).isolation_accept_unrecorded);
        assert!(start_enabled(&ui), "server: {server_host}");
        assert!(blockers_shown(&ui).is_empty());
    }
}

#[test]
fn the_acknowledgement_label_is_part_of_its_switch() {
    let ui = window();
    setup_on(&ui, false, false);
    support::update_host(&ui, 0, |view| {
        view.isolation_unrecorded = strings(&["unrecorded.jar"]);
    });
    settle(&ui);
    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-isolation-accept-unrecorded"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Text))
        .expect("the acknowledgement's label is shown")
        .mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert!(support::host(&ui, 0).isolation_accept_unrecorded);
    assert!(start_enabled(&ui));
}

#[test]
fn missing_dependencies_block_start_with_their_own_reason_until_acknowledged() {
    let ui = window();
    for server_host in [false, true] {
        setup_on(&ui, server_host, false);
        support::update_host(&ui, 0, |view| {
            view.isolation_missing = strings(&["feature.jar: library"]);
        });
        support::update_host(&ui, 0, |view| {
            view.isolation_unrecorded = strings(&["unrecorded.jar"]);
        });
        settle(&ui);
        assert_eq!(blockers_shown(&ui), ["gui-isolation-start-blocked-missing"]);

        acknowledgement(&ui, "gui-isolation-accept-unrecorded").invoke_accessible_default_action();
        settle(&ui);
        assert!(!start_enabled(&ui), "server: {server_host}");
        assert_eq!(blockers_shown(&ui), ["gui-isolation-start-blocked-missing"]);

        acknowledgement(&ui, "gui-isolation-accept-missing").invoke_accessible_default_action();
        settle(&ui);
        assert!(start_enabled(&ui), "server: {server_host}");
        assert!(blockers_shown(&ui).is_empty());
    }
}

#[test]
fn a_linked_servers_gaps_wait_for_an_acknowledgement_only_once_it_joins_the_session() {
    let ui = window();
    setup_on(&ui, false, false);
    support::update_host(&ui, 0, |view| {
        view.isolation_linked_servers = strings(&["Linked (server-1)"]);
        view.isolation_linked_unrecorded = strings(&["Linked: unrecorded.jar"]);
    });
    settle(&ui);
    assert!(start_enabled(&ui), "a server that stays out adds no gap");
    assert!(!visible(&ui, "Linked: unrecorded.jar"));

    acknowledgement(&ui, "gui-isolation-sync-linked").invoke_accessible_default_action();
    settle(&ui);

    assert!(support::host(&ui, 0).isolation_sync_linked_servers);
    assert!(visible(&ui, "Linked: unrecorded.jar"));
    assert!(!start_enabled(&ui));
    assert_eq!(
        blockers_shown(&ui),
        ["gui-isolation-start-blocked-unrecorded"]
    );
    acknowledgement(&ui, "gui-isolation-accept-unrecorded").invoke_accessible_default_action();
    settle(&ui);
    assert!(start_enabled(&ui));
}

#[test]
fn starting_passes_the_choice_to_keep_linked_servers_in_step() {
    let ui = window();
    setup_on(&ui, false, false);
    support::update_host(&ui, 0, |view| {
        view.isolation_linked_servers = strings(&["Linked (server-1)"]);
        view.isolation_sync_linked_servers = true;
    });
    settle(&ui);
    let started = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&started);
    ui.on_start_isolation(move |_, _, unrecorded, missing, linked| {
        *sink.borrow_mut() = Some((unrecorded, missing, linked));
    });

    button(&ui, "gui-isolation-start").mock_single_click(PointerEventButton::Left);

    assert_eq!(*started.borrow(), Some((false, false, true)));
}

#[test]
fn a_round_names_what_each_followed_server_is_set_to() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| {
        view.isolation_phase = "round".into();
        view.isolation_followed_servers = strings(&["Linked: 3 enabled, 5 disabled"]);
    });
    settle(&ui);

    assert!(visible(&ui, &lookup("gui-isolation-followed-servers")));
    assert!(visible(&ui, "Linked: 3 enabled, 5 disabled"));
}

#[test]
fn a_result_names_the_server_mods_no_round_tested() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| {
        view.isolation_phase = "result".into();
        view.isolation_followed_servers = strings(&["Linked: 1 enabled, 2 disabled"]);
        view.isolation_untested_server_mods = strings(&["Linked: serveronly.jar"]);
    });
    settle(&ui);

    assert!(visible(&ui, &lookup("gui-isolation-untested-server-mods")));
    assert!(visible(&ui, "Linked: serveronly.jar"));
}

#[test]
fn a_server_owned_by_a_client_session_cannot_start_a_setup_and_names_the_client() {
    let ui = window();
    server_workspace(&ui, server(false));
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| {
        view.isolation_phase = "none".into();
        view.isolation_led_by = "Main Client".into();
    });
    settle(&ui);

    assert!(visible(
        &ui,
        &lookup_args(
            "gui-isolation-led-by",
            [("value", Arg::from("Main Client"))]
        )
    ));
    assert!(
        !button(&ui, "gui-isolation-start-setup")
            .accessible_enabled()
            .expect("the setup button reports whether it is enabled")
    );
}

#[test]
fn a_running_host_disables_start_and_says_to_stop_it() {
    let ui = window();
    for server_host in [false, true] {
        setup_on(&ui, server_host, true);
        assert!(!start_enabled(&ui), "server: {server_host}");
        assert_eq!(blockers_shown(&ui), ["gui-isolation-start-blocked-running"]);
    }
}

#[test]
fn a_start_in_progress_disables_start_and_shows_only_its_progress() {
    let ui = window();
    for server_host in [false, true] {
        setup_on(&ui, server_host, false);
        support::update_host(&ui, 0, |view| {
            view.isolation_unrecorded = strings(&["unrecorded.jar"]);
        });
        settle(&ui);
        assert_eq!(
            notes_beside_start(&ui, "gui-isolation-accept-unrecorded"),
            [lookup("gui-isolation-start-blocked-unrecorded")]
        );
        support::update_host(&ui, 0, |view| view.isolation_accept_unrecorded = true);
        let (kind, id) = if server_host {
            ("server", "server")
        } else {
            ("client", "host")
        };
        ui.set_host_busy_kind(kind.into());
        ui.set_host_busy_id(id.into());
        ui.set_host_busy_text("Creating the safety snapshot".into());
        settle(&ui);

        assert!(!start_enabled(&ui), "server: {server_host}");
        assert!(
            !button(&ui, "gui-cancel")
                .accessible_enabled()
                .expect("the cancel button reports whether it is enabled")
        );
        assert!(visible(&ui, "Creating the safety snapshot"));
        assert!(blockers_shown(&ui).is_empty(), "server: {server_host}");
        assert_eq!(
            notes_beside_start(&ui, "gui-isolation-accept-unrecorded"),
            Vec::<String>::new(),
            "server: {server_host}"
        );

        ui.set_host_busy_kind("".into());
        ui.set_host_busy_id("".into());
        settle(&ui);
        assert!(start_enabled(&ui), "server: {server_host}");
        assert!(!visible(&ui, "Creating the safety snapshot"));
    }
}

#[test]
fn canceling_setup_stays_on_the_isolation_tab() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "setup".into());
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
    assert_eq!(support::surface(&ui, 0), "Isolation");
}

#[test]
fn detailed_round_names_the_exact_configuration_and_records_either_answer() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "round".into());
    support::update_host(&ui, 0, |view| view.isolation_round = 4);
    support::update_host(&ui, 0, |view| {
        view.isolation_enabled = strings(&["mods/a.jar", "mods/shared.jar"]);
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_disabled = strings(&["mods/b.jar"]);
    });
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
fn a_busy_isolation_change_disables_answers_and_launch_and_says_what_it_is_doing() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "round".into());
    support::update_host(&ui, 0, |view| {
        view.isolation_history = strings(&["Round 1"]);
    });
    ui.set_host_busy_kind("client".into());
    ui.set_host_busy_id("host".into());
    ui.set_host_busy_text("Applying the next mod setup".into());
    settle(&ui);

    assert!(visible(&ui, "Applying the next mod setup"));
    for message in [
        "gui-isolation-issue-gone",
        "gui-isolation-issue-present",
        "gui-isolation-undo",
        "gui-isolation-cancel",
        "gui-launch",
    ] {
        assert!(
            !button(&ui, message)
                .accessible_enabled()
                .expect("the button reports whether it is enabled"),
            "{message} stays enabled while isolation changes the host"
        );
    }

    ui.set_host_busy_kind("client".into());
    ui.set_host_busy_id("another".into());
    settle(&ui);
    assert!(!visible(&ui, "Applying the next mod setup"));
    assert!(
        button(&ui, "gui-isolation-issue-present")
            .accessible_enabled()
            .expect("the button reports whether it is enabled")
    );
}

#[test]
fn a_round_awaiting_recovery_blocks_answers_and_offers_only_a_retry() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "round".into());
    support::update_host(&ui, 0, |view| {
        view.isolation_history = strings(&["Round 1"]);
    });
    support::update_host(&ui, 0, |view| view.isolation_recovery = "failed".into());
    support::update_host(&ui, 0, |view| {
        view.isolation_recovery_detail = "The jar is missing.".into();
    });
    settle(&ui);

    assert!(visible(&ui, &lookup("gui-isolation-recovery-failed")));
    assert!(visible(&ui, "The jar is missing."));
    assert!(!visible(&ui, &lookup("gui-isolation-test-instruction")));
    for message in [
        "gui-isolation-issue-gone",
        "gui-isolation-issue-present",
        "gui-isolation-undo",
    ] {
        assert!(
            !button(&ui, message)
                .accessible_enabled()
                .expect("the button reports whether it is enabled"),
            "{message} is offered before the round's mods are back"
        );
    }
    assert!(
        button(&ui, "gui-isolation-cancel")
            .accessible_enabled()
            .expect("the button reports whether it is enabled")
    );
    let retried = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&retried);
    ui.on_retry_isolation_recovery(move |kind, id| {
        sink.borrow_mut().push((kind.to_string(), id.to_string()));
    });
    button(&ui, "gui-isolation-recovery-retry").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        retried.borrow().as_slice(),
        &[("client".to_owned(), "host".to_owned())]
    );

    support::update_host(&ui, 0, |view| view.isolation_recovery = "running".into());
    support::update_host(&ui, 0, |view| view.isolation_recovery_detail = "".into());
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-isolation-recovery-running")));

    support::update_host(&ui, 0, |view| view.isolation_recovery = "".into());
    settle(&ui);
    assert!(!visible(&ui, &lookup("gui-isolation-recovery-retry")));
    assert!(visible(&ui, &lookup("gui-isolation-test-instruction")));
    assert!(
        button(&ui, "gui-isolation-issue-present")
            .accessible_enabled()
            .expect("the button reports whether it is enabled")
    );
}

#[test]
fn no_manual_round_restore_action_is_offered() {
    assert!(!ephervex_i18n::has("gui-isolation-reapply"));
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "round".into());
    settle(&ui);
    assert!(!visible(&ui, "Restore this round's mod setup"));
    assert!(!visible(&ui, &lookup("gui-isolation-recovery-retry")));
}

#[test]
fn isolation_state_survives_logs_and_mods_navigation() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "round".into());
    support::update_host(&ui, 0, |view| view.isolation_round = 3);
    support::update_host(&ui, 0, |view| {
        view.isolation_enabled = strings(&["mods/persistent.jar"]);
    });
    settle(&ui);

    tab(&ui, &lookup("gui-instance-tab-logs")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(support::surface(&ui, 0), "Logs");
    assert_eq!(support::host(&ui, 0).isolation_round, 3);

    tab(&ui, &lookup("gui-isolation-tab")).invoke_accessible_default_action();
    settle(&ui);
    assert!(visible(&ui, "mods/persistent.jar"));

    tab(&ui, &lookup("gui-instance-tab-mods")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(support::surface(&ui, 0), "Mods");

    tab(&ui, &lookup("gui-isolation-tab")).invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(support::host(&ui, 0).isolation_round, 3);
    assert!(visible(&ui, "mods/persistent.jar"));
}

/// The workflow area offers the round's own actions above every surface, and
/// hands each to the controller with the pane and host it was pressed for.
#[test]
fn persistent_round_actions_name_their_pane_and_host() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Logs");
    support::update_host(&ui, 0, |view| view.workflow_id = "mod-isolation".into());
    support::update_host(&ui, 0, |view| {
        view.workflow_title = lookup("gui-isolation-active").into();
    });
    support::update_host(&ui, 0, |view| {
        view.workflow_summary = "Waiting for an answer to round 2.".into();
    });
    support::update_host(&ui, 0, |view| {
        view.workflow_actions = ModelRc::new(VecModel::from(vec![
            workflow_action("details", "gui-isolation-detailed-view", false),
            workflow_action("gone", "gui-isolation-issue-gone", true),
            workflow_action("present", "gui-isolation-issue-present", true),
            workflow_action("undo", "gui-isolation-undo", true),
        ]));
    });
    settle(&ui);

    let recorded = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&recorded);
    ui.on_workflow_action(move |pane, kind, id, workflow, action| {
        sink.borrow_mut()
            .push(format!("{pane} {kind} {id} {workflow} {action}"));
    });
    button(&ui, "gui-isolation-issue-gone").mock_single_click(PointerEventButton::Left);
    button(&ui, "gui-isolation-detailed-view").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        recorded.borrow().as_slice(),
        [
            "1 client host mod-isolation gone".to_owned(),
            "1 client host mod-isolation details".to_owned(),
        ]
    );
}

#[test]
fn result_separates_testable_candidates_from_untested_pins_and_dependencies() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    support::update_host(&ui, 0, |view| view.isolation_phase = "contradiction".into());
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-isolation-contradiction")));
    assert!(
        button(&ui, "gui-isolation-undo")
            .accessible_enabled()
            .expect("the undo button reports whether it is enabled")
    );

    support::update_host(&ui, 0, |view| view.isolation_phase = "result".into());
    support::update_host(&ui, 0, |view| {
        view.isolation_summary = "One testable candidate root remains.".into();
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_remaining_candidates = strings(&["mods/advancedae.jar"]);
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_pinned = strings(&["mods/libx.jar", "mods/skyblock-builder.jar"]);
    });
    support::update_host(&ui, 0, |view| {
        view.isolation_required_dependencies = strings(&["mods/required-library.jar"]);
    });
    settle(&ui);
    assert!(visible(&ui, &lookup("gui-isolation-finished")));
    assert!(visible(&ui, &lookup("gui-isolation-remaining-candidates")));
    assert!(visible(&ui, "mods/advancedae.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-pinned-result")));
    assert!(visible(&ui, "mods/libx.jar"));
    assert!(visible(&ui, "mods/skyblock-builder.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-pinned-result-detail")));
    assert!(visible(&ui, &lookup("gui-isolation-result-dependencies")));
    assert!(visible(&ui, "mods/required-library.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-finished-detail")));
}

#[test]
fn host_actions_remain_available_for_client_and_server_workspaces() {
    let ui = window();
    client_workspace(&ui);
    support::set_surface(&ui, 1, "Isolation");
    settle(&ui);

    let client_launches = Rc::new(RefCell::new(Vec::new()));
    let launch_sink = Rc::clone(&client_launches);
    ui.on_launch_client(move |id| launch_sink.borrow_mut().push(id.to_string()));
    button(&ui, "gui-launch").mock_single_click(PointerEventButton::Left);
    assert_eq!(client_launches.borrow().as_slice(), ["host".to_owned()]);

    server_workspace(&ui, server(false));
    support::set_surface(&ui, 1, "Isolation");
    settle(&ui);
    let starts = Rc::new(RefCell::new(Vec::new()));
    let start_sink = Rc::clone(&starts);
    ui.on_server_start(move |id| start_sink.borrow_mut().push(id.to_string()));
    button(&ui, "gui-start").mock_single_click(PointerEventButton::Left);
    assert_eq!(starts.borrow().as_slice(), ["server".to_owned()]);

    server_workspace(&ui, server(true));
    support::set_surface(&ui, 1, "Isolation");
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
    client_workspace_with(ui, instance());
}

fn client_workspace_with(ui: &AppWindow, row: InstanceRow) {
    support::show_client(ui, row, "Overview");
    support::follow_surface_requests(ui);
    support::follow_acknowledgements(ui);
    settle(ui);
}

fn server_workspace(ui: &AppWindow, row: ServerRow) {
    support::show_server(ui, row, "Overview");
    support::follow_surface_requests(ui);
    support::follow_acknowledgements(ui);
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
