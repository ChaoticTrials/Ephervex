//! What the mod browser's own action does.
//!
//! The browser row is the shortest path from "I need this mod" to having it, so
//! what its button says and what it invokes are both worth pinning down.

#![allow(clippy::expect_used)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ModRow, ModSearchRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Image, Model, ModelRc, PhysicalSize, SharedString, VecModel};

#[test]
fn the_browser_row_installs_rather_than_offering_a_review() {
    let ui = window();
    let asked = record_installs(&ui);

    mod_browser(&ui, vec![result("utilitix", "UtilitiX")]);
    settle(&ui);

    install_buttons(&ui)
        .first()
        .expect("the row offers Install")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        asked.borrow().as_slice(),
        [("modrinth".to_owned(), "utilitix".to_owned())]
    );
}

/// Installing several mods is pressing several buttons, and a user who knows
/// what they want does not wait for each answer before asking for the next.
#[test]
fn every_install_pressed_in_a_row_reaches_the_backend() {
    let ui = window();
    let asked = record_installs(&ui);

    mod_browser(
        &ui,
        vec![
            result("utilitix", "UtilitiX"),
            result("jei", "Just Enough Items"),
            result("sodium", "Sodium"),
        ],
    );
    settle(&ui);

    // No settling between presses: nothing has answered yet, which is exactly
    // the case a single in-flight slot used to lose.
    for button in install_buttons(&ui) {
        button.mock_single_click(PointerEventButton::Left);
    }

    assert_eq!(
        asked
            .borrow()
            .iter()
            .map(|(_, project)| project.as_str())
            .collect::<Vec<_>>(),
        ["utilitix", "jei", "sodium"],
        "every press has to reach the backend, not only the last one"
    );
}

/// A result the instance already has is not something to install again.
#[test]
fn a_result_the_instance_already_has_is_marked_and_cannot_be_pressed() {
    let ui = window();
    let asked = record_installs(&ui);

    mod_browser(
        &ui,
        vec![
            ModSearchRow {
                installed: true,
                ..result("utilitix", "UtilitiX")
            },
            result("jei", "Just Enough Items"),
        ],
    );
    settle(&ui);

    assert_eq!(
        install_buttons(&ui).len(),
        1,
        "an installed result must not offer Install"
    );
    let installed =
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-result-installed"))
            .next()
            .expect("an installed result says so");
    installed.mock_single_click(PointerEventButton::Left);

    assert!(
        asked.borrow().is_empty(),
        "an installed result's button must not act"
    );
}

/// A queued install is under way, so pressing it again would mean nothing.
#[test]
fn a_queued_result_says_so_and_cannot_be_pressed_again() {
    let ui = window();
    let asked = record_installs(&ui);

    mod_browser(
        &ui,
        vec![ModSearchRow {
            queued: true,
            ..result("utilitix", "UtilitiX")
        }],
    );
    settle(&ui);

    let queued = ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-result-queued"))
        .next()
        .expect("a queued result says so");
    queued.mock_single_click(PointerEventButton::Left);

    assert!(asked.borrow().is_empty());
    assert!(
        install_buttons(&ui).is_empty(),
        "a queued result must not offer Install as well"
    );
}

/// The managed list is where a mod's catalog is now stated, in that catalog's
/// own color, so the row has to carry which one it is.
#[test]
fn a_managed_mod_names_the_catalog_it_came_from() {
    let ui = window();
    mod_browser(&ui, Vec::new());
    support::set_mods(&ui, ModelRc::new(VecModel::from(vec![managed_mod()])));
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-provider-modrinth"))
            .any(|element| element.size().width > 0.0),
        "a Modrinth mod has to say so where it is listed"
    );
}

