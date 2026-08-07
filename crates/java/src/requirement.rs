//! Which Java major a Minecraft version needs.
//!
//! # Ask the metadata first
//!
//! Since 1.17 Mojang publishes `javaVersion.majorVersion` in each version
//! document, and `ephervex-minecraft` already parses it. That is the answer
//! whenever it exists, because it is Mojang's own statement rather than this
//! build's opinion of it.
//!
//! # The table is for the versions that predate the field
//!
//! Older documents carry no `javaVersion` at all, so a small table fills in.
//! It is deliberately small, and it is bounded *below* rather than above: the
//! entries say "from this Minecraft version onward, this Java major", and the
//! last entry is not a ceiling. Hardcoding 21 as the newest requirement was
//! already wrong by the time this was written — 26.x asks for Java 25 — and
//! writing the table as an upper bound is how a launcher acquires an artificial
//! one.
//!
//! # Why the answer says where it came from
//!
//! [`RequirementBasis`] travels with the major so a caller can say "Mojang says
//! 21" rather than "we think 21". When metadata is unreachable and the table
//! answered instead, the user is entitled to know that is what happened

use ephervex_minecraft::{MinecraftVersionId, VersionMetadata};

/// A Java major version requirement, with its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredJava {
    pub major: u32,
    pub basis: RequirementBasis,
}

impl RequiredJava {
    /// The requirement a version document states.
    #[must_use]
    pub fn from_metadata(metadata: &VersionMetadata) -> Option<Self> {
        metadata.java_version.as_ref().map(|java| Self {
            major: java.major_version,
            basis: RequirementBasis::Metadata {
                component: java.component.clone(),
            },
        })
    }

    /// The requirement for a version id, from the table.
    #[must_use]
    pub fn from_table(id: &MinecraftVersionId) -> Self {
        Self {
            major: fallback_major(id),
            basis: RequirementBasis::FallbackTable,
        }
    }

    /// The requirement for a resolved version: the metadata's if it has one,
    /// the table's otherwise.
    #[must_use]
    pub fn resolve(metadata: &VersionMetadata) -> Self {
        Self::from_metadata(metadata)
            .unwrap_or_else(|| Self::from_table(&MinecraftVersionId::parse(&metadata.id)))
    }
}

/// Where a [`RequiredJava`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementBasis {
    /// Mojang's own `javaVersion` block. `component` is the Mojang runtime
    /// name — `jre-legacy`, `java-runtime-delta` — which is untranslated
    /// technical text.
    Metadata { component: String },
    /// [`fallback_major`], because the document declared nothing.
    FallbackTable,
}

impl RequirementBasis {
    /// A stable, untranslated identifier for logs and `--json`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Metadata { .. } => "metadata",
            Self::FallbackTable => "fallback-table",
        }
    }
}

/// The Java major each era of Minecraft needs, as `(from this version, major)`.
///
/// Ordered oldest first and read from the end, so the last entry a version is
/// at or above wins. Adding a future era is one line and no restructuring —
/// which is the property The test suite covers.
///
/// The boundaries:
///
/// * **1.12.2 – 1.16.5** — Java 8. Nothing before 1.17 carries `javaVersion`,
///   so every entry the table actually gets consulted for lives in this band.
/// * **1.17** — Java 16, the one release that asked for it.
/// * **1.18 – 1.20.4** — Java 17.
/// * **1.20.5 onward** — Java 21.
/// * **26.1 onward** — Java 25. The calendar scheme orders above `1.x` for
///   free, because `26 > 1`.
///
/// Everything from 1.17 up is redundant with the metadata in practice; it is
/// here so that a document truncated or unreachable in that range still
/// produces a defensible answer rather than Java 8.
const FALLBACK_TABLE: &[(&str, u32)] = &[
    ("1.0", 8),
    ("1.17", 16),
    ("1.18", 17),
    ("1.20.5", 21),
    ("26.1", 25),
];

/// The table's answer for a version id.
///
/// An id in a scheme this build cannot parse gets the oldest entry, because the
/// table cannot rank it and an unparseable id is far more likely to be an
/// oddity from Minecraft's past than something from its future. Callers that
/// can do better — by resolving the metadata — should.
#[must_use]
pub fn fallback_major(id: &MinecraftVersionId) -> u32 {
    let oldest = FALLBACK_TABLE.first().map_or(8, |(_, major)| *major);
    if !id.is_recognized() {
        return oldest;
    }
    FALLBACK_TABLE
        .iter()
        .rfind(|(floor, _)| id.at_least(&MinecraftVersionId::parse(floor)) == Some(true))
        .map_or(oldest, |(_, major)| *major)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn major_for(id: &str) -> u32 {
        fallback_major(&MinecraftVersionId::parse(id))
    }

    #[test]
    fn the_table_covers_every_era_the_launcher_supports() {
        for (id, expected) in [
            ("1.12.2", 8),
            ("1.14.4", 8),
            ("1.16.5", 8),
            ("1.17", 16),
            ("1.17.1", 16),
            ("1.18", 17),
            ("1.20.1", 17),
            ("1.20.4", 17),
            ("1.20.5", 21),
            ("1.21.1", 21),
            ("1.21.8", 21),
            ("26.1", 25),
            ("26.4", 25),
        ] {
            assert_eq!(major_for(id), expected, "{id}");
        }
    }

    #[test]
    fn the_newest_entry_is_a_floor_and_not_a_ceiling() {
        // A launcher that treated its newest table entry as the maximum would
        // refuse the next Minecraft era outright. `27.1` is not in the table
        // and must still resolve to the newest known requirement rather than to
        // the oldest or to nothing.
        assert_eq!(major_for("27.1"), 25);
        assert_eq!(major_for("31.0"), 25);
    }

    #[test]
    fn an_unparseable_id_falls_to_the_oldest_entry_rather_than_the_newest() {
        // `1.RV-Pre1` and friends are April Fools' releases from Minecraft's
        // past, not previews of its future.
        assert_eq!(major_for("1.RV-Pre1"), 8);
        assert_eq!(major_for("b1.7.3"), 8);
    }

    #[test]
    fn metadata_beats_the_table_and_says_so() {
        let metadata = ephervex_minecraft::VersionMetadata::parse(
            r#"{
                "id": "1.20.1",
                "type": "release",
                "mainClass": "net.minecraft.client.main.Main",
                "releaseTime": "2023-06-12T08:57:47+00:00",
                "time": "2023-06-12T08:57:47+00:00",
                "javaVersion": {"component": "java-runtime-gamma", "majorVersion": 17}
            }"#,
        )
        .expect("parses");

        let required = RequiredJava::resolve(&metadata);
        assert_eq!(required.major, 17);
        assert_eq!(
            required.basis,
            RequirementBasis::Metadata {
                component: "java-runtime-gamma".to_owned()
            }
        );
    }

    #[test]
    fn a_document_without_a_java_block_falls_back_and_says_so() {
        // Every 1.12.2-era document looks like this: the field did not exist
        // yet, which is exactly the case the table is for.
        let metadata = ephervex_minecraft::VersionMetadata::parse(
            r#"{
                "id": "1.12.2",
                "type": "release",
                "mainClass": "net.minecraft.client.main.Main",
                "releaseTime": "2017-09-18T08:39:46+00:00",
                "time": "2017-09-18T08:39:46+00:00"
            }"#,
        )
        .expect("parses");

        let required = RequiredJava::resolve(&metadata);
        assert_eq!(required.major, 8);
        assert_eq!(required.basis, RequirementBasis::FallbackTable);
    }
}
