//! A Mojang per-version JSON, parsed.
//!
//! This is the document the whole vanilla pipeline is built on: it names the
//! client jar, the asset index, every library and its rules, the launch
//! arguments, the main class, the log4j configuration, and the Java version the
//! release expects.
//!
//! # Optional where a loader JSON would omit it
//!
//! Forge and NeoForge publish version JSONs in the *same* format, but as
//! *children*: they carry `inheritsFrom`, their own libraries and arguments,
//! and nothing else — no `assets`, no `downloads`, no `javaVersion`. Fields a
//! child may legitimately omit are therefore `Option` here, and the ones a
//! vanilla launch genuinely requires are reached through accessors that fail
//! with a typed error naming the missing field
//! ([`VersionMetadata::client_jar`], [`VersionMetadata::asset_index`]).
//!
//! A raw document with `inherits_from()` set is parseable but not launchable.
//! [`VersionMetadata::inherit`] validates the named parent, merges arguments,
//! and applies the version-excluding Maven conflict key before the common
//! installation pipeline receives it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDateTime, Utc};
use ephervex_model::{Hash, HashAlgorithm};
use serde::{Deserialize, Deserializer};
use tracing::warn;

use crate::arguments::{Arguments, RawArgument};
use crate::library::{Artifact, Library, LibraryError, RawArtifact, RawLibrary};
use crate::version_id::MinecraftVersionId;

/// The release channel Mojang assigns a version.
///
/// Release candidates and pre-releases are **not** separate values: Mojang
/// types both as `snapshot`, which is why filtering on `Release` alone is
/// sufficient and no extra exclusion rules exist.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub enum ReleaseType {
    #[serde(rename = "release")]
    Release,
    #[serde(rename = "snapshot")]
    Snapshot,
    #[serde(rename = "old_beta")]
    OldBeta,
    #[serde(rename = "old_alpha")]
    OldAlpha,
    /// A channel this build has not been taught. Never treated as a release.
    #[serde(untagged)]
    Other(String),
}

impl ReleaseType {
    /// Whether this is the one channel Ephervex offers.
    #[must_use]
    pub const fn is_release(&self) -> bool {
        matches!(self, Self::Release)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Release => "release",
            Self::Snapshot => "snapshot",
            Self::OldBeta => "old_beta",
            Self::OldAlpha => "old_alpha",
            Self::Other(value) => value,
        }
    }
}

impl std::fmt::Display for ReleaseType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// The Java runtime a release expects.
///
/// Parsed, recorded, and deliberately not acted on: choosing or downloading a
/// runtime is Java runtime support's job. `component` is Mojang's own runtime name
/// (`jre-legacy`, `java-runtime-gamma`, `java-runtime-delta`,
/// `java-runtime-epsilon`), which Java runtime support uses to fetch a *managed* runtime;
/// `major_version` is what a discovered system JDK is matched against.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaRequirement {
    pub component: String,
    pub major_version: u32,
}

/// The asset index a version uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetIndexRef {
    /// The index's own id, e.g. `1.12`, `17`, `30`. Several versions share one.
    pub id: String,
    pub artifact: Artifact,
    /// Combined size of every object the index names, for progress reporting
    /// before a single object has been fetched.
    pub total_size: Option<u64>,
}

/// The log4j configuration a release wants passed to the JVM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggingConfig {
    /// The JVM argument template, e.g. `-Dlog4j.configurationFile=${path}`.
    pub argument: String,
    /// Mojang's own id for the file, e.g. `client-1.12.xml`.
    pub id: String,
    /// Usually `log4j2-xml`.
    pub kind: String,
    pub file: Artifact,
}

