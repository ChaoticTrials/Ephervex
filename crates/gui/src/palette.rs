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

/// The built-in schemes in the order the theme list offers them. `Dark` and
/// `Light` are accepted names that paint as Workbench, so they are not offered.
const BUILT_IN: [ApplicationColorScheme; 1] = [ApplicationColorScheme::Workbench];

/// Every color token a theme file may set, by its `Theme` name.
pub(crate) const PALETTE_COLORS: &[&str] = &[
    "background",
    "surface",
    "field",
    "elevated",
    "overlay",
    "border",
    "bar",
    "header-ribbon-1",
    "header-ribbon-2",
    "header-ribbon-3",
    "header-ribbon-4",
    "header-ribbon-5",
    "header-ribbon-6",
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
    "group-tone-1",
    "group-tone-2",
    "group-tone-3",
    "group-tone-4",
    "group-tone-5",
    "group-tone-6",
    "shadow",
    "scrim",
    "shadow-hard",
    "border-strong",
    "accent-primary",
    "on-accent",
    "state-running",
    "state-building",
    "state-attention",
    "state-stopped",
    "state-server",
    "note-fill",
    "danger-soft",
    "grid-line",
    "terminal",
    "on-terminal",
    "on-terminal-muted",
    "terminal-error",
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
        "header-ribbon-1" => &mut palette.header_ribbon_1,
        "header-ribbon-2" => &mut palette.header_ribbon_2,
        "header-ribbon-3" => &mut palette.header_ribbon_3,
        "header-ribbon-4" => &mut palette.header_ribbon_4,
        "header-ribbon-5" => &mut palette.header_ribbon_5,
        "header-ribbon-6" => &mut palette.header_ribbon_6,
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
        "group-tone-1" => &mut palette.group_tone_1,
        "group-tone-2" => &mut palette.group_tone_2,
        "group-tone-3" => &mut palette.group_tone_3,
        "group-tone-4" => &mut palette.group_tone_4,
        "group-tone-5" => &mut palette.group_tone_5,
        "group-tone-6" => &mut palette.group_tone_6,
        "shadow" => &mut palette.shadow,
        "scrim" => &mut palette.scrim,
        "shadow-hard" => &mut palette.shadow_hard,
        "border-strong" => &mut palette.border_strong,
        "accent-primary" => &mut palette.accent_primary,
        "on-accent" => &mut palette.on_accent,
        "state-running" => &mut palette.state_running,
        "state-building" => &mut palette.state_building,
        "state-attention" => &mut palette.state_attention,
        "state-stopped" => &mut palette.state_stopped,
        "state-server" => &mut palette.state_server,
        "note-fill" => &mut palette.note_fill,
        "danger-soft" => &mut palette.danger_soft,
        "grid-line" => &mut palette.grid_line,
        "terminal" => &mut palette.terminal,
        "on-terminal" => &mut palette.on_terminal,
        "on-terminal-muted" => &mut palette.on_terminal_muted,
        "terminal-error" => &mut palette.terminal_error,
        _ => return None,
    })
}

/// The built-in palette a color scheme paints in.
fn built_in_palette(ui: &AppWindow, scheme: ApplicationColorScheme) -> ThemePalette {
    let palettes = ui.global::<BuiltInPalettes>();
    match scheme {
        ApplicationColorScheme::Workbench
        | ApplicationColorScheme::Dark
        | ApplicationColorScheme::Light => palettes.get_workbench(),
    }
}

