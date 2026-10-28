//! Where a hover note opens.
//!
//! A control at the window's trailing or bottom edge has to show its note
//! inside the window, not run it off the edge.

#![allow(clippy::expect_used)]

mod support;

use std::time::Duration;

use ephervex_gui::ui::{AppWindow, GuiI18n, ModSearchRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions, mock_elapsed_time,
};
use slint::platform::WindowEvent;
use slint::{
    ComponentHandle, Image, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel,
};

const WINDOW_WIDTH: u16 = 1480;
const WINDOW_HEIGHT: u16 = 900;

/// The Install button of a result the server cannot take, which is disabled
/// and explains why on hover.
fn blocked_install(ui: &AppWindow, reason: &str) -> ElementHandle {
    support::show_server(
        ui,
        ServerRow {
            id: "repro-server".into(),
            name: "Repro Server".into(),
            installed: true,
            loader_kind: "neoforge".into(),
            state: "Stopped".into(),
            state_kind: "stopped".into(),
            ..ServerRow::default()
        },
        "Mods",
    );
    support::update_host(ui, 0, |view| {
        view.browser_provider = "modrinth".into();
        view.browser_results = ModelRc::new(VecModel::from(vec![ModSearchRow {
            project_id: "sodium".into(),
            title: "Sodium".into(),
            description: "Nice to have items, blocks, and other features.".into(),
            author: "MelanX".into(),
            note: "Compatible".into(),
            downloads: SharedString::default(),
            icon: Image::default(),
            installed: false,
            queued: false,
            server_support: "client-only".into(),
            blocked_reason: reason.into(),
        }]));
    });
    ui.set_add_mods_host(0);
    ui.set_modal("add-mods".into());
    settle(ui);
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-install"))
        .find(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .expect("the row offers Install")
}

fn hover(ui: &AppWindow, x: f32, y: f32) {
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
    mock_elapsed_time(Duration::from_millis(600));
    settle(ui);
}

/// The note currently drawn for this text, if any.
fn note(ui: &AppWindow, text: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, text).find(|element| {
        element.size().width > 0.0 && element.accessible_role() != Some(AccessibleRole::Button)
    })
}

#[test]
fn a_note_for_a_control_at_the_trailing_edge_opens_inside_the_window() {
    // Narrow enough that the dialog holding the button reaches the window's
    // trailing edge.
    const NARROW: u16 = 1000;
    let ui = window();
    ui.window().set_size(slint::PhysicalSize::new(
        u32::from(NARROW),
        u32::from(WINDOW_HEIGHT),
    ));
    let reason = lookup("gui-hint-mod-client-only");
    let button = blocked_install(&ui, &reason);
    let (at, size) = (button.absolute_position(), button.size());
    assert!(
        at.x + size.width > f32::from(NARROW) - 320.0,
        "the button has to sit where a note opened to the right would not fit"
    );

    hover(&ui, at.x + size.width - 4.0, at.y + size.height / 2.0);

    let shown = note(&ui, &reason).expect("hovering a disabled button explains why");
    assert!(shown.absolute_position().x >= 0.0);
    assert!(
        shown.absolute_position().x + shown.size().width <= f32::from(NARROW),
        "the note ends at {}px in a window {NARROW}px wide",
        shown.absolute_position().x + shown.size().width
    );
}

#[test]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn a_note_for_a_control_near_the_bottom_edge_opens_inside_the_window() {
    let ui = window();
    let reason = lookup("gui-hint-mod-client-only");
    let button = blocked_install(&ui, &reason);
    // A window that ends just below the button leaves no room under the pointer.
    let height = button.absolute_position().y + button.size().height + 30.0;
    ui.window()
        .set_size(PhysicalSize::new(u32::from(WINDOW_WIDTH), height as u32));
    settle(&ui);
    let (at, size) = (button.absolute_position(), button.size());

    hover(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0);

    let shown = note(&ui, &reason).expect("hovering a disabled button explains why");
    assert!(shown.absolute_position().y >= 0.0);
    assert!(
        shown.absolute_position().y + shown.size().height <= height,
        "the note ends at {}px in a window {height}px tall",
        shown.absolute_position().y + shown.size().height
    );
}

#[test]
fn a_note_opens_beside_the_pointer_when_there_is_room() {
    let ui = window();
    // Short enough to fit between the pointer and the window's edge.
    let reason = "Not here";
    let button = blocked_install(&ui, reason);
    let (at, size) = (button.absolute_position(), button.size());
    let pointer = (at.x + 4.0, at.y + size.height / 2.0);

    hover(&ui, pointer.0, pointer.1);

    let shown = note(&ui, reason).expect("hovering a disabled button explains why");
    assert!(
        shown.absolute_position().x >= pointer.0,
        "with room to the right the note starts at the pointer, not left of it"
    );
    assert!(
        shown.absolute_position().y > pointer.1,
        "the note sits below the pointer, clear of the control's own caption"
    );
}

#[test]
fn a_note_closes_when_the_pointer_leaves_its_control() {
    let ui = window();
    let reason = lookup("gui-hint-mod-client-only");
    let button = blocked_install(&ui, &reason);
    let (at, size) = (button.absolute_position(), button.size());
    hover(&ui, at.x + size.width / 2.0, at.y + size.height / 2.0);
    assert!(note(&ui, &reason).is_some(), "the note opens first");

    hover(&ui, 2.0, 2.0);

    assert!(
        note(&ui, &reason).is_none(),
        "a note left behind would cover whatever is under it"
    );
}

#[test]
fn a_note_is_not_shown_before_the_pointer_has_rested() {
    let ui = window();
    let reason = lookup("gui-hint-mod-client-only");
    let button = blocked_install(&ui, &reason);
    let (at, size) = (button.absolute_position(), button.size());

    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0),
    });
    mock_elapsed_time(Duration::from_millis(100));
    settle(&ui);

    assert!(
        note(&ui, &reason).is_none(),
        "a pointer passing over a control does not flash its note"
    );
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
    ui.window().set_size(PhysicalSize::new(
        u32::from(WINDOW_WIDTH),
        u32::from(WINDOW_HEIGHT),
    ));
    ui.show().expect("show");
    ui
}
