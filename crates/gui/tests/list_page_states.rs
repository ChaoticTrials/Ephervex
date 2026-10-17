//! The Instances and Servers pages before their lists arrive and while the
//! background service is down: the header keeps its place, the spinner sits
//! under its label, and nothing claims a count or an empty list it cannot know.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};

const INSTANCES: i32 = 0;
const SERVERS: i32 = 1;

#[derive(Clone, Copy, Debug)]
enum State {
    Loading,
    Loaded,
    ServiceDown,
}

/// Where the page title, its subtitle and the first control under them sit.
fn header(ui: &AppWindow, page: i32) -> Vec<f32> {
    let (title, subtitle, control) = if page == INSTANCES {
        ("gui-instances", "gui-instances-detail", "gui-all")
    } else {
        ("gui-servers", "gui-servers-detail", "gui-filter-servers")
    };
    [title, subtitle, control]
        .into_iter()
        .map(|id| {
            ElementHandle::find_by_accessible_label(ui, &lookup(id))
                .filter(|element| element.size().width > 0.0)
                .map(|element| element.absolute_position().y)
                .reduce(f32::min)
                .expect("every header row is on the page")
        })
        .collect()
}

#[test]
fn the_page_header_stays_at_the_top_in_every_state() {
    for page in [INSTANCES, SERVERS] {
        let ui = window(page);
        enter(&ui, State::Loaded);
        let expected = header(&ui, page);

        for state in [State::Loading, State::ServiceDown] {
            let ui = window(page);
            enter(&ui, state);
            for (at, wanted) in header(&ui, page).into_iter().zip(&expected) {
                assert!(
                    (at - wanted).abs() < 1.0,
                    "page {page} {state:?}: header row at {at}px instead of {wanted}px"
                );
            }
        }
    }
}

#[test]
fn the_loading_spinner_is_centered_over_its_label() {
    for (page, label) in [
        (INSTANCES, "gui-loading-instances"),
        (SERVERS, "gui-loading-servers"),
    ] {
        let ui = window(page);
        enter(&ui, State::Loading);
        let spinner = ElementHandle::find_by_element_type_name(&ui, "BusySpinner")
            .find(|element| element.size().width > 0.0)
            .expect("a spinner while loading");
        let text = ElementHandle::find_by_accessible_label(&ui, &lookup(label))
            .find(|element| element.size().width > 0.0)
            .expect("the loading label");

        let spinner_center = spinner.absolute_position().x + spinner.size().width / 2.0;
        let text_center = text.absolute_position().x + text.size().width / 2.0;
        assert!(
            (spinner_center - text_center).abs() < 1.0,
            "page {page}: spinner centered at {spinner_center}px, label at {text_center}px"
        );
    }
}

#[test]
fn no_instance_count_is_shown_before_the_list_has_loaded() {
    let none = lookup_args("gui-client-instances-count", [("count", Arg::Count(0))]);
    for state in [State::Loading, State::ServiceDown] {
        let ui = window(INSTANCES);
        enter(&ui, state);
        assert!(
            !shows(&ui, &none),
            "{state:?}: the header counts a list that never arrived"
        );
    }

    let ui = window(INSTANCES);
    enter(&ui, State::Loaded);
    assert!(shows(&ui, &none), "a loaded empty list is counted");
}

/// A list that loaded before the service stopped is still a real list.
#[test]
fn a_list_loaded_before_the_service_stopped_keeps_its_count() {
    let ui = window(INSTANCES);
    enter(&ui, State::Loaded);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_instance_total(1);
    ui.set_startup_error("The background service stopped.".into());
    settle(&ui);

    let one = lookup_args("gui-client-instances-count", [("count", Arg::Count(1))]);
    assert!(shows(&ui, &one));
    assert!(shows(&ui, "Repro Client"), "the list itself stays");
}

#[test]
fn the_servers_page_does_not_claim_no_servers_before_it_knows() {
    for state in [State::Loading, State::ServiceDown] {
        let ui = window(SERVERS);
        enter(&ui, state);
        assert!(
            !shows(&ui, &lookup("gui-no-servers")),
            "{state:?}: the page says there are no servers"
        );
    }

    let ui = window(SERVERS);
    enter(&ui, State::Loaded);
    assert!(shows(&ui, &lookup("gui-no-servers")));
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

fn window(page: i32) -> AppWindow {
    // The platform is per thread and can be set only once.
    let _ = slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })));
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui.set_page(page);
    ui.set_workspace_open(false);
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

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}
