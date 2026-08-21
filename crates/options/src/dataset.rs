//! The generated options compatibility dataset.
//!
//! # What it answers
//!
//! One question, for every registered setting and every Minecraft release
//! Ephervex offers: *what exactly does this release store, and was that ever
//! proven?* Everything else in this crate is downstream of that answer.
//!
//! # Three states, and why the last two are not the same
//!
//! A release's schema names the settings whose representation was proven, and
//! separately the settings that provably did not exist yet. Anything in neither
//! list is **unknown**. Unknown and unsupported both mean "do not write it",
//! but they mean opposite things to a person reading a report: unsupported is a
//! finished answer, unknown is an open question. Collapsing them would turn "we
//! have not analyzed 26.3 yet" into "26.3 has no field of view".
//!
//! # Where it comes from
//!
//! `tools/minecraft-options/update.py` generates it from static analysis of
//! official client JARs. The same document is embedded in the binary as the
//! bundled baseline and published to the remote feed, so a build always
//! understands at least the releases that were verified when it shipped, and a
//! newer feed can extend that without a new build.
//!
//! # Deduplication
//!
//! Releases point at *schema families* identified by a digest of the family's
//! own content. Releases whose extracted representations are identical share
//! one family; a release whose extraction differs gets its own, whatever its
//! version number suggests. That is what keeps fifty-one releases' worth of two
//! hundred settings to one file rather than fifty-one copies of it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::codec::OptionCodec;

/// The dataset shape this build understands.
///
/// A document declaring a higher version is refused rather than read
/// optimistically: a format change is exactly the case where guessing at the
/// parts that still look familiar produces a wrong `options.txt`.
pub const DATASET_FORMAT_VERSION: u32 = 1;

/// The baseline compiled into the binary.
///
/// Guarantees that a fresh installation with no network and no cache still
/// understands every release that was verified when this build shipped.
const BUNDLED_DATASET: &str = include_str!("../data/minecraft-options.json");

/// One stable setting definition retained for current and historical schemas.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RegistrySetting {
    /// The stable id schemas refer to.
    pub id: String,
    /// The persisted key the canonical release uses. Individual releases may
    /// spell it differently; the schema entry carries the spelling that applies.
    pub key: String,
    #[serde(rename = "type")]
    pub value_type: String,
    pub category: Option<String>,
    pub translation_key: Option<String>,
    /// Whether the research considered the setting one a launcher may manage at
    /// all. Advisory: what Ephervex actually offers is decided by its own
    /// preference model, never by this flag.
    pub manageable: bool,
}

/// One release's storage form for one setting: where it is written and how.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Representation {
    /// The `options.txt` key, in this release's own spelling.
    pub key: String,
    pub codec: OptionCodec,
}

/// One deduplicated per-release representation of every canonical setting.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SchemaFamily {
    /// A digest of this family's own expanded content.
    pub id: String,
    /// Canonical setting id to a position in [`OptionsDataset::representations`].
    pub supported: BTreeMap<String, usize>,
    /// Canonical settings that provably did not exist in this release.
    pub unsupported: Vec<String>,
}

/// Which family one Minecraft release uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseMapping {
    pub minecraft_version: String,
    pub schema: String,
    /// The data version this release stamps into its own options file.
    ///
    /// The game reads it back to decide which of its historical fixes the rest
    /// of the file still needs, so a file without one is treated as predating
    /// every one of them. `None` where the generator could not read it, which
    /// leaves a file this build creates without the line rather than carrying
    /// a number from a different release.
    #[serde(default)]
    pub data_version: Option<i64>,
}

/// The generated compatibility dataset.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OptionsDataset {
    pub format_version: u32,
    /// Advances whenever the generator produces different content. A remote
    /// document only replaces the bundled baseline when it is *newer*, so a
    /// rolled-back feed cannot quietly downgrade a build.
    pub revision: u32,
    /// The release the canonical setting definitions were taken from.
    pub canonical_minecraft_version: String,
    /// Cumulative stable definitions required by every retained schema.
    pub setting_registry: Vec<RegistrySetting>,
    /// Settings present in [`Self::canonical_minecraft_version`]. Historical
    /// registry entries that disappeared from the current release are absent.
    pub current_canonical_setting_ids: Vec<String>,
    pub representations: Vec<Representation>,
    pub schemas: Vec<SchemaFamily>,
    pub releases: Vec<ReleaseMapping>,
}

