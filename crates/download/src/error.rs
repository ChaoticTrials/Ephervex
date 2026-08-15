//! Typed errors for the download subsystem.
//!
//! Every variant carries structured fields rather than a pre-formatted
//! sentence: the frontend resolves user-facing text through `ephervex-i18n`,
//! and the `Display` implementations here exist for logs and developer output
//! only. URLs embedded in a variant are always run through
//! [`crate::allowlist::redacted`] first so a signed CDN URL cannot leak its
//! query string into a log file.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use ephervex_model::HashAlgorithm;

use crate::allowlist::UrlPolicyError;
use crate::retry::status_is_transient;

/// Anything that can go wrong while fetching an artifact.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DownloadError {
    /// The URL failed the scheme/host policy before any request was made.
    #[error("download url rejected by policy")]
    Policy(#[from] UrlPolicyError),

    /// The server answered, but not with a success status.
    #[error("http {status} from {url}")]
    HttpStatus { url: String, status: u16 },

    /// Connection, TLS, timeout, or truncated-body failure.
    #[error("transport failure for {url}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    /// The HTTP client itself could not be constructed.
    #[error("could not build the http client")]
    ClientBuild {
        #[source]
        source: reqwest::Error,
    },

    /// A verified hash did not match what was downloaded. Never retried: the
    /// bytes are wrong, and asking the same server again is not a fix
    #[error("{algorithm} mismatch for {url}")]
    HashMismatch {
        url: String,
        algorithm: HashAlgorithm,
        expected: String,
        actual: String,
    },

    /// The metadata declared a size the response did not deliver.
    #[error("size mismatch for {url}: expected {expected} bytes, got {actual}")]
    SizeMismatch {
        url: String,
        expected: u64,
        actual: u64,
    },

    /// A response exceeded the configured ceiling. Imported pack metadata is
    /// untrusted, so an unbounded body must not be able to fill the disk.
    #[error("body from {url} exceeded the {limit} byte ceiling")]
    BodyTooLarge { url: String, limit: u64 },

    /// The server ignored or mishandled a `Range` request. Transient: the next
    /// attempt restarts from byte zero.
    #[error("range request was not honored by {url}")]
    RangeNotHonored { url: String },

    /// Filesystem failure against a specific path.
    ///
    /// The step is named because a bare "io failure" cannot be acted on:
    /// creating a cache directory, creating a temporary artifact, writing a
    /// body, and publishing a finished file fail for different reasons and have
    /// different fixes. The operating system's own message is part of the
    /// display rather than only the source chain, because a single log line is
    /// where these are read.
    #[error("could not {operation} at {path}: {source}")]
    Io {
        operation: ArtifactStep,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The caller's [`tokio_util::sync::CancellationToken`] fired. Distinct
    /// from a failure: `Operation` reports this as `Canceled`, not `Failed`
    #[error("canceled while downloading {url}")]
    Canceled { url: String },

    /// A transient error kept recurring until the retry budget ran out. The
    /// last underlying failure is preserved as the source.
    #[error("gave up on {url} after {attempts} attempt(s)")]
    RetriesExhausted {
        url: String,
        attempts: u32,
        #[source]
        source: Box<DownloadError>,
    },

    /// The manager's concurrency semaphore was closed, i.e. the process is
    /// shutting down.
    #[error("download manager is shutting down")]
    ShuttingDown,
}

impl DownloadError {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// The distinction matters because retrying the wrong things is actively
    /// harmful: a 404 will never become a 200, and a hash mismatch means the
    /// bytes are wrong rather than late.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::HttpStatus { status, .. } => status_is_transient(*status),
            // Timeouts, dropped connections, and truncated bodies are exactly
            // the flaky-service case Ephervex is expected to survive
            Self::Transport { source, .. } => {
                source.is_timeout() || source.is_connect() || source.is_body() || source.is_decode()
            }
            Self::RangeNotHonored { .. } => true,
            Self::Policy(_)
            | Self::ClientBuild { .. }
            | Self::HashMismatch { .. }
            | Self::SizeMismatch { .. }
            | Self::BodyTooLarge { .. }
            | Self::Io { .. }
            | Self::Canceled { .. }
            | Self::RetriesExhausted { .. }
            | Self::ShuttingDown => false,
        }
    }

    /// Whether the caller asked for this outcome rather than hitting a fault.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled { .. })
    }

    /// Builds a [`DownloadError::Io`] naming the step and the path that failed.
    pub(crate) fn io(
        operation: ArtifactStep,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

/// Which filesystem step of an artifact transfer failed.
///
/// A transfer touches the disk in five distinguishable places, and knowing
/// which one gave way is the difference between an actionable report and
/// "something went wrong with a file". A name too long for the filesystem, a
/// full disk, and a cache directory somebody made read-only all surface as an
/// I/O error, and each is reported at a different step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArtifactStep {
    /// Creating the directory an artifact is cached in.
    CreateDirectory,
    /// Creating or positioning the temporary file a transfer writes into.
    CreateTemporary,
    /// Writing the response body.
    WriteBody,
    /// Flushing and syncing the temporary file before it is published.
    Flush,
    /// Renaming the verified file into its cache path.
    Publish,
    /// Reading an entry already at the cache path, to decide whether it can be
    /// reused.
    ReadCached,
}

impl ArtifactStep {
    /// A stable identifier for structured logs and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateDirectory => "create-directory",
            Self::CreateTemporary => "create-temporary",
            Self::WriteBody => "write-body",
            Self::Flush => "flush",
            Self::Publish => "publish",
            Self::ReadCached => "read-cached",
        }
    }
}

impl fmt::Display for ArtifactStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CreateDirectory => "create the cache directory",
            Self::CreateTemporary => "create the temporary artifact",
            Self::WriteBody => "write the artifact",
            Self::Flush => "flush the artifact",
            Self::Publish => "publish the artifact",
            Self::ReadCached => "read the cached artifact",
        })
    }
}

/// One failed attempt, plus any server-supplied hint about when to try again.
///
/// Internal to the retry loop: `Retry-After` belongs to the attempt, not to the
/// error the caller eventually sees, so it is not part of [`DownloadError`].
#[derive(Debug)]
pub(crate) struct AttemptFailure {
    pub(crate) error: DownloadError,
    pub(crate) retry_after: Option<Duration>,
}

impl From<DownloadError> for AttemptFailure {
    fn from(error: DownloadError) -> Self {
        Self {
            error,
            retry_after: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_hash_mismatch_is_never_retried() {
        let error = DownloadError::HashMismatch {
            url: "https://example.invalid/a.jar".to_owned(),
            algorithm: HashAlgorithm::Sha1,
            expected: "a".repeat(40),
            actual: "b".repeat(40),
        };
        assert!(!error.is_transient());
    }

    #[test]
    fn a_not_found_is_never_retried_but_a_gateway_error_is() {
        let not_found = DownloadError::HttpStatus {
            url: "https://example.invalid/a.jar".to_owned(),
            status: 404,
        };
        let gateway = DownloadError::HttpStatus {
            url: "https://example.invalid/a.jar".to_owned(),
            status: 502,
        };
        assert!(!not_found.is_transient());
        assert!(gateway.is_transient());
    }

    #[test]
    fn cancellation_is_reported_separately_from_failure() {
        let canceled = DownloadError::Canceled {
            url: "https://example.invalid/a.jar".to_owned(),
        };
        assert!(canceled.is_cancellation());
        assert!(!canceled.is_transient());
    }
}
