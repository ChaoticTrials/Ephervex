//! Fetching, caching, and resolving Mojang metadata.
//!
//! # Two documents, two caching strategies
//!
//! The version manifest is **mutable**: a new release changes it, and its
//! content has no name of its own. It therefore goes through
//! [`MetadataCache`], with a fetch timestamp and a TTL, and it degrades to a
//! stale copy when Mojang is unreachable.
//!
//! A per-version JSON is **immutable and named by its own digest**: the
//! manifest publishes the SHA-1 of each one, and Mojang has never changed a
//! published version document. So it is written straight to a
//! content-addressed path under the metadata store and verified against that
//! digest on the way in. There is no TTL because there is nothing to expire,
//! and a second resolve of the same version does no network I/O at all - the
//! download manager's own "already at the destination" check short-circuits it,
//! which it may do safely because a partial file can never appear at a cache
//! path.
//!
//! That asymmetry is the point: refreshing the manifest is cheap and
//! occasionally necessary; re-fetching a 40 kB version document that cannot
//! have changed is neither.
//!
//! # Everything goes through the download manager
//!
//! There is no second HTTP path here - no bare `reqwest` call, no separate
//! client. Retry policy, redirect bounds, the `User-Agent`, atomic writes, and
//! hash verification are all properties of `ephervex-download`, and a metadata
//! fetch that bypassed it would quietly opt out of every one of them
//!
//! # Destinations never come from the URL
//!
//! The manifest is remote input. Its `url` fields decide *what* is fetched;
//! [`CacheLayout`] decides *where* it lands - from the digest, never from a URL
//! segment or a filename.

use chrono::{DateTime, Utc};
use ephervex_download::{
    CacheLayout, ContentStore, DownloadError, DownloadManager, DownloadRequest, FileExtension,
    Freshness, MetadataCache, MetadataKey, OperationReporter, UrlPolicyError,
};
use reqwest::Url;
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::error::{MinecraftError, MinecraftResult};
use crate::manifest::{
    LatestVersions, VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL, VersionManifest,
    VersionRecord,
};
use crate::version::VersionMetadata;
use crate::version_id::MINIMUM_SUPPORTED_VERSION;

/// A manifest, plus how much the caller should trust its age.
#[derive(Debug, Clone)]
pub struct CachedManifest {
    pub manifest: VersionManifest,
    pub freshness: Freshness,
    pub fetched_at: DateTime<Utc>,
}

/// The supported releases, with the provenance of the list they came from.
///
/// `freshness` is deliberately part of the result rather than something a
/// caller has to ask for separately: a list that is three days old because
/// Mojang is down is still worth showing, but only if the user is told
#[derive(Debug, Clone)]
pub struct ReleaseListing {
    /// Supported releases, newest first, in Mojang's own manifest order.
    pub releases: Vec<VersionRecord>,
    /// What Mojang calls the newest release and snapshot.
    pub latest: LatestVersions,
    pub freshness: Freshness,
    pub fetched_at: DateTime<Utc>,
    /// How many manifest entries were considered before filtering, so a caller
    /// can say "5 of 905" rather than implying Minecraft has five versions.
    pub total_versions: usize,
}

impl ReleaseListing {
    /// Whether the list is being served because a refresh failed.
    #[must_use]
    pub const fn is_stale(&self) -> bool {
        self.freshness.is_stale()
    }

    /// Releases whose ids this build cannot rank, and which are therefore
    /// offered on the strength of their release date alone.
    ///
    /// Surfacing them is the point: an id in an unknown scheme is a signal that
    /// this build's parser has fallen behind Mojang, and silently absorbing it
    /// is how that goes unnoticed.
    pub fn unrecognized_version_ids(&self) -> impl Iterator<Item = &VersionRecord> {
        self.releases
            .iter()
            .filter(|record| !record.has_recognized_version_id())
    }
}

/// Reads Mojang's version metadata.
#[derive(Debug, Clone)]
pub struct VersionCatalog {
    manager: DownloadManager,
    layout: CacheLayout,
    cache: MetadataCache,
    manifest_url: Url,
    manifest_key: MetadataKey,
    /// Suffix given to cached version documents so the cache is legible to
    /// someone browsing it. Validated once, here, rather than at each use.
    json: FileExtension,
}

impl VersionCatalog {
    /// A catalog reading Mojang's production manifest.
    pub fn mojang(manager: DownloadManager, layout: CacheLayout) -> MinecraftResult<Self> {
        Self::new(manager, layout, VERSION_MANIFEST_URL)
    }

    /// A catalog reading a manifest from an explicit URL.
    ///
    /// Exists for the offline tests, which point it at a loopback server. It is
    /// not a configuration knob: the manifest URL is Mojang's, and letting a
    /// user redirect it would make every downstream digest meaningless.
    pub fn new(
        manager: DownloadManager,
        layout: CacheLayout,
        manifest_url: &str,
    ) -> MinecraftResult<Self> {
        let manifest_url = Url::parse(manifest_url).map_err(|_| UrlPolicyError::Malformed {
            url: manifest_url.to_owned(),
        })?;
        Ok(Self {
            cache: MetadataCache::new(layout.clone()),
            manifest_key: MetadataKey::new(VERSION_MANIFEST_CACHE_KEY)?,
            json: FileExtension::new("json")?,
            manager,
            layout,
            manifest_url,
        })
    }

    /// Replaces the metadata cache, e.g. to shorten the manifest TTL.
    #[must_use]
    pub fn with_cache(mut self, cache: MetadataCache) -> Self {
        self.cache = cache;
        self
    }

