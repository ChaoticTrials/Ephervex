//! Typed errors for the Java subsystem.
//!
//! No English prose lives in these variants. Each carries the structured data a
//! caller needs plus a stable [`JavaError::message_id`], and the frontend turns
//! that into a sentence through `ephervex-i18n`. The `Display` implementations
//! exist for logs and developer output.
//!
//! The variants are shaped around what a user can *do* about the failure, not
//! around where in the code it happened. "No compatible runtime" and "the
//! runtime you pinned is incompatible" are the same fact mechanically and
//! completely different situations to be in: one offers a download, the other
//! points at a setting.

use std::path::PathBuf;

use crate::archive::ArchiveError;
use crate::rules::RuleRejection;
use crate::selection::PinSource;

/// Anything the Java subsystem can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JavaError {
    /// Nothing installed satisfies the requirement. This represents the "Wrong Java
    /// installed" describes: offer a discovered runtime or a managed download,
    /// never a near miss.
    #[error("no installed java {required_major} runtime is compatible ({considered} considered)")]
    NoCompatibleRuntime {
        required_major: u32,
        considered: usize,
        /// Why each candidate was refused, so the user is not told merely that
        /// something failed.
        rejections: Vec<RuleRejection>,
    },

    /// A runtime the user pinned exists but cannot run this version.
    #[error("the java runtime at {path} is not compatible: {rejection}")]
    PinnedRuntimeIncompatible {
        path: PathBuf,
        pinned_by: PinSource,
        rejection: Box<RuleRejection>,
    },

    /// A path was given as a Java runtime and is not one.
    #[error("no usable java runtime at {path}")]
    RuntimeUnusable {
        path: PathBuf,
        #[source]
        source: Box<ProbeError>,
    },

    /// Automatic detection is switched off and nothing is pinned, so there is
    /// nothing to select from.
    #[error("java auto-detection is disabled and no java path is configured")]
    AutoDetectDisabled { required_major: u32 },

    /// No compatible runtime, and automatic download is switched off.
    #[error("no compatible java {required_major} runtime, and automatic download is disabled")]
    ManagedDownloadDisabled { required_major: u32 },

    /// This build runs on an architecture Adoptium does not publish for.
    #[error("no managed java runtime is published for {arch}")]
    ManagedArchUnsupported { arch: String },

    /// The Adoptium API root is not a usable URL.
    #[error("`{url}` is not a usable managed-runtime API address")]
    ManagedApiUnusable { url: String },

    /// The release index could not be read and nothing was cached.
    #[error("the managed java {major} release index is unavailable")]
    ManagedIndexUnavailable {
        major: u32,
        #[source]
        source: Box<ephervex_download::MetadataCacheError>,
    },

    /// The release index was served but is not what the API documents.
    #[error("the managed java {major} release index could not be understood")]
    ManagedIndexInvalid {
        major: u32,
        #[source]
        source: Box<serde_json::Error>,
    },

    /// The index parsed but lists no build for this major and architecture.
    #[error("no managed java {major} build is published for {architecture}")]
    ManagedReleaseMissing {
        major: u32,
        architecture: &'static str,
    },

    /// A listed release is missing a usable download URL or checksum. Refused
    /// rather than fetched unverified.
    #[error("the managed java {major} release `{release}` is not usable")]
    ManagedReleaseMalformed { major: u32, release: String },

    /// The archive could not be fetched, or failed its checksum.
    #[error("the managed java {major} archive `{release}` could not be downloaded")]
    ManagedDownloadFailed {
        major: u32,
        release: String,
        #[source]
        source: Box<ephervex_download::DownloadError>,
    },

    /// The archive violated an extraction rule or a limit.
    #[error("the managed java archive was refused")]
    ManagedArchiveRejected {
        #[source]
        source: Box<ArchiveError>,
    },

    /// The extraction task did not complete.
    #[error("unpacking the managed java runtime did not finish")]
    ManagedExtractionPanicked,

    /// The runtime installed but could not be identified.
    #[error("the managed java runtime at {path} could not be identified")]
    ManagedRuntimeUnusable {
        path: PathBuf,
        #[source]
        source: Box<ProbeError>,
    },

    /// The installed runtime is a different major than the one requested.
    #[error("the managed runtime at {path} is java {found}, not java {requested}")]
    ManagedRuntimeWrongMajor {
        path: PathBuf,
        requested: u32,
        found: u32,
    },

    /// Something at a release's install path cannot be replaced safely: it is
    /// not a directory, or this filesystem cannot lock out another process
    /// that may be installing there.
    #[error("{path} is in the way of a managed java install and cannot be replaced safely")]
    ManagedInstallBlocked { path: PathBuf },

    /// A cache destination could not be formed.
    #[error("a managed runtime could not be given a safe cache location")]
    ManagedCacheKey {
        #[source]
        source: ephervex_download::CacheKeyError,
    },

    /// A path offered for removal is not one of the launcher's own installs.
    ///
    /// Removal takes a path from a frontend and deletes a directory tree, so
    /// it accepts only what it put there: a runtime found on the system
    /// belongs to whoever installed it, and everything else under the data
    /// directory belongs to something other than Java.
    #[error("{path} is not a runtime Ephervex installed")]
    NotManaged { path: PathBuf },

    /// A filesystem operation failed.
    #[error("could not write {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl JavaError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::NoCompatibleRuntime { .. } => "error-java-no-compatible-runtime",
            Self::PinnedRuntimeIncompatible { .. } => "error-java-pinned-incompatible",
            Self::RuntimeUnusable { .. } => "error-java-runtime-unusable",
            Self::AutoDetectDisabled { .. } => "error-java-auto-detect-disabled",
            Self::ManagedDownloadDisabled { .. } => "error-java-managed-download-disabled",
            Self::ManagedArchUnsupported { .. } => "error-java-managed-arch-unsupported",
            Self::ManagedApiUnusable { .. } => "error-java-managed-api-unusable",
            Self::ManagedIndexUnavailable { .. } => "error-java-managed-index-unavailable",
            Self::ManagedIndexInvalid { .. } => "error-java-managed-index-invalid",
            Self::ManagedReleaseMissing { .. } => "error-java-managed-release-missing",
            Self::ManagedReleaseMalformed { .. } => "error-java-managed-release-malformed",
            Self::ManagedDownloadFailed { .. } => "error-java-managed-download-failed",
            Self::ManagedArchiveRejected { source } => source.message_id(),
            Self::ManagedExtractionPanicked => "error-java-managed-extraction-failed",
            Self::ManagedRuntimeUnusable { .. } | Self::ManagedRuntimeWrongMajor { .. } => {
                "error-java-managed-runtime-unusable"
            }
            Self::ManagedInstallBlocked { .. } => "error-java-managed-install-blocked",
            Self::ManagedCacheKey { .. } => "error-java-managed-cache-key-invalid",
            Self::NotManaged { .. } => "error-java-not-managed",
            Self::Io { .. } => "error-java-io",
        }
    }

    /// The path this failure is about, where it names one.
    ///
    /// Dug out here rather than in the frontend so that "show me the file"
    /// does not drag this whole enum across the `ephervex-core` boundary
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::PinnedRuntimeIncompatible { path, .. }
            | Self::RuntimeUnusable { path, .. }
            | Self::ManagedRuntimeUnusable { path, .. }
            | Self::ManagedRuntimeWrongMajor { path, .. }
            | Self::ManagedInstallBlocked { path }
            | Self::NotManaged { path }
            | Self::Io { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    /// The Java major this failure is about, where it names one.
    ///
    /// A frontend offering "download Java 21" needs the number without matching
    /// on five variants to find it.
    #[must_use]
    pub const fn required_major(&self) -> Option<u32> {
        match self {
            Self::NoCompatibleRuntime { required_major, .. }
            | Self::AutoDetectDisabled { required_major }
            | Self::ManagedDownloadDisabled { required_major } => Some(*required_major),
            Self::ManagedIndexUnavailable { major, .. }
            | Self::ManagedIndexInvalid { major, .. }
            | Self::ManagedReleaseMissing { major, .. }
            | Self::ManagedReleaseMalformed { major, .. }
            | Self::ManagedDownloadFailed { major, .. } => Some(*major),
            Self::ManagedRuntimeWrongMajor { requested, .. } => Some(*requested),
            _ => None,
        }
    }

    /// Whether offering a managed download is a sensible next step.
    ///
    /// False once the managed provider itself is the thing that failed -
    /// suggesting a download that has just failed is worse than saying nothing.
    #[must_use]
    pub const fn managed_download_would_help(&self) -> bool {
        matches!(self, Self::NoCompatibleRuntime { .. })
    }
}

