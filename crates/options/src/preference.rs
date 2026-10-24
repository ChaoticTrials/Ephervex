//! The generic managed-preference runtime: stable ids and typed values.
//!
//! # Two vocabularies, one identity
//!
//! [`crate::catalog::MinecraftPreferenceCatalog`] says which preferences this
//! build exposes and what product shape each one has.
//! [`crate::dataset::OptionsDataset`] says how one exact Minecraft release
//! persists the canonical settings behind them. This module is the third
//! thing both need: a stable identity ([`PreferenceId`]) and a typed value
//! ([`MinecraftPreferenceValue`]) that neither the settings screen nor the
//! options writer has to spell as text.
//!
//! # Why the identity is a validated string rather than an enum
//!
//! The bundled catalog carries more than a hundred and eighty preferences and
//! grows with every reviewed Ephervex release. Mirroring it as hand-written
//! Rust variants would make the generated catalog and the compiled build two
//! sources of the same truth that can disagree. The catalog is the source; a
//! [`PreferenceId`] is only a *well-formed* id, and whether it names anything
//! is a question for the catalog.
//!
//! That split is also what lets configuration written by a newer Ephervex
//! survive an older one: an id with no descriptor is not a product preference
//! here, it is simply not this build's business, and nothing in this crate
//! deletes it.
//!
//! # Why the value is closed and typed
//!
//! Every value that reaches an `options.txt` passes through
//! [`crate::codec::OptionCodec`], which is a closed set precisely so remote
//! compatibility data cannot introduce a way of computing a stored value. A
//! loosely typed preference value would hand that property back: "the string
//! `0.5`" carries no proof that the release in question stores a decimal
//! there. So a value is one of a fixed set of shapes, a shape is checked
//! against the product descriptor before anything is written, and the release
//! codec still gets the last word on whether the exact number fits.

use std::collections::BTreeMap;
use std::fmt;

use crate::catalog::{
    MinecraftPreferenceCatalog, MinecraftPreferenceDescriptor, PreferencePresentation,
};
use crate::input::InputBinding;
use crate::preferences::{WindowSize, format_number};

/// The longest a stable preference id may be.
///
/// Generous for every generated id, and small enough that a hand-edited
/// configuration file cannot turn an open map key into a large allocation.
const MAXIMUM_ID_LENGTH: usize = 64;

/// Durable value spellings the original seven preferences accepted before the
/// generic engine existed.
///
/// Configuration written by an earlier Ephervex must resolve to exactly the
/// same value, so the one spelling that is not simply the value's own text
/// form is translated here rather than special-cased inside a shape parser.
/// `gui_scale` is an `Integer` preference whose `0` has always been spelled
/// `auto` on the command line.
const LEGACY_VALUE_ALIASES: &[(&str, &str, &str)] = &[("gui_scale", "auto", "0")];

// ------------------------------------------------------------------ identity

/// A stable product preference id, as the bundled catalog spells it.
///
/// Validated on construction so an id can be used as a map key, a log field,
/// and a fragment of an error message without any of those having to defend
/// themselves. Ordering is the id's own byte ordering, deliberately unrelated
/// to how a settings screen chooses to sort rows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreferenceId(String);

/// An id that cannot be a product preference id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{value}` is not a Minecraft preference id")]
pub struct InvalidPreferenceId {
    /// The rejected text, truncated to something safe to put in a message.
    pub value: String,
}

impl PreferenceId {
    /// Reads an id from configuration, a command line, or the catalog.
    ///
    /// Lowercase ASCII letters, digits and `_`, which is exactly the shape the
    /// generator produces. Anything else is refused rather than normalized:
    /// an id is an identity, and two spellings of one identity is how a
    /// preference ends up configured twice.
    pub fn parse(text: &str) -> Result<Self, InvalidPreferenceId> {
        let acceptable = !text.is_empty()
            && text.len() <= MAXIMUM_ID_LENGTH
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if acceptable {
            Ok(Self(text.to_owned()))
        } else {
            Err(InvalidPreferenceId {
                value: text.chars().take(MAXIMUM_ID_LENGTH).collect(),
            })
        }
    }

