//! What a pointer event on a list card actually reaches.
//!
//! A card carries both its own selection area and the buttons that act on it.
//! Which one a click lands on is decided by the markup's sibling order and by
//! the geometry Slint gives an element that sets a size without a position, and
//! neither is visible from reading the callbacks alone. So the window is built
//! for real and clicked for real.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, SharedString, VecModel};

/// One window per test thread. Both card kinds are exercised in the first.
#[test]
fn a_card_button_is_clickable_across_its_whole_face() {
    let ui = window();

    let launched: Rc<RefCell<Vec<String>>> = Rc::default();
    let chosen: Rc<RefCell<Vec<String>>> = Rc::default();
    let started: Rc<RefCell<Vec<String>>> = Rc::default();

    let record = Rc::clone(&launched);
    ui.on_launch_client(move |id| record.borrow_mut().push(id.to_string()));
    let record = Rc::clone(&chosen);
    ui.on_select_instance(move |id| record.borrow_mut().push(id.to_string()));
    let record = Rc::clone(&started);
    ui.on_server_start(move |id| record.borrow_mut().push(id.to_string()));

    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace(SharedString::default());
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));

    settle(&ui);
    let launch = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch"))
        .next()
        .expect("the card offers Launch");
    launch.mock_single_click(PointerEventButton::Left);

    assert_eq!(
        launched.borrow().as_slice(),
        ["repro-client"],
        "a click on the middle of Launch must launch"
    );
    assert!(
        chosen.borrow().is_empty(),
        "the card's own selection area must not sit over its buttons"
    );

    // The same arrangement on the servers page, whose primary action shares the
    // card shape.
    ui.set_page(1);
    ui.set_global_servers(ModelRc::new(VecModel::from(vec![server()])));

    let start = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-start"))
        .next()
        .expect("the card offers Start");
    start.mock_single_click(PointerEventButton::Left);

    assert_eq!(started.borrow().as_slice(), ["repro-server"]);
}

/// Renders once, which is what instantiates the cards a repeater holds and
/// gives every element the geometry a pointer event is resolved against.
fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

/// A running instance offers a stop where a ready one offers a launch, and the
/// two are not each other's negation: an instance that needs attention offers
/// neither.
#[test]
fn a_running_instance_offers_a_kill_in_place_of_a_launch() {
    let ui = window();
    let killed: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&killed);
    ui.on_kill_instance(move |id| record.borrow_mut().push(id.to_string()));

    let mut running = instance();
    running.can_launch = false;
    running.can_kill = true;
    running.state = "Running".into();
    running.state_kind = "running".into();

    ui.set_loading(false);
    ui.set_page(0);
    ui.set_instances(ModelRc::new(VecModel::from(vec![running])));
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch"))
            .next()
            .is_none(),
        "a running instance is not launchable"
    );
    let kill = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-kill-instance"))
        .next()
        .expect("the card offers a stop");
    kill.mock_single_click(PointerEventButton::Left);
    assert_eq!(killed.borrow().as_slice(), ["repro-client"]);
}

/// A laid-out, drawn window on this thread's own testing backend.
///
/// Software-rendered, because the pointer positions a click is resolved against
/// only exist once the window has been laid out and drawn. Wide enough that a
/// card's action column and its body are far apart, which is the arrangement the
/// touch-area overlap used to hide in.
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

/// A card opens its workspace on a double click.
///
/// The two presses have to reach the same touch area: a list model replaced
/// between them rebuilds every card, and the second press then lands on an
/// element that never saw the first, which reads as two ordinary clicks.
#[test]
fn double_clicking_a_card_opens_its_workspace() {
    let ui = window();
    let opened: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_load_instance_workspace(move |id| record.borrow_mut().push(id.to_string()));

    ui.set_loading(false);
    ui.set_page(0);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    settle(&ui);

    let card = ElementHandle::find_by_accessible_label(&ui, "Repro Client")
        .next()
        .expect("the list holds the card");
    double_click(&ui, &card);

    assert_eq!(opened.borrow().as_slice(), ["repro-client"]);
    assert_eq!(ui.get_workspace(), "instance");
}

/// Two presses inside the platform's double-click interval, at the element's
/// own center. The testing backend has no event loop, so the wait between them
/// is mocked rather than real.
fn double_click(ui: &AppWindow, element: &ElementHandle) {
    let position = LogicalPosition::new(
        element.absolute_position().x + element.size().width / 2.0,
        element.absolute_position().y + element.size().height / 2.0,
    );
    for _ in 0..2 {
        ui.window()
            .dispatch_event(WindowEvent::PointerMoved { position });
        ui.window().dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(20));
        ui.window().dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }
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

fn instance() -> InstanceRow {
    InstanceRow {
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/repro-client".into(),
        updated: "Just now".into(),
        linked_servers: 0,
        mods: 0,
        needs_attention: false,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: SharedString::default(),
        connect_server_name: SharedString::default(),
        can_connect: false,
    }
}

fn server() -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        linked_client_id: SharedString::default(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        state_detail: "Not running".into(),
        address: "127.0.0.1:25565".into(),
        path: "/tmp/ephervex/servers/repro-server".into(),
        installed: true,
        can_start: true,
        can_stop: false,
        can_restart: false,
        can_kill: false,
        can_send_command: false,
        can_connect: false,
        connect_reason: SharedString::default(),
        needs_attention: false,
    }
}