    /// The version manifest, refreshed only if what is cached has expired.
    ///
    /// A failed refresh with a cached copy available returns that copy marked
    /// [`Freshness::Stale`]; a failed refresh with nothing cached is
    /// [`MinecraftError::ManifestUnavailable`].
    #[instrument(skip_all)]
    pub async fn manifest(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<CachedManifest> {
        let document = self
            .cache
            .fetch(
                &self.manager,
                &self.manifest_key,
                &self.manifest_url,
                reporter,
                cancel,
            )
            .await
            .map_err(|source| MinecraftError::ManifestUnavailable { source })?;

        let manifest = VersionManifest::parse(document.json())
            .map_err(|source| MinecraftError::ManifestInvalid { source })?;
        debug!(
            versions = manifest.versions.len(),
            freshness = ?document.freshness(),
            "read the mojang version manifest"
        );
        Ok(CachedManifest {
            manifest,
            freshness: document.freshness(),
            fetched_at: document.fetched_at(),
        })
    }

    /// The releases Ephervex supports: `type == "release"` at or above 1.12.2
    #[instrument(skip_all)]
    pub async fn supported_releases(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<ReleaseListing> {
        let cached = self.manifest(reporter, cancel).await?;
        let releases: Vec<VersionRecord> = cached
            .manifest
            .supported_releases()
            .into_iter()
            .cloned()
            .collect();
        debug!(
            offered = releases.len(),
            total = cached.manifest.versions.len(),
            minimum = MINIMUM_SUPPORTED_VERSION,
            "filtered the manifest to supported releases"
        );
        Ok(ReleaseListing {
            releases,
            latest: cached.manifest.latest,
            freshness: cached.freshness,
            fetched_at: cached.fetched_at,
            total_versions: cached.manifest.versions.len(),
        })
    }

    /// One release's complete metadata, from what is already cached.
    ///
    /// Answers `None` rather than reaching for anything. That is the whole
    /// point: it lets a caller that has promised not to touch the network ask
    /// what a version needs, and say "not cached" when nobody has asked yet,
    /// instead of quietly becoming an operation that needs a connection.
    ///
    /// The version document is addressed by its own digest, so a cached one is
    /// read straight off disk and a tampered one is not read at all.
    #[instrument(skip(self))]
    pub async fn cached_metadata(&self, id: &str) -> MinecraftResult<Option<VersionMetadata>> {
        let Some(document) = self.cache.peek(&self.manifest_key).await else {
            return Ok(None);
        };
        let manifest = VersionManifest::parse(document.json())
            .map_err(|source| MinecraftError::ManifestInvalid { source })?;
        let Some(record) = manifest.version(id) else {
            return Ok(None);
        };

        // The digest names the file, exactly as it does when one is fetched,
        // so this reads the same bytes a resolve would have verified.
        let destination =
            self.layout
                .content(ContentStore::Metadata, &record.sha1, Some(&self.json));
        let Ok(json) = tokio::fs::read_to_string(&destination).await else {
            return Ok(None);
        };
        Ok(Some(VersionMetadata::parse(&json).map_err(|_| {
            MinecraftError::VersionMetadataCorrupt {
                id: record.id.clone(),
            }
        })?))
    }

    /// Resolves one supported release's complete metadata.
    ///
    /// Refuses an id that is not a release or that predates the floor, rather
    /// than resolving it anyway: offering metadata for a version the launcher
    /// will not run is a way to promise something it cannot keep
    #[instrument(skip(self, reporter, cancel))]
    pub async fn resolve(
        &self,
        id: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<VersionMetadata> {
        let cached = self.manifest(reporter, cancel).await?;
        let record = cached
            .manifest
            .version(id)
            .ok_or_else(|| MinecraftError::UnknownVersion { id: id.to_owned() })?;
        if !record.release_type.is_release() {
            return Err(MinecraftError::not_a_release(id, &record.release_type));
        }
        if cached.manifest.support_basis(record).is_none() {
            return Err(MinecraftError::unsupported(id));
        }
        self.resolve_record(record, reporter, cancel).await
    }

    /// Resolves a manifest entry the caller already holds, skipping the support
    /// check.
    ///
    /// loader support needs this: a loader's `inheritsFrom` names a parent that has
    /// already been vetted, and re-running the filter on it would only add a
    /// way for the two paths to disagree.
    #[instrument(skip(self, record, reporter, cancel), fields(version = %record.id))]
    pub async fn resolve_record(
        &self,
        record: &VersionRecord,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> MinecraftResult<VersionMetadata> {
        let url = Url::parse(&record.url).map_err(|_| UrlPolicyError::Malformed {
            url: record.url.clone(),
        })?;
        // The digest names the file; the manifest's URL does not get a say in
        // where it lands.
        let destination =
            self.layout
                .content(ContentStore::Metadata, &record.sha1, Some(&self.json));

        let request = DownloadRequest::new(url, &destination)
            .verifying(record.sha1.clone())
            .labeled(record.id.clone());
        let outcome = self
            .manager
            .download(&request, reporter, cancel)
            .await
            .map_err(|source| match source {
                // A mismatch here is not a flaky network. Either the manifest
                // and the document disagree or something rewrote the response,
                // and both deserve to be said out loud.
                DownloadError::HashMismatch { .. } => MinecraftError::VersionMetadataCorrupt {
                    id: record.id.clone(),
                },
                other => MinecraftError::VersionMetadataUnavailable {
                    id: record.id.clone(),
                    source: Box::new(other),
                },
            })?;
        debug!(source = ?outcome.source, path = %destination.display(), "have the version document");

        let json = fs::read_to_string(&destination).await.map_err(|source| {
            MinecraftError::VersionMetadataUnreadable {
                path: destination.clone(),
                source,
            }
        })?;
        VersionMetadata::parse(&json).map_err(|source| MinecraftError::VersionMetadataInvalid {
            id: record.id.clone(),
            source,
        })
    }
}
