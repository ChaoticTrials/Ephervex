//! The host list's organization controls, in a real window: the filter panel,
//! the empty states, and the favorite stars on cards.

use crate::support;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, InstanceRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, VecModel};

fn window() -> AppWindow {
    let ui = support::test_window();
    ui.set_loading(false);
    ui
}

fn settle(ui: &AppWindow) {
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn instance(favorite: bool) -> InstanceRow {
    InstanceRow {
        id: "repro-client".into(),
        name: "Repro Client".into(),
        minecraft: "1.20.1".into(),
        loader: "Vanilla".into(),
        loader_kind: "vanilla".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Manual".into(),
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        favorite,
        icon_slot: -1,
        ..InstanceRow::default()
    }
}

fn server(favorite: bool) -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        minecraft: "1.20.1".into(),
        loader: "Vanilla".into(),
        loader_kind: "vanilla".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        can_start: true,
        favorite,
        icon_slot: -1,
        ..ServerRow::default()
    }
}

fn host_label(id: &str, name: &str) -> String {
    lookup_args(id, [("value", Arg::from(name))])
}

#[test]
fn clearing_the_filters_resets_every_filter_and_the_query() {
    let ui = window();
    let changed = Rc::new(Cell::new(0_u32));
    let count = Rc::clone(&changed);
    ui.on_instance_view_changed(move || count.set(count.get() + 1));

    ui.set_instances(ModelRc::new(VecModel::from(vec![instance(false)])));
    ui.set_instance_filters_open(true);
    ui.set_instance_filter("temporary".into());
    ui.set_list_query("repro".into());
    ui.set_instance_favorites_only(true);
    ui.set_instance_minecraft_index(2);
    ui.set_instance_loader_index(1);
    ui.set_instance_run_index(1);
    ui.set_instance_linking_index(3);
    ui.set_instance_group_index(2);
    ui.set_instance_filters_active(6);
    settle(&ui);

    let clear = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-clear-filters"))
        .next()
        .expect("active filters offer to be cleared");
    clear.mock_single_click(PointerEventButton::Left);

    assert_eq!(ui.get_instance_filter(), "all");
    assert_eq!(ui.get_list_query(), "");
    assert!(!ui.get_instance_favorites_only());
    assert_eq!(ui.get_instance_minecraft_index(), 0);
    assert_eq!(ui.get_instance_loader_index(), 0);
    assert_eq!(ui.get_instance_run_index(), 0);
    assert_eq!(ui.get_instance_linking_index(), 0);
    assert_eq!(ui.get_instance_group_index(), 0);
    assert_eq!(changed.get(), 1, "one clear asks for one re-render");
}

#[test]
fn nothing_offers_to_clear_filters_while_none_is_active() {
    let ui = window();
    ui.set_instance_filters_open(true);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance(false)])));
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-clear-filters"))
            .next()
            .is_none()
    );
}

#[test]
fn the_filters_button_counts_the_choices_that_are_set() {
    let ui = window();
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance(false)])));
    settle(&ui);
    let plain = lookup("gui-more-filters");
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &plain)
            .next()
            .is_some(),
        "with nothing set the button says only what it opens"
    );

    ui.set_instance_favorites_only(true);
    ui.set_instance_loader_index(1);
    settle(&ui);
    let counted = lookup_args("gui-more-filters-active", [("value", Arg::from("2"))]);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &counted)
            .next()
            .is_some(),
        "with two set it says how many"
    );
}

#[test]
fn clearing_filters_is_offered_only_while_something_narrows_the_list() {
    let ui = window();
    ui.set_instance_filters_open(true);
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance(false)])));
    let clear = lookup("gui-clear-filters");
    for (active, offered) in [(0, false), (1, true), (3, true)] {
        ui.set_instance_filters_active(active);
        settle(&ui);
        assert_eq!(
            ElementHandle::find_by_accessible_label(&ui, &clear)
                .next()
                .is_some(),
            offered,
            "{active} active"
        );
    }
}

#[test]
fn filters_that_hide_every_instance_say_nothing_matches_instead_of_no_instances_yet() {
    let ui = window();
    ui.set_instances(ModelRc::new(VecModel::default()));
    ui.set_instance_total(4);
    ui.set_instance_filters_active(1);
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-no-matching-instances"))
            .next()
            .is_some()
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-no-client-instances"))
            .next()
            .is_none()
    );

    ui.set_instance_total(0);
    ui.set_instance_filters_active(0);
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-no-client-instances"))
            .next()
            .is_some()
    );
}

#[test]
fn an_instance_card_star_reports_the_change_it_asks_for_without_naming_a_version() {
    let ui = window();
    let seen: Rc<RefCell<Vec<(String, String, bool)>>> = Rc::default();
    let record = Rc::clone(&seen);
    ui.on_set_favorite(move |kind, id, favorite| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), favorite));
    });
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance(false)])));
    settle(&ui);

    let offer = host_label("gui-favorite-host", "Repro Client");
    assert!(!offer.contains("Minecraft"), "{offer}");
    let star = ElementHandle::find_by_accessible_label(&ui, &offer)
        .next()
        .expect("the card offers a star");
    star.mock_single_click(PointerEventButton::Left);
    assert_eq!(
        seen.borrow().as_slice(),
        [("client".to_owned(), "repro-client".to_owned(), true)]
    );

    ui.set_instances(ModelRc::new(VecModel::from(vec![instance(true)])));
    settle(&ui);
    let withdraw = host_label("gui-unfavorite-host", "Repro Client");
    let star = ElementHandle::find_by_accessible_label(&ui, &withdraw)
        .next()
        .expect("a favorite offers to be removed");
    star.mock_single_click(PointerEventButton::Left);
    assert!(!seen.borrow()[1].2);
}

#[test]
fn a_server_card_star_toggles_the_server() {
    let ui = window();
    let seen: Rc<RefCell<Vec<(String, String, bool)>>> = Rc::default();
    let record = Rc::clone(&seen);
    ui.on_set_favorite(move |kind, id, favorite| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), favorite));
    });
    ui.set_listed_servers(ModelRc::new(VecModel::from(vec![server(false)])));
    settle(&ui);

    let star = ElementHandle::find_by_accessible_label(
        &ui,
        &host_label("gui-favorite-host", "Repro Server"),
    )
    .next()
    .expect("the server card offers a star");
    star.mock_single_click(PointerEventButton::Left);
    assert_eq!(
        seen.borrow().as_slice(),
        [("server".to_owned(), "repro-server".to_owned(), true)]
    );
}

#[test]
fn a_group_shows_on_its_instances_card() {
    let ui = window();
    let mut grouped = instance(false);
    grouped.group_id = "group-1".into();
    grouped.group_name = "Bisect".into();
    ui.set_instances(ModelRc::new(VecModel::from(vec![grouped])));
    settle(&ui);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Bisect")
            .next()
            .is_some()
    );
}
