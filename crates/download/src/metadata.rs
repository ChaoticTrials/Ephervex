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
//! A document with a name of Ephervex's own choosing is stored under that name.
//! A document whose identity is a *request* is stored under the SHA-256 digest
//! of that request's canonical key, in a provider namespace, sharded on the
//! first two digest characters (see [`MetadataKey::addressed`]).
//!
//! ```text
//! $XDG_CACHE_HOME/ephervex/metadata/
//! ├── mojang-version-manifest.json         the envelope below
//! ├── mojang-version-manifest.fetch.json   staging, only while a refresh runs
//! └── modrinth/
//!     └── 7a/
//!         ├── 7a...<64 hex characters>.json
//!         └── 7a...<64 hex characters>.fetch.json
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
use sha2::{Digest, Sha256};
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

/// How many leading digest characters become the shard directory.
///
/// One level of 256 is enough to keep a directory listing manageable without
/// turning the store into a deep tree nobody can inspect by hand. The shared
/// content stores shard the same way, and matching them means a user browsing
/// the cache sees one layout rather than two.
const DIGEST_SHARD_LEN: usize = 2;

/// A validated location for one cached document, relative to the metadata
/// store root.
///
/// Wrapping it in a type means the path can only be built from something that
/// has been checked once, at construction. There are two ways to build one and
/// no third:
///
/// - [`MetadataKey::new`], for a document Ephervex names itself
///   (`"mojang-version-manifest"`). The name is a single validated segment.
/// - [`MetadataKey::addressed`], for a document whose identity is a request.
///   The request never reaches the filesystem; its digest does.
///
/// Both forms are bounded, which is the property the rest of the launcher
/// depends on: a metadata path is a sibling away from a staging file and a
/// temporary file, and filesystem name limits apply to those too.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MetadataKey(String);

impl MetadataKey {
    /// Validates a key: one safe path segment, no separators, no `..`.
    pub fn new(key: &str) -> Result<Self, CacheKeyError> {
        Ok(Self(safe_segment(key.trim())?.to_owned()))
    }

    /// Addresses a document by the SHA-256 digest of its canonical key.
    ///
    /// For documents whose identity is a request rather than a name: a project
    /// reference, a search, a list of ids. Those values arrive from users and
    /// from providers, they have no length anyone controls, and a name derived
    /// from one is a name a filesystem may refuse — an encrypted home directory
    /// accepts 143 bytes for a filename, and a slug alone can exceed that. So
    /// the canonical key is hashed and the digest is the name: 64 lowercase hex
    /// characters, whatever went in.
    ///
    /// `canonical_key` is the caller's stable, unambiguous rendering of the
    /// request it identifies, and it is the caller's job to keep it stable —
    /// changing it changes every address derived from it, which is why callers
    /// carry a schema version inside it. `namespace` is the provider directory
    /// the entry lands in, so two providers cannot collide even if they ever
    /// chose the same canonical rendering.
    ///
    /// The digest is not a security boundary. It is here for bounded names,
    /// deterministic addressing, and directory distribution; nothing is
    /// authenticated by it and nothing secret may be put into a canonical key.
    pub fn addressed(namespace: &str, canonical_key: &str) -> Result<Self, CacheKeyError> {
        let namespace = safe_segment(namespace.trim())?;
        let digest = hex::encode(Sha256::digest(canonical_key.as_bytes()));
        let shard = digest.get(..DIGEST_SHARD_LEN).unwrap_or(digest.as_str());
        Ok(Self(format!("{namespace}/{shard}/{digest}")))
    }

