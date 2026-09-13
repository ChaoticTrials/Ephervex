//! The seven-preference facade that predates the generated product catalog.
//!
//! # What this module is now
//!
//! [`crate::preference`] is the engine: stable ids, typed values, and a sparse
//! configured set covering every preference the bundled catalog exposes. This
//! module is the vocabulary the configuration file and the command line have
//! used since before that engine existed - `gui_scale`, `fov`, `render_distance`
//! and four more - kept working unchanged, resolving through exactly the same
//! engine. The seven ids here are the catalog's own ids for the same
//! preferences, so one configuration entry means one thing to both.
//!
//! # Why this is a model rather than a stored `options.txt`
//!
//! The obvious implementation of "apply my settings to every new instance" is
//! to keep one master `options.txt` and copy it. Ephervex does not allow that, and the
//! reason is version skew: a file copied out of a 1.12.2 instance carries that
//! release's `version:` marker, its key set, and its value encodings into a
//! 26.x instance, which is how an options file gets corrupted rather than
//! configured.
//!
//! So preferences are held as *typed values with names of their own*, and
//! turning one into a line of a particular release's `options.txt` is the
//! compatibility layer's job ([`crate::compatibility`] and [`crate::codec`]).
//!
//! # Why every preference carries an `enabled` flag
//!
//! [`Managed`] expresses whether Ephervex applies a global preference to new
//! instances. A preference the launcher does not manage must be
//! left *entirely alone* in the instance's file - which is a different outcome
//! from managing it and happening to agree with what is already there. Without
//! the flag, a pack that ships `renderDistance:32` for a reason would be
//! silently overwritten by a default nobody chose.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::catalog::MinecraftPreferenceCatalog;
use crate::preference::{
    ConfiguredPreference, ConfiguredPreferences, LoadedConfiguredPreferences,
    MinecraftPreferenceValue, PreferenceId,
};

/// One global preference, and whether Ephervex applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managed<T> {
    /// Apply this global preference to new instances.
    pub enabled: bool,
    /// The value to apply. Meaningful only when `enabled`, but always present
    /// so that turning management back on restores what the user last chose.
    pub value: T,
}

impl<T> Managed<T> {
    /// A preference Ephervex applies to new instances.
    pub const fn apply(value: T) -> Self {
        Self {
            enabled: true,
            value,
        }
    }

    /// A preference Ephervex leaves alone, remembering the value.
    pub const fn ignore(value: T) -> Self {
        Self {
            enabled: false,
            value,
        }
    }

    /// The value if it is managed, and nothing at all if it is not.
    ///
    /// Every adapter goes through this rather than reading `value` directly, so
    /// "not managed" cannot be forgotten at a call site.
    pub const fn get(&self) -> Option<&T> {
        if self.enabled {
            Some(&self.value)
        } else {
            None
        }
    }
}

/// Minecraft's GUI scale setting.
///
/// `Auto` is `guiScale:0` in every supported release: the game picks a scale
/// from the window size. A fixed scale larger than the current window allows is
/// clamped by the game at runtime, so this type does not second-guess it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiScalePreference {
    Auto,
    Fixed(u32),
}

impl GuiScalePreference {
    /// The largest fixed scale a settings screen offers. Values above this are
    /// refused when a preference is set, not silently clamped.
    pub const MAXIMUM_FIXED: u32 = 4;

    /// The number Minecraft stores for this scale.
    ///
    /// A number rather than a string: how it is written is the target
    /// release's business, and this type has no way to know which release it is
    /// destined for.
    #[must_use]
    pub const fn option_number(self) -> i64 {
        match self {
            Self::Auto => 0,
            Self::Fixed(scale) => scale as i64,
        }
    }
}

impl fmt::Display for GuiScalePreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Fixed(scale) => write!(f, "{scale}"),
        }
    }
}

/// A game window size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSize {
    pub width: u32,
    pub height: u32,
}

