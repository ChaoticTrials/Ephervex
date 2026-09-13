//! What the mod browser's own action does.
//!
//! The browser row is the shortest path from "I need this mod" to having it, so
//! what its button says and what it invokes are both worth pinning down.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ModRow, ModSearchRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Image, ModelRc, PhysicalSize, SharedString, VecModel};

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
    ui.set_managed_mods(ModelRc::new(VecModel::from(vec![managed_mod()])));
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
    ui.set_managed_mods(ModelRc::new(VecModel::from(vec![hand_placed_mod()])));
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

/// Renaming a jar underneath a running loader does not unload it, and the core
/// refuses it. The row says so before the click rather than after it.
#[test]
fn the_mod_switch_is_unavailable_while_the_instance_is_running() {
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
        .expect("the switch is still shown, so its state stays readable")
        .mock_single_click(PointerEventButton::Left);

    assert!(
        flipped.borrow().is_empty(),
        "a press while the game is running must not reach the backend"
    );
    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-mods-locked-while-running"))
            .any(|element| element.size().width > 0.0),
        "an unavailable switch has to say why"
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
    ui.set_instances(ModelRc::new(VecModel::from(vec![instance])));
    ui.set_selected_instance_index(0);
    ui.set_managed_mods(ModelRc::new(VecModel::from(vec![managed_mod()])));
    settle(ui);
}

/// An instance sitting there with nothing running.
fn idle_instance() -> InstanceRow {
    InstanceRow {
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
        mods: 1,
        needs_attention: false,
        state: "Idle".into(),
        state_kind: "idle".into(),
        can_launch: true,
        can_kill: false,
        connect_server_id: SharedString::default(),
        connect_server_name: SharedString::default(),
        can_connect: false,
    }
}

/// Both controls sit on one line under the badges, so a reader looks in one
/// place for the two things a row can have done to it. The switch used to ride
/// on the title, where it landed in a different column on every card.
#[test]
fn the_switch_and_the_bin_share_the_rows_trailing_column() {
    let ui = window();
    mod_browser(&ui, Vec::new());
    ui.set_managed_mods(ModelRc::new(VecModel::from(vec![hand_placed_mod()])));
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
    ui.set_managed_mods(ModelRc::new(VecModel::from(vec![ModRow {
        origin: "From pack".into(),
        origin_kind: "pack".into(),
        deleted: true,
        can_delete_file: true,
        can_remove: false,
        ..managed_mod()
    }])));
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
    ui.on_install_from_browser(move |_instance, provider, project| {
        record
            .borrow_mut()
            .push((provider.to_string(), project.to_string()));
    });
    asked
}

fn mod_browser(ui: &AppWindow, results: Vec<ModSearchRow>) {
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace("instance".into());
    ui.set_workspace_tab("Mods".into());
    ui.set_selected_instance_id("repro-client".into());
    ui.set_mod_provider("modrinth".into());
    ui.set_mod_results(ModelRc::new(VecModel::from(results)));
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
