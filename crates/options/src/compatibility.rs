//! Answering "may Ephervex write this preference into this release?"
//!
//! # The one question, and its three answers
//!
//! ```text
//! canonical preference + target Minecraft release
//!     -> authoritative representation
//!     -> unsupported
//!     -> unknown
//! ```
//!
//! Only the first answer may change an `options.txt`. The other two both mean
//! "leave it alone", and they are kept apart because they are different facts:
//! unsupported is a proven absence, unknown is an unanswered question. A
//! release Mojang published this morning is unknown until somebody analyzes it,
//! and calling that unsupported would be a claim nobody made.
//!
//! # What remote data may and may not do
//!
//! The dataset decides *how* a release stores a setting Ephervex already knows
//! about. It does not decide *which* settings exist as Ephervex preferences:
//! that is [`PreferenceKey`], compiled into the build, and a canonical id the
//! build does not know is ignored. A feed can therefore extend compatibility to
//! a new Minecraft release without a new Ephervex release, and cannot invent a
//! setting the settings screen has never heard of.
//!
//! # Fallback order
//!
//! ```text
//! valid remote dataset, newer than the bundled one
//!     -> bundled baseline
//! ```
//!
//! A remote document that is invalid, truncated, from a newer format, or older
//! than what shipped is not adopted, and the build carries on with what it has.

use crate::codec::{OptionCodec, SettingValue};
use crate::dataset::{OptionsDataset, Representation};
use crate::preferences::{GlobalMinecraftPreferences, PreferenceKey};

/// Which part of a preference one options key holds.
///
/// Most preferences are one value. A window size is two keys, and writing one
/// without the other would configure half a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PreferenceComponent {
    Value,
    Width,
    Height,
}

impl PreferenceComponent {
    /// A short stable name, for developer logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Width => "width",
            Self::Height => "height",
        }
    }
}

/// The canonical dataset settings one Ephervex preference is made of.
///
/// This table is the boundary between the product's own vocabulary and the
/// research's. It lives in the binary rather than in the dataset precisely so
/// that remote data cannot re-point a preference at a different Minecraft
/// setting.
const fn dataset_settings(key: PreferenceKey) -> &'static [(PreferenceComponent, &'static str)] {
    match key {
        PreferenceKey::GuiScale => &[(PreferenceComponent::Value, "guiScale")],
        PreferenceKey::Language => &[(PreferenceComponent::Value, "languageCode")],
        PreferenceKey::Fullscreen => &[(PreferenceComponent::Value, "fullscreen")],
        PreferenceKey::WindowSize => &[
            (PreferenceComponent::Width, "overrideWidth"),
            (PreferenceComponent::Height, "overrideHeight"),
        ],
        PreferenceKey::Fov => &[(PreferenceComponent::Value, "fov")],
        PreferenceKey::RenderDistance => &[(PreferenceComponent::Value, "renderDistance")],
        PreferenceKey::SimulationDistance => &[(PreferenceComponent::Value, "simulationDistance")],
    }
}

/// One options key a preference resolves to for one release.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedField<'a> {
    pub component: PreferenceComponent,
    pub representation: &'a Representation,
}

/// What a release lets Ephervex do with one preference.
#[derive(Debug, Clone, PartialEq)]
pub enum PreferenceSupport<'a> {
    /// Proven, and therefore writable.
    Authoritative(Vec<ResolvedField<'a>>),
    /// Provably absent from this release.
    Unsupported,
    /// Not established for this release. Never written.
    Unknown,
}

impl PreferenceSupport<'_> {
    /// The fields to write, if there are any.
    #[must_use]
    pub fn fields(&self) -> &[ResolvedField<'_>] {
        match self {
            Self::Authoritative(fields) => fields,
            _ => &[],
        }
    }

    /// A short stable name, for developer logs and reports.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Authoritative(_) => "authoritative",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

/// Where the active compatibility knowledge came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilitySource {
    /// The baseline compiled into this build.
    Bundled,
    /// A validated document from the remote options feed's cache.
    Feed,
}

impl CompatibilitySource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Feed => "feed",
        }
    }
}

/// The active options compatibility knowledge.
#[derive(Debug, Clone)]
pub struct OptionsCompatibility {
    dataset: OptionsDataset,
    source: CompatibilitySource,
}

impl Default for OptionsCompatibility {
    /// The bundled baseline: what a fresh, offline installation gets.
    fn default() -> Self {
        Self::bundled()
    }
}

impl OptionsCompatibility {
    /// The baseline compiled into this build.
    #[must_use]
    pub fn bundled() -> Self {
        Self {
            dataset: OptionsDataset::bundled(),
            source: CompatibilitySource::Bundled,
        }
    }

