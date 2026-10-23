//! What the instances overview shows while a linked server is being built.
//!
//! The work belongs to one instance, so it is reported on that instance's own
//! row: a card of its own above the instance would be a second row for a
//! single thing, and the loop that placed one had to leave an empty card's
//! worth of space above every instance it did not belong to.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// The gap the overview puts between two cards, from `Theme.card-gap`.
const CARD_GAP: f32 = 12.0;

#[test]
fn the_instance_being_built_for_says_so_on_its_own_row() {
    let ui = overview(&[
        creating("cavestone", "CaveStone"),
        instance("all-the-mods", "All the Mods 11"),
    ]);

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-state-creating-server"))
            .any(|element| element.size().width > 0.0),
        "the instance's own row has to say a server is being built for it"
    );
    assert!(
        ElementHandle::find_by_element_id(&ui, "PendingCard::root")
            .all(|element| element.size().width == 0.0),
        "the work belongs on the instance's row, not on a card above it"
    );
}

#[test]
fn an_instance_nothing_is_being_built_for_keeps_its_ordinary_gap() {
    let ui = overview(&[
        creating("cavestone", "CaveStone"),
        instance("all-the-mods", "All the Mods 11"),
    ]);

    let first = card(&ui, "CaveStone").expect("the first instance is listed");
    let second = card(&ui, "All the Mods 11").expect("the second instance is listed");

    let gap = second.absolute_position().y - (first.absolute_position().y + first.size().height);
    assert!(
        (gap - CARD_GAP).abs() < 1.0,
        "an instance nothing is being built for sits {gap}px below its neighbour, not {CARD_GAP}px"
    );
}

/// Carrying the work must not make the row a different size from the rows
/// around it, which is the whole reason it goes here rather than above.
#[test]
fn the_row_is_the_same_size_whether_a_server_is_being_built_or_not() {
    let ui = overview(&[
        creating("cavestone", "CaveStone"),
        instance("all-the-mods", "All the Mods 11"),
    ]);

    let building = card(&ui, "CaveStone").expect("the first instance is listed");
    let idle = card(&ui, "All the Mods 11").expect("the second instance is listed");

    assert!(
        (building.size().height - idle.size().height).abs() < 1.0,
        "{}px against {}px",
        building.size().height,
        idle.size().height
    );
}

fn overview(instances: &[InstanceRow]) -> AppWindow {
    let ui = window();
    ui.set_loading(false);
    ui.set_page(0);
    ui.set_workspace_open(false);
    ui.set_instance_filter("all".into());
    ui.set_instances(ModelRc::new(VecModel::from(instances.to_vec())));
    settle(&ui);
    ui
}

/// One instance card, found by the name it shows.
fn card(ui: &AppWindow, name: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(ui, name).find(|element| element.size().height > 50.0)
}

fn instance(id: &str, name: &str) -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        id: id.into(),
        name: name.into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        source: "Created in Ephervex".into(),
        path: "/tmp/ephervex/instances/repro".into(),
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
    }
}

/// The same instance, with a linked server part-way through being built.
fn creating(id: &str, name: &str) -> InstanceRow {
    InstanceRow {
        snapshot_deletion_note: "Deleting files also deletes 2 retained snapshots.".into(),
        creating_server_detail: "Installing the linked server | 1.20.1 server_mappings".into(),
        creating_server_progress: 0.82,
        creating_server_indeterminate: false,
        restart_stage: SharedString::default(),
        can_restart: false,
        ..instance(id, name)
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
