//! Changing exactly one mod from its row, leaving the mods that need it, or
//! that it needs, as they are.
//!
//! Shift+click on the switch or the bin asks for the narrow change, and the
//! row's right-click menu offers the same, whatever the dependency settings
//! say.

#![allow(clippy::panic)]

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ephervex_gui::ui::{AppWindow, InstanceRow, ModRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Image, LogicalPosition, ModelRc, SharedString, VecModel};

const MOD: &str = "LibX";

/// What the switch asked for: the path, whether on, and whether alone.
type Toggles = Rc<RefCell<Vec<(String, bool, bool)>>>;

#[test]
fn a_plain_click_on_the_switch_follows_the_dependency_settings() {
    let ui = support::test_window();
    let toggles = record_toggles(&ui);
    show(&ui, false, row(false));

    click(&ui, &switch(&ui), false);

    assert_eq!(
        toggles.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), false, false)]
    );
}

#[test]
fn shift_click_on_the_switch_changes_only_this_mod() {
    let ui = support::test_window();
    let toggles = record_toggles(&ui);
    show(&ui, false, row(false));

    click(&ui, &switch(&ui), true);

    assert_eq!(
        toggles.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), false, true)]
    );
}

#[test]
fn the_rows_menu_turns_only_this_mod_off() {
    let ui = support::test_window();
    let toggles = record_toggles(&ui);
    show(&ui, false, row(false));

    pick_from_menu(&ui, "gui-mod-turn-off-alone");

    assert_eq!(
        toggles.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), false, true)]
    );
}

#[test]
fn the_rows_menu_turns_only_a_disabled_mod_on() {
    let ui = support::test_window();
    let toggles = record_toggles(&ui);
    show(
        &ui,
        false,
        ModRow {
            enabled: false,
            ..row(false)
        },
    );

    pick_from_menu(&ui, "gui-mod-turn-on-alone");

    assert_eq!(
        toggles.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), true, true)]
    );
}

#[test]
fn shift_click_on_the_bin_deletes_only_this_jar() {
    let ui = support::test_window();
    let deleted = record_deletions(&ui);
    show(&ui, false, row(false));

    click(&ui, &bin(&ui, "gui-delete-mod"), true);
    assert_eq!(ui.get_modal(), "delete-mod");
    assert!(visible(&ui, &lookup("gui-delete-mod-alone-note")));
    press(&ui, "gui-delete");

    assert_eq!(
        deleted.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), true)]
    );
}

#[test]
fn a_plain_click_on_the_bin_asks_about_the_mods_that_need_it() {
    let ui = support::test_window();
    let deleted = record_deletions(&ui);
    show(&ui, false, row(false));

    click(&ui, &bin(&ui, "gui-delete-mod"), false);
    assert!(!visible(&ui, &lookup("gui-delete-mod-alone-note")));
    press(&ui, "gui-delete");

    assert_eq!(
        deleted.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), false)]
    );
}

/// Where the bin would take the mod out of the instance, deleting only its
/// jar is still on offer: every recorded file may be put aside.
#[test]
fn the_rows_menu_deletes_only_the_jar_where_the_bin_would_remove_the_mod() {
    let ui = support::test_window();
    let deleted = record_deletions(&ui);
    show(&ui, true, row(true));
    assert!(bin_labeled(&ui, "gui-remove-mod").is_some());

    pick_from_menu(&ui, "gui-mod-delete-alone");
    assert_eq!(ui.get_modal(), "delete-mod");
    press(&ui, "gui-delete");

    assert_eq!(
        deleted.borrow().as_slice(),
        [("mods/libx.jar".to_owned(), true)]
    );
}

fn record_toggles(ui: &AppWindow) -> Toggles {
    let toggles: Toggles = Rc::default();
    let sink = Rc::clone(&toggles);
    ui.on_set_mod_enabled(move |_kind, _host, path, enabled, alone| {
        sink.borrow_mut().push((path.to_string(), enabled, alone));
    });
    toggles
}

fn record_deletions(ui: &AppWindow) -> Rc<RefCell<Vec<(String, bool)>>> {
    let deleted: Rc<RefCell<Vec<(String, bool)>>> = Rc::default();
    let sink = Rc::clone(&deleted);
    ui.on_delete_hand_placed_mod(move |_kind, _host, path, alone| {
        sink.borrow_mut().push((path.to_string(), alone));
    });
    deleted
}