    /// The key as a `/`-separated relative path. Every segment is validated.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// This key's file beneath the metadata store root, with `extension`
    /// appended to the final segment.
    ///
    /// Built segment by segment rather than by joining the whole string, so
    /// nothing here depends on a platform reading `/` as a separator.
    fn relative_file(&self, extension: &str) -> PathBuf {
        let mut path = PathBuf::new();
        let mut segments = self.0.split('/').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_some() {
                path.push(segment);
            } else {
                path.push(format!("{segment}.{extension}"));
            }
        }
        path
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
        self.entry_path(key, "json")
    }

    /// Where a refresh writes before it is known to have succeeded.
    ///
    /// Distinct from the envelope path so that a failed refresh cannot destroy
    /// the copy the launcher is currently able to serve. A sibling of it, so
    /// that publishing stays a rename inside one directory.
    fn staging_path(&self, key: &MetadataKey) -> PathBuf {
        self.entry_path(key, "fetch.json")
    }

    fn entry_path(&self, key: &MetadataKey, extension: &str) -> PathBuf {
        self.layout
            .store_root(ContentStore::Metadata)
            .join(key.relative_file(extension))
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
        // The key's own directory, not the store root: an addressed key lives
        // in a provider namespace and a shard beneath it.
        let directory = path.parent().map_or_else(
            || self.layout.store_root(ContentStore::Metadata),
            Path::to_path_buf,
        );
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
        // cheapest way to guarantee that. The suffix is a fixed eight bytes on
        // top of a key that [`MetadataKey`] already bounds, so the sibling is
        // creatable wherever the entry itself is.
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

    /// The digest of an addressed key, split back into the parts the layout
    /// promises: a namespace, a shard, and the digest itself.
    fn addressed_parts(key: &MetadataKey) -> (String, String, String) {
        let parts: Vec<&str> = key.as_str().split('/').collect();
        assert_eq!(parts.len(), 3, "`{key}` is not namespace/shard/digest");
        (
            parts[0].to_owned(),
            parts[1].to_owned(),
            parts[2].to_owned(),
        )
    }

    #[test]
    fn an_addressed_key_is_a_namespace_a_two_character_shard_and_a_digest() {
        let key = MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=jei\n")
            .expect("safe namespace");
        let (namespace, shard, digest) = addressed_parts(&key);

        assert_eq!(namespace, "modrinth");
        assert_eq!(digest.len(), 64);
        assert_eq!(shard.len(), 2);
        assert!(digest.starts_with(&shard), "the shard leads the digest");
        for part in [&shard, &digest] {
            assert!(
                part.bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "`{part}` is not lowercase hexadecimal"
            );
        }
    }

    #[test]
    fn an_addressed_key_is_the_same_every_time_and_different_per_request() {
        let one = MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=jei\n")
            .expect("safe namespace");
        assert_eq!(
            one,
            MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=jei\n")
                .expect("safe namespace")
        );
        assert_ne!(
            one,
            MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=rei\n")
                .expect("safe namespace")
        );
        // The namespace is part of the address, so two providers that ever
        // chose the same canonical rendering still get different entries.
        assert_ne!(
            one,
            MetadataKey::addressed("other-provider", "modrinth:v1:project\nreference=jei\n")
                .expect("safe namespace")
        );
    }

    #[test]
    fn an_addressed_key_stays_the_same_size_however_long_the_request_was() {
        // The point of the whole exercise: a slug or a search query has no
        // length anyone controls, and a filename does.
        let enormous = "s".repeat(20_000);
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::addressed(
            "modrinth",
            &format!("modrinth:v1:search\nquery={enormous}\n"),
        )
        .expect("safe namespace");

        let (_, shard, digest) = addressed_parts(&key);
        assert_eq!(shard.len(), 2);
        assert_eq!(digest.len(), 64);

        for path in [cache.path(&key), cache.staging_path(&key)] {
            let name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .expect("a name");
            assert!(
                name.len() <= 64 + ".fetch.json".len(),
                "`{name}` grows with the request"
            );
            assert!(
                !path.to_string_lossy().contains(&enormous),
                "the request leaked into {}",
                path.display()
            );
        }
        assert_eq!(
            cache
                .path(&key)
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some(format!("{digest}.json").as_str())
        );
    }

    #[test]
    fn an_addressed_entry_publishes_from_a_sibling_in_its_own_shard() {
        // A rename is only atomic inside one directory, so the staging file and
        // the temporary file have to be siblings of the entry, not of the store.
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=jei\n")
            .expect("safe namespace");

        let path = cache.path(&key);
        let parent = path.parent().expect("an entry has a directory");
        assert_eq!(cache.staging_path(&key).parent(), Some(parent));
        assert_eq!(
            path.with_extension("json.writing").parent(),
            Some(parent),
            "the envelope's temporary file must not leave the shard"
        );
        assert!(parent.starts_with(base.path().join("metadata").join("modrinth")));
    }

    #[tokio::test]
    async fn an_addressed_document_round_trips_through_its_shard_as_compact_json() {
        let base = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(base.path());
        let cache = MetadataCache::new(layout.clone());
        let key = MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=jei\n")
            .expect("safe namespace");

        cache
            .store(&key, &url(), raw(r#"{"slug":"jei"}"#))
            .await
            .expect("store");

        let read = cache.peek(&key).await.expect("cached");
        assert_eq!(read.json(), r#"{"slug":"jei"}"#);
        assert_eq!(read.freshness(), Freshness::Cached);

        let text = std::fs::read_to_string(cache.path(&key)).expect("read");
        assert!(
            !text.contains('\n'),
            "the envelope carries pretty-printing whitespace: {text}"
        );
        serde_json::from_str::<serde_json::Value>(&text).expect("valid JSON");

        // The same TTL rules as any other entry: nothing about the address
        // changes when a document expires.
        assert_eq!(
            MetadataCache::with_ttl(layout, Duration::ZERO)
                .peek(&key)
                .await
                .expect("cached")
                .freshness(),
            Freshness::Stale
        );
    }

    #[tokio::test]
    async fn an_entry_left_by_an_older_layout_neither_answers_nor_blocks_an_addressed_read() {
        // Cache files from a build that named entries differently are disposable
        // leftovers: they are not read, and they are not in the way.
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let store = base.path().join("metadata");
        std::fs::create_dir_all(&store).expect("mkdir");
        std::fs::write(
            store.join("modrinth-project-jei.json"),
            br#"{"schema":1,"source":"https://api.modrinth.invalid/v2/project/jei","fetched_at":"2026-01-01T00:00:00Z","payload":{"slug":"stale"}}"#,
        )
        .expect("write");

        let key = MetadataKey::addressed("modrinth", "modrinth:v1:project\nreference=jei\n")
            .expect("safe namespace");
        assert!(
            cache.peek(&key).await.is_none(),
            "an old-layout file must read as a miss, not as an answer"
        );

        cache
            .store(&key, &url(), raw(r#"{"slug":"fresh"}"#))
            .await
            .expect("store");
        assert_eq!(
            cache.peek(&key).await.expect("cached").json(),
            r#"{"slug":"fresh"}"#
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
