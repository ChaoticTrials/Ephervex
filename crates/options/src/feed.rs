//! The remote options compatibility feed.
//!
//! # Why there is a feed at all
//!
//! A Minecraft release must not require an Ephervex release. Mojang publishes
//! 26.3, the manifest refresh finds it, the launcher installs and launches it -
//! and the only thing missing is the proof of how that release stores a
//! setting. Publishing that proof is a data update, so it is one.
//!
//! # Why it is treated as hostile anyway
//!
//! The feed is maintainer-controlled, which is not the same as trustworthy: a
//! compromised host, a truncated response, or a mistake in a hand-edited
//! document would otherwise reach a file Minecraft reads. So the whole document
//! is parsed and validated *before* anything is kept, and only a wholly valid
//! one is written into the cache. The last known-good copy is never replaced by
//! something that failed.
//!
//! # Hosting
//!
//! The document is a plain JSON file on the documentation site, published from
//! the committed dataset (see [`DEFAULT_FEED_URL`]). The host allowlist is
//! derived from the feed's own URL, so reading the feed never lets remote JSON
//! name another host.
//!
//! The site is not a dependency of anything that works offline. When it is
//! unreachable, answers with something that is not a dataset, or serves an older
//! revision, the cached document or the compiled-in baseline keeps answering.
//! No credentials are involved: this is a public read-only document.

use ephervex_download::{
    Admission, CacheLayout, CachedMetadata, ConditionalStore, DomainAllowlist, DownloadManager,
    MetadataCache, MetadataCacheError, MetadataKey, OperationReporter, UrlPolicyError, redacted,
};
use reqwest::Url;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::dataset::{DatasetError, OptionsDataset};

/// The cache key the validated feed document is stored under.
pub const OPTIONS_FEED_CACHE_KEY: &str = "ephervex-minecraft-options";

/// Where the published compatibility dataset is served.
///
/// A fixed, unversioned path: the file is replaced in place whenever the
/// dataset changes, so the URL never names a revision.
pub const DEFAULT_FEED_URL: &str =
    "https://ephervex.chaotictrials.de/assets/minecraft-options.json";

/// Where the compatibility dataset is published: one document at one URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedSource {
    url: String,
}

impl Default for FeedSource {
    fn default() -> Self {
        Self::document(DEFAULT_FEED_URL)
    }
}

impl FeedSource {
    /// A document served directly at `url`.
    #[must_use]
    pub fn document(url: &str) -> Self {
        Self {
            url: url.to_owned(),
        }
    }

    /// The URL the refresh reads.
    pub fn entry_url(&self) -> Result<Url, UrlPolicyError> {
        Url::parse(&self.url).map_err(|_| UrlPolicyError::Malformed)
    }

    /// A short stable description for logs. Never a credential-bearing URL.
    #[must_use]
    pub fn describe(&self) -> String {
        describe_document(&self.url)
    }
}

/// A document URL without its userinfo, query, or fragment. A URL that does not
/// parse is not echoed, since it may still carry a signed query.
fn describe_document(raw: &str) -> String {
    Url::parse(raw).map_or_else(|_| "<unparsable url>".to_owned(), |url| redacted(&url))
}

/// Why a feed refresh did not produce a new dataset.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FeedError {
    /// The feed could not be reached or read.
    #[error("the Minecraft options feed could not be fetched")]
    Unreachable {
        #[source]
        source: Box<MetadataCacheError>,
    },
    /// The URL policy refused the feed's location.
    #[error("the Minecraft options feed named a location that is not permitted")]
    Location(#[from] UrlPolicyError),
    /// The response could not be stored as the document it was read as.
    #[error("the Minecraft options feed response is not well formed")]
    Response {
        #[source]
        source: serde_json::Error,
    },
    /// The document was fetched but is not a usable dataset.
    #[error("the published Minecraft options data was refused")]
    Invalid(#[from] DatasetError),
    /// A valid publish tried to reuse an existing revision for different data.
    #[error("the Minecraft options feed reused revision {revision} for different content")]
    RevisionCollision { revision: u32 },
    /// A valid publish is older than the last known-good cached document.
    #[error(
        "the Minecraft options feed revision {incoming} is older than cached revision {cached}"
    )]
    Rollback { incoming: u32, cached: u32 },
    /// The validated document could not be cached. The dataset is still
    /// returned; only the saving failed.
    #[error("the Minecraft options data could not be cached")]
    NotCached {
        #[source]
        source: Box<MetadataCacheError>,
    },
}