/// Where a row's badges and its bin sit relative to each other.
///
/// Layout rather than behavior, and asserted because reading the markup does
/// not tell you: a badge placed in the title row rides on however long the
/// title is, so it lands in a different column on every card, and the bin then
/// has nothing above it. Both belong in the row's own trailing column.
#[test]
fn a_rows_badges_sit_at_its_trailing_edge_above_the_bin() {
    let ui = window();
    mod_browser(&ui, Vec::new());
    support::set_mods(&ui, ModelRc::new(VecModel::from(vec![hand_placed_mod()])));
    settle(&ui);

    let badge = ElementHandle::find_by_accessible_label(&ui, "Added by hand")
        .find(|element| element.size().width > 0.0)
        .expect("the row says who put the file there");
    let bin = ElementHandle::find_by_accessible_label(
        &ui,
        &lookup_args("gui-delete-mod", [("value", Arg::from("UtilitiX"))]),
    )
    .find(|element| element.size().width > 0.0)
    .expect("a hand-placed row offers the bin");

    assert!(
        bin.absolute_position().y > badge.absolute_position().y,
        "the bin belongs under the badge, not beside it"
    );
    // Same column, within a badge's own width. This is what tells a stacked
    // pair apart from a badge left in the title row, where it sits at a
    // title-dependent offset and the bin is a column further right.
    let badge_center = badge.absolute_position().x + badge.size().width / 2.0;
    let bin_center = bin.absolute_position().x + bin.size().width / 2.0;
    assert!(
        (badge_center - bin_center).abs() < badge.size().width,
        "badge center {badge_center} and bin center {bin_center} are not one column"
    );

    let title = ElementHandle::find_by_accessible_label(&ui, "UtilitiX")
        .find(|element| element.size().width > 0.0);
    if let Some(title) = title {
        assert!(
            badge.absolute_position().x > title.absolute_position().x,
            "the badge trails the title rather than preceding it"
        );
    }
}

/// Turning a mod off to see whether the crash goes away is why this launcher
/// exists, and for a long time only a server's list offered it.
#[test]
fn an_instances_mod_row_offers_the_switch_its_servers_row_already_has() {
    let ui = window();
    let flipped = record_toggles(&ui);
    instance_mods(&ui, idle_instance());

    switch(&ui)
        .expect("the row offers the switch")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        flipped.borrow().as_slice(),
        [(
            "client".to_owned(),
            "mods/utilitix-1.21.1-1.13.2.jar".to_owned(),
            false
        )],
        "the switch has to reach the instance's own mods, not a server's"
    );
}

/// Toggling a mod off moves its jar aside rather than touching a file the
/// running loader has open, so it is allowed whether the instance is running
/// or not, just like deleting and restoring one.
#[test]
fn the_mod_switch_stays_available_while_the_instance_is_running() {
    let ui = window();
    let flipped = record_toggles(&ui);
    instance_mods(
        &ui,
        InstanceRow {
            can_kill: true,
            can_launch: false,
            state: "Running".into(),
            state_kind: "running".into(),
            ..idle_instance()
        },
    );

    switch(&ui)
        .expect("the row offers the switch")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        flipped.borrow().as_slice(),
        [(
            "client".to_owned(),
            "mods/utilitix-1.21.1-1.13.2.jar".to_owned(),
            false
        )],
        "a press while the game is running must still reach the backend"
    );
}

/// The one mod row's own switch.
fn switch(ui: &AppWindow) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(
        ui,
        &lookup_args("gui-mod-loads", [("value", Arg::from("UtilitiX"))]),
    )
    .find(|element| element.size().width > 0.0)
}

fn record_toggles(ui: &AppWindow) -> Rc<RefCell<Vec<(String, String, bool)>>> {
    let flipped: Rc<RefCell<Vec<(String, String, bool)>>> = Rc::default();
    let recorded = Rc::clone(&flipped);
    ui.on_set_mod_enabled(move |kind, _host, path, enabled| {
        recorded
            .borrow_mut()
            .push((kind.to_string(), path.to_string(), enabled));
    });
    flipped
}

/// The instance workspace's Mods tab, showing one managed mod.
fn instance_mods(ui: &AppWindow, instance: InstanceRow) {
    mod_browser(ui, Vec::new());
    support::show_client(ui, instance, "Mods");
    support::set_mods(ui, ModelRc::new(VecModel::from(vec![managed_mod()])));
    settle(ui);
}

/// An instance sitting there with nothing running.
fn idle_instance() -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: "repro-client".into(),
        name: "Repro".into(),
        minecraft: "1.21.1".into(),
        loader: "NeoForge 21.1.72".into(),
        loader_kind: "neoforge".into(),
        storage: lookup("gui-temporary").into(),
        storage_kind: "temporary".into(),
        source: "Created here".into(),
        path: "/tmp/ephervex/instances/repro".into(),
        updated: "just now".into(),
        linked_servers: 0,
        stoppable_linked_servers: 0,
        unstoppable_linked_servers: 0,
        mods: 1,
        needs_attention: false,
        state: "Idle".into(),
        state_kind: "idle".into(),
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
    }
}