fn switch(ui: &AppWindow) -> ElementHandle {
    let label = lookup_args("gui-mod-loads", [("value", Arg::from(MOD))]);
    ElementHandle::find_by_accessible_label(ui, &label)
        .find(|element| element.size().width > 0.0)
        .expect("the row's switch")
}

fn bin_labeled(ui: &AppWindow, id: &str) -> Option<ElementHandle> {
    let label = lookup_args(id, [("value", Arg::from(MOD))]);
    ElementHandle::find_by_accessible_label(ui, &label).find(|element| element.size().width > 0.0)
}

fn bin(ui: &AppWindow, id: &str) -> ElementHandle {
    bin_labeled(ui, id).unwrap_or_else(|| panic!("a {id} bin"))
}

/// Clicks the middle of `element`, holding Shift when asked.
fn click(ui: &AppWindow, element: &ElementHandle, shift: bool) {
    let position = LogicalPosition::new(
        element.absolute_position().x + element.size().width / 2.0,
        element.absolute_position().y + element.size().height / 2.0,
    );
    let shift_key: SharedString = Key::Shift.into();
    if shift {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: shift_key.clone(),
        });
    }
    pointer(ui, position, PointerEventButton::Left);
    if shift {
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: shift_key });
    }
    settle(ui);
}

/// Opens the row's menu where the mod's name is and picks the entry `id`.
fn pick_from_menu(ui: &AppWindow, id: &str) {
    let name = ElementHandle::find_by_accessible_label(ui, MOD)
        .find(|element| element.size().width > 0.0)
        .expect("the row is on screen");
    let origin = LogicalPosition::new(
        name.absolute_position().x + name.size().width / 2.0,
        name.absolute_position().y + name.size().height / 2.0,
    );
    pointer(ui, origin, PointerEventButton::Right);
    let entry = ElementHandle::find_by_accessible_label(ui, &lookup(id))
        .find(|element| {
            element.accessible_role() == Some(AccessibleRole::Button) && element.size().width > 0.0
        })
        .unwrap_or_else(|| panic!("the menu offers {id}"));
    // A popup's elements report their positions from the popup's own corner.
    let target = LogicalPosition::new(
        origin.x + entry.absolute_position().x + entry.size().width / 2.0,
        origin.y + entry.absolute_position().y + entry.size().height / 2.0,
    );
    pointer(ui, target, PointerEventButton::Left);
}

fn press(ui: &AppWindow, id: &str) {
    ElementHandle::find_by_accessible_label(ui, &lookup(id))
        .filter(|element| element.size().width > 0.0)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("a {id} button"))
        .invoke_accessible_default_action();
    settle(ui);
}

fn visible(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

fn pointer(ui: &AppWindow, position: LogicalPosition, button: PointerEventButton) {
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
    ui.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
    settle(ui);
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn row(can_remove: bool) -> ModRow {
    ModRow {
        project_id: "LIBX".into(),
        logical_path: "mods/libx.jar".into(),
        title: MOD.into(),
        file_name: "libx.jar".into(),
        version: "1.0.0".into(),
        version_known: true,
        provider: "Modrinth".into(),
        provider_kind: "modrinth".into(),
        origin: "Added".into(),
        origin_kind: "user_selected".into(),
        compatibility: SharedString::default(),
        author: SharedString::default(),
        icon: Image::default(),
        enabled: true,
        deleted: false,
        pack_owned: false,
        can_manage: true,
        can_change_version: true,
        can_update: true,
        can_remove,
        can_delete_file: true,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

fn show(ui: &AppWindow, mods_are_the_users: bool, mod_row: ModRow) {
    let mut view = support::client_view(InstanceRow {
        id: "an-id".into(),
        ..InstanceRow::default()
    });
    view.allow_pack_mod_changes = mods_are_the_users;
    support::show(ui, vec![view], vec![support::pane(1, 0, "Mods")]);
    support::set_mods(ui, ModelRc::from(Rc::new(VecModel::from(vec![mod_row]))));
    settle(ui);
}
