//! The issue link in an instance's header: shown, opened, added, edited, and
//! cleared.

#![allow(clippy::panic)]

use crate::support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, InstanceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;

const LINK: &str = "https://github.com/ChaoticTrials/Ephervex/issues/42";

fn window(link: &str) -> AppWindow {
    let ui = support::test_window();
    support::show_client(
        &ui,
        InstanceRow {
            id: "client".into(),
            name: "Conveyor dupe".into(),
            issue_link: link.into(),
            ..InstanceRow::default()
        },
        "Overview",
    );
    settle(&ui);
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn control(ui: &AppWindow, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, label).find(|element| element.size().width > 0.0)
}

fn press(ui: &AppWindow, label: &str) {
    control(ui, label)
        .unwrap_or_else(|| panic!("a control labeled {label}"))
        .invoke_accessible_default_action();
    settle(ui);
}

fn saved(ui: &AppWindow) -> Rc<RefCell<Vec<(String, String)>>> {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&calls);
    ui.on_set_issue_link(move |client, link| {
        sink.borrow_mut()
            .push((client.to_string(), link.to_string()));
    });
    calls
}

#[test]
fn an_instance_without_a_link_offers_to_add_one() {
    let ui = window("");
    assert!(control(&ui, &lookup("gui-issue-link-add")).is_some());
    assert!(control(&ui, &lookup("gui-issue-link-edit")).is_none());
}

#[test]
fn the_link_opens_when_pressed() {
    let ui = window(LINK);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_open_issue_link(move |link| sink.borrow_mut().push(link.to_string()));
    press(
        &ui,
        &lookup_args("gui-issue-link-open", [("value", Arg::from(LINK))]),
    );
    assert_eq!(opened.borrow().as_slice(), [LINK]);
}

#[test]
fn a_typed_link_is_saved_with_the_instance_it_belongs_to() {
    let ui = window("");
    let calls = saved(&ui);
    press(&ui, &lookup("gui-issue-link-add"));
    let field = control(&ui, &lookup("gui-issue-link")).expect("the field");
    field.set_accessible_value(LINK);
    settle(&ui);
    press(&ui, &lookup("gui-issue-link-save"));
    assert_eq!(
        calls.borrow().as_slice(),
        [("client".to_owned(), LINK.to_owned())]
    );
}

#[test]
fn editing_starts_from_the_stored_link_and_cancel_stores_nothing() {
    let ui = window(LINK);
    let calls = saved(&ui);
    press(&ui, &lookup("gui-issue-link-edit"));
    let field = control(&ui, &lookup("gui-issue-link")).expect("the field");
    assert_eq!(field.accessible_value().as_deref(), Some(LINK));

    press(&ui, &lookup("gui-cancel"));
    assert!(calls.borrow().is_empty());
    assert!(control(&ui, &lookup("gui-issue-link-edit")).is_some());
}

#[test]
fn emptying_the_field_and_saving_clears_the_link() {
    let ui = window(LINK);
    let calls = saved(&ui);
    press(&ui, &lookup("gui-issue-link-edit"));
    control(&ui, &lookup("gui-issue-link"))
        .expect("the field")
        .set_accessible_value("");
    settle(&ui);
    press(&ui, &lookup("gui-issue-link-save"));
    assert_eq!(
        calls.borrow().as_slice(),
        [("client".to_owned(), String::new())]
    );
}

#[test]
fn a_server_page_has_no_issue_link() {
    let ui = support::test_window();
    support::show_server(&ui, ephervex_gui::ui::ServerRow::default(), "Overview");
    settle(&ui);
    assert!(control(&ui, &lookup("gui-issue-link-add")).is_none());
}

fn key(ui: &AppWindow, text: slint::SharedString) {
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
    settle(ui);
}

#[test]
fn enter_saves_the_typed_link() {
    let ui = window("");
    let calls = saved(&ui);
    press(&ui, &lookup("gui-issue-link-add"));
    control(&ui, &lookup("gui-issue-link"))
        .expect("the field")
        .set_accessible_value(LINK);
    settle(&ui);
    key(&ui, slint::platform::Key::Return.into());
    assert_eq!(
        calls.borrow().as_slice(),
        [("client".to_owned(), LINK.to_owned())]
    );
}

#[test]
fn escape_leaves_the_edit_without_saving() {
    let ui = window(LINK);
    let calls = saved(&ui);
    press(&ui, &lookup("gui-issue-link-edit"));
    key(&ui, slint::platform::Key::Escape.into());
    assert!(calls.borrow().is_empty());
    assert!(control(&ui, &lookup("gui-issue-link")).is_none());
}

#[test]
fn a_save_the_backend_has_not_confirmed_leaves_the_text_to_be_fixed() {
    let ui = window("");
    let calls = saved(&ui);
    press(&ui, &lookup("gui-issue-link-add"));
    control(&ui, &lookup("gui-issue-link"))
        .expect("the field")
        .set_accessible_value("ftp://example.com");
    settle(&ui);
    press(&ui, &lookup("gui-issue-link-save"));
    assert_eq!(calls.borrow().len(), 1);
    let field = control(&ui, &lookup("gui-issue-link")).expect("still editing");
    assert_eq!(
        field.accessible_value().as_deref(),
        Some("ftp://example.com")
    );

    ui.set_host_views(slint::ModelRc::new(slint::VecModel::from(vec![
        support::client_view(InstanceRow {
            id: "client".into(),
            name: "Conveyor dupe".into(),
            issue_link: "ftp://example.com".into(),
            ..InstanceRow::default()
        }),
    ])));
    settle(&ui);
    assert!(
        control(&ui, &lookup("gui-issue-link")).is_none(),
        "a stored link closes the edit"
    );
}

#[test]
fn an_edit_does_not_follow_the_pane_to_another_instance() {
    let ui = window("");
    press(&ui, &lookup("gui-issue-link-add"));
    control(&ui, &lookup("gui-issue-link"))
        .expect("the field")
        .set_accessible_value(LINK);
    settle(&ui);

    let other = support::client_view(InstanceRow {
        id: "other".into(),
        name: "Another".into(),
        ..InstanceRow::default()
    });
    support::show(&ui, vec![other], vec![support::pane(1, 0, "Overview")]);
    settle(&ui);
    assert!(control(&ui, &lookup("gui-issue-link")).is_none());
    assert!(control(&ui, &lookup("gui-issue-link-add")).is_some());
}

#[test]
fn a_very_long_link_does_not_push_the_header_actions_out_of_the_page() {
    let long = format!("https://example.com/issues?{}", "q=1&".repeat(120));
    let ui = window(&long);
    ui.global::<ephervex_gui::ui::Theme>().set_text_scale(1.25);
    settle(&ui);
    let page = ElementHandle::find_by_element_type_name(&ui, "HostWorkspace")
        .next()
        .expect("the page");
    let right = page.absolute_position().x + page.size().width;
    for label in [lookup("gui-open-folder"), lookup("gui-issue-link-edit")] {
        let button = control(&ui, &label).expect("the button");
        assert!(
            button.absolute_position().x + button.size().width <= right + 1.0,
            "{label:?} ends beyond the page"
        );
    }
}
