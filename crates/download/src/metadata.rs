//! The timestamped provider-metadata cache.
//!
//! Mojang's version manifest, Modrinth's project index, CurseForge's search
//! results - all of them are *mutable* documents that the launcher re-reads
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
//! ├── mojang-version-manifest.fetch.json.part.<id>   one refresh's private staging
//! └── modrinth/
//!     └── 7a/
//!         ├── 7a...<64 hex characters>.json
//!         └── 7a...<64 hex characters>.json.part.<id>   an envelope being published
//! ```
//!
//! Every refresh stages into a file of its own and every envelope is published
//! from a temporary file of its own, each named with a fresh identifier, so
//! concurrent refreshes of one document, in one process or several, never
//! read, delete, or publish one another's files. A file a crash left behind is
//! removed by a later refresh of the same document once it is a day old.
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
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::cache::{CacheKeyError, CacheLayout, ContentStore, safe_segment};
use crate::error::DownloadError;
use crate::manager::{DownloadManager, DownloadRequest};
use crate::progress::OperationReporter;
use crate::publication::{Discard, FileIdentity, discard_unless_replaced};

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

/// The largest provider document this cache will accept.
///
/// A metadata response is untrusted input whose size nobody publishes: there is
/// no hash and no declared length to check it against, so without a ceiling a
/// hostile or broken endpoint can stream until the disk is full. The number is
/// chosen against the largest documents Ephervex actually reads -- Mojang's
/// version manifest and the per-release language catalogs are the biggest by a
/// wide margin, and a loader install profile is capped separately at 16 MiB --
/// so it bounds the damage without being near anything legitimate. An opaque
/// text document is also held to it once escaped as a JSON string.
pub const MAX_METADATA_BYTES: u64 = 64 * 1024 * 1024;

/// How old a staging or temporary file must be before a refresh treats it as
/// left behind by a crashed process rather than owned by a live one.
const ABANDONED_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);

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
    /// from one is a name a filesystem may refuse - an encrypted home directory
    /// accepts 143 bytes for a filename, and a slug alone can exceed that. So
    /// the canonical key is hashed and the digest is the name: 64 lowercase hex
    /// characters, whatever went in.
    ///
    /// `canonical_key` is the caller's stable, unambiguous rendering of the
    /// request it identifies, and it is the caller's job to keep it stable -
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
#[derive(Debug, Deserialize)]
struct Envelope {
    schema: u32,
    source: String,
    fetched_at: DateTime<Utc>,
    payload: Box<RawValue>,
}

/// The envelope as written, borrowing the payload so that publishing an entry
/// never copies the document.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    schema: u32,
    source: &'a str,
    fetched_at: DateTime<Utc>,
    payload: &'a RawValue,
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

    /// An opaque provider document is larger than [`MAX_METADATA_BYTES`] once
    /// encoded for the cache. Treated as a provider fault, so a stale copy is
    /// preferred over it where one exists.
    #[error("`{key}` is larger than {limit} bytes once encoded for the cache")]
    EncodedTooLarge { key: String, limit: u64 },

    /// Filesystem failure against a specific path.
    #[error("io failure at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl MetadataCacheError {
    /// Whether the caller canceled, as opposed to the provider failing.
    #[must_use]
    pub fn is_cancellation(&self) -> bool {
        match self {
            Self::Fetch { source, .. } | Self::Unavailable { source, .. } => {
                source.is_cancellation()
            }
            _ => false,
        }
    }

    /// The error for a canceled caller, which no cached copy may stand in for.
    fn canceled(key: &MetadataKey, url: &Url) -> Self {
        Self::Fetch {
            key: key.to_string(),
            source: Box::new(DownloadError::Canceled {
                url: crate::allowlist::redacted(url),
            }),
        }
    }

    /// Promotes an unreachable-provider failure to the offline case, once the
    /// caller has established there is nothing cached to serve instead.
    fn into_unavailable(self) -> Self {
        match self {
            Self::Fetch { key, source } => Self::Unavailable { key, source },
            other => other,
        }
    }
}