fn built_in_label(scheme: ApplicationColorScheme) -> String {
    lookup(match scheme {
        ApplicationColorScheme::Workbench
        | ApplicationColorScheme::Dark
        | ApplicationColorScheme::Light => "gui-color-scheme-workbench",
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

/// The complete palette a theme paints in. A user theme whose file is gone
/// paints in the default scheme, and the preference is left as it is.
fn resolve(ui: &AppWindow, theme: &ApplicationTheme, user_themes: &[UserTheme]) -> ThemePalette {
    match theme {
        ApplicationTheme::BuiltIn(scheme) => built_in_palette(ui, *scheme),
        ApplicationTheme::User(id) => user_themes.iter().find(|user| user.id == *id).map_or_else(
            || built_in_palette(ui, ApplicationColorScheme::default()),
            |user| user_palette(ui, user),
        ),
    }
}

/// Repaints the interface in the palette the theme resolves to.
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

    const SCHEMES: [ApplicationColorScheme; 1] = BUILT_IN;

    /// Every name a theme can record or name as its parent.
    const NAMES: [ApplicationColorScheme; 3] = [
        ApplicationColorScheme::Workbench,
        ApplicationColorScheme::Dark,
        ApplicationColorScheme::Light,
    ];

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
    }

    /// Dark and light are recorded names that stay valid, and each paints as
    /// the one built-in palette.
    #[test]
    fn the_dark_and_light_names_paint_as_workbench() {
        let ui = window();
        let workbench = ui.global::<BuiltInPalettes>().get_workbench();
        for scheme in NAMES {
            apply_theme(&ui, &ApplicationTheme::BuiltIn(scheme), &[]);
            assert_eq!(ui.global::<Theme>().get_palette(), workbench, "{scheme:?}");
        }
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

    /// A theme file leaves most tokens out, and a token added after it was
    /// written is one of them: it comes from the parent, never from nothing.
    #[test]
    fn a_user_theme_inherits_the_tokens_it_leaves_unset_from_workbench() {
        let ui = window();
        let marker = Color::from_argb_u8(255, 1, 2, 3);
        let workbench = ui.global::<BuiltInPalettes>().get_workbench();
        let themes = NAMES.map(|parent| user(parent.as_str(), parent, &[("border", marker)]));
        for parent in NAMES {
            apply_theme(
                &ui,
                &ApplicationTheme::User(parent.as_str().to_owned()),
                &themes,
            );
            let palette = ui.global::<Theme>().get_palette();
            let mut expected = workbench.clone();
            expected.border = marker;
            assert_eq!(palette, expected, "parent {parent:?}");
            assert_ne!(palette.shadow_hard.alpha(), 0);
            assert_ne!(palette.terminal.alpha(), 0);
            assert_ne!(palette.state_running.alpha(), 0);
        }
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
    fn the_theme_list_offers_workbench_then_the_users_themes() {
        let themes = [user("mine", ApplicationColorScheme::Dark, &[])];
        let choices = theme_choices(&themes);
        let offered: Vec<ApplicationTheme> =
            choices.iter().map(|choice| choice.theme.clone()).collect();
        assert_eq!(
            offered,
            [
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Workbench),
                ApplicationTheme::User("mine".to_owned()),
            ]
        );
        assert_eq!(choices[0].label, lookup("gui-color-scheme-workbench"));
        assert_eq!(choices[1].label, "MINE");
        assert_eq!(
            choice_index(&choices, &ApplicationTheme::User("mine".to_owned())),
            1
        );
        assert_eq!(
            choice_index(&choices, &ApplicationTheme::User("gone".to_owned())),
            choice_index(&choices, &ApplicationTheme::default())
        );
    }

    /// A recorded dark or light shows Workbench as the chosen entry, since that
    /// is what it paints, and the list has no entry of its own for it.
    #[test]
    fn a_recorded_dark_or_light_shows_workbench_selected() {
        let choices = theme_choices(&[]);
        for scheme in [ApplicationColorScheme::Dark, ApplicationColorScheme::Light] {
            let chosen = choice_index(&choices, &ApplicationTheme::BuiltIn(scheme));
            assert_eq!(
                choices[chosen].theme,
                ApplicationTheme::BuiltIn(ApplicationColorScheme::Workbench)
            );
        }
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

    /// Every palette Ephervex ships.
    fn built_ins() -> Vec<(String, ThemePalette)> {
        let ui = window();
        SCHEMES
            .into_iter()
            .map(|scheme| (format!("{scheme:?}"), built_in_palette(&ui, scheme)))
            .collect()
    }

    /// A switch that is on is its knob over its track. A palette that paints
    /// both in one color leaves a flat pill with nothing to say where it is set.
    #[test]
    fn the_knob_of_a_switch_that_is_on_stands_apart_from_its_track() {
        for (scheme, palette) in built_ins() {
            let ratio = contrast(palette.toggle_knob, palette.toggle_on);
            assert!(ratio >= 3.0, "{scheme}: knob over track is {ratio:.2}:1");
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
                    "{scheme} {name}: {ratio:.2}:1 is under {minimum}:1"
                );
            }
        }
    }

    /// Text typed into a field, and the text a dropdown shows, sits on `field`.
    #[test]
    fn text_is_readable_on_a_field() {
        for (scheme, palette) in built_ins() {
            let ratio = contrast(palette.text, palette.field);
            assert!(ratio >= 7.0, "{scheme}: text on a field is {ratio:.2}:1");
            let ratio = contrast(palette.text_secondary, palette.field);
            assert!(
                ratio >= 4.5,
                "{scheme}: secondary text on a field is {ratio:.2}:1"
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
                "{scheme}: scrim alpha {}",
                palette.scrim.alpha()
            );
        }
    }

    /// Every opaque fill text is read on.
    fn text_surfaces(palette: &ThemePalette) -> [(&'static str, Color); 12] {
        [
            ("background", palette.background),
            ("surface", palette.surface),
            ("field", palette.field),
            ("elevated", palette.elevated),
            ("overlay", palette.overlay),
            ("bar", palette.bar),
            ("well", palette.well),
            ("selection", palette.selection),
            ("selection-card", palette.selection_card),
            ("selection-strong", palette.selection_strong),
            ("note-fill", palette.note_fill),
            ("danger-soft", palette.danger_soft),
        ]
    }

    #[test]
    fn text_is_readable_on_every_surface() {
        for (scheme, palette) in built_ins() {
            for (surface, fill) in text_surfaces(&palette) {
                for (name, color, minimum) in [
                    ("text", palette.text, 7.0),
                    ("text-secondary", palette.text_secondary, 4.5),
                    ("text-muted", palette.text_muted, 4.5),
                ] {
                    let ratio = contrast(color, fill);
                    assert!(
                        ratio >= minimum,
                        "{scheme}: {name} on {surface} is {ratio:.2}:1, under {minimum}:1"
                    );
                }
            }
        }
    }

    /// Accent, state, and mark tones are drawn as text in pills and links.
    #[test]
    fn tones_drawn_as_text_are_readable_on_every_surface() {
        for (scheme, palette) in built_ins() {
            for (name, tone) in [
                ("accent", palette.accent),
                ("accent-secondary", palette.accent_secondary),
                ("success", palette.success),
                ("warning", palette.warning),
                ("danger", palette.danger),
                ("log-error", palette.log_error),
                ("server", palette.server),
                ("modrinth", palette.modrinth),
                ("curseforge", palette.curseforge),
                ("vanilla", palette.vanilla),
                ("forge", palette.forge),
                ("neoforge", palette.neoforge),
                ("group-tone-1", palette.group_tone_1),
                ("group-tone-2", palette.group_tone_2),
                ("group-tone-3", palette.group_tone_3),
                ("group-tone-4", palette.group_tone_4),
                ("group-tone-5", palette.group_tone_5),
                ("group-tone-6", palette.group_tone_6),
            ] {
                for (surface, fill) in text_surfaces(&palette) {
                    // A problem box carries `danger` and the text colors only.
                    if surface == "danger-soft" {
                        continue;
                    }
                    let ratio = contrast(tone, fill);
                    assert!(
                        ratio >= 4.5,
                        "{scheme}: {name} on {surface} is {ratio:.2}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn danger_text_is_readable_on_a_problem_box() {
        for (scheme, palette) in built_ins() {
            for (name, color) in [("danger", palette.danger), ("log-error", palette.log_error)] {
                let ratio = contrast(color, palette.danger_soft);
                assert!(
                    ratio >= 4.5,
                    "{scheme}: {name} on a problem box is {ratio:.2}:1"
                );
            }
        }
    }

    /// The pastel fills carry meaning and take a dark caption.
    #[test]
    fn a_caption_is_readable_on_every_accent_and_state_fill() {
        for (scheme, palette) in built_ins() {
            for (name, fill) in [
                ("accent-primary", palette.accent_primary),
                ("state-running", palette.state_running),
                ("state-building", palette.state_building),
                ("state-attention", palette.state_attention),
                ("state-stopped", palette.state_stopped),
                ("state-server", palette.state_server),
                ("primary-fill", palette.primary_fill),
                ("danger-fill", palette.danger_fill),
            ] {
                let ratio = contrast(palette.on_accent, fill);
                assert!(ratio >= 7.0, "{scheme}: caption on {name} is {ratio:.2}:1");
            }
        }
    }

    #[test]
    fn terminal_text_is_readable_on_the_terminal() {
        for (scheme, palette) in built_ins() {
            for (name, color, minimum) in [
                ("on-terminal", palette.on_terminal, 7.0),
                ("on-terminal-muted", palette.on_terminal_muted, 4.5),
                ("terminal-error", palette.terminal_error, 4.5),
                ("accent-primary", palette.accent_primary, 4.5),
                ("state-running", palette.state_running, 4.5),
                ("state-server", palette.state_server, 4.5),
            ] {
                let ratio = contrast(color, palette.terminal);
                assert!(
                    ratio >= minimum,
                    "{scheme}: {name} on the terminal is {ratio:.2}:1, under {minimum}:1"
                );
            }
        }
    }

    /// Only the header ribbon is meant to be absent. Any other token painted
    /// fully transparent is one a palette forgot.
    #[test]
    fn no_built_in_token_but_the_header_ribbon_is_transparent() {
        for (scheme, palette) in built_ins() {
            for field in palette_fields(ui_markup())
                .into_iter()
                .filter(|field| *field != "widget-scheme")
            {
                let color = *color_mut(&mut palette.clone(), field).expect("a known token");
                assert_eq!(
                    color.alpha() == 0,
                    field.starts_with("header-ribbon-"),
                    "{scheme}: `{field}` has alpha {}",
                    color.alpha()
                );
            }
        }
    }
}
