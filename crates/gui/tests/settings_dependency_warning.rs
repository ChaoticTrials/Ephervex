//! The two answers that decide what a mod's switch does to other mods.
//!
//! The dialog records "do not ask again" where it is given, which makes
//! turning the warning off one click away and turning it back on unreachable
//! unless the settings screen carries the same switch. Beside it is the
//! question the warning is about at all: whether a change acts on other mods.

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
    ui.set_settings_tab("General".into());
    ui.set_follow_mod_dependencies(true);
}

/// One setting's switch, which shares its label with the card it sits in.
fn switch_named(ui: &AppWindow, id: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, &lookup(id))
        .find(|element| element.accessible_checked().is_some())
        .unwrap_or_else(|| panic!("the settings screen carries no switch for {id}"))
}

fn switch(ui: &AppWindow) -> ElementHandle {
    switch_named(ui, "gui-dependency-warning")
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

#[test]
fn the_confirmation_is_unavailable_while_nothing_else_is_being_changed() {
    // With a change acting on the named mod alone there is nothing for a
    // confirmation to be about. The row stays, greyed, because a setting that
    // vanished would read as one that was lost.
    let ui = window();
    open_general_settings(&ui);
    ui.set_warn_on_dependency_impact(true);
    ui.on_set_dependency_warning(|_| {});

    assert!(is_on(&ui), "following relations, the question is asked");
    ui.set_follow_mod_dependencies(false);
    assert!(
        !is_on(&ui),
        "nothing is carried along, so nothing is confirmed"
    );
    assert!(
        !switch(&ui).accessible_enabled().expect("a switch says so"),
        "and the switch says it cannot be answered from here"
    );
}

#[test]
fn following_relations_can_be_turned_off_from_settings() {
    let ui = window();
    open_general_settings(&ui);

    let asked = Rc::new(Cell::new(None));
    let recorded = Rc::clone(&asked);
    ui.on_set_dependency_following(move |follow| recorded.set(Some(follow)));

    switch_named(&ui, "gui-follow-dependencies").mock_single_click(PointerEventButton::Left);
    assert_eq!(
        asked.get(),
        Some(false),
        "a mod set arranged by hand is a thing this launcher is for"
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