    /// Picks between the bundled baseline and a validated remote document.
    ///
    /// The remote document is adopted only when it is strictly newer. Equal
    /// revisions mean the same content, and an older revision means the feed
    /// has been rolled back — neither is a reason to move off what shipped.
    #[must_use]
    pub fn resolve(remote: Option<OptionsDataset>) -> Self {
        let bundled = OptionsDataset::bundled();
        match remote {
            Some(dataset) if dataset.revision > bundled.revision => {
                tracing::debug!(
                    revision = dataset.revision,
                    bundled = bundled.revision,
                    "using the cached remote Minecraft options compatibility data"
                );
                Self {
                    dataset,
                    source: CompatibilitySource::Feed,
                }
            }
            Some(dataset) => {
                tracing::debug!(
                    revision = dataset.revision,
                    bundled = bundled.revision,
                    "keeping the bundled Minecraft options compatibility data; the cached feed is not newer"
                );
                Self {
                    dataset: bundled,
                    source: CompatibilitySource::Bundled,
                }
            }
            None => Self {
                dataset: bundled,
                source: CompatibilitySource::Bundled,
            },
        }
    }

    #[must_use]
    pub const fn source(&self) -> CompatibilitySource {
        self.source
    }

    #[must_use]
    pub const fn dataset(&self) -> &OptionsDataset {
        &self.dataset
    }

    /// The family id a release resolves to, for reports and logs.
    #[must_use]
    pub fn schema_id(&self, minecraft_version: &str) -> Option<&str> {
        self.dataset
            .schema_for(minecraft_version)
            .map(|schema| schema.id.as_str())
    }

    /// What this release lets Ephervex do with one preference.
    #[must_use]
    pub fn support(&self, key: PreferenceKey, minecraft_version: &str) -> PreferenceSupport<'_> {
        let Some(schema) = self.dataset.schema_for(minecraft_version) else {
            // An unmapped release is the case a launcher meets the day after a
            // Minecraft release. It stays usable; it just gets no managed
            // preferences it cannot vouch for.
            return PreferenceSupport::Unknown;
        };

        let mut fields = Vec::new();
        let mut unsupported = false;
        for (component, setting) in dataset_settings(key) {
            match self.dataset.representation(schema, setting) {
                Some(representation) => fields.push(ResolvedField {
                    component: *component,
                    representation,
                }),
                None if schema.unsupported.iter().any(|entry| entry == setting) => {
                    unsupported = true;
                }
                None => return PreferenceSupport::Unknown,
            }
        }

        if fields.len() == dataset_settings(key).len() {
            PreferenceSupport::Authoritative(fields)
        } else if unsupported {
            // A preference whose keys are only partly present cannot be
            // expressed either: half a window size is not a window size.
            PreferenceSupport::Unsupported
        } else {
            PreferenceSupport::Unknown
        }
    }

    /// Every preference this release can express, for a settings screen that
    /// wants to explain why a field is unavailable.
    #[must_use]
    pub fn expressible_keys(&self, minecraft_version: &str) -> Vec<PreferenceKey> {
        PreferenceKey::ALL
            .iter()
            .copied()
            .filter(|key| {
                matches!(
                    self.support(*key, minecraft_version),
                    PreferenceSupport::Authoritative(_)
                )
            })
            .collect()
    }
}

/// One component of a managed preference, in release-neutral form.
///
/// `None` when the preference is not managed, which is how "leave the
/// instance's own value alone" reaches every caller without a flag to forget.
#[must_use]
pub fn component_value(
    preferences: &GlobalMinecraftPreferences,
    key: PreferenceKey,
    component: PreferenceComponent,
) -> Option<SettingValue> {
    match (key, component) {
        (PreferenceKey::GuiScale, _) => preferences
            .gui_scale
            .get()
            .map(|scale| SettingValue::Integer(scale.option_number())),
        (PreferenceKey::Language, _) => preferences
            .language
            .get()
            .map(|code| SettingValue::Text(code.clone())),
        (PreferenceKey::Fullscreen, _) => preferences
            .fullscreen
            .get()
            .map(|on| SettingValue::Boolean(*on)),
        (PreferenceKey::WindowSize, PreferenceComponent::Width) => preferences
            .window_size
            .get()
            .map(|size| SettingValue::Integer(i64::from(size.width))),
        (PreferenceKey::WindowSize, PreferenceComponent::Height) => preferences
            .window_size
            .get()
            .map(|size| SettingValue::Integer(i64::from(size.height))),
        (PreferenceKey::WindowSize, PreferenceComponent::Value) => None,
        (PreferenceKey::Fov, _) => preferences
            .fov
            .get()
            .map(|degrees| SettingValue::Decimal(*degrees)),
        (PreferenceKey::RenderDistance, _) => preferences
            .render_distance
            .get()
            .map(|chunks| SettingValue::Integer(i64::from(*chunks))),
        (PreferenceKey::SimulationDistance, _) => preferences
            .simulation_distance
            .get()
            .map(|chunks| SettingValue::Integer(i64::from(*chunks))),
    }
}

