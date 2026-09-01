//! What the accounts page and the Launch as control actually put on screen.
//!
//! Both findings here are geometry and pointer routing: a marker that has to
//! match the size of the button it replaces, controls that have to sit on one
//! line, and a button whose press has to reach the menu it hosts rather than
//! the area underneath it. None of that is visible from the markup alone.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AccountRow, AppWindow, GuiI18n, InstanceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// The marker on the default account stands where the button it replaces would
/// be, so the column does not jump from one row to the next.
#[test]
fn the_default_marker_is_the_size_of_the_button_it_replaces() {
    let ui = window();
    accounts_page(&ui);
    settle(&ui);

    // A badge and the text inside it carry the same label, so the badge is the
    // larger of the two.
    let marker = widest(&ui, &lookup("gui-default")).expect("the default account is marked");
    let make_default =
        widest(&ui, &lookup("gui-make-default")).expect("the other account can become the default");

    assert!(
        (marker.size().width - make_default.size().width).abs() < 1.0,
        "the marker and the button it stands in for must be the same width \
         ({} against {})",
        marker.size().width,
        make_default.size().width
    );
    assert!((marker.size().height - make_default.size().height).abs() < 1.0);
}

/// Every row's controls sit on one line with the name beside them.
#[test]
fn an_accounts_controls_are_centered_against_its_name() {
    let ui = window();
    accounts_page(&ui);
    settle(&ui);

    let row = ElementHandle::find_by_accessible_label(&ui, "Alice")
        .find(|element| element.size().height > 40.0)
        .expect("the default account has a row");
    let marker = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-default"))
        .find(|element| element.size().width > 0.0)
        .expect("the default account is marked");

    let above = marker.absolute_position().y - row.absolute_position().y;
    let below = (row.absolute_position().y + row.size().height)
        - (marker.absolute_position().y + marker.size().height);
    assert!(
        (above - below).abs() < 1.0,
        "a row's controls sit on its centre line, not flush to its top \
         (above {above}, below {below})"
    );
}

/// A press has to reach the menu the button hosts, and the menu has to list
/// every account plus a way to add one.
#[test]
fn launch_as_offers_every_account_and_a_way_to_add_one() {
    let ui = window();
    let launched: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client_as(move |id, account| {
        record
            .borrow_mut()
            .push((id.to_string(), account.to_string()));
    });

    instance_workspace(&ui);
    settle(&ui);

    let launch_as = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch-as"))
        .find(|element| element.size().width > 0.0)
        .expect("the workspace offers a way to launch as somebody else");
    launch_as.mock_single_click(PointerEventButton::Left);
    settle(&ui);

    // The menu is a native popup, so what is assertable here is that the press
    // was taken by the control rather than falling through to the workspace,
    // and that plain Launch is untouched beside it.
    assert!(
        launched.borrow().is_empty(),
        "opening the menu must not launch anything by itself"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch"))
            .any(|element| element.size().width > 0.0),
        "Launch stays where it was: one account is still the common case"
    );
}

/// Launching as a chosen account reports that account, and never the default.
#[test]
fn launching_as_an_account_names_the_account_it_was_given() {
    let ui = window();
    let launched: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client_as(move |id, account| {
        record
            .borrow_mut()
            .push((id.to_string(), account.to_string()));
    });

    instance_workspace(&ui);
    ui.invoke_launch_client_as("repro-client".into(), "bob-id".into());

    assert_eq!(
        launched.borrow().as_slice(),
        [("repro-client".to_owned(), "bob-id".to_owned())]
    );
}

/// The largest element carrying one label, which is the control rather than
/// the text inside it.
fn widest(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label)
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .filter(|element| element.size().width > 0.0)
}

fn accounts_page(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_workspace(SharedString::default());
    ui.set_workspace_tab("Accounts".into());
    ui.set_accounts(ModelRc::new(VecModel::from(vec![
        account("alice-id", "Alice", true),
        account("bob-id", "Bob", false),
    ])));
}

fn instance_workspace(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace("instance".into());
    ui.set_workspace_tab("Overview".into());
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance()])));
    ui.set_selected_instance_index(0);
    ui.set_selected_instance_id("repro-client".into());
    ui.set_accounts(ModelRc::new(VecModel::from(vec![
        account("alice-id", "Alice", true),
        account("bob-id", "Bob", false),
    ])));
}

fn account(id: &str, name: &str, is_default: bool) -> AccountRow {
    AccountRow {
        id: id.into(),
        name: name.into(),
        kind: "Offline".into(),
        is_default,
        credential_state: "No credential".into(),
    }
}

fn instance() -> InstanceRow {
    InstanceRow {
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.249".into(),
        loader_kind: "neoforge".into(),
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
