//! What a mod row's bin does, which depends on whose the mods are.
//!
//! An instance preserving an imported pack's declaration puts a jar aside so
//! it can come back, which is how a crash gets bisected. One whose mods are
//! the user's own takes the mod out of the instance instead, dependencies and
//! all, and so does a server made here rather than from a pack. A library something else needs keeps the reversible bin either way,
//! because its release was never the user's to end.

#![allow(clippy::expect_used, clippy::panic)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ModRow, ServerRow};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Image, ModelRc, PhysicalSize, SharedString, VecModel};

const MOD: &str = "Repro Mod";

#[test]
fn an_instance_preserving_a_packs_declaration_offers_the_reversible_bin() {
    let ui = window();
    show(&ui, false, &[row(true, true)]);

    assert!(
        bin(&ui, "gui-delete-mod").is_some(),
        "the jar goes aside so it can come back"
    );
    assert!(bin(&ui, "gui-remove-mod").is_none());
}

#[test]
fn an_instance_whose_mods_are_the_users_takes_the_mod_out_for_good() {
    let ui = window();
    show(&ui, true, &[row(true, true)]);

    assert!(
        bin(&ui, "gui-remove-mod").is_some(),
        "the bin takes the mod out of the instance, not only off disk"
    );
    assert!(bin(&ui, "gui-delete-mod").is_none());
}

/// Its release follows whatever required it, so removing it is not on offer
/// however the instance is set. Putting its jar aside still is.
#[test]
fn a_mod_that_may_not_be_removed_keeps_the_reversible_bin() {
    let ui = window();
    show(&ui, true, &[row(false, true)]);

    assert!(bin(&ui, "gui-delete-mod").is_some());
    assert!(bin(&ui, "gui-remove-mod").is_none());
}

/// Nothing to put back while the jar is gone, so the row offers the one thing
/// left instead of a second way to throw it away.
#[test]
fn a_jar_already_put_aside_offers_only_putting_it_back() {
    let ui = window();
    show(
        &ui,
        false,
        &[ModRow {
            deleted: true,
            ..row(true, true)
        }],
    );

    assert!(
        ElementHandle::find_by_accessible_label(&ui, &lookup("gui-restore-mod"))
            .any(|element| element.size().width > 0.0)
    );
    assert!(bin(&ui, "gui-delete-mod").is_none());
    assert!(bin(&ui, "gui-remove-mod").is_none());
}

/// A server made here has no pack declaration to keep, so its bin takes the
/// mod out of the server, and the server is the host it asks about.
#[test]
fn a_server_whose_mods_are_the_users_takes_the_mod_out_of_the_server() {
    let ui = window();
    let asked: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&asked);
    ui.on_plan_mod_removal(move |kind, id, path| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), path.to_string()));
    });
    show_server(&ui, true, &[row(true, true)]);

    bin(&ui, "gui-remove-mod")
        .expect("the bin takes the mod out")
        .invoke_accessible_default_action();

    assert_eq!(
        asked.borrow().as_slice(),
        [(
            "server".to_owned(),
            "a-server".to_owned(),
            "mods/repro.jar".to_owned()
        )]
    );
    assert!(bin(&ui, "gui-delete-mod").is_none());
}

#[test]
fn a_server_from_a_pack_offers_the_reversible_bin() {
    let ui = window();
    show_server(&ui, false, &[row(true, true)]);

    assert!(bin(&ui, "gui-delete-mod").is_some());
    assert!(bin(&ui, "gui-remove-mod").is_none());
}

/// A server's mod opens the same manager a client's does, for the server.
#[test]
fn pressing_a_servers_mod_opens_its_manager_for_that_server() {
    let ui = window();
    let opened: Rc<RefCell<Vec<(String, String, String)>>> = Rc::default();
    let record = Rc::clone(&opened);
    ui.on_open_mod_manager(move |kind, id, path| {
        record
            .borrow_mut()
            .push((kind.to_string(), id.to_string(), path.to_string()));
    });
    show_server(&ui, true, &[row(true, true)]);

    ElementHandle::find_by_accessible_label(&ui, MOD)
        .find(|element| element.size().width > 0.0)
        .expect("the row is on screen")
        .mock_single_click(PointerEventButton::Left);

    assert_eq!(
        opened.borrow().as_slice(),
        [(
            "server".to_owned(),
            "a-server".to_owned(),
            "mods/repro.jar".to_owned()
        )]
    );
}

