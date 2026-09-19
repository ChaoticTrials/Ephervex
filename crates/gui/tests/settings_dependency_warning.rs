//! Taking back the mod dialog's "do not ask again".
//!
//! The dialog records that answer where it is given, which makes turning the
//! warning off one click away and turning it back on unreachable unless the
//! settings screen carries the same switch.

#![allow(clippy::expect_used, clippy::panic)]

use std::cell::Cell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, PhysicalSize};

fn open_general_settings(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_workspace_tab("General".into());
}

/// The switch itself, which shares its label with the card it sits in.
fn switch(ui: &AppWindow) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-dependency-warning"))
        .find(|element| element.accessible_checked().is_some())
        .expect("the settings screen carries the warning's own switch")
}

fn is_on(ui: &AppWindow) -> bool {
    switch(ui).accessible_checked().expect("a switch says so")
}

#[test]
fn the_warning_can_be_turned_back_on_from_settings() {
    let ui = window();
    open_general_settings(&ui);
    ui.set_warn_on_dependency_impact(false);

    let asked = Rc::new(Cell::new(None));
    let recorded = Rc::clone(&asked);
    ui.on_set_dependency_warning(move |warn| recorded.set(Some(warn)));

    assert!(
        !is_on(&ui),
        "the switch shows what is recorded, which is off"
    );
    switch(&ui).mock_single_click(PointerEventButton::Left);
    assert_eq!(
        asked.get(),
        Some(true),
        "clicking it asks for the warning back"
    );
}

#[test]
fn the_switch_shows_the_stored_answer_rather_than_the_last_click() {
    // A save can fail, and the settings screen is refreshed for reasons that
    // have nothing to do with this switch. Either way its position comes from
    // the stored value.
    let ui = window();
    open_general_settings(&ui);
    ui.set_warn_on_dependency_impact(true);
    ui.on_set_dependency_warning(|_| {});

    switch(&ui).mock_single_click(PointerEventButton::Left);
    assert!(
        is_on(&ui),
        "nothing was recorded yet, so the switch stays where the setting is"
    );

    ui.set_warn_on_dependency_impact(false);
    assert!(
        !is_on(&ui),
        "once the change is stored, the switch follows it"
    );
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
        lookup_args(id.as_str(), [("count", Arg::from(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
    ui.window().set_size(PhysicalSize::new(1480, 1200));
    ui.show().expect("show");
    ui
}