impl WindowSize {
    /// Minecraft's own default window, and the floor a size is checked against.
    pub const DEFAULT: Self = Self {
        width: 854,
        height: 480,
    };
    /// Below this a window cannot usefully hold the game's own menus.
    pub const MINIMUM_EDGE: u32 = 320;
}

impl fmt::Display for WindowSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// The preferences Ephervex knows how to manage.
///
/// The names are the keys used in `config.toml`'s `[minecraft_preferences]`
/// table, and they are *normalized* names rather than Minecraft's own: the
/// same preference is `overrideWidth`/`overrideHeight` in one release and not
/// expressible at all in another, which is exactly the difference this layer
/// exists to hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreferenceKey {
    GuiScale,
    Language,
    Fullscreen,
    WindowSize,
    Fov,
    RenderDistance,
    SimulationDistance,
}

impl PreferenceKey {
    /// Every managed preference, in the order a settings screen lists them.
    pub const ALL: &'static [Self] = &[
        Self::GuiScale,
        Self::Language,
        Self::Fullscreen,
        Self::WindowSize,
        Self::Fov,
        Self::RenderDistance,
        Self::SimulationDistance,
    ];

    /// The stable name used in configuration and on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GuiScale => "gui_scale",
            Self::Language => "language",
            Self::Fullscreen => "fullscreen",
            Self::WindowSize => "window_size",
            Self::Fov => "fov",
            Self::RenderDistance => "render_distance",
            Self::SimulationDistance => "simulation_distance",
        }
    }

    /// The stable product preference id this key names.
    ///
    /// The seven keys that predate the generated product catalog use exactly
    /// the ids the catalog gives them, so one configuration entry means the
    /// same preference to both the facade and the generic engine.
    #[must_use]
    pub fn id(self) -> PreferenceId {
        PreferenceId::compiled_in(self.as_str())
    }

    /// Parses a configuration or command-line name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|key| key.as_str() == name.trim())
    }

    /// A short description of the accepted values, for an error message or a
    /// `--help` line. Untranslated: these are literal input formats.
    #[must_use]
    pub const fn value_syntax(self) -> &'static str {
        match self {
            Self::GuiScale => "auto, or 1 to 4",
            Self::Language => "a Minecraft language code such as en_us",
            Self::Fullscreen => "true or false",
            Self::WindowSize => "WIDTHxHEIGHT, such as 1920x1080",
            Self::Fov => "field of view in degrees, 30 to 110",
            Self::RenderDistance => "chunks, 2 to 32",
            Self::SimulationDistance => "chunks, 5 to 32",
        }
    }
}

