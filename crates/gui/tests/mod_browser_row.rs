//! What the mod browser's own action does.
//!
//! The browser row is the shortest path from "I need this mod" to having it, so
//! what its button says and what it invokes are both worth pinning down.

#![allow(clippy::expect_used)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, ModRow, ModSearchRow};
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
