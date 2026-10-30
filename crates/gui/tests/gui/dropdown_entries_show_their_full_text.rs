//! What a dropdown does with an entry that does not fit its width.
//!
//! A dropdown cuts a long entry off with an ellipsis, in the closed box and in
//! the open list. Hovering the cut entry shows all of it, and an entry that
//! fits has nothing more to say.

use std::time::Duration;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions, mock_elapsed_time,
};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

const LONG_ENTRY: &str =
    "Review: Linked branding folder with a name too long for any dropdown to show in one line";

fn open_dialog(ui: &AppWindow, selected: &str) {
    open_dialog_with(ui, selected, &["0.16.0", LONG_ENTRY, "0.15.10"]);
}

fn open_dialog_with(ui: &AppWindow, selected: &str, entries: &[&str]) {
    ui.set_loading(false);
    ui.set_loader_change_loading(false);
    ui.set_loader_change_versions(ModelRc::new(VecModel::from(
        entries
            .iter()
            .map(|entry| SharedString::from(*entry))
            .collect::<Vec<_>>(),
    )));
    ui.set_loader_change_selected(selected.into());
    ui.set_modal("loader-change".into());
    settle(ui);
}

fn target(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-loader-change-target"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("the dialog has a target dropdown")
}

/// How many elements on screen carry this text.
fn drawn(ui: &AppWindow, text: &str) -> usize {
    ElementHandle::find_by_accessible_label(ui, text)
        .filter(|element| element.size().width > 0.0)
        .count()
}

fn hover(ui: &AppWindow, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
    mock_elapsed_time(Duration::from_millis(600));
    settle(ui);
}

/// The middle of the open list's entry for `text`. The list opens just below
/// the dropdown, and an entry reports its place in the list.
fn entry_center(ui: &AppWindow, text: &str) -> (f32, f32) {
    let closed = target(ui);
    let entry = ElementHandle::find_by_accessible_label(ui, text)
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .expect("the open list has the entry");
    let (list, at, size) = (
        closed.absolute_position(),
        entry.absolute_position(),
        entry.size(),
    );
    (
        list.x + at.x + size.width / 2.0,
        list.y + closed.size().height + 4.0 + at.y + size.height / 2.0,
    )
}

#[test]
fn hovering_a_cut_off_value_in_the_closed_box_shows_all_of_it() {
    let ui = window();
    open_dialog(&ui, LONG_ENTRY);
    let before = drawn(&ui, LONG_ENTRY);
    let (at, size) = (target(&ui).absolute_position(), target(&ui).size());

    hover(&ui, at.x + size.width / 3.0, at.y + size.height / 2.0);

    assert_eq!(
        drawn(&ui, LONG_ENTRY),
        before + 1,
        "the full value opens as a note"
    );
}

#[test]
fn hovering_a_value_that_fits_the_closed_box_shows_nothing_more() {
    let ui = window();
    open_dialog(&ui, "0.16.0");
    let before = drawn(&ui, "0.16.0");
    let (at, size) = (target(&ui).absolute_position(), target(&ui).size());

    hover(&ui, at.x + size.width / 3.0, at.y + size.height / 2.0);

    assert_eq!(drawn(&ui, "0.16.0"), before);
}

#[test]
fn the_note_of_a_closed_box_closes_when_the_pointer_leaves() {
    let ui = window();
    open_dialog(&ui, LONG_ENTRY);
    let before = drawn(&ui, LONG_ENTRY);
    let (at, size) = (target(&ui).absolute_position(), target(&ui).size());
    hover(&ui, at.x + size.width / 3.0, at.y + size.height / 2.0);
    assert_eq!(drawn(&ui, LONG_ENTRY), before + 1);

    hover(&ui, 2.0, 2.0);

    assert_eq!(drawn(&ui, LONG_ENTRY), before);
}

