//! Settings rows in a group.
//!
//! Settings that belong together are rows of one card. Their descriptions wrap
//! to different numbers of lines, so each row takes its own height, and every
//! row of a group is as wide as the group.

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, PhysicalSize};

#[test]
fn the_rows_of_a_group_are_as_wide_as_the_group_and_do_not_overlap() {
    let ui = window();
    ui.set_loading(false);
    ui.set_settings_open(true);

    for tab in ["General", "Java"] {
        ui.set_settings_tab(tab.into());
        for width in [1240, 1570, 1920] {
            ui.window().set_size(PhysicalSize::new(width, 1600));
            settle(&ui);

            let mut rows: Vec<(f32, f32, f32, f32)> =
                ElementHandle::find_by_element_type_name(&ui, "SettingRow")
                    .filter(|row| row.size().width > 0.0)
                    .map(|row| {
                        (
                            row.absolute_position().x,
                            row.absolute_position().y,
                            row.size().width,
                            row.size().height,
                        )
                    })
                    .collect();
            assert!(rows.len() > 1, "{tab} at {width}px has rows to compare");
            rows.sort_by(|left, right| left.1.total_cmp(&right.1));
            let (x, _, group_width, _) = rows[0];
            for pair in rows.windows(2) {
                assert!(
                    (pair[1].0 - x).abs() < 1.0 && (pair[1].2 - group_width).abs() < 1.0
                        || pair[1].1 > pair[0].1 + pair[0].3,
                    "{tab} at {width}px: rows of one group differ in width"
                );
                assert!(
                    pair[0].1 + pair[0].3 <= pair[1].1 + 0.5,
                    "{tab} at {width}px: a row overlaps the next"
                );
            }
        }
    }
}

/// The provider overrides are rows of one card, one under the other.
#[test]
fn the_provider_overrides_are_rows_of_one_group() {
    let ui = window();
    ui.set_loading(false);
    ui.invoke_open_settings("Providers & APIs".into());
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    settle(&ui);

    let mut cards: Vec<(f32, f32)> = ElementHandle::find_by_element_type_name(&ui, "OverrideCard")
        .filter(|card| card.size().width > 0.0)
        .map(|card| (card.absolute_position().x, card.absolute_position().y))
        .collect();
    cards.sort_by(|left, right| left.1.total_cmp(&right.1));
    assert_eq!(cards.len(), 2, "both overrides are on screen");
    assert!(
        (cards[0].0 - cards[1].0).abs() < 1.0,
        "both start at the group's left edge"
    );
    assert!(cards[1].1 > cards[0].1, "the second is under the first");
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
