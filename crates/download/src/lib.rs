//! The reusable download subsystem: bounded concurrency, atomic writes, hash
//! verification, retries, and cancellation.
//!
//! Every artifact Ephervex fetches comes through this crate — Mojang assets and
//! libraries, loader installers, Modrinth and CurseForge pack files, managed
//! JDK archives. There is one implementation because there is one set of rules
//! that must hold for all of them:
//!
//! - **Atomic finalization.** A download is written to a unique partial sibling and
//!   renamed into place only after verification. A partial artifact never
//!   appears at a cache path, which is what entitles downstream consumers to treat
//!   presence at that path as proof of a verified artifact.
//! - **Verification.** SHA-1, SHA-256, and SHA-512 are checked whenever the
//!   provider metadata supplies them, as is the declared size. A mismatch
//!   deletes the artifact and fails without retrying.
//! - **Paths are never taken from remote input.** Destinations come from
//!   [`CacheLayout`], derived from a content hash or a validated logical key —
//!   never from a URL segment, a filename in a pack manifest, or a
//!   `Content-Disposition` header.
//! - **Untrusted URLs are filtered.** [`DomainAllowlist`] is the explicit,
//!   auditable list of hosts a `.mrpack` may name.
//! - **Provider metadata is cached with the time it was fetched.**
//!   [`MetadataCache`] serves a document as fresh, cached, or explicitly
//!   *stale*, so a provider outage degrades the launcher instead of stopping it
//!
//! # Example
//!
//! ```no_run
//! use ephervex_download::{
//!     CacheLayout, ContentStore, DownloadManager, DownloadRequest, DownloaderConfig,
//!     OperationReporter,
//! };
//! use ephervex_model::{Hash, HashAlgorithm};
//! use tokio_util::sync::CancellationToken;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let layout = CacheLayout::new("/home/dev/.cache/ephervex");
//! let sha1 = Hash::new(HashAlgorithm::Sha1, "da39a3ee5e6b4b0d3255bfef95601890afd80709")
//!     .ok_or("malformed digest")?;
//!
//! let manager = DownloadManager::new(DownloaderConfig::default())?;
//! let request = DownloadRequest::parse(
//!     "https://cdn.modrinth.com/data/AABBCCDD/versions/1/mod.jar",
//!     layout.content(ContentStore::Downloads, &sha1, None),
//! )?
//!     .verifying(sha1)
//!     .labeled("mod.jar");
//!
//! let reporter = OperationReporter::silent("download-artifact");
//! let outcome = manager
//!     .download(&request, &reporter, &CancellationToken::new())
//!     .await?;
//! println!("{} bytes at {}", outcome.bytes, outcome.path.display());
//! # Ok(())
//! # }
//! ```

pub mod allowlist;
pub mod cache;
pub mod client;
pub mod error;
pub mod manager;
pub mod metadata;
pub mod progress;
pub mod retry;
pub mod verify;

pub use allowlist::{
    DomainAllowlist, MODRINTH_PACK_HOSTS, UrlPolicyError, check_transport, redacted,
};
pub use cache::{
    ARTWORK_FORMAT, CacheKeyError, CacheLayout, ContentStore, FileExtension, MavenConflictKey,
    MavenCoordinate,
};
pub use client::{HttpConfig, build_client};
pub use error::{ArtifactStep, DownloadError};
pub use manager::{
    ArtifactSource, DownloadManager, DownloadOutcome, DownloadRequest, DownloaderConfig,
};
pub use metadata::{
    CachedMetadata, DEFAULT_METADATA_TTL, Freshness, METADATA_ENVELOPE_VERSION, MetadataCache,
    MetadataCacheError, MetadataKey,
};
pub use progress::{ByteTally, ChannelSink, OperationReporter, ProgressSink};
/// The URL type the policy functions take, re-exported.
///
/// [`redacted`] and [`check_transport`] are part of this crate's public surface
/// and both take one, so a caller that wants to redact an address before
/// logging it must be able to name the type without adding `reqwest` to its own
/// manifest — which would put an HTTP client on the compile-time surface of a
/// crate that only parses text.
pub use reqwest::Url;
pub use retry::{RetryPolicy, retry_after, status_is_transient};
pub use verify::{HashMismatch, HashVerifier};
