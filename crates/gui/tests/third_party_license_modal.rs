//! The "About & Licenses" screen's third-party license surface.
//!
//! Two things have to hold here and nowhere else. Ephervex uses Slint under its
//! royalty-free license, whose attribution condition is satisfied by showing
//! Slint's own `AboutSlint` widget on an About screen reachable from the top
//! level (ADR 0001): losing that widget would put a release outside the terms
//! it ships under. And the list itself is generated from the real dependency
//! graph, so a screen that showed nothing would be a screen claiming Ephervex
//! depends on nothing.

#![allow(clippy::expect_used)]

use ephervex_gui::presentation::ThirdPartyLicenses;
use ephervex_gui::ui::{AppWindow, GuiI18n};
use ephervex_i18n::{Arg, lookup, lookup_args};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Model as _, PhysicalSize};

#[test]
fn the_screen_lists_what_this_build_is_actually_made_of() {
    let ui = window();
    ephervex_gui::controller::show_third_party_licenses(&ui);

    assert_eq!(
        ui.get_third_party_error().as_str(),
        "",
        "this build's own generated inventory has to be readable"
    );
    let packages = ui.get_third_party_packages();
    assert!(
        packages.row_count() > 100,
        "a release depends on hundreds of packages, not {}",
        packages.row_count()
    );
    assert!(!ui.get_third_party_summary().is_empty());

    for index in 0..packages.row_count() {
        let package = packages.row_data(index).expect("a row");
        assert!(!package.name.is_empty(), "row {index} has no name");
        assert!(
            !package.version.is_empty(),
            "{} has no version",
            package.name
        );
        assert!(
            !package.license.is_empty(),
            "{} has no license",
            package.name
        );
        assert!(
            !package.text.is_empty(),
            "{} is listed with no license text, so a reader cannot see its terms",
            package.name
        );
    }
}

#[test]
fn the_slint_terms_the_application_is_built_under_are_on_the_screen() {
    let ui = window();
    ephervex_gui::controller::show_third_party_licenses(&ui);

    let packages = ui.get_third_party_packages();
    let slint_row = (0..packages.row_count())
        .filter_map(|index| packages.row_data(index))
        .find(|package| package.name == "slint")
        .expect("the desktop application is built on slint");
    assert!(
        slint_row.text.contains("AboutSlint"),
        "slint is listed without the royalty-free terms Ephervex uses it under"
    );
}

#[test]
fn the_bundled_fonts_are_on_the_screen_with_their_notices() {
    let ui = window();
    ephervex_gui::controller::show_third_party_licenses(&ui);

    let packages = ui.get_third_party_packages();
    for (font, notice) in [
        ("Pixelify Sans (font)", "The Pixelify Sans Project Authors"),
        ("Space Grotesk (font)", "The Space Grotesk Project Authors"),
        ("IBM Plex Mono (font)", "Reserved Font Name \"Plex\""),
    ] {
        let row = (0..packages.row_count())
            .filter_map(|index| packages.row_data(index))
            .find(|package| package.name == font);
        assert!(row.is_some(), "{font} is bundled but not listed");
        assert!(
            row.expect("checked above").text.contains(notice),
            "{font} is listed without its own notice"
        );
    }
}

#[test]
fn the_slint_attribution_widget_is_on_the_about_screen() {
    // The license condition itself, not a nicety. The widget lives on the
    // About & Licenses tab, which the sidebar reaches in one step.
    let ui = window();
    ui.set_loading(false);
    ui.set_page(4);
    ui.set_settings_tab("About & Licenses".into());
    settle(&ui);

    let found = ElementHandle::find_by_element_type_name(&ui, "AboutSlint");
    assert_eq!(
        found.count(),
        1,
        "the About screen no longer shows Slint's attribution widget"
    );
}

#[test]
fn opening_the_list_starts_at_the_list_rather_than_the_last_license_read() {
    let ui = window();
    ephervex_gui::controller::show_third_party_licenses(&ui);
    assert_eq!(ui.get_third_party_open(), -1);
}

#[test]
fn the_presentation_model_joins_every_text_a_package_is_covered_by() {
    // A package offered under two licenses ships both, and showing one of them
    // would be showing half the terms.
    let licenses = ThirdPartyLicenses::bundled();
    assert!(licenses.error.is_none(), "{:?}", licenses.error);
    let dual = licenses
        .packages
        .iter()
        .find(|package| package.license.contains(" OR ") || package.license.contains(" AND "))
        .expect("some dependency offers a choice of licenses");
    assert!(!dual.text.is_empty(), "{} carries no text", dual.name);
}

fn settle(ui: &AppWindow) {
    ui.window().request_redraw();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(32));
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
    ui.window().set_size(PhysicalSize::new(1440, 1000));
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