#[test]
fn hovering_a_cut_off_entry_of_the_open_list_shows_all_of_it_inside_the_list() {
    let ui = window();
    open_dialog(&ui, "0.16.0");
    target(&ui).invoke_accessible_expand_action();
    settle(&ui);
    let before = drawn(&ui, LONG_ENTRY);
    let (x, y) = entry_center(&ui, LONG_ENTRY);

    hover(&ui, x, y);

    assert_eq!(
        drawn(&ui, LONG_ENTRY),
        before + 1,
        "the full entry opens as a note over the list"
    );
    // The entry's own caption is as tall as its row; the note is one line of
    // small text. A popup is placed in its own coordinates, so the note's
    // position is measured from the list's left edge.
    let row = ElementHandle::find_by_accessible_label(&ui, LONG_ENTRY)
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .expect("the list has the entry")
        .size()
        .height;
    let note = ElementHandle::find_by_accessible_label(&ui, LONG_ENTRY)
        .find(|element| {
            element.accessible_role() == Some(AccessibleRole::Text) && element.size().height < row
        })
        .expect("the note is drawn");
    let (left, note_width) = (note.absolute_position().x, note.size().width);
    assert!(left >= 0.0, "the note starts {left}px outside the list");
    assert!(
        left + note_width <= target(&ui).size().width,
        "a {note_width}px note starting at {left}px runs past a {}px list",
        target(&ui).size().width
    );
}

#[test]
fn the_note_of_the_last_entry_stays_inside_the_list_whose_end_is_the_windows_edge() {
    let ui = window();
    open_dialog_with(&ui, "0.16.0", &["0.16.0", "0.15.10", LONG_ENTRY]);
    target(&ui).invoke_accessible_expand_action();
    settle(&ui);
    let (x, y) = entry_center(&ui, LONG_ENTRY);

    hover(&ui, x, y);

    let row = ElementHandle::find_by_accessible_label(&ui, LONG_ENTRY)
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .expect("the list has the entry");
    // The list's padding below its last entry.
    let list_end = row.absolute_position().y + row.size().height + 4.0;
    let note = ElementHandle::find_by_accessible_label(&ui, LONG_ENTRY)
        .find(|element| {
            element.accessible_role() == Some(AccessibleRole::Text)
                && element.size().height < row.size().height
        })
        .expect("the note is drawn");
    assert!(
        note.absolute_position().y + note.size().height <= list_end,
        "the note ends at {}px in a list that ends at {list_end}px",
        note.absolute_position().y + note.size().height
    );
}

#[test]
fn hovering_an_entry_of_the_open_list_that_fits_shows_nothing_more() {
    let ui = window();
    open_dialog(&ui, "0.16.0");
    target(&ui).invoke_accessible_expand_action();
    settle(&ui);
    let before = drawn(&ui, "0.15.10");
    let (x, y) = entry_center(&ui, "0.15.10");

    hover(&ui, x, y);

    assert_eq!(drawn(&ui, "0.15.10"), before);
}

#[test]
fn the_note_of_the_open_list_closes_when_the_pointer_moves_to_another_entry() {
    let ui = window();
    open_dialog(&ui, "0.16.0");
    target(&ui).invoke_accessible_expand_action();
    settle(&ui);
    let before = drawn(&ui, LONG_ENTRY);
    let (x, y) = entry_center(&ui, LONG_ENTRY);
    hover(&ui, x, y);
    assert_eq!(drawn(&ui, LONG_ENTRY), before + 1);

    let (x, y) = entry_center(&ui, "0.15.10");
    hover(&ui, x, y);

    assert_eq!(drawn(&ui, LONG_ENTRY), before);
}

#[test]
fn the_note_of_the_open_list_is_gone_when_the_list_closes() {
    let ui = window();
    open_dialog(&ui, "0.16.0");
    target(&ui).invoke_accessible_expand_action();
    settle(&ui);
    let before = drawn(&ui, LONG_ENTRY);
    let (x, y) = entry_center(&ui, LONG_ENTRY);
    hover(&ui, x, y);
    assert_eq!(drawn(&ui, LONG_ENTRY), before + 1);

    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: slint::platform::Key::Escape.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: slint::platform::Key::Escape.into(),
    });
    settle(&ui);

    target(&ui).invoke_accessible_expand_action();
    settle(&ui);
    assert_eq!(
        drawn(&ui, LONG_ENTRY),
        before,
        "a reopened list starts without a note"
    );
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
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
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}
