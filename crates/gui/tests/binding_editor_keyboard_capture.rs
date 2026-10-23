//! Starting and finishing a key binding capture from the keyboard.
//!
//! The binding editor takes Tab focus, so Enter and Space start the capture
//! the way a click does, and the key that started it never becomes the
//! binding by repeating.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, MinecraftPreferenceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, ElementQuery, TestingBackend, TestingBackendOptions};
use slint::platform::{Key, WindowEvent};
use slint::{
    ComponentHandle, ModelRc, PhysicalSize, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

/// More Tab presses than the busiest window has focusable controls, so an
/// editor that Tab never reaches fails the test instead of looping.
const TAB_LIMIT: usize = 400;

const BINDING_ID: &str = "keybind_sneak";
const BINDING_LABEL: &str = "Sneak";

#[test]
fn enter_on_the_focused_binding_editor_starts_the_capture() {
    let session = open_binding_editor();
    focus_editor(&session);

    press(&session.ui, Key::Return);

    assert_eq!(session.begun.borrow().as_slice(), [BINDING_ID]);
    assert!(session.captured.borrow().is_empty());
}

#[test]
fn space_on_the_focused_binding_editor_starts_the_capture() {
    let session = open_binding_editor();
    focus_editor(&session);

    press_text(&session.ui, " ");

    assert_eq!(session.begun.borrow().as_slice(), [BINDING_ID]);
    assert!(session.captured.borrow().is_empty());
}

#[test]
fn a_held_key_starts_the_capture_once_and_never_becomes_the_binding() {
    let session = open_binding_editor();
    focus_editor(&session);

    session
        .ui
        .window()
        .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
    for _ in 0..5 {
        session
            .ui
            .window()
            .dispatch_event(WindowEvent::KeyPressRepeated { text: " ".into() });
    }
    session
        .ui
        .window()
        .dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
    settle(&session.ui);

    assert_eq!(session.begun.borrow().as_slice(), [BINDING_ID]);
    assert!(session.captured.borrow().is_empty());
}

#[test]
fn a_held_key_does_not_start_another_capture_after_one_ends() {
    let session = open_binding_editor();
    focus_editor(&session);

    session
        .ui
        .window()
        .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
    session
        .ui
        .set_minecraft_capturing_id(SharedString::default());
    session
        .ui
        .window()
        .dispatch_event(WindowEvent::KeyPressRepeated { text: " ".into() });
    session
        .ui
        .window()
        .dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
    settle(&session.ui);

    assert_eq!(session.begun.borrow().as_slice(), [BINDING_ID]);
}

#[test]
fn a_fresh_press_during_the_capture_is_reported_as_the_binding() {
    let session = open_binding_editor();
    focus_editor(&session);
    press(&session.ui, Key::Return);

    press_text(&session.ui, "k");

    assert_eq!(session.captured.borrow().as_slice(), ["k"]);
}

#[test]
fn enter_and_space_can_be_bound_by_a_fresh_press() {
    let session = open_binding_editor();
    focus_editor(&session);
    press(&session.ui, Key::Return);

    press(&session.ui, Key::Return);
    press_text(&session.ui, " ");

    assert_eq!(
        session.captured.borrow().as_slice(),
        [SharedString::from(Key::Return).as_str(), " "]
    );
}

#[test]
fn other_keys_do_not_start_the_capture_while_the_editor_is_idle() {
    let session = open_binding_editor();
    focus_editor(&session);

    press_text(&session.ui, "k");
    press(&session.ui, Key::Escape);

    assert!(session.begun.borrow().is_empty());
    assert!(session.captured.borrow().is_empty());
}

struct Session {
    ui: AppWindow,
    /// The row ids the window asked to start capturing.
    begun: Rc<RefCell<Vec<String>>>,
    /// The key reports the window forwarded as the binding.
    captured: Rc<RefCell<Vec<String>>>,
}

/// A Minecraft defaults page with one key binding row, wired the way the
/// controller wires it: beginning a capture names the capturing row.
fn open_binding_editor() -> Session {
    let ui = window();
    let begun: Rc<RefCell<Vec<String>>> = Rc::default();
    let captured: Rc<RefCell<Vec<String>>> = Rc::default();

    let record = Rc::clone(&begun);
    let handle = ui.as_weak();
    ui.on_minecraft_begin_capture(move |id| {
        record.borrow_mut().push(id.to_string());
        if let Some(ui) = handle.upgrade() {
            ui.set_minecraft_capturing_id(id);
        }
    });
    let record = Rc::clone(&captured);
    ui.on_minecraft_captured_key(move |_, reported| record.borrow_mut().push(reported.to_string()));

    let row = MinecraftPreferenceRow {
        id: BINDING_ID.into(),
        label: BINDING_LABEL.into(),
        editor: "keybinding".into(),
        binding_label: "Left Shift".into(),
        ..MinecraftPreferenceRow::default()
    };
    ui.set_minecraft_preference_rows(ModelRc::new(VecModel::from(vec![row])));
    ui.invoke_open_settings("Minecraft".into());
    settle(&ui);
    Session {
        ui,
        begun,
        captured,
    }
}

/// Presses Tab until the editor draws its focused border, or focus has gone
/// all the way around without reaching it.
fn focus_editor(session: &Session) {
    let editor = editor(&session.ui);
    let edge = border_pixel(&session.ui, &editor);
    let reached = (0..TAB_LIMIT).any(|_| {
        press(&session.ui, Key::Tab);
        border_pixel(&session.ui, &editor) != edge
    });
    assert!(reached, "Tab never reached the key binding editor");
}

fn editor(ui: &AppWindow) -> ElementHandle {
    ElementQuery::from_root(ui)
        .match_descendants()
        .match_type_name("PreferenceKeyBindingEditor")
        .find_first()
        .expect("the page has a key binding editor")
}

/// The pixel on the editor's left border, halfway down. The border takes the
/// accent color while the editor has focus.
#[allow(clippy::cast_possible_truncation)]
fn border_pixel(ui: &AppWindow, editor: &ElementHandle) -> Rgba8Pixel {
    let at = editor.absolute_position();
    let size = editor.size();
    let snapshot: SharedPixelBuffer<Rgba8Pixel> = ui.window().take_snapshot().expect("render");
    let column = usize::try_from(at.x.round() as i32).expect("column");
    let row = usize::try_from((at.y + size.height / 2.0).round() as i32).expect("row");
    snapshot.as_slice()[row * snapshot.width() as usize + column]
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
