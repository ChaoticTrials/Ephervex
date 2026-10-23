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
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
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

/// The card carries its own click target and four buttons, and the pointer has
/// to reach the button rather than the card underneath it.
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

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-stop-game"))
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

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-force-stop"))
        .find(|element| element.size().width > 0.0)
        .expect("a launch nobody is supervising still has to be endable")
        .mock_single_click(PointerEventButton::Left);
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

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-stop-game"))
        .find(|element| element.size().width > 0.0)
        .expect("the control is shown, so its state is readable")
        .mock_single_click(PointerEventButton::Left);
    assert!(
        killed.borrow().is_empty(),
        "canceling a launch this run does not own would do nothing"
    );
    assert!(
        ui.get_shows_home(),
        "a press on a greyed-out control must not navigate away from Home"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-force-stop"))
            .any(|element| element.size().width > 0.0),
        "which is exactly when the bigger hammer is the only way out"
    );
}

/// A console is a server's, and a client has none to send commands to.
#[test]
fn only_a_server_is_offered_a_console() {
    let ui = window();
    running_section(&ui, vec![client()]);
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-console"))
            .all(|element| element.size().width == 0.0),
        "a client has no console"
    );

    let stopped: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&stopped);
    ui.on_server_stop(move |id| record.borrow_mut().push(id.to_string()));

    running_section(&ui, vec![server()]);
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-console"))
            .any(|element| element.size().width > 0.0)
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-force-stop"))
            .all(|element| element.size().width == 0.0),
        "a server is supervised through its own control channel, not a launch lock"
    );

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-stop"))
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

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-log"))
        .find(|element| element.size().width > 0.0)
        .expect("every run offers its log")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        opened.borrow().as_slice(),
        [(
            "server".to_owned(),
            "repro-server".to_owned(),
            "Logs".to_owned()
        )]
    );
}

/// The badge says what the row is and the buttons say what can be done to it.
/// Read on one line they compete for the same corner, so the badge sits above
/// the buttons and against the same right edge.
#[test]
fn a_rows_badge_sits_above_its_buttons_against_the_same_edge() {
    let ui = window();
    running_section(&ui, vec![server()]);
    settle(&ui);

    let badge = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-server"))
        .find(|element| element.size().width > 0.0)
        .expect("a server row says it is a server");
    let stop = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-stop"))
        .find(|element| element.size().width > 0.0)
        .expect("a running server can be stopped");

    let badge_bottom = badge.absolute_position().y + badge.size().height;
    assert!(
        badge_bottom <= stop.absolute_position().y,
        "the badge ends at {badge_bottom}px, below the buttons at {}px",
        stop.absolute_position().y
    );
    let badge_right = badge.absolute_position().x + badge.size().width;
    let stop_right = stop.absolute_position().x + stop.size().width;
    assert!(
        (badge_right - stop_right).abs() < 1.0,
        "the badge ends at {badge_right}px and the buttons at {stop_right}px"
    );
}

fn running_section(ui: &AppWindow, rows: Vec<RunningRow>) {
    support::open_home(ui);
    ui.set_running(ModelRc::new(VecModel::from(rows)));
}

fn client() -> RunningRow {
    RunningRow {
        icon_slot: -1,
        kind: "client".into(),
        id: "repro-client".into(),
        launch: SharedString::default(),
        name: "All the Mods 11".into(),
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
        id: "repro-server".into(),
        launch: SharedString::default(),
        name: "All the Mods 11 Server".into(),
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
