//! The seasonal themes switch on the General settings page.
//!
//! The switch changes nothing on a day outside a season, so the page offers it
//! only while a season is on.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, PhysicalSize};

#[test]
fn the_switch_is_offered_only_while_a_season_is_on() {
    let ui = window();
    let label = lookup("gui-seasonal-themes");
    let offered = |ui: &AppWindow| {
        slint::platform::update_timers_and_animations();
        ui.window().take_snapshot().expect("render");
        ElementHandle::find_by_accessible_label(ui, &label)
            .any(|element| element.size().width > 0.0)
    };

    ui.set_seasonal_theme_in_season(false);
    assert!(!offered(&ui), "no season is on");

    ui.set_seasonal_theme_in_season(true);
    assert!(offered(&ui), "a season is on");

    ui.set_seasonal_themes(false);
    assert!(
        offered(&ui),
        "the switch stays offered while it is off, so it can be turned on again"
    );
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
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    ui.show().expect("show");
    ui.set_loading(false);
    ui.invoke_open_settings("General".into());
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
