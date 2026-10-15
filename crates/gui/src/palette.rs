//! Which palette the interface paints in.
//!
//! The preference and the palette are separate things. A preference names what
//! the user asked for; this module resolves it to one complete palette and
//! hands that to `Theme`, which is the only reader. Components read `Theme`
//! tokens and never learn which palette is active or where it came from.

use ephervex_core::{ApplicationColorScheme, ApplicationTheme};
use ephervex_i18n::lookup;
use slint::{Color, ComponentHandle};

use crate::theme_files::UserTheme;
use crate::ui::{AppWindow, BuiltInPalettes, Theme, ThemePalette};

/// The built-in schemes in the order the theme list offers them.
const BUILT_IN: [ApplicationColorScheme; 2] =
    [ApplicationColorScheme::Dark, ApplicationColorScheme::Light];

/// Every color token a theme file may set, by its `Theme` name.
pub(crate) const PALETTE_COLORS: &[&str] = &[
    "background",
    "surface",
    "field",
    "elevated",
    "overlay",
    "border",
    "bar",
    "well",
    "selection",
    "selection-card",
    "selection-strong",
    "toggle-on",
    "toggle-knob",
    "capture-active",
    "accent",
    "accent-secondary",
    "success",
    "warning",
    "danger",
    "primary-fill",
    "primary-fill-hover",
    "on-primary",
    "danger-fill",
    "danger-fill-hover",
    "text",
    "text-secondary",
    "text-muted",
    "on-danger",
    "log-error",
    "server",
    "modrinth",
    "curseforge",
    "vanilla",
    "forge",
    "neoforge",
    "shadow",
    "scrim",
];

/// Tokens that were once the color of another token. A theme file written
/// before they could be set separately keeps its look: when it sets the old
/// token and leaves the new one out, the new one follows it.
const INHERITS_WHEN_UNSET: [(&str, &str); 3] = [
    ("toggle-knob", "accent"),
    ("field", "background"),
    ("on-primary", "text"),
];

/// The palette field behind a color token.
fn color_mut<'a>(palette: &'a mut ThemePalette, token: &str) -> Option<&'a mut Color> {
    Some(match token {
        "background" => &mut palette.background,
        "surface" => &mut palette.surface,
        "field" => &mut palette.field,
        "elevated" => &mut palette.elevated,
        "overlay" => &mut palette.overlay,
        "border" => &mut palette.border,
        "bar" => &mut palette.bar,
        "well" => &mut palette.well,
        "selection" => &mut palette.selection,
        "selection-card" => &mut palette.selection_card,
        "selection-strong" => &mut palette.selection_strong,
        "toggle-on" => &mut palette.toggle_on,
        "toggle-knob" => &mut palette.toggle_knob,
        "capture-active" => &mut palette.capture_active,
        "accent" => &mut palette.accent,
        "accent-secondary" => &mut palette.accent_secondary,
        "success" => &mut palette.success,
        "warning" => &mut palette.warning,
        "danger" => &mut palette.danger,
        "primary-fill" => &mut palette.primary_fill,
        "primary-fill-hover" => &mut palette.primary_fill_hover,
        "on-primary" => &mut palette.on_primary,
        "danger-fill" => &mut palette.danger_fill,
        "danger-fill-hover" => &mut palette.danger_fill_hover,
        "text" => &mut palette.text,
        "text-secondary" => &mut palette.text_secondary,
        "text-muted" => &mut palette.text_muted,
        "on-danger" => &mut palette.on_danger,
        "log-error" => &mut palette.log_error,
        "server" => &mut palette.server,
        "modrinth" => &mut palette.modrinth,
        "curseforge" => &mut palette.curseforge,
        "vanilla" => &mut palette.vanilla,
        "forge" => &mut palette.forge,
        "neoforge" => &mut palette.neoforge,
        "shadow" => &mut palette.shadow,
        "scrim" => &mut palette.scrim,
        _ => return None,
    })
}

/// The built-in palette a color scheme paints in.
fn built_in_palette(ui: &AppWindow, scheme: ApplicationColorScheme) -> ThemePalette {
    let palettes = ui.global::<BuiltInPalettes>();
    match scheme {
        ApplicationColorScheme::Dark => palettes.get_dark(),
        ApplicationColorScheme::Light => palettes.get_light(),
    }
}

