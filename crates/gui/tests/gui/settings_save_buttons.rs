//! The Save and Reset buttons of the settings groups that save to the
//! configuration file.
//!
//! A button that stays pressable while its save runs queues another identical
//! command behind it, so each is disabled from the click until the save ends.

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

/// What a Java settings save sent: detection, download, minimum and maximum memory.
type Saved = (bool, bool, String, String);

/// Whether each button with this label is enabled, in layout order.
fn enabled(ui: &AppWindow, message: &str) -> Vec<bool> {
    let mut buttons: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(ui, &lookup(message))
            .filter(|element| element.accessible_enabled().is_some())
            .collect();
    buttons.sort_by(|left, right| {
        left.absolute_position()
            .x
            .total_cmp(&right.absolute_position().x)
    });
    buttons
        .iter()
        .filter_map(ElementHandle::accessible_enabled)
        .collect()
}

fn open(tab: &str) -> AppWindow {
    let ui = window();
    ui.set_loading(false);
    ui.set_settings_open(true);
    ui.set_settings_tab(tab.into());
    // Tall enough that the whole page is on screen, since a control below the
    // fold is not part of what the window reports.
    ui.window().set_size(PhysicalSize::new(1600, 3200));
    settle(&ui);
    ui
}

#[test]
fn a_java_toggle_saves_the_java_settings_as_soon_as_it_is_changed() {
    let ui = open("Java");
    ui.set_java_auto_detect(true);
    ui.set_java_auto_download(true);
    ui.set_java_memory_min("512".into());
    ui.set_java_memory_max("4096".into());
    let saved: Rc<RefCell<Vec<Saved>>> = Rc::default();
    let sink = Rc::clone(&saved);
    ui.on_save_java_settings(move |detect, download, minimum, maximum, _| {
        sink.borrow_mut()
            .push((detect, download, minimum.to_string(), maximum.to_string()));
    });
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-java-detect"))
        .find(|element| element.accessible_checked().is_some())
        .expect("the detection toggle")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(
        saved.borrow().as_slice(),
        [(false, true, "512".to_owned(), "4096".to_owned())]
    );
}

#[test]
fn the_storage_buttons_are_disabled_while_the_storage_root_save_runs() {
    let ui = open("Storage");
    ui.set_persistent_root_is_custom(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-choose-folder"), [true]);
    assert_eq!(enabled(&ui, "gui-reset-default"), [true]);

    ui.set_storage_root_saving(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-choose-folder"), [false]);
    assert_eq!(enabled(&ui, "gui-reset-default"), [false]);
}

#[test]
fn picking_a_cache_limit_saves_it_without_a_save_button() {
    let ui = open("Storage");
    ui.set_artifact_cache_limit_labels(ModelRc::new(VecModel::from(vec![
        SharedString::from("1 GiB"),
        SharedString::from("10 GiB"),
        SharedString::from("Unlimited"),
    ])));
    ui.set_artifact_cache_limit_index(0);
    let saved: Rc<RefCell<Vec<i32>>> = Rc::default();
    let sink = Rc::clone(&saved);
    ui.on_save_artifact_cache_limit(move |index, _, _| sink.borrow_mut().push(index));
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-artifact-cache-limit"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("the limit dropdown")
        .invoke_accessible_expand_action();
    settle(&ui);
    ElementHandle::find_by_accessible_label(&ui, "Unlimited")
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .expect("the entry")
        .invoke_accessible_default_action();
    settle(&ui);

    assert_eq!(saved.borrow().as_slice(), [2]);
}

#[test]
fn picking_a_custom_cache_limit_waits_for_its_size_and_stays_chosen() {
    let ui = open("Storage");
    ui.set_artifact_cache_limit_labels(ModelRc::new(VecModel::from(vec![
        SharedString::from("1 GiB"),
        SharedString::from("10 GiB"),
        SharedString::from("Custom"),
    ])));
    ui.set_artifact_cache_custom_index(2);
    ui.set_artifact_cache_limit_index(1);
    // The size the field starts with is the preset's own, which saving would
    // turn the choice back into.
    ui.set_artifact_cache_custom_value("10".into());
    let saved: Rc<RefCell<Vec<i32>>> = Rc::default();
    let sink = Rc::clone(&saved);
    ui.on_save_artifact_cache_limit(move |index, _, _| sink.borrow_mut().push(index));
    settle(&ui);

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-artifact-cache-limit"))
        .find(|element| element.accessible_role() == Some(AccessibleRole::Combobox))
        .expect("the limit dropdown")
        .invoke_accessible_expand_action();
    settle(&ui);
    ElementHandle::find_by_accessible_label(&ui, "Custom")
        .find(|element| element.accessible_role() == Some(AccessibleRole::ListItem))
        .expect("the entry")
        .invoke_accessible_default_action();
    settle(&ui);

    assert!(
        saved.borrow().is_empty(),
        "nothing is saved until a size is typed"
    );
    assert_eq!(ui.get_artifact_cache_limit_index(), 2);
}

#[test]
fn each_provider_reset_is_disabled_only_while_its_own_value_saves() {
    let ui = open("Providers & APIs");
    ui.set_microsoft_client_id_is_custom(true);
    ui.set_user_agent_is_custom(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-reset-default"), [true, true]);
    assert_eq!(enabled(&ui, "gui-save-api-settings"), [true]);

    ui.set_microsoft_client_id_saving(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-reset-default"), [false, true]);
    assert_eq!(enabled(&ui, "gui-save-api-settings"), [false]);

    ui.set_microsoft_client_id_saving(false);
    ui.set_user_agent_saving(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-reset-default"), [true, false]);
    assert_eq!(enabled(&ui, "gui-save-api-settings"), [false]);
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
