//! Reaching every mod's controls from the keyboard in a list that builds only
//! the rows in view.
//!
//! A row outside the view does not exist, so Tab has no control to walk on to.
//! These tests press Tab and Shift+Tab through a list far longer than the
//! window and read where the focus outline sits after each press.

#![allow(clippy::panic)]

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, InstanceRow, ModRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::platform::{Key, WindowEvent};
use slint::{
    ComponentHandle, Image, LogicalPosition, Model, ModelRc, SharedPixelBuffer, SharedString,
    VecModel,
};

/// More Tab presses than the busiest list has controls, so a list that traps
/// focus fails the test instead of looping.
const TAB_LIMIT: usize = 400;

/// How far the focus outline may stand from the control it surrounds.
const OUTLINE_REACH: f32 = 4.0;

const PASSES: usize = 1;

fn mod_row(title: &str) -> ModRow {
    ModRow {
        project_id: title.into(),
        logical_path: format!("mods/{title}.jar").into(),
        title: title.into(),
        file_name: format!("{title}.jar").into(),
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
        can_change_version: false,
        can_update: false,
        can_remove: true,
        can_delete_file: false,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

fn title(index: usize) -> String {
    format!("Mod {index:03}")
}

fn rows(count: usize) -> Vec<ModRow> {
    (0..count).map(|index| mod_row(&title(index))).collect()
}

fn loads(title: &str) -> String {
    lookup_args("gui-mod-loads", [("value", Arg::from(title))])
}

fn removes(title: &str) -> String {
    lookup_args("gui-remove-mod", [("value", Arg::from(title))])
}

/// Opens the Mods tab on `rows`, the way the controller keeps the list.
fn show(rows: &[ModRow]) -> (AppWindow, Rc<VecModel<ModRow>>) {
    let ui = support::test_window();
    support::show_client(
        &ui,
        InstanceRow {
            id: "an-id".into(),
            ..InstanceRow::default()
        },
        "Mods",
    );
    let model = Rc::new(VecModel::from(rows.to_vec()));
    support::set_mods(&ui, ModelRc::from(model.clone()));
    settle(&ui);
    (ui, model)
}

/// Renders until the rows a scroll brought in have run what they asked for.
fn settle(ui: &AppWindow) {
    for _ in 0..PASSES {
        slint::platform::update_timers_and_animations();
        let _: SharedPixelBuffer<slint::Rgba8Pixel> = ui.window().take_snapshot().expect("render");
    }
}

fn press(ui: &AppWindow, key: Key) {
    let text = SharedString::from(key);
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    settle(ui);
}

fn press_text(ui: &AppWindow, text: &str) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    settle(ui);
}

type Geometry = (f32, f32, f32, f32);

fn geometry(element: &ElementHandle) -> Geometry {
    let at = element.absolute_position();
    let size = element.size();
    (at.x, at.y, size.width, size.height)
}

fn surrounds(outline: Geometry, control: Geometry) -> bool {
    let (left, top) = (control.0 - outline.0, control.1 - outline.1);
    let (right, bottom) = (
        outline.0 + outline.2 - control.0 - control.2,
        outline.1 + outline.3 - control.1 - control.3,
    );
    left > 0.0
        && left <= OUTLINE_REACH
        && [top, right, bottom]
            .iter()
            .all(|side| (side - left).abs() < 1.0)
}

fn focus_outlines(ui: &AppWindow) -> Vec<Geometry> {
    ElementQuery::from_root(ui)
        .match_descendants()
        .match_id("KeyboardFocus::ring")
        .find_all()
        .iter()
        .map(geometry)
        .collect()
}

/// The accessible labels of the mod controls that carry the focus outline.
fn focused_labels(ui: &AppWindow) -> Vec<String> {
    let outlines = focus_outlines(ui);
    if outlines.is_empty() {
        return Vec::new();
    }
    let mut labels = Vec::new();
    for role in [AccessibleRole::Checkbox, AccessibleRole::Button] {
        for element in ElementQuery::from_root(ui)
            .match_descendants()
            .match_accessible_role(role)
            .find_all()
        {
            let own = geometry(&element);
            if outlines.iter().any(|outline| surrounds(*outline, own))
                && let Some(label) = element.accessible_label()
            {
                labels.push(label.to_string());
            }
        }
    }
    labels.sort();
    labels.dedup();
    labels
}

/// The one control that has the focus, by its label.
fn focused(ui: &AppWindow) -> Option<String> {
    let mut labels = focused_labels(ui);
    assert!(
        labels.len() <= 1,
        "more than one control shows the focus outline: {labels:?}"
    );
    labels.pop()
}

/// The list the mods are shown in: the one over a mod's switch. A switch the
/// list clips at its top edge can sit just above the list's own box.
fn list(ui: &AppWindow) -> ElementHandle {
    let switch = ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::Checkbox)
        .find_all()
        .into_iter()
        .find(|element| {
            element
                .accessible_label()
                .is_some_and(|label| label.contains("Mod 0"))
        })
        .expect("a mod's switch is built");
    let at = switch.absolute_position();
    ElementQuery::from_root(ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::List)
        .find_all()
        .into_iter()
        .find(|element| {
            let (x, y, width, height) = geometry(element);
            height > 100.0
                && at.x >= x
                && at.x <= x + width
                && at.y + 24.0 >= y
                && at.y <= y + height
        })
        .expect("the Mods list")
}