    /// An id this build compiled in, which is therefore known well formed.
    ///
    /// Only for constants inside this crate. Every name reaching it is also
    /// checked against [`Self::parse`] by this module's own tests, so the two
    /// construction paths cannot drift apart.
    pub(crate) fn compiled_in(name: &'static str) -> Self {
        Self(name.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PreferenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::borrow::Borrow<str> for PreferenceId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

// --------------------------------------------------------------------- value

/// A decimal that is guaranteed to be a real number.
///
/// A preference value has to survive being written into a file the game parses
/// by hand, and `NaN`, `inf` and `-inf` have no spelling there. Refusing them
/// at construction keeps every later layer from having to ask.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct FiniteDecimal(f64);

impl FiniteDecimal {
    /// The value, or `None` when it is not a real number.
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then_some(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl fmt::Display for FiniteDecimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_number(self.0))
    }
}

/// The shape one preference's value has.
///
/// Derived from the product descriptor, never from the value that happens to
/// be configured: a value of the wrong shape is a configuration error to
/// report, not a reason to reinterpret the preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferenceValueShape {
    Boolean,
    Integer,
    Decimal,
    Text,
    LanguageCode,
    Enum,
    KeyBinding,
    WindowSize,
}

impl PreferenceValueShape {
    /// A short stable name, for developer logs and error messages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Text => "text",
            Self::LanguageCode => "language_code",
            Self::Enum => "enum",
            Self::KeyBinding => "key_binding",
            Self::WindowSize => "window_size",
        }
    }
}

impl fmt::Display for PreferenceValueShape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PreferencePresentation {
    /// The value shape this presentation kind describes.
    #[must_use]
    pub const fn value_shape(&self) -> PreferenceValueShape {
        match self {
            Self::Toggle { .. } => PreferenceValueShape::Boolean,
            Self::Integer { .. } => PreferenceValueShape::Integer,
            Self::Decimal { .. } => PreferenceValueShape::Decimal,
            Self::Enum { .. } => PreferenceValueShape::Enum,
            Self::Language { .. } => PreferenceValueShape::LanguageCode,
            Self::KeyBinding { .. } => PreferenceValueShape::KeyBinding,
            Self::WindowSize => PreferenceValueShape::WindowSize,
            Self::Text { .. } => PreferenceValueShape::Text,
        }
    }
}

/// One preference's configured value, typed and release-neutral.
///
/// `Enum` carries the *technical* value the compatibility dataset proved -
/// an id or a token, as text - and which of the two it is stays the exact
/// release codec's decision. Holding one form here is what keeps a
/// configuration file readable by a build whose target release persists the
/// same enumeration the other way round.
#[derive(Debug, Clone, PartialEq)]
pub enum MinecraftPreferenceValue {
    Boolean(bool),
    Integer(i64),
    Decimal(FiniteDecimal),
    Text(String),
    /// A Minecraft language code, whose validity for a release additionally
    /// depends on that release's own language catalog.
    LanguageCode(String),
    /// One proven technical value of a closed enumeration.
    Enum(String),
    KeyBinding(InputBinding),
    /// Exactly one window size, never a width or a height on its own.
    WindowSize(WindowSize),
}

impl MinecraftPreferenceValue {
    /// A decimal value, or `None` when the number is not a real one.
    #[must_use]
    pub fn decimal(value: f64) -> Option<Self> {
        FiniteDecimal::new(value).map(Self::Decimal)
    }

    /// Which shape this value is.
    #[must_use]
    pub const fn shape(&self) -> PreferenceValueShape {
        match self {
            Self::Boolean(_) => PreferenceValueShape::Boolean,
            Self::Integer(_) => PreferenceValueShape::Integer,
            Self::Decimal(_) => PreferenceValueShape::Decimal,
            Self::Text(_) => PreferenceValueShape::Text,
            Self::LanguageCode(_) => PreferenceValueShape::LanguageCode,
            Self::Enum(_) => PreferenceValueShape::Enum,
            Self::KeyBinding(_) => PreferenceValueShape::KeyBinding,
            Self::WindowSize(_) => PreferenceValueShape::WindowSize,
        }
    }

    /// The durable text form stored in configuration.
    #[must_use]
    pub fn to_stored_text(&self) -> String {
        match self {
            Self::Boolean(on) => on.to_string(),
            Self::Integer(number) => number.to_string(),
            Self::Decimal(number) => number.to_string(),
            Self::Text(text) | Self::LanguageCode(text) | Self::Enum(text) => text.clone(),
            Self::KeyBinding(binding) => binding.to_stored_text(),
            Self::WindowSize(size) => size.to_string(),
        }
    }
}

/// Why a configured value cannot be used for a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferenceValueProblem {
    /// Not the shape this preference's product descriptor has.
    WrongShape,
    /// Not written in a form this shape can be read from.
    Unreadable,
    /// Outside the domain the product descriptor proved.
    OutOfDomain,
    /// Not one of the values this preference enumerates.
    NotEnumerated,
    /// Could not be written as one options file line.
    Unwritable,
}