/// Why a version document could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VersionMetadataError {
    /// A field a vanilla launch requires is absent. On a loader child document
    /// this is expected and means the parent has to be merged in first — which
    /// loader support does.
    #[error("version `{id}` declares no `{field}`")]
    Incomplete { id: String, field: &'static str },
    /// A library entry could not be turned into something addressable.
    #[error("version `{id}` has an unusable library")]
    Library {
        id: String,
        #[source]
        source: LibraryError,
    },
    /// A digest in the document is not a well-formed SHA-1.
    #[error("version `{id}` declares a malformed sha1 for `{field}`")]
    MalformedDigest { id: String, field: &'static str },
    /// A loader child extends a different Minecraft release than the one it
    /// was being installed for.
    #[error("loader version `{child}` inherits `{declared}`, not `{expected}`")]
    WrongParent {
        child: String,
        declared: String,
        expected: String,
    },
}

/// One version's complete metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMetadata {
    /// The id as published, e.g. `1.12.2` for a vanilla document and
    /// `neoforge-26.2.0.59` for a loader child.
    pub id: String,
    /// The canonical Minecraft release the document is for, parsed for
    /// ordering and the support floor.
    ///
    /// For a vanilla document that is [`Self::id`] itself. For a Forge or
    /// NeoForge child it is `inheritsFrom`, because a loader document's id is
    /// the loader build's identity and not a Minecraft version:
    /// `neoforge-26.2.0.59` names NeoForge build 59, not a fourth Minecraft
    /// component, and `1.20.1-forge-47.4.10` is not a version string at all.
    /// [`MinecraftVersionId`] answers questions about Minecraft releases, so
    /// routing a loader id through it would be reading a loader build number
    /// as a Minecraft patch level.
    pub minecraft_version: MinecraftVersionId,
    pub release_type: ReleaseType,
    /// When Mojang released it. The manifest's ordering authority
    pub release_time: DateTime<Utc>,
    /// When the document was last modified. Not a release date, and not usable
    /// as one: Mojang rewrites old documents.
    pub time: DateTime<Utc>,
    pub main_class: String,
    pub minimum_launcher_version: u32,
    /// `1` means Mojang considers the version's log4j configuration patched.
    pub compliance_level: u32,
    pub java_version: Option<JavaRequirement>,
    /// The asset index id, e.g. `1.12`. Duplicated in `asset_index.id`; kept
    /// because a child document may name it without carrying the index itself.
    pub assets: Option<String>,
    pub asset_index: Option<AssetIndexRef>,
    /// `client`, `server`, and — on 1.20/1.21 but not on 26.x —
    /// `client_mappings` and `server_mappings`.
    ///
    /// Left as a map rather than a struct because the set of keys is not
    /// stable across the supported range, and a struct would have made the
    /// mappings' disappearance in 26.x look like a parse bug.
    pub downloads: BTreeMap<String, Artifact>,
    pub libraries: Vec<Library>,
    pub arguments: Arguments,
    pub logging: Option<LoggingConfig>,
    /// The parent this document extends, for loader JSONs. Always `None` for
    /// vanilla and for metadata returned by [`Self::inherit`].
    pub inherits_from: Option<String>,
}

impl VersionMetadata {
    /// Parses a version JSON document.
    pub fn parse(json: &str) -> Result<Self, VersionParseError> {
        let raw: RawVersion = serde_json::from_str(json)?;
        Ok(raw.into_metadata()?)
    }

    /// The client jar.
    pub fn client_jar(&self) -> Result<&Artifact, VersionMetadataError> {
        self.download("client")
    }

    /// The dedicated-server jar, where the version ships one.
    pub fn server_jar(&self) -> Result<&Artifact, VersionMetadataError> {
        self.download("server")
    }

    /// One entry of `downloads` by key.
    pub fn download(&self, key: &'static str) -> Result<&Artifact, VersionMetadataError> {
        self.downloads
            .get(key)
            .ok_or_else(|| VersionMetadataError::Incomplete {
                id: self.id.clone(),
                field: key,
            })
    }

    /// The asset index.
    pub fn asset_index(&self) -> Result<&AssetIndexRef, VersionMetadataError> {
        self.asset_index
            .as_ref()
            .ok_or_else(|| VersionMetadataError::Incomplete {
                id: self.id.clone(),
                field: "assetIndex",
            })
    }

    /// The Java major version this release expects, if it declares one.
    ///
    /// Minecraft metadata support records it and stops there; matching it against an installed
    /// runtime is Java runtime support.
    #[must_use]
    pub fn java_major_version(&self) -> Option<u32> {
        self.java_version
            .as_ref()
            .map(|requirement| requirement.major_version)
    }

    /// The parent document this one extends, if any.
    ///
    /// A `Some` here means the document is **not** launchable on its own and
    /// still needs to be resolved with [`Self::inherit`].
    #[must_use]
    pub fn inherits_from(&self) -> Option<&str> {
        self.inherits_from.as_deref()
    }

