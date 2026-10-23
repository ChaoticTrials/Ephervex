//! Reaching and pressing the shared controls from the keyboard.
//!
//! Buttons, switches, chips and navigation entries are drawn by their own
//! components, so nothing but the component itself can make them take focus.
//! These tests walk Tab through a real window and press the control that the
//! focus outline surrounds.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, ElementQuery, TestingBackend, TestingBackendOptions,
};
use slint::platform::{Key, WindowEvent};
use slint::{
    ComponentHandle, ModelRc, PhysicalSize, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

/// More Tab presses than the busiest window has focusable controls, so a
/// control that Tab never reaches fails the test instead of looping.
const TAB_LIMIT: usize = 400;

#[test]
fn tab_reaches_the_settings_button_and_enter_opens_the_settings() {
    let ui = window();
    assert!(ui.get_shows_home());

    let settings = control(&ui, &lookup("gui-settings"));
    assert!(
        tab_to(&ui, &settings),
        "Tab never reached the Settings button"
    );
    press(&ui, Key::Return);

    assert!(ui.get_settings_open());
    assert_eq!(ui.get_settings_tab(), "General");
}

#[test]
fn tab_reaches_the_home_button_and_enter_leaves_the_settings() {
    let ui = window();
    ui.invoke_open_settings("Java".into());
    settle(&ui);

    let home = control(&ui, &lookup("gui-home"));
    assert!(tab_to(&ui, &home), "Tab never reached the Home button");
    press(&ui, Key::Return);

    assert!(!ui.get_settings_open());
    assert!(ui.get_shows_home());
}

#[test]
fn tab_reaches_a_home_card_button_and_enter_presses_it() {
    let ui = window();
    let launched: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client(move |id| record.borrow_mut().push(id.to_string()));
    ui.set_home_resume(ModelRc::new(VecModel::from(vec![
        ephervex_gui::ui::ResumeRow {
            kind: "client".into(),
            id: "repro-client".into(),
            host_name: "Repro".into(),
            can_launch: true,
            ..ephervex_gui::ui::ResumeRow::default()
        },
    ])));
    settle(&ui);

    let launch = control(
        &ui,
        &lookup_args("gui-home-launch-host", [("value", Arg::from("Repro"))]),
    );
    assert!(tab_to(&ui, &launch), "Tab never reached Launch");
    press(&ui, Key::Return);

    assert_eq!(launched.borrow().as_slice(), ["repro-client"]);
}

#[test]
fn escape_leaves_the_settings_for_the_page_they_were_opened_over() {
    let ui = window();
    ui.invoke_open_settings("General".into());
    settle(&ui);
    assert!(ui.get_settings_open());

    press(&ui, Key::Escape);

    assert!(!ui.get_settings_open());
}

#[test]
fn space_activates_the_focused_settings_navigation_entry() {
    let ui = window();
    ui.set_settings_open(true);
    settle(&ui);

    let java = control(&ui, &lookup("gui-settings-java"));
    assert!(tab_to(&ui, &java), "Tab never reached the Java entry");
    press_text(&ui, " ");

    assert_eq!(ui.get_settings_tab(), "Java");
}

#[test]
fn space_and_enter_each_press_the_focused_star_once() {
    let ui = window();
    let toggled = open_create_instance(&ui);
    let star = control(&ui, &offer("1.16.3"));

    assert!(tab_to(&ui, &star), "Tab never reached the star");
    press_text(&ui, " ");
    assert_eq!(toggled.borrow().as_slice(), ["1.16.3"]);
    press(&ui, Key::Return);
    assert_eq!(toggled.borrow().as_slice(), ["1.16.3", "1.16.3"]);
}

#[test]
fn a_held_key_does_not_press_the_focused_control_again() {
    let ui = window();
    let toggled = open_create_instance(&ui);
    let star = control(&ui, &offer("1.16.3"));
    assert!(tab_to(&ui, &star), "Tab never reached the star");

    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
    for _ in 0..5 {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressRepeated { text: " ".into() });
    }
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: " ".into() });

    assert_eq!(toggled.borrow().len(), 1);
}

#[test]
fn enter_presses_the_focused_icon_button_and_text_button() {
    let ui = window();
    open_create_instance(&ui);

    let cancel = control(&ui, &lookup("gui-cancel"));
    assert!(tab_to(&ui, &cancel), "Tab never reached Cancel");
    press(&ui, Key::Return);
    assert_eq!(ui.get_modal(), "");

    open_create_instance(&ui);
    let close = control(&ui, &lookup("gui-close"));
    assert!(tab_to(&ui, &close), "Tab never reached the close button");
    press(&ui, Key::Return);
    assert_eq!(ui.get_modal(), "");
}