/// Why a dataset was refused.
///
/// Every variant is a reason to keep whatever was already active. A dataset is
/// either wholly valid and becomes the answer to every compatibility question,
/// or it is refused; there is no partial adoption, because a half-read document
/// is exactly how a setting gets written through the wrong codec.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DatasetError {
    /// Not a compatibility dataset, or a codec variant this build does not
    /// implement.
    #[error("not a well-formed options compatibility dataset")]
    Malformed {
        #[source]
        source: serde_json::Error,
    },

    /// A document written for a later dataset format.
    #[error("options compatibility format {found} is newer than {DATASET_FORMAT_VERSION}")]
    UnsupportedFormat { found: u32 },

    /// Internally inconsistent: a dangling schema reference, a setting that is
    /// both supported and unsupported, a codec with unusable parameters.
    #[error("the options compatibility dataset is inconsistent: {detail}")]
    Inconsistent { detail: String },
}

impl DatasetError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        "error-options-compatibility-invalid"
    }
}

/// Just enough of a document to decide whether this build may read the rest.
#[derive(Debug, Deserialize)]
struct FormatProbe {
    format_version: u32,
}

impl OptionsDataset {
    /// The baseline compiled into this build.
    ///
    /// # Panics
    ///
    /// Never in a shipped binary: the bundled document is generated, validated
    /// by the generator, and checked by this crate's own tests, so a failure
    /// here means the build itself is broken rather than that some input was
    /// bad.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the bundled dataset is build input, and a broken one must fail loudly rather than leave the launcher with no compatibility knowledge at all"
    )]
    pub fn bundled() -> Self {
        Self::parse(BUNDLED_DATASET).expect("the bundled options compatibility dataset is valid")
    }

    /// Reads and fully validates a document.
    ///
    /// The format check happens first and on its own, so a document from a
    /// future Ephervex is refused with a reason a user can act on rather than
    /// as a wall of deserialization detail.
    pub fn parse(json: &str) -> Result<Self, DatasetError> {
        let probe: FormatProbe =
            serde_json::from_str(json).map_err(|source| DatasetError::Malformed { source })?;
        if probe.format_version > DATASET_FORMAT_VERSION {
            return Err(DatasetError::UnsupportedFormat {
                found: probe.format_version,
            });
        }
        let dataset: Self =
            serde_json::from_str(json).map_err(|source| DatasetError::Malformed { source })?;
        dataset.validate()?;
        Ok(dataset)
    }

    /// The family one release uses, or `None` when the release is not mapped.
    ///
    /// Matching is exact. A release the dataset does not name is *unknown*, not
    /// "probably like the one before it": a hotfix that changed an encoding
    /// looks exactly like one that did not until somebody analyzes it.
    #[must_use]
    pub fn schema_for(&self, minecraft_version: &str) -> Option<&SchemaFamily> {
        let mapping = self
            .releases
            .iter()
            .find(|release| release.minecraft_version == minecraft_version)?;
        self.schemas
            .iter()
            .find(|schema| schema.id == mapping.schema)
    }

    /// The data version one release stamps into its own options file, when
    /// the generator could read it.
    #[must_use]
    pub fn data_version_for(&self, minecraft_version: &str) -> Option<i64> {
        self.releases
            .iter()
            .find(|release| release.minecraft_version == minecraft_version)?
            .data_version
    }

    /// The representation one family gives one canonical setting.
    #[must_use]
    pub fn representation(&self, schema: &SchemaFamily, setting: &str) -> Option<&Representation> {
        schema
            .supported
            .get(setting)
            .and_then(|position| self.representations.get(*position))
    }

    /// Whether this dataset names a canonical setting at all.
    #[must_use]
    pub fn knows_setting(&self, setting: &str) -> bool {
        self.setting_registry
            .iter()
            .any(|entry| entry.id == setting)
    }

    /// Whether a setting is part of the newest release's canonical set.
    #[must_use]
    pub fn is_current_canonical_setting(&self, setting: &str) -> bool {
        self.current_canonical_setting_ids
            .iter()
            .any(|entry| entry == setting)
    }

    /// The releases this dataset can answer for, in its own order.
    pub fn mapped_releases(&self) -> impl ExactSizeIterator<Item = &str> {
        self.releases
            .iter()
            .map(|release| release.minecraft_version.as_str())
    }

    fn validate(&self) -> Result<(), DatasetError> {
        let inconsistent = |detail: String| DatasetError::Inconsistent { detail };

        if self.revision == 0 {
            return Err(inconsistent(
                "revision 0 is not a published revision".to_owned(),
            ));
        }

        let ids = self.validated_setting_ids()?;

        for representation in &self.representations {
            if representation.key.is_empty()
                || representation.key.chars().any(char::is_control)
                || representation.key.contains(':')
            {
                return Err(inconsistent(format!(
                    "`{}` is not usable as an options.txt key",
                    representation.key.escape_debug()
                )));
            }
            if !representation.codec.is_well_formed() {
                return Err(inconsistent(format!(
                    "the {} codec for `{}` has unusable parameters",
                    representation.codec, representation.key
                )));
            }
        }

        let mut families = BTreeSet::new();
        for schema in &self.schemas {
            if !families.insert(schema.id.as_str()) {
                return Err(inconsistent(format!(
                    "schema `{}` is declared twice",
                    schema.id
                )));
            }
            for (setting, position) in &schema.supported {
                if !ids.contains(setting.as_str()) {
                    return Err(inconsistent(format!(
                        "schema `{}` names unknown setting `{setting}`",
                        schema.id
                    )));
                }
                if *position >= self.representations.len() {
                    return Err(inconsistent(format!(
                        "schema `{}` points past the representation table",
                        schema.id
                    )));
                }
            }
            for setting in &schema.unsupported {
                if !ids.contains(setting.as_str()) {
                    return Err(inconsistent(format!(
                        "schema `{}` names unknown unsupported setting `{setting}`",
                        schema.id
                    )));
                }
                if schema.supported.contains_key(setting) {
                    return Err(inconsistent(format!(
                        "schema `{}` calls `{setting}` both supported and unsupported",
                        schema.id
                    )));
                }
            }
        }

        let mut versions = BTreeSet::new();
        for release in &self.releases {
            if !versions.insert(release.minecraft_version.as_str()) {
                return Err(inconsistent(format!(
                    "release `{}` is mapped twice",
                    release.minecraft_version
                )));
            }
            if !families.contains(release.schema.as_str()) {
                return Err(inconsistent(format!(
                    "release `{}` names unknown schema `{}`",
                    release.minecraft_version, release.schema
                )));
            }
        }
        if !versions.contains(self.canonical_minecraft_version.as_str()) {
            return Err(inconsistent(
                "the canonical release is not itself mapped".to_owned(),
            ));
        }
        Ok(())
    }

    fn validated_setting_ids(&self) -> Result<BTreeSet<&str>, DatasetError> {
        let ids: BTreeSet<&str> = self
            .setting_registry
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        if ids.len() != self.setting_registry.len() {
            return Err(DatasetError::Inconsistent {
                detail: "two setting registry entries share one id".to_owned(),
            });
        }

        let current: BTreeSet<&str> = self
            .current_canonical_setting_ids
            .iter()
            .map(String::as_str)
            .collect();
        if current.len() != self.current_canonical_setting_ids.len() {
            return Err(DatasetError::Inconsistent {
                detail: "a current canonical setting is declared twice".to_owned(),
            });
        }
        if let Some(unknown) = current.iter().find(|setting| !ids.contains(**setting)) {
            return Err(DatasetError::Inconsistent {
                detail: format!(
                    "current canonical setting `{unknown}` is absent from the registry"
                ),
            });
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A minimal but structurally complete document, so a test can break one
    /// thing at a time.
    fn document(body: &str) -> String {
        format!(
            r#"{{
              "format_version": 1,
              "revision": 3,
              "canonical_minecraft_version": "26.2",
              "setting_registry": [{{"id":"fov","key":"fov","type":"integer","category":"Video","translation_key":"options.fov","manageable":true}}],
              "current_canonical_setting_ids": ["fov"],
              "representations": [{{"key":"fov","codec":{{"kind":"decimal","min":30,"max":110}}}}],
              "schemas": [{{"id":"aaaaaaaaaaaaaaaa","supported":{{"fov":0}},"unsupported":[]}}],
              "releases": [{{"minecraft_version":"26.2","schema":"aaaaaaaaaaaaaaaa"}}]
              {body}
            }}"#
        )
    }

    #[test]
    fn the_bundled_baseline_is_valid_and_covers_the_supported_range() {
        let bundled = OptionsDataset::bundled();
        assert_eq!(bundled.format_version, DATASET_FORMAT_VERSION);
        assert!(bundled.revision >= 1);
        assert!(
            bundled.schema_for("1.12.2").is_some(),
            "the oldest supported release must be mapped"
        );
        assert!(
            bundled
                .schema_for(&bundled.canonical_minecraft_version)
                .is_some()
        );
        assert!(
            bundled.schemas.len() < bundled.releases.len(),
            "families exist so releases with one conclusion share one entry"
        );
    }

    #[test]
    fn a_release_the_dataset_does_not_name_is_not_matched_to_a_neighbor() {
        let bundled = OptionsDataset::bundled();
        assert!(bundled.schema_for("26.2.1").is_none());
        assert!(bundled.schema_for("99.9").is_none());
    }

    #[test]
    fn a_document_from_a_newer_format_is_refused_cleanly() {
        let error = OptionsDataset::parse(
            &document("").replace("\"format_version\": 1", "\"format_version\": 7"),
        )
        .expect_err("a future format must not be read optimistically");
        assert!(matches!(
            error,
            DatasetError::UnsupportedFormat { found: 7 }
        ));
    }

    #[test]
    fn a_document_naming_a_codec_this_build_cannot_implement_is_refused() {
        let hostile = document("").replace(
            r#"{"kind":"decimal","min":30,"max":110}"#,
            r#"{"kind":"eval","expression":"value * 2"}"#,
        );
        assert!(matches!(
            OptionsDataset::parse(&hostile).expect_err("an unknown codec must fail the document"),
            DatasetError::Malformed { .. }
        ));
    }

    #[test]
    fn an_internally_inconsistent_document_is_refused_rather_than_partly_used() {
        let cases = [
            (
                "a dangling schema reference",
                document("").replace(
                    "\"schema\":\"aaaaaaaaaaaaaaaa\"",
                    "\"schema\":\"bbbbbbbbbbbbbbbb\"",
                ),
            ),
            (
                "a representation index past the table",
                document("").replace("\"fov\":0", "\"fov\":9"),
            ),
            (
                "a setting that is both supported and unsupported",
                document("").replace("\"unsupported\":[]", "\"unsupported\":[\"fov\"]"),
            ),
            (
                "an unknown setting id",
                document("").replace("\"fov\":0", "\"guiScale\":0"),
            ),
            (
                "an unmapped canonical release",
                document("").replace(
                    "\"canonical_minecraft_version\": \"26.2\"",
                    "\"canonical_minecraft_version\": \"27.0\"",
                ),
            ),
            (
                "a codec with an unusable scale",
                document("").replace(
                    r#"{"kind":"decimal","min":30,"max":110}"#,
                    r#"{"kind":"scaled_decimal","offset":70,"scale":0,"min":null,"max":null}"#,
                ),
            ),
            (
                "an options key that would break the line format",
                document("").replace("\"key\":\"fov\"", "\"key\":\"fov:x\""),
            ),
        ];
        for (why, json) in cases {
            assert!(
                matches!(
                    OptionsDataset::parse(&json),
                    Err(DatasetError::Inconsistent { .. })
                ),
                "{why} must be refused"
            );
        }
    }

    #[test]
    fn a_valid_document_round_trips_into_the_lookups_downstream_needs() {
        let dataset = OptionsDataset::parse(&document("")).expect("valid");
        let schema = dataset.schema_for("26.2").expect("mapped");
        let representation = dataset.representation(schema, "fov").expect("supported");
        assert_eq!(representation.key, "fov");
        assert!(dataset.knows_setting("fov"));
        assert!(!dataset.knows_setting("guiScale"));
        assert!(dataset.is_current_canonical_setting("fov"));
        assert_eq!(dataset.mapped_releases().collect::<Vec<_>>(), ["26.2"]);
    }

    #[test]
    fn a_historical_registry_setting_is_not_implicitly_current() {
        let historical = document("")
            .replace(
                r#"{"id":"fov","key":"fov","type":"integer","category":"Video","translation_key":"options.fov","manageable":true}"#,
                r#"{"id":"fov","key":"fov","type":"integer","category":"Video","translation_key":"options.fov","manageable":true},{"id":"touchscreen","key":"touchscreen","type":"boolean","category":"Controls","translation_key":"options.touchscreen","manageable":true}"#,
            );
        let dataset = OptionsDataset::parse(&historical).expect("valid historical registry entry");
        assert!(dataset.knows_setting("touchscreen"));
        assert!(!dataset.is_current_canonical_setting("touchscreen"));
    }
}
