//! Settings cards that share a row.
//!
//! The settings screen puts two cards side by side where the window is wide
//! enough. Their descriptions wrap to different numbers of lines, so without a
//! shared height the shorter one floats against a taller neighbor and the page
//! reads as unfinished.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, PhysicalSize};

#[test]
fn two_settings_cards_sharing_a_row_end_level() {
    let ui = window();
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_workspace_tab("Java".into());
    // Wide enough for the two-column layout, and tall enough that every card
    // is on screen to be measured.
    ui.window().set_size(PhysicalSize::new(1600, 1600));
    settle(&ui);

    let rows = card_rows(&ui);
    assert!(
        rows.len() >= 2,
        "the Java page has {} rows of cards to compare",
        rows.len()
    );
    for (top, heights) in rows {
        let (shortest, tallest) = (
            heights.iter().copied().fold(f32::MAX, f32::min),
            heights.iter().copied().fold(0.0, f32::max),
        );
        assert!(
            tallest - shortest < 1.0,
            "cards at {top}px are {shortest}px and {tallest}px tall"
        );
    }
}

/// The visible cards, grouped by the row they sit in and reduced to their
/// heights. Rounded to the pixel so two cards laid out at the same height are
/// grouped together rather than split by a fraction.
fn card_rows(ui: &AppWindow) -> Vec<(i32, Vec<f32>)> {
    let mut rows: Vec<(i32, Vec<f32>)> = Vec::new();
    let mut cards: Vec<(i32, f32)> = ElementHandle::find_by_element_type_name(ui, "SettingCard")
        .filter(|card| card.size().width > 0.0)
        .map(|card| {
            (
                #[allow(clippy::cast_possible_truncation)]
                {
                    card.absolute_position().y.round() as i32
                },
                card.size().height,
            )
        })
        .collect();
    cards.sort_by_key(|(top, _)| *top);
    for (top, height) in cards {
        match rows.last_mut() {
            Some((row_top, heights)) if (*row_top - top).abs() < 2 => heights.push(height),
            _ => rows.push((top, vec![height])),
        }
    }
    rows.retain(|(_, heights)| heights.len() > 1);
    rows
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