impl PreferenceValueProblem {
    /// A short stable name, for developer logs and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WrongShape => "wrong-shape",
            Self::Unreadable => "unreadable",
            Self::OutOfDomain => "out-of-domain",
            Self::NotEnumerated => "not-enumerated",
            Self::Unwritable => "unwritable",
        }
    }
}

impl fmt::Display for PreferenceValueProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected preference value, named so a caller can say which one and why.
///
/// Carries the stable product id rather than any presentation label: the id is
/// what a user typed, what configuration holds, and what stays meaningful
/// across releases.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the configured value for `{id}` is not usable ({problem}, expected {expected})")]
pub struct PreferenceValueError {
    pub id: PreferenceId,
    /// The shape the product descriptor requires.
    pub expected: PreferenceValueShape,
    pub problem: PreferenceValueProblem,
}

impl PreferenceValueError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        "error-options-preference-value-unusable"
    }
}

// -------------------------------------------------- descriptor-driven values

impl MinecraftPreferenceDescriptor {
    /// The value shape this preference's configured value must have.
    #[must_use]
    pub const fn value_shape(&self) -> PreferenceValueShape {
        self.presentation.value_shape()
    }

    /// Checks a typed value against this preference's own product domain.
    ///
    /// The descriptor's domain is what the research *proved* about the
    /// canonical release, so an absent bound means no bound was established
    /// and nothing is enforced here - the exact release's codec still gets the
    /// last word before anything is written. Nothing is ever coerced: a value
    /// outside a proven domain is an error to report, never a value to move.
    pub fn validate_value(
        &self,
        value: &MinecraftPreferenceValue,
    ) -> Result<(), PreferenceValueError> {
        let id = PreferenceId::parse(&self.id).map_err(|_| PreferenceValueError {
            id: PreferenceId(self.id.clone()),
            expected: self.value_shape(),
            problem: PreferenceValueProblem::Unreadable,
        })?;
        let reject = |problem| PreferenceValueError {
            id: id.clone(),
            expected: self.value_shape(),
            problem,
        };

        if value.shape() != self.value_shape() {
            return Err(reject(PreferenceValueProblem::WrongShape));
        }

        match (&self.presentation, value) {
            (
                PreferencePresentation::Integer { min, max, .. },
                MinecraftPreferenceValue::Integer(number),
            ) => {
                let inside =
                    min.is_none_or(|low| *number >= low) && max.is_none_or(|high| *number <= high);
                if !inside {
                    return Err(reject(PreferenceValueProblem::OutOfDomain));
                }
            }
            (
                PreferencePresentation::Decimal { min, max, .. },
                MinecraftPreferenceValue::Decimal(number),
            ) => {
                let number = number.get();
                let inside =
                    min.is_none_or(|low| number >= low) && max.is_none_or(|high| number <= high);
                if !inside {
                    return Err(reject(PreferenceValueProblem::OutOfDomain));
                }
            }
            (
                PreferencePresentation::Enum { values, .. },
                MinecraftPreferenceValue::Enum(technical),
            ) => {
                if !values
                    .iter()
                    .any(|candidate| candidate.technical_value == *technical)
                {
                    return Err(reject(PreferenceValueProblem::NotEnumerated));
                }
            }
            (PreferencePresentation::Text { .. }, MinecraftPreferenceValue::Text(text)) => {
                if text.chars().any(char::is_control) {
                    return Err(reject(PreferenceValueProblem::Unwritable));
                }
            }
            (
                PreferencePresentation::Language { .. },
                MinecraftPreferenceValue::LanguageCode(code),
            ) => {
                if !is_language_code(code) {
                    return Err(reject(PreferenceValueProblem::Unwritable));
                }
            }
            (PreferencePresentation::WindowSize, MinecraftPreferenceValue::WindowSize(size))
                if size.width < WindowSize::MINIMUM_EDGE
                    || size.height < WindowSize::MINIMUM_EDGE =>
            {
                return Err(reject(PreferenceValueProblem::OutOfDomain));
            }
            _ => {}
        }
        Ok(())
    }

