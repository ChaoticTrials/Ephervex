//! The Mods list card is exactly as tall as its rows, whatever the count.
//!
//! A floor on the card's height must not pad a short list: a filter that
//! leaves one mod shows a card of one row, with no empty band under it.

use crate::support;

use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, GuiI18n, InstanceRow, ModRow, Theme};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Image, ModelRc, PhysicalSize, SharedString, VecModel};

const WIDE: (u32, u32) = (1480, 900);
const NARROW: (u32, u32) = (700, 900);

#[test]
fn a_card_with_one_row_is_as_tall_as_that_row_and_its_border() {
    let ui = window(WIDE);
    show(&ui, 1);

    assert_eq!(card_height(&ui), rows_height(&ui, 1));
}

#[test]
fn a_card_with_two_rows_is_as_tall_as_those_rows_and_its_border() {
    let ui = window(WIDE);
    show(&ui, 2);

    assert_eq!(card_height(&ui), rows_height(&ui, 2));
}

#[test]
fn a_card_with_one_row_has_no_empty_band_in_the_compact_layout() {
    let ui = window(NARROW);
    show(&ui, 1);

    assert_eq!(card_height(&ui), rows_height(&ui, 1));
}

/// Narrowing the filter drops the card by exactly the row that went.
#[test]
fn narrowing_the_filter_to_one_mod_shrinks_the_card_to_its_row() {
    let ui = window(WIDE);
    show(&ui, 4);
    let four = card_height(&ui);
    show(&ui, 1);

    assert_eq!(four, rows_height(&ui, 4));
    assert_eq!(card_height(&ui), rows_height(&ui, 1));
}

/// A list taller than the window still fills the room it is given and
/// scrolls, instead of growing to hold every row.
#[test]
fn a_list_taller_than_the_window_stays_within_it() {
    let ui = window((1480, 500));
    show(&ui, 40);

    let height = card_height(&ui);
    assert!(
        height < rows_height(&ui, 40),
        "the card scrolls rather than holding every row: {height}"
    );
    assert!(height >= rows_height(&ui, 1));
}

fn card_height(ui: &AppWindow) -> f32 {
    ElementHandle::find_by_element_id(ui, "ManagedModsPanel::mod-card")
        .find(|element| element.size().width > 0.0)
        .expect("the card is on screen")
        .size()
        .height
}

fn rows_height(ui: &AppWindow, count: u8) -> f32 {
    let theme = ui.global::<Theme>();
    f32::from(count) * theme.get_mod_row_height() + 2.0 * theme.get_strong_border_width()
}

fn mod_row(index: usize) -> ModRow {
    let title = format!("Mod {index}");
    ModRow {
        project_id: title.clone().into(),
        logical_path: format!("mods/mod-{index}.jar").into(),
        title: title.into(),
        file_name: format!("mod-{index}.jar").into(),
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
        can_remove: true,
        can_delete_file: true,
        restriction: SharedString::default(),
        suspect_reason: SharedString::default(),
        suspect_confidence: SharedString::default(),
    }
}

fn show(ui: &AppWindow, count: u8) {
    let view = support::client_view(InstanceRow {
        id: "an-id".into(),
        ..InstanceRow::default()
    });
    support::show(ui, vec![view], vec![support::pane(1, 0, "Mods")]);
    let rows: Vec<ModRow> = (0..usize::from(count)).map(mod_row).collect();
    support::update_host(ui, 0, |view| {
        view.mods = ModelRc::from(Rc::new(VecModel::from(rows)));
    });
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().expect("render");
}

fn window((width, height): (u32, u32)) -> AppWindow {
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
    ui.window().set_size(PhysicalSize::new(width, height));
    ui.show().expect("show");
    ui
}
