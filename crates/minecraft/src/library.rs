//! Libraries, downloadable artifacts, and natives.
//!
//! # Two eras of natives, both still in range
//!
//! Everything from 1.12.2 to 1.18.x ships natives as a *classifier map*: the
//! library carries `"natives": {"linux": "natives-linux", …}`, and the named
//! classifier is looked up in `downloads.classifiers` and extracted into the
//! instance's natives directory, minus whatever `extract.exclude` lists.
//!
//! From 1.19 onward Mojang stopped doing that. Natives are ordinary library
//! entries whose Maven coordinate already carries the classifier
//! (`org.lwjgl:lwjgl-glfw:3.3.3:natives-linux`) and whose applicability is
//! decided by an ordinary OS rule. There is no `natives` map, no `classifiers`
//! object, and — importantly — no extraction step: they go on the classpath.
//!
//! Both forms are parsed here and neither is normalized into the other, because
//! they genuinely behave differently at launch. [`Library::natives_classifier`]
//! is how Vanilla launch support asks which era a given entry belongs to.
//!
//! # Why the coordinate is parsed eagerly
//!
//! `name` is the library's identity *and* its path in the shared libraries
//! store. Deferring the parse would mean a malformed coordinate first shows up
//! as a path being built somewhere downstream, which is exactly the class of
//! bug [`MavenCoordinate`] exists to make impossible. Parsing it
//! at the boundary means a bad coordinate fails clearly, naming the library

use std::collections::BTreeMap;

use ephervex_download::{CacheKeyError, MavenCoordinate};
use ephervex_model::{Hash, HashAlgorithm};
use serde::Deserialize;

use crate::rules::{RawRule, Rule, parse_rules};

/// One downloadable file described by Mojang metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub url: String,
    /// Always present in Mojang metadata, and always verified before an
    /// artifact is accepted into a shared store.
    pub sha1: Hash,
    pub size: u64,
    /// Mojang's own suggested relative path, present on library artifacts.
    ///
    /// Recorded for diagnostics only. Ephervex derives its cache destination
    /// from the validated [`MavenCoordinate`] instead, because a path that
    /// arrives inside a metadata document is remote input and must never
    /// choose where a file lands.
    pub path: Option<String>,
}

/// The wire shape of an artifact, before the digest has been validated.
#[derive(Debug, Deserialize)]
pub(crate) struct RawArtifact {
    url: String,
    sha1: String,
    size: u64,
    #[serde(default)]
    path: Option<String>,
}

impl RawArtifact {
    /// Validates the digest. A malformed SHA-1 is rejected at the parsing
    /// boundary rather than surviving to fail a verification later, where the
    /// message would blame the download instead of the metadata.
    pub(crate) fn into_artifact(self) -> Option<Artifact> {
        Some(Artifact {
            sha1: Hash::new(HashAlgorithm::Sha1, &self.sha1)?,
            url: self.url,
            size: self.size,
            path: self.path,
        })
    }
}

/// What a library ships.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryDownloads {
    /// The main jar. Absent on a few pre-1.19 natives-only libraries, which
    /// carry classifiers and nothing else.
    pub artifact: Option<Artifact>,
    /// Classifier-keyed artifacts, e.g. `natives-linux`. Pre-1.19 only.
    pub classifiers: BTreeMap<String, Artifact>,
}

/// Paths excluded when a natives jar is unpacked.
///
/// Universally `["META-INF/"]`, but honored from the metadata rather than
/// hardcoded — and note this is a *filter*, not a substitute for the path
/// checks extraction itself must do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ExtractRules {
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One entry of a version's `libraries` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    /// The Maven coordinate as published.
    pub name: String,
    /// The same coordinate, validated — and therefore usable to build a path
    /// under the shared libraries store.
    pub coordinate: MavenCoordinate,
    pub downloads: LibraryDownloads,
    /// Pre-1.19 OS-keyed classifier names. Values may contain `${arch}`, which
    /// is substituted at launch, not here.
    pub natives: BTreeMap<String, String>,
    pub rules: Vec<Rule>,
    pub extract: Option<ExtractRules>,
    /// A Maven repository base URL. Never present in Mojang metadata; loader
    /// JSONs use it, and it is parsed so loader support does not have to reopen this
    /// type.
    pub url: Option<String>,
}

impl Library {
    /// The classifier holding this library's natives for `os`, if it uses the
    /// pre-1.19 form.
    ///
    /// `None` means either "this library has no natives" or "this library *is*
    /// a native, in the post-1.18 style" — [`Self::is_legacy_natives`]
    /// separates them.
    #[must_use]
    pub fn natives_classifier(&self, os: &str) -> Option<&str> {
        self.natives.get(os).map(String::as_str)
    }

    /// Whether this entry uses the pre-1.19 extract-a-classifier form.
    #[must_use]
    pub fn is_legacy_natives(&self) -> bool {
        !self.natives.is_empty()
    }

    /// The artifact for a classifier, e.g. the one
    /// [`Self::natives_classifier`] named.
    #[must_use]
    pub fn classifier(&self, classifier: &str) -> Option<&Artifact> {
        self.downloads.classifiers.get(classifier)
    }
}

