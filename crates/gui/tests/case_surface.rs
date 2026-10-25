//! An instance's Case surface: its steps, how often the bug was seen again,
//! and the note.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, CaseStepRow, InstanceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize};
use slint::{ModelRc, VecModel};

type Calls = Rc<RefCell<Vec<String>>>;

fn client(steps: &[(&str, bool)], note: &str, reproduced: i32) -> InstanceRow {
    let rows: Vec<CaseStepRow> = steps
        .iter()
        .enumerate()
        .map(|(index, (text, done))| CaseStepRow {
            id: i32::try_from(index).expect("small") + 1,
            text: (*text).into(),
            done: *done,
        })
        .collect();
    InstanceRow {
        id: "client".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        case_done: i32::try_from(steps.iter().filter(|(_, done)| *done).count()).expect("small"),
        case_steps: ModelRc::new(VecModel::from(rows)),
        case_note: note.into(),
        case_reproduced: reproduced,
        ..InstanceRow::default()
    }
}

fn shown(row: InstanceRow) -> (AppWindow, Calls) {
    let ui = support::test_window();
    support::set_minimum_size(&ui);
    let calls: Calls = Rc::default();
    let sink = Rc::clone(&calls);
    ui.on_case_add_step(move |host, text| sink.borrow_mut().push(format!("add {host} {text}")));
    let sink = Rc::clone(&calls);
    ui.on_case_set_step_done(move |host, step, done| {
        sink.borrow_mut().push(format!("done {host} {step} {done}"));
    });
    let sink = Rc::clone(&calls);
    ui.on_case_remove_step(move |host, step| {
        sink.borrow_mut().push(format!("remove {host} {step}"));
    });
    let sink = Rc::clone(&calls);
    ui.on_case_save_note(move |host, note| sink.borrow_mut().push(format!("note {host} {note}")));
    let sink = Rc::clone(&calls);
    ui.on_case_count_reproduced(move |host| sink.borrow_mut().push(format!("count {host}")));
    let sink = Rc::clone(&calls);
    ui.on_case_reset_reproduced(move |host| sink.borrow_mut().push(format!("reset {host}")));
    support::show_client(&ui, row, "Case");
    (ui, calls)
}

fn labeled(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .expect("the surface has an element with this label")
}

fn press(ui: &AppWindow, label: &str) {
    labeled(ui, label).invoke_accessible_default_action();
    slint::platform::update_timers_and_animations();
    let _ = ui;
}

#[test]
fn every_step_is_listed_with_how_many_are_done() {
    let (ui, _) = shown(client(&[("Place a belt", true), ("Fill it", false)], "", 0));

    labeled(&ui, "Place a belt");
    labeled(&ui, "Fill it");
    let progress = lookup_args(
        "gui-case-progress",
        [
            ("first", Arg::from("1".to_string())),
            ("second", Arg::from("2".to_string())),
        ],
    );
    assert_eq!(progress, "1 of 2 done");
    labeled(&ui, &progress);
}

#[test]
fn checking_a_step_asks_for_it_by_id() {
    let (ui, calls) = shown(client(
        &[("Place a belt", false), ("Fill it", false)],
        "",
        0,
    ));

    press(&ui, "Fill it");

    assert_eq!(calls.borrow().as_slice(), ["done client 2 true"]);
}

#[test]
fn removing_a_step_asks_for_it_by_id() {
    let (ui, calls) = shown(client(
        &[("Place a belt", false), ("Fill it", false)],
        "",
        0,
    ));

    press(
        &ui,
        &lookup_args(
            "gui-case-remove-step",
            [("value", Arg::from("Place a belt".to_string()))],
        ),
    );

    assert_eq!(calls.borrow().as_slice(), ["remove client 1"]);
}

#[test]
fn the_add_button_is_off_until_a_step_is_typed() {
    let (ui, calls) = shown(client(&[], "", 0));

    let add = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-case-add"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("the add button");
    assert_eq!(add.accessible_enabled(), Some(false));
    assert!(calls.borrow().is_empty());
}

#[test]
fn the_reproduced_count_is_counted_and_reset_through_its_buttons() {
    let (ui, calls) = shown(client(&[], "", 2));

    press(&ui, &lookup("gui-case-reproduced-it"));
    press(&ui, &lookup("gui-case-reset-count"));

    assert_eq!(calls.borrow().as_slice(), ["count client", "reset client"]);
}

