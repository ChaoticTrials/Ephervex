//! The interface's built-in palettes.
//!
//! Every color the frontend paints with is named in the theme and resolves
//! through the active palette. What is worth proving is that changing palette
//! reaches all of them: a token that kept a single value is invisible until
//! somebody is looking at a dark card on a light page.

#![allow(clippy::expect_used)]

use ephervex_gui::ui::{AppWindow, BuiltInPalettes, Theme};
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::language::ColorScheme;
use slint::{Color, ComponentHandle};

#[test]
fn the_light_scheme_lifts_every_surface_and_darkens_every_foreground() {
    let ui = window();
    let theme = ui.global::<Theme>();
    let palettes = ui.global::<BuiltInPalettes>();
    assert_eq!(
        theme.get_palette(),
        palettes.get_dark(),
        "dark is what an untouched install gets"
    );
    assert_eq!(theme.get_widget_scheme(), ColorScheme::Dark);

    let surfaces = |theme: &Theme<'_>| {
        [
            ("background", theme.get_background()),
            ("surface", theme.get_surface()),
            ("elevated", theme.get_elevated()),
            ("bar", theme.get_bar()),
            ("well", theme.get_well()),
            ("border", theme.get_border()),
        ]
    };
    let foregrounds = |theme: &Theme<'_>| {
        [
            ("text", theme.get_text()),
            ("text-secondary", theme.get_text_secondary()),
        ]
    };

    let (dark_surfaces, dark_foregrounds) = (surfaces(&theme), foregrounds(&theme));
    theme.set_palette(palettes.get_light());
    assert_eq!(theme.get_widget_scheme(), ColorScheme::Light);
    let (light_surfaces, light_foregrounds) = (surfaces(&theme), foregrounds(&theme));

    for ((name, dark), (_, light)) in dark_surfaces.iter().zip(&light_surfaces) {
        assert!(
            luminance(*light) > luminance(*dark),
            "{name} reads {} dark and {} light, so it never changed scheme",
            luminance(*dark),
            luminance(*light)
        );
    }
    for ((name, dark), (_, light)) in dark_foregrounds.iter().zip(&light_foregrounds) {
        assert!(
            luminance(*light) < luminance(*dark),
            "{name} stays light on a light page"
        );
    }
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
