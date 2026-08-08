use std::cmp::Ordering;
use std::collections::BTreeMap;

use ephervex_download::{
    CacheLayout, DownloadManager, Freshness, MetadataCache, MetadataKey, OperationReporter,
};
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
}

impl LoaderCatalog {
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> LoaderResult<Self> {
        Self::with_urls(
            manager,
            layout,
            FORGE_PROMOTIONS_URL,
            FORGE_MAVEN_METADATA_URL,
            NEOFORGE_MAVEN_METADATA_URL,
        )
    }

    pub fn with_urls(
        manager: DownloadManager,
        layout: CacheLayout,
        forge_promotions: &str,
        forge_maven: &str,
        neoforge_maven: &str,
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
        let key = MetadataKey::new("neoforge-maven-metadata")?;
        let document = self
            .cache
            .fetch_text(&self.manager, &key, &self.neoforge_maven, reporter, cancel)
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
        Ok(neoforge_listing(minecraft, &versions, document.freshness()))
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
    let mut builds: Vec<_> = versions
        .iter()
        .filter(|version| neoforge_minecraft(version).as_deref() == Some(minecraft))
        .map(|version| LoaderBuild {
            version: version.clone(),
            prerelease: is_prerelease(version),
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

fn neoforge_minecraft(version: &str) -> Option<String> {
    let numeric = version.split('-').next()?;
    let parts: Vec<u32> = numeric
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() < 2 {
        return None;
    }
    if parts[0] >= 26 {
        let patch = parts.get(2).copied().unwrap_or(0);
        Some(if patch == 0 {
            format!("{}.{}", parts[0], parts[1])
        } else {
            format!("{}.{}.{}", parts[0], parts[1], patch)
        })
    } else {
        Some(format!("1.{}.{}", parts[0], parts[1]))
    }
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
    fn maven_xml_is_parsed_as_text_not_json() {
        let xml = r"<metadata><versioning><versions><version>1.0</version><version>2.0-beta</version></versions></versioning></metadata>";
        assert_eq!(
            parse_maven_versions(xml, "test").expect("xml"),
            ["1.0", "2.0-beta"]
        );
    }
}
