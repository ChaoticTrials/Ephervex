//! What a text field does with a value that is longer than the field.
//!
//! Paths and JVM arguments have no length anybody can rely on, while the field
//! that holds them is as wide as the column. A `TextInput` neither clips nor
//! scrolls by itself, so the field around it has to: the value must not be
//! drawn through the border, and the caret must stay where the user can see it.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, PhysicalSize, Rgba8Pixel, SharedPixelBuffer};

const LONG_VALUE: &str = "/home/melanx/Games/Minecraft/Ephervex/persistent-instances/\
    garden-of-glass-questbook-edition/saves/reproduction-of-the-crash-on-world-load/\
    region-files-copied-from-the-reporter";

/// Gap between the field and the button beside it in the storage settings row.
const GAP_BESIDE_FIELD: usize = 8;

/// How far the hard shadow of a field that has focus falls past its right edge.
const FOCUS_SHADOW_REACH: usize = 3;

#[test]
fn a_long_value_is_not_drawn_past_the_border_of_its_field() {
    let ui = window();
    storage_settings(&ui);
    let field = storage_root_field(&ui);
    let empty = gap_beside(&field, &ui);

    ui.set_persistent_root(LONG_VALUE.into());
    settle(&ui);

    assert!(
        gap_beside(&field, &ui) == empty,
        "the value has to be cut at the border instead of being drawn into the row beside the field"
    );
}

#[test]
fn a_long_value_is_not_drawn_past_the_border_after_the_caret_moves_to_its_end() {
    let ui = window();
    storage_settings(&ui);
    let field = storage_root_field(&ui);
    let empty = gap_beside(&field, &ui);

    ui.set_persistent_root(LONG_VALUE.into());
    focus_and_press(&ui, &field, Key::End);

    assert!(
        gap_beside(&field, &ui) == empty,
        "scrolling a value has to keep it inside the field as well"
    );
}

#[test]
fn moving_the_caret_to_the_end_of_a_long_value_scrolls_the_field_to_it() {
    let ui = window();
    storage_settings(&ui);
    let field = storage_root_field(&ui);
    ui.set_persistent_root(LONG_VALUE.into());
    settle(&ui);
    let start = input_of(&field).absolute_position().x;

    focus_and_press(&ui, &field, Key::End);

    let input = input_of(&field);
    let field_right = field.absolute_position().x + field.size().width;
    assert!(
        input.absolute_position().x < start,
        "the text has to move left to bring its end into view"
    );
    assert!(
        input.absolute_position().x + input.size().width <= field_right,
        "the end of the value, where the caret is, has to be inside the border: it reaches {}px of {field_right}px",
        input.absolute_position().x + input.size().width
    );
}

#[test]
fn moving_the_caret_back_to_the_start_scrolls_the_field_back() {
    let ui = window();
    storage_settings(&ui);
    let field = storage_root_field(&ui);
    ui.set_persistent_root(LONG_VALUE.into());
    settle(&ui);
    let start = input_of(&field).absolute_position().x;

    focus_and_press(&ui, &field, Key::End);
    press(&ui, Key::Home);

    assert_eq!(
        input_of(&field).absolute_position().x,
        start,
        "the start of the value has to be back at the left edge of the field"
    );
}

#[test]
fn a_shorter_value_does_not_leave_the_field_scrolled() {
    let ui = window();
    storage_settings(&ui);
    let field = storage_root_field(&ui);
    ui.set_persistent_root(LONG_VALUE.into());
    settle(&ui);
    let start = input_of(&field).absolute_position().x;
    focus_and_press(&ui, &field, Key::End);

    ui.set_persistent_root("/tmp".into());
    settle(&ui);

    assert_eq!(
        input_of(&field).absolute_position().x,
        start,
        "a value that fits has to be shown from its first character"
    );
}

#[test]
fn a_value_that_fits_keeps_the_field_unscrolled_with_the_caret_at_its_end() {
    let ui = window();
    storage_settings(&ui);
    let field = storage_root_field(&ui);
    ui.set_persistent_root("/tmp".into());
    settle(&ui);
    let start = input_of(&field).absolute_position().x;

    focus_and_press(&ui, &field, Key::End);

    assert_eq!(
        input_of(&field).absolute_position().x,
        start,
        "a value that fits has nothing to scroll"
    );
}

fn press(ui: &AppWindow, key: Key) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    settle(ui);
}

fn focus_and_press(ui: &AppWindow, field: &ElementHandle, key: Key) {
    input_of(field).mock_single_click(slint::platform::PointerEventButton::Left);
    settle(ui);
    press(ui, key);
}

/// The strip of the row between the field and the button beside it, past the
/// shadow a focused field casts. It is outside the field's border and so holds
/// nothing that belongs to the field.
fn gap_beside(field: &ElementHandle, ui: &AppWindow) -> Vec<Rgba8Pixel> {
    let snapshot: SharedPixelBuffer<Rgba8Pixel> = ui.window().take_snapshot().expect("render");
    let width = snapshot.width() as usize;
    let left = pixel(field.absolute_position().x + field.size().width) + FOCUS_SHADOW_REACH;
    let top = pixel(field.absolute_position().y);
    let rows = pixel(field.size().height);
    (top..top + rows)
        .flat_map(|y| {
            (left..left + GAP_BESIDE_FIELD - FOCUS_SHADOW_REACH).map(move |x| y * width + x)
        })
        .map(|index| snapshot.as_slice()[index])
        .collect()
}

/// A layout coordinate as a pixel index. The window is not scaled, and every
/// coordinate read here is on screen, so it is neither negative nor large.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pixel(coordinate: f32) -> usize {
    coordinate.round() as usize
}

fn storage_root_field(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-no-override"))
        .filter(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
        .expect("the storage root has a text field")
}

fn input_of(field: &ElementHandle) -> ElementHandle {
    field
        .query_descendants()
        .match_inherits("TextInput")
        .find_first()
        .expect("a text field wraps a text input")
}

fn storage_settings(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_settings_tab("Storage".into());
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
