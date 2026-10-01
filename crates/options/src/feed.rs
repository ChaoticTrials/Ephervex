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
//! Today the document is a public Gist, read through GitHub's stable Gist API
//! endpoint for a fixed id rather than a revision-specific raw URL, so the feed
//! keeps working when it is edited. GitHub truncates large file bodies in that
//! response and supplies a `raw_url` for the full content; that fallback is
//! followed, with the URL checked against an explicit host allowlist because it
//! arrived inside remote JSON.
//!
//! Nothing above the transport knows any of that. [`OptionsFeed`] takes a
//! [`FeedSource`], and replacing the Gist with a plain static JSON endpoint is
//! a different `FeedSource`, not a change to anything that reads the dataset.
//! No credentials are involved: this is a public read-only document.

use ephervex_download::{
    CacheLayout, DomainAllowlist, DownloadManager, DownloadRequest, MetadataCache,
    MetadataCacheError, MetadataKey, OperationReporter, UrlPolicyError,
};
use reqwest::Url;
use serde::Deserialize;
use tokio::fs;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tokio_util::sync::CancellationToken as Cancel;

use crate::dataset::{DatasetError, OptionsDataset};

/// The cache key the validated feed document is stored under.
pub const OPTIONS_FEED_CACHE_KEY: &str = "ephervex-minecraft-options";

/// The Gist holding the published compatibility dataset.
pub const DEFAULT_GIST_ID: &str = "84f4b9229c20bb0fcda74215aabe3e76";

/// GitHub's stable Gist API root.
///
/// The API endpoint for a fixed id rather than a revision-specific raw URL: the
/// raw URL stops serving the current document the moment the feed is edited,
/// which is the one thing a feed has to survive.
pub const GIST_API_BASE: &str = "https://api.github.com";

/// The file inside that Gist.
pub const FEED_FILE_NAME: &str = "minecraft-options.json";

/// Hosts the feed may be read from.
///
/// Exact-match and deliberately short. `api.github.com` serves the Gist
/// document; `gist.githubusercontent.com` serves the raw body GitHub points at
/// when it truncates a file. Every addition widens what remote JSON can make
/// the launcher talk to.
pub const FEED_HOSTS: &[&str] = &["api.github.com", "gist.githubusercontent.com"];

/// Where the compatibility dataset is published.
///
/// Two shapes rather than a plugin system: a Gist, which needs one indirection
/// through GitHub's API, and a plain document at a URL, which does not. A
/// future move to static hosting is the second variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedSource {
    /// A public Gist, addressed by its stable id through the Gist API.
    Gist {
        /// The API root. A field rather than a constant so the offline tests
        /// can point it at a loopback server; production uses
        /// [`GIST_API_BASE`].
        api_base: String,
        id: String,
        file: String,
    },
    /// A document served directly at one URL.
    Document { url: String },
}

impl Default for FeedSource {
    fn default() -> Self {
        Self::gist(DEFAULT_GIST_ID, FEED_FILE_NAME)
    }
}

impl FeedSource {
    /// A Gist read through GitHub's public API.
    #[must_use]
    pub fn gist(id: &str, file: &str) -> Self {
        Self::Gist {
            api_base: GIST_API_BASE.to_owned(),
            id: id.to_owned(),
            file: file.to_owned(),
        }
    }

    /// The same Gist, read through another API root.
    #[must_use]
    pub fn through(mut self, api_base: &str) -> Self {
        if let Self::Gist { api_base: base, .. } = &mut self {
            base.clear();
            base.push_str(api_base.trim_end_matches('/'));
        }
        self
    }

    /// The URL the refresh starts from.
    pub fn entry_url(&self) -> Result<Url, UrlPolicyError> {
        let raw = match self {
            Self::Gist { api_base, id, .. } => format!("{api_base}/gists/{id}"),
            Self::Document { url } => url.clone(),
        };
        Url::parse(&raw).map_err(|_| UrlPolicyError::Malformed { url: raw })
    }