impl FeedError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        "error-options-feed-unavailable"
    }
}

/// Decides whether `incoming` may replace the cached document.
///
/// A cached entry that no longer parses counts as no entry, as in
/// [`OptionsFeed::cached`].
fn admit_revision(
    incoming: &OptionsDataset,
    cached: Option<&CachedMetadata>,
) -> Result<Admission<OptionsDataset>, FeedError> {
    let Some(cached) = cached.and_then(|entry| OptionsDataset::parse(entry.json()).ok()) else {
        return Ok(Admission::Store);
    };
    if incoming.revision < cached.revision {
        return Err(FeedError::Rollback {
            incoming: incoming.revision,
            cached: cached.revision,
        });
    }
    if incoming.revision == cached.revision {
        if *incoming == cached {
            return Ok(Admission::Keep(cached));
        }
        return Err(FeedError::RevisionCollision {
            revision: incoming.revision,
        });
    }
    Ok(Admission::Store)
}

/// Reads the published compatibility dataset.
#[derive(Debug, Clone)]
pub struct OptionsFeed {
    manager: DownloadManager,
    cache: MetadataCache,
    source: FeedSource,
    allowlist: DomainAllowlist,
    key: MetadataKey,
    refresh_lock: std::sync::Arc<Mutex<()>>,
}

impl OptionsFeed {
    /// A feed reading the default published source.
    pub fn new(manager: DownloadManager, layout: CacheLayout) -> Result<Self, UrlPolicyError> {
        Self::from_source(manager, layout, FeedSource::default())
    }

    /// A feed reading an explicit source.
    ///
    /// The offline tests point this at a loopback server; production uses
    /// [`FeedSource::default`].
    pub fn from_source(
        manager: DownloadManager,
        layout: CacheLayout,
        source: FeedSource,
    ) -> Result<Self, UrlPolicyError> {
        let entry = source.entry_url()?;
        let host = entry.host_str().ok_or(UrlPolicyError::Malformed)?;
        let allowlist = DomainAllowlist::new([host]);
        Ok(Self {
            cache: MetadataCache::new(layout),
            key: MetadataKey::new(OPTIONS_FEED_CACHE_KEY).map_err(|_| UrlPolicyError::Malformed)?,
            allowlist,
            manager,
            source,
            refresh_lock: std::sync::Arc::new(Mutex::new(())),
        })
    }

    /// Replaces the host allowlist, for the loopback integration tests.
    #[must_use]
    pub fn with_allowlist(mut self, allowlist: DomainAllowlist) -> Self {
        self.allowlist = allowlist;
        self
    }

    /// Replaces the metadata cache, e.g. to shorten the refresh interval.
    #[must_use]
    pub fn with_cache(mut self, cache: MetadataCache) -> Self {
        self.cache = cache;
        self
    }

    #[must_use]
    pub const fn source(&self) -> &FeedSource {
        &self.source
    }