/// The wire shape of a library.
#[derive(Debug, Deserialize)]
pub(crate) struct RawLibrary {
    name: String,
    #[serde(default)]
    downloads: Option<RawLibraryDownloads>,
    #[serde(default)]
    natives: BTreeMap<String, String>,
    #[serde(default)]
    rules: Vec<RawRule>,
    #[serde(default)]
    extract: Option<ExtractRules>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLibraryDownloads {
    #[serde(default)]
    artifact: Option<RawArtifact>,
    #[serde(default)]
    classifiers: BTreeMap<String, RawArtifact>,
}

/// Why a library entry could not be turned into a [`Library`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LibraryError {
    /// The `name` is not a coordinate that can safely become a cache path.
    #[error("library `{name}` has an unusable maven coordinate")]
    Coordinate {
        name: String,
        #[source]
        source: CacheKeyError,
    },
    /// A digest in the metadata is not a well-formed SHA-1.
    #[error("library `{name}` declares a malformed sha1 for `{artifact}`")]
    MalformedDigest { name: String, artifact: String },
}

impl RawLibrary {
    pub(crate) fn into_library(self) -> Result<Library, LibraryError> {
        let coordinate =
            MavenCoordinate::parse(&self.name).map_err(|source| LibraryError::Coordinate {
                name: self.name.clone(),
                source,
            })?;

        let raw_downloads = self.downloads.unwrap_or_default();
        let artifact = match raw_downloads.artifact {
            Some(raw) => {
                Some(
                    raw.into_artifact()
                        .ok_or_else(|| LibraryError::MalformedDigest {
                            name: self.name.clone(),
                            artifact: "artifact".to_owned(),
                        })?,
                )
            }
            None => None,
        };
        let mut classifiers = BTreeMap::new();
        for (classifier, raw) in raw_downloads.classifiers {
            let parsed = raw
                .into_artifact()
                .ok_or_else(|| LibraryError::MalformedDigest {
                    name: self.name.clone(),
                    artifact: classifier.clone(),
                })?;
            classifiers.insert(classifier, parsed);
        }

        Ok(Library {
            name: self.name,
            coordinate,
            downloads: LibraryDownloads {
                artifact,
                classifiers,
            },
            natives: self.natives,
            rules: parse_rules(self.rules),
            extract: self.extract,
            url: self.url,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn parse(json: &str) -> Result<Library, LibraryError> {
        let raw: RawLibrary = serde_json::from_str(json).expect("well-formed library");
        raw.into_library()
    }

    const SHA1: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";

    #[test]
    fn a_malformed_coordinate_names_the_library_rather_than_failing_anonymously() {
        let error = parse(&format!(
            r#"{{"name":"../evil:x:1.0","downloads":{{"artifact":{{"url":"https://x.invalid/a.jar","sha1":"{SHA1}","size":1}}}}}}"#
        ))
.expect_err("a coordinate that could escape the store must be refused");
        assert!(matches!(error, LibraryError::Coordinate { name,.. } if name == "../evil:x:1.0"));
    }

    #[test]
    fn a_malformed_digest_is_rejected_at_the_parsing_boundary() {
        let error = parse(
            r#"{"name":"com.mojang:patchy:1.3.9","downloads":{"artifact":{"url":"https://x.invalid/a.jar","sha1":"nope","size":1}}}"#,
        )
.expect_err("a bad sha1 must not survive parsing");
        assert!(matches!(error, LibraryError::MalformedDigest { .. }));
    }

    #[test]
    fn a_classifier_only_library_parses_without_a_main_artifact() {
        let library = parse(&format!(
            r#"{{"name":"net.java.jinput:jinput-platform:2.0.5","downloads":{{"classifiers":{{"natives-linux":{{"url":"https://x.invalid/n.jar","sha1":"{SHA1}","size":2,"path":"a/b.jar"}}}}}},"natives":{{"linux":"natives-linux"}},"extract":{{"exclude":["META-INF/"]}}}}"#
        ))
.expect("library parses");

        assert!(library.downloads.artifact.is_none());
        assert!(library.is_legacy_natives());
        assert_eq!(library.natives_classifier("linux"), Some("natives-linux"));
        assert_eq!(
            library.classifier("natives-linux").expect("artifact").size,
            2
        );
        assert_eq!(
            library.extract.expect("extract rules").exclude,
            ["META-INF/"]
        );
    }

    #[test]
    fn a_modern_native_is_an_ordinary_rule_gated_artifact_not_a_classifier_map() {
        let library = parse(&format!(
            r#"{{"name":"org.lwjgl:lwjgl-glfw:3.3.3:natives-linux","downloads":{{"artifact":{{"url":"https://x.invalid/g.jar","sha1":"{SHA1}","size":3}}}},"rules":[{{"action":"allow","os":{{"name":"linux"}}}}]}}"#
        ))
.expect("library parses");

        assert!(!library.is_legacy_natives());
        assert!(library.natives_classifier("linux").is_none());
        assert_eq!(library.rules.len(), 1);
        // The classifier lives in the coordinate, so the cache path carries it
        // and two natives of the same library cannot collide.
        assert!(
            library
                .coordinate
                .relative_path()
                .to_string_lossy()
                .ends_with("lwjgl-glfw-3.3.3-natives-linux.jar")
        );
    }

    #[test]
    fn an_arch_placeholder_in_a_natives_classifier_is_left_for_the_launcher_to_substitute() {
        let library = parse(
            r#"{"name":"com.mojang:text2speech:1.10.3","natives":{"windows":"natives-windows-${arch}"}}"#,
        )
.expect("library parses");
        assert_eq!(
            library.natives_classifier("windows"),
            Some("natives-windows-${arch}")
        );
    }
}