/// Both controls sit on one line under the badges, so a reader looks in one
/// place for the two things a row can have done to it. The switch used to ride
/// on the title, where it landed in a different column on every card.
#[test]
fn the_switch_and_the_bin_share_the_rows_trailing_column() {
    let ui = window();
    mod_browser(&ui, Vec::new());
    support::set_mods(&ui, ModelRc::new(VecModel::from(vec![hand_placed_mod()])));
    settle(&ui);

    let switch = ElementHandle::find_by_accessible_label(
        &ui,
        &lookup_args("gui-mod-loads", [("value", Arg::from("UtilitiX"))]),
    )
    .find(|element| element.size().width > 0.0)
    .expect("the row offers the switch");
    let bin = ElementHandle::find_by_accessible_label(
        &ui,
        &lookup_args("gui-delete-mod", [("value", Arg::from("UtilitiX"))]),
    )
    .find(|element| element.size().width > 0.0)
    .expect("a hand-placed row offers the bin");
    let badge = ElementHandle::find_by_accessible_label(&ui, "Added by hand")
        .find(|element| element.size().width > 0.0)
        .expect("the row says who put the file there");

    // Same line: their centers agree vertically within half a control.
    let switch_middle = switch.absolute_position().y + switch.size().height / 2.0;
    let bin_middle = bin.absolute_position().y + bin.size().height / 2.0;
    assert!(
        (switch_middle - bin_middle).abs() < bin.size().height / 2.0,
        "switch center {switch_middle} and bin center {bin_middle} are not one line"
    );
    assert!(
        switch.absolute_position().x < bin.absolute_position().x,
        "the switch sits before the bin"
    );
    assert!(
        switch.absolute_position().y > badge.absolute_position().y,
        "both controls belong under the badges, not in the title row"
    );
}

/// A pack's mod can be put aside, and the row stays so it can come back. That
/// is what keeps the bin from being a one-way door.
#[test]
fn a_deleted_pack_mod_keeps_its_row_and_offers_to_come_back() {
    let ui = window();
    let restored: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&restored);
    ui.on_restore_mod_file(move |_kind, _host, path| sink.borrow_mut().push(path.to_string()));

    mod_browser(&ui, Vec::new());
    support::set_mods(
        &ui,
        ModelRc::new(VecModel::from(vec![ModRow {
            origin: "From pack".into(),
            origin_kind: "pack".into(),
            deleted: true,
            can_delete_file: true,
            can_remove: false,
            ..managed_mod()
        }])),
    );
    settle(&ui);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-file-deleted"))
            .any(|element| element.size().width > 0.0),
        "a deleted mod has to say so rather than look installed"
    );

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-restore-mod"))
        .find(|element| element.size().width > 0.0)
        .expect("a deleted pack mod offers to come back")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        restored.borrow().as_slice(),
        ["mods/utilitix-1.21.1-1.13.2.jar".to_owned()]
    );
}

/// A jar the user put in `mods/` themselves and no catalog has recognized.
///
/// One badge rather than two, which is what makes the column testable: with a
/// catalog badge beside it neither pill is centered on the bin, only the pair
/// is.
fn hand_placed_mod() -> ModRow {
    ModRow {
        origin: "Added by hand".into(),
        origin_kind: "hand-placed".into(),
        provider: lookup("gui-mod-provider-unknown").into(),
        provider_kind: SharedString::default(),
        can_delete_file: true,
        can_remove: false,
        can_change_version: false,
        can_update: false,
        ..managed_mod()
    }
}

/// The Install buttons on screen, one handle each.
///
/// Filtered by role because a button's own label and the label of the text
/// inside it both answer to the same search, and a press dispatched twice at
/// one place would look like two presses.
fn install_buttons(ui: &AppWindow) -> Vec<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-install"))
        .filter(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .collect()
}

fn record_installs(ui: &AppWindow) -> Rc<RefCell<Vec<(String, String)>>> {
    let asked: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_install_from_browser(move |_kind, _instance, provider, project| {
        record
            .borrow_mut()
            .push((provider.to_string(), project.to_string()));
    });
    asked
}

fn mod_browser(ui: &AppWindow, results: Vec<ModSearchRow>) {
    support::show_client(
        ui,
        InstanceRow {
            id: "repro-client".into(),
            ..InstanceRow::default()
        },
        "Mods",
    );
    support::update_host(ui, 0, |view| {
        view.browser_provider = "modrinth".into();
        view.browser_results = ModelRc::new(VecModel::from(results));
    });
}