#[test]
fn space_flips_the_focused_switch() {
    let ui = window();
    open_create_instance(&ui);
    let before = ui.get_create_persistent();
    // The dialog is drawn after the page behind it, so its switch is the last
    // checkbox: the page has filter chips of its own.
    let switch = ElementQuery::from_root(&ui)
        .match_descendants()
        .match_accessible_role(AccessibleRole::Checkbox)
        .find_all()
        .into_iter()
        .next_back()
        .expect("the dialog has a switch");

    assert!(tab_to(&ui, &switch), "Tab never reached the switch");
    press_text(&ui, " ");

    assert_ne!(ui.get_create_persistent(), before);
}

#[test]
fn a_disabled_button_is_skipped_by_tab_and_ignores_the_keyboard() {
    let ui = window();
    let submitted: Rc<RefCell<usize>> = Rc::default();
    let count = Rc::clone(&submitted);
    ui.on_submit_create_instance(move |_, _, _, _, _| *count.borrow_mut() += 1);
    open_create_instance(&ui);
    // A loader with no version chosen leaves Create unavailable.
    ui.set_create_loader("Fabric".into());
    ui.set_create_loader_version("".into());
    settle(&ui);

    // Cancel sits beside Create and is enabled, so reaching it shows the walk
    // gets as far as the pair of buttons.
    let cancel = control(&ui, &lookup("gui-cancel"));
    assert!(tab_to(&ui, &cancel), "Tab never reached Cancel");
    let create = control(&ui, &lookup("gui-create-instance"));
    assert_eq!(create.accessible_enabled(), Some(false));
    assert!(!tab_to(&ui, &create), "Tab stopped on a disabled button");
    press(&ui, Key::Return);
    press_text(&ui, " ");

    assert_eq!(*submitted.borrow(), 0);
}

#[test]
fn a_control_that_is_disabled_while_focused_ignores_the_keyboard() {
    let ui = window();
    let toggled = open_create_instance(&ui);
    let star = control(&ui, &offer("1.16.3"));
    assert!(tab_to(&ui, &star), "Tab never reached the star");

    // The star turns off with the release it stars, while it still has focus.
    ui.set_create_minecraft("".into());
    settle(&ui);
    press(&ui, Key::Return);
    press_text(&ui, " ");

    assert!(toggled.borrow().is_empty());
    assert!(
        focus_outlines(&ui).is_empty(),
        "a disabled control must not keep drawing the focus outline"
    );
}

#[test]
fn tab_stays_inside_an_open_dialog_in_both_directions() {
    let ui = window();
    let behind = [
        control(&ui, &lookup("gui-home")),
        control(&ui, &lookup("gui-settings")),
        new_instance_on_home(&ui),
    ]
    .map(|control| geometry(&control));
    open_create_instance(&ui);
    let cancel = geometry(&control(&ui, &lookup("gui-cancel")));
    let close = geometry(&control(&ui, &lookup("gui-close")));

    for key in [Key::Tab, Key::Backtab] {
        let mut stops = Vec::new();
        for _ in 0..TAB_LIMIT / 4 {
            press(&ui, key);
            stops.extend(focus_outlines(&ui));
        }
        let reaches = |control: &Geometry| stops.iter().any(|stop| surrounds(*stop, *control));
        assert!(
            reaches(&cancel) && reaches(&close),
            "{key:?} never reached the dialog's own buttons"
        );
        assert!(
            !behind.iter().any(reaches),
            "{key:?} reached a control on the page behind the dialog"
        );
    }
    assert!(ui.get_shows_home());
    assert_eq!(ui.get_modal(), "create-instance");
}

#[test]
fn the_focus_outline_is_drawn_outside_a_primary_button_whose_own_border_is_accent() {
    let ui = window();
    let button = new_instance_on_home(&ui);
    let (x, y, _, height) = geometry(&button);
    // A point in the outline's band, left of the button's own border.
    let (column, row) = (x - 2, y + height / 2);
    let before = pixel_at(&ui, column, row);

    assert!(tab_to(&ui, &button), "Tab never reached New instance");

    assert_ne!(
        pixel_at(&ui, column, row),
        before,
        "focusing the button must change what is drawn beyond its border"
    );
}

