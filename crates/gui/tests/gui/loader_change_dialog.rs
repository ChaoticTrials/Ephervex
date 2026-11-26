//! Changing a loader build when some mods do not fit it.
//!
//! A refusal that names only a count, and offers nothing to answer it with,
//! leaves the user unable to install the build they chose. The dialog shows
//! which mods do not fit and why, and installs anyway only on a second,
//! explicit action.

use std::cell::RefCell;
use std::rc::Rc;

use crate::support;

use ephervex_gui::ui::AppWindow;
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

type Request = (String, String, bool);

#[test]
fn the_first_request_never_accepts_incompatible_mods() {
    let ui = support::test_window();
    let requests = record_requests(&ui);
    open_dialog(&ui, "21.1.1", "21.1.2");

    press(&ui, "gui-loader-change-action");

    assert_eq!(
        requests.borrow().as_slice(),
        [("instance".to_owned(), "21.1.2".to_owned(), false)]
    );
}

#[test]
fn a_refused_change_lists_the_mods_and_offers_to_install_anyway() {
    let ui = support::test_window();
    let requests = record_requests(&ui);
    open_dialog(&ui, "21.1.1", "21.1.2");
    ui.set_loader_change_blocked(lines(&[
        "Alpha: Does not support NeoForge.",
        "Beta: Does not support Minecraft 1.21.1.",
    ]));
    settle(&ui);

    for line in [
        "Alpha: Does not support NeoForge.",
        "Beta: Does not support Minecraft 1.21.1.",
    ] {
        assert!(widest(&ui, line).is_some(), "{line} is listed");
    }
    let heading = lookup_args("gui-loader-change-blocked", [("count", Arg::Count(2))]);
    assert!(
        widest(&ui, &heading).is_some(),
        "the count introduces the list"
    );
    assert!(
        widest(&ui, &lookup("gui-loader-change-action")).is_none(),
        "the plain action is replaced, not repeated"
    );

    press(&ui, "gui-loader-change-action-anyway");

    assert_eq!(
        requests.borrow().as_slice(),
        [("instance".to_owned(), "21.1.2".to_owned(), true)]
    );
}

#[test]
fn choosing_another_build_withdraws_the_acceptance_offered_for_the_first() {
    let ui = support::test_window();
    let requests = record_requests(&ui);
    open_dialog(&ui, "21.1.1", "21.1.2");
    ui.set_loader_change_versions(lines(&["21.1.3", "21.1.2", "21.1.1"]));
    ui.set_loader_change_blocked(lines(&["Alpha: Does not support NeoForge."]));
    settle(&ui);
    assert!(widest(&ui, &lookup("gui-loader-change-action-anyway")).is_some());

    pick_up(&ui, &lookup("gui-loader-change-target"), 1);

    assert_eq!(ui.get_loader_change_selected(), "21.1.3");
    assert_eq!(ui.get_loader_change_blocked().row_count(), 0);
    assert!(widest(&ui, &lookup("gui-loader-change-action-anyway")).is_none());
    press(&ui, "gui-loader-change-action");
    assert_eq!(
        requests.borrow().as_slice(),
        [("instance".to_owned(), "21.1.3".to_owned(), false)]
    );
}

/// Opens the dropdown and moves its selection up by `steps` entries, the way
/// the arrow keys do, then confirms.
fn pick_up(ui: &AppWindow, label: &str, steps: usize) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("a dropdown with that label")
        .invoke_accessible_expand_action();
    settle(ui);
    for key in std::iter::repeat_n(Key::UpArrow, steps).chain([Key::Return]) {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }
    settle(ui);
}

fn open_dialog(ui: &AppWindow, current: &str, selected: &str) {
    ui.set_mod_action_host("instance".into());
    ui.set_loader_change_current(format!("NeoForge {current}").into());
    ui.set_loader_change_current_build(current.into());
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(lines(&[selected, current]));
    ui.set_loader_change_selected(selected.into());
    ui.set_modal("loader-change".into());
    settle(ui);
}

fn record_requests(ui: &AppWindow) -> Rc<RefCell<Vec<Request>>> {
    let requests: Rc<RefCell<Vec<Request>>> = Rc::default();
    let sink = Rc::clone(&requests);
    ui.on_change_loader_version(move |instance, version, accept| {
        sink.borrow_mut()
            .push((instance.to_string(), version.to_string(), accept));
    });
    requests
}

fn press(ui: &AppWindow, label: &str) {
    widest(ui, &lookup(label))
        .expect("the action is offered")
        .invoke_accessible_default_action();
}

fn lines(entries: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        entries
            .iter()
            .map(|entry| SharedString::from(*entry))
            .collect::<Vec<_>>(),
    ))
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