fn result(project: &str, title: &str) -> ModSearchRow {
    ModSearchRow {
        project_id: project.into(),
        title: title.into(),
        description: "Nice to have items, blocks, and other features.".into(),
        author: "MelanX".into(),
        note: "Compatible".into(),
        icon: Image::default(),
        installed: false,
        queued: false,
        server_support: SharedString::default(),
        blocked_reason: SharedString::default(),
    }
}

fn managed_mod() -> ModRow {
    ModRow {
        project_id: "utilitix".into(),
        logical_path: "mods/utilitix-1.21.1-1.13.2.jar".into(),
        title: "UtilitiX".into(),
        file_name: "utilitix-1.21.1-1.13.2.jar".into(),
        version: "1.13.2".into(),
        version_known: true,
        provider: lookup("gui-provider-modrinth").into(),
        provider_kind: "modrinth".into(),
        origin: "Chosen here".into(),
        origin_kind: "user".into(),
        compatibility: "1.21.1 | NeoForge".into(),
        author: "MelanX".into(),
        icon: Image::default(),
        enabled: true,
        deleted: false,
        pack_owned: false,
        can_manage: true,
        can_change_version: true,
        can_update: true,
        can_remove: true,
        can_delete_file: false,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
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

// -- a server's browser ---------------------------------------------------

/// The server and the projects each confirmed install named.
type ConfirmedInstalls = Vec<(String, Vec<String>)>;

/// A stopped, installed server on a modded loader.
fn modded_server() -> ServerRow {
    ServerRow {
        id: "repro-server".into(),
        name: "Repro Server".into(),
        installed: true,
        loader_kind: "neoforge".into(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        ..ServerRow::default()
    }
}

fn server_browser(ui: &AppWindow, row: ServerRow, results: Vec<ModSearchRow>) {
    support::show_server(ui, row, "Mods");
    support::update_host(ui, 0, |view| {
        view.browser_provider = "modrinth".into();
        view.browser_results = ModelRc::new(VecModel::from(results));
    });
    settle(ui);
}

fn search_buttons(ui: &AppWindow) -> usize {
    ElementHandle::find_by_accessible_label(ui, &lookup("gui-search"))
        .filter(|element| {
            element.size().width > 0.0 && element.accessible_role() == Some(AccessibleRole::Button)
        })
        .count()
}

#[test]
fn a_servers_mods_surface_offers_the_catalog_browser() {
    let ui = window();
    server_browser(&ui, modded_server(), vec![result("utilitix", "UtilitiX")]);

    assert_eq!(search_buttons(&ui), 1, "the server can search a catalog");
    assert_eq!(install_buttons(&ui).len(), 1);
}

#[test]
fn a_vanilla_server_has_no_catalog_browser() {
    let ui = window();
    server_browser(
        &ui,
        ServerRow {
            loader_kind: "vanilla".into(),
            ..modded_server()
        },
        vec![result("utilitix", "UtilitiX")],
    );

    assert_eq!(search_buttons(&ui), 0);
    assert!(install_buttons(&ui).is_empty());
}

#[test]
fn a_server_that_is_not_installed_has_no_catalog_browser() {
    let ui = window();
    server_browser(
        &ui,
        ServerRow {
            installed: false,
            ..modded_server()
        },
        Vec::new(),
    );

    assert_eq!(search_buttons(&ui), 0);
}

#[test]
fn a_servers_install_names_the_server_and_not_a_client() {
    let ui = window();
    let asked: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_install_from_browser(move |kind, id, _provider, project| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), project.to_string()));
    });
    server_browser(&ui, modded_server(), vec![result("utilitix", "UtilitiX")]);

    install_buttons(&ui)
        .first()
        .expect("the row offers Install")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        asked.borrow().as_slice(),
        [(
            "server".to_owned(),
            "repro-server".to_owned(),
            "utilitix".to_owned()
        )]
    );
}

#[test]
fn a_result_with_no_stated_server_side_carries_a_question_mark_that_says_so() {
    let ui = window();
    server_browser(
        &ui,
        modded_server(),
        vec![ModSearchRow {
            server_support: "unknown".into(),
            ..result("utilitix", "UtilitiX")
        }],
    );

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-server-support-unstated"))
            .any(|element| element.size().width > 0.0),
        "the badge has an accessible label"
    );
    assert_eq!(
        install_buttons(&ui).len(),
        1,
        "an unstated side is still installable, after a confirmation"
    );
}

