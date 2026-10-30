//! The links on the About page open through the application.

use crate::support;

use std::cell::Cell;
use std::rc::Rc;

use ephervex_gui::ui::AppWindow;
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, PhysicalSize};

fn about() -> AppWindow {
    let ui = support::test_window();
    support::open_settings(&ui, "About & Licenses");
    ui.window().set_size(PhysicalSize::new(1500, 1200));
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
    ui
}

fn click(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.size().width > 0.0)
        .expect("a control with this label")
        .mock_single_click(PointerEventButton::Left);
}

#[test]
fn the_source_button_asks_the_application_to_open_the_source_page() {
    let ui = about();
    let opened = Rc::new(Cell::new(0));
    let sink = Rc::clone(&opened);
    ui.on_open_source_page(move || sink.set(sink.get() + 1));

    click(&ui, &lookup("gui-source"));

    assert_eq!(opened.get(), 1);
}

#[test]
fn clicking_the_slint_attribution_asks_the_application_to_open_the_slint_page() {
    let ui = about();
    let opened = Rc::new(Cell::new(0));
    let sink = Rc::clone(&opened);
    ui.on_open_slint_page(move || sink.set(sink.get() + 1));

    click(&ui, &lookup("gui-open-slint-page"));

    assert_eq!(
        opened.get(),
        1,
        "the attribution is opened by the application, like Source"
    );
}
