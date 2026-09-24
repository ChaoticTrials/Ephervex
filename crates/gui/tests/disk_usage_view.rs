//! The disk usage view in Settings: how it is reached, what it shows while it
//! measures and after, and what its actions ask for.
#![allow(clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::rc::Rc;

use ephervex_gui::ui::{AppWindow, DiskUsageRow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{
    AccessibleRole, ElementHandle, TestingBackend, TestingBackendOptions,
};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};

fn visible(ui: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(ui, text)
        .next()
        .is_some()
}

fn labeled_button(ui: &AppWindow, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(ui, label)
        .filter(|element| element.accessible_role() == Some(AccessibleRole::Button))
        .max_by(|left, right| left.size().width.total_cmp(&right.size().width))
        .unwrap_or_else(|| panic!("no button labeled {label}"))
}

fn button(ui: &AppWindow, message: &str) -> ElementHandle {
    labeled_button(ui, &lookup(message))
}

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(*value))
            .collect::<Vec<_>>(),
    ))
}

fn row(id: &str, label: &str, size: &str, fraction: f32, locations: &[&str]) -> DiskUsageRow {
    DiskUsageRow {
        id: id.into(),
        label: label.into(),
        detail: SharedString::default(),
        size: size.into(),
        fraction,
        locations: strings(locations),
    }
}

fn settings(ui: &AppWindow, tab: &str) {
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_workspace(SharedString::default());
    ui.set_workspace_tab(tab.into());
    settle(ui);
}

/// The view after a measurement that found a cache in two folders and could
/// not read one place.
fn measured(ui: &AppWindow) {
    ui.set_disk_usage_state("measured".into());
    ui.set_disk_usage_total("1.0 GiB".into());
    ui.set_disk_usage_measured_at("Measured at 2026-09-24 18:00".into());
    ui.set_disk_usage_retention(ModelRc::new(VecModel::from(vec![
        row("persistent", "Kept", "700.0 MiB", 0.7, &[]),
        row("regenerable", "Rebuildable", "300.0 MiB", 0.3, &[]),
    ])));
    ui.set_disk_usage_categories(ModelRc::new(VecModel::from(vec![
        row(
            "client_instances",
            "Client instances",
            "700.0 MiB",
            0.7,
            &["/data/instances"],
        ),
        row(
            "download_cache",
            "Download cache",
            "300.0 MiB",
            0.3,
            &["/cache/downloads", "/cache/metadata"],
        ),
        row("mods", "Mods", "0 B", 0.0, &[]),
    ])));
    ui.set_disk_usage_incomplete(true);
    ui.set_disk_usage_incomplete_text("One place could not be read".into());
    ui.set_disk_usage_unreadable(strings(&["/data/instances/x/saves"]));
    settle(ui);
}

#[test]
fn disk_usage_is_a_settings_page_that_measures_when_asked() {
    let ui = window();
    let asked = Rc::new(RefCell::new(0));
    let sink = Rc::clone(&asked);
    ui.on_measure_disk_usage(move || *sink.borrow_mut() += 1);
    settings(&ui, "General");

    ElementHandle::find_by_accessible_label(&ui, &lookup("gui-settings-disk-usage"))
        .next()
        .expect("a settings entry")
        .mock_single_click(PointerEventButton::Left);
    settle(&ui);
    assert_eq!(ui.get_workspace_tab(), "Disk Usage");
    assert_eq!(
        *asked.borrow(),
        0,
        "opening the page does not walk the disk"
    );
    assert!(visible(&ui, &lookup("gui-disk-usage-idle")));

    button(&ui, "gui-disk-usage-measure").mock_single_click(PointerEventButton::Left);
    assert_eq!(*asked.borrow(), 1);
}

#[test]
fn a_measurement_in_progress_says_so_and_can_be_stopped() {
    let ui = window();
    let stopped = Rc::new(RefCell::new(false));
    let sink = Rc::clone(&stopped);
    ui.on_stop_disk_usage(move || *sink.borrow_mut() = true);
    settings(&ui, "Disk Usage");
    ui.set_disk_usage_state("measuring".into());
    ui.set_disk_usage_progress(
        lookup_args("gui-disk-usage-progress", [("count", Arg::from(1500))]).into(),
    );
    settle(&ui);

    assert!(visible(
        &ui,
        &lookup_args("gui-disk-usage-progress", [("count", Arg::from(1500))])
    ));
    assert_eq!(
        button(&ui, "gui-disk-usage-refresh").accessible_enabled(),
        Some(false),
        "no second walk while one runs"
    );
    button(&ui, "gui-disk-usage-stop").mock_single_click(PointerEventButton::Left);
    assert!(*stopped.borrow());
}

#[test]
fn a_measurement_shows_its_total_every_category_in_words_and_what_it_missed() {
    let ui = window();
    settings(&ui, "Disk Usage");
    measured(&ui);

    assert!(visible(
        &ui,
        &lookup_args("gui-disk-usage-total", [("value", Arg::from("1.0 GiB"))])
    ));
    for text in [
        "Client instances",
        "700.0 MiB",
        "Download cache",
        "300.0 MiB",
        "Mods",
        "0 B",
    ] {
        assert!(visible(&ui, text), "{text} is written, not only drawn");
    }
    assert!(
        visible(&ui, "Kept") && visible(&ui, "Rebuildable"),
        "the legend names each part"
    );
    assert!(visible(&ui, &lookup("gui-disk-usage-retention-bar")));
    assert!(visible(&ui, "One place could not be read"));
    assert!(visible(&ui, "/data/instances/x/saves"));
}

#[test]
fn a_category_folder_opens_by_category_and_position() {
    let ui = window();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&opened);
    ui.on_open_storage_location(move |id, index| sink.borrow_mut().push((id.to_string(), index)));
    let refreshed = Rc::new(RefCell::new(0));
    let refresh_sink = Rc::clone(&refreshed);
    ui.on_measure_disk_usage(move || *refresh_sink.borrow_mut() += 1);
    settings(&ui, "Disk Usage");
    measured(&ui);

    labeled_button(
        &ui,
        &lookup_args(
            "gui-disk-usage-open",
            [("value", Arg::from("/cache/metadata"))],
        ),
    )
    .mock_single_click(PointerEventButton::Left);
    assert_eq!(
        opened.borrow().as_slice(),
        [("download_cache".to_owned(), 1)]
    );
    assert!(
        ElementHandle::find_by_accessible_label(
            &ui,
            &lookup_args("gui-disk-usage-open", [("value", Arg::from(""))])
        )
        .next()
        .is_none(),
        "a category with no folder of its own offers none"
    );

    button(&ui, "gui-disk-usage-refresh").mock_single_click(PointerEventButton::Left);
    assert_eq!(*refreshed.borrow(), 1, "measuring again is a new request");
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
    let global = ui.global::<GuiI18n>();
    global.on_text(|id| lookup(id.as_str()).into());
    global.on_count(|id, count| {
        lookup_args(id.as_str(), [("count", Arg::from(i64::from(count)))]).into()
    });
    global.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    global.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
    // Tall enough that the whole surface is on screen: the testing backend
    // finds only what is within the window, and the bundle choice sits below
    // the verification results.
    ui.window().set_size(PhysicalSize::new(1480, 1400));
    ui.show().expect("show");
    ui
}