/// What [`MetadataCache::store_if`] does with the document it was handed.
#[derive(Debug)]
pub enum Admission<T> {
    /// Replace the cached entry.
    Store,
    /// Leave the cached entry alone and report `T` to the caller.
    Keep(T),
}

/// What [`MetadataCache::store_if`] did.
#[derive(Debug)]
pub enum ConditionalStore<T> {
    /// The document was written.
    Stored(CachedMetadata),
    /// The cached entry was left alone; the value the check produced.
    Kept(T),
}

/// A cached document together with the file it was read from.
///
/// Holds that file open so its identity cannot be reused by a later
/// publication before [`MetadataCache::discard`] has compared it.
#[derive(Debug)]
pub struct InspectedMetadata {
    entry: CachedMetadata,
    path: PathBuf,
    identity: Option<FileIdentity>,
    _file: fs::File,
}

impl InspectedMetadata {
    #[must_use]
    pub const fn entry(&self) -> &CachedMetadata {
        &self.entry
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

    /// The name a refresh's private staging file is derived from.
    ///
    /// Distinct from the envelope path so that a failed refresh cannot destroy
    /// the copy the launcher is currently able to serve. A sibling of it, so
    /// that publishing stays a rename inside one directory. Never used as is:
    /// each refresh makes it unique with [`crate::manager::staging_path`].
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
    /// understood - an unreadable cache entry is a cache miss, never an error,
    /// because the correct response to both is the same: fetch it again.
    pub async fn peek(&self, key: &MetadataKey) -> Option<CachedMetadata> {
        self.inspect(key).await.map(|inspected| inspected.entry)
    }

    /// Like [`Self::peek`], but remembers which file was read so that
    /// [`Self::discard`] can drop exactly that publication.
    pub async fn inspect(&self, key: &MetadataKey) -> Option<InspectedMetadata> {
        let path = self.path(key);
        let mut file = fs::File::open(&path).await.ok()?;
        let identity = file
            .metadata()
            .await
            .ok()
            .and_then(|metadata| FileIdentity::of(&metadata));
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await.ok()?;
        let envelope: Envelope = serde_json::from_slice(&bytes).inspect_err(|error| {
            debug!(path = %path.display(), %error, "discarding unreadable metadata cache entry");
        }).ok()?;
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
        Some(InspectedMetadata {
            entry: entry.with_freshness(freshness),
            path,
            identity,
            _file: file,
        })
    }

    /// Serves the document, fetching it only if what is cached has expired.
    ///
    /// If the refresh fails and a cached copy exists, that copy is returned as
    /// [`Freshness::Stale`] and the failure is logged rather than propagated:
    /// the launcher stays usable while the provider is down, and the caller is
    /// told exactly what it is looking at.
    ///
    /// A canceled caller is never served from the cache, fresh or stale: the
    /// error says it was canceled (see [`MetadataCacheError::is_cancellation`]).
    pub async fn fetch(
        &self,
        manager: &DownloadManager,
        key: &MetadataKey,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        if cancel.is_cancelled() {
            return Err(MetadataCacheError::canceled(key, url));
        }
        let cached = self.peek(key).await;
        if let Some(entry) = cached.as_ref().filter(|entry| !entry.freshness.is_stale()) {
            debug!(%key, age_s = entry.age().as_secs(), "serving cached provider metadata");
            return Ok(entry.clone());
        }

        match self.refresh(manager, key, url, reporter, cancel).await {
            Ok(entry) => Ok(entry),
            // Canceling is not a provider outage, so no stale copy answers it.
            Err(error) if error.is_cancellation() => Err(error),
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
                // say so. `refresh` cannot know it - it has not looked.
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
        if cancel.is_cancelled() {
            return Err(MetadataCacheError::canceled(key, url));
        }
        let cached = self.peek(key).await;
        if let Some(entry) = cached.as_ref().filter(|entry| !entry.freshness.is_stale()) {
            debug!(%key, age_s = entry.age().as_secs(), "serving cached provider text");
            return Ok(entry.clone());
        }

        match self.refresh_text(manager, key, url, reporter, cancel).await {
            Ok(entry) => Ok(entry),
            Err(error) if error.is_cancellation() => Err(error),
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
        let bytes = self
            .download_document(manager, key, url, reporter, cancel)
            .await?;
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
        let bytes = self
            .download_document(manager, key, url, reporter, cancel)
            .await?;
        let text = String::from_utf8(bytes).map_err(|source| MetadataCacheError::NotUtf8 {
            key: key.to_string(),
            source,
        })?;
        let payload = encode_text(key, &text)?;
        self.store(key, url, payload).await
    }

    /// Downloads `url` bounded by [`MAX_METADATA_BYTES`] and returns its bytes,
    /// without touching the cached entry.
    ///
    /// The response is staged in a file only this call knows about, and that
    /// file is removed before returning. Concurrent calls for one `key` from any
    /// number of threads or processes therefore each read the response they
    /// asked for, and none can delete another's staging file. For a caller that
    /// post-processes a document before caching it with [`Self::store`].
    pub async fn download_document(
        &self,
        manager: &DownloadManager,
        key: &MetadataKey,
        url: &Url,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, MetadataCacheError> {
        self.remove_abandoned_staging(key).await;

        // The manager treats an existing destination as a finished download, so
        // the destination must be a name nothing has ever used.
        let staging = crate::manager::staging_path(&self.staging_path(key));
        let request = DownloadRequest::new(url.clone(), &staging)
            .labeled(key.to_string())
            .bounded_by(MAX_METADATA_BYTES);
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
        read
    }

    /// Removes staging and temporary files for `key` that a crashed process
    /// left behind.
    ///
    /// Age is the ownership test: nothing live keeps one of these files for a
    /// day, so a file that old belongs to no refresh.
    async fn remove_abandoned_staging(&self, key: &MetadataKey) {
        let envelope = self.path(key);
        let Some(directory) = envelope.parent() else {
            return;
        };
        let staging = self.staging_path(key);
        let Ok(mut entries) = fs::read_dir(directory).await else {
            return;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if !crate::manager::is_partial_of(file_name, &envelope)
                && !crate::manager::is_partial_of(file_name, &staging)
            {
                continue;
            }
            let abandoned = match entry.metadata().await {
                Ok(metadata) => {
                    metadata.is_file()
                        && metadata
                            .modified()
                            .ok()
                            .and_then(|modified| modified.elapsed().ok())
                            .is_some_and(|age| age >= ABANDONED_STAGING_AGE)
                }
                Err(_) => false,
            };
            if abandoned {
                remove_if_present(&entry.path()).await;
            }
        }
    }

    /// Writes a document into the cache, stamped with the current time.
    ///
    /// Exposed so a caller that obtained a document some other way - a test, or
    /// a provider whose response has to be post-processed before it is worth
    /// caching - can still record it with provenance.
    pub async fn store(
        &self,
        key: &MetadataKey,
        source: &Url,
        payload: Box<RawValue>,
    ) -> Result<CachedMetadata, MetadataCacheError> {
        Ok(self.publish(key, source, payload).await)
    }

    /// Stamps and writes a document, returning it even when it could not be
    /// written.
    async fn publish(
        &self,
        key: &MetadataKey,
        source: &Url,
        payload: Box<RawValue>,
    ) -> CachedMetadata {
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
        entry
    }

    /// Writes a document only if `admit` accepts what is cached at the moment
    /// of publication.
    ///
    /// The read of the cached entry, the decision, and the write happen under
    /// one exclusive lock on a sibling file of the entry, so every writer that
    /// uses this method, in this process or another, decides against the entry
    /// the previous writer left. Deciding before calling [`Self::store`]
    /// cannot give that: another process may publish in between.
    ///
    /// `admit` runs while the lock is held and must not block for long. Its
    /// error is returned without writing.
    pub async fn store_if<T, E>(
        &self,
        key: &MetadataKey,
        source: &Url,
        payload: Box<RawValue>,
        admit: impl FnOnce(Option<&CachedMetadata>) -> Result<Admission<T>, E>,
    ) -> Result<ConditionalStore<T>, E> {
        let lock_path = self.entry_path(key, "lock");
        if let Some(directory) = lock_path.parent() {
            // The write below reports a directory that cannot be created; here
            // the lock is merely skipped.
            let _ = fs::create_dir_all(directory).await;
        }
        let _lock = crate::publication::lock_entry(&lock_path).await;

        match admit(self.peek(key).await.as_ref())? {
            Admission::Keep(kept) => Ok(ConditionalStore::Kept(kept)),
            Admission::Store => Ok(ConditionalStore::Stored(
                self.publish(key, source, payload).await,
            )),
        }
    }

    /// Drops one document from the cache, whatever is published there now.
    ///
    /// For a document nothing else shares. A verdict reached by reading the
    /// entry belongs to [`Self::discard`].
    pub async fn invalidate(&self, key: &MetadataKey) {
        remove_if_present(&self.path(key)).await;
    }

    /// Drops the entry that was inspected, unless another publication has
    /// replaced it since.
    ///
    /// A process that judged an entry unusable may be acting on a stale
    /// verdict: another process can publish a valid revision in between, and
    /// removing that would erase the revision a later rollback check compares
    /// against. The comparison and the removal run under the same directory
    /// lock every publication renames under. Where the platform reports no
    /// file identity the entry is left for the next publication to replace.
    pub async fn discard(&self, inspected: InspectedMetadata) {
        let Some(identity) = inspected.identity else {
            return;
        };
        // `inspected` stays open across the call, which keeps the inode unique.
        match discard_unless_replaced(&inspected.path, identity).await {
            Ok(Discard::Removed) => {}
            Ok(Discard::Replaced) => {
                debug!(
                    path = %inspected.path.display(),
                    "kept a metadata cache entry published after it was inspected"
                );
            }
            Err(error) => {
                debug!(path = %inspected.path.display(), %error, "could not remove metadata cache file");
            }
        }
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

        let envelope = EnvelopeRef {
            schema: METADATA_ENVELOPE_VERSION,
            source: &entry.source,
            fetched_at: entry.fetched_at,
            payload: &entry.payload,
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
        // cheapest way to guarantee that. The temporary file is unique to this
        // write, so concurrent writers of one entry each publish a complete
        // envelope and the last rename wins.
        let temporary = crate::manager::staging_path(&path);
        let published = match fs::write(&temporary, &encoded).await {
            Ok(()) => crate::manager::publish_staged(&temporary, &path).await,
            Err(error) => Err(error),
        };
        if let Err(source) = published {
            remove_if_present(&temporary).await;
            return Err(MetadataCacheError::Io { path, source });
        }
        Ok(())
    }
}

/// Encodes a text document as the JSON string the envelope stores.
///
/// JSON escaping can grow control characters sixfold, so the response limit
/// alone does not bound the stored payload. The encoder stops as soon as the
/// output would pass [`MAX_METADATA_BYTES`] instead of building it first.
fn encode_text(key: &MetadataKey, text: &str) -> Result<Box<RawValue>, MetadataCacheError> {
    let too_large = || MetadataCacheError::EncodedTooLarge {
        key: key.to_string(),
        limit: MAX_METADATA_BYTES,
    };
    let limit = usize::try_from(MAX_METADATA_BYTES).unwrap_or(usize::MAX);
    let mut writer = BoundedWriter {
        bytes: Vec::with_capacity(text.len().saturating_add(2).min(limit)),
        limit,
    };
    serde_json::to_writer(&mut writer, text).map_err(|error| {
        if error.is_io() {
            too_large()
        } else {
            MetadataCacheError::NotJson {
                key: key.to_string(),
                source: error,
            }
        }
    })?;
    let encoded =
        String::from_utf8(writer.bytes).map_err(|source| MetadataCacheError::NotUtf8 {
            key: key.to_string(),
            source,
        })?;
    RawValue::from_string(encoded).map_err(|source| MetadataCacheError::NotJson {
        key: key.to_string(),
        source,
    })
}

/// A byte sink that refuses to grow past `limit`.
struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.limit - self.bytes.len() {
            return Err(std::io::Error::other("encoded document exceeds its limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
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

    fn cache_in(dir: &tempfile::TempDir) -> MetadataCache {
        MetadataCache::new(CacheLayout::new(dir.path()))
    }

    #[tokio::test]
    async fn a_conditional_store_writes_only_when_the_check_admits_the_cached_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir);
        let key = MetadataKey::new("revisioned").unwrap();

        let first = cache
            .store_if(&key, &url(), raw("1"), |cached| {
                assert!(cached.is_none());
                Ok::<_, ()>(Admission::<()>::Store)
            })
            .await
            .unwrap();
        assert!(matches!(first, ConditionalStore::Stored(_)));

        let second = cache
            .store_if(&key, &url(), raw("2"), |cached| {
                assert_eq!(cached.unwrap().json(), "1");
                Ok::<_, ()>(Admission::Keep("kept"))
            })
            .await
            .unwrap();
        assert!(matches!(second, ConditionalStore::Kept("kept")));
        assert_eq!(cache.peek(&key).await.unwrap().json(), "1");

        let refused = cache
            .store_if(&key, &url(), raw("3"), |_| {
                Err::<Admission<()>, _>("refused")
            })
            .await;
        assert_eq!(refused.unwrap_err(), "refused");
        assert_eq!(cache.peek(&key).await.unwrap().json(), "1");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discarding_an_inspected_entry_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir);
        let key = MetadataKey::new("revisioned").unwrap();
        cache.store(&key, &url(), raw("1")).await.unwrap();

        let inspected = cache.inspect(&key).await.unwrap();
        assert_eq!(inspected.entry().json(), "1");
        cache.discard(inspected).await;

        assert!(cache.peek(&key).await.is_none());
        assert!(!cache.path(&key).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discarding_an_inspected_entry_keeps_a_revision_published_after_the_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir);
        let key = MetadataKey::new("revisioned").unwrap();
        cache.store(&key, &url(), raw("1")).await.unwrap();
        let inspected = cache.inspect(&key).await.unwrap();

        cache.store(&key, &url(), raw("2")).await.unwrap();
        cache.discard(inspected).await;

        assert_eq!(cache.peek(&key).await.unwrap().json(), "2");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discarding_an_inspected_entry_keeps_a_revision_stored_through_the_entry_lock() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir);
        let key = MetadataKey::new("revisioned").unwrap();
        cache.store(&key, &url(), raw("1")).await.unwrap();
        let inspected = cache.inspect(&key).await.unwrap();

        cache
            .store_if(&key, &url(), raw("2"), |_| {
                Ok::<_, ()>(Admission::<()>::Store)
            })
            .await
            .unwrap();
        cache.discard(inspected).await;

        assert_eq!(cache.peek(&key).await.unwrap().json(), "2");
    }

    #[tokio::test]
    async fn discarding_an_entry_someone_else_already_removed_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir);
        let key = MetadataKey::new("revisioned").unwrap();
        cache.store(&key, &url(), raw("1")).await.unwrap();
        let inspected = cache.inspect(&key).await.unwrap();

        cache.invalidate(&key).await;
        cache.discard(inspected).await;

        assert!(cache.peek(&key).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_conditional_store_decides_against_what_another_writer_published_while_it_waited() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir);
        let key = MetadataKey::new("revisioned").unwrap();
        cache.store(&key, &url(), raw("1")).await.unwrap();

        // Another process holds the entry's lock, as it would while publishing.
        let lock_path = cache.entry_path(&key, "lock");
        let holder = crate::publication::lock_entry(&lock_path).await;
        let waiting = tokio::spawn({
            let (cache, key) = (cache.clone(), key.clone());
            async move {
                cache
                    .store_if(&key, &url(), raw("2"), |cached| {
                        let seen = cached.unwrap().json().to_owned();
                        Ok::<_, ()>(if seen == "1" {
                            Admission::Store
                        } else {
                            Admission::Keep(seen)
                        })
                    })
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!waiting.is_finished(), "decided without taking the lock");

        cache.store(&key, &url(), raw("3")).await.unwrap();
        drop(holder);

        let outcome = waiting.await.unwrap().unwrap();
        assert!(matches!(&outcome, ConditionalStore::Kept(seen) if seen == "3"));
        assert_eq!(cache.peek(&key).await.unwrap().json(), "3");
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
            crate::manager::staging_path(&path).parent(),
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

    fn files_in(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn loopback_manager() -> DownloadManager {
        DownloadManager::new(DownloaderConfig {
            http: crate::client::HttpConfig::for_loopback_tests(),
            retry: RetryPolicy::none(),
            ..DownloaderConfig::default()
        })
        .expect("client")
    }

    #[tokio::test]
    async fn concurrent_refreshes_of_one_key_each_publish_their_own_complete_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"versions":[1,2,3]}"#)
                    .set_delay(Duration::from_millis(20)),
            )
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");
        let url = Url::parse(&format!("{}/index.json", server.uri())).expect("url");
        let manager = loopback_manager();

        let refreshes = (0..40).map(|index| {
            let (cache, manager, key, url) =
                (cache.clone(), manager.clone(), key.clone(), url.clone());
            tokio::spawn(async move {
                let reporter = OperationReporter::silent("metadata");
                let cancel = CancellationToken::new();
                if index % 2 == 0 {
                    cache
                        .refresh(&manager, &key, &url, &reporter, &cancel)
                        .await
                } else {
                    cache
                        .refresh_text(&manager, &key, &url, &reporter, &cancel)
                        .await
                }
            })
        });
        for refresh in futures_util::future::join_all(refreshes).await {
            refresh
                .expect("task")
                .expect("a healthy provider never fails a refresh");
        }

        let published = cache.peek(&key).await.expect("an envelope was published");
        assert!(
            published.json() == r#"{"versions":[1,2,3]}"#
                || published
                    .text()
                    .is_ok_and(|text| text == r#"{"versions":[1,2,3]}"#),
            "published a torn or foreign document: {}",
            published.json()
        );
        assert_eq!(
            files_in(&base.path().join("metadata")),
            ["provider-index.json"],
            "no staging or temporary file may outlive its refresh"
        );
    }

    #[tokio::test]
    async fn text_whose_escaped_form_passes_the_limit_is_refused_and_not_cached() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Each control character escapes to six bytes, so this body is inside
        // the response limit and its encoded form is not.
        let hostile = vec![0x01_u8; 12 * 1024 * 1024];
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/maven-metadata.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(hostile))
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("maven-metadata").expect("safe key");
        let url = Url::parse(&format!("{}/maven-metadata.xml", server.uri())).expect("url");

        let error = cache
            .refresh_text(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("an oversized encoded document is refused");

        assert!(
            matches!(error, MetadataCacheError::EncodedTooLarge { .. }),
            "got {error:?}"
        );
        assert!(!error.is_cancellation());
        assert!(
            files_in(&base.path().join("metadata")).is_empty(),
            "a refused document leaves nothing in the cache"
        );
    }

    #[tokio::test]
    async fn an_oversized_refresh_text_leaves_the_previous_entry_servable() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/maven-metadata.xml"))
            .respond_with(
                ResponseTemplate::new(200).set_body_bytes(vec![0x01_u8; 12 * 1024 * 1024]),
            )
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(base.path());
        let key = MetadataKey::new("maven-metadata").expect("safe key");
        let url = Url::parse(&format!("{}/maven-metadata.xml", server.uri())).expect("url");
        MetadataCache::new(layout.clone())
            .store(&key, &url, raw(r#""<metadata/>""#))
            .await
            .expect("store");

        let entry = MetadataCache::with_ttl(layout, Duration::ZERO)
            .fetch_text(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("the earlier copy is served");

        assert!(entry.freshness().is_stale());
        assert_eq!(entry.text().expect("text"), "<metadata/>");
    }

    #[tokio::test]
    async fn text_with_quotes_newlines_and_unicode_round_trips_through_refresh_text() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let document =
            "<?xml version=\"1.0\"?>\n<metadata>\t<v>1.0</v> \u{e9}\u{1f600}</metadata>\n";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/maven-metadata.xml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(document))
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("maven-metadata").expect("safe key");
        let url = Url::parse(&format!("{}/maven-metadata.xml", server.uri())).expect("url");

        let fetched = cache
            .refresh_text(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh_text");

        assert_eq!(fetched.text().expect("text"), document);
        let cached = cache.peek(&key).await.expect("cached");
        assert_eq!(cached.text().expect("text"), document);
    }

    #[tokio::test]
    async fn a_refresh_never_adopts_a_file_another_refresh_left_at_a_staging_name() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"own":true}"#))
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");
        let root = base.path().join("metadata");
        std::fs::create_dir_all(&root).expect("mkdir");
        let foreign = root.join("provider-index.fetch.json");
        std::fs::write(&foreign, br#"{"own":false}"#).expect("write");
        let url = Url::parse(&format!("{}/index.json", server.uri())).expect("url");

        let refreshed = cache
            .refresh(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert_eq!(refreshed.json(), r#"{"own":true}"#);
        assert_eq!(
            std::fs::read(&foreign).expect("another refresh's file is untouched"),
            br#"{"own":false}"#
        );
    }

    #[tokio::test]
    async fn concurrent_stores_of_one_key_always_leave_a_complete_envelope() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");

        let writes = (0..40).map(|index| {
            let (cache, key) = (cache.clone(), key.clone());
            tokio::spawn(async move {
                cache
                    .write_envelope(
                        &key,
                        &CachedMetadata {
                            payload: raw(&format!(r#"{{"writer":{index}}}"#)),
                            fetched_at: Utc::now(),
                            source: url().to_string(),
                            freshness: Freshness::Fetched,
                        },
                    )
                    .await
            })
        });
        for write in futures_util::future::join_all(writes).await {
            write.expect("task").expect("every write publishes");
        }

        cache.peek(&key).await.expect("a complete envelope");
        assert_eq!(
            files_in(&base.path().join("metadata")),
            ["provider-index.json"]
        );
    }

    #[tokio::test]
    async fn only_abandoned_staging_for_the_same_document_is_removed_by_a_refresh() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");
        let root = base.path().join("metadata");
        std::fs::create_dir_all(&root).expect("mkdir");

        let old = root.join(format!("provider-index.fetch.json.part.{}", "a".repeat(32)));
        let recent = root.join(format!("provider-index.json.part.{}", "b".repeat(32)));
        let other = root.join(format!("other-index.fetch.json.part.{}", "c".repeat(32)));
        for file in [&old, &recent, &other] {
            std::fs::write(file, b"x").expect("write");
        }
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        for file in [&old, &other] {
            std::fs::File::options()
                .write(true)
                .open(file)
                .expect("open")
                .set_modified(long_ago)
                .expect("set mtime");
        }

        let url = Url::parse(&format!("{}/index.json", server.uri())).expect("url");
        cache
            .refresh(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert!(!old.exists(), "a day-old staging file is abandoned");
        assert!(recent.exists(), "a recent file may belong to a live writer");
        assert!(
            other.exists(),
            "another document's staging is not this refresh's to remove"
        );
    }

    #[tokio::test]
    async fn a_refresh_leaves_other_documents_whose_names_extend_its_own() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let root = base.path().join("metadata");
        let lookalike = MetadataKey::new("provider-index.json.part.x").expect("safe key");
        cache
            .store(&lookalike, &url(), raw("{}"))
            .await
            .expect("store");
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(cache.path(&lookalike))
            .expect("open")
            .set_modified(long_ago)
            .expect("set mtime");

        let key = MetadataKey::new("provider-index").expect("safe key");
        let url = Url::parse(&format!("{}/index.json", server.uri())).expect("url");
        cache
            .refresh(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert!(
            cache.peek(&lookalike).await.is_some(),
            "a refresh removed another document's envelope; the store holds {:?}",
            files_in(&root)
        );
    }

    #[tokio::test]
    async fn abandoned_staging_of_a_long_key_is_removed_despite_name_truncation() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/index.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new(&"k".repeat(100)).expect("safe key");
        let root = base.path().join("metadata");
        std::fs::create_dir_all(&root).expect("mkdir");

        let staging = cache.staging_path(&key);
        let outer = crate::manager::staging_path(&staging);
        let nested = crate::manager::staging_path(&outer);
        let envelope = crate::manager::staging_path(&cache.path(&key));
        let abandoned = [outer, nested, envelope];
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        for file in &abandoned {
            std::fs::write(file, b"x").expect("write");
            std::fs::File::options()
                .write(true)
                .open(file)
                .expect("open")
                .set_modified(long_ago)
                .expect("set mtime");
        }

        let url = Url::parse(&format!("{}/index.json", server.uri())).expect("url");
        cache
            .refresh(
                &loopback_manager(),
                &key,
                &url,
                &OperationReporter::silent("metadata"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        for file in &abandoned {
            assert!(!file.exists(), "{} was never collected", file.display());
        }
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

    #[tokio::test]
    async fn a_canceled_refresh_is_an_error_rather_than_a_stale_copy() {
        let base = tempfile::tempdir().expect("temp dir");
        let layout = CacheLayout::new(base.path());
        let key = MetadataKey::new("provider-index").expect("safe key");
        MetadataCache::new(layout.clone())
            .store(&key, &url(), raw(r#"{"versions":[1]}"#))
            .await
            .expect("store");
        let expired = MetadataCache::with_ttl(layout, Duration::ZERO);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let json = expired
            .fetch(
                &unreachable_manager(),
                &key,
                &url(),
                &OperationReporter::silent("metadata"),
                &cancel,
            )
            .await
            .expect_err("a canceled caller is not served stale data");
        assert!(json.is_cancellation(), "got {json:?}");

        let text = expired
            .fetch_text(
                &unreachable_manager(),
                &key,
                &url(),
                &OperationReporter::silent("metadata"),
                &cancel,
            )
            .await
            .expect_err("a canceled caller is not served stale text");
        assert!(text.is_cancellation(), "got {text:?}");
    }

    #[tokio::test]
    async fn a_canceled_caller_is_not_served_a_fresh_cached_copy() {
        let base = tempfile::tempdir().expect("temp dir");
        let cache = MetadataCache::new(CacheLayout::new(base.path()));
        let key = MetadataKey::new("provider-index").expect("safe key");
        cache.store(&key, &url(), raw("{}")).await.expect("store");

        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = cache
            .fetch(
                &unreachable_manager(),
                &key,
                &url(),
                &OperationReporter::silent("metadata"),
                &cancel,
            )
            .await
            .expect_err("cancellation wins over a cache hit");
        assert!(error.is_cancellation(), "got {error:?}");
    }

    /// The stale path logs why the refresh failed, and that reason is read by
    /// somebody debugging an outage. It must not say "no cached copy" while a
    /// cached copy is being served - only `fetch` knows whether one exists.
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
