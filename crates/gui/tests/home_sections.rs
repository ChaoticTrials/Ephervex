//! What Home shows besides what is running: what needs a look, where to pick up
//! again, what the launcher has been doing, and how the cards sit on the page.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{ActivityRow, AppWindow, AttentionRow, ResumeRow, RunningRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, VecModel};

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn home() -> AppWindow {
    let ui = support::test_window();
    support::open_home(&ui);
    ui
}

fn shown(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

fn press(ui: &AppWindow, label: &str) {
    ElementHandle::find_by_accessible_label(ui, label)
        .find(|element| element.size().width > 0.0)
        .unwrap_or_else(|| panic!("nothing labeled {label}"))
        .mock_single_click(PointerEventButton::Left);
    settle(ui);
}

fn named(key: &str, host: &str) -> String {
    lookup_args(key, [("value", Arg::from(host.to_owned()))])
}

type Opened = Rc<RefCell<Vec<(String, String, String)>>>;

fn record_opened(ui: &AppWindow) -> Opened {
    let opened: Opened = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_workspace(move |kind, id, surface| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), surface.to_string()));
    });
    opened
}

fn attention(host: &str, title: &str) -> AttentionRow {
    AttentionRow {
        kind: "client".into(),
        id: "repro-client".into(),
        host_name: host.into(),
        title: title.into(),
        detail: "It is Java 21, older than the Java 25 the loader needs.".into(),
        surface: "Diagnostics".into(),
    }
}

fn resume(kind: &str, id: &str, name: &str, can_launch: bool) -> ResumeRow {
    ResumeRow {
        kind: kind.into(),
        id: id.into(),
        host_name: name.into(),
        last_run: "yesterday".into(),
        state_line: "Last run yesterday".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.2.0".into(),
        loader_kind: "forge".into(),
        state_kind: "ready".into(),
        needs_attention: false,
        can_launch,
    }
}

fn activity(id: &str, detail: &str) -> ActivityRow {
    ActivityRow {
        id: id.into(),
        title: "Install mods".into(),
        entity: "Frostbound".into(),
        status: "Failed".into(),
        detail: detail.into(),
        time: "11:40".into(),
        failed: true,
        ..ActivityRow::default()
    }
}

#[test]
fn the_greeting_and_the_summary_lead_the_page() {
    let ui = home();
    ui.set_home_greeting("Good afternoon, QuietBlock".into());
    ui.set_home_summary("2 running | 1 problem".into());
    settle(&ui);

    assert!(shown(&ui, "Good afternoon, QuietBlock"));
    assert!(shown(&ui, "2 running | 1 problem"));
}

#[test]
fn a_problem_opens_its_host_on_the_surface_that_explains_it_and_offers_no_fix() {
    let ui = home();
    let opened = record_opened(&ui);
    ui.set_home_attention(model(vec![attention(
        "Frostbound",
        "The selected Java cannot run this host",
    )]));
    settle(&ui);

    assert!(shown(&ui, "The selected Java cannot run this host"));
    assert!(
        shown(&ui, &lookup("gui-home-attention-mark")),
        "the card is marked as a problem"
    );
    assert!(
        !shown(&ui, &lookup("gui-fix-apply")),
        "Home previews and applies nothing"
    );
    press(&ui, &named("gui-home-attention-open", "Frostbound"));

    assert_eq!(
        opened.borrow().as_slice(),
        [(
            "client".to_owned(),
            "repro-client".to_owned(),
            "Diagnostics".to_owned()
        )]
    );
    assert!(!ui.get_settings_open());
}

#[test]
fn an_empty_page_says_so_in_each_section_that_stays() {
    let ui = home();
    settle(&ui);

    assert!(shown(&ui, &lookup("gui-nothing-running-detail")));
    assert!(shown(&ui, &lookup("gui-home-attention-empty")));
    assert!(shown(&ui, &lookup("gui-no-recent-activity-detail")));
    assert!(
        !shown(&ui, &lookup("gui-home-continue")),
        "a section with nothing to continue is left out"
    );
}

#[test]
fn a_host_to_continue_launches_or_opens_by_its_kind() {
    let ui = home();
    let opened = record_opened(&ui);
    let launched: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&launched);
    ui.on_launch_client(move |id| record.borrow_mut().push(format!("client/{id}")));
    let record = Rc::clone(&launched);
    ui.on_server_start(move |id| record.borrow_mut().push(format!("server/{id}")));
    ui.set_home_resume(model(vec![
        resume("client", "c1", "Train schedule", true),
        resume("server", "s1", "Creative", true),
        resume("client", "c2", "Blocked", false),
    ]));
    settle(&ui);

    assert!(shown(&ui, &lookup("gui-home-continue")));
    assert!(shown(&ui, "Last run yesterday"));
    assert!(shown(&ui, "Forge 47.2.0"));
    press(&ui, &named("gui-home-launch-host", "Train schedule"));
    press(&ui, &named("gui-home-launch-host", "Creative"));
    assert_eq!(launched.borrow().as_slice(), ["client/c1", "server/s1"]);
    assert!(
        !shown(&ui, &named("gui-home-launch-host", "Blocked")),
        "a host that cannot start offers no Launch"
    );

    press(&ui, &named("gui-home-open-host", "Blocked"));
    assert_eq!(
        opened.borrow().as_slice(),
        [("client".to_owned(), "c2".to_owned(), "Overview".to_owned())]
    );
}

