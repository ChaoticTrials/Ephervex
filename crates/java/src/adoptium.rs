//! Eclipse Temurin releases, read through the Adoptium API v3.
//!
//! # The contract, re-verified
//!
//! Checked against the live service on 2026-08-07 rather than taken from
//! the project's snapshot:
//!
//! ```text
//! GET /v3/assets/latest/{major}/hotspot
//!         ?architecture=x64&image_type=jdk&os=linux&vendor=eclipse
//! ```
//!
//! returns a JSON **array**, each element carrying one `binary` — singular —
//! whose `package` holds `link`, `name`, `size`, and `checksum`. The checksum
//! is a hex **SHA-256**; the API does not say so anywhere in the payload, and
//! guessing SHA-1 from its presence would fail verification on every download.
//!
//! The sibling `/v3/assets/feature_releases/{major}/{type}` endpoint returns
//! `binaries` — plural — instead. Both are live; this uses `latest` because
//! "the newest GA build of this major" is exactly the question being asked, and
//! answering it with a paged release list would mean re-implementing Adoptium's
//! own ordering.
//!
//! Java 8 is still served (`jdk8u502-b07` at the time of writing) and
//! `/v3/info/available_releases` reports majors 8 through 26 with 25 as the
//! most recent LTS — so nothing here may treat 21 as a ceiling.
//!
//! # Architecture names do not match Java's
//!
//! A JVM reports `os.arch = amd64`. Adoptium calls that architecture `x64`, and
//! a query that passes `amd64` through asks for something that does not exist —
//! it returns an empty array rather than an error, so the failure arrives as
//! "no release found" and looks like Adoptium's fault. [`AdoptiumArchitecture`]
//! is the translation, and it is a hard boundary: the JVM's spelling never
//! reaches a URL.
//!
//! # No second HTTP path
//!
//! The API document goes through `ephervex-download`'s [`MetadataCache`], so it
//! inherits the retry policy, the redirect bound, the User-Agent, and the
//! stale-on-failure behavior every other provider fetch has.

use ephervex_download::{
    CacheLayout, DownloadManager, MetadataCache, MetadataCacheError, MetadataKey, OperationReporter,
};
use ephervex_model::{Hash, HashAlgorithm};
use reqwest::Url;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::error::JavaError;

/// The production API root.
pub const ADOPTIUM_API_BASE: &str = "https://api.adoptium.net";

/// How long a resolved release stays fresh.
///
/// Adoptium publishes quarterly, so a day is generous and still means a
/// machine that installs two runtimes in a session makes one request.
pub const RELEASE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// An architecture in Adoptium's spelling.
///
/// A newtype rather than a `&str` so the JVM's `os.arch` cannot reach a query
/// string without passing through [`AdoptiumArchitecture::from_java_arch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdoptiumArchitecture(&'static str);

impl AdoptiumArchitecture {
    /// Translates a `java.arch`/Rust `target_arch` value.
    ///
    /// Returns `None` for an architecture Adoptium does not publish, which is a
    /// clear "no managed runtime here" rather than a request that quietly
    /// returns nothing.
    #[must_use]
    pub fn from_java_arch(arch: &str) -> Option<Self> {
        let name = match arch.trim().to_ascii_lowercase().as_str() {
            // The one that matters on a desktop: every x86-64 JVM says `amd64`
            // and Rust says `x86_64`; Adoptium says neither.
            "amd64" | "x86_64" | "x64" => "x64",
            "aarch64" | "arm64" => "aarch64",
            "x86" | "i386" | "i486" | "i586" | "i686" => "x86",
            "arm" | "armv7l" | "armv7" => "arm",
            "ppc64le" | "powerpc64le" => "ppc64le",
            "ppc64" | "powerpc64" => "ppc64",
            "s390x" => "s390x",
            "riscv64" => "riscv64",
            _ => return None,
        };
        Some(Self(name))
    }

    /// The architecture this build of Ephervex is running on.
    #[must_use]
    pub fn current() -> Option<Self> {
        Self::from_java_arch(std::env::consts::ARCH)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// One downloadable Temurin build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRelease {
    pub major: u32,
    /// Adoptium's release name, e.g. `jdk-21.0.12+8` or `jdk8u502-b07`.
    pub release_name: String,
    /// `eclipse` for Temurin. Recorded because the managed install path is keyed on
    /// `<vendor-build>`.
    pub vendor: String,
    pub url: Url,
    /// The archive's file name as Adoptium publishes it. Used for display and
    /// for nothing else — never to choose a path.
    pub filename: String,
    pub checksum: Hash,
    pub size: u64,
    pub architecture: AdoptiumArchitecture,
}

impl ManagedRelease {
    /// The install directory name: `<vendor>-<release>` with anything that is
    /// not a safe path character replaced.
    ///
    /// Derived from provider text, so it is sanitized here rather than trusted.
    /// `+` survives because it is harmless and because
    /// `eclipse-jdk-21.0.12+8` is recognizable while `eclipse-jdk-21.0.12-8`
    /// is ambiguous with a hyphenated build.
    #[must_use]
    pub fn install_directory_name(&self) -> String {
        let raw = format!("{}-{}", self.vendor, self.release_name);
        let mut cleaned: String = raw
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '+') {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        // `.` has to survive — every release name contains one — so `..` is
        // collapsed separately rather than by the character filter.
        while cleaned.contains("..") {
            cleaned = cleaned.replace("..", "-");
        }
        let trimmed = cleaned.trim_matches(['.', '-']).to_owned();
        if trimmed.is_empty() {
            format!("java-{}", self.major)
        } else {
            trimmed
        }
    }
}

/// Reads Adoptium's release index.
#[derive(Debug, Clone)]
pub struct AdoptiumClient {
    manager: DownloadManager,
    cache: MetadataCache,
    base: Url,
}

impl AdoptiumClient {
    /// A client against the production API.
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> Result<Self, JavaError> {
        Self::with_base(manager, layout, ADOPTIUM_API_BASE)
    }

