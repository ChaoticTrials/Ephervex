//! The isolation dialog exposes every durable state and exact test configuration.
#![allow(clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, IsolationCandidateRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
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
    ElementHandle::find_by_accessible_label(ui, &lookup(message))
        .find(|element| element.accessible_enabled().is_some())
        .unwrap_or_else(|| panic!("no button for {message}"))
}

#[test]
fn setup_requires_the_explicit_unrecorded_dependency_override() {
    let ui = window();
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
    ui.set_isolation_unrecorded(strings(&["mods/a.jar"]));
    ui.set_modal("isolate".into());

    assert!(visible(&ui, "A"));
    assert!(visible(&ui, "mods/b.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-accept-unrecorded")));
    assert!(
        !button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );

    ui.set_isolation_accept_unrecorded(true);
    assert!(
        button(&ui, "gui-isolation-start")
            .accessible_enabled()
            .expect("the start button reports whether it is enabled")
    );
}

#[test]
fn active_round_names_the_exact_configuration_and_records_either_answer() {
    let ui = window();
    ui.set_isolation_phase("round".into());
    ui.set_isolation_round(4);
    ui.set_isolation_enabled(strings(&["mods/a.jar", "mods/shared.jar"]));
    ui.set_isolation_disabled(strings(&["mods/b.jar"]));
    ui.set_isolation_host_kind("client".into());
    ui.set_isolation_host_id("host".into());
    ui.set_modal("isolate".into());

    assert!(visible(&ui, "mods/a.jar"));
    assert!(visible(&ui, "mods/shared.jar"));
    assert!(visible(&ui, "mods/b.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-test-instruction")));
    let enabled_rows = ["mods/a.jar", "mods/shared.jar"].map(|label| {
        ElementHandle::find_by_accessible_label(&ui, label)
            .next()
            .unwrap_or_else(|| panic!("no isolation row for {label}"))
    });
    assert!(
        enabled_rows[1].absolute_position().y
            >= enabled_rows[0].absolute_position().y + enabled_rows[0].size().height,
        "enabled isolation rows must not overlap"
    );

    let answer = Rc::new(RefCell::new(String::new()));
    let recorded = Rc::clone(&answer);
    ui.on_answer_isolation(move |_, _, value| *recorded.borrow_mut() = value.into());
    button(&ui, "gui-isolation-issue-gone").mock_single_click(PointerEventButton::Left);
    assert_eq!(&*answer.borrow(), "gone");
    button(&ui, "gui-isolation-issue-present").mock_single_click(PointerEventButton::Left);
    assert_eq!(&*answer.borrow(), "present");
}

#[test]
fn contradiction_and_result_views_offer_the_safe_next_actions() {
    let ui = window();
    ui.set_isolation_phase("contradiction".into());
    ui.set_modal("isolate".into());
    assert!(visible(&ui, &lookup("gui-isolation-contradiction")));
    assert!(
        button(&ui, "gui-isolation-undo")
            .accessible_enabled()
            .expect("the undo button reports whether it is enabled")
    );

    ui.set_isolation_phase("result".into());
    ui.set_isolation_summary("One candidate root remains.".into());
    ui.set_isolation_reproducing(strings(&["mods/a.jar"]));
    assert!(visible(&ui, &lookup("gui-isolation-finished")));
    assert!(visible(&ui, "mods/a.jar"));
    assert!(visible(&ui, &lookup("gui-isolation-finished-detail")));
}

#[test]
fn an_active_session_stays_visible_after_the_modal_closes_and_can_be_resumed() {
    let ui = window();
    ui.set_page(0);
    ui.set_workspace("instance".into());
    ui.set_selected_instance_id("host".into());
    ui.set_isolation_host_kind("client".into());
    ui.set_isolation_host_id("host".into());
    ui.set_isolation_active(true);
    ui.set_isolation_phase("round".into());
    ui.set_isolation_summary("Waiting for an answer to round 3.".into());

    assert!(visible(&ui, &lookup("gui-isolation-active")));
    assert!(visible(&ui, "Waiting for an answer to round 3."));

    let resumed = Rc::new(RefCell::new((String::new(), String::new())));
    let recorded = Rc::clone(&resumed);
    ui.on_resume_isolation(move |kind, id| {
        *recorded.borrow_mut() = (kind.into(), id.into());
    });
    button(&ui, "gui-isolation-resume").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        &*resumed.borrow(),
        &("client".to_owned(), "host".to_owned())
    );
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
