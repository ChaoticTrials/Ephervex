//! The quick view: a host shown over the window, with the page beneath it
//! left as it was.

use crate::support;

use std::cell::Cell;
use std::rc::Rc;

use ephervex_gui::ui::{InstanceRow, WorkspacePaneRow};
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

fn page_with_quick_view() -> ephervex_gui::ui::AppWindow {
    let ui = support::test_window();
    ui.window().set_size(PhysicalSize::new(1700, 1000));
    let row = InstanceRow {
        id: "client".into(),
        name: "Belt render flicker".into(),
        minecraft: "1.21.1".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        ..InstanceRow::default()
    };
    support::show_client(&ui, row, "Case");
    let page = WorkspacePaneRow {
        id: 1,
        host: 0,
        log: -1,
        surface: "Case".into(),
        label: "page".into(),
        ..WorkspacePaneRow::default()
    };
    let quick = WorkspacePaneRow {
        id: 1 << 20,
        host: 0,
        log: -1,
        surface: "Overview".into(),
        label: "quick".into(),
        can_close: true,
        overlay: true,
        ..WorkspacePaneRow::default()
    };
    ui.set_workspace_panes(ModelRc::new(VecModel::from(vec![page, quick])));
    ui.set_quick_view_open(true);
    slint::platform::update_timers_and_animations();
    ui
}

fn button(ui: &ephervex_gui::ui::AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.size().width > 0.0)
        .expect("a control with this label")
}

#[test]
fn the_quick_view_covers_the_right_part_of_the_window_and_leaves_the_page_in_place() {
    let ui = page_with_quick_view();
    let quick = ElementHandle::find_by_accessible_label(&ui, "quick")
        .next()
        .expect("the quick view");
    let page = ElementHandle::find_by_accessible_label(&ui, "page")
        .next()
        .expect("the page");
    assert!(quick.absolute_position().x > 700.0, "over the right side");
    assert!(
        quick.absolute_position().x + quick.size().width <= 1700.0,
        "inside the window"
    );
    assert!(
        page.size().width > 0.0 && page.absolute_position().x < quick.absolute_position().x,
        "the page keeps its place beneath"
    );
}

#[test]
fn fullscreen_takes_the_window_and_leaving_it_returns_to_the_side() {
    let ui = page_with_quick_view();
    let side_width = ElementHandle::find_by_accessible_label(&ui, "quick")
        .next()
        .expect("the quick view")
        .size()
        .width;

    button(&ui, &lookup("gui-quick-view-fullscreen")).invoke_accessible_default_action();
    assert!(ui.get_quick_view_fullscreen());
    slint::platform::update_timers_and_animations();
    let full_width = ElementHandle::find_by_accessible_label(&ui, "quick")
        .next()
        .expect("the quick view")
        .size()
        .width;
    assert!(full_width > side_width, "{full_width} beside {side_width}");

    button(&ui, &lookup("gui-quick-view-exit-fullscreen")).invoke_accessible_default_action();
    assert!(!ui.get_quick_view_fullscreen());
}

#[test]
fn closing_and_opening_as_a_page_are_asked_of_the_controller() {
    let ui = page_with_quick_view();
    let closed = Rc::new(Cell::new(0));
    let paged = Rc::new(Cell::new(0));
    let count = Rc::clone(&closed);
    ui.on_close_quick_view(move || count.set(count.get() + 1));
    let count = Rc::clone(&paged);
    ui.on_open_quick_view_as_page(move || count.set(count.get() + 1));

    button(&ui, &lookup("gui-quick-view-close")).invoke_accessible_default_action();
    button(&ui, &lookup("gui-quick-view-open-as-page")).invoke_accessible_default_action();

    assert_eq!((closed.get(), paged.get()), (1, 1));
}

fn press_escape(ui: &ephervex_gui::ui::AppWindow) {
    let text: slint::SharedString = Key::Escape.into();
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
    slint::platform::update_timers_and_animations();
}

#[test]
fn escape_leaves_fullscreen_first_and_then_closes_the_quick_view() {
    let ui = page_with_quick_view();
    let closed = Rc::new(Cell::new(0));
    let count = Rc::clone(&closed);
    ui.on_close_quick_view(move || count.set(count.get() + 1));
    // Showing a host puts keyboard focus where Escape reaches it.
    ui.invoke_show_quick_view("client".into(), "client".into());
    ui.set_quick_view_fullscreen(true);

    press_escape(&ui);
    assert!(!ui.get_quick_view_fullscreen());
    assert_eq!(closed.get(), 0, "the quick view stays open");

    press_escape(&ui);
    assert_eq!(closed.get(), 1);
}

#[test]
fn escape_leaves_the_quick_view_alone_while_a_dialog_is_open() {
    let ui = page_with_quick_view();
    let closed = Rc::new(Cell::new(0));
    let count = Rc::clone(&closed);
    ui.on_close_quick_view(move || count.set(count.get() + 1));
    ui.invoke_show_quick_view("client".into(), "client".into());
    ui.set_modal("create-instance".into());

    press_escape(&ui);
    assert_eq!(closed.get(), 0);
}