    /// The last validated document, without touching the network.
    ///
    /// A cache entry that no longer parses is treated as no cache entry:
    /// discarding it costs one refresh, while trusting it could write a setting
    /// through a codec that has since been withdrawn.
    pub async fn cached(&self) -> Option<OptionsDataset> {
        let inspected = self.cache.inspect(&self.key).await?;
        match OptionsDataset::parse(inspected.entry().json()) {
            Ok(dataset) => Some(dataset),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "discarding the cached Minecraft options feed; it is no longer usable"
                );
                self.cache.discard(inspected).await;
                None
            }
        }
    }

    /// Whether the cached copy is recent enough that a refresh would be waste.
    pub async fn is_fresh(&self) -> bool {
        let Some(inspected) = self.cache.inspect(&self.key).await else {
            return false;
        };
        if OptionsDataset::parse(inspected.entry().json()).is_err() {
            self.cache.discard(inspected).await;
            return false;
        }
        !inspected.entry().freshness().is_stale()
    }

    /// Fetches, validates, and - only then - caches the published dataset.
    ///
    /// The order is the point. A corrupt, truncated, incompatible, or
    /// semantically invalid document fails before the cache is touched, so the
    /// last known-good copy survives every kind of bad publish.
    pub async fn refresh(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<OptionsDataset, FeedError> {
        // Serializes this process's refreshes of one feed so they do not
        // download the same document repeatedly. It does not protect the
        // cache: the revision check that does is made under the cache entry's
        // cross-process lock, in `store_if` below.
        let _refresh = self.refresh_lock.lock().await;
        let entry = self.source.entry_url()?;
        self.allowlist.validate_url(&entry)?;

        let text = self.read(&entry, reporter, cancel).await?;

        let dataset = OptionsDataset::parse(&text)?;
        // Cached only now, after the document has been read and validated in
        // full. Anything that failed above never reaches this line, which is
        // what keeps the last known-good copy safe from a bad publish.
        let payload = serde_json::value::RawValue::from_string(text)
            .map_err(|source| FeedError::Response { source })?;
        // The revision is compared with what is cached at publication, not
        // with what was cached when the download began: another process
        // sharing the cache may have published a newer revision meanwhile.
        let stored = self
            .cache
            .store_if(&self.key, &entry, payload, |cached| {
                admit_revision(&dataset, cached)
            })
            .await?;
        if let ConditionalStore::Kept(cached) = stored {
            return Ok(cached);
        }

        tracing::info!(
            source = self.source.describe(),
            revision = dataset.revision,
            releases = dataset.releases.len(),
            "refreshed the Minecraft options compatibility feed"
        );
        Ok(dataset)
    }

    /// Fetches one document through staging private to this call and reads it
    /// back.
    ///
    /// Staging rather than the cache path: nothing that has not been validated
    /// may sit where a reader would find it. The staging file is this call's
    /// alone, so feeds that share a cache, in one process or several, never
    /// consume or delete one another's responses.
    async fn read(
        &self,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<String, FeedError> {
        let bytes = self
            .cache
            .download_document(&self.manager, &self.key, url, reporter, cancel)
            .await
            .map_err(|source| FeedError::Unreachable {
                source: Box::new(source),
            })?;
        String::from_utf8(bytes).map_err(|source| FeedError::Unreachable {
            source: Box::new(MetadataCacheError::NotUtf8 {
                key: OPTIONS_FEED_CACHE_KEY.to_owned(),
                source,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_default_source_is_one_unversioned_https_document_on_the_site() {
        // A path that names a revision stops serving the current document the
        // moment the dataset changes, which is the one thing a feed must do.
        let entry = FeedSource::default().entry_url().expect("valid url");
        assert_eq!(entry.as_str(), DEFAULT_FEED_URL);
        assert_eq!(entry.scheme(), "https");
        assert_eq!(entry.host_str(), Some("ephervex.chaotictrials.de"));
        assert_eq!(entry.path(), "/assets/minecraft-options.json");
    }

    #[test]
    fn a_document_source_reads_exactly_its_url() {
        let source = FeedSource::document("https://example.invalid/minecraft-options.json");
        assert_eq!(
            source.entry_url().expect("valid url").as_str(),
            "https://example.invalid/minecraft-options.json"
        );
        assert!(source.describe().contains("minecraft-options.json"));
    }

    #[test]
    fn a_document_description_omits_userinfo_query_and_fragment() {
        let source = FeedSource::document(
            "https://user:hunter2@example.invalid/data.json?token=FAKE_REVIEW_VALUE#part",
        );
        assert_eq!(source.describe(), "https://example.invalid/data.json");
    }

    #[test]
    fn an_unparsable_document_url_is_not_echoed_by_its_description_or_error() {
        let source =
            FeedSource::document("https://exa mple.invalid/data.json?token=FAKE_REVIEW_VALUE");
        assert!(!source.describe().contains("FAKE_REVIEW_VALUE"));
        let error = source.entry_url().expect_err("unparsable url");
        assert!(matches!(error, UrlPolicyError::Malformed));
        assert!(!error.to_string().contains("FAKE_REVIEW_VALUE"));
    }

    #[test]
    fn a_cannot_be_a_base_document_url_is_described_without_its_query() {
        let source = FeedSource::document("mailto:someone@example.invalid?token=FAKE_REVIEW_VALUE");
        assert!(!source.describe().contains("FAKE_REVIEW_VALUE"));
    }
}