#[test]
fn a_result_whose_server_side_is_stated_carries_no_badge() {
    let ui = window();
    server_browser(&ui, modded_server(), vec![result("utilitix", "UtilitiX")]);

    assert!(
        !ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-server-support-unstated"))
            .any(|element| element.size().width > 0.0)
    );
    assert!(
        !ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-client-only"))
            .any(|element| element.size().width > 0.0)
    );
}

#[test]
fn a_client_only_result_is_marked_and_its_install_cannot_be_pressed() {
    let ui = window();
    let asked: Rc<RefCell<Vec<String>>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_install_from_browser(move |_, _, _, project| {
        record.borrow_mut().push(project.to_string());
    });
    server_browser(
        &ui,
        modded_server(),
        vec![ModSearchRow {
            server_support: "client-only".into(),
            blocked_reason: lookup("gui-hint-mod-client-only").into(),
            ..result("sodium", "Sodium")
        }],
    );

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mod-client-only"))
            .any(|element| element.size().width > 0.0),
        "the row says the mod is client only"
    );
    install_buttons(&ui)
        .first()
        .expect("the row still shows Install")
        .mock_single_click(PointerEventButton::Left);
    assert!(
        asked.borrow().is_empty(),
        "a client-only mod is never installed"
    );
}

#[test]
fn the_servers_browser_offers_the_incompatible_toggle_and_a_clients_does_not() {
    let ui = window();
    let toggled: Rc<RefCell<Vec<(String, String, bool)>>> = Rc::default();
    let record = Rc::clone(&toggled);
    ui.on_set_mod_include_incompatible(move |kind, id, on| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), on));
    });
    server_browser(&ui, modded_server(), Vec::new());

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-show-incompatible-mods"))
        .find(|element| element.size().width > 0.0)
        .expect("the server's browser offers the toggle")
        .invoke_accessible_default_action();

    assert_eq!(
        toggled.borrow().as_slice(),
        [("server".to_owned(), "repro-server".to_owned(), true)]
    );

    mod_browser(&ui, Vec::new());
    settle(&ui);
    assert!(
        !ElementHandle::find_by_accessible_label(&ui, &lookup("gui-show-incompatible-mods"))
            .any(|element| element.size().width > 0.0),
        "a client's browser is unchanged"
    );
}

#[test]
fn a_server_confirmation_names_each_mod_whose_server_side_was_not_stated() {
    let ui = window();
    let asked: Rc<RefCell<ConfirmedInstalls>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_install_server_mod(move |server, _provider, _project, _version, ids| {
        record.borrow_mut().push((
            server.to_string(),
            ids.iter().map(|id| id.to_string()).collect(),
        ));
    });
    server_browser(&ui, modded_server(), Vec::new());
    ui.set_mod_plan_kind("server".into());
    ui.set_mod_plan_instance("repro-server".into());
    ui.set_mod_plan_provider("modrinth".into());
    ui.set_mod_plan_project("utilitix".into());
    ui.set_mod_plan_title("UtilitiX".into());
    ui.set_mod_plan_install_count(2);
    ui.set_mod_plan_unstated(ModelRc::new(VecModel::from(vec![
        SharedString::from("Quiet Library"),
        SharedString::from("Silent Helper"),
    ])));
    ui.set_mod_plan_unstated_ids(ModelRc::new(VecModel::from(vec![
        SharedString::from("quiet-library"),
        SharedString::from("silent-helper"),
    ])));
    ui.set_mod_plan_client_only(ModelRc::new(VecModel::from(vec![SharedString::from(
        "Screen Helper says it does not run on a server, but UtilitiX needs it.",
    )])));
    ui.set_mod_plan_restart_note(lookup("gui-server-mod-plan-restart").into());
    ui.set_modal("mod-plan".into());
    settle(&ui);

    for text in [
        "- Quiet Library",
        "- Silent Helper",
        "Screen Helper says it does not run on a server, but UtilitiX needs it.",
        &lookup("gui-server-mod-plan-restart"),
    ] {
        assert!(
            ElementHandle::find_by_element_type_name(&ui, "Text")
                .any(|element| element.accessible_label().as_deref() == Some(text)),
            "the confirmation shows {text}"
        );
    }

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-install-required"))
        .find(|element| element.size().width > 0.0)
        .expect("the confirmation installs")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        asked.borrow().as_slice(),
        [(
            "repro-server".to_owned(),
            vec!["quiet-library".to_owned(), "silent-helper".to_owned()]
        )],
        "the mods the dialog named are the ones confirmed"
    );
}
