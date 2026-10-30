//! The slider a numeric Minecraft default is edited with.
//!
//! The row it sits in re-renders whenever the stored value changes, so the
//! slider reports a change only once the pointer is released or a key is
//! pressed, and a drag held in the meantime must not be lost.

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, MinecraftPreferenceRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};

type Reports = Rc<RefCell<Vec<(String, f32)>>>;

#[test]
fn a_keyboard_step_reports_the_neighboring_value() {
    let (ui, reports) = window_with_slider(70.0);
    let slider = slider(&ui);
    assert_eq!(slider.accessible_value_minimum(), Some(30.0));
    assert_eq!(slider.accessible_value_maximum(), Some(110.0));
    assert_eq!(slider.accessible_value_step(), Some(1.0));

    slider.invoke_accessible_increment_action();
    slider.invoke_accessible_decrement_action();

    assert_eq!(
        *reports.borrow(),
        [("fov".to_owned(), 71.0), ("fov".to_owned(), 70.0)]
    );
}

#[test]
fn a_drag_is_reported_once_at_the_value_under_the_pointer_when_it_is_released() {
    let (ui, reports) = window_with_slider(70.0);
    let slider = slider(&ui);
    let (origin, size) = (slider.absolute_position(), slider.size());
    let y = origin.y + size.height / 2.0;
    let at = |fraction: f32| LogicalPosition::new(origin.x + size.width * fraction, y);

    let window = ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position: at(0.2) });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: at(0.2),
        button: PointerEventButton::Left,
    });
    // The scroll area around the rows holds a press back for a moment before
    // it reaches the slider.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(200));
    window.dispatch_event(WindowEvent::PointerMoved { position: at(0.6) });
    window.dispatch_event(WindowEvent::PointerMoved { position: at(1.5) });
    assert!(reports.borrow().is_empty(), "reported mid-drag");
    // The handle follows the pointer without the stored value moving.
    assert_eq!(slider.accessible_value().as_deref(), Some("110"));

    window.dispatch_event(WindowEvent::PointerReleased {
        position: at(1.5),
        button: PointerEventButton::Left,
    });
    assert_eq!(*reports.borrow(), [("fov".to_owned(), 110.0)]);
}

#[test]
fn taking_hold_of_the_handle_does_not_move_it() {
    let (ui, reports) = window_with_slider(70.0);
    let slider = slider(&ui);
    let (origin, size) = (slider.absolute_position(), slider.size());
    // The handle is 16px wide and 70 sits at the middle of 30 to 110, so a
    // press 6px from its center is still on it.
    let position = LogicalPosition::new(
        origin.x + size.width / 2.0 + 6.0,
        origin.y + size.height / 2.0,
    );
    let window = ui.window();
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(200));
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });

    assert_eq!(*reports.borrow(), [("fov".to_owned(), 70.0)]);
}

#[test]
fn assistive_technology_can_set_the_value_directly() {
    let (ui, reports) = window_with_slider(70.0);

    slider(&ui).set_accessible_value("55");

    assert_eq!(*reports.borrow(), [("fov".to_owned(), 55.0)]);
}

#[test]
fn an_assistive_value_between_steps_is_reported_on_the_nearest_step() {
    let (ui, reports) = window_with_slider(70.0);
    let slider = slider(&ui);

    slider.set_accessible_value("55.4");
    slider.set_accessible_value("500");

    assert_eq!(
        *reports.borrow(),
        [("fov".to_owned(), 55.0), ("fov".to_owned(), 110.0)]
    );
    assert_eq!(slider.accessible_value().as_deref(), Some("110"));
}

#[test]
fn keys_move_a_focused_slider_to_either_end_of_its_range() {
    let (ui, reports) = window_with_slider(70.0);
    let slider = slider(&ui);
    let (origin, size) = (slider.absolute_position(), slider.size());
    // A press focuses the slider without moving it far from where it is.
    let position = LogicalPosition::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
    let window = ui.window();
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    reports.borrow_mut().clear();

    for key in [Key::Home, Key::End] {
        window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    }

    assert_eq!(
        *reports.borrow(),
        [("fov".to_owned(), 30.0), ("fov".to_owned(), 110.0)]
    );
}

#[test]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn the_filled_part_of_the_track_runs_from_its_start_to_the_handle() {
    let (ui, _reports) = window_with_slider(70.0);
    let slider = slider(&ui);
    let (origin, size) = (slider.absolute_position(), slider.size());
    let snapshot = ui.window().take_snapshot().expect("render");
    let handle_size = 20.0;
    let travel = size.width - handle_size;
    let pixel_at = |x: f32| {
        let y = origin.y + size.height / 2.0;
        let width = snapshot.width() as usize;
        let index = y.round() as usize * width + x.round() as usize;
        snapshot.as_slice()[index]
    };
    let along = |fraction: f32| origin.x + handle_size / 2.0 + travel * fraction;

    // The value 70 of 30..110 puts the handle halfway along the track. The
    // fill starts inside the track's outline, a few pixels from its start.
    let near_start = pixel_at(origin.x + handle_size / 2.0 + 4.0);
    let before_handle = pixel_at(along(0.4));
    let after_handle = pixel_at(along(0.75));
    assert_eq!(
        near_start, before_handle,
        "the fill starts at the track start"
    );
    assert_ne!(before_handle, after_handle, "the fill stops at the handle");

    // The track's outline runs over the filled part too: the middle of the
    // track's top edge is ink where the fill is, not fill.
    let outline = ui.global::<Theme>().get_border_strong();
    let top_edge = {
        let x = along(0.4).round() as usize;
        let y = (origin.y + size.height / 2.0).round() as usize - 5;
        snapshot.as_slice()[y * snapshot.width() as usize + x]
    };
    assert_eq!(
        (top_edge.r, top_edge.g, top_edge.b),
        (outline.red(), outline.green(), outline.blue()),
        "the outline stays whole over the fill"
    );
}

fn slider(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, "Field of view")
        .find(|element| element.accessible_role() == Some(AccessibleRole::Slider))
        .expect("the slider")
}

fn window_with_slider(value: f32) -> (AppWindow, Reports) {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    ui.show().expect("show");
    ui.set_loading(false);
    ui.invoke_open_settings("Minecraft".into());
    let row = MinecraftPreferenceRow {
        id: "fov".into(),
        label: "Field of view".into(),
        editor: "slider".into(),
        number_min: 30.0,
        number_max: 110.0,
        number_step: 1.0,
        number_value: value,
        ..MinecraftPreferenceRow::default()
    };
    ui.set_minecraft_preference_rows(ModelRc::new(VecModel::from(vec![row])));
    let reports = Reports::default();
    let sink = Rc::clone(&reports);
    ui.on_minecraft_set_number(move |id, value| sink.borrow_mut().push((id.to_string(), value)));
    settle(&ui);
    (ui, reports)
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
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
