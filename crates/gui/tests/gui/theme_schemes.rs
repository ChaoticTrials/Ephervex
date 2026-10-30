//! The interface's built-in palettes.
//!
//! Every color the frontend paints with is named in the theme and resolves
//! through the active palette. What is worth proving is that changing palette
//! reaches all of them: a token that kept a single value is invisible until
//! somebody is looking at a dark card on a light page.

use ephervex_gui::ui::{AppWindow, BuiltInPalettes, Theme};
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::language::ColorScheme;
use slint::{Color, ComponentHandle};

#[test]
fn an_untouched_install_paints_the_light_workbench_page() {
    let ui = window();
    let theme = ui.global::<Theme>();
    let palettes = ui.global::<BuiltInPalettes>();
    assert_eq!(
        theme.get_palette(),
        palettes.get_workbench(),
        "workbench is what an untouched install gets"
    );
    assert_eq!(theme.get_widget_scheme(), ColorScheme::Light);

    let surfaces = [
        ("background", theme.get_background()),
        ("surface", theme.get_surface()),
        ("field", theme.get_field()),
        ("elevated", theme.get_elevated()),
        ("bar", theme.get_bar()),
        ("well", theme.get_well()),
    ];
    let foregrounds = [
        ("text", theme.get_text()),
        ("text-secondary", theme.get_text_secondary()),
        ("text-muted", theme.get_text_muted()),
    ];
    for (surface, page) in surfaces {
        for (foreground, ink) in foregrounds {
            assert!(
                luminance(ink) < luminance(page),
                "{foreground} is not darker than {surface}, so the widgets' light scheme would not read"
            );
        }
    }
}

#[test]
fn workbench_dark_paints_a_dark_page_the_widgets_read_on() {
    let ui = window();
    let theme = ui.global::<Theme>();
    let palettes = ui.global::<BuiltInPalettes>();
    theme.set_palette(palettes.get_workbench_dark());
    assert_eq!(theme.get_widget_scheme(), ColorScheme::Dark);

    for (surface, page) in [
        ("background", theme.get_background()),
        ("surface", theme.get_surface()),
        ("field", theme.get_field()),
        ("elevated", theme.get_elevated()),
    ] {
        for (foreground, ink) in [
            ("text", theme.get_text()),
            ("text-secondary", theme.get_text_secondary()),
            ("text-muted", theme.get_text_muted()),
        ] {
            assert!(
                luminance(ink) > luminance(page),
                "{foreground} is not lighter than {surface}, so the widgets' dark scheme would not read"
            );
        }
    }
    assert_ne!(
        theme.get_shadow_hard(),
        theme.get_border_strong(),
        "the hard shadow must stay visible against a light outline"
    );
}

/// How bright a color reads, weighted the way an eye sees the three channels.
fn luminance(color: Color) -> u32 {
    u32::from(color.red()) * 299 + u32::from(color.green()) * 587 + u32::from(color.blue()) * 114
}

fn window() -> AppWindow {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        mock_time: true,
        threading: false,
    })))
    .expect("testing backend");
    AppWindow::new().expect("window")
}
