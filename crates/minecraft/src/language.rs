//! Per-release language catalogs, from Mojang's own metadata.
//!
//! # Where the list actually lives
//!
//! A client JAR embeds exactly one translation: `en_us`. The catalog the game's
//! own language screen shows is built from the `language` section of the
//! resource pack metadata - the `pack.mcmeta` object named by that release's
//! asset index. Each entry carries a native name, a region, and a
//! bidirectional flag, which is precisely what a launcher needs to show
//! "Deutschland - Deutsch" while storing `de_de`.
//!
//! # Why this is not application-release data
//!
//! The list changes when Mojang ships a translation, not when Ephervex ships a
//! build, and it differs per release: 1.12.2 has 107 codes and 26.2 has 143.
//! Hard-coding it would mean a launcher update to add a language, and it would
//! mean claiming support for a code an older release never had. So it is
//! fetched from the release's own official metadata and cached per release,
//! alongside the version manifest, with the same freshness and stale-fallback
//! behavior.
//!
//! # What "not available" means
//!
//! Offline with nothing cached, the honest answer is "unknown", and the caller
//! must not write a language into that release's `options.txt`. Inventing
//! support would set a code the game silently falls back from, which looks
//! exactly like Ephervex having done nothing - except that the file now claims
//! otherwise.

use std::collections::BTreeMap;

use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, DownloadRequest, MetadataCache, MetadataKey,
    OperationReporter,
};
use ephervex_model::Hash;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::assets::AssetIndex;
use crate::catalog::VersionCatalog;
use crate::error::{MinecraftError, MinecraftResult};
use crate::install::ASSET_RESOURCE_BASE;

/// The asset the `language` section is read from.
///
/// Mojang's own resource-pack metadata file, named by the asset index like any
/// other object.
pub const LANGUAGE_METADATA_OBJECT: &str = "pack.mcmeta";

/// Prefix for the per-release cache entries.
const CACHE_KEY_PREFIX: &str = "mojang-language-catalog";

/// One language a release offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageEntry {
    /// The stable code stored in `options.txt`, e.g. `de_de`.
    pub code: String,
    /// The language's name in itself, as the game's own screen shows it.
    pub name: String,
    /// The region's name in that language.
    pub region: String,
    /// Whether the script runs right to left, so a frontend can lay the label
    /// out correctly instead of guessing from the code.
    #[serde(default)]
    pub bidirectional: bool,
}

/// One release's complete language catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageCatalog {
    pub minecraft_version: String,
    /// Mojang's id for the asset index the catalog came from, so two releases
    /// sharing an index are visibly the same catalog.
    pub asset_index_id: String,
    /// Every language, ordered by code.
    pub languages: Vec<LanguageEntry>,
}

impl LanguageCatalog {
    /// Whether this release offers a language code.
    #[must_use]
    pub fn contains(&self, code: &str) -> bool {
        let wanted = code.trim().to_ascii_lowercase();
        self.languages.iter().any(|entry| entry.code == wanted)
    }

    /// One language's full description, for a label.
    #[must_use]
    pub fn entry(&self, code: &str) -> Option<&LanguageEntry> {
        let wanted = code.trim().to_ascii_lowercase();
        self.languages.iter().find(|entry| entry.code == wanted)
    }

    /// The codes this release offers, in order.
    pub fn codes(&self) -> impl ExactSizeIterator<Item = &str> {
        self.languages.iter().map(|entry| entry.code.as_str())
    }

    /// Whether the catalog is usable as an answer at all.
    ///
    /// A catalog with no `en_us` is not a Minecraft language catalog: every
    /// release ships the built-in English translation, so its absence means the
    /// document was something else.
    fn is_plausible(&self) -> bool {
        !self.languages.is_empty() && self.contains("en_us")
    }
}

/// Reads and caches per-release language catalogs.
#[derive(Debug, Clone)]
pub struct LanguageCatalogs {
    catalog: VersionCatalog,
    manager: DownloadManager,
    layout: CacheLayout,
    cache: MetadataCache,
    assets_base: String,
}

impl LanguageCatalogs {
    /// Builds a resolver on top of the version catalog that already knows how
    /// to fetch and verify Mojang metadata.
    #[must_use]
    pub fn new(catalog: VersionCatalog, manager: DownloadManager, layout: CacheLayout) -> Self {
        Self {
            cache: MetadataCache::new(layout.clone()),
            catalog,
            manager,
            layout,
            assets_base: ASSET_RESOURCE_BASE.to_owned(),
        }
    }

    /// Points the asset endpoint somewhere else, for the offline tests.
    ///
    /// Not a user setting: redirecting it would make every asset digest
    /// meaningless.
    #[must_use]
    pub fn with_assets_base(mut self, base: &str) -> Self {
        base.trim_end_matches('/').clone_into(&mut self.assets_base);
        self
    }

    /// Replaces the metadata cache, e.g. to shorten the refresh interval.
    #[must_use]
    pub fn with_cache(mut self, cache: MetadataCache) -> Self {
        self.cache = cache;
        self
    }

