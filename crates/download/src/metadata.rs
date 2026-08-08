//! The timestamped provider-metadata cache.
//!
//! Mojang's version manifest, Modrinth's project index, CurseForge's search
//! results — all of them are *mutable* documents that the launcher re-reads
//! constantly and that it must survive not being able to reach. A
//! bug-reproduction launcher is used precisely when things are on fire, and a
//! flaky provider must not stop it listing versions it already knows about.
//!
//! So every such document is stored with the time it was fetched, and every
//! read reports one of three outcomes:
//!
//! | [`Freshness`] | Meaning |
//! |---|---|
//! | [`Fetched`](Freshness::Fetched) | Came off the network during this call. |
//! | [`Cached`](Freshness::Cached) | Served from disk, still inside its TTL; no request was made. |
//! | [`Stale`](Freshness::Stale) | Served from disk **past** its TTL, because the refresh failed. |
//!
//! and a refresh that fails with nothing cached is
//! [`MetadataCacheError::Unavailable`] rather than a hang. Those four states are
//! this produces explicit offline/error states instead of indefinite
//! spinners": a frontend can render "showing data from 40 minutes ago" instead
//! of a spinner that never resolves, and it can only do that if the distinction
//! reaches it.
//!
//! # What does *not* belong here
//!
//! Immutable, hash-identified documents. A Mojang per-version JSON never
//! changes once published and its SHA-1 is supplied by the manifest, so it is
//! written straight to a content-addressed path via
//! [`CacheLayout::content`](crate::CacheLayout::content) with the digest handed
//! to [`DownloadRequest::verifying`](crate::DownloadRequest::verifying). It has
//! no TTL because it has no way to go out of date, and it needs no envelope
//! because its identity *is* its content. Giving such a document a timestamp
//! would only invite pointless re-fetching.
//!
//! # On-disk shape
//!
//! ```text
//! $XDG_CACHE_HOME/ephervex/metadata/
//! ├── mojang-version-manifest.json         the envelope below
//! └── mojang-version-manifest.fetch.json   staging, only while a refresh runs
//! ```
//!
//! ```json
//! {
//!   "schema": 1,
//!   "source": "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json",
//!   "fetched_at": "2026-08-07T09:13:44Z",
//!   "payload": { "latest": { … }, "versions": [ … ] }
//! }
//! ```
//!
//! The payload is embedded verbatim rather than stored beside the envelope so
//! that the document and the time it was fetched land in one atomic rename.
//! Two files could disagree after a crash, and a payload with an unknown age is
//! exactly the state this module exists to prevent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::fs;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::cache::{CacheKeyError, CacheLayout, ContentStore, safe_segment};
use crate::error::DownloadError;
use crate::manager::{DownloadManager, DownloadRequest};
use crate::progress::OperationReporter;

/// Envelope format version. Bumped only if the envelope's own fields change;
/// an envelope from the future is treated as "nothing cached", because a cache
/// is the one thing that may always be discarded.
pub const METADATA_ENVELOPE_VERSION: u32 = 1;

/// How long a provider document is served without asking the network.
///
/// Fifteen minutes is chosen against what these documents are for: a new
/// Minecraft release, a new pack version, a new mod file. None of those need to
/// be visible within seconds, and re-fetching a quarter-megabyte manifest on
/// every list operation is exactly the behavior The interface asks launchers not to
/// have. Callers that genuinely need "now" call
/// [`MetadataCache::refresh`].
pub const DEFAULT_METADATA_TTL: Duration = Duration::from_secs(15 * 60);

/// Where a document came from and how old it is.
///
/// The three variants are deliberately distinguishable by a frontend: "live",
/// "cached", and "the provider is down and this is what we last saw" are three
/// different things to tell a user, and collapsing them is how launchers end up
/// showing a spinner forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Freshness {
    /// Fetched over the network during this call.
    Fetched,
    /// Read from the cache while still inside its TTL. No request was made.
    Cached,
    /// Read from the cache *past* its TTL, because refreshing failed.
    Stale,
}