    /// A short stable description for logs. Never a credential-bearing URL.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Gist { id, file, .. } => format!("gist {id}/{file}"),
            Self::Document { url } => url.clone(),
        }
    }
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
    /// The URL policy refused something, including a `raw_url` the response
    /// supplied that does not point at an allowed host.
    #[error("the Minecraft options feed named a location that is not permitted")]
    Location(#[from] UrlPolicyError),
    /// The Gist response did not contain the expected file.
    #[error("the Minecraft options feed response has no `{file}`")]
    FileMissing { file: String },
    /// The response was not the shape the hosting expects.
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

/// The shape of GitHub's Gist document, reduced to what the feed needs.
#[derive(Debug, Deserialize)]
struct GistResponse {
    files: std::collections::BTreeMap<String, GistFile>,
}

#[derive(Debug, Deserialize)]
struct GistFile {
    /// Absent when GitHub omits the body entirely.
    content: Option<String>,
    /// Set when the body was cut short and `raw_url` must be followed.
    #[serde(default)]
    truncated: bool,
    raw_url: Option<String>,
}

/// Reads the published compatibility dataset.
#[derive(Debug, Clone)]
pub struct OptionsFeed {
    manager: DownloadManager,
    layout: CacheLayout,
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
        let allowlist = match &source {
            FeedSource::Gist { .. } => DomainAllowlist::new(FEED_HOSTS.iter().copied()),
            FeedSource::Document { .. } => {
                let entry = source.entry_url()?;
                let host = entry.host_str().ok_or_else(|| UrlPolicyError::Malformed {
                    url: source.describe(),
                })?;
                DomainAllowlist::new([host])
            }
        };
        Ok(Self {
            cache: MetadataCache::new(layout.clone()),
            key: MetadataKey::new(OPTIONS_FEED_CACHE_KEY).map_err(|_| {
                UrlPolicyError::Malformed {
                    url: OPTIONS_FEED_CACHE_KEY.to_owned(),
                }
            })?,
            allowlist,
            manager,
            layout,
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
        let entry = self.cache.peek(&self.key).await?;
        match OptionsDataset::parse(entry.json()) {
            Ok(dataset) => Some(dataset),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "discarding the cached Minecraft options feed; it is no longer usable"
                );
                self.cache.invalidate(&self.key).await;
                None
            }
        }
    }

    /// Whether the cached copy is recent enough that a refresh would be waste.
    pub async fn is_fresh(&self) -> bool {
        let Some(entry) = self.cache.peek(&self.key).await else {
            return false;
        };
        if OptionsDataset::parse(entry.json()).is_err() {
            self.cache.invalidate(&self.key).await;
            return false;
        }
        !entry.freshness().is_stale()
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
        // Revision comparison and publication are one critical section. Two
        // concurrent refreshes must not let a slower older response overwrite
        // a newer response that finished first.
        let _refresh = self.refresh_lock.lock().await;
        let entry = self.source.entry_url()?;
        self.allowlist.validate_url(&entry)?;

        let body = self.read(&entry, "entry", reporter, cancel).await?;
        let text = match &self.source {
            FeedSource::Document { .. } => body,
            FeedSource::Gist { file, .. } => self.gist_body(&body, file, reporter, cancel).await?,
        };

        let dataset = OptionsDataset::parse(&text)?;
        if let Some(cached) = self.cached().await {
            if dataset.revision < cached.revision {
                return Err(FeedError::Rollback {
                    incoming: dataset.revision,
                    cached: cached.revision,
                });
            }
            if dataset.revision == cached.revision {
                if dataset == cached {
                    return Ok(cached);
                }
                return Err(FeedError::RevisionCollision {
                    revision: dataset.revision,
                });
            }
        }
        // Cached only now, after the document has been read and validated in
        // full. Anything that failed above never reaches this line, which is
        // what keeps the last known-good copy safe from a bad publish.
        let payload = serde_json::value::RawValue::from_string(text)
            .map_err(|source| FeedError::Response { source })?;
        self.cache
            .store(&self.key, &entry, payload)
            .await
            .map_err(|source| FeedError::NotCached {
                source: Box::new(source),
            })?;

        tracing::info!(
            source = self.source.describe(),
            revision = dataset.revision,
            releases = dataset.releases.len(),
            "refreshed the Minecraft options compatibility feed"
        );
        Ok(dataset)
    }

    /// Extracts the document from a Gist response, following `raw_url` when
    /// GitHub says the inline body was cut short.
    async fn gist_body(
        &self,
        response: &str,
        file: &str,
        reporter: &OperationReporter,
        cancel: &Cancel,
    ) -> Result<String, FeedError> {
        let parsed: GistResponse =
            serde_json::from_str(response).map_err(|source| FeedError::Response { source })?;
        let entry = parsed
            .files
            .get(file)
            .ok_or_else(|| FeedError::FileMissing {
                file: file.to_owned(),
            })?;

        if !entry.truncated {
            if let Some(content) = entry.content.as_ref() {
                return Ok(content.clone());
            }
        }

        let raw = entry
            .raw_url
            .as_deref()
            .ok_or_else(|| FeedError::FileMissing {
                file: file.to_owned(),
            })?;
        // The URL came out of a remote document, so it is checked against the
        // allowlist exactly like any other untrusted location.
        let url = self.allowlist.validate(raw)?;
        self.read(&url, "raw", reporter, cancel).await
    }

    /// Fetches one document into a staging path and reads it back.
    ///
    /// Staging rather than the cache path: nothing that has not been validated
    /// may sit where a reader would find it.
    async fn read(
        &self,
        url: &Url,
        stage: &str,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<String, FeedError> {
        let staging = self
            .layout
            .store_root(ephervex_download::ContentStore::Metadata)
            .join(format!("{OPTIONS_FEED_CACHE_KEY}.{stage}.fetch"));
        if let Some(parent) = staging.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|source| FeedError::Unreachable {
                    source: Box::new(MetadataCacheError::Io {
                        path: parent.to_path_buf(),
                        source,
                    }),
                })?;
        }
        // The download manager treats an existing destination as a finished
        // download, so a leftover staging file would be served as this refresh.
        let _ = fs::remove_file(&staging).await;

        // Bounded like every other metadata fetch: the feed is a document with
        // no published size and no hash, so nothing but this stops a hostile or
        // broken host from streaming until the disk is full.
        let request = DownloadRequest::new(url.clone(), &staging)
            .labeled(OPTIONS_FEED_CACHE_KEY)
            .bounded_by(ephervex_download::MAX_METADATA_BYTES);
        let outcome = self.manager.download(&request, reporter, cancel).await;
        let read = match outcome {
            Ok(_) => fs::read(&staging)
                .await
                .map_err(|source| MetadataCacheError::Io {
                    path: staging.clone(),
                    source,
                }),
            Err(source) => Err(MetadataCacheError::Fetch {
                key: OPTIONS_FEED_CACHE_KEY.to_owned(),
                source: Box::new(source),
            }),
        };
        let _ = fs::remove_file(&staging).await;

        let bytes = read.map_err(|source| FeedError::Unreachable {
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
    fn the_default_source_is_the_stable_gist_endpoint_rather_than_a_revision() {
        // A revision-specific raw URL stops serving the current document the
        // moment the feed is edited, which is the one thing a feed must do.
        let entry = FeedSource::default().entry_url().expect("valid url");
        assert_eq!(
            entry.as_str(),
            "https://api.github.com/gists/84f4b9229c20bb0fcda74215aabe3e76"
        );
        assert!(
            DomainAllowlist::new(FEED_HOSTS.iter().copied())
                .validate_url(&entry)
                .is_ok()
        );
    }

    #[test]
    fn the_feed_allowlist_permits_only_the_two_hosts_it_needs() {
        let allowlist = DomainAllowlist::new(FEED_HOSTS.iter().copied());
        assert!(
            allowlist
                .validate("https://api.github.com/gists/abc")
                .is_ok()
        );
        assert!(
            allowlist
                .validate(
                    "https://gist.githubusercontent.com/user/abc/raw/x/minecraft-options.json"
                )
                .is_ok()
        );
        for refused in [
            "https://evil.example/minecraft-options.json",
            "http://api.github.com/gists/abc",
            "https://api.github.com.evil.example/gists/abc",
            "file:///etc/passwd",
        ] {
            assert!(allowlist.validate(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_document_source_needs_no_gist_indirection() {
        let source = FeedSource::Document {
            url: "https://example.invalid/minecraft-options.json".to_owned(),
        };
        assert_eq!(
            source.entry_url().expect("valid url").as_str(),
            "https://example.invalid/minecraft-options.json"
        );
        assert!(source.describe().contains("minecraft-options.json"));
    }
}