    /// The cached catalog for one release, however old, without any network.
    ///
    /// This is the read the preference application uses: it must never block on
    /// a fetch, and "nothing cached" is a perfectly good answer that simply
    /// leaves the language unverified.
    pub async fn cached(&self, minecraft_version: &str) -> Option<LanguageCatalog> {
        let key = Self::key(minecraft_version).ok()?;
        let entry = self.cache.peek(&key).await?;
        let catalog: LanguageCatalog = entry
            .parse()
            .inspect_err(|error| {
                debug!(
                    version = minecraft_version,
                    %error,
                    "discarding an unreadable cached language catalog"
                );
            })
            .ok()?;
        catalog.is_plausible().then_some(catalog)
    }

    /// Resolves one release's catalog, using the cache when it is still fresh.
    ///
    /// Validation happens before the cache is written, so a truncated or
    /// unrecognizable `pack.mcmeta` cannot replace a catalog that worked.
    #[instrument(skip(self, reporter, cancel), fields(version = minecraft_version))]
    pub async fn resolve(
        &self,
        minecraft_version: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<LanguageCatalog> {
        let key = Self::key(minecraft_version)?;
        if let Some(entry) = self.cache.peek(&key).await
            && !entry.freshness().is_stale()
            && let Ok(catalog) = entry.parse::<LanguageCatalog>()
            && catalog.is_plausible()
        {
            debug!(
                version = minecraft_version,
                "serving a cached language catalog"
            );
            return Ok(catalog);
        }

        let metadata = self
            .catalog
            .resolve(minecraft_version, reporter, cancel)
            .await?;
        let index_ref = metadata.asset_index()?;
        let index_url = Url::parse(&index_ref.artifact.url).map_err(|_| {
            ephervex_download::UrlPolicyError::Malformed {
                url: index_ref.artifact.url.clone(),
            }
        })?;
        let index = self
            .read_asset_index(
                &index_ref.id,
                &index_url,
                &index_ref.artifact,
                reporter,
                cancel,
            )
            .await?;

        let object = index.objects.get(LANGUAGE_METADATA_OBJECT).ok_or_else(|| {
            MinecraftError::LanguageCatalogUnavailable {
                id: minecraft_version.to_owned(),
            }
        })?;
        let document = self
            .read_asset_object(&object.hash, reporter, cancel)
            .await?;

        let catalog =
            parse_catalog(minecraft_version, &index_ref.id, &document).ok_or_else(|| {
                MinecraftError::LanguageCatalogUnavailable {
                    id: minecraft_version.to_owned(),
                }
            })?;

        // Caching is best effort: the caller already holds the catalog, and the
        // only cost of not saving it is resolving it again next time. Losing
        // the answer over a full disk would be the worse trade.
        if let Some(payload) = serde_json::to_string(&catalog)
            .ok()
            .and_then(|text| serde_json::value::RawValue::from_string(text).ok())
        {
            if let Err(error) = self.cache.store(&key, &index_url, payload).await {
                debug!(version = minecraft_version, %error, "could not cache the language catalog");
            }
        } else {
            debug!(
                version = minecraft_version,
                "could not encode the language catalog"
            );
        }

        debug!(
            version = minecraft_version,
            languages = catalog.languages.len(),
            index = index_ref.id,
            "resolved a language catalog"
        );
        Ok(catalog)
    }

    /// Fetches and verifies the asset index, then parses it.
    async fn read_asset_index(
        &self,
        id: &str,
        url: &Url,
        artifact: &crate::library::Artifact,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<AssetIndex> {
        // The digest names the file; the metadata's URL does not get a say in
        // where it lands.
        let extension = ephervex_download::FileExtension::new("json")?;
        let destination =
            self.layout
                .content(ContentStore::Metadata, &artifact.sha1, Some(&extension));
        let request = DownloadRequest::new(url.clone(), &destination)
            .verifying(artifact.sha1.clone())
            .labeled(format!("assets/{id}"));
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| MinecraftError::VersionMetadataUnavailable {
                id: id.to_owned(),
                source: Box::new(source),
            })?;

        let json = fs::read_to_string(&destination).await.map_err(|source| {
            MinecraftError::VersionMetadataUnreadable {
                path: destination.clone(),
                source,
            }
        })?;
        Ok(AssetIndex::parse(id, &json)?)
    }

    /// Fetches and verifies one asset object, then reads it back as text.
    async fn read_asset_object(
        &self,
        sha1: &Hash,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<String> {
        let digest = sha1.digest();
        let prefix = digest.get(..2).unwrap_or(digest);
        let url = Url::parse(&format!("{}/{prefix}/{digest}", self.assets_base)).map_err(|_| {
            ephervex_download::UrlPolicyError::Malformed {
                url: self.assets_base.clone(),
            }
        })?;
        let destination = self.layout.asset_object(sha1)?;
        let request = DownloadRequest::new(url, &destination)
            .verifying(sha1.clone())
            .labeled(LANGUAGE_METADATA_OBJECT);
        self.manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| MinecraftError::VersionMetadataUnavailable {
                id: LANGUAGE_METADATA_OBJECT.to_owned(),
                source: Box::new(source),
            })?;

        fs::read_to_string(&destination).await.map_err(|source| {
            MinecraftError::VersionMetadataUnreadable {
                path: destination,
                source,
            }
        })
    }

    fn key(minecraft_version: &str) -> MinecraftResult<MetadataKey> {
        // A version id is remote input, and this one becomes a filename.
        Ok(MetadataKey::new(&format!(
            "{CACHE_KEY_PREFIX}-{}",
            minecraft_version.trim()
        ))?)
    }
}

/// Reads the `language` section out of a `pack.mcmeta` document.
///
/// Returns `None` for anything that is not one: a truncated download, an HTML
/// error page, or a metadata file with no language section at all. The caller
/// turns that into "unavailable", never into "no languages".
fn parse_catalog(
    minecraft_version: &str,
    asset_index_id: &str,
    document: &str,
) -> Option<LanguageCatalog> {
    #[derive(Deserialize)]
    struct PackMetadata {
        language: Option<BTreeMap<String, RawLanguage>>,
    }

    #[derive(Deserialize)]
    struct RawLanguage {
        name: Option<String>,
        region: Option<String>,
        #[serde(default)]
        bidirectional: bool,
    }

    let parsed: PackMetadata = serde_json::from_str(document).ok()?;
    let section = parsed.language?;
    let languages: Vec<LanguageEntry> = section
        .into_iter()
        .filter(|(code, _)| is_language_code(code))
        .map(|(code, raw)| LanguageEntry {
            name: raw.name.unwrap_or_else(|| code.clone()),
            region: raw.region.unwrap_or_default(),
            bidirectional: raw.bidirectional,
            code,
        })
        .collect();

    let catalog = LanguageCatalog {
        minecraft_version: minecraft_version.to_owned(),
        asset_index_id: asset_index_id.to_owned(),
        languages,
    };
    catalog.is_plausible().then_some(catalog)
}

/// Minecraft's own code form. Also what keeps a path separator out of a value
/// that a frontend may later hand back as a preference.
fn is_language_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 32
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    const PACK_METADATA: &str = r#"{
        "pack": {"pack_format": 64, "description": "Minecraft resources"},
        "language": {
            "en_us": {"name": "English", "region": "United States", "bidirectional": false},
            "de_de": {"name": "Deutsch", "region": "Deutschland"},
            "ar_sa": {"name": "العربية", "region": "العالم العربي", "bidirectional": true}
        }
    }"#;

    fn catalog() -> LanguageCatalog {
        parse_catalog("26.2", "32", PACK_METADATA).expect("a pack.mcmeta language section")
    }

    #[test]
    fn a_catalog_carries_the_code_the_name_the_region_and_the_script_direction() {
        // A frontend needs all four: the code is what is stored, the rest is
        // what makes a usable label.
        let catalog = catalog();
        assert_eq!(catalog.minecraft_version, "26.2");
        assert_eq!(catalog.asset_index_id, "32");
        assert_eq!(
            catalog.codes().collect::<Vec<_>>(),
            ["ar_sa", "de_de", "en_us"]
        );

        let german = catalog.entry("de_de").expect("present");
        assert_eq!(german.name, "Deutsch");
        assert_eq!(german.region, "Deutschland");
        assert!(!german.bidirectional);
        assert!(catalog.entry("ar_sa").expect("present").bidirectional);
    }

    #[test]
    fn support_is_asked_per_release_rather_than_assumed() {
        let catalog = catalog();
        assert!(catalog.contains("de_de"));
        assert!(
            catalog.contains("DE_DE"),
            "codes are compared case-insensitively"
        );
        assert!(!catalog.contains("qya_aa"));
    }

    #[test]
    fn a_document_that_is_not_a_language_catalog_yields_nothing_rather_than_no_languages() {
        // "Zero languages" would read as a release that offers none, which is
        // a claim no Minecraft release has ever made.
        for document in [
            "<html>503</html>",
            r#"{"pack":{"pack_format":64}}"#,
            r#"{"language":{}}"#,
            r#"{"language":{"de_de":{"name":"Deutsch","region":"Deutschland"}}}"#,
            "",
        ] {
            assert!(
                parse_catalog("26.2", "32", document).is_none(),
                "{document:?}"
            );
        }
    }

    #[test]
    fn a_code_that_could_reach_a_path_is_dropped_rather_than_carried() {
        let hostile = r#"{"language":{
            "en_us": {"name":"English","region":"US"},
            "../../etc": {"name":"Escape","region":"Nowhere"},
            "DE_DE": {"name":"Deutsch","region":"Deutschland"}
        }}"#;
        let catalog = parse_catalog("26.2", "32", hostile).expect("has en_us");
        assert_eq!(catalog.codes().collect::<Vec<_>>(), ["en_us"]);
    }

    #[test]
    fn a_catalog_round_trips_through_the_cache_encoding() {
        let encoded = serde_json::to_string(&catalog()).expect("encodes");
        let decoded: LanguageCatalog = serde_json::from_str(&encoded).expect("decodes");
        assert_eq!(decoded, catalog());
    }
}
