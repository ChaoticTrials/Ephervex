//! The host list before its rows arrive and while the background service is
//! down: the search row keeps its place, the spinner sits under its label, and
//! nothing claims an empty list it cannot know.

#![allow(clippy::expect_used)]

mod support;

use ephervex_gui::ui::{AppWindow, InstanceRow};
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, VecModel};

#[derive(Clone, Copy, Debug)]
enum State {
    Loading,
    Loaded,
    ServiceDown,
}

/// Where the search field and the Filters button beside it sit.
fn search_row(ui: &AppWindow) -> Vec<f32> {
    [lookup("gui-filter-hosts"), lookup("gui-more-filters")]
        .iter()
        .map(|label| {
            ElementHandle::find_by_accessible_label(ui, label)
                .filter(|element| element.size().width > 0.0)
                .map(|element| element.absolute_position().y)
                .reduce(f32::min)
                .expect("every row above the list is on screen")
        })
        .collect()
}

#[test]
fn the_search_row_stays_at_the_top_in_every_state() {
    let ui = window();
    enter(&ui, State::Loaded);
    let expected = search_row(&ui);

    for state in [State::Loading, State::ServiceDown] {
        let ui = window();
        enter(&ui, state);
        for (at, wanted) in search_row(&ui).into_iter().zip(&expected) {
            assert!(
                (at - wanted).abs() < 1.0,
                "{state:?}: search row at {at}px instead of {wanted}px"
            );
        }
    }
}

#[test]
fn the_loading_spinner_is_centered_over_its_label() {
    let ui = window();
    enter(&ui, State::Loading);
    let spinner = ElementHandle::find_by_element_type_name(&ui, "BusySpinner")
        .find(|element| element.size().width > 0.0)
        .expect("a spinner while loading");
    let text = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-loading-instances"))
        .find(|element| element.size().width > 0.0)
        .expect("the loading label");

    let spinner_center = spinner.absolute_position().x + spinner.size().width / 2.0;
    let text_center = text.absolute_position().x + text.size().width / 2.0;
    assert!(
        (spinner_center - text_center).abs() < 1.0,
        "spinner centered at {spinner_center}px, label at {text_center}px"
    );
}

#[test]
fn the_list_does_not_claim_there_is_nothing_before_it_knows() {
    for state in [State::Loading, State::ServiceDown] {
        let ui = window();
        enter(&ui, state);
        assert!(
            !shows(&ui, &lookup("gui-no-client-instances")),
            "{state:?}: the list says there are no instances"
        );
    }

    let ui = window();
    enter(&ui, State::Loaded);
    assert!(shows(&ui, &lookup("gui-no-client-instances")));
}

#[test]
fn a_loaded_list_names_its_sections_and_an_empty_one_says_so() {
    let ui = window();
    enter(&ui, State::Loaded);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_instance_total(1);
    settle(&ui);

    assert!(shows(&ui, "Repro Client"));
    assert!(shows(&ui, &lookup("gui-instances").to_uppercase()));
    assert!(shows(&ui, &lookup("gui-servers").to_uppercase()));
    assert!(
        shows(&ui, &lookup("gui-host-list-no-servers")),
        "a section with nothing in it says so"
    );
}

/// A list that loaded before the service stopped is still a real list.
#[test]
fn a_list_loaded_before_the_service_stopped_keeps_its_rows() {
    let ui = window();
    enter(&ui, State::Loaded);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_instance_total(1);
    ui.set_startup_error("The background service stopped.".into());
    settle(&ui);

    assert!(shows(&ui, "Repro Client"), "the list itself stays");
}

fn enter(ui: &AppWindow, state: State) {
    match state {
        State::Loading => ui.set_loading(true),
        State::Loaded => ui.set_loading(false),
        State::ServiceDown => {
            ui.set_loading(false);
            ui.set_startup_error("The launcher index database reported an error.".into());
        }
    }
    settle(ui);
}

fn shows(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

fn instance() -> InstanceRow {
    InstanceRow {
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.20.1".into(),
        loader: "Vanilla".into(),
        loader_kind: "vanilla".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        icon_slot: -1,
        ..InstanceRow::default()
    }
}

fn window() -> AppWindow {
    let ui = support::test_window();
    support::open_home(&ui);
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}