/// Whether the focus outline lies entirely inside the list's visible area.
fn outline_in_view(ui: &AppWindow, bounds: Geometry) -> bool {
    let (list_x, list_y, list_width, list_height) = bounds;
    focus_outlines(ui).iter().all(|&(x, y, width, height)| {
        x >= list_x
            && x + width <= list_x + list_width
            && y >= list_y
            && y + height <= list_y + list_height
    })
}

/// Presses Tab until `label` has the focus.
fn tab_to(ui: &AppWindow, label: &str) {
    for _ in 0..TAB_LIMIT {
        press(ui, Key::Tab);
        if focused(ui).as_deref() == Some(label) {
            return;
        }
    }
    panic!("Tab never reached {label}");
}

fn scroll_list(ui: &AppWindow, delta: f32) {
    let list = list(ui);
    let (x, y, width, height) = geometry(&list);
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(x + width / 2.0, y + height / 2.0),
        delta_x: 0.0,
        delta_y: delta,
    });
    settle(ui);
}

#[test]
fn tab_reaches_every_mods_controls_in_a_list_longer_than_the_view() {
    let count = 24;
    let (ui, _model) = show(&rows(count));
    let bounds = geometry(&list(&ui));
    tab_to(&ui, &loads(&title(0)));

    let mut walked = vec![loads(&title(0))];
    for _ in 0..(2 * count - 1) {
        press(&ui, Key::Tab);
        let label = focused(&ui).expect("Tab keeps the focus on a mod control while mods remain");
        assert!(
            outline_in_view(&ui, bounds),
            "{label} has the focus but is not fully in view"
        );
        walked.push(label);
    }

    let expected: Vec<String> = (0..count)
        .flat_map(|index| [loads(&title(index)), removes(&title(index))])
        .collect();
    assert_eq!(walked, expected);
}

#[test]
fn shift_tab_walks_back_through_every_mods_controls() {
    let count = 24;
    let (ui, _model) = show(&rows(count));
    let bounds = geometry(&list(&ui));
    tab_to(&ui, &loads(&title(0)));
    for _ in 0..(2 * count - 1) {
        press(&ui, Key::Tab);
    }
    assert_eq!(focused(&ui), Some(removes(&title(count - 1))));

    let mut walked = vec![removes(&title(count - 1))];
    for _ in 0..(2 * count - 1) {
        press(&ui, Key::Backtab);
        let label = focused(&ui).expect("Shift+Tab keeps the focus on a mod control");
        assert!(
            outline_in_view(&ui, bounds),
            "{label} has the focus but is not fully in view"
        );
        walked.push(label);
    }

    let expected: Vec<String> = (0..count)
        .rev()
        .flat_map(|index| [removes(&title(index)), loads(&title(index))])
        .collect();
    assert_eq!(walked, expected);
}

