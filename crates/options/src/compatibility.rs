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

use std::collections::BTreeSet;

use crate::catalog::{FieldRole, MinecraftPreferenceDescriptor};
use crate::codec::{OptionCodec, SettingValue};
use crate::dataset::{OptionsDataset, Representation};
use crate::preferences::PreferenceKey;

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
    /// The component one product descriptor field holds.
    ///
    /// The two vocabularies are deliberately the same distinction under two
    /// names: the generated catalog spells it [`FieldRole`], and the
    /// compatibility layer has spelled it this way since the seven-preference
    /// engine.
    #[must_use]
    pub const fn of_field_role(role: FieldRole) -> Self {
        match role {
            FieldRole::Value => Self::Value,
            FieldRole::Width => Self::Width,
            FieldRole::Height => Self::Height,
        }
    }

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

/// What a release lets Ephervex do with one preference, without borrowing the
/// dataset that proved it.
///
/// The same three answers as [`PreferenceSupport`], for a report or a settings
/// screen that outlives the compatibility document's borrow. Unknown stays a
/// separate answer here too: collapsing it into unsupported would turn "nobody
/// has analyzed this release yet" into "this release does not have the
/// setting".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseSupport {
    Authoritative,
    Unsupported,
    Unknown,
}

impl ReleaseSupport {
    /// A short stable name, for developer logs and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authoritative => "authoritative",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

impl PreferenceSupport<'_> {
    /// The same answer, without the dataset borrow.
    #[must_use]
    pub const fn release_support(&self) -> ReleaseSupport {
        match self {
            Self::Authoritative(_) => ReleaseSupport::Authoritative,
            Self::Unsupported => ReleaseSupport::Unsupported,
            Self::Unknown => ReleaseSupport::Unknown,
        }
    }

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

/// What keeps one release's startup preset from replacing settings Ephervex
/// writes, resolved to the exact text and keys of that release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetGuard {
    /// The options key that holds the preset.
    pub key: String,
    /// The stored text under which the preset replaces nothing.
    pub neutral_value: String,
    /// The options keys the preset replaces at startup.
    pub controlled_keys: BTreeSet<String>,
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
    /// has been rolled back - neither is a reason to move off what shipped.
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

    /// The data version this release stamps into its own options file, when
    /// the compatibility data knows it.
    #[must_use]
    pub fn data_version(&self, minecraft_version: &str) -> Option<i64> {
        self.dataset.data_version_for(minecraft_version)
    }

    /// The startup preset this release applies, resolved for writing.
    ///
    /// `None` when the release has no such preset, or when the dataset does not
    /// prove a neutral value this build can write for it.
    #[must_use]
    pub fn preset_guard(&self, minecraft_version: &str) -> Option<PresetGuard> {
        let schema = self.dataset.schema_for(minecraft_version)?;
        let preset = schema.startup_preset.as_ref()?;
        let holder = self.dataset.representation(schema, &preset.setting)?;
        let neutral_value = holder
            .codec
            .encode(&SettingValue::Text(preset.neutral.clone()))
            .ok()?;
        let controlled_keys = preset
            .controls
            .iter()
            .filter_map(|setting| self.dataset.representation(schema, setting))
            .map(|representation| representation.key.clone())
            .collect();
        Some(PresetGuard {
            key: holder.key.clone(),
            neutral_value,
            controlled_keys,
        })
    }

