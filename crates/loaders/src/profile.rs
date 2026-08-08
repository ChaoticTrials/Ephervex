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
    fn spec_zero_is_the_legacy_no_processor_shape() {
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
    fn unknown_profile_specs_fail_instead_of_being_approximated() {
        let error = InstallProfile::parse(
            r#"{"spec":2,"profile":"forge","version":"x","minecraft":"1.20.1"}"#,
        )
        .expect_err("future spec");
        assert!(matches!(error, LoaderError::UnsupportedProfile { spec: 2 }));
    }
}