#[test]
fn tab_leaves_the_list_after_the_last_mods_last_control() {
    let count = 24;
    let (ui, _model) = show(&rows(count));
    tab_to(&ui, &removes(&title(0)));
    for _ in 0..(2 * (count - 1)) {
        press(&ui, Key::Tab);
    }
    assert_eq!(focused(&ui), Some(removes(&title(count - 1))));

    press(&ui, Key::Tab);

    let now = focused(&ui);
    assert!(
        now.as_deref() != Some(removes(&title(count - 1)).as_str()),
        "Tab stayed on the last control"
    );
    assert!(
        now.is_none_or(|label| !label.contains("Mod 0")),
        "Tab went back into the list instead of leaving it"
    );
}

#[test]
fn a_deleted_mods_restore_button_is_reached_by_tab() {
    let mut rows = rows(24);
    rows[18].deleted = true;
    let (ui, _model) = show(&rows);
    tab_to(&ui, &loads(&title(0)));

    let mut seen_restore = false;
    for _ in 0..(2 * 24) {
        press(&ui, Key::Tab);
        if focused(&ui).as_deref() == Some(lookup("gui-restore-mod").as_str()) {
            seen_restore = true;
            break;
        }
    }

    assert!(
        seen_restore,
        "Tab never reached the Restore button of a mod that is scrolled out of view"
    );
}

#[test]
fn a_row_the_wheel_scrolls_away_never_leaves_the_focus_on_another_mod() {
    let (ui, _model) = show(&rows(80));
    let toggled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&toggled);
    ui.on_set_mod_enabled(move |_kind, _host, path, _enabled| {
        record.borrow_mut().push(path.to_string());
    });
    tab_to(&ui, &loads(&title(2)));

    for _ in 0..6 {
        scroll_list(&ui, -300.0);
        if let Some(label) = focused(&ui) {
            assert!(
                label.contains(&title(2)),
                "the focus moved to {label} while the list scrolled"
            );
        }
    }
    press_text(&ui, " ");

    assert!(
        toggled
            .borrow()
            .iter()
            .all(|path| path == &format!("mods/{}.jar", title(2))),
        "Space acted on a mod other than the one that was focused: {:?}",
        toggled.borrow()
    );
}

#[test]
fn a_focused_row_that_comes_to_show_another_mod_gives_up_the_focus() {
    let (ui, model) = show(&rows(10));
    let toggled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&toggled);
    ui.on_set_mod_enabled(move |_kind, _host, path, _enabled| {
        record.borrow_mut().push(path.to_string());
    });
    tab_to(&ui, &loads(&title(2)));

    model.set_row_data(2, mod_row("A Different Mod"));
    settle(&ui);

    assert_eq!(
        focused(&ui),
        None,
        "the focus stayed on a row that now shows a different mod"
    );
    press_text(&ui, " ");
    assert!(toggled.borrow().is_empty(), "Space acted on the other mod");
}

#[test]
fn tab_into_a_partly_scrolled_first_row_brings_the_whole_row_into_view() {
    let (ui, _model) = show(&rows(60));
    let bounds = geometry(&list(&ui));
    // Half a row, so the first built row is cut off at the top.
    scroll_list(&ui, -30.0);

    tab_to(&ui, &loads(&title(1)));

    let label = focused(&ui).expect("a control has the focus");
    assert!(
        outline_in_view(&ui, bounds),
        "{label} has the focus but is cut off by the edge of the list"
    );
}