fn built_in_label(scheme: ApplicationColorScheme) -> String {
    lookup(match scheme {
        ApplicationColorScheme::Dark => "gui-color-scheme-dark",
        ApplicationColorScheme::Light => "gui-color-scheme-light",
    })
}

/// A user theme: its parent's palette with the colors it sets.
fn user_palette(ui: &AppWindow, theme: &UserTheme) -> ThemePalette {
    let mut palette = built_in_palette(ui, theme.parent);
    for (token, color) in &theme.colors {
        if let Some(field) = color_mut(&mut palette, token) {
            *field = *color;
        }
    }
    let set_by_theme = |name: &str| {
        theme
            .colors
            .iter()
            .find(|(set, _)| *set == name)
            .map(|(_, color)| *color)
    };
    for (token, replaces) in INHERITS_WHEN_UNSET {
        if set_by_theme(token).is_none()
            && let Some((field, color)) = color_mut(&mut palette, token).zip(set_by_theme(replaces))
        {
            *field = color;
        }
    }
    palette
}

/// The complete palette a preference paints in. A user theme whose file is
/// gone paints in the default scheme, and the preference is left as it is.
fn resolve(ui: &AppWindow, theme: &ApplicationTheme, user_themes: &[UserTheme]) -> ThemePalette {
    match theme {
        ApplicationTheme::BuiltIn(scheme) => built_in_palette(ui, *scheme),
        ApplicationTheme::User(id) => user_themes.iter().find(|user| user.id == *id).map_or_else(
            || built_in_palette(ui, ApplicationColorScheme::default()),
            |user| user_palette(ui, user),
        ),
    }
}

/// Repaints the interface in the palette the preference resolves to.
pub(crate) fn apply_theme(ui: &AppWindow, theme: &ApplicationTheme, user_themes: &[UserTheme]) {
    ui.global::<Theme>()
        .set_palette(resolve(ui, theme, user_themes));
}

/// One entry of the theme list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThemeChoice {
    pub(crate) theme: ApplicationTheme,
    pub(crate) label: String,
}

/// The theme list: the built-in schemes, then the user's themes.
pub(crate) fn theme_choices(user_themes: &[UserTheme]) -> Vec<ThemeChoice> {
    BUILT_IN
        .into_iter()
        .map(|scheme| ThemeChoice {
            theme: ApplicationTheme::BuiltIn(scheme),
            label: built_in_label(scheme),
        })
        .chain(user_themes.iter().map(|user| ThemeChoice {
            theme: user.preference(),
            label: user.name.clone(),
        }))
        .collect()
}

