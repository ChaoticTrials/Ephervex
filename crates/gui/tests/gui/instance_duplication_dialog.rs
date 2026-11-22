//! Duplicating an instance from the desktop app.
//!
//! A copy of a persistent instance is usually made to try something
//! disposable, so the dialog asks where the copy lives and starts on
//! temporary.

#![allow(clippy::panic)]

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::AppWindow;
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

/// What the dialog answered: the instance, whether absent mods are left out,
/// and whether the copy is persistent.
type Answers = Rc<RefCell<Vec<(String, bool, bool)>>>;

#[test]
fn a_persistent_instance_is_copied_as_temporary_unless_the_user_picks_persistent() {
    let ui = support::test_window();
    let answers = record_answers(&ui);
    open(&ui, true, &[]);

    assert!(asks_for_storage(&ui), "the storage is asked for");
    assert!(!ui.get_duplicate_persistent());
    press(&ui, "gui-duplicate-instance");

    assert_eq!(
        answers.borrow().as_slice(),
        [("repro-client".to_owned(), false, false)]
    );
}

#[test]
fn choosing_persistent_makes_a_persistent_copy() {
    let ui = support::test_window();
    let answers = record_answers(&ui);
    open(&ui, true, &[]);

    ui.set_duplicate_persistent(true);
    press(&ui, "gui-duplicate-instance");

    assert_eq!(
        answers.borrow().as_slice(),
        [("repro-client".to_owned(), false, true)]
    );
}

#[test]
fn both_questions_are_answered_in_one_dialog() {
    let ui = support::test_window();
    let answers = record_answers(&ui);
    open(&ui, true, &["Absent Mod"]);

    assert!(asks_for_storage(&ui));
    assert!(
        widest(&ui, &lookup("gui-duplicate-instance")).is_none(),
        "the absent-mods answer is the button"
    );
    press(&ui, "gui-copy-without-them");

    assert_eq!(
        answers.borrow().as_slice(),
        [("repro-client".to_owned(), true, false)]
    );
}

#[test]
fn a_temporary_instance_asks_only_about_absent_mods() {
    let ui = support::test_window();
    let answers = record_answers(&ui);
    open(&ui, false, &["Absent Mod"]);

    assert!(
        !asks_for_storage(&ui),
        "a copy of a temporary instance is temporary"
    );
    press(&ui, "gui-copy-with-them");

    assert_eq!(
        answers.borrow().as_slice(),
        [("repro-client".to_owned(), false, false)]
    );
}

fn open(ui: &AppWindow, persistent: bool, absent: &[&str]) {
    let names: Vec<SharedString> = absent.iter().map(|name| (*name).into()).collect();
    ui.set_absent_mods(ModelRc::new(VecModel::from(names)));
    ui.set_duplicate_source_persistent(persistent);
    ui.set_duplicate_persistent(false);
    ui.set_pending_action_id("repro-client".into());
    ui.set_modal("duplicate-instance".into());
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn record_answers(ui: &AppWindow) -> Answers {
    let answers: Answers = Rc::default();
    let sink = Rc::clone(&answers);
    ui.on_duplicate_client_deciding(move |id, drop_absent, persistent| {
        sink.borrow_mut()
            .push((id.to_string(), drop_absent, persistent));
    });
    answers
}

/// Whether the dialog shows its storage note, which only the storage question
/// carries. "Storage" alone also labels controls elsewhere in the window.
fn asks_for_storage(ui: &AppWindow) -> bool {
    widest(ui, &lookup("gui-duplicate-temporary-note")).is_some()
}

fn press(ui: &AppWindow, label: &str) {
    widest(ui, &lookup(label))
        .unwrap_or_else(|| panic!("a {label} button"))
        .invoke_accessible_default_action();
}

fn widest(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
}