    /// Whether every field a vanilla launch needs is present.
    #[must_use]
    pub fn is_complete_vanilla(&self) -> bool {
        self.inherits_from.is_none()
            && self.asset_index.is_some()
            && self.downloads.contains_key("client")
            && !self.main_class.is_empty()
    }

    /// Resolves a Forge/NeoForge child document onto its exact vanilla parent.
    ///
    /// A child library shadows all parent entries with the same
    /// group/artifact/classifier/extension key. The Maven version is excluded
    /// deliberately, while duplicate rule-gated entries inside either source
    /// document retain their original order.
    pub fn inherit(parent: &Self, child: &Self) -> Result<Self, VersionMetadataError> {
        let declared = child.inherits_from.clone().unwrap_or_default();
        if declared != parent.id {
            return Err(VersionMetadataError::WrongParent {
                child: child.id.clone(),
                declared,
                expected: parent.id.clone(),
            });
        }

        let child_keys: BTreeSet<_> = child
            .libraries
            .iter()
            .map(|library| library.coordinate.conflict_key())
            .collect();
        let mut libraries = child.libraries.clone();
        libraries.extend(
            parent
                .libraries
                .iter()
                .filter(|library| !child_keys.contains(&library.coordinate.conflict_key()))
                .cloned(),
        );

        let mut downloads = parent.downloads.clone();
        downloads.extend(child.downloads.clone());

        Ok(Self {
            id: child.id.clone(),
            // The merged document keeps the child's loader id but *is* the
            // parent's Minecraft release, and `inherits_from` is cleared below
            // — so the canonical version has to be carried here rather than
            // recovered from either field afterwards.
            minecraft_version: parent.minecraft_version.clone(),
            release_type: child.release_type.clone(),
            release_time: child.release_time,
            time: child.time,
            main_class: child.main_class.clone(),
            minimum_launcher_version: child.minimum_launcher_version,
            compliance_level: child.compliance_level,
            java_version: child
                .java_version
                .clone()
                .or_else(|| parent.java_version.clone()),
            assets: child.assets.clone().or_else(|| parent.assets.clone()),
            asset_index: child
                .asset_index
                .clone()
                .or_else(|| parent.asset_index.clone()),
            downloads,
            libraries,
            arguments: Arguments::inherit(&parent.arguments, &child.arguments),
            logging: child.logging.clone().or_else(|| parent.logging.clone()),
            inherits_from: None,
        })
    }
}