/// Where the list shows `theme`. A user theme whose file is gone shows as the
/// default scheme it paints in.
pub(crate) fn choice_index(choices: &[ThemeChoice], theme: &ApplicationTheme) -> usize {
    let default = ApplicationTheme::default();
    choices
        .iter()
        .position(|choice| choice.theme == *theme)
        .or_else(|| choices.iter().position(|choice| choice.theme == default))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::collections::BTreeSet;

    use super::*;
    use crate::test_support::{ui_declaration, ui_markup};

    const SCHEMES: [ApplicationColorScheme; 2] = BUILT_IN;

    fn window() -> AppWindow {
        i_slint_backend_testing::init_no_event_loop();
        AppWindow::new().expect("window")
    }

    /// A sample of tokens as `Theme` resolves them, and as the palette names
    /// them, for comparing the two.
    fn resolved(theme: &Theme<'_>) -> [slint::Color; 8] {
        [
            theme.get_background(),
            theme.get_surface(),
            theme.get_border(),
            theme.get_accent(),
            theme.get_danger(),
            theme.get_text(),
            theme.get_text_secondary(),
            theme.get_scrim(),
        ]
    }

    fn named(palette: &ThemePalette) -> [slint::Color; 8] {
        [
            palette.background,
            palette.surface,
            palette.border,
            palette.accent,
            palette.danger,
            palette.text,
            palette.text_secondary,
            palette.scrim,
        ]
    }

    #[test]
    fn an_untouched_window_paints_in_the_default_schemes_palette() {
        let ui = window();
        let expected = built_in_palette(&ui, ApplicationColorScheme::default());
        assert_eq!(ui.global::<Theme>().get_palette(), expected);
    }

    #[test]
    fn each_scheme_resolves_to_its_own_palette_and_repaints_the_tokens() {
        let ui = window();
        let theme = ui.global::<Theme>();
        for scheme in SCHEMES.into_iter().chain(SCHEMES.into_iter().rev()) {
            apply_theme(&ui, &ApplicationTheme::BuiltIn(scheme), &[]);
            let palette = built_in_palette(&ui, scheme);
            assert_eq!(resolved(&theme), named(&palette), "{scheme:?}");
            assert_eq!(theme.get_widget_scheme(), palette.widget_scheme);
        }
        let dark = built_in_palette(&ui, ApplicationColorScheme::Dark);
        let light = built_in_palette(&ui, ApplicationColorScheme::Light);
        assert_ne!(dark, light, "two schemes that paint the same");
    }

    fn user(
        id: &str,
        parent: ApplicationColorScheme,
        colors: &[(&'static str, Color)],
    ) -> UserTheme {
        UserTheme {
            id: id.to_owned(),
            name: id.to_uppercase(),
            parent,
            colors: colors.to_vec(),
            branding: crate::theme_files::ThemeBranding::default(),
        }
    }

    #[test]
    fn a_user_theme_is_its_parent_with_the_colors_it_sets() {
        let ui = window();
        let marker = Color::from_argb_u8(255, 1, 2, 3);
        let themes = [user(
            "mine",
            ApplicationColorScheme::Light,
            &[("border", marker)],
        )];

        apply_theme(&ui, &ApplicationTheme::User("mine".to_owned()), &themes);

        let mut expected = built_in_palette(&ui, ApplicationColorScheme::Light);
        expected.border = marker;
        assert_eq!(ui.global::<Theme>().get_palette(), expected);
        assert_eq!(ui.global::<Theme>().get_border(), marker);
    }

    /// A theme file written before a token could be set on its own keeps its
    /// look: the new token follows the token it used to be, and a theme that
    /// sets the new token itself is taken at its word.
    #[test]
    fn a_token_split_from_another_follows_it_until_the_theme_sets_it() {
        let ui = window();
        let old = Color::from_argb_u8(255, 1, 2, 3);
        let own = Color::from_argb_u8(255, 4, 5, 6);
        let themes = [
            user(
                "old",
                ApplicationColorScheme::Light,
                &[("accent", old), ("background", old), ("text", old)],
            ),
            user(
                "new",
                ApplicationColorScheme::Light,
                &[
                    ("accent", old),
                    ("background", old),
                    ("text", old),
                    ("toggle-knob", own),
                    ("field", own),
                    ("on-primary", own),
                ],
            ),
            user(
                "untouched",
                ApplicationColorScheme::Light,
                &[("border", old)],
            ),
        ];
        let palette_of = |id: &str| {
            apply_theme(&ui, &ApplicationTheme::User(id.to_owned()), &themes);
            ui.global::<Theme>().get_palette()
        };

        let palette = palette_of("old");
        assert_eq!(
            [palette.toggle_knob, palette.field, palette.on_primary],
            [old; 3]
        );
        let palette = palette_of("new");
        assert_eq!(
            [palette.toggle_knob, palette.field, palette.on_primary],
            [own; 3]
        );
        let light = built_in_palette(&ui, ApplicationColorScheme::Light);
        let palette = palette_of("untouched");
        assert_eq!(
            [palette.toggle_knob, palette.field, palette.on_primary],
            [light.toggle_knob, light.field, light.on_primary]
        );
    }

    #[test]
    fn a_user_theme_whose_file_is_gone_paints_in_the_default_scheme() {
        let ui = window();
        apply_theme(
            &ui,
            &ApplicationTheme::BuiltIn(ApplicationColorScheme::Light),
            &[],
        );
        apply_theme(&ui, &ApplicationTheme::User("gone".to_owned()), &[]);
        assert_eq!(
            ui.global::<Theme>().get_palette(),
            built_in_palette(&ui, ApplicationColorScheme::default())
        );
    }

    #[test]
    fn the_theme_list_offers_the_built_ins_then_the_users_themes() {
        let themes = [user("mine", ApplicationColorScheme::Dark, &[])];
        let choices = theme_choices(&themes);
        let offered: Vec<ApplicationTheme> =
            choices.iter().map(|choice| choice.theme.clone()).collect();
        assert_eq!(
            offered,
            [
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Dark),
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Light),
                ApplicationTheme::User("mine".to_owned()),
            ]
        );
        assert_eq!(choices[2].label, "MINE");
        assert_eq!(
            choice_index(&choices, &ApplicationTheme::User("mine".to_owned())),
            2
        );
        assert_eq!(
            choice_index(&choices, &ApplicationTheme::User("gone".to_owned())),
            choice_index(&choices, &ApplicationTheme::default())
        );
    }

    /// A theme file names tokens the way `Theme` does, so the list a file is
    /// checked against has to be exactly the palette's colors, each reaching
    /// the field of its own name.
    #[test]
    fn every_palette_color_is_settable_by_its_token_name() {
        let fields: Vec<String> = palette_fields(ui_markup())
            .into_iter()
            .filter(|field| *field != "widget-scheme")
            .map(str::to_owned)
            .collect();
        assert_eq!(PALETTE_COLORS, fields.as_slice());

        let marker = Color::from_argb_u8(255, 1, 2, 3);
        for token in PALETTE_COLORS {
            let mut palette = ThemePalette::default();
            *color_mut(&mut palette, token).expect("a known token") = marker;
            let written = format!("{palette:?}");
            let field = token.replace('-', "_");
            assert!(
                [", ", "{ "]
                    .iter()
                    .any(|before| written.contains(&format!("{before}{field}: {marker:?}"))),
                "`{token}` does not reach `{field}`: {written}"
            );
            assert_eq!(
                written.matches(&format!("{marker:?}")).count(),
                1,
                "{token}"
            );
        }
        assert!(color_mut(&mut ThemePalette::default(), "widget-scheme").is_none());
    }

    /// The field names of `ThemePalette`, in the order declared.
    fn palette_fields(source: &str) -> Vec<&str> {
        let declaration = ui_declaration(source, "export struct ThemePalette {")
            .expect("the markup declares the palette shape");
        declaration
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.starts_with("//") && line.contains(':'))
            .filter_map(|line| line.split(':').next())
            .collect()
    }

    /// Slint fills a field a struct literal leaves out with its default, which
    /// for a color is transparent. A palette missing a token would compile and
    /// paint that token invisible.
    #[test]
    fn every_built_in_palette_sets_every_token() {
        let source = ui_markup();
        let fields: BTreeSet<&str> = palette_fields(source).into_iter().collect();
        assert!(fields.contains("background") && fields.contains("widget-scheme"));

        let palettes = ui_declaration(source, "export global BuiltInPalettes {")
            .expect("the markup declares the built-in palettes");
        let mut seen = 0;
        for block in palettes
            .split("out property <ThemePalette> ")
            .skip(1)
            .map(|rest| rest.split("};").next().unwrap_or_default())
        {
            let name = block.split(':').next().unwrap_or_default();
            let set: BTreeSet<&str> = block
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|line| !line.starts_with("//") && line.contains(':'))
                .filter_map(|line| line.split(':').next())
                .collect();
            assert_eq!(set, fields, "palette `{name}` does not set every token");
            seen += 1;
        }
        assert_eq!(seen, SCHEMES.len());
    }

    /// `Theme` is the contract: each color token reads the palette field of the
    /// same name, and nothing else selects a color.
    #[test]
    fn every_theme_color_reads_the_palette_field_of_its_own_name() {
        let source = ui_markup();
        let theme =
            ui_declaration(source, "export global Theme {").expect("the markup declares the theme");
        let tokens: BTreeSet<&str> = theme
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_prefix("out property <color> "))
            .map(|line| {
                let (name, value) = line.split_once(':').expect("a token with a value");
                assert_eq!(
                    value.trim(),
                    format!("palette.{name};"),
                    "`{name}` does not read its palette field"
                );
                name
            })
            .collect();
        let fields: BTreeSet<&str> = palette_fields(source)
            .into_iter()
            .filter(|field| *field != "widget-scheme")
            .collect();
        assert_eq!(tokens, fields);
    }

    /// Components paint with tokens. A component that reads the palette, a
    /// built-in palette, or asks which scheme is active is a component that
    /// needs editing when another palette is added.
    #[test]
    fn only_the_theme_reads_a_palette() {
        for (path, source) in crate::test_support::UI_SOURCES {
            if matches!(*path, "theme.slint" | "palettes.slint") {
                continue;
            }
            // The root module re-exports the palettes for the Rust side and
            // hands the shipped widgets their scheme.
            let markup: String = source
                .lines()
                .filter(|line| !line.starts_with("export {"))
                .filter(|line| {
                    line.trim() != "property <ColorScheme> widget-scheme: Theme.widget-scheme;"
                })
                .collect::<Vec<_>>()
                .join("\n");
            for reach in [
                "BuiltInPalettes",
                "ThemePalette",
                "Theme.palette",
                "Theme.widget-scheme",
            ] {
                assert!(!markup.contains(reach), "{path} reads `{reach}`");
            }
        }
    }

    /// The WCAG contrast ratio of two opaque colors.
    fn contrast(a: Color, b: Color) -> f64 {
        fn luminance(color: Color) -> f64 {
            let linear = |channel: u8| {
                let value = f64::from(channel) / 255.0;
                if value <= 0.039_28 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.red())
                + 0.7152 * linear(color.green())
                + 0.0722 * linear(color.blue())
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn built_ins() -> Vec<(ApplicationColorScheme, ThemePalette)> {
        let ui = window();
        SCHEMES
            .into_iter()
            .map(|scheme| (scheme, built_in_palette(&ui, scheme)))
            .collect()
    }

    /// A switch that is on is its knob over its track. A palette that paints
    /// both in one color leaves a flat pill with nothing to say where it is set.
    #[test]
    fn the_knob_of_a_switch_that_is_on_stands_apart_from_its_track() {
        for (scheme, palette) in built_ins() {
            let ratio = contrast(palette.toggle_knob, palette.toggle_on);
            assert!(ratio >= 3.0, "{scheme:?}: knob over track is {ratio:.2}:1");
        }
    }

    /// The caption of a filled button is read against the fill it sits on, at
    /// rest and under the pointer.
    #[test]
    fn button_captions_are_readable_on_their_fills() {
        for (scheme, palette) in built_ins() {
            for (caption, fill, name, minimum) in [
                (palette.on_primary, palette.primary_fill, "primary", 4.0),
                (
                    palette.on_primary,
                    palette.primary_fill_hover,
                    "primary hover",
                    3.0,
                ),
                (palette.on_danger, palette.danger_fill, "danger", 4.5),
                (
                    palette.on_danger,
                    palette.danger_fill_hover,
                    "danger hover",
                    4.5,
                ),
            ] {
                let ratio = contrast(caption, fill);
                assert!(
                    ratio >= minimum,
                    "{scheme:?} {name}: {ratio:.2}:1 is under {minimum}:1"
                );
            }
        }
    }

    /// Text typed into a field, and the text a dropdown shows, sits on `field`.
    #[test]
    fn text_is_readable_on_a_field() {
        for (scheme, palette) in built_ins() {
            let ratio = contrast(palette.text, palette.field);
            assert!(ratio >= 7.0, "{scheme:?}: text on a field is {ratio:.2}:1");
            let ratio = contrast(palette.text_secondary, palette.field);
            assert!(
                ratio >= 4.5,
                "{scheme:?}: secondary text on a field is {ratio:.2}:1"
            );
        }
    }

    /// A modal puts the page behind it out of reach, so the wash has to hold
    /// back a light page as firmly as a dark one.
    #[test]
    fn every_built_in_scrim_dims_the_page_behind_a_dialog() {
        for (scheme, palette) in built_ins() {
            assert!(
                palette.scrim.alpha() >= 0x99,
                "{scheme:?}: scrim alpha {}",
                palette.scrim.alpha()
            );
        }
    }
}