/// Whether the codec a field uses needs the target release's language catalog.
#[must_use]
pub fn needs_language_catalog(fields: &[ResolvedField<'_>]) -> bool {
    fields
        .iter()
        .any(|field| field.representation.codec.needs_language_catalog())
}

/// The codec one field writes through.
#[must_use]
pub fn codec_of<'a>(field: &ResolvedField<'a>) -> &'a OptionCodec {
    &field.representation.codec
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::preferences::{GuiScalePreference, Managed, WindowSize};

    fn compatibility() -> OptionsCompatibility {
        OptionsCompatibility::bundled()
    }

    #[test]
    fn every_preference_this_build_offers_is_described_by_the_bundled_dataset() {
        // The settings screen and the dataset are two halves of one feature.
        // A preference with no canonical definition would be a field that can
        // never be applied to anything.
        let dataset = compatibility();
        for key in PreferenceKey::ALL {
            for (_, setting) in dataset_settings(*key) {
                assert!(
                    dataset.dataset().knows_setting(setting),
                    "`{key}` needs canonical setting `{setting}`"
                );
            }
        }
    }

    #[test]
    fn the_field_of_view_encoding_differs_by_era_exactly_as_the_research_proved() {
        let dataset = compatibility();
        let legacy = dataset.support(PreferenceKey::Fov, "1.18.2");
        let modern = dataset.support(PreferenceKey::Fov, "1.19");

        let legacy_codec = codec_of(&legacy.fields()[0]);
        let modern_codec = codec_of(&modern.fields()[0]);
        assert_eq!(
            legacy_codec.encode(&SettingValue::Decimal(90.0)).unwrap(),
            "0.5",
            "1.18.2 stores (degrees - 70) / 40"
        );
        assert_eq!(
            modern_codec.encode(&SettingValue::Decimal(90.0)).unwrap(),
            "90.0",
            "1.19 stores degrees"
        );
    }

    #[test]
    fn key_bindings_keep_their_two_encodings_across_the_1_13_boundary() {
        // Not an Ephervex preference, but the dataset has to carry the fact:
        // 1.12.2 stores LWJGL2 codes and 1.13 onward stores GLFW names.
        let dataset = compatibility();
        let legacy = dataset.dataset().schema_for("1.12.2").expect("mapped");
        let modern = dataset.dataset().schema_for("1.13").expect("mapped");
        assert_eq!(
            *dataset
                .dataset()
                .representation(legacy, "keybind.forward")
                .map(|entry| &entry.codec)
                .expect("proven"),
            OptionCodec::KeyCodeLwjgl2
        );
        assert_eq!(
            *dataset
                .dataset()
                .representation(modern, "keybind.forward")
                .map(|entry| &entry.codec)
                .expect("proven"),
            OptionCodec::KeyNameGlfw
        );
    }

    #[test]
    fn a_release_that_predates_a_setting_reports_unsupported_not_unknown() {
        let dataset = compatibility();
        assert_eq!(
            dataset.support(PreferenceKey::SimulationDistance, "1.12.2"),
            PreferenceSupport::Unsupported
        );
    }

    #[test]
    fn a_release_the_dataset_has_never_seen_reports_unknown_for_everything() {
        // The day-after-a-Minecraft-release case. Unknown, never assumed to
        // match the previous release, and never written.
        let dataset = compatibility();
        for key in PreferenceKey::ALL {
            assert_eq!(
                dataset.support(*key, "26.99"),
                PreferenceSupport::Unknown,
                "{key}"
            );
        }
        assert!(dataset.expressible_keys("26.99").is_empty());
        assert!(dataset.schema_id("26.99").is_none());
    }

    #[test]
    fn a_remote_document_is_adopted_only_when_it_is_newer_than_what_shipped() {
        let bundled = OptionsDataset::bundled();
        assert_eq!(
            OptionsCompatibility::resolve(None).source(),
            CompatibilitySource::Bundled
        );
        assert_eq!(
            OptionsCompatibility::resolve(Some(bundled.clone())).source(),
            CompatibilitySource::Bundled,
            "an equal revision says the same thing"
        );

        let mut newer = bundled.clone();
        newer.revision += 1;
        assert_eq!(
            OptionsCompatibility::resolve(Some(newer)).source(),
            CompatibilitySource::Feed
        );

        let mut older = bundled;
        older.revision = older.revision.saturating_sub(1);
        assert_eq!(
            OptionsCompatibility::resolve(Some(older)).source(),
            CompatibilitySource::Bundled,
            "a rolled-back feed must not downgrade a build"
        );
    }

    #[test]
    fn an_unmanaged_preference_yields_no_component_value_at_all() {
        let mut preferences = GlobalMinecraftPreferences {
            gui_scale: Managed::apply(GuiScalePreference::Fixed(2)),
            window_size: Managed::apply(WindowSize {
                width: 1920,
                height: 1080,
            }),
            ..GlobalMinecraftPreferences::default()
        };
        assert_eq!(
            component_value(
                &preferences,
                PreferenceKey::GuiScale,
                PreferenceComponent::Value
            ),
            Some(SettingValue::Integer(2))
        );
        assert_eq!(
            component_value(
                &preferences,
                PreferenceKey::WindowSize,
                PreferenceComponent::Height
            ),
            Some(SettingValue::Integer(1080))
        );

        preferences.gui_scale.enabled = false;
        assert_eq!(
            component_value(
                &preferences,
                PreferenceKey::GuiScale,
                PreferenceComponent::Value
            ),
            None
        );
    }
}
