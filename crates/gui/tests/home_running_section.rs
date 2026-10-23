//! What Home's Running section offers, and which control each action reaches.
//!
//! A running game is not an operation, so it is listed apart from the activity.
//! What is worth proving here is that the card's own area does not swallow its
//! buttons, that the two ways to end a client launch stay distinct, and that a
//! server is offered a console where a client is not.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, RunningRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

#[test]
fn home_asks_for_what_is_running_each_time_it_appears_and_measures_the_cache_once() {
    let ui = window();
    let asked: Rc<RefCell<usize>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_load_running(move || *record.borrow_mut() += 1);
    let measured: Rc<RefCell<usize>> = Rc::default();
    let record = Rc::clone(&measured);
    ui.on_measure_artifact_cache(move || *record.borrow_mut() += 1);

    support::open_settings(&ui, "General");
    settle(&ui);
    assert_eq!(
        *asked.borrow(),
        0,
        "the settings do not read what is running"
    );

    support::open_home(&ui);
    settle(&ui);
    assert_eq!(*asked.borrow(), 1, "Home asks for what is running");
    assert_eq!(*measured.borrow(), 1);

    support::open_settings(&ui, "General");
    settle(&ui);
    let after_settings = *measured.borrow();
    support::open_home(&ui);
    settle(&ui);
    assert_eq!(*asked.borrow(), 2, "every visit asks again");
    assert_eq!(
        *measured.borrow(),
        after_settings,
        "the cache is not measured again on a later visit"
    );
}

#[test]
fn home_is_shown_when_neither_the_settings_nor_a_host_is_open() {
    let ui = window();
    support::open_home(&ui);
    assert!(ui.get_shows_home());

    support::open_settings(&ui, "General");
    assert!(!ui.get_shows_home());

    ui.invoke_open_home();
    assert!(!ui.get_settings_open());
    assert!(ui.get_shows_home());
}

#[test]
fn an_idle_launcher_says_so_rather_than_showing_an_empty_list() {
    let ui = window();
    running_section(&ui, Vec::new());
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-nothing-running-detail"))
            .next()
            .is_some()
    );
}

/// The card carries its own press area, two buttons and a menu, and the
/// pointer has to reach the control rather than the card underneath it.
#[test]
fn a_clients_two_ways_to_stop_are_offered_separately() {
    let ui = window();
    let killed: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&killed);
    ui.on_kill_instance(move |id, launch| {
        record.borrow_mut().push(format!("{id}#{launch}"));
    });
    let forced: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&forced);
    ui.on_force_stop_launch(move |id, _launch| record.borrow_mut().push(id.to_string()));
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_workspace(move |_kind, id, _surface| record.borrow_mut().push(id.to_string()));

    running_section(&ui, vec![client()]);
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &named("gui-home-stop-host", CLIENT))
        .find(|element| element.size().width > 0.0)
        .expect("a supervised launch can be stopped")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(killed.borrow().as_slice(), ["repro-client#"]);
    assert!(
        forced.borrow().is_empty(),
        "the ordinary stop must not be the bigger hammer"
    );
    assert!(
        opened.borrow().is_empty(),
        "the card's own area must not sit over its buttons"
    );

    open_more(&ui, CLIENT);
    menu_entry(&ui, "gui-force-stop")
        .expect("a launch nobody is supervising still has to be endable")
        .invoke_accessible_default_action();
    assert_eq!(forced.borrow().as_slice(), ["repro-client"]);
}

/// A launch this run is not supervising cannot be canceled, and says so by
/// offering only the hammer.
#[test]
fn an_unsupervised_launch_cannot_be_stopped_the_ordinary_way() {
    let ui = window();
    let killed: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&killed);
    ui.on_kill_instance(move |id, launch| {
        record.borrow_mut().push(format!("{id}#{launch}"));
    });

    let mut orphan = client();
    orphan.can_stop = false;
    running_section(&ui, vec![orphan]);
    settle(&ui);

    let stop = ElementHandle::find_by_accessible_label(&ui, &named("gui-home-stop-host", CLIENT))
        .find(|element| element.size().width > 0.0)
        .expect("the control is shown, so its state is readable");
    assert_eq!(stop.accessible_enabled(), Some(false));
    stop.mock_single_click(PointerEventButton::Left);
    assert!(
        killed.borrow().is_empty(),
        "canceling a launch this run does not own would do nothing"
    );
    assert!(
        ui.get_shows_home(),
        "a press on a greyed-out control must not navigate away from Home"
    );
    open_more(&ui, CLIENT);
    assert!(
        menu_entry(&ui, "gui-force-stop").is_some(),
        "which is exactly when the bigger hammer is the only way out"
    );
}

/// A console is a server's, and a client has none to send commands to.
#[test]
fn only_a_server_is_offered_a_console() {
    let ui = window();
    running_section(&ui, vec![client()]);
    settle(&ui);
    open_more(&ui, CLIENT);
    assert!(
        menu_entry(&ui, "gui-open-console").is_none(),
        "a client has no console"
    );
    assert!(menu_entry(&ui, "gui-open-log").is_some());

    let stopped: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&stopped);
    ui.on_server_stop(move |id| record.borrow_mut().push(id.to_string()));
    let consoles: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&consoles);
    ui.on_open_workspace(move |kind, id, surface| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), surface.to_string()));
    });

    running_section(&ui, vec![server()]);
    settle(&ui);
    open_more(&ui, SERVER);
    assert!(
        menu_entry(&ui, "gui-force-stop").is_none(),
        "a server is supervised through its own control channel, not a launch lock"
    );
    menu_entry(&ui, "gui-open-console")
        .expect("a server has a console")
        .invoke_accessible_default_action();
    settle(&ui);
    assert_eq!(
        consoles.borrow().as_slice(),
        [(
            "server".to_owned(),
            "repro-server".to_owned(),
            "Console".to_owned()
        )]
    );

    running_section(&ui, vec![server()]);
    settle(&ui);
    ElementHandle::find_by_accessible_label(&ui, &named("gui-home-stop-host", SERVER))
        .find(|element| element.size().width > 0.0)
        .expect("a controlled server can be stopped")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(stopped.borrow().as_slice(), ["repro-server"]);
}

