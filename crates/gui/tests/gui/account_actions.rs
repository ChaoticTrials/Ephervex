//! What the accounts page and the Launch as control actually put on screen.
//!
//! Both findings here are geometry and pointer routing: a marker that has to
//! match the size of the button it replaces, controls that have to sit on one
//! line, and a button whose press has to reach the menu it hosts rather than
//! the area underneath it. None of that is visible from the markup alone.

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AccountRow, AppWindow, GuiI18n, InstanceRow, MenuRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// The default account says so with a badge, and offers Make Default only on
/// the accounts that are not the default.
#[test]
fn only_an_account_that_is_not_the_default_offers_to_become_it() {
    let ui = window();
    accounts_page(&ui);
    settle(&ui);

    assert!(
        widest(&ui, &lookup("gui-default")).is_some(),
        "the default account is marked"
    );
    let offers = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-make-default"))
        .filter(|element| {
            element.size().width > 0.0
                && element.accessible_role()
                    == Some(i_slint_backend_testing::AccessibleRole::Button)
        })
        .count();
    assert_eq!(offers, 1, "only Bob can become the default");
}

/// Every row's controls sit on one line with the name beside them.
#[test]
fn an_accounts_controls_are_centered_against_its_row() {
    let ui = window();
    accounts_page(&ui);
    settle(&ui);

    let row = ElementHandle::find_by_accessible_label(&ui, "Alice")
        .find(|element| element.size().height > 40.0)
        .expect("the default account has a row");
    let remove = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-remove"))
        .find(|element| {
            element.size().width > 0.0
                && element.absolute_position().y >= row.absolute_position().y
                && element.absolute_position().y < row.absolute_position().y + row.size().height
        })
        .expect("the row offers Remove");

    let above = remove.absolute_position().y - row.absolute_position().y;
    let below = (row.absolute_position().y + row.size().height)
        - (remove.absolute_position().y + remove.size().height);
    assert!(
        (above - below).abs() < 2.0,
        "a row's controls sit on its center line, not flush to its top \
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
    ui.on_launch_client_as(move |id, account, _alongside| {
        record
            .borrow_mut()
            .push((id.to_string(), account.to_string()));
    });

    instance_workspace(&ui);
    ui.set_account_launch_entries(ModelRc::new(VecModel::from(vec![
        MenuRow {
            id: "alice-id".into(),
            label: "Alice".into(),
            ..MenuRow::default()
        },
        MenuRow {
            id: "bob-id".into(),
            label: "Bob".into(),
            ..MenuRow::default()
        },
    ])));
    settle(&ui);

    let launch_as = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch-as"))
        .find(|element| element.size().width > 0.0)
        .expect("the workspace offers a way to launch as somebody else");
    launch_as.mock_single_click(PointerEventButton::Left);
    settle(&ui);

    // The press was taken by the control rather than falling through to the
    // workspace, plain Launch is untouched beside it, and the menu it opened
    // lists every account and a way to add one.
    assert!(
        launched.borrow().is_empty(),
        "opening the menu must not launch anything by itself"
    );
    for label in ["Alice", "Bob"] {
        assert!(menu_row(&ui, label), "the menu lists the account {label}");
    }
    assert!(
        menu_row(&ui, &lookup("gui-add-account")),
        "and ends with a way to add one"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-launch"))
            .any(|element| element.size().width > 0.0),
        "Launch stays where it was: one account is still the common case"
    );
}

/// A running instance can still be launched as somebody else -- that is a
/// second client beside the first, which is the whole point of the control.
/// What it cannot do is start again as the same account.
#[test]
fn a_running_instance_can_still_be_launched_as_someone_else() {
    let ui = window();
    let mut running = instance();
    running.can_launch = false;
    running.can_kill = true;

    support::show_client(&ui, running, "Overview");
    ui.set_accounts(ModelRc::new(VecModel::from(vec![account(
        "alice-id", "Alice", true,
    )])));
    settle(&ui);

    let launch_as =
        widest(&ui, &lookup("gui-launch-as")).expect("a second account is still launchable");
    assert!(launch_as.size().width > 0.0);
    assert!(
        widest(&ui, &lookup("gui-stop-game")).is_some(),
        "and the running client stoppable"
    );
    assert!(
        widest(&ui, &lookup("gui-launch")).is_none(),
        "plain Launch would be a second client as the same account"
    );
}

/// Launching as somebody while the instance runs asks for a second client;
/// launching as somebody while it is idle does not.
#[test]
fn launching_as_someone_says_whether_it_is_a_second_client() {
    let ui = window();
    let launched: Rc<RefCell<Vec<bool>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client_as(move |_, _, alongside| record.borrow_mut().push(alongside));

    instance_workspace(&ui);
    settle(&ui);
    ui.invoke_launch_client_as("repro-client".into(), "bob-id".into(), false);

    let mut running = instance();
    running.can_launch = false;
    running.can_kill = true;
    ui.set_instances(ModelRc::new(VecModel::from(vec![running])));
    settle(&ui);
    ui.invoke_launch_client_as("repro-client".into(), "bob-id".into(), true);

    assert_eq!(launched.borrow().as_slice(), [false, true]);
}

/// Launching as a chosen account reports that account, and never the default.
#[test]
fn launching_as_an_account_names_the_account_it_was_given() {
    let ui = window();
    let launched: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client_as(move |id, account, _alongside| {
        record
            .borrow_mut()
            .push((id.to_string(), account.to_string()));
    });

    instance_workspace(&ui);
    ui.invoke_launch_client_as("repro-client".into(), "bob-id".into(), false);

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
    support::open_settings(ui, "Accounts");
    ui.set_accounts(ModelRc::new(VecModel::from(vec![
        account("alice-id", "Alice", true),
        account("bob-id", "Bob", false),
    ])));
}

fn instance_workspace(ui: &AppWindow) {
    support::show_client(ui, instance(), "Overview");
    ui.set_accounts(ModelRc::new(VecModel::from(vec![
        account("alice-id", "Alice", true),
        account("bob-id", "Bob", false),
    ])));
}

fn account(id: &str, name: &str, is_default: bool) -> AccountRow {
    AccountRow {
        id: id.into(),
        name: name.into(),
        initial: name.chars().take(1).collect::<String>().into(),
        kind: "Offline".into(),
        is_default,
        credential_state: "No credential".into(),
    }
}

fn instance() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
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
        stoppable_linked_servers: 0,
        unstoppable_linked_servers: 0,
        mods: 0,
        needs_attention: false,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: SharedString::default(),
        connect_server_name: SharedString::default(),
        connect_reason: SharedString::default(),
        can_connect: false,
        creating_server_detail: SharedString::default(),
        creating_server_progress: 0.0,
        creating_server_indeterminate: false,
        restart_stage: SharedString::default(),
        can_restart: false,
        paired_server_id: SharedString::default(),
        paired_server_name: SharedString::default(),
        pair_start_together: false,
        pair_starting: false,
        favorite: false,
        group_id: slint::SharedString::default(),
        group_name: slint::SharedString::default(),
        group_index: 0,
        group_tone: 0,
        icon_slot: -1,
        problems: 0,
        state_line: "".into(),
        last_run: "".into(),
        open: false,
        ..InstanceRow::default()
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

/// Whether an open menu has a row carrying `label`.
fn menu_row(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| {
        element.accessible_role() == Some(AccessibleRole::Button) && element.size().width > 0.0
    })
}