    /// Reads a durable configuration value for this preference.
    ///
    /// Rejects rather than repairs. A value this build cannot read is a value
    /// a user has to see, not one to replace with a neighbor and then write
    /// into every instance they create afterwards.
    pub fn parse_stored_value(
        &self,
        text: &str,
    ) -> Result<MinecraftPreferenceValue, PreferenceValueError> {
        let id = PreferenceId::parse(&self.id).map_err(|_| PreferenceValueError {
            id: PreferenceId(self.id.clone()),
            expected: self.value_shape(),
            problem: PreferenceValueProblem::Unreadable,
        })?;
        let reject = |problem| PreferenceValueError {
            id: id.clone(),
            expected: self.value_shape(),
            problem,
        };

        // Text and Enum payloads are stored verbatim, so edge whitespace is part of the value.
        let raw = match self.value_shape() {
            PreferenceValueShape::Text | PreferenceValueShape::Enum => text,
            _ => text.trim(),
        };
        let raw = LEGACY_VALUE_ALIASES
            .iter()
            .find(|(preference, spelling, _)| *preference == self.id && *spelling == raw)
            .map_or(raw, |(_, _, canonical)| *canonical);

        let value = match self.value_shape() {
            PreferenceValueShape::Boolean => match raw {
                "true" => MinecraftPreferenceValue::Boolean(true),
                "false" => MinecraftPreferenceValue::Boolean(false),
                _ => return Err(reject(PreferenceValueProblem::Unreadable)),
            },
            PreferenceValueShape::Integer => MinecraftPreferenceValue::Integer(
                raw.parse()
                    .map_err(|_| reject(PreferenceValueProblem::Unreadable))?,
            ),
            PreferenceValueShape::Decimal => {
                let number: f64 = raw
                    .parse()
                    .map_err(|_| reject(PreferenceValueProblem::Unreadable))?;
                MinecraftPreferenceValue::decimal(number)
                    .ok_or_else(|| reject(PreferenceValueProblem::Unreadable))?
            }
            PreferenceValueShape::Text => MinecraftPreferenceValue::Text(raw.to_owned()),
            PreferenceValueShape::LanguageCode => {
                MinecraftPreferenceValue::LanguageCode(raw.to_ascii_lowercase())
            }
            PreferenceValueShape::Enum => MinecraftPreferenceValue::Enum(raw.to_owned()),
            PreferenceValueShape::KeyBinding => MinecraftPreferenceValue::KeyBinding(
                InputBinding::parse_stored(raw)
                    .ok_or_else(|| reject(PreferenceValueProblem::Unreadable))?,
            ),
            PreferenceValueShape::WindowSize => {
                let (width, height) = raw
                    .split_once(['x', 'X'])
                    .ok_or_else(|| reject(PreferenceValueProblem::Unreadable))?;
                let width: u32 = width
                    .trim()
                    .parse()
                    .map_err(|_| reject(PreferenceValueProblem::Unreadable))?;
                let height: u32 = height
                    .trim()
                    .parse()
                    .map_err(|_| reject(PreferenceValueProblem::Unreadable))?;
                MinecraftPreferenceValue::WindowSize(WindowSize { width, height })
            }
        };

        self.validate_value(&value)?;
        Ok(value)
    }
}

/// Minecraft's own language-code form: lowercase letters, digits, `_` and `-`.
fn is_language_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 32
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

// ----------------------------------------------------------- the sparse model

/// One preference's configured value, and whether Ephervex applies it.
///
/// The two are separate facts. A preference Ephervex does not manage must be
/// left entirely alone in a new instance's file, which is a different outcome
/// from managing it and happening to agree with what is already there - and
/// for a toggle, `managed: false` and `Boolean(false)` are not remotely the
/// same statement.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredPreference {
    /// Apply this preference to instances created from here on.
    pub managed: bool,
    /// The value to apply. Kept even while unmanaged, so turning management
    /// back on restores what the user last chose.
    pub value: MinecraftPreferenceValue,
}

impl ConfiguredPreference {
    /// A preference Ephervex applies to new instances.
    #[must_use]
    pub const fn managed(value: MinecraftPreferenceValue) -> Self {
        Self {
            managed: true,
            value,
        }
    }

    /// A preference Ephervex leaves alone, remembering the value.
    #[must_use]
    pub const fn remembered(value: MinecraftPreferenceValue) -> Self {
        Self {
            managed: false,
            value,
        }
    }

    /// The value if it is managed, and nothing at all if it is not.
    #[must_use]
    pub const fn applied_value(&self) -> Option<&MinecraftPreferenceValue> {
        if self.managed {
            Some(&self.value)
        } else {
            None
        }
    }
}

/// Every product preference this build has a configured value for.
///
/// Sparse on purpose: an absent id means Ephervex has no opinion about that
/// preference at all, which is the only safe default for a launcher that
/// writes into files imported packs also own. Nothing here holds a catalog
/// default, and nothing here is written to configuration unless a user chose
/// it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfiguredPreferences {
    entries: BTreeMap<PreferenceId, ConfiguredPreference>,
}

impl ConfiguredPreferences {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One preference's configuration, or `None` when it has none.
    #[must_use]
    pub fn get(&self, id: &PreferenceId) -> Option<&ConfiguredPreference> {
        self.entries.get(id)
    }

    /// Records one preference's configuration.
    pub fn insert(&mut self, id: PreferenceId, configured: ConfiguredPreference) {
        self.entries.insert(id, configured);
    }