/// Why a version document could not be read at all.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VersionParseError {
    /// Not the document shape at all.
    #[error("not a well-formed version document")]
    Json(#[from] serde_json::Error),
    /// Well-formed JSON, but something inside it is unusable.
    #[error(transparent)]
    Content(#[from] VersionMetadataError),
}

// ------------------------------------------------------------------ the wire

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVersion {
    id: String,
    #[serde(rename = "type")]
    release_type: ReleaseType,
    #[serde(deserialize_with = "deserialize_published_time")]
    release_time: DateTime<Utc>,
    #[serde(deserialize_with = "deserialize_published_time")]
    time: DateTime<Utc>,
    main_class: String,
    #[serde(default)]
    minimum_launcher_version: u32,
    #[serde(default)]
    compliance_level: u32,
    #[serde(default)]
    java_version: Option<JavaRequirement>,
    #[serde(default)]
    assets: Option<String>,
    #[serde(default)]
    asset_index: Option<RawAssetIndex>,
    #[serde(default)]
    downloads: BTreeMap<String, RawArtifact>,
    #[serde(default)]
    libraries: Vec<RawLibrary>,
    /// 1.13 onward.
    #[serde(default)]
    arguments: Option<BTreeMap<String, Vec<RawArgument>>>,
    /// 1.12.2 and earlier.
    #[serde(default)]
    minecraft_arguments: Option<String>,
    #[serde(default)]
    logging: Option<RawLogging>,
    #[serde(default)]
    inherits_from: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAssetIndex {
    id: String,
    url: String,
    sha1: String,
    size: u64,
    #[serde(default)]
    total_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawLogging {
    #[serde(default)]
    client: Option<RawLoggingEntry>,
}

#[derive(Debug, Deserialize)]
struct RawLoggingEntry {
    argument: String,
    #[serde(rename = "type")]
    kind: String,
    file: RawLoggingFile,
}

#[derive(Debug, Deserialize)]
struct RawLoggingFile {
    id: String,
    url: String,
    sha1: String,
    size: u64,
}

/// Mojang and Forge use RFC 3339 offsets. Current NeoForge metadata omits the
/// offset while publishing an otherwise ISO-8601 UTC timestamp with
/// nanoseconds; the repository timestamps are UTC, so that spelling is
/// accepted explicitly rather than rejected or interpreted in local time.
fn deserialize_published_time<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(&value) {
        return Ok(timestamp.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(&value, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|timestamp| timestamp.and_utc())
        .map_err(serde::de::Error::custom)
}

impl RawVersion {
    fn into_metadata(self) -> Result<VersionMetadata, VersionMetadataError> {
        let id = self.id;
        let malformed = |field: &'static str| VersionMetadataError::MalformedDigest {
            id: id.clone(),
            field,
        };

        let mut downloads = BTreeMap::new();
        for (key, raw) in self.downloads {
            // The key is Mojang's, not a path, and the destination comes from
            // the digest — so an unrecognized key such as a future
            // `*_mappings` is carried rather than dropped.
            let artifact =
                raw.into_artifact()
                    .ok_or_else(|| VersionMetadataError::MalformedDigest {
                        id: id.clone(),
                        field: "downloads",
                    })?;
            downloads.insert(key, artifact);
        }

        let asset_index = match self.asset_index {
            Some(raw) => Some(AssetIndexRef {
                artifact: Artifact {
                    sha1: Hash::new(HashAlgorithm::Sha1, &raw.sha1)
                        .ok_or_else(|| malformed("assetIndex"))?,
                    url: raw.url,
                    size: raw.size,
                    path: None,
                },
                id: raw.id,
                total_size: raw.total_size,
            }),
            None => None,
        };

        let logging = match self.logging.and_then(|logging| logging.client) {
            Some(entry) => Some(LoggingConfig {
                argument: entry.argument,
                kind: entry.kind,
                id: entry.file.id,
                file: Artifact {
                    sha1: Hash::new(HashAlgorithm::Sha1, &entry.file.sha1)
                        .ok_or_else(|| malformed("logging"))?,
                    url: entry.file.url,
                    size: entry.file.size,
                    path: None,
                },
            }),
            None => None,
        };

        let libraries = self
            .libraries
            .into_iter()
            .map(RawLibrary::into_library)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| VersionMetadataError::Library {
                id: id.clone(),
                source,
            })?;

        // A document carrying both forms has never been published. If one ever
        // is, the modern form wins because it is the richer one — and the fact
        // is worth a log line rather than a silent choice.
        let arguments = match (self.arguments, self.minecraft_arguments) {
            (Some(modern), legacy) => {
                if legacy.is_some() {
                    warn!(
                        version = %id,
                        "version declares both argument forms; using the modern one"
                    );
                }
                Arguments::from_modern(modern)
            }
            (None, Some(legacy)) => Arguments::from_legacy(&legacy),
            (None, None) => Arguments::from_modern(BTreeMap::new()),
        };

        for group in arguments.unrecognized_groups() {
            warn!(version = %id, %group, "version declares an argument group this build does not know");
        }

        // A loader child names the release it extends; its own id is the
        // loader build's identity. Parsing that as a Minecraft version is what
        // made a correct NeoForge installation report a malformed version.
        let canonical = self.inherits_from.as_deref().unwrap_or(&id);
        let minecraft_version = MinecraftVersionId::parse(canonical);
        if !minecraft_version.is_recognized() {
            warn!(
                version = %id,
                minecraft = %canonical,
                "minecraft version id is in a scheme this build does not parse"
            );
        }

        Ok(VersionMetadata {
            minecraft_version,
            id,
            release_type: self.release_type,
            release_time: self.release_time,
            time: self.time,
            main_class: self.main_class,
            minimum_launcher_version: self.minimum_launcher_version,
            compliance_level: self.compliance_level,
            java_version: self.java_version,
            assets: self.assets,
            asset_index,
            downloads,
            libraries,
            arguments,
            logging,
            inherits_from: self.inherits_from,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::arguments::{ArgumentForm, ArgumentGroup};
    use crate::rules::{OsName, OsVersionCondition, RuleAction};
    use crate::test_support::fixture;

    fn parse(name: &str) -> VersionMetadata {
        VersionMetadata::parse(fixture(name)).unwrap_or_else(|error| {
            panic!("fixture `{name}` should parse: {error}");
        })
    }

    fn child(json: &str) -> VersionMetadata {
        VersionMetadata::parse(json).expect("child metadata")
    }

    const EMPTY_SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

    fn child_json(libraries: &str, arguments: &str) -> String {
        format!(
            r#"{{
                "id":"1.20.1-loader-test",
                "inheritsFrom":"1.20.1",
                "type":"release",
                "releaseTime":"2026-08-08T00:00:00Z",
                "time":"2026-08-08T00:00:00Z",
                "mainClass":"example.Loader",
                "libraries":[{libraries}],
                "arguments":{arguments}
            }}"#
        )
    }

    fn library(name: &str) -> String {
        format!(
            r#"{{"name":"{name}","downloads":{{"artifact":{{"url":"https://example.invalid/library.jar","sha1":"{EMPTY_SHA1}","size":0}}}}}}"#
        )
    }

    #[test]
    fn every_era_fixture_resolves_to_complete_vanilla_metadata() {
        for (name, java, arguments) in [
            ("1.12.2.json", 8, ArgumentForm::Legacy),
            ("1.20.1.json", 17, ArgumentForm::Modern),
            ("1.21.1.json", 21, ArgumentForm::Modern),
            ("26.1.json", 25, ArgumentForm::Modern),
        ] {
            let version = parse(name);
            assert!(version.is_complete_vanilla(), "{name} is incomplete");
            assert!(version.inherits_from().is_none(), "{name} is not a child");
            assert_eq!(version.java_major_version(), Some(java), "{name}");
            assert_eq!(version.arguments.form(), arguments, "{name}");
            assert_eq!(version.release_type, ReleaseType::Release, "{name}");
            assert_eq!(version.main_class, "net.minecraft.client.main.Main");

            let client = version.client_jar().expect("client jar");
            assert!(client.url.starts_with("https://"), "{name}");
            assert!(client.size > 0, "{name}");
            version.server_jar().expect("server jar");

            let assets = version.asset_index().expect("asset index");
            assert!(assets.total_size.is_some_and(|size| size > 0), "{name}");
            assert!(!version.libraries.is_empty(), "{name}");
            assert!(version.logging.is_some(), "{name}");
        }
    }

    #[test]
    fn a_child_version_shadows_parent_versions_by_artifact_identity() {
        let parent = parse("1.20.1.json");
        let inherited = parent
            .libraries
            .iter()
            .find(|entry| entry.name.starts_with("com.github.oshi:oshi-core:"))
            .expect("parent library");
        let replacement = library("com.github.oshi:oshi-core:99.0");
        let child = child(&child_json(&replacement, r#"{"game":[],"jvm":[]}"#));

        let merged = VersionMetadata::inherit(&parent, &child).expect("merge");
        assert_eq!(merged.libraries[0].name, "com.github.oshi:oshi-core:99.0");
        assert!(
            !merged
                .libraries
                .iter()
                .any(|entry| entry.name == inherited.name),
            "the inherited version must not remain on the classpath"
        );
    }

    #[test]
    fn classifiers_and_extensions_are_distinct_conflict_keys() {
        let mut parent = parse("1.20.1.json");
        parent.libraries = [
            library("example:artifact:1"),
            library("example:artifact:1:natives-linux"),
            library("example:artifact:1@zip"),
        ]
        .into_iter()
        .map(|json| {
            child(&child_json(&json, r#"{"game":[],"jvm":[]}"#))
                .libraries
                .remove(0)
        })
        .collect();
        let child_library = library("example:artifact:2@JAR");
        let child = child(&child_json(&child_library, r#"{"game":[],"jvm":[]}"#));

        let merged = VersionMetadata::inherit(&parent, &child).expect("merge");
        let names: Vec<_> = merged
            .libraries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "example:artifact:2@JAR",
                "example:artifact:1:natives-linux",
                "example:artifact:1@zip"
            ]
        );
    }

    #[test]
    fn child_duplicates_keep_their_order_while_the_parent_is_shadowed() {
        let mut parent = parse("1.20.1.json");
        parent.libraries = vec![
            child(&child_json(
                &library("example:rules:1"),
                r#"{"game":[],"jvm":[]}"#,
            ))
            .libraries
            .remove(0),
        ];
        let entries = format!(
            "{},{}",
            library("example:rules:2"),
            library("example:rules:3")
        );
        let child = child(&child_json(&entries, r#"{"game":[],"jvm":[]}"#));

        let merged = VersionMetadata::inherit(&parent, &child).expect("merge");
        let names: Vec<_> = merged
            .libraries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["example:rules:2", "example:rules:3"]);
    }

    #[test]
    fn modern_arguments_append_but_legacy_arguments_replace() {
        let modern_parent = parse("1.20.1.json");
        let modern_child = child(&child_json(
            "",
            r#"{"game":["--loader"],"jvm":["-Dloader=true"]}"#,
        ));
        let merged = VersionMetadata::inherit(&modern_parent, &modern_child).expect("modern merge");
        assert_eq!(
            merged
                .arguments
                .group(&ArgumentGroup::Game)
                .last()
                .expect("child game")
                .values,
            ["--loader"]
        );

        let legacy_parent = parse("1.12.2.json");
        let legacy_child = child(
            r#"{
                "id":"1.12.2-forge-test","inheritsFrom":"1.12.2","type":"release",
                "releaseTime":"2026-08-08T00:00:00Z","time":"2026-08-08T00:00:00Z",
                "mainClass":"net.minecraft.launchwrapper.Launch",
                "minecraftArguments":"--tweakClass example.Loader"
            }"#,
        );
        let merged = VersionMetadata::inherit(&legacy_parent, &legacy_child).expect("legacy merge");
        let values = &merged.arguments.group(&ArgumentGroup::Game)[0].values;
        assert_eq!(values, &["--tweakClass", "example.Loader"]);
    }

    #[test]
    fn the_java_component_is_recorded_but_not_acted_on() {
        // Java runtime support resolves these; Minecraft metadata support only has to carry them faithfully.
        for (name, component) in [
            ("1.12.2.json", "jre-legacy"),
            ("1.20.1.json", "java-runtime-gamma"),
            ("1.21.1.json", "java-runtime-delta"),
            ("26.1.json", "java-runtime-epsilon"),
        ] {
            let version = parse(name);
            assert_eq!(
                version.java_version.expect("java requirement").component,
                component
            );
        }
    }

    #[test]
    fn the_1_12_2_fixture_uses_the_legacy_argument_string_and_the_natives_map() {
        let version = parse("1.12.2.json");
        assert_eq!(version.arguments.form(), ArgumentForm::Legacy);
        assert!(version.arguments.group(&ArgumentGroup::Jvm).is_empty());

        let game = version.arguments.group(&ArgumentGroup::Game);
        assert_eq!(game.len(), 1);
        assert!(game[0].values.contains(&"${auth_player_name}".to_owned()));

        let legacy_natives: Vec<_> = version
            .libraries
            .iter()
            .filter(|library| library.is_legacy_natives())
            .collect();
        assert!(
            legacy_natives.len() >= 3,
            "the pre-1.19 natives map must survive parsing"
        );

        let lwjgl = legacy_natives
            .iter()
            .find(|library| {
                library
                    .name
                    .starts_with("org.lwjgl.lwjgl:lwjgl-platform:2.9.4")
            })
            .expect("lwjgl-platform");
        assert_eq!(lwjgl.natives_classifier("linux"), Some("natives-linux"));
        assert!(lwjgl.classifier("natives-linux").is_some());
        assert_eq!(
            lwjgl.extract.as_ref().expect("extract rules").exclude,
            ["META-INF/"]
        );
        // `allow` then `disallow os=osx` — the pair form, both halves kept.
        assert_eq!(lwjgl.rules.len(), 2);
        assert_eq!(lwjgl.rules[0].action, RuleAction::Allow);
        assert_eq!(lwjgl.rules[1].action, RuleAction::Disallow);
    }

    #[test]
    fn a_library_that_ships_only_classifiers_has_no_main_artifact() {
        let version = parse("1.12.2.json");
        let jinput = version
            .libraries
            .iter()
            .find(|library| library.name.starts_with("net.java.jinput:jinput-platform"))
            .expect("jinput-platform");
        assert!(jinput.downloads.artifact.is_none());
        assert_eq!(jinput.downloads.classifiers.len(), 3);
        assert!(jinput.rules.is_empty());
    }

    #[test]
    fn modern_releases_ship_natives_as_rule_gated_artifacts_instead() {
        for name in ["1.20.1.json", "1.21.1.json", "26.1.json"] {
            let version = parse(name);
            assert!(
                version
                    .libraries
                    .iter()
                    .all(|library| !library.is_legacy_natives()),
                "{name} should not use the pre-1.19 natives map"
            );
            let gated = version
                .libraries
                .iter()
                .find(|library| library.name.contains(":natives-"))
                .unwrap_or_else(|| panic!("{name} should ship a classifier-named native"));
            assert_eq!(gated.rules.len(), 1, "{name}");
            assert!(gated.downloads.artifact.is_some(), "{name}");
            assert!(gated.downloads.classifiers.is_empty(), "{name}");
        }
    }

    #[test]
    fn feature_and_os_conditions_survive_into_the_parsed_arguments() {
        let version = parse("1.20.1.json");
        let game = version.arguments.group(&ArgumentGroup::Game);
        let demo = game
            .iter()
            .find(|entry| entry.values == ["--demo"])
            .expect("--demo entry");
        assert_eq!(demo.rules[0].features.get("is_demo_user"), Some(&true));

        let jvm = version.arguments.group(&ArgumentGroup::Jvm);
        let arch = jvm
            .iter()
            .find(|entry| entry.values == ["-Xss1M"])
            .expect("-Xss1M entry");
        assert_eq!(
            arch.rules[0]
                .os
                .as_ref()
                .expect("os condition")
                .arch
                .as_deref(),
            Some("x86")
        );
    }

    #[test]
    fn the_26_x_fixture_carries_the_default_user_jvm_group_and_a_version_range() {
        let version = parse("26.1.json");
        let defaults = version.arguments.group(&ArgumentGroup::DefaultUserJvm);
        assert!(
            !defaults.is_empty(),
            "the group the project's snapshot does not mention must not be dropped"
        );
        assert!(
            defaults
                .iter()
                .any(|entry| entry.values.iter().any(|value| value.starts_with("-Xmx")))
        );

        let ranged = defaults
            .iter()
            .flat_map(|entry| entry.rules.iter())
            .filter_map(|rule| rule.os.as_ref())
            .find(|os| matches!(os.version, Some(OsVersionCondition::Range { .. })))
            .expect("26.x uses os.versionRange");
        assert_eq!(ranged.name, Some(OsName::Windows));

        assert_eq!(
            version.arguments.unrecognized_groups().count(),
            0,
            "default-user-jvm is a known group, not an unrecognized one"
        );
    }

    #[test]
    fn the_download_map_tolerates_keys_appearing_and_disappearing_between_eras() {
        // 1.20.1 ships obfuscation mappings; 26.1 stopped.
        assert!(parse("1.20.1.json").download("client_mappings").is_ok());
        let error = parse("26.1.json")
            .download("client_mappings")
            .expect_err("26.1 ships no mappings");
        assert!(matches!(error, VersionMetadataError::Incomplete { .. }));
    }

    #[test]
    fn a_loader_style_child_document_parses_and_reports_that_it_needs_a_parent() {
        // Shaped like a Forge JSON: no assets, no downloads, no javaVersion.
        let child = r#"{
            "id": "1.20.1-forge-47.4.0",
            "inheritsFrom": "1.20.1",
            "type": "release",
            "time": "2024-01-01T00:00:00+00:00",
            "releaseTime": "2024-01-01T00:00:00+00:00",
            "mainClass": "cpw.mods.bootstraplauncher.BootstrapLauncher",
            "libraries": [],
            "arguments": {"game": ["--x"]}
        }"#;
        let version = VersionMetadata::parse(child).expect("a child document must still parse");
        assert_eq!(version.inherits_from(), Some("1.20.1"));
        assert!(
            !version.is_complete_vanilla(),
            "a raw child is not launchable until its parent is merged"
        );
        let error = version.client_jar().expect_err("no client jar of its own");
        assert!(matches!(
            error,
            VersionMetadataError::Incomplete {
                field: "client",
                ..
            }
        ));
    }

    #[test]
    fn a_vanilla_document_is_for_the_release_its_own_id_names() {
        for (name, expected) in [
            ("1.12.2.json", "1.12.2"),
            ("1.20.1.json", "1.20.1"),
            ("1.21.1.json", "1.21.1"),
            ("26.1.json", "26.1"),
        ] {
            let version = parse(name);
            assert_eq!(version.minecraft_version.as_str(), expected, "{name}");
            assert!(
                version.minecraft_version.is_recognized(),
                "{name} is a release this build parses"
            );
        }
    }

    #[test]
    fn a_loader_document_is_for_the_release_it_inherits_not_the_build_it_names() {
        // The official NeoForge 26.2.0.59 and Forge 1.20.1 children. Their ids
        // are loader build identities; the Minecraft release each is for is the
        // one it inherits, and that is what the whole pipeline downstream means
        // by "the Minecraft version".
        for (json, id, minecraft) in [
            (
                include_str!("../../../fixtures/loaders/neoforge-26.2.0.59-version.json"),
                "neoforge-26.2.0.59",
                "26.2",
            ),
            (
                include_str!("../../../fixtures/loaders/forge-1.20.1-version.json"),
                "1.20.1-forge-47.4.10",
                "1.20.1",
            ),
        ] {
            let version = VersionMetadata::parse(json).expect("official loader child fixture");
            assert_eq!(version.id, id);
            assert_eq!(version.minecraft_version.as_str(), minecraft);
            assert!(
                version.minecraft_version.is_recognized(),
                "`{id}` is for Minecraft {minecraft}, which this build parses"
            );
        }
    }

    #[test]
    fn merging_a_loader_child_keeps_the_parents_minecraft_release() {
        // `inherit` clears `inheritsFrom` and keeps the child's loader id, so
        // after the merge neither field can still be reparsed for the release —
        // the answer has to have been carried across.
        let parent = parse("1.20.1.json");
        let child = child(include_str!(
            "../../../fixtures/loaders/forge-1.20.1-version.json"
        ));
        let merged = VersionMetadata::inherit(&parent, &child).expect("merge");

        assert_eq!(merged.id, "1.20.1-forge-47.4.10");
        assert_eq!(merged.inherits_from(), None);
        assert_eq!(merged.minecraft_version, parent.minecraft_version);
        assert_eq!(merged.minecraft_version.as_str(), "1.20.1");
    }

    #[test]
    fn a_release_id_in_an_unknown_scheme_stays_unrecognized_wherever_it_appears() {
        // The diagnostic this build actually needs: an id it cannot rank is
        // still reported as such, whether it is a document's own or the parent
        // a loader child claims to extend.
        let vanilla = child(
            r#"{
                "id":"2030w01a","type":"snapshot",
                "releaseTime":"2030-01-01T00:00:00Z","time":"2030-01-01T00:00:00Z",
                "mainClass":"net.minecraft.client.main.Main"
            }"#,
        );
        assert!(!vanilla.minecraft_version.is_recognized());
        assert_eq!(vanilla.minecraft_version.as_str(), "2030w01a");

        let loader = child(
            r#"{
                "id":"neoforge-99.0.0.1","inheritsFrom":"2030w01a","type":"release",
                "releaseTime":"2030-01-01T00:00:00Z","time":"2030-01-01T00:00:00Z",
                "mainClass":"net.neoforged.fml.startup.Client"
            }"#,
        );
        assert!(!loader.minecraft_version.is_recognized());
        assert_eq!(
            loader.minecraft_version.as_str(),
            "2030w01a",
            "the value reported as unparseable is the one that was supposed to be a release"
        );
    }

    #[test]
    fn current_neoforge_timestamps_without_offsets_are_utc() {
        let metadata = child(
            r#"{
                "id":"neoforge-26.1.2.94","inheritsFrom":"26.1.2","type":"release",
                "releaseTime":"2026-08-01T13:59:11.541184081",
                "time":"2026-08-01T13:59:11.541184081",
                "mainClass":"net.neoforged.fml.startup.Client"
            }"#,
        );
        assert_eq!(
            metadata.release_time.to_rfc3339(),
            "2026-08-01T13:59:11.541184081+00:00"
        );
    }

    #[test]
    fn a_document_that_is_not_a_version_document_fails_rather_than_half_parsing() {
        let error = VersionMetadata::parse(r#"{"id":"1.20.1"}"#).expect_err("missing fields");
        assert!(matches!(error, VersionParseError::Json(_)));
    }

    #[test]
    fn an_unknown_release_channel_is_never_treated_as_a_release() {
        let channel: ReleaseType =
            serde_json::from_str(r#""experimental_snapshot""#).expect("parses");
        assert_eq!(
            channel,
            ReleaseType::Other("experimental_snapshot".to_owned())
        );
        assert!(!channel.is_release());
    }
}
