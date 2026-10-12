//! The Save and Reset buttons of the settings groups that save to the
//! configuration file.
//!
//! A button that stays pressable while its save runs queues another identical
//! command behind it, so each is disabled from the click until the save ends.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, PhysicalSize};

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
    ui.set_page(4);
    ui.set_settings_tab(tab.into());
    // Tall enough that the whole page is on screen, since a control below the
    // fold is not part of what the window reports.
    ui.window().set_size(PhysicalSize::new(1600, 3200));
    settle(&ui);
    ui
}

#[test]
fn the_java_save_is_disabled_while_the_java_settings_save_runs() {
    let ui = open("Java");
    assert_eq!(enabled(&ui, "gui-java-save"), [true]);

    ui.set_java_saving(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-java-save"), [false]);

    ui.set_java_saving(false);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-java-save"), [true]);
}

#[test]
fn the_storage_buttons_are_disabled_while_the_storage_root_save_runs() {
    let ui = open("Storage");
    ui.set_persistent_root_is_custom(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-save-storage-root"), [true]);
    assert_eq!(enabled(&ui, "gui-reset-default"), [true]);

    ui.set_storage_root_saving(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-save-storage-root"), [false]);
    assert_eq!(enabled(&ui, "gui-reset-default"), [false]);
}

#[test]
fn the_cache_limit_save_is_disabled_while_the_limit_save_runs() {
    let ui = open("Storage");
    assert_eq!(enabled(&ui, "gui-save-artifact-cache-limit"), [true]);

    ui.set_artifact_cache_limit_saving(true);
    settle(&ui);
    assert_eq!(enabled(&ui, "gui-save-artifact-cache-limit"), [false]);
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
