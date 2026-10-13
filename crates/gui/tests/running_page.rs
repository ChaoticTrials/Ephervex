//! What the Running page offers, and which control each action reaches.
//!
//! A running game is not an operation, so it does not belong on the Activity
//! page and gets its own. What is worth proving here is that the card's own
//! area does not swallow its buttons, that the two ways to end a client launch
//! stay distinct, and that a server is offered a console where a client is not.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, RunningRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn the_running_page_is_its_own_entry_and_asks_for_the_list_when_opened() {
    let ui = window();
    let asked: Rc<RefCell<usize>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_load_running(move || *record.borrow_mut() += 1);

    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace_open(false);
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-running"))
        .find(|element| element.size().width > 0.0)
        .expect("Running is offered in the navigation")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(ui.get_page(), 2);
    assert_eq!(
        *asked.borrow(),
        1,
        "opening the page asks for what is running"
    );
}

#[test]
fn an_idle_launcher_says_so_rather_than_showing_an_empty_list() {
    let ui = window();
    running_page(&ui, Vec::new());
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-nothing-running"))
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

    running_page(&ui, vec![client()]);
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
    running_page(&ui, vec![orphan]);
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-stop-game"))
        .find(|element| element.size().width > 0.0)
        .expect("the control is shown, so its state is readable")
        .mock_single_click(PointerEventButton::Left);
    assert!(
        killed.borrow().is_empty(),
        "canceling a launch this run does not own would do nothing"
    );
    assert_eq!(
        ui.get_page(),
        2,
        "a press on a greyed-out control must not navigate away from the page"
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
    running_page(&ui, vec![client()]);
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-open-console"))
            .all(|element| element.size().width == 0.0),
        "a client has no console"
    );

    let stopped: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&stopped);
    ui.on_server_stop(move |id| record.borrow_mut().push(id.to_string()));

    running_page(&ui, vec![server()]);
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

    running_page(&ui, vec![server()]);
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
    running_page(&ui, vec![server()]);
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

fn running_page(ui: &AppWindow, rows: Vec<RunningRow>) {
    ui.set_loading(false);
    ui.set_page(2);
    ui.set_workspace_open(false);
    ui.set_running(ModelRc::new(VecModel::from(rows)));
}

fn client() -> RunningRow {
    RunningRow {
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
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
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