#[test]
fn a_count_of_zero_offers_no_reset() {
    let (ui, _) = shown(client(&[], "", 0));

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-case-reset-count"))
            .next()
            .is_none()
    );
    labeled(&ui, &lookup("gui-case-reproduced-it"));
}

#[test]
fn the_note_can_be_saved_only_after_it_changed() {
    let (ui, calls) = shown(client(&[], "Only when full", 0));
    // The page header leaves the note below the fold of a minimum-size window.
    ui.window().set_size(PhysicalSize::new(1000, 1200));
    slint::platform::update_timers_and_animations();

    let save = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-case-save-note"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .expect("the save button");
    assert_eq!(save.accessible_enabled(), Some(false));
    assert!(calls.borrow().is_empty());
}

#[test]
fn a_surface_for_servers_is_not_offered() {
    assert!(
        !ephervex_gui::workspace_layout::Surface::server()
            .contains(&ephervex_gui::workspace_layout::Surface::Case)
    );
    assert_eq!(
        ephervex_gui::workspace_layout::Surface::client()[1],
        ephervex_gui::workspace_layout::Surface::Case,
        "Case follows Overview"
    );
}

/// Focuses the add-a-step field by pressing it, then types `text`.
fn type_step(ui: &AppWindow, text: &str) {
    let field = ElementHandle::find_by_accessible_label(ui, &lookup("gui-case-add-step"))
        .max_by(|a, b| a.size().width.total_cmp(&b.size().width))
        .expect("the add field");
    let at = field.absolute_position();
    let size = field.size();
    let point = LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position: point,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position: point,
        button: PointerEventButton::Left,
    });
    for character in text.chars() {
        let key = character.to_string();
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: key.clone().into(),
        });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
}

fn press_enter(ui: &AppWindow) {
    let key: slint::SharedString = Key::Return.into();
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key });
}

#[test]
fn enter_adds_the_typed_step_and_the_field_empties_once_the_step_is_listed() {
    let (ui, calls) = shown(client(&[("Place a belt", false)], "", 0));

    type_step(&ui, "Fill it");
    press_enter(&ui);
    assert_eq!(calls.borrow().as_slice(), ["add client Fill it"]);

    // Refused so far: the text stays for the user to fix.
    assert_eq!(draft(&ui), "Fill it");

    // The backend lists it.
    support::update_host(&ui, 0, |host| {
        host.client.case_steps = ModelRc::new(VecModel::from(vec![
            CaseStepRow {
                id: 1,
                text: "Place a belt".into(),
                done: false,
            },
            CaseStepRow {
                id: 2,
                text: "Fill it".into(),
                done: false,
            },
        ]));
    });
    slint::platform::update_timers_and_animations();
    assert_eq!(draft(&ui), "");
}

/// What the add-a-step field holds: the text input inside it.
fn draft(ui: &AppWindow) -> String {
    let field = ElementHandle::find_by_accessible_label(ui, &lookup("gui-case-add-step"))
        .max_by(|a, b| a.size().width.total_cmp(&b.size().width))
        .expect("the add field");
    let (left, top) = (field.absolute_position().x, field.absolute_position().y);
    let (right, bottom) = (left + field.size().width, top + field.size().height);
    ElementHandle::find_by_element_type_name(ui, "TextInput")
        .find(|input| {
            let at = input.absolute_position();
            at.x >= left && at.x < right && at.y >= top && at.y < bottom
        })
        .and_then(|input| input.accessible_value())
        .map(|value| value.to_string())
        .unwrap_or_default()
}

#[test]
fn an_unsaved_step_does_not_follow_the_pane_to_another_instance() {
    let (ui, calls) = shown(client(&[("Place a belt", false)], "", 0));
    type_step(&ui, "Only for the first");
    assert_eq!(draft(&ui), "Only for the first");

    // The pane's host picker moves the pane to another instance.
    support::update_host(&ui, 0, |host| {
        host.id = "other".into();
        host.client.id = "other".into();
        host.client.name = "Other Client".into();
    });
    slint::platform::update_timers_and_animations();

    assert_eq!(draft(&ui), "");
    assert!(calls.borrow().is_empty());
}