/// Opening a run's log opens the workspace that owns it, on its Logs surface.
#[test]
fn opening_a_runs_log_opens_its_own_workspace_on_logs() {
    let ui = window();
    let opened: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_workspace(move |kind, id, surface| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), surface.to_string()));
    });

    running_section(&ui, vec![server()]);
    settle(&ui);
    open_more(&ui, SERVER);
    menu_entry(&ui, "gui-open-log")
        .expect("every run offers its log")
        .invoke_accessible_default_action();

    assert_eq!(
        opened.borrow().as_slice(),
        [(
            "server".to_owned(),
            "repro-server".to_owned(),
            "Logs".to_owned()
        )]
    );
}

/// Open, and a press anywhere else on the card, open the host on its Overview.
#[test]
fn open_and_a_press_on_the_card_open_the_host() {
    let ui = window();
    let opened: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_workspace(move |kind, id, surface| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), surface.to_string()));
    });
    running_section(&ui, vec![client()]);
    settle(&ui);
    let overview = (
        "client".to_owned(),
        "repro-client".to_owned(),
        "Overview".to_owned(),
    );

    ElementHandle::find_by_accessible_label(&ui, &named("gui-home-open-host", CLIENT))
        .find(|element| element.size().width > 0.0)
        .expect("a run can be opened")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(opened.borrow().len(), 1);

    ElementHandle::find_by_accessible_label(&ui, "pid 48213  |  214% CPU | 6.1 GiB")
        .find(|element| element.size().width > 0.0)
        .expect("the card shows what it knows of the process")
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(opened.borrow().as_slice(), [overview.clone(), overview]);
}

/// The card says what it runs and for how long, and nothing it has no data for.
#[test]
fn a_card_shows_its_versions_uptime_and_process() {
    let ui = window();
    running_section(&ui, vec![client()]);
    settle(&ui);

    for text in [
        "1.20.1",
        "Forge 47.3.0",
        "12m",
        "pid 48213  |  214% CPU | 6.1 GiB",
    ] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, text)
                .any(|element| element.size().width > 0.0),
            "the card shows {text}"
        );
    }
}

/// The process line puts its separator between two parts only.
#[test]
fn the_process_line_has_no_dangling_separator() {
    let ui = window();
    let mut first = client();
    first.usage = SharedString::default();
    let mut second = server();
    second.pid = SharedString::default();
    running_section(&ui, vec![first, second]);
    settle(&ui);

    for text in ["pid 48213", "61% CPU | 2.4 GiB"] {
        assert!(
            ElementHandle::find_by_accessible_label(&ui, text)
                .any(|element| element.size().width > 0.0),
            "the line is {text} and nothing more"
        );
    }
}

const CLIENT: &str = "All the Mods 11";
const SERVER: &str = "All the Mods 11 Server";

fn open_more(ui: &AppWindow, host: &str) {
    ElementHandle::find_by_accessible_label(ui, &named("gui-home-more-host", host))
        .find(|element| element.size().width > 0.0)
        .expect("a run offers more actions")
        .mock_single_click(PointerEventButton::Left);
    settle(ui);
}

fn named(key: &str, host: &str) -> String {
    lookup_args(key, [("value", Arg::from(host.to_owned()))])
}

/// The row of an open menu carrying the label of `key`.
fn menu_entry(ui: &AppWindow, key: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup(key)).find(|element| {
        element.accessible_role() == Some(AccessibleRole::Button) && element.size().width > 0.0
    })
}

fn running_section(ui: &AppWindow, rows: Vec<RunningRow>) {
    support::open_home(ui);
    ui.set_running(ModelRc::new(VecModel::from(rows)));
}

fn client() -> RunningRow {
    RunningRow {
        icon_slot: -1,
        kind: "client".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.3.0".into(),
        loader_kind: "forge".into(),
        id: "repro-client".into(),
        launch: SharedString::default(),
        name: CLIENT.into(),
        pid: "48213".into(),
        uptime: "12m".into(),
        usage: "214% CPU | 6.1 GiB".into(),
        last_line: "[12:13:02] [Render thread/INFO]: Sound engine started".into(),
        controllable: false,
        can_stop: true,
    }
}

fn server() -> RunningRow {
    RunningRow {
        icon_slot: -1,
        kind: "server".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.3.0".into(),
        loader_kind: "forge".into(),
        id: "repro-server".into(),
        launch: SharedString::default(),
        name: SERVER.into(),
        pid: "48999".into(),
        uptime: "2h1m".into(),
        usage: "61% CPU | 2.4 GiB".into(),
        last_line: "[12:04:51] [Server thread/INFO]: Preparing spawn area: 82%".into(),
        controllable: true,
        can_stop: true,
    }
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window() -> AppWindow {
    support::test_window()
}