    /// What this release lets Ephervex do with one of the seven preferences
    /// that predate the generated product catalog.
    #[must_use]
    pub fn support(&self, key: PreferenceKey, minecraft_version: &str) -> PreferenceSupport<'_> {
        self.resolve_components(minecraft_version, dataset_settings(key).iter().copied())
    }

    /// What this release lets Ephervex do with one product preference.
    ///
    /// The generic form of [`Self::support`], and the one a settings screen
    /// asks. The descriptor's own fields decide which canonical settings are
    /// involved, so remote compatibility data can extend a shipped preference
    /// to a new release without ever being able to re-point it at a different
    /// Minecraft setting.
    #[must_use]
    pub fn descriptor_support(
        &self,
        descriptor: &MinecraftPreferenceDescriptor,
        minecraft_version: &str,
    ) -> PreferenceSupport<'_> {
        self.resolve_components(
            minecraft_version,
            descriptor.fields.iter().map(|field| {
                (
                    PreferenceComponent::of_field_role(field.role),
                    field.canonical_id.as_str(),
                )
            }),
        )
    }

    /// Resolves one preference's canonical settings against one exact release.
    ///
    /// Exact matching only, and a compound is authoritative only when *every*
    /// component is: half a window size is not a window size, and a release
    /// nobody has analyzed is unknown rather than assumed to match the one
    /// before it.
    fn resolve_components<'a, I>(
        &self,
        minecraft_version: &str,
        settings: I,
    ) -> PreferenceSupport<'_>
    where
        I: IntoIterator<Item = (PreferenceComponent, &'a str)>,
    {
        let Some(schema) = self.dataset.schema_for(minecraft_version) else {
            // An unmapped release is the case a launcher meets the day after a
            // Minecraft release. It stays usable; it just gets no managed
            // preferences it cannot vouch for.
            return PreferenceSupport::Unknown;
        };

        let mut fields = Vec::new();
        let mut expected = 0_usize;
        let mut unsupported = false;
        for (component, setting) in settings {
            expected += 1;
            match self.dataset.representation(schema, setting) {
                Some(representation) => fields.push(ResolvedField {
                    component,
                    representation,
                }),
                None if schema.unsupported.iter().any(|entry| entry == setting) => {
                    unsupported = true;
                }
                None => return PreferenceSupport::Unknown,
            }
        }

        if expected > 0 && fields.len() == expected {
            PreferenceSupport::Authoritative(fields)
        } else if unsupported {
            // A preference whose keys are only partly present cannot be
            // expressed either: half a window size is not a window size.
            PreferenceSupport::Unsupported
        } else {
            PreferenceSupport::Unknown
        }
    }

    /// What one exact release lets Ephervex do with every shipped product
    /// preference.
    ///
    /// The whole answer a settings screen needs to explain why a row is inert,
    /// resolved once per release rather than per row.
    #[must_use]
    pub fn catalog_release_support(
        &self,
        catalog: &crate::catalog::MinecraftPreferenceCatalog,
        minecraft_version: &str,
    ) -> std::collections::BTreeMap<String, ReleaseSupport> {
        catalog
            .descriptors
            .iter()
            .map(|descriptor| {
                (
                    descriptor.id.clone(),
                    self.descriptor_support(descriptor, minecraft_version)
                        .release_support(),
                )
            })
            .collect()
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
    use crate::codec::SettingValue;

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
    fn the_field_of_view_is_stored_scaled_in_every_era_the_dataset_covers() {
        // The file has never held degrees. Every release stores
        // `(degrees - 70) / 40`, which is why the game's own files read
        // `fov:0.0` at the default of 70 rather than `fov:70.0`. Releases from
        // 1.19 reach that arithmetic through a codec rather than a field
        // transform, which is a difference in where it is written down and not
        // in what lands in the file.
        let dataset = compatibility();
        for version in ["1.12.2", "1.16.5", "1.18.2", "1.19", "1.20.1", "26.2"] {
            let support = dataset.support(PreferenceKey::Fov, version);
            let codec = codec_of(&support.fields()[0]);
            assert_eq!(
                codec.encode(&SettingValue::Decimal(90.0)).unwrap(),
                "0.5",
                "{version} stores (degrees - 70) / 40"
            );
            assert_eq!(
                codec.encode(&SettingValue::Decimal(70.0)).unwrap(),
                "0.0",
                "{version} stores its own default as zero"
            );
        }
    }

    #[test]
    fn the_cloud_status_is_written_as_the_token_each_release_stores() {
        // Until 1.20.2 the game stores `true`, `fast` or `false` unquoted; a
        // boolean codec would drop `fast` and refuse the others.
        let catalog = crate::catalog::MinecraftPreferenceCatalog::bundled_shared().unwrap();
        let descriptor = catalog.descriptor("cloud_status").unwrap();
        let dataset = compatibility();
        for version in ["1.12.2", "1.13.2", "1.14", "1.16.5"] {
            let support = dataset.descriptor_support(descriptor, version);
            let PreferenceSupport::Authoritative(fields) = &support else {
                panic!("{version} proves how the cloud status is stored");
            };
            let codec = codec_of(&fields[0]);
            for token in ["true", "fast", "false"] {
                assert_eq!(
                    codec.encode(&SettingValue::Text(token.to_owned())).unwrap(),
                    token,
                    "{version} writes `{token}` raw"
                );
            }
            assert!(
                codec.encode(&SettingValue::Boolean(true)).is_err(),
                "{version} stores more than a boolean"
            );
        }
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
}
