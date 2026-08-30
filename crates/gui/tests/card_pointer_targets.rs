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

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// One window, because the testing backend is a per-process platform and a
/// second one would panic. Both card kinds are exercised inside it.
#[test]
fn a_card_button_is_clickable_across_its_whole_face() {
    // A software renderer, because the pointer positions a click is resolved
    // against only exist once the window has actually been laid out and drawn.
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");

    let ui = AppWindow::new().expect("window");
    install_i18n(&ui);
    // Wide enough that the action column and the card body are far apart, which
    // is the arrangement the overlap used to hide in.
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");

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