impl Freshness {
    /// Whether the caller is looking at data the provider could not confirm.
    #[must_use]
    pub const fn is_stale(self) -> bool {
        matches!(self, Self::Stale)
    }
}

/// A validated name for one cached document.
///
/// Wrapping it in a type means the filename can only be built from a string
/// that has been checked once, at construction. The key is chosen by Ephervex
/// (`"mojang-version-manifest"`), never taken from provider output, but the
/// validation is here anyway because "never" has a way of becoming "not yet"
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MetadataKey(String);

impl MetadataKey {
    /// Validates a key: one safe path segment, no separators, no `..`.
    pub fn new(key: &str) -> Result<Self, CacheKeyError> {
        Ok(Self(safe_segment(key.trim())?.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MetadataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(&self.0)
    }
}

/// The on-disk envelope. Private: callers see [`CachedMetadata`].
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    schema: u32,
    source: String,
    fetched_at: DateTime<Utc>,
    payload: Box<RawValue>,
}

/// One provider document, with the provenance a caller needs to decide whether
/// to trust it.
#[derive(Debug, Clone)]
pub struct CachedMetadata {
    payload: Box<RawValue>,
    fetched_at: DateTime<Utc>,
    source: String,
    freshness: Freshness,
}

impl CachedMetadata {
    /// The document as JSON text, byte-identical to what the provider sent.
    #[must_use]
    pub fn json(&self) -> &str {
        self.payload.get()
    }

    /// Deserializes the document.
    pub fn parse<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.payload.get())
    }

    /// Decodes an opaque UTF-8 provider document cached as a JSON string.
    ///
    /// Maven metadata is XML, but still needs the same freshness envelope and
    /// stale fallback as JSON provider documents.
    pub fn text(&self) -> Result<String, serde_json::Error> {
        self.parse()
    }

    /// When this document was fetched from the provider.
    #[must_use]
    pub const fn fetched_at(&self) -> DateTime<Utc> {
        self.fetched_at
    }

    /// The URL it was fetched from, for display and diagnostics.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub const fn freshness(&self) -> Freshness {
        self.freshness
    }

    /// How long ago it was fetched.
    ///
    /// A timestamp in the future means the machine's clock moved backwards
    /// since the fetch; that reports as zero here and as *not* fresh in
    /// [`Self::fresh_within`], so a bad clock causes a re-fetch rather than
    /// pinning a document forever.
    #[must_use]
    pub fn age(&self) -> Duration {
        (Utc::now() - self.fetched_at)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }

    /// Whether the document is inside `ttl` and the timestamp is believable.
    #[must_use]
    pub fn fresh_within(&self, ttl: Duration) -> bool {
        self.fetched_at <= Utc::now() && self.age() <= ttl
    }

    fn with_freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = freshness;
        self
    }
}

/// Why a metadata document could not be produced.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetadataCacheError {
    /// The provider could not be reached. On its own this says nothing about
    /// whether the launcher can carry on: [`MetadataCache::fetch`] turns it
    /// into either a stale hit or [`Self::Unavailable`] depending on what is
    /// cached. It escapes only from [`MetadataCache::refresh`], which by
    /// definition has no fallback to offer.
    #[error("`{key}` could not be fetched")]
    Fetch {
        key: String,
        #[source]
        source: Box<DownloadError>,
    },

    /// The refresh failed and there was nothing cached to fall back on. This
    /// is the genuinely offline case, and it is an error precisely so a caller
    /// cannot mistake it for the stale case.
    #[error("no cached copy of `{key}` and it could not be fetched")]
    Unavailable {
        key: String,
        #[source]
        source: Box<DownloadError>,
    },

    /// The provider answered, but not with JSON. Treated as a provider fault,
    /// so a stale copy is preferred over it where one exists.
    #[error("`{key}` was fetched but is not valid json")]
    NotJson {
        key: String,
        #[source]
        source: serde_json::Error,
    },

    /// An opaque provider document was not UTF-8.
    #[error("`{key}` was fetched but is not utf-8 text")]
    NotUtf8 {
        key: String,
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// Filesystem failure against a specific path.
    #[error("io failure at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl MetadataCacheError {
    /// Promotes an unreachable-provider failure to the offline case, once the
    /// caller has established there is nothing cached to serve instead.
    fn into_unavailable(self) -> Self {
        match self {
            Self::Fetch { key, source } => Self::Unavailable { key, source },
            other => other,
        }
    }
}