    /// Forgets one preference entirely, restoring "Ephervex has no opinion".
    pub fn remove(&mut self, id: &PreferenceId) -> Option<ConfiguredPreference> {
        self.entries.remove(id)
    }

    /// Every configured preference, managed or not, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (&PreferenceId, &ConfiguredPreference)> {
        self.entries.iter()
    }

    /// Every preference that is applied to new instances, in id order.
    pub fn managed(&self) -> impl Iterator<Item = (&PreferenceId, &MinecraftPreferenceValue)> {
        self.entries
            .iter()
            .filter_map(|(id, configured)| configured.applied_value().map(|value| (id, value)))
    }

    /// Whether any preference is applied to new instances at all.
    #[must_use]
    pub fn manages_anything(&self) -> bool {
        self.managed().next().is_some()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The result of reading configured preferences out of stored settings.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfiguredPreferences {
    pub preferences: ConfiguredPreferences,
    /// Values this build could not read, and therefore does not apply.
    pub rejected: Vec<PreferenceValueError>,
    /// How many stored ids this build has no product descriptor for.
    ///
    /// A count rather than the ids themselves: an open configuration-map key
    /// is untrusted text that can itself carry credential material, so it is
    /// not copied into a report or a log. The entries stay in the
    /// configuration file untouched; this build simply has nothing to do with
    /// them.
    pub unrecognized: usize,
}

impl ConfiguredPreferences {
    /// Builds the sparse model from stored `(id, managed, value)` settings.
    ///
    /// Takes the settings table's shape rather than its type, so this crate
    /// keeps no opinion about TOML or about where preferences are persisted.
    ///
    /// A value this build cannot read does **not** fail the load: it is
    /// reported and left out, because refusing to create an instance over one
    /// mistyped number would be a far worse outcome than not applying it.
    #[must_use]
    pub fn from_settings<'a, I>(
        catalog: &MinecraftPreferenceCatalog,
        settings: I,
    ) -> LoadedConfiguredPreferences
    where
        I: IntoIterator<Item = (&'a str, bool, &'a str)>,
    {
        let mut preferences = Self::new();
        let mut rejected = Vec::new();
        let mut unrecognized = 0_usize;

        for (name, managed, text) in settings {
            let Ok(id) = PreferenceId::parse(name) else {
                unrecognized += 1;
                continue;
            };
            let Some(descriptor) = catalog.descriptor(id.as_str()) else {
                // An id from a newer Ephervex, or one this build has retired.
                // Storage keeps it in the file; this layer has nothing to do
                // with it and must not present it as a product preference.
                unrecognized += 1;
                continue;
            };
            match descriptor.parse_stored_value(text) {
                Ok(value) => {
                    preferences.insert(id, ConfiguredPreference { managed, value });
                }
                Err(error) => {
                    tracing::warn!(
                        preference = %error.id,
                        problem = error.problem.as_str(),
                        "leaving a global Minecraft preference unmanaged; its configured value is not usable"
                    );
                    rejected.push(error);
                }
            }
        }

        LoadedConfiguredPreferences {
            preferences,
            rejected,
            unrecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::catalog::EnumValuePresentation;
    use crate::input::{MouseButton, PhysicalKey};

    fn catalog() -> &'static MinecraftPreferenceCatalog {
        MinecraftPreferenceCatalog::bundled_shared().expect("the bundled catalog is valid")
    }

    fn descriptor(id: &str) -> &'static MinecraftPreferenceDescriptor {
        catalog().descriptor(id).expect("a bundled preference")
    }

    fn id(text: &str) -> PreferenceId {
        PreferenceId::parse(text).expect("a well-formed id")
    }

    #[test]
    fn a_preference_id_is_a_validated_identity_rather_than_free_text() {
        for good in ["fov", "gui_scale", "keybind_inventory", "window_size"] {
            assert_eq!(PreferenceId::parse(good).expect("valid").as_str(), good);
        }
        for bad in ["", "Fov", "gui scale", "gui-scale", "fov!", "fov\n"] {
            assert!(PreferenceId::parse(bad).is_err(), "{bad:?}");
        }
        assert!(
            PreferenceId::parse(&"a".repeat(MAXIMUM_ID_LENGTH + 1)).is_err(),
            "an open configuration key must not be unbounded"
        );
    }

    #[test]
    fn the_seven_preferences_that_predate_the_catalog_keep_their_exact_ids() {
        for name in [
            "gui_scale",
            "language",
            "fullscreen",
            "window_size",
            "fov",
            "render_distance",
            "simulation_distance",
        ] {
            assert!(
                catalog().descriptor(name).is_some(),
                "`{name}` must stay a product preference"
            );
        }
    }

    #[test]
    fn every_id_the_catalog_ships_is_one_this_build_can_read_back() {
        // The catalog is generated, and an id it spelled in a way
        // `PreferenceId` refuses would be a preference no configuration could
        // ever name.
        for descriptor in &catalog().descriptors {
            let parsed = PreferenceId::parse(&descriptor.id)
                .unwrap_or_else(|error| panic!("{}: {error}", descriptor.id));
            assert_eq!(parsed.as_str(), descriptor.id);
        }
    }

    #[test]
    fn a_compiled_in_id_is_one_the_validating_reader_would_also_accept() {
        // The two construction paths must not drift: one is checked, and the
        // other is trusted precisely because this holds.
        for name in [
            "gui_scale",
            "language",
            "fullscreen",
            "window_size",
            "fov",
            "render_distance",
            "simulation_distance",
        ] {
            assert_eq!(
                PreferenceId::compiled_in(name),
                PreferenceId::parse(name).unwrap_or_else(|error| panic!("{name}: {error}"))
            );
        }
    }

    #[test]
    fn every_presentation_kind_has_exactly_one_value_shape() {
        let expected = [
            ("fullscreen", PreferenceValueShape::Boolean),
            ("render_distance", PreferenceValueShape::Integer),
            ("fov", PreferenceValueShape::Decimal),
            ("language", PreferenceValueShape::LanguageCode),
            ("cloud_status", PreferenceValueShape::Enum),
            ("keybind_inventory", PreferenceValueShape::KeyBinding),
            ("window_size", PreferenceValueShape::WindowSize),
        ];
        for (name, shape) in expected {
            assert_eq!(descriptor(name).value_shape(), shape, "{name}");
        }
    }

    #[test]
    fn a_value_of_the_wrong_shape_is_refused_before_anything_is_written() {
        let error = descriptor("fullscreen")
            .validate_value(&MinecraftPreferenceValue::Integer(1))
            .expect_err("a toggle does not hold a number");
        assert_eq!(error.id, id("fullscreen"));
        assert_eq!(error.problem, PreferenceValueProblem::WrongShape);
        assert_eq!(error.expected, PreferenceValueShape::Boolean);

        assert!(
            descriptor("window_size")
                .validate_value(&MinecraftPreferenceValue::Integer(1920))
                .is_err(),
            "a window size is one compound value, never a lone number"
        );
    }

    #[test]
    fn a_decimal_preference_cannot_hold_a_number_that_is_not_a_real_one() {
        for hostile in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(FiniteDecimal::new(hostile).is_none(), "{hostile}");
            assert!(
                MinecraftPreferenceValue::decimal(hostile).is_none(),
                "{hostile}"
            );
        }
        assert_eq!(
            FiniteDecimal::new(82.5).expect("finite").to_string(),
            "82.5"
        );
    }

    #[test]
    fn a_value_outside_the_proven_product_domain_is_refused_rather_than_moved() {
        // Clamping would write a setting the user never chose into every
        // instance created afterwards.
        let fov = descriptor("fov");
        assert!(
            fov.validate_value(&MinecraftPreferenceValue::decimal(90.0).expect("finite"))
                .is_ok()
        );
        let error = fov
            .validate_value(&MinecraftPreferenceValue::decimal(180.0).expect("finite"))
            .expect_err("outside the proven slider");
        assert_eq!(error.problem, PreferenceValueProblem::OutOfDomain);

        let error = descriptor("cloud_status")
            .validate_value(&MinecraftPreferenceValue::Enum("7".to_owned()))
            .expect_err("not a value this setting enumerates");
        assert_eq!(error.problem, PreferenceValueProblem::NotEnumerated);
    }

    #[test]
    fn an_unbounded_product_domain_enforces_nothing_of_its_own() {
        // `render_distance` has no proven bound in the canonical release. The
        // exact release's codec is the gate, and inventing a bound here would
        // be exactly the kind of guess this crate refuses elsewhere.
        assert!(
            descriptor("render_distance")
                .validate_value(&MinecraftPreferenceValue::Integer(999))
                .is_ok()
        );
    }

    #[test]
    fn a_window_size_is_one_typed_value_with_a_floor_on_both_edges() {
        let window = descriptor("window_size");
        assert!(
            window
                .validate_value(&MinecraftPreferenceValue::WindowSize(WindowSize {
                    width: 1920,
                    height: 1080,
                }))
                .is_ok()
        );
        for size in [
            WindowSize {
                width: 100,
                height: 1080,
            },
            WindowSize {
                width: 1920,
                height: 100,
            },
        ] {
            assert!(
                window
                    .validate_value(&MinecraftPreferenceValue::WindowSize(size))
                    .is_err(),
                "{size}"
            );
        }
    }

    #[test]
    fn a_language_code_cannot_smuggle_a_second_setting_into_the_options_file() {
        let language = descriptor("language");
        for hostile in ["en_us\nfullscreen:true", "en:us", "", "en us"] {
            assert!(
                language
                    .validate_value(&MinecraftPreferenceValue::LanguageCode(hostile.to_owned()))
                    .is_err(),
                "{hostile:?}"
            );
        }
        assert_eq!(
            language.parse_stored_value("EN_US").expect("valid"),
            MinecraftPreferenceValue::LanguageCode("en_us".to_owned()),
            "Minecraft's own codes are lowercase"
        );
    }

    #[test]
    fn every_value_shape_round_trips_through_its_durable_text_form() {
        let cases = [
            ("fullscreen", "true"),
            ("render_distance", "16"),
            ("fov", "82.5"),
            ("fov", "90"),
            ("language", "de_de"),
            ("cloud_status", "fast"),
            ("window_size", "1920x1080"),
            ("keybind_inventory", "key:left.shift"),
            ("keybind_inventory", "mouse:middle"),
            ("keybind_inventory", "unbound"),
        ];
        for (name, text) in cases {
            let value = descriptor(name)
                .parse_stored_value(text)
                .unwrap_or_else(|error| panic!("{name} {text}: {error}"));
            assert_eq!(value.to_stored_text(), text, "{name} {text}");
            assert_eq!(
                descriptor(name)
                    .parse_stored_value(&value.to_stored_text())
                    .expect("second read"),
                value,
                "{name} {text} must survive a second round trip"
            );
        }
    }

    #[test]
    fn a_text_value_keeps_its_edge_whitespace_through_a_stored_round_trip() {
        let text_preference = MinecraftPreferenceDescriptor {
            presentation: PreferencePresentation::Text { default: None },
            ..descriptor("language").clone()
        };
        for stored in [" foo ", "\u{a0}foo", "foo  ", "   "] {
            let value = text_preference
                .parse_stored_value(stored)
                .unwrap_or_else(|error| panic!("{stored:?}: {error}"));
            assert_eq!(value, MinecraftPreferenceValue::Text(stored.to_owned()));
            assert_eq!(value.to_stored_text(), stored);
        }
    }

    #[test]
    fn an_enum_value_keeps_its_edge_whitespace_through_a_stored_round_trip() {
        let enum_preference = MinecraftPreferenceDescriptor {
            presentation: PreferencePresentation::Enum {
                default: None,
                values: vec![EnumValuePresentation {
                    technical_value: " foo ".to_owned(),
                    enum_constant: None,
                    translation_key: None,
                    ordinal: None,
                }],
            },
            ..descriptor("cloud_status").clone()
        };
        let value = enum_preference
            .parse_stored_value(" foo ")
            .expect("the enumerated value, spaces included");
        assert_eq!(value, MinecraftPreferenceValue::Enum(" foo ".to_owned()));
        assert_eq!(value.to_stored_text(), " foo ");
        assert_eq!(
            enum_preference
                .parse_stored_value("foo")
                .expect_err("the trimmed spelling is a different value")
                .problem,
            PreferenceValueProblem::NotEnumerated
        );
    }

    #[test]
    fn whitespace_around_a_numeric_or_boolean_stored_value_is_still_ignored() {
        assert_eq!(
            descriptor("render_distance")
                .parse_stored_value(" 16 ")
                .expect("valid"),
            MinecraftPreferenceValue::Integer(16)
        );
        assert_eq!(
            descriptor("fullscreen")
                .parse_stored_value(" true\t")
                .expect("valid"),
            MinecraftPreferenceValue::Boolean(true)
        );
    }

    #[test]
    fn the_gui_scale_spelling_written_by_an_earlier_build_still_reads_the_same() {
        // `auto` is how the command line has always spelled `guiScale:0`.
        let gui_scale = descriptor("gui_scale");
        assert_eq!(
            gui_scale.parse_stored_value("auto").expect("valid"),
            MinecraftPreferenceValue::Integer(0)
        );
        assert_eq!(
            gui_scale.parse_stored_value("0").expect("valid"),
            MinecraftPreferenceValue::Integer(0)
        );
        assert!(
            descriptor("render_distance")
                .parse_stored_value("auto")
                .is_err(),
            "the alias belongs to one preference, not to every integer"
        );
    }

    #[test]
    fn an_unreadable_stored_value_is_reported_rather_than_repaired() {
        for (name, text) in [
            ("fullscreen", "yes"),
            ("render_distance", "sixteen"),
            ("fov", "wide"),
            ("window_size", "1920"),
            ("keybind_inventory", "W"),
        ] {
            let error = descriptor(name)
                .parse_stored_value(text)
                .expect_err("must be refused");
            assert_eq!(error.id.as_str(), name);
        }
    }

    #[test]
    fn managing_a_toggle_to_false_is_not_the_same_as_not_managing_it() {
        let off = ConfiguredPreference::managed(MinecraftPreferenceValue::Boolean(false));
        let unmanaged = ConfiguredPreference::remembered(MinecraftPreferenceValue::Boolean(false));
        assert_eq!(
            off.applied_value(),
            Some(&MinecraftPreferenceValue::Boolean(false))
        );
        assert_eq!(unmanaged.applied_value(), None);
        assert_eq!(unmanaged.value, MinecraftPreferenceValue::Boolean(false));
    }

    #[test]
    fn the_configured_set_holds_only_what_was_chosen() {
        let loaded = ConfiguredPreferences::from_settings(
            catalog(),
            [
                ("fullscreen", true, "false"),
                ("render_distance", false, "16"),
            ],
        );
        assert!(loaded.rejected.is_empty());
        assert_eq!(loaded.unrecognized, 0);
        assert_eq!(
            loaded.preferences.len(),
            2,
            "nothing else in the catalog is configured"
        );
        assert_eq!(
            loaded.preferences.managed().count(),
            1,
            "only the managed one is applied"
        );
        assert_eq!(
            loaded
                .preferences
                .get(&id("render_distance"))
                .expect("remembered")
                .value,
            MinecraftPreferenceValue::Integer(16),
            "an unmanaged preference still remembers its value"
        );
    }

    #[test]
    fn an_id_this_build_has_no_descriptor_for_is_counted_and_never_exposed() {
        // Configuration written by a newer Ephervex. Nothing here presents it
        // as a product preference, and nothing here applies it.
        let loaded = ConfiguredPreferences::from_settings(
            catalog(),
            [
                ("a_setting_from_the_future", true, "17"),
                ("Not A Valid Id", true, "17"),
                ("fullscreen", true, "true"),
            ],
        );
        assert_eq!(loaded.unrecognized, 2);
        assert_eq!(loaded.preferences.len(), 1);
        assert!(loaded.preferences.get(&id("fullscreen")).is_some());
    }

    #[test]
    fn an_unusable_stored_value_is_reported_and_left_out_rather_than_failing_the_load() {
        // A mistyped field of view must not be able to stop an instance being
        // created.
        let loaded = ConfiguredPreferences::from_settings(
            catalog(),
            [("fov", true, "very wide"), ("fullscreen", true, "true")],
        );
        assert_eq!(loaded.rejected.len(), 1);
        assert_eq!(loaded.rejected[0].id, id("fov"));
        assert_eq!(loaded.preferences.len(), 1);
        assert!(loaded.preferences.get(&id("fov")).is_none());
    }

    #[test]
    fn a_configured_set_survives_a_restart_with_the_same_ids_and_values() {
        // What is written to configuration has to read back as exactly the
        // same preferences, because a launcher restarts between choosing a
        // preference and applying it.
        let original = [
            ("fullscreen", true, "false"),
            ("fov", true, "82.5"),
            ("window_size", false, "1920x1080"),
            ("cloud_status", true, "fast"),
            ("keybind_inventory", true, "key:left.shift"),
        ];
        let loaded = ConfiguredPreferences::from_settings(catalog(), original);
        assert!(loaded.rejected.is_empty());

        let stored: Vec<(String, bool, String)> = loaded
            .preferences
            .iter()
            .map(|(id, configured)| {
                (
                    id.as_str().to_owned(),
                    configured.managed,
                    configured.value.to_stored_text(),
                )
            })
            .collect();
        let reread = ConfiguredPreferences::from_settings(
            catalog(),
            stored
                .iter()
                .map(|(id, managed, value)| (id.as_str(), *managed, value.as_str())),
        );
        assert_eq!(reread.preferences, loaded.preferences);
    }

    #[test]
    fn a_key_binding_value_keeps_its_release_neutral_model() {
        let binding = descriptor("keybind_inventory")
            .parse_stored_value("mouse:4")
            .expect("valid");
        assert_eq!(
            binding,
            MinecraftPreferenceValue::KeyBinding(InputBinding::Mouse(MouseButton::Button4))
        );
        assert_eq!(
            descriptor("keybind_inventory")
                .parse_stored_value("key:f3")
                .expect("valid"),
            MinecraftPreferenceValue::KeyBinding(InputBinding::Keyboard(PhysicalKey::F3))
        );
    }
}