impl fmt::Display for PreferenceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected preference value, named so a caller can say which one and why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{value}` is not a valid {key} ({syntax})")]
pub struct InvalidPreferenceValue {
    pub key: PreferenceKey,
    pub value: String,
    /// The accepted syntax, from [`PreferenceKey::value_syntax`].
    pub syntax: &'static str,
}

impl InvalidPreferenceValue {
    fn new(key: PreferenceKey, value: &str) -> Self {
        Self {
            key,
            value: value.to_owned(),
            syntax: key.value_syntax(),
        }
    }

    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        "error-options-preference-value-invalid"
    }
}

/// One preference's value, typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "key", content = "value")]
pub enum PreferenceValue {
    GuiScale(GuiScalePreference),
    Language(String),
    Fullscreen(bool),
    WindowSize(WindowSize),
    /// Field of view in **degrees**, as the settings screen shows it. The
    /// release-specific encoding is the adapters' business.
    Fov(f64),
    RenderDistance(u32),
    SimulationDistance(u32),
}

impl PreferenceValue {
    /// Which preference this is a value for.
    #[must_use]
    pub const fn key(&self) -> PreferenceKey {
        match self {
            Self::GuiScale(_) => PreferenceKey::GuiScale,
            Self::Language(_) => PreferenceKey::Language,
            Self::Fullscreen(_) => PreferenceKey::Fullscreen,
            Self::WindowSize(_) => PreferenceKey::WindowSize,
            Self::Fov(_) => PreferenceKey::Fov,
            Self::RenderDistance(_) => PreferenceKey::RenderDistance,
            Self::SimulationDistance(_) => PreferenceKey::SimulationDistance,
        }
    }

    /// Parses a value as written in `config.toml` or on the command line.
    ///
    /// Rejects rather than clamps. A value the user typed wrongly must be
    /// visible as a mistake, not silently turned into a neighboring one that
    /// then gets written into every instance they create.
    pub fn parse(key: PreferenceKey, text: &str) -> Result<Self, InvalidPreferenceValue> {
        let raw = text.trim();
        let invalid = || InvalidPreferenceValue::new(key, text);

        match key {
            PreferenceKey::GuiScale => {
                if raw.eq_ignore_ascii_case("auto") || raw == "0" {
                    return Ok(Self::GuiScale(GuiScalePreference::Auto));
                }
                let scale: u32 = raw.parse().map_err(|_| invalid())?;
                if scale == 0 || scale > GuiScalePreference::MAXIMUM_FIXED {
                    return Err(invalid());
                }
                Ok(Self::GuiScale(GuiScalePreference::Fixed(scale)))
            }
            PreferenceKey::Language => {
                // Minecraft's own code form: lowercase letters, digits, and
                // separators. The character filter is also what keeps a colon
                // or a line break out of the options file.
                let acceptable = !raw.is_empty()
                    && raw.len() <= 32
                    && raw
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
                if !acceptable {
                    return Err(invalid());
                }
                Ok(Self::Language(raw.to_ascii_lowercase()))
            }
            PreferenceKey::Fullscreen => match raw {
                "true" => Ok(Self::Fullscreen(true)),
                "false" => Ok(Self::Fullscreen(false)),
                _ => Err(invalid()),
            },
            PreferenceKey::WindowSize => {
                let (width, height) = raw.split_once(['x', 'X']).ok_or_else(invalid)?;
                let width: u32 = width.trim().parse().map_err(|_| invalid())?;
                let height: u32 = height.trim().parse().map_err(|_| invalid())?;
                if width < WindowSize::MINIMUM_EDGE || height < WindowSize::MINIMUM_EDGE {
                    return Err(invalid());
                }
                Ok(Self::WindowSize(WindowSize { width, height }))
            }
            PreferenceKey::Fov => {
                let degrees: f64 = raw.parse().map_err(|_| invalid())?;
                if !(MINIMUM_FOV_DEGREES..=MAXIMUM_FOV_DEGREES).contains(&degrees) {
                    return Err(invalid());
                }
                Ok(Self::Fov(degrees))
            }
            PreferenceKey::RenderDistance => {
                let chunks: u32 = raw.parse().map_err(|_| invalid())?;
                if !(2..=32).contains(&chunks) {
                    return Err(invalid());
                }
                Ok(Self::RenderDistance(chunks))
            }
            PreferenceKey::SimulationDistance => {
                let chunks: u32 = raw.parse().map_err(|_| invalid())?;
                if !(5..=32).contains(&chunks) {
                    return Err(invalid());
                }
                Ok(Self::SimulationDistance(chunks))
            }
        }
    }

    /// The same value in the generic engine's typed form.
    ///
    /// `None` only for a field of view built out of a number that is not a
    /// real one, which the parser cannot produce.
    #[must_use]
    pub fn to_preference_value(&self) -> Option<MinecraftPreferenceValue> {
        Some(match self {
            Self::GuiScale(scale) => MinecraftPreferenceValue::Integer(scale.option_number()),
            Self::Language(code) => MinecraftPreferenceValue::LanguageCode(code.clone()),
            Self::Fullscreen(on) => MinecraftPreferenceValue::Boolean(*on),
            Self::WindowSize(size) => MinecraftPreferenceValue::WindowSize(*size),
            Self::Fov(degrees) => MinecraftPreferenceValue::decimal(*degrees)?,
            Self::RenderDistance(chunks) | Self::SimulationDistance(chunks) => {
                MinecraftPreferenceValue::Integer(i64::from(*chunks))
            }
        })
    }

    /// The canonical text form, as stored in `config.toml`.
    #[must_use]
    pub fn to_setting_value(&self) -> String {
        match self {
            Self::GuiScale(scale) => scale.to_string(),
            Self::Language(code) => code.clone(),
            Self::Fullscreen(on) => on.to_string(),
            Self::WindowSize(size) => size.to_string(),
            Self::Fov(degrees) => format_number(*degrees),
            Self::RenderDistance(chunks) | Self::SimulationDistance(chunks) => chunks.to_string(),
        }
    }
}

/// The lowest field of view Minecraft's own slider offers.
pub const MINIMUM_FOV_DEGREES: f64 = 30.0;
/// The highest field of view Minecraft's own slider offers ("Quake Pro").
pub const MAXIMUM_FOV_DEGREES: f64 = 110.0;

/// Formats a number the way an options file spells one: no exponent, and a
/// whole number that is conceptually fractional keeps its `.0`.
pub(crate) fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// The complete normalized preference model.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalMinecraftPreferences {
    pub gui_scale: Managed<GuiScalePreference>,
    pub language: Managed<String>,
    pub fullscreen: Managed<bool>,
    pub window_size: Managed<WindowSize>,
    /// Field of view in degrees.
    pub fov: Managed<f64>,
    pub render_distance: Managed<u32>,
    pub simulation_distance: Managed<u32>,
}

impl Default for GlobalMinecraftPreferences {
    /// Nothing managed, and every value at Minecraft's own default.
    ///
    /// "Manage nothing" is the only safe default: a launcher that rewrites a
    /// setting the user never asked it to touch is a launcher that corrupts
    /// imported packs. Which preferences ship *enabled* is a
    /// configuration decision, made in `config.toml`.
    fn default() -> Self {
        Self {
            gui_scale: Managed::ignore(GuiScalePreference::Auto),
            language: Managed::ignore("en_us".to_owned()),
            fullscreen: Managed::ignore(false),
            window_size: Managed::ignore(WindowSize::DEFAULT),
            fov: Managed::ignore(70.0),
            render_distance: Managed::ignore(12),
            simulation_distance: Managed::ignore(12),
        }
    }
}

impl GlobalMinecraftPreferences {
    /// Whether any preference is managed at all.
    #[must_use]
    pub fn manages_anything(&self) -> bool {
        !self.managed_keys().is_empty()
    }

    /// The preferences that are managed, in [`PreferenceKey::ALL`] order.
    #[must_use]
    pub fn managed_keys(&self) -> Vec<PreferenceKey> {
        PreferenceKey::ALL
            .iter()
            .copied()
            .filter(|key| self.is_managed(*key))
            .collect()
    }

    /// Whether one preference is applied to new instances.
    #[must_use]
    pub const fn is_managed(&self, key: PreferenceKey) -> bool {
        match key {
            PreferenceKey::GuiScale => self.gui_scale.enabled,
            PreferenceKey::Language => self.language.enabled,
            PreferenceKey::Fullscreen => self.fullscreen.enabled,
            PreferenceKey::WindowSize => self.window_size.enabled,
            PreferenceKey::Fov => self.fov.enabled,
            PreferenceKey::RenderDistance => self.render_distance.enabled,
            PreferenceKey::SimulationDistance => self.simulation_distance.enabled,
        }
    }

    /// One preference's value, whether or not it is managed.
    #[must_use]
    pub fn value(&self, key: PreferenceKey) -> PreferenceValue {
        match key {
            PreferenceKey::GuiScale => PreferenceValue::GuiScale(self.gui_scale.value),
            PreferenceKey::Language => PreferenceValue::Language(self.language.value.clone()),
            PreferenceKey::Fullscreen => PreferenceValue::Fullscreen(self.fullscreen.value),
            PreferenceKey::WindowSize => PreferenceValue::WindowSize(self.window_size.value),
            PreferenceKey::Fov => PreferenceValue::Fov(self.fov.value),
            PreferenceKey::RenderDistance => {
                PreferenceValue::RenderDistance(self.render_distance.value)
            }
            PreferenceKey::SimulationDistance => {
                PreferenceValue::SimulationDistance(self.simulation_distance.value)
            }
        }
    }

    /// Replaces one preference's value, leaving its managed flag alone.
    pub fn set_value(&mut self, value: PreferenceValue) {
        match value {
            PreferenceValue::GuiScale(scale) => self.gui_scale.value = scale,
            PreferenceValue::Language(code) => self.language.value = code,
            PreferenceValue::Fullscreen(on) => self.fullscreen.value = on,
            PreferenceValue::WindowSize(size) => self.window_size.value = size,
            PreferenceValue::Fov(degrees) => self.fov.value = degrees,
            PreferenceValue::RenderDistance(chunks) => self.render_distance.value = chunks,
            PreferenceValue::SimulationDistance(chunks) => self.simulation_distance.value = chunks,
        }
    }

    /// Turns management of one preference on or off.
    pub const fn set_managed(&mut self, key: PreferenceKey, enabled: bool) {
        match key {
            PreferenceKey::GuiScale => self.gui_scale.enabled = enabled,
            PreferenceKey::Language => self.language.enabled = enabled,
            PreferenceKey::Fullscreen => self.fullscreen.enabled = enabled,
            PreferenceKey::WindowSize => self.window_size.enabled = enabled,
            PreferenceKey::Fov => self.fov.enabled = enabled,
            PreferenceKey::RenderDistance => self.render_distance.enabled = enabled,
            PreferenceKey::SimulationDistance => self.simulation_distance.enabled = enabled,
        }
    }

    /// Builds the model from stored `(name, enabled, value)` settings.
    ///
    /// Takes the settings table's shape rather than the type, so this crate
    /// keeps no opinion about TOML or about where preferences are persisted -
    /// the same split that keeps `ephervex-java` free of `JavaConfig`.
    ///
    /// A value this build cannot parse does **not** fail the load: it is
    /// reported and left unmanaged, because refusing to create an instance over
    /// a mistyped FOV would be a far worse outcome than not applying it.
    #[must_use]
    pub fn from_settings<'a, I>(settings: I) -> LoadedPreferences
    where
        I: IntoIterator<Item = (&'a str, bool, &'a str)>,
    {
        let mut preferences = Self::default();
        let mut rejected = Vec::new();

        for (name, enabled, value) in settings {
            let Some(key) = PreferenceKey::parse(name) else {
                // A key from a newer build. Storage keeps it in the file; this
                // layer simply has nothing to do with it.
                // An open configuration-map key is untrusted and can itself
                // contain credential material, so do not copy it into logs.
                tracing::debug!("ignoring an unknown managed preference");
                continue;
            };
            match PreferenceValue::parse(key, value) {
                Ok(parsed) => {
                    preferences.set_value(parsed);
                    preferences.set_managed(key, enabled);
                }
                Err(error) => {
                    tracing::warn!(
                        preference = %key,
                        %error,
                        "leaving a global Minecraft preference unmanaged; its value is not usable"
                    );
                    rejected.push(error);
                }
            }
        }

        LoadedPreferences {
            preferences,
            rejected,
        }
    }

    /// The same preferences in the generic engine's typed, sparse form.
    ///
    /// Only preferences the catalog still describes come through, so the
    /// facade cannot ask the engine to write a setting this build no longer
    /// exposes. A value the descriptor refuses is reported rather than
    /// repaired, exactly as a configured one is.
    #[must_use]
    pub fn to_configured(
        &self,
        catalog: &MinecraftPreferenceCatalog,
    ) -> LoadedConfiguredPreferences {
        let mut preferences = ConfiguredPreferences::new();
        let mut rejected = Vec::new();
        let mut unrecognized = 0_usize;

        for key in PreferenceKey::ALL {
            let id = key.id();
            let Some(descriptor) = catalog.descriptor(id.as_str()) else {
                unrecognized += 1;
                continue;
            };
            // The `None` case is only reachable for a field of view a caller
            // built out of a number that is not a real one.
            let Some(value) = self.value(*key).to_preference_value() else {
                unrecognized += 1;
                continue;
            };
            if let Err(error) = descriptor.validate_value(&value) {
                rejected.push(error);
                continue;
            }
            preferences.insert(
                id,
                ConfiguredPreference {
                    managed: self.is_managed(*key),
                    value,
                },
            );
        }

        LoadedConfiguredPreferences {
            preferences,
            rejected,
            unrecognized,
        }
    }

    /// The settings to persist, as `(name, enabled, value)`.
    #[must_use]
    pub fn to_settings(&self) -> Vec<(&'static str, bool, String)> {
        PreferenceKey::ALL
            .iter()
            .map(|key| {
                (
                    key.as_str(),
                    self.is_managed(*key),
                    self.value(*key).to_setting_value(),
                )
            })
            .collect()
    }
}

/// The result of reading preferences out of stored settings.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPreferences {
    pub preferences: GlobalMinecraftPreferences,
    /// Values that could not be understood and are therefore not managed.
    pub rejected: Vec<InvalidPreferenceValue>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn parse(key: PreferenceKey, text: &str) -> Result<PreferenceValue, InvalidPreferenceValue> {
        PreferenceValue::parse(key, text)
    }

    #[test]
    fn an_unmanaged_preference_yields_nothing_even_though_it_has_a_value() {
        // not managing a preference is not the same as managing it to
        // its current value. Everything downstream keys off `get()`.
        let managed = Managed::apply(GuiScalePreference::Fixed(2));
        let unmanaged = Managed::ignore(GuiScalePreference::Fixed(2));
        assert_eq!(managed.get(), Some(&GuiScalePreference::Fixed(2)));
        assert_eq!(unmanaged.get(), None);
        assert_eq!(unmanaged.value, GuiScalePreference::Fixed(2));
    }

    #[test]
    fn nothing_is_managed_by_default() {
        let preferences = GlobalMinecraftPreferences::default();
        assert!(!preferences.manages_anything());
        assert!(preferences.managed_keys().is_empty());
    }

    #[test]
    fn every_key_round_trips_through_its_stored_text_form() {
        let cases = [
            (PreferenceKey::GuiScale, "auto"),
            (PreferenceKey::GuiScale, "3"),
            (PreferenceKey::Language, "de_de"),
            (PreferenceKey::Fullscreen, "true"),
            (PreferenceKey::WindowSize, "1920x1080"),
            (PreferenceKey::Fov, "90"),
            (PreferenceKey::Fov, "82.5"),
            (PreferenceKey::RenderDistance, "16"),
            (PreferenceKey::SimulationDistance, "12"),
        ];
        for (key, text) in cases {
            let value = parse(key, text).expect("valid");
            assert_eq!(value.key(), key);
            assert_eq!(value.to_setting_value(), text, "{key} {text}");
            assert_eq!(
                parse(key, &value.to_setting_value()).expect("valid"),
                value,
                "{key} must survive a second round trip"
            );
        }
    }

    #[test]
    fn a_bad_value_is_rejected_rather_than_clamped_into_a_neighboring_one() {
        // Clamping would write a setting the user never chose into every
        // instance they create afterwards.
        for (key, text) in [
            (PreferenceKey::GuiScale, "5"),
            (PreferenceKey::GuiScale, "large"),
            (PreferenceKey::Fullscreen, "yes"),
            (PreferenceKey::WindowSize, "1920"),
            (PreferenceKey::WindowSize, "100x100"),
            (PreferenceKey::Fov, "180"),
            (PreferenceKey::Fov, "wide"),
            (PreferenceKey::RenderDistance, "0"),
            (PreferenceKey::RenderDistance, "64"),
            (PreferenceKey::SimulationDistance, "1"),
        ] {
            let error = parse(key, text).expect_err("must be refused");
            assert_eq!(error.key, key);
            assert_eq!(error.value, text);
        }
    }

    #[test]
    fn a_language_code_cannot_smuggle_a_second_setting_into_the_options_file() {
        // A newline would turn one assignment into two, and a colon would end
        // the key early.
        for text in ["en_us\nfullscreen:true", "en:us", "", "  ", "en us"] {
            assert!(parse(PreferenceKey::Language, text).is_err(), "{text:?}");
        }
        assert_eq!(
            parse(PreferenceKey::Language, "EN_US").expect("valid"),
            PreferenceValue::Language("en_us".to_owned()),
            "Minecraft's own codes are lowercase"
        );
    }

    #[test]
    fn gui_scale_auto_is_expressible_as_both_words_and_zero() {
        assert_eq!(
            parse(PreferenceKey::GuiScale, "AUTO").expect("valid"),
            PreferenceValue::GuiScale(GuiScalePreference::Auto)
        );
        assert_eq!(
            parse(PreferenceKey::GuiScale, "0").expect("valid"),
            PreferenceValue::GuiScale(GuiScalePreference::Auto)
        );
        assert_eq!(GuiScalePreference::Auto.option_number(), 0);
        assert_eq!(GuiScalePreference::Fixed(2).option_number(), 2);
    }

    #[test]
    fn settings_load_into_the_model_and_come_back_out_unchanged() {
        let loaded = GlobalMinecraftPreferences::from_settings([
            ("gui_scale", true, "2"),
            ("language", true, "en_us"),
            ("render_distance", false, "16"),
        ]);
        assert!(loaded.rejected.is_empty());

        let preferences = &loaded.preferences;
        assert_eq!(
            preferences.managed_keys(),
            vec![PreferenceKey::GuiScale, PreferenceKey::Language]
        );
        assert_eq!(
            preferences.render_distance,
            Managed::ignore(16),
            "a disabled preference still remembers its value"
        );

        let round_tripped = GlobalMinecraftPreferences::from_settings(
            preferences
                .to_settings()
                .iter()
                .map(|(key, enabled, value)| (*key, *enabled, value.as_str()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(&round_tripped.preferences, preferences);
    }

    #[test]
    fn an_unusable_stored_value_is_reported_and_left_unmanaged() {
        // A mistyped FOV must not be able to stop an instance being created.
        let loaded = GlobalMinecraftPreferences::from_settings([
            ("fov", true, "very wide"),
            ("gui_scale", true, "2"),
            ("a_key_from_the_future", true, "17"),
        ]);

        assert_eq!(loaded.rejected.len(), 1);
        assert_eq!(loaded.rejected[0].key, PreferenceKey::Fov);
        assert!(!loaded.preferences.fov.enabled);
        assert_eq!(
            loaded.preferences.managed_keys(),
            vec![PreferenceKey::GuiScale]
        );
    }

    #[test]
    fn preference_names_are_stable_and_parse_back() {
        for key in PreferenceKey::ALL {
            assert_eq!(PreferenceKey::parse(key.as_str()), Some(*key));
        }
        assert_eq!(
            PreferenceKey::parse(" gui_scale "),
            Some(PreferenceKey::GuiScale)
        );
        assert_eq!(PreferenceKey::parse("guiScale"), None);
    }

    #[test]
    fn numbers_are_formatted_the_way_an_options_file_spells_them() {
        assert_eq!(format_number(0.0), "0");
        assert_eq!(format_number(-1.0), "-1");
        assert_eq!(format_number(0.5), "0.5");
    }
}
