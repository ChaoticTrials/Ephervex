use std::cmp::Ordering;
use std::collections::BTreeMap;

use ephervex_download::{
    CacheLayout, DownloadManager, Freshness, MetadataCache, MetadataKey, OperationReporter,
};
use ephervex_minecraft::MinecraftVersionId;
use ephervex_model::{LoaderKind, LoaderVersionMode};
use quick_xml::de::from_str;
use reqwest::Url;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{LoaderError, LoaderResult};

pub const FORGE_PROMOTIONS_URL: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
pub const FORGE_MAVEN_METADATA_URL: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
pub const NEOFORGE_MAVEN_METADATA_URL: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
/// The Maven artifact NeoForge published its first line under, before it
/// renamed the artifact from `forge` to `neoforge`.
pub const NEOFORGE_FORGE_ARTIFACT_MAVEN_METADATA_URL: &str =
    "https://maven.neoforged.net/releases/net/neoforged/forge/maven-metadata.xml";

/// The oldest Minecraft release NeoForge was ever published for.
///
/// NeoForge forked from Forge at Minecraft 1.20.1 and published that line as
/// `net.neoforged:forge`, with Forge's own `47.1.x` numbering. Its successor
/// artifact `net.neoforged:neoforge` starts at `20.2.x`, Minecraft 1.20.2.
/// Answering from the release rather than from an empty catalog answer is what
/// lets a frontend decline to offer the loader for anything older, instead of
/// offering it and then leaving the user in front of a build list that will
/// never fill in.
pub const NEOFORGE_MINIMUM_MINECRAFT: &str = "1.20.1";

/// The one Minecraft release whose NeoForge builds are published as
/// `net.neoforged:forge:<minecraft>-<build>`, in Forge's layout, rather than as
/// `net.neoforged:neoforge:<build>`.
const NEOFORGE_FORGE_ARTIFACT_MINECRAFT: &str = "1.20.1";

/// The build-number major NeoForge's `net.neoforged:forge` line carries, shared
/// with the Forge line it forked from.
const NEOFORGE_FORGE_ARTIFACT_MAJOR: u32 = 47;

/// Whether NeoForge builds for a release are published under the
/// `net.neoforged:forge` artifact, which names its builds, installer, and
/// server files the way Forge does.
#[must_use]
pub(crate) fn neoforge_uses_forge_artifact(minecraft: &str) -> bool {
    minecraft == NEOFORGE_FORGE_ARTIFACT_MINECRAFT
}

/// The oldest Minecraft release a loader was ever published for, where it has
/// one. `None` means the loader has always been available.
#[must_use]
pub const fn minimum_minecraft(loader: LoaderKind) -> Option<&'static str> {
    match loader {
        LoaderKind::Vanilla | LoaderKind::Forge => None,
        LoaderKind::NeoForge => Some(NEOFORGE_MINIMUM_MINECRAFT),
    }
}

/// Whether a loader has any build at all for a Minecraft release.
///
/// This is about the loader's own history, not about what a catalog happens to
/// serve today: it is answerable offline and never changes for a release that
/// has already shipped. A version in a scheme this build cannot parse is not
/// refused, because refusing it would hide a loader on the strength of a guess
/// about an unknown version scheme.
#[must_use]
pub fn supports_minecraft(loader: LoaderKind, minecraft: &str) -> bool {
    let Some(floor) = minimum_minecraft(loader) else {
        return true;
    };
    MinecraftVersionId::parse(minecraft)
        .at_least(&MinecraftVersionId::parse(floor))
        .unwrap_or(true)
}

/// The first Forge build for Minecraft 1.12.2 whose installer ships `version.json`
/// beside a `spec` install profile.
///
/// Forge published 1.12.2 installers in two layouts. Earlier builds carry only an
/// `install_profile.json` with `install` and `versionInfo` entries and no
/// `version.json`, which the installer engine does not read. The boundary was
/// established from the official installers of builds on either side of it;
/// builds just below it were not each inspected, so a build this cutoff
/// misjudges is still caught by the layout check made after the installer is
/// downloaded.
const FORGE_1_12_2_FIRST_MODERN_INSTALLER_BUILD: &str = "14.23.5.2851";