#[test]
fn only_the_focused_control_draws_the_focus_outline() {
    let ui = window();
    assert!(
        focus_outlines(&ui).is_empty(),
        "nothing has focus until Tab is pressed"
    );

    let home = control(&ui, &lookup("gui-home"));
    assert!(tab_to(&ui, &home));
    assert_eq!(focus_outlines(&ui).len(), 1);
}

/// Presses Tab until the focus outline sits exactly on `target`, or focus has
/// gone all the way around without reaching it.
fn tab_to(ui: &AppWindow, target: &ElementHandle) -> bool {
    let wanted = geometry(target);
    let mut visited = Vec::new();
    for _ in 0..TAB_LIMIT {
        press(ui, Key::Tab);
        let focused = focus_outlines(ui);
        if focused.iter().any(|outline| surrounds(*outline, wanted)) {
            return true;
        }
        // Text fields and dropdowns take focus without this outline, so only
        // a repeated outline means focus has come back around.
        if focused.is_empty() {
            continue;
        }
        if visited.contains(&focused) {
            return false;
        }
        visited.push(focused);
    }
    false
}

type Geometry = (i32, i32, i32, i32);

/// How far the focus outline may stand from the control it surrounds: its gap
/// plus its width, with a pixel of rounding.
const OUTLINE_REACH: i32 = 4;

/// Whether `outline` is the focus outline of the control at `control`: the
/// same box, grown evenly on every side by no more than the outline's reach.
fn surrounds(outline: Geometry, control: Geometry) -> bool {
    let (left, top) = (control.0 - outline.0, control.1 - outline.1);
    let (right, bottom) = (
        outline.0 + outline.2 - control.0 - control.2,
        outline.1 + outline.3 - control.1 - control.3,
    );
    left > 0 && left <= OUTLINE_REACH && [top, right, bottom].iter().all(|side| *side == left)
}

/// Where an element is, as whole pixels so that elements compare.
#[allow(clippy::cast_possible_truncation)]
fn geometry(element: &ElementHandle) -> Geometry {
    let at = element.absolute_position();
    let size = element.size();
    (
        at.x.round() as i32,
        at.y.round() as i32,
        size.width.round() as i32,
        size.height.round() as i32,
    )
}

/// Where each distinct focus outline is. The element query reports one outline
/// several times, once for each place the compiler recorded it.
fn focus_outlines(ui: &AppWindow) -> Vec<Geometry> {
    let mut outlines: Vec<Geometry> = ElementQuery::from_root(ui)
        .match_descendants()
        .match_id("KeyboardFocus::ring")
        .find_all()
        .iter()
        .map(geometry)
        .collect();
    outlines.sort_unstable();
    outlines.dedup();
    outlines
}

fn pixel_at(ui: &AppWindow, column: i32, row: i32) -> Rgba8Pixel {
    let snapshot: SharedPixelBuffer<Rgba8Pixel> = ui.window().take_snapshot().expect("render");
    let index = usize::try_from(row).expect("row") * snapshot.width() as usize
        + usize::try_from(column).expect("column");
    snapshot.as_slice()[index]
}

/// Home's New Instance button, not the one the empty list offers.
fn new_instance_on_home(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-modal-new-instance"))
        .find(|element| element.absolute_position().x > 340.0)
        .expect("Home offers New Instance")
}

fn control(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .expect("the window has a control with this label")
}

fn open_create_instance(ui: &AppWindow) -> Rc<RefCell<Vec<String>>> {
    let toggled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&toggled);
    ui.on_toggle_favorite_version(move |version| record.borrow_mut().push(version.to_string()));
    ui.set_loading(false);
    ui.set_minecraft_releases(ModelRc::new(VecModel::from(
        ["1.21.1", "1.16.3"].map(SharedString::from).to_vec(),
    )));
    ui.set_create_minecraft("1.16.3".into());
    ui.set_create_minecraft_favorite(false);
    ui.set_modal("create-instance".into());
    settle(ui);
    toggled
}

fn offer(version: &str) -> String {
    lookup_args("gui-favorite-version", [("value", Arg::from(version))])
}

fn press(ui: &AppWindow, key: Key) {
    press_text(ui, SharedString::from(key).as_str());
}

fn press_text(ui: &AppWindow, text: &str) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    settle(ui);
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
    settle(&ui);
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