#[test]
fn an_operation_can_be_canceled_and_its_long_detail_stays_above_its_footer() {
    let ui = home();
    let canceled: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&canceled);
    ui.on_cancel_operation(move |id| record.borrow_mut().push(id.to_string()));
    let detail = "A long failure message that wraps onto a second line because it keeps going \
                  and going well past the end of the card so that the height has to follow it";
    let mut running = activity("op1", detail);
    running.cancellable = true;
    running.failed = false;
    running.active = true;
    running.status = "Running".into();
    ui.set_activities(model(vec![running]));
    settle(&ui);

    assert!(shown(&ui, "Running"), "the state is a chip");
    let text = ElementHandle::find_by_accessible_label(&ui, detail)
        .find(|element| element.size().width > 0.0)
        .expect("the card shows the detail");
    let footer = ElementHandle::find_by_accessible_label(&ui, "op1 | 11:40")
        .find(|element| element.size().width > 0.0)
        .expect("the card shows its id and time");
    assert!(
        text.absolute_position().y + text.size().height <= footer.absolute_position().y + 0.5,
        "the detail ends at {}px, below the footer at {}px",
        text.absolute_position().y + text.size().height,
        footer.absolute_position().y
    );

    press(&ui, &named("gui-home-cancel-operation", "Install mods"));
    assert_eq!(canceled.borrow().as_slice(), ["op1"]);
}

#[test]
fn a_finished_operation_offers_no_cancel_and_its_card_opens_nothing() {
    let ui = home();
    let opened = record_opened(&ui);
    ui.set_activities(model(vec![activity("op2", "")]));
    settle(&ui);

    assert!(!shown(
        &ui,
        &named("gui-home-cancel-operation", "Install mods")
    ));
    ElementHandle::find_by_accessible_label(&ui, "Frostbound")
        .find(|element| element.size().width > 0.0)
        .expect("the card names its host")
        .mock_single_click(PointerEventButton::Left);
    assert!(opened.borrow().is_empty());
}

/// The cards of every section keep their hard shadow inside the page.
#[test]
fn no_card_shadow_reaches_past_the_edge_of_the_page() {
    let ui = home();
    ui.set_home_attention(model(vec![attention("Frostbound", "A problem")]));
    ui.set_running(model(vec![RunningRow {
        kind: "client".into(),
        id: "a".into(),
        name: "Running".into(),
        ..RunningRow::default()
    }]));
    ui.set_home_resume(model(vec![resume("client", "c1", "Resume", true)]));
    ui.set_activities(model(vec![activity("op1", "")]));
    settle(&ui);

    let theme = ui.global::<Theme>();
    let reach = theme.get_shadow_room();
    let page = ElementHandle::find_by_element_type_name(&ui, "HomePage")
        .next()
        .expect("Home is on screen");
    let right = page.absolute_position().x + page.size().width - theme.get_strong_border_width();
    for card in ["RunningCard", "AttentionCard", "ResumeCard", "ActivityCard"] {
        let element = ElementHandle::find_by_element_type_name(&ui, card)
            .find(|element| element.size().width > 0.0)
            .unwrap_or_else(|| panic!("no {card}"));
        let edge = element.absolute_position().x + element.size().width + reach;
        assert!(
            edge <= right,
            "a {card}'s shadow reaches {edge}px past {right}px"
        );
    }
}

/// At the smallest window and the largest text the quick actions all stay
/// reachable and the cards keep their minimum width.
#[test]
fn the_quick_actions_and_cards_fit_the_page_at_the_smallest_window_and_largest_text() {
    let ui = home();
    support::set_minimum_size(&ui);
    ui.global::<Theme>().set_text_scale(1.25);
    ui.set_home_greeting("Good afternoon, QuietBlock".into());
    ui.set_running(model(vec![RunningRow {
        kind: "client".into(),
        id: "a".into(),
        name: "Running".into(),
        ..RunningRow::default()
    }]));
    settle(&ui);

    let page = ElementHandle::find_by_element_type_name(&ui, "HomePage")
        .next()
        .expect("Home is on screen");
    let (left, right) = (
        page.absolute_position().x,
        page.absolute_position().x + page.size().width,
    );
    for key in [
        "gui-modal-new-instance",
        "gui-import-modpack",
        "gui-modal-new-server",
    ] {
        let button = ElementHandle::find_by_accessible_label(&ui, &lookup(key))
            .find(|element| {
                element.accessible_role() == Some(AccessibleRole::Button)
                    && element.absolute_position().x > left
            })
            .unwrap_or_else(|| panic!("Home offers {key}"));
        let edge = button.absolute_position().x + button.size().width;
        assert!(
            button.size().width > 0.0
                && edge <= right - ui.global::<Theme>().get_strong_border_width(),
            "{key} reaches {edge}px on a page that ends at {right}px"
        );
    }

    let floor = ui.global::<Theme>().get_home_card_min_width();
    for card in ElementHandle::find_by_element_type_name(&ui, "RunningCard")
        .filter(|element| element.size().width > 0.0)
    {
        assert!(
            card.size().width >= floor,
            "a card is {}px wide",
            card.size().width
        );
    }
}