/// Whether the installer for an exact build is in a layout this build installs.
///
/// Answerable offline from the version alone, so a listing can leave the build
/// out and an exact selection can be refused before anything is downloaded.
#[must_use]
pub(crate) fn installer_format_supported(loader: LoaderKind, minecraft: &str, exact: &str) -> bool {
    !(loader == LoaderKind::Forge
        && minecraft == "1.12.2"
        && compare_versions(exact, FORGE_1_12_2_FIRST_MODERN_INSTALLER_BUILD) == Ordering::Less)
}

/// Refuses an exact build whose installer uses a layout that is not installed.
///
/// # Errors
///
/// [`LoaderError::UnsupportedInstallerFormat`] when
/// [`installer_format_supported`] is false for the build.
pub fn require_supported_installer_format(
    loader: LoaderKind,
    minecraft: &str,
    exact: &str,
) -> LoaderResult<()> {
    if installer_format_supported(loader, minecraft, exact) {
        return Ok(());
    }
    Err(LoaderError::UnsupportedInstallerFormat {
        loader,
        minecraft: minecraft.to_owned(),
        version: exact.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderBuild {
    pub version: String,
    pub prerelease: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderVersionListing {
    pub loader: LoaderKind,
    pub minecraft: String,
    pub builds: Vec<LoaderBuild>,
    pub latest: Option<String>,
    pub stable: Option<String>,
    pub freshness: Freshness,
}

impl LoaderVersionListing {
    pub fn resolve(&self, mode: LoaderVersionMode, exact: Option<&str>) -> LoaderResult<String> {
        let selected = match mode {
            LoaderVersionMode::Latest => self.latest.clone(),
            LoaderVersionMode::Stable => self.stable.clone(),
            LoaderVersionMode::Exact => exact.map(str::to_owned),
        };
        selected.ok_or(LoaderError::VersionUnavailable {
            loader: self.loader,
            minecraft: self.minecraft.clone(),
            mode: match mode {
                LoaderVersionMode::Latest => "latest",
                LoaderVersionMode::Stable => "stable",
                LoaderVersionMode::Exact => "exact",
            },
        })
    }
}

#[derive(Debug, Clone)]
pub struct LoaderCatalog {
    manager: DownloadManager,
    cache: MetadataCache,
    forge_promotions: Url,
    forge_maven: Url,
    neoforge_maven: Url,
    // Boxed to keep the catalog small enough not to trip the large-futures lint.
    neoforge_forge_artifact_maven: Box<Url>,
}

impl LoaderCatalog {
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> LoaderResult<Self> {
        Self::with_urls(
            manager,
            layout,
            FORGE_PROMOTIONS_URL,
            FORGE_MAVEN_METADATA_URL,
            NEOFORGE_MAVEN_METADATA_URL,
            NEOFORGE_FORGE_ARTIFACT_MAVEN_METADATA_URL,
        )
    }

    pub fn with_urls(
        manager: DownloadManager,
        layout: CacheLayout,
        forge_promotions: &str,
        forge_maven: &str,
        neoforge_maven: &str,
        neoforge_forge_artifact_maven: &str,
    ) -> LoaderResult<Self> {
        let parse = |value: &str| {
            Url::parse(value).map_err(|_| LoaderError::MetadataInvalid {
                provider: "loader",
                detail: format!("invalid provider URL `{value}`"),
            })
        };
        Ok(Self {
            manager,
            cache: MetadataCache::new(layout),
            forge_promotions: parse(forge_promotions)?,
            forge_maven: parse(forge_maven)?,
            neoforge_maven: parse(neoforge_maven)?,
            neoforge_forge_artifact_maven: Box::new(parse(neoforge_forge_artifact_maven)?),
        })
    }

    pub async fn forge(
        &self,
        minecraft: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<LoaderVersionListing> {
        let promotions_key = MetadataKey::new("forge-promotions")?;
        let maven_key = MetadataKey::new("forge-maven-metadata")?;
        let promotions = self
            .cache
            .fetch(
                &self.manager,
                &promotions_key,
                &self.forge_promotions,
                reporter,
                cancel,
            )
            .await
            .map_err(|source| LoaderError::MetadataUnavailable {
                provider: "Forge",
                source: Box::new(source),
            })?;
        let maven = self
            .cache
            .fetch_text(
                &self.manager,
                &maven_key,
                &self.forge_maven,
                reporter,
                cancel,
            )
            .await
            .map_err(|source| LoaderError::MetadataUnavailable {
                provider: "Forge",
                source: Box::new(source),
            })?;

        let freshness = combine_freshness(promotions.freshness(), maven.freshness());
        let promotions: ForgePromotions =
            promotions
                .parse()
                .map_err(|error| LoaderError::MetadataInvalid {
                    provider: "Forge",
                    detail: error.to_string(),
                })?;
        let xml = maven.text().map_err(|error| LoaderError::MetadataInvalid {
            provider: "Forge",
            detail: error.to_string(),
        })?;
        let versions = parse_maven_versions(&xml, "Forge")?;
        Ok(forge_listing(
            minecraft,
            &versions,
            &promotions.promos,
            freshness,
        ))
    }

    pub async fn neoforge(
        &self,
        minecraft: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LoaderResult<LoaderVersionListing> {
        let forge_artifact = neoforge_uses_forge_artifact(minecraft);
        let (key, url) = if forge_artifact {
            (
                MetadataKey::new("neoforge-forge-artifact-maven-metadata")?,
                &*self.neoforge_forge_artifact_maven,
            )
        } else {
            (
                MetadataKey::new("neoforge-maven-metadata")?,
                &self.neoforge_maven,
            )
        };
        let document = self
            .cache
            .fetch_text(&self.manager, &key, url, reporter, cancel)
            .await
            .map_err(|source| LoaderError::MetadataUnavailable {
                provider: "NeoForge",
                source: Box::new(source),
            })?;
        let xml = document
            .text()
            .map_err(|error| LoaderError::MetadataInvalid {
                provider: "NeoForge",
                detail: error.to_string(),
            })?;
        let versions = parse_maven_versions(&xml, "NeoForge")?;
        Ok(if forge_artifact {
            neoforge_forge_artifact_listing(minecraft, &versions, document.freshness())
        } else {
            neoforge_listing(minecraft, &versions, document.freshness())
        })
    }
}

// Kept separate so fixture tests can prove selection without constructing HTTP.
pub(crate) fn forge_listing(
    minecraft: &str,
    versions: &[String],
    promotions: &BTreeMap<String, String>,
    freshness: Freshness,
) -> LoaderVersionListing {
    let prefix = format!("{minecraft}-");
    let mut builds: Vec<_> = versions
        .iter()
        .filter_map(|published| published.strip_prefix(&prefix))
        .filter(|version| installer_format_supported(LoaderKind::Forge, minecraft, version))
        .map(|version| LoaderBuild {
            version: version.to_owned(),
            prerelease: is_prerelease(version),
        })
        .collect();
    builds.sort_by(|left, right| compare_versions(&right.version, &left.version));
    let contains = |value: &str| builds.iter().any(|build| build.version == value);
    let promoted = |channel: &str| {
        promotions
            .get(&format!("{minecraft}-{channel}"))
            .filter(|value| contains(value))
            .cloned()
    };
    LoaderVersionListing {
        loader: LoaderKind::Forge,
        minecraft: minecraft.to_owned(),
        latest: promoted("latest"),
        stable: promoted("recommended"),
        builds,
        freshness,
    }
}

pub(crate) fn neoforge_listing(
    minecraft: &str,
    versions: &[String],
    freshness: Freshness,
) -> LoaderVersionListing {
    let builds = versions
        .iter()
        .filter(|version| neoforge_minecraft(version).as_deref() == Some(minecraft))
        .cloned()
        .collect();
    neoforge_build_listing(minecraft, builds, freshness)
}

/// The builds of NeoForge's `net.neoforged:forge` artifact, whose published
/// versions are `<minecraft>-<build>` as Forge's are.
pub(crate) fn neoforge_forge_artifact_listing(
    minecraft: &str,
    versions: &[String],
    freshness: Freshness,
) -> LoaderVersionListing {
    let prefix = format!("{minecraft}-");
    let builds = versions
        .iter()
        .filter_map(|published| published.strip_prefix(&prefix))
        .map(str::to_owned)
        .collect();
    neoforge_build_listing(minecraft, builds, freshness)
}

fn neoforge_build_listing(
    minecraft: &str,
    versions: Vec<String>,
    freshness: Freshness,
) -> LoaderVersionListing {
    let mut builds: Vec<_> = versions
        .into_iter()
        .map(|version| LoaderBuild {
            prerelease: is_prerelease(&version),
            version,
        })
        .collect();
    builds.sort_by(|left, right| compare_versions(&right.version, &left.version));
    let latest = builds.first().map(|build| build.version.clone());
    let stable = builds
        .iter()
        .find(|build| !build.prerelease)
        .map(|build| build.version.clone());
    LoaderVersionListing {
        loader: LoaderKind::NeoForge,
        minecraft: minecraft.to_owned(),
        builds,
        latest,
        stable,
        freshness,
    }
}

/// The Minecraft release a NeoForge build targets, or `None` when the build
/// targets no release (a snapshot, pre-release, or release candidate) or its
/// version is not in a known scheme.
///
/// Builds below 26 are `MAJOR.MINOR.PATCH` for Minecraft `1.MAJOR.MINOR`, where
/// a minor of zero names the release without a patch (`21.0.x` is `1.21`). The
/// exception is the `47.x` line of `net.neoforged:forge`, which kept Forge's
/// numbering and targets Minecraft 1.20.1.
/// Builds from 26 on are `YEAR.DROP.PATCH.BUILD` for Minecraft `YEAR.DROP`, with
/// the patch kept only when it is not zero. A `+snapshot-N`, `+pre-N`, or
/// `+rc-N` suffix names the pre-release Minecraft id the installer declares, so
/// such a build never belongs to the release its numbers precede.
#[must_use]
pub fn neoforge_minecraft(version: &str) -> Option<String> {
    let (base, metadata) = version
        .split_once('+')
        .map_or((version, None), |(base, metadata)| (base, Some(metadata)));
    if metadata.is_some_and(targets_unreleased_minecraft) {
        return None;
    }
    let numeric = base.split('-').next()?;
    let parts: Vec<u32> = numeric
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() < 2 {
        return None;
    }
    if parts[0] == NEOFORGE_FORGE_ARTIFACT_MAJOR {
        Some(NEOFORGE_FORGE_ARTIFACT_MINECRAFT.to_owned())
    } else if parts[0] >= 26 {
        let patch = parts.get(2).copied().unwrap_or(0);
        Some(if patch == 0 {
            format!("{}.{}", parts[0], parts[1])
        } else {
            format!("{}.{}.{}", parts[0], parts[1], patch)
        })
    } else if parts[1] == 0 {
        Some(format!("1.{}", parts[0]))
    } else {
        Some(format!("1.{}.{}", parts[0], parts[1]))
    }
}

fn targets_unreleased_minecraft(metadata: &str) -> bool {
    let lower = metadata.to_ascii_lowercase();
    ["snapshot", "pre", "rc"].iter().any(|marker| {
        lower
            .strip_prefix(marker)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '.']))
    })
}

fn is_prerelease(version: &str) -> bool {
    let lower = version.to_ascii_lowercase();
    ["alpha", "beta", "-rc", "-pre", "snapshot"]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let tokens = |value: &str| {
        value
            .split(['.', '-', '_', '+'])
            .map(|token| {
                token.parse::<u64>().map_or_else(
                    |_| (1, 0, token.to_ascii_lowercase()),
                    |number| (0, number, String::new()),
                )
            })
            .collect::<Vec<_>>()
    };
    tokens(left).cmp(&tokens(right))
}

fn combine_freshness(left: Freshness, right: Freshness) -> Freshness {
    if left == Freshness::Stale || right == Freshness::Stale {
        Freshness::Stale
    } else if left == Freshness::Fetched || right == Freshness::Fetched {
        Freshness::Fetched
    } else {
        Freshness::Cached
    }
}

#[derive(Debug, Deserialize)]
struct ForgePromotions {
    promos: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct MavenMetadata {
    versioning: MavenVersioning,
}

#[derive(Debug, Deserialize)]
struct MavenVersioning {
    versions: MavenVersions,
}

#[derive(Debug, Deserialize)]
struct MavenVersions {
    #[serde(rename = "version", default)]
    values: Vec<String>,
}

fn parse_maven_versions(xml: &str, provider: &'static str) -> LoaderResult<Vec<String>> {
    let metadata: MavenMetadata = from_str(xml).map_err(|error| LoaderError::MetadataInvalid {
        provider,
        detail: error.to_string(),
    })?;
    Ok(metadata.versioning.versions.values)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;

    fn official_versions(path: &str) -> Vec<String> {
        parse_maven_versions(path, "fixture").expect("official Maven metadata fixture")
    }

    #[test]
    fn neoforge_is_offered_only_where_it_was_ever_published() {
        for version in ["1.20.1", "1.20.2", "1.20.4", "1.21.1", "26.1.2", "26.2"] {
            assert!(
                supports_minecraft(LoaderKind::NeoForge, version),
                "{version} has NeoForge builds"
            );
        }
        for version in ["1.12.2", "1.16.3", "1.18.2", "1.19.4", "1.20"] {
            assert!(
                !supports_minecraft(LoaderKind::NeoForge, version),
                "{version} predates NeoForge"
            );
            assert!(supports_minecraft(LoaderKind::Forge, version));
            assert!(supports_minecraft(LoaderKind::Vanilla, version));
        }
    }

    #[test]
    fn neoforge_builds_for_minecraft_1_20_1_are_listed_from_the_forge_named_artifact() {
        let versions = official_versions(include_str!(
            "../../../fixtures/loaders/neoforge-forge-artifact-maven-metadata.xml"
        ));
        assert!(neoforge_uses_forge_artifact("1.20.1"));
        assert!(!neoforge_uses_forge_artifact("1.20.2"));
        let listed = neoforge_forge_artifact_listing("1.20.1", &versions, Freshness::Cached);
        let listed_versions: Vec<_> = listed.builds.iter().map(|b| b.version.as_str()).collect();
        assert_eq!(listed_versions, ["47.1.106", "47.1.105", "47.1.5"]);
        assert_eq!(listed.latest.as_deref(), Some("47.1.106"));
        assert_eq!(listed.stable.as_deref(), Some("47.1.106"));
        assert!(
            neoforge_forge_artifact_listing("1.20.2", &versions, Freshness::Cached)
                .builds
                .is_empty()
        );
    }

    #[test]
    fn a_build_in_the_forge_numbered_line_targets_minecraft_1_20_1() {
        assert_eq!(neoforge_minecraft("47.1.106").as_deref(), Some("1.20.1"));
        assert_eq!(neoforge_minecraft("20.2.86").as_deref(), Some("1.20.2"));
    }

    #[test]
    fn a_version_scheme_this_build_cannot_parse_hides_nothing() {
        // Hiding a loader on the strength of a guess about an unknown scheme
        // would be worse than offering one whose build list turns out empty.
        assert!(supports_minecraft(
            LoaderKind::NeoForge,
            "some-future-scheme"
        ));
    }

    #[test]
    fn official_forge_fixtures_select_recommended_as_stable() {
        let versions = official_versions(include_str!(
            "../../../fixtures/loaders/forge-maven-metadata.xml"
        ));
        let promotions: ForgePromotions = serde_json::from_str(include_str!(
            "../../../fixtures/loaders/forge-promotions.json"
        ))
        .expect("official promotions fixture");
        let listed = forge_listing("1.20.1", &versions, &promotions.promos, Freshness::Cached);
        assert_eq!(listed.latest.as_deref(), Some("47.4.22"));
        assert_eq!(listed.stable.as_deref(), Some("47.4.10"));
    }

    #[test]
    fn official_neoforge_fixture_keeps_beta_latest_but_not_stable() {
        let versions = official_versions(include_str!(
            "../../../fixtures/loaders/neoforge-maven-metadata.xml"
        ));
        let listed = neoforge_listing("26.1.2", &versions, Freshness::Cached);
        assert_eq!(listed.latest.as_deref(), Some("26.1.2.95-beta"));
        assert_eq!(listed.stable.as_deref(), Some("26.1.2.94"));
    }

    #[test]
    fn forge_stable_is_recommended_and_never_latest_by_fallback() {
        let versions = vec![
            "1.20.1-47.4.22".to_owned(),
            "1.20.1-47.4.10".to_owned(),
            "1.12.2-14.23.5.2859".to_owned(),
        ];
        let promotions = BTreeMap::from([
            ("1.20.1-latest".to_owned(), "47.4.22".to_owned()),
            ("1.20.1-recommended".to_owned(), "47.4.10".to_owned()),
            ("1.12.2-latest".to_owned(), "14.23.5.2859".to_owned()),
        ]);
        let listed = forge_listing("1.20.1", &versions, &promotions, Freshness::Fetched);
        assert_eq!(listed.latest.as_deref(), Some("47.4.22"));
        assert_eq!(listed.stable.as_deref(), Some("47.4.10"));
        let no_recommended = forge_listing("1.12.2", &versions, &promotions, Freshness::Fetched);
        assert_eq!(no_recommended.stable, None);
    }

    #[test]
    fn forge_1_12_2_builds_with_the_older_installer_layout_are_not_listed() {
        let versions: Vec<String> = [
            "1.12.2-14.23.0.2486",
            "1.12.2-14.23.5.2847",
            "1.12.2-14.23.5.2851",
            "1.12.2-14.23.5.2859",
            "1.12.2-14.23.5.2864",
            "1.16.5-36.2.0",
        ]
        .map(str::to_owned)
        .to_vec();
        let promotions = BTreeMap::from([
            ("1.12.2-latest".to_owned(), "14.23.5.2864".to_owned()),
            ("1.12.2-recommended".to_owned(), "14.23.5.2859".to_owned()),
        ]);

        let listed = forge_listing("1.12.2", &versions, &promotions, Freshness::Fetched);

        let offered: Vec<_> = listed.builds.iter().map(|b| b.version.as_str()).collect();
        assert_eq!(
            offered,
            ["14.23.5.2864", "14.23.5.2859", "14.23.5.2851"],
            "builds below the cutoff are absent and the rest keep newest-first order"
        );
        assert_eq!(listed.latest.as_deref(), Some("14.23.5.2864"));
        assert_eq!(listed.stable.as_deref(), Some("14.23.5.2859"));
    }

    #[test]
    fn a_promotion_pointing_at_an_unsupported_installer_layout_is_not_offered() {
        let versions = vec![
            "1.12.2-14.23.5.2847".to_owned(),
            "1.12.2-14.23.5.2859".to_owned(),
        ];
        let promotions =
            BTreeMap::from([("1.12.2-recommended".to_owned(), "14.23.5.2847".to_owned())]);

        let listed = forge_listing("1.12.2", &versions, &promotions, Freshness::Fetched);

        assert_eq!(listed.stable, None);
    }

    #[test]
    fn the_installer_layout_cutoff_applies_only_to_forge_for_1_12_2() {
        assert!(!installer_format_supported(
            LoaderKind::Forge,
            "1.12.2",
            "14.23.5.2850"
        ));
        assert!(installer_format_supported(
            LoaderKind::Forge,
            "1.12.2",
            "14.23.5.2851"
        ));
        // 36.2.0 is numerically below the 1.12.2 cutoff and is unaffected by it.
        assert!(installer_format_supported(
            LoaderKind::Forge,
            "1.16.5",
            "36.2.0"
        ));
        assert!(installer_format_supported(
            LoaderKind::NeoForge,
            "1.12.2",
            "1.0.0"
        ));
    }

    #[test]
    fn an_exact_build_with_the_older_installer_layout_is_refused_by_name() {
        let error = require_supported_installer_format(LoaderKind::Forge, "1.12.2", "14.23.5.2847")
            .expect_err("older layout");
        assert!(matches!(
            &error,
            LoaderError::UnsupportedInstallerFormat { version, .. } if version == "14.23.5.2847"
        ));
        assert_eq!(
            error.message_id(),
            "error-loader-installer-format-unsupported"
        );
        assert!(
            require_supported_installer_format(LoaderKind::Forge, "1.12.2", "14.23.5.2859").is_ok()
        );
    }

    #[test]
    fn neoforge_filters_both_numbering_eras_and_excludes_beta_from_stable() {
        let versions = vec![
            "21.1.200".to_owned(),
            "26.1.2.94".to_owned(),
            "26.1.2.95-beta".to_owned(),
            "26.2.0.1".to_owned(),
        ];
        let listed = neoforge_listing("26.1.2", &versions, Freshness::Cached);
        assert_eq!(listed.latest.as_deref(), Some("26.1.2.95-beta"));
        assert_eq!(listed.stable.as_deref(), Some("26.1.2.94"));
        assert_eq!(neoforge_minecraft("21.1.200").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_minecraft("26.2.0.1").as_deref(), Some("26.2"));
    }

    #[test]
    fn neoforge_zero_minor_builds_belong_to_the_release_without_a_patch() {
        let versions = vec![
            "20.2.86".to_owned(),
            "21.0.166".to_owned(),
            "21.0.167".to_owned(),
            "21.0.168-beta".to_owned(),
            "21.1.0".to_owned(),
            "21.1.200".to_owned(),
        ];
        let listed = neoforge_listing("1.21", &versions, Freshness::Cached);
        let listed_versions: Vec<_> = listed.builds.iter().map(|b| b.version.as_str()).collect();
        assert_eq!(listed_versions, ["21.0.168-beta", "21.0.167", "21.0.166"]);
        assert_eq!(listed.latest.as_deref(), Some("21.0.168-beta"));
        assert_eq!(listed.stable.as_deref(), Some("21.0.167"));
        assert!(
            neoforge_listing("1.21.0", &versions, Freshness::Cached)
                .builds
                .is_empty()
        );
        assert_eq!(neoforge_minecraft("20.2.86").as_deref(), Some("1.20.2"));
        assert_eq!(neoforge_minecraft("21.1.0").as_deref(), Some("1.21.1"));
        assert_eq!(neoforge_minecraft("26.1.2.94").as_deref(), Some("26.1.2"));
    }

    #[test]
    fn neoforge_snapshot_and_pre_release_builds_are_not_listed_for_a_release() {
        let versions = vec![
            "26.1.0.0-alpha.14+snapshot-11".to_owned(),
            "26.1.0.0-alpha.15+pre-3".to_owned(),
            "26.1.0.0-beta+rc-1".to_owned(),
            "26.1.0.1".to_owned(),
            "26.1.0.2-beta".to_owned(),
        ];
        let listed = neoforge_listing("26.1", &versions, Freshness::Cached);
        let listed_versions: Vec<_> = listed.builds.iter().map(|b| b.version.as_str()).collect();
        assert_eq!(listed_versions, ["26.1.0.2-beta", "26.1.0.1"]);
        assert_eq!(listed.stable.as_deref(), Some("26.1.0.1"));
        for version in &versions[..3] {
            assert_eq!(neoforge_minecraft(version), None, "{version}");
        }
    }

    #[test]
    fn maven_xml_is_parsed_as_text_not_json() {
        let xml = r"<metadata><versioning><versions><version>1.0</version><version>2.0-beta</version></versions></versioning></metadata>";
        assert_eq!(
            parse_maven_versions(xml, "test").expect("xml"),
            ["1.0", "2.0-beta"]
        );
    }
}