/// Why one candidate executable could not be identified as a JVM.
///
/// Separate from [`JavaError`] because these are per-candidate outcomes that
/// discovery collects and carries on past, not failures of an operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProbeError {
    /// The file could not be executed.
    #[error("{path} could not be executed")]
    NotExecutable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The process ran but exited non-zero.
    #[error("{path} exited with status {status:?} instead of reporting a version")]
    Failed { path: PathBuf, status: Option<i32> },

    /// The process did not finish within [`crate::probe::PROBE_TIMEOUT`].
    #[error("{path} did not report a version within {seconds}s")]
    TimedOut { path: PathBuf, seconds: u64 },

    /// The process ran and produced output that is not a JVM's.
    #[error("{path} did not report a java version")]
    Unintelligible { path: PathBuf },
}

impl ProbeError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::NotExecutable { .. } => "error-java-probe-not-executable",
            Self::Failed { .. } | Self::Unintelligible { .. } => "error-java-probe-unintelligible",
            Self::TimedOut { .. } => "error-java-probe-timed-out",
        }
    }

    #[must_use]
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::NotExecutable { path, .. }
            | Self::Failed { path, .. }
            | Self::TimedOut { path, .. }
            | Self::Unintelligible { path } => path,
        }
    }
}

/// Shorthand for a Java result.
pub type JavaResult<T> = Result<T, JavaError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_failures_a_user_can_act_on_carry_the_major_they_are_about() {
        let error = JavaError::NoCompatibleRuntime {
            required_major: 21,
            considered: 4,
            rejections: Vec::new(),
        };
        assert_eq!(error.required_major(), Some(21));
        assert!(
            error.managed_download_would_help(),
            "this is the one case where offering a download is the right next step"
        );
    }

    #[test]
    fn a_failure_of_the_managed_provider_does_not_offer_another_download() {
        let error = JavaError::ManagedArchUnsupported {
            arch: "sparcv9".to_owned(),
        };
        assert!(!error.managed_download_would_help());
    }

    #[test]
    fn a_rejected_archive_keeps_the_archive_layers_message_id() {
        // Otherwise "this archive tried to escape its directory" would be
        // flattened into a generic managed-install failure, and the user would
        // never learn what was actually refused.
        let inner = ArchiveError::UnsafePath {
            entry: "../../etc/passwd".to_owned(),
        };
        let expected = inner.message_id();
        let wrapped = JavaError::ManagedArchiveRejected {
            source: Box::new(inner),
        };
        assert_eq!(wrapped.message_id(), expected);
    }

    #[test]
    fn no_two_variants_share_a_message_id_by_accident() {
        let ids = [
            JavaError::NoCompatibleRuntime {
                required_major: 8,
                considered: 0,
                rejections: Vec::new(),
            }
            .message_id(),
            JavaError::AutoDetectDisabled { required_major: 8 }.message_id(),
            JavaError::ManagedDownloadDisabled { required_major: 8 }.message_id(),
            JavaError::ManagedArchUnsupported {
                arch: String::new(),
            }
            .message_id(),
            JavaError::ManagedExtractionPanicked.message_id(),
        ];
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(sorted.len(), before, "duplicate message id in {ids:?}");
    }
}