#[test]
fn a_hand_placed_jars_confirmation_says_the_deletion_is_permanent() {
    let ui = window();
    show(
        &ui,
        false,
        &[ModRow {
            origin_kind: "hand-placed".into(),
            ..row(false, true)
        }],
    );

    open_delete_confirmation(&ui);

    assert!(visible(
        &ui,
        &lookup_args("gui-delete-mod-detail", [("value", Arg::from(MOD))])
    ));
    assert!(visible(&ui, &lookup("gui-delete-mod-note")));
    assert!(!visible(&ui, &lookup("gui-delete-restorable-mod-note")));
}

#[test]
fn a_recorded_mods_confirmation_says_the_jar_can_be_restored() {
    let ui = window();
    show(
        &ui,
        false,
        &[ModRow {
            origin_kind: "pack".into(),
            ..row(false, true)
        }],
    );

    open_delete_confirmation(&ui);

    assert!(visible(
        &ui,
        &lookup_args(
            "gui-delete-restorable-mod-detail",
            [("value", Arg::from(MOD))]
        )
    ));
    assert!(visible(&ui, &lookup("gui-delete-restorable-mod-note")));
    assert!(!visible(&ui, &lookup("gui-delete-mod-note")));
}

/// The bin belonging to the one mod on screen, when it is labeled this way.
fn bin(ui: &AppWindow, id: &str) -> Option<ElementHandle> {
    let label = lookup_args(id, [("value", Arg::from(MOD))]);
    ElementHandle::find_by_accessible_label(ui, &label).find(|element| element.size().width > 0.0)
}

fn open_delete_confirmation(ui: &AppWindow) {
    bin(ui, "gui-delete-mod")
        .expect("the row offers the reversible bin")
        .invoke_accessible_default_action();
    slint::platform::update_timers_and_animations();
    ui.window()
        .take_snapshot()
        .expect("render the confirmation");
    assert_eq!(ui.get_modal(), "delete-mod");
}

fn visible(ui: &AppWindow, label: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, label).any(|element| element.size().width > 0.0)
}

fn row(can_remove: bool, can_delete_file: bool) -> ModRow {
    ModRow {
        project_id: MOD.into(),
        logical_path: "mods/repro.jar".into(),
        title: MOD.into(),
        file_name: "repro.jar".into(),
        version: "1.0.0".into(),
        version_known: true,
        provider: "CurseForge".into(),
        provider_kind: "curseforge".into(),
        origin: "Added".into(),
        origin_kind: "user_selected".into(),
        compatibility: SharedString::default(),
        author: SharedString::default(),
        icon: Image::default(),
        enabled: true,
        deleted: false,
        pack_owned: false,
        can_manage: true,
        can_change_version: false,
        can_update: false,
        can_remove,
        can_delete_file,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

fn client() -> InstanceRow {
    InstanceRow {
        id: "an-id".into(),
        ..InstanceRow::default()
    }
}

fn show(ui: &AppWindow, mods_are_the_users: bool, rows: &[ModRow]) {
    let mut view = support::client_view(client());
    view.allow_pack_mod_changes = mods_are_the_users;
    support::show(ui, 0, vec![view], vec![support::pane(1, 0, "Mods")]);
    support::update_host(ui, 0, |view| {
        view.mods = ModelRc::from(Rc::new(VecModel::from(rows.to_vec())));
    });
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn show_server(ui: &AppWindow, mods_are_the_users: bool, rows: &[ModRow]) {
    let mut view = support::server_view(ServerRow {
        id: "a-server".into(),
        ..ServerRow::default()
    });
    view.mods_are_the_users = mods_are_the_users;
    support::show(ui, 1, vec![view], vec![support::pane(1, 0, "Mods")]);
    support::update_host(ui, 0, |view| {
        view.mods = ModelRc::from(Rc::new(VecModel::from(rows.to_vec())));
    });
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
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::from(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    ui.window().set_size(PhysicalSize::new(1480, 900));
    ui.show().expect("show");
    ui
}