    /// A client against an explicit API root.
    ///
    /// Exists for the offline tests, which point it at a loopback server. Not a
    /// configuration knob: redirecting the runtime index is a way to install an
    /// arbitrary binary, and the checksum it publishes would be the attacker's.
    pub fn with_base(
        manager: DownloadManager,
        layout: CacheLayout,
        base: &str,
    ) -> Result<Self, JavaError> {
        Ok(Self {
            manager,
            cache: MetadataCache::with_ttl(layout, RELEASE_TTL),
            base: Url::parse(base).map_err(|_| JavaError::ManagedApiUnusable {
                url: base.to_owned(),
            })?,
        })
    }

    /// Resolves the newest GA build of one major for one architecture.
    #[instrument(skip(self, reporter, cancel))]
    pub async fn latest_release(
        &self,
        major: u32,
        architecture: AdoptiumArchitecture,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<ManagedRelease, JavaError> {
        let mut url = self
            .base
            .join(&format!("/v3/assets/latest/{major}/hotspot"))
            .map_err(|_| JavaError::ManagedApiUnusable {
                url: self.base.to_string(),
            })?;
        url.query_pairs_mut()
            .append_pair("architecture", architecture.as_str())
            .append_pair("image_type", "jdk")
            .append_pair("os", "linux")
            .append_pair("vendor", "eclipse");

        let key = MetadataKey::new(&format!(
            "adoptium-latest-{major}-linux-{}",
            architecture.as_str()
        ))
        .map_err(|source| JavaError::ManagedCacheKey { source })?;

        let document = self
            .cache
            .fetch(&self.manager, &key, &url, reporter, cancel)
            .await
            .map_err(|source| JavaError::ManagedIndexUnavailable {
                major,
                source: Box::new(source),
            })?;

        let assets: Vec<LatestAsset> =
            document
                .parse()
                .map_err(|source| JavaError::ManagedIndexInvalid {
                    major,
                    source: Box::new(source),
                })?;
        debug!(
            major,
            assets = assets.len(),
            freshness = ?document.freshness(),
            "read the adoptium release index"
        );

        let asset = assets
            .into_iter()
            .next()
            .ok_or(JavaError::ManagedReleaseMissing {
                major,
                architecture: architecture.as_str(),
            })?;
        asset.into_release(major, architecture)
    }
}

/// One element of `/v3/assets/latest/{major}/{jvm_impl}`.
///
/// Only the fields Ephervex acts on are modelled. The API returns download
/// counts, signature links, and SCM references that would be dead weight here,
/// and `serde` ignores what is not named.
#[derive(Debug, Deserialize)]
struct LatestAsset {
    binary: LatestBinary,
    release_name: String,
    #[serde(default)]
    vendor: String,
}

#[derive(Debug, Deserialize)]
struct LatestBinary {
    package: BinaryPackage,
}

#[derive(Debug, Deserialize)]
struct BinaryPackage {
    link: String,
    name: String,
    /// Hex SHA-256. The API does not label the algorithm anywhere in the
    /// payload; the `checksum_link` points at a `.sha256.txt`, which is the
    /// only place it says so.
    checksum: String,
    size: u64,
}

impl LatestAsset {
    fn into_release(
        self,
        major: u32,
        architecture: AdoptiumArchitecture,
    ) -> Result<ManagedRelease, JavaError> {
        let malformed = || JavaError::ManagedReleaseMalformed {
            major,
            release: self.release_name.clone(),
        };

        let url = Url::parse(&self.binary.package.link).map_err(|_| malformed())?;
        // A release with no usable checksum is refused rather than downloaded
        // unverified: an unverified JDK is a binary the launcher then executes
        let checksum = Hash::new(HashAlgorithm::Sha256, &self.binary.package.checksum)
            .ok_or_else(malformed)?;

        Ok(ManagedRelease {
            major,
            release_name: self.release_name,
            vendor: if self.vendor.is_empty() {
                "eclipse".to_owned()
            } else {
                self.vendor
            },
            url,
            filename: self.binary.package.name,
            checksum,
            size: self.binary.package.size,
            architecture,
        })
    }
}

/// Adapts a metadata-cache failure into this crate's error, for a caller that
/// already knows which major it was asking about.
impl From<MetadataCacheError> for JavaError {
    fn from(source: MetadataCacheError) -> Self {
        Self::ManagedIndexUnavailable {
            major: 0,
            source: Box::new(source),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_jvms_architecture_spelling_is_translated_before_it_reaches_a_url() {
        // `amd64` is what every x86-64 JVM reports and it is not a name
        // Adoptium knows. Passing it through returns an empty array, which
        // surfaces as "no such release" and looks like the provider's fault.
        assert_eq!(
            AdoptiumArchitecture::from_java_arch("amd64")
                .expect("amd64")
                .as_str(),
            "x64"
        );
        assert_eq!(
            AdoptiumArchitecture::from_java_arch("x86_64")
                .expect("x86_64")
                .as_str(),
            "x64"
        );
        assert_eq!(
            AdoptiumArchitecture::from_java_arch("aarch64")
                .expect("aarch64")
                .as_str(),
            "aarch64"
        );
        assert_eq!(
            AdoptiumArchitecture::from_java_arch("arm64")
                .expect("arm64")
                .as_str(),
            "aarch64"
        );
        assert_eq!(
            AdoptiumArchitecture::from_java_arch("  AMD64 ")
                .expect("padded")
                .as_str(),
            "x64"
        );
    }

    #[test]
    fn an_architecture_adoptium_does_not_publish_is_refused_rather_than_guessed() {
        for arch in ["sparcv9", "mips64", "loongarch64", ""] {
            assert!(
                AdoptiumArchitecture::from_java_arch(arch).is_none(),
                "{arch}"
            );
        }
    }

    #[test]
    fn this_build_runs_on_an_architecture_adoptium_publishes_for() {
        // Not a tautology: it fails the day someone builds Ephervex for a
        // target with no Temurin, which is precisely when the managed provider
        // needs to say so rather than 404.
        assert!(AdoptiumArchitecture::current().is_some());
    }

    /// The live response shape, reduced to the fields the parser reads.
    const LATEST_21: &str = r#"[
        {
            "binary": {
                "architecture": "x64",
                "image_type": "jdk",
                "os": "linux",
                "package": {
                    "checksum": "e4446ff06a276155697597cc0f1b15da004ff083f4964a35271ecee567177370",
                    "link": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12%2B8/OpenJDK21U-jdk_x64_linux_hotspot_21.0.12_8.tar.gz",
                    "name": "OpenJDK21U-jdk_x64_linux_hotspot_21.0.12_8.tar.gz",
                    "size": 207486543
                }
            },
            "release_name": "jdk-21.0.12+8",
            "vendor": "eclipse"
        }
    ]"#;

    fn parse_latest(json: &str, major: u32) -> Result<ManagedRelease, JavaError> {
        let assets: Vec<LatestAsset> = serde_json::from_str(json).expect("parses");
        assets.into_iter().next().expect("one asset").into_release(
            major,
            AdoptiumArchitecture::from_java_arch("x64").expect("x64"),
        )
    }

    #[test]
    fn a_latest_asset_becomes_a_release_with_a_sha256_checksum() {
        let release = parse_latest(LATEST_21, 21).expect("release");
        assert_eq!(release.release_name, "jdk-21.0.12+8");
        assert_eq!(release.checksum.algorithm, HashAlgorithm::Sha256);
        assert_eq!(release.size, 207_486_543);
        assert!(release.url.as_str().ends_with(".tar.gz"));
    }

    #[test]
    fn a_release_without_a_well_formed_checksum_is_refused() {
        // Downloading a JDK unverified means executing an unverified binary.
        let tampered = LATEST_21.replace(
            "e4446ff06a276155697597cc0f1b15da004ff083f4964a35271ecee567177370",
            "not-a-digest",
        );
        let error = parse_latest(&tampered, 21).expect_err("must refuse");
        assert!(
            matches!(error, JavaError::ManagedReleaseMalformed { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn the_install_directory_name_is_derived_from_provider_text_and_sanitized() {
        let mut release = parse_latest(LATEST_21, 21).expect("release");
        assert_eq!(release.install_directory_name(), "eclipse-jdk-21.0.12+8");

        release.release_name = "../../etc/cron.d/pwn".to_owned();
        let name = release.install_directory_name();
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
    }

    #[test]
    fn a_java_8_release_name_survives_sanitization_unchanged() {
        // Adoptium names Java 8 builds `jdk8u502-b07`, not `jdk-8.0.502+7`.
        let mut release = parse_latest(LATEST_21, 8).expect("release");
        release.release_name = "jdk8u502-b07".to_owned();
        assert_eq!(release.install_directory_name(), "eclipse-jdk8u502-b07");
    }
}