/// Stores provider documents with the time they were fetched.
#[derive(Debug, Clone)]
pub struct MetadataCache {
    layout: CacheLayout,
    ttl: Duration,
}

impl MetadataCache {
    /// A cache under `layout`'s metadata store, using
    /// [`DEFAULT_METADATA_TTL`].
    #[must_use]
    pub fn new(layout: CacheLayout) -> Self {
        Self {
            layout,
            ttl: DEFAULT_METADATA_TTL,
        }
    }

    /// A cache with an explicit TTL.
    #[must_use]
    pub const fn with_ttl(layout: CacheLayout, ttl: Duration) -> Self {
        Self { layout, ttl }
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Where one document's envelope lives.
    #[must_use]
    pub fn path(&self, key: &MetadataKey) -> PathBuf {
        self.layout
            .store_root(ContentStore::Metadata)
            .join(format!("{}.json", key.as_str()))
    }

    /// Where a refresh writes before it is known to have succeeded.
    ///
    /// Distinct from the envelope path so that a failed refresh cannot destroy
    /// the copy the launcher is currently able to serve.
    fn staging_path(&self, key: &MetadataKey) -> PathBuf {
        self.layout
            .store_root(ContentStore::Metadata)
            .join(format!("{}.fetch.json", key.as_str()))
    }

    /// Reads whatever is cached, however old, without touching the network.
    ///
    /// Returns `None` when nothing is cached or when what is cached cannot be
    /// understood — an unreadable cache entry is a cache miss, never an error,
    /// because the correct response to both is the same: fetch it again.
    pub async fn peek(&self, key: &MetadataKey) -> Option<CachedMetadata> {
        let path = self.path(key);
        let bytes = fs::read(&path).await.ok()?;
        let envelope: Envelope = serde_json::from_slice(&bytes)
.inspect_err(|error| {
                debug!(path = %path.display(), %error, "discarding unreadable metadata cache entry");
            })
.ok()?;
        if envelope.schema > METADATA_ENVELOPE_VERSION {
            debug!(
                path = %path.display(),
                schema = envelope.schema,
                "ignoring a metadata cache entry written by a newer build"
            );
            return None;
        }

        let entry = CachedMetadata {
            payload: envelope.payload,
            fetched_at: envelope.fetched_at,
            source: envelope.source,
            freshness: Freshness::Cached,
        };
        let freshness = if entry.fresh_within(self.ttl) {
            Freshness::Cached
        } else {
            Freshness::Stale
        };
        Some(entry.with_freshness(freshness))
    }

    /// Serves the document, fetching it only if what is cached has expired.
    ///
    /// If the refresh fails and a cached copy exists, that copy is returned as
    /// [`Freshness::Stale`] and the failure is logged rather than propagated:
    /// the launcher stays usable while the provider is down, and the caller is
    /// told exactly what it is looking at.
    pub async fn fetch(
        &self,
        manager: &DownloadManager,
        key: &MetadataKey,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        let cached = self.peek(key).await;
        if let Some(entry) = cached.as_ref().filter(|entry| !entry.freshness.is_stale()) {
            debug!(%key, age_s = entry.age().as_secs(), "serving cached provider metadata");
            return Ok(entry.clone());
        }

        match self.refresh(manager, key, url, reporter, cancel).await {
            Ok(entry) => Ok(entry),
            Err(error) => match cached {
                Some(entry) => {
                    warn!(
                        %key,
                        age_s = entry.age().as_secs(),
                        %error,
                        "serving stale provider metadata; the refresh failed"
                    );
                    Ok(entry.with_freshness(Freshness::Stale))
                }
                // Only now is "no cached copy" true, so only now may the error
                // say so. `refresh` cannot know it — it has not looked.
                None => Err(error.into_unavailable()),
            },
        }
    }

    /// Serves an opaque UTF-8 document with the same TTL and stale fallback as
    /// [`Self::fetch`]. The on-disk payload is a JSON string so the envelope
    /// format remains unchanged.
    pub async fn fetch_text(
        &self,
        manager: &DownloadManager,
        key: &MetadataKey,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        let cached = self.peek(key).await;
        if let Some(entry) = cached.as_ref().filter(|entry| !entry.freshness.is_stale()) {
            debug!(%key, age_s = entry.age().as_secs(), "serving cached provider text");
            return Ok(entry.clone());
        }

        match self.refresh_text(manager, key, url, reporter, cancel).await {
            Ok(entry) => Ok(entry),
            Err(error) => match cached {
                Some(entry) => {
                    warn!(
                        %key,
                        age_s = entry.age().as_secs(),
                        %error,
                        "serving stale provider text; the refresh failed"
                    );
                    Ok(entry.with_freshness(Freshness::Stale))
                }
                None => Err(error.into_unavailable()),
            },
        }
    }

    /// Fetches unconditionally, ignoring both the TTL and any cached copy.
    ///
    /// On success the cache is updated. A failure leaves the previous entry
    /// intact; use [`Self::fetch`] if a stale fallback is wanted.
    pub async fn refresh(
        &self,
        manager: &DownloadManager,
        key: &MetadataKey,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        let staging = self.staging_path(key);
        // The manager treats an existing destination as a finished download, so
        // yesterday's staging file would be served as today's refresh.
        remove_if_present(&staging).await;

        let request = DownloadRequest::new(url.clone(), &staging).labeled(key.to_string());
        let outcome = manager.download(&request, reporter, cancel).await;
        let read = match outcome {
            Ok(_) => fs::read(&staging)
                .await
                .map_err(|source| MetadataCacheError::Io {
                    path: staging.clone(),
                    source,
                }),
            Err(source) => Err(MetadataCacheError::Fetch {
                key: key.to_string(),
                source: Box::new(source),
            }),
        };
        remove_if_present(&staging).await;
        let bytes = read?;

        let payload: Box<RawValue> =
            serde_json::from_slice(&bytes).map_err(|source| MetadataCacheError::NotJson {
                key: key.to_string(),
                source,
            })?;
        self.store(key, url, payload).await
    }

    /// Refreshes an opaque UTF-8 document unconditionally.
    pub async fn refresh_text(
        &self,
        manager: &DownloadManager,
        key: &MetadataKey,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        let staging = self.staging_path(key);
        remove_if_present(&staging).await;

        let request = DownloadRequest::new(url.clone(), &staging).labeled(key.to_string());
        let outcome = manager.download(&request, reporter, cancel).await;
        let read = match outcome {
            Ok(_) => fs::read(&staging)
                .await
                .map_err(|source| MetadataCacheError::Io {
                    path: staging.clone(),
                    source,
                }),
            Err(source) => Err(MetadataCacheError::Fetch {
                key: key.to_string(),
                source: Box::new(source),
            }),
        };
        remove_if_present(&staging).await;
        let text = String::from_utf8(read?).map_err(|source| MetadataCacheError::NotUtf8 {
            key: key.to_string(),
            source,
        })?;
        let payload = serde_json::value::to_raw_value(&text).map_err(|source| {
            MetadataCacheError::NotJson {
                key: key.to_string(),
                source,
            }
        })?;
        self.store(key, url, payload).await
    }

    /// Writes a document into the cache, stamped with the current time.
    ///
    /// Exposed so a caller that obtained a document some other way — a test, or
    /// a provider whose response has to be post-processed before it is worth
    /// caching — can still record it with provenance.
    pub async fn store(
        &self,
        key: &MetadataKey,
        source: &Url,
        payload: Box<RawValue>,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        let entry = CachedMetadata {
            payload,
            fetched_at: Utc::now(),
            source: crate::allowlist::redacted(source),
            freshness: Freshness::Fetched,
        };

        // A cache that cannot be written is not a reason to fail: the caller
        // has the document in hand, and the only cost is fetching it again next
        // time. Losing the *document* over a full disk would be the worse
        // trade.
        if let Err(error) = self.write_envelope(key, &entry).await {
            warn!(%key, %error, "provider metadata was fetched but could not be cached");
        }
        Ok(entry)
    }

    /// Drops one document from the cache.
    pub async fn invalidate(&self, key: &MetadataKey) {
        remove_if_present(&self.path(key)).await;
    }

    async fn write_envelope(
        &self,
        key: &MetadataKey,
        entry: &CachedMetadata,
    ) -> Result<(), MetadataCacheError> {
        let path = self.path(key);
        let directory = self.layout.store_root(ContentStore::Metadata);
        fs::create_dir_all(&directory)
            .await
            .map_err(|source| MetadataCacheError::Io {
                path: directory,
                source,
            })?;

        let envelope = Envelope {
            schema: METADATA_ENVELOPE_VERSION,
            source: entry.source.clone(),
            fetched_at: entry.fetched_at,
            payload: entry.payload.clone(),
        };
        // Serializing an envelope whose payload is already-valid JSON cannot
        // fail, so a failure here means the document was not what it claimed;
        // reported as I/O rather than inventing a variant nothing can produce.
        let encoded = serde_json::to_vec(&envelope).map_err(|source| MetadataCacheError::Io {
            path: path.clone(),
            source: std::io::Error::other(source),
        })?;

        // Same atomicity rule as the download manager's: a reader must never
        // see a half-written envelope, and a rename within one directory is the
        // cheapest way to guarantee that.
        let temporary = path.with_extension("json.writing");
        fs::write(&temporary, &encoded)
            .await
            .map_err(|source| MetadataCacheError::Io {
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, &path)
            .await
            .map_err(|source| MetadataCacheError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(())
    }
}

/// Deletes a path, treating "it was not there" as success.
async fn remove_if_present(path: &Path) {
    if let Err(error) = fs::remove_file(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            debug!(path = %path.display(), %error, "could not remove metadata cache file");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::manager::DownloaderConfig;
    use crate::retry::RetryPolicy;

    fn raw(json: &str) -> Box<RawValue> {
        RawValue::from_string(json.to_owned()).expect("valid json")
    }

    fn url() -> Url {
        Url::parse("https://provider.invalid/index.json").expect("valid url")
    }

    #[test]
    fn a_key_cannot_escape_the_metadata_store() {
        for hostile in ["../evil", "a/b", "", ".", "..", "a\0b"] {
            assert!(
                MetadataKey::new(hostile).is_err(),
                "`{hostile}` should be rejected"
            );
        }
        assert_eq!(
            MetadataKey::new(" mojang-version-manifest ")
                .expect("safe key")
                .as_str(),
            "mojang-version-manifest"
        );
    }

    #[tokio::test]
    async fn a_stored_document_comes_back_with_its_provenance() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");

        let stored = cache
            .store(&key, &url(), raw(r#"{"versions":[1,2]}"#))
            .await
            .expect("store");
        assert_eq!(stored.freshness(), Freshness::Fetched);

        let read = cache.peek(&key).await.expect("cached");
        assert_eq!(read.json(), r#"{"versions":[1,2]}"#);
        assert_eq!(read.source(), "https://provider.invalid/index.json");
        assert_eq!(read.freshness(), Freshness::Cached);
        assert!(read.age() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn opaque_text_uses_the_same_envelope_without_becoming_json() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-xml").expect("safe key");
        let payload = serde_json::value::to_raw_value("<metadata><version>1</version></metadata>")
            .expect("json string");

        cache.store(&key, &url(), payload).await.expect("store");
        let read = cache.peek(&key).await.expect("cached");
        assert_eq!(
            read.text().expect("text"),
            "<metadata><version>1</version></metadata>"
        );
    }

    #[tokio::test]
    async fn an_entry_past_its_ttl_reads_back_as_stale() {
        let base = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(base.path());
        let key = MetadataKey::new("provider-index").expect("safe key");

        MetadataCache::new(layout.clone())
            .store(&key, &url(), raw("{}"))
            .await
            .expect("store");

        // A zero TTL makes anything already on disk expired, which is the same
        // condition as a document that has simply been sitting there.
        let expired = MetadataCache::with_ttl(layout, Duration::ZERO)
            .peek(&key)
            .await
            .expect("cached");
        assert_eq!(expired.freshness(), Freshness::Stale);
    }

    /// Builds a manager that gives up immediately, so an unreachable host
    /// fails in one attempt rather than working through the backoff schedule.
    fn unreachable_manager() -> DownloadManager {
        let config = DownloaderConfig {
            retry: RetryPolicy::none(),
            ..DownloaderConfig::default()
        };
        DownloadManager::new(config).expect("client")
    }

    #[tokio::test]
    async fn an_unreachable_provider_with_nothing_cached_is_the_offline_error() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");

        let error = cache
            .fetch(
                &unreachable_manager(),
                &key,
                &url(),
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("nothing cached and the provider is unreachable");

        assert!(
            matches!(error, MetadataCacheError::Unavailable { .. }),
            "expected the offline case, got {error:?}"
        );
    }

    /// The stale path logs why the refresh failed, and that reason is read by
    /// somebody debugging an outage. It must not say "no cached copy" while a
    /// cached copy is being served — only `fetch` knows whether one exists.
    #[tokio::test]
    async fn a_failed_refresh_over_a_cached_copy_does_not_claim_there_is_none() {
        let base = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(base.path());
        let key = MetadataKey::new("provider-index").expect("safe key");

        MetadataCache::new(layout.clone())
            .store(&key, &url(), raw(r#"{"versions":[1]}"#))
            .await
            .expect("store");

        // Zero TTL forces the refresh; the provider is unreachable, so the
        // cached copy is what the caller gets.
        let served = MetadataCache::with_ttl(layout, Duration::ZERO)
            .fetch(
                &unreachable_manager(),
                &key,
                &url(),
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("the cached copy is served rather than the failure");

        assert_eq!(served.freshness(), Freshness::Stale);
        assert_eq!(served.json(), r#"{"versions":[1]}"#);

        let reported = MetadataCacheError::Fetch {
            key: key.to_string(),
            source: Box::new(DownloadError::Canceled {
                url: url().to_string(),
            }),
        }
        .to_string();
        assert!(
            !reported.contains("no cached copy"),
            "the refresh failure reads `{reported}`, which contradicts the stale hit it accompanies"
        );
    }

    #[tokio::test]
    async fn a_corrupt_envelope_is_a_cache_miss_rather_than_an_error() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");

        let path = cache.path(&key);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, b"{ not json").expect("write");

        assert!(cache.peek(&key).await.is_none());
    }

    #[tokio::test]
    async fn an_envelope_from_a_newer_build_is_ignored_rather_than_misread() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");

        let path = cache.path(&key);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &path,
            br#"{"schema":99,"source":"https://x.invalid/","fetched_at":"2026-01-01T00:00:00Z","payload":{}}"#,
        )
.expect("write");

        assert!(cache.peek(&key).await.is_none());
    }

    #[tokio::test]
    async fn a_future_timestamp_is_treated_as_expired_rather_than_eternally_fresh() {
        let entry = CachedMetadata {
            payload: raw("{}"),
            fetched_at: Utc::now() + chrono::TimeDelta::days(1),
            source: String::new(),
            freshness: Freshness::Cached,
        };
        assert_eq!(entry.age(), Duration::ZERO);
        assert!(!entry.fresh_within(DEFAULT_METADATA_TTL));
    }
}
