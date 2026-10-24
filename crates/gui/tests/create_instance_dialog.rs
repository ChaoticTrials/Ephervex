//! The New Instance dialog: what it asks for and what it submits.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::AppWindow;
use ephervex_i18n::lookup;
use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

type Submission = (
    String,
    String,
    String,
    String,
    bool,
    String,
    bool,
    String,
    String,
);

fn dialog() -> AppWindow {
    let ui = support::test_window();
    ui.set_loading(false);
    ui.window().set_size(PhysicalSize::new(1300, 1100));
    ui.set_minecraft_releases(ModelRc::new(VecModel::from(vec![SharedString::from(
        "1.20.1",
    )])));
    ui.set_create_minecraft("1.20.1".into());
    ui.set_create_name_suggestion("Vanilla 1.20.1".into());
    ui.set_modal("create-instance".into());
    settle(&ui);
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn shown(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|element| element.size().width > 0.0)
}

fn press(ui: &AppWindow, label: &str) {
    shown(ui, label)
        .expect("a control with this label")
        .invoke_accessible_default_action();
    settle(ui);
}

fn record(ui: &AppWindow) -> Rc<RefCell<Vec<Submission>>> {
    let calls: Rc<RefCell<Vec<Submission>>> = Rc::default();
    let sink = Rc::clone(&calls);
    ui.on_submit_create_instance(
        move |name, minecraft, loader, version, persistent, link, with_server, server, pack| {
            sink.borrow_mut().push((
                name.to_string(),
                minecraft.to_string(),
                loader.to_string(),
                version.to_string(),
                persistent,
                link.to_string(),
                with_server,
                server.to_string(),
                pack.to_string(),
            ));
        },
    );
    calls
}

#[test]
fn the_dialog_asks_for_an_optional_issue_link_and_a_storage_choice() {
    let ui = dialog();
    assert!(shown(&ui, &lookup("gui-issue-link-optional")).is_some());
    assert!(shown(&ui, &lookup("gui-temporary")).is_some());
    assert!(shown(&ui, &lookup("gui-persistent")).is_some());
}

#[test]
fn a_plain_instance_submits_without_a_link_or_a_server() {
    let ui = dialog();
    let calls = record(&ui);
    press(&ui, &lookup("gui-create-instance"));
    assert_eq!(
        calls.borrow().as_slice(),
        [(
            "Vanilla 1.20.1".to_owned(),
            "1.20.1".to_owned(),
            "Vanilla".to_owned(),
            String::new(),
            false,
            String::new(),
            false,
            String::new(),
            String::new()
        )]
    );
}

#[test]
fn the_link_the_storage_and_a_linked_server_are_submitted_with_the_instance() {
    let ui = dialog();
    let calls = record(&ui);
    ui.set_create_name("Conveyor dupe".into());
    ui.set_create_issue_link("https://example.com/issues/7".into());
    press(&ui, &lookup("gui-persistent"));
    press(&ui, &lookup("gui-create-with-server"));
    assert!(
        shown(&ui, &lookup("gui-linked-server-name")).is_some(),
        "the server's name is asked for once one is wanted"
    );
    press(&ui, &lookup("gui-create-instance"));
    let calls = calls.borrow();
    let submitted = calls.last().expect("a submission");
    assert!(submitted.4, "persistent");
    assert_eq!(submitted.5, "https://example.com/issues/7");
    assert!(submitted.6, "a linked server is wanted");
    assert_eq!(
        submitted.7, "",
        "the dialog leaves naming it to the suggestion"
    );
}

#[test]
fn a_chosen_server_name_is_used_instead_of_the_suggestion() {
    let ui = dialog();
    let calls = record(&ui);
    press(&ui, &lookup("gui-create-with-server"));
    ui.set_create_linked_server_name("mine".into());
    press(&ui, &lookup("gui-create-instance"));
    assert_eq!(calls.borrow().last().expect("a submission").7, "mine");
}

#[test]
fn the_server_files_are_asked_for_only_once_a_server_is_wanted() {
    let ui = dialog();
    assert!(
        shown(&ui, &lookup("gui-choose-server-pack")).is_none(),
        "no server, no files to choose"
    );
    press(&ui, &lookup("gui-create-with-server"));
    assert!(shown(&ui, &lookup("gui-choose-server-pack")).is_some());
}

#[test]
fn the_chosen_server_files_are_submitted_with_the_instance() {
    let ui = dialog();
    let calls = record(&ui);
    press(&ui, &lookup("gui-create-with-server"));
    ui.set_create_instance_server_pack("/tmp/pack-server.zip".into());
    ui.set_create_instance_server_pack_name("pack-server.zip".into());
    settle(&ui);
    assert!(shown(&ui, &lookup("gui-clear-server-pack")).is_some());
    press(&ui, &lookup("gui-create-instance"));
    assert_eq!(
        calls.borrow().last().expect("a submission").8,
        "/tmp/pack-server.zip"
    );
}

#[test]
fn choosing_and_clearing_ask_the_controller() {
    let ui = dialog();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let chosen = Rc::clone(&asked);
    ui.on_choose_instance_server_pack(move || chosen.borrow_mut().push("choose"));
    let cleared = Rc::clone(&asked);
    ui.on_clear_instance_server_pack(move || cleared.borrow_mut().push("clear"));
    press(&ui, &lookup("gui-create-with-server"));
    ui.set_create_instance_server_pack("/tmp/pack-server.zip".into());
    settle(&ui);
    press(&ui, &lookup("gui-choose-server-pack"));
    press(&ui, &lookup("gui-clear-server-pack"));
    assert_eq!(asked.borrow().as_slice(), ["choose", "clear"]);
}
