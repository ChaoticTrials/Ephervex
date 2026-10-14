use std::collections::BTreeMap;

use ephervex_download::MavenCoordinate;
use ephervex_model::{Hash, HashAlgorithm};
use serde::Deserialize;

use crate::{LoaderError, LoaderResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSide {
    Client,
    Server,
}

impl InstallSide {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProfileArtifact {
    pub url: String,
    pub sha1: Hash,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct ProfileLibrary {
    pub name: String,
    pub coordinate: MavenCoordinate,
    pub artifact: ProfileArtifact,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SideValue {
    pub client: String,
    pub server: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Processor {
    #[serde(default)]
    pub sides: Vec<String>,
    pub jar: String,
    #[serde(default)]
    pub classpath: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

impl Processor {
    #[must_use]
    pub fn applies_to(&self, side: InstallSide) -> bool {
        self.sides.is_empty() || self.sides.iter().any(|value| value == side.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct InstallProfile {
    pub spec: u32,
    pub profile: String,
    pub version: String,
    pub minecraft: String,
    pub path: Option<String>,
    pub data: BTreeMap<String, SideValue>,
    pub processors: Vec<Processor>,
    pub libraries: Vec<ProfileLibrary>,
}

impl InstallProfile {
    pub fn parse(json: &str) -> LoaderResult<Self> {
        let raw: RawInstallProfile =
            serde_json::from_str(json).map_err(|error| LoaderError::MetadataInvalid {
                provider: "installer",
                detail: error.to_string(),
            })?;
        if raw.spec > 1 {
            return Err(LoaderError::UnsupportedProfile { spec: raw.spec });
        }
        let mut libraries = Vec::with_capacity(raw.libraries.len());
        for library in raw.libraries {
            let coordinate = MavenCoordinate::parse(&library.name)?;
            let artifact = library.downloads.artifact;
            let sha1 = Hash::new(HashAlgorithm::Sha1, &artifact.sha1).ok_or_else(|| {
                LoaderError::MetadataInvalid {
                    provider: "installer",
                    detail: format!("library `{}` has a malformed SHA-1", library.name),
                }
            })?;
            libraries.push(ProfileLibrary {
                name: library.name,
                coordinate,
                artifact: ProfileArtifact {
                    url: artifact.url,
                    sha1,
                    size: artifact.size,
                },
            });
        }
        Ok(Self {
            spec: raw.spec,
            profile: raw.profile,
            version: raw.version,
            minecraft: raw.minecraft,
            path: raw.path,
            data: raw.data,
            processors: raw.processors,
            libraries,
        })
    }

    /// Whether an `install_profile.json` is in the layout Forge used before it
    /// shipped a separate `version.json`.
    ///
    /// That layout has no `spec` and embeds the launch metadata under
    /// `versionInfo` beside an `install` section, so it cannot be read as an
    /// [`InstallProfile`]. Anything that is not such a document, including text
    /// that is not JSON, is left for [`InstallProfile::parse`] to judge.
    #[must_use]
    pub fn is_embedded_version_layout(json: &str) -> bool {
        #[derive(Deserialize)]
        struct Layout {
            #[serde(default)]
            spec: Option<serde::de::IgnoredAny>,
            #[serde(default)]
            install: Option<serde::de::IgnoredAny>,
            #[serde(default, rename = "versionInfo")]
            version_info: Option<serde::de::IgnoredAny>,
        }
        serde_json::from_str::<Layout>(json).is_ok_and(|layout| {
            layout.spec.is_none() && layout.install.is_some() && layout.version_info.is_some()
        })
    }

    /// Whether this profile's install is driven by the processor pipeline.
    ///
    /// `spec` does not answer this and never has. Forge 1.12.2 is `spec` 0 with
    /// no processors, Forge 1.13 through 1.16.5 is `spec` 0 *with* a full
    /// processor chain, and Forge 1.17 onward and every NeoForge build are
    /// `spec` 1. What separates an install that has to patch and remap the game
    /// from one that only unpacks a jar is whether the profile declares
    /// processors, so that is what is asked.
    #[must_use]
    pub fn is_processor_driven(&self) -> bool {
        !self.processors.is_empty()
    }

    pub fn data_value(&self, name: &str, side: InstallSide) -> Option<&str> {
        let value = self.data.get(name)?;
        Some(match side {
            InstallSide::Client => &value.client,
            InstallSide::Server => &value.server,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawInstallProfile {
    spec: u32,
    profile: String,
    version: String,
    minecraft: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    data: BTreeMap<String, SideValue>,
    #[serde(default)]
    processors: Vec<Processor>,
    #[serde(default)]
    libraries: Vec<RawProfileLibrary>,
}

#[derive(Debug, Deserialize)]
struct RawProfileLibrary {
    name: String,
    downloads: RawProfileDownloads,
}

#[derive(Debug, Deserialize)]
struct RawProfileDownloads {
    artifact: RawProfileArtifact,
}

#[derive(Debug, Deserialize)]
struct RawProfileArtifact {
    #[serde(default)]
    url: String,
    sha1: String,
    size: u64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_launchwrapper_era_declares_no_processors() {
        let profile = InstallProfile::parse(include_str!(
            "../../../fixtures/loaders/forge-1.12.2-install-profile.json"
        ))
        .expect("profile");
        assert_eq!(profile.spec, 0);
        assert!(profile.processors.is_empty());
        assert_eq!(
            profile.libraries[1].coordinate.version(),
            "1.12.2-14.23.5.2859"
        );
        assert!(profile.libraries[1].artifact.url.is_empty());
    }

    #[test]
    fn the_modlauncher_era_is_processor_driven_despite_declaring_spec_zero() {
        let legacy = InstallProfile::parse(include_str!(
            "../../../fixtures/loaders/forge-1.12.2-install-profile.json"
        ))
        .expect("profile");
        let modlauncher = InstallProfile::parse(include_str!(
            "../../../fixtures/loaders/forge-1.16.3-install-profile.json"
        ))
        .expect("profile");
        let argfile = InstallProfile::parse(include_str!(
            "../../../fixtures/loaders/forge-1.20.1-install-profile.json"
        ))
        .expect("profile");

        // Both of the first two declare the same `spec`, and only one of them
        // has a game to patch. Reading the era off `spec` skipped every
        // processor for Forge 1.13 through 1.16.5 and produced an install whose
        // launcher jar and remapped game were simply absent.
        assert_eq!(legacy.spec, 0);
        assert_eq!(modlauncher.spec, 0);
        assert_eq!(argfile.spec, 1);
        assert!(!legacy.is_processor_driven());
        assert!(modlauncher.is_processor_driven());
        assert!(argfile.is_processor_driven());
    }

    #[test]
    fn the_modlauncher_era_names_a_launcher_jar_no_processor_produces() {
        let profile = InstallProfile::parse(include_str!(
            "../../../fixtures/loaders/forge-1.16.3-install-profile.json"
        ))
        .expect("profile");
        assert_eq!(
            profile.path.as_deref(),
            Some("net.minecraftforge:forge:1.16.3-34.1.42")
        );
        // The profile's own library list names the `universal` classifier, so
        // the coordinate `path` points at is installed by nothing the profile
        // describes.
        assert!(
            !profile
                .libraries
                .iter()
                .any(|library| library.name == "net.minecraftforge:forge:1.16.3-34.1.42")
        );
        assert!(
            profile
                .libraries
                .iter()
                .any(|library| library.name == "net.minecraftforge:forge:1.16.3-34.1.42:universal")
        );
    }

    #[test]
    fn a_profile_without_sides_applies_to_both() {
        let profile = InstallProfile::parse(include_str!(
            "../../../fixtures/loaders/forge-1.16.3-install-profile.json"
        ))
        .expect("profile");
        for processor in &profile.processors {
            assert!(processor.applies_to(InstallSide::Client));
            assert!(processor.applies_to(InstallSide::Server));
        }
    }

    #[test]
    fn official_modern_profiles_are_processor_driven_and_side_filtered() {
        for (fixture, version) in [
            (
                include_str!("../../../fixtures/loaders/forge-1.20.1-install-profile.json"),
                "1.20.1-forge-47.4.10",
            ),
            (
                include_str!("../../../fixtures/loaders/neoforge-26.1.2-install-profile.json"),
                "neoforge-26.1.2.94",
            ),
        ] {
            let profile = InstallProfile::parse(fixture).expect("official profile shape");
            assert_eq!(profile.spec, 1);
            assert_eq!(profile.version, version);
            assert!(!profile.processors[0].applies_to(InstallSide::Client));
            assert!(profile.processors[0].applies_to(InstallSide::Server));
            assert!(profile.processors[1].applies_to(InstallSide::Client));
            assert!(profile.processors[1].applies_to(InstallSide::Server));
        }
    }

    #[test]
    fn a_profile_embedding_its_version_metadata_is_recognized_as_the_older_layout() {
        let older = r#"{"install":{"profileName":"Forge","target":"forge-x","path":"net.minecraftforge:forge:1.12.2-14.23.5.2847","version":"Forge 14.23.5.2847","filePath":"forge-universal.jar","minecraft":"1.12.2"},"versionInfo":{"id":"1.12.2-forge1.12.2-14.23.5.2847","libraries":[]},"optionals":[]}"#;
        assert!(InstallProfile::is_embedded_version_layout(older));
        assert!(matches!(
            InstallProfile::parse(older),
            Err(LoaderError::MetadataInvalid { .. })
        ));
    }

    #[test]
    fn current_profiles_and_non_json_are_not_the_older_layout() {
        for fixture in [
            include_str!("../../../fixtures/loaders/forge-1.12.2-install-profile.json"),
            include_str!("../../../fixtures/loaders/forge-1.16.3-install-profile.json"),
            include_str!("../../../fixtures/loaders/forge-1.20.1-install-profile.json"),
            include_str!("../../../fixtures/loaders/neoforge-26.1.2-install-profile.json"),
            "not json",
            "[]",
        ] {
            assert!(!InstallProfile::is_embedded_version_layout(fixture));
        }
        // A declared `spec` means the document is a current profile, whatever else it carries.
        assert!(!InstallProfile::is_embedded_version_layout(
            r#"{"spec":0,"install":{},"versionInfo":{}}"#
        ));
    }

    #[test]
    fn unknown_profile_specs_fail_instead_of_being_approximated() {
        let error = InstallProfile::parse(
            r#"{"spec":2,"profile":"forge","version":"x","minecraft":"1.20.1"}"#,
        )
        .expect_err("future spec");
        assert!(matches!(error, LoaderError::UnsupportedProfile { spec: 2 }));
    }
}
