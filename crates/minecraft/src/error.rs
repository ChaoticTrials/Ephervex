//! Typed errors for Minecraft metadata.
//!
//! No English prose lives in these variants. Each carries the structured data
//! a caller needs plus a stable [`MinecraftError::message_id`], and the
//! frontend turns that into a sentence through `ephervex-i18n`. The `Display`
//! implementations exist for logs and developer output.
//!
//! Failures from `ephervex-download` are re-expressed rather than wrapped
//! transparently. "http 503 from piston-meta.mojang.com" is the truth but not
//! the *useful* truth; "the Minecraft version list could not be refreshed and
//! nothing is cached" is what a user can act on, and it is also what lets the
//! stale case be described differently from the offline one.

use std::path::PathBuf;

use ephervex_download::{MetadataCacheError, UrlPolicyError};

use crate::manifest::ManifestError;
use crate::version::{ReleaseType, VersionMetadataError, VersionParseError};
use crate::version_id::MINIMUM_SUPPORTED_VERSION;

/// Anything the Minecraft metadata layer can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MinecraftError {
    /// The version list could not be refreshed and nothing was cached. The
    /// genuinely offline case - distinct from being served a stale list, which
    /// is a success.
    #[error("the minecraft version list is unavailable")]
    ManifestUnavailable {
        #[source]
        source: MetadataCacheError,
    },

    /// Mojang answered with something that is not a version manifest.
    #[error("the minecraft version manifest could not be understood")]
    ManifestInvalid {
        #[source]
        source: ManifestError,
    },

    /// No entry with that id exists in the manifest.
    #[error("`{id}` is not a known minecraft version")]
    UnknownVersion { id: String },

    /// The version exists but predates the support floor. Refused rather than
    /// approximated.
    #[error("minecraft `{id}` predates the supported minimum {minimum}")]
    VersionUnsupported { id: String, minimum: &'static str },

    /// The version exists but is not on the release channel - a snapshot, a
    /// pre-release, or a release candidate. A different thing to tell a user
    /// than "too old", so it gets a different message rather than being folded
    /// into one vague refusal.
    #[error("minecraft `{id}` is a {channel}, not a release")]
    VersionNotARelease { id: String, channel: String },

    /// The version's own JSON could not be fetched and is not cached.
    #[error("metadata for minecraft `{id}` could not be fetched")]
    VersionMetadataUnavailable {
        id: String,
        #[source]
        source: Box<ephervex_download::DownloadError>,
    },

    /// The fetched document did not match the SHA-1 the manifest published for
    /// it. Never retried and never used: a mismatch means the bytes are wrong,
    /// not late.
    #[error("metadata for minecraft `{id}` failed its integrity check")]
    VersionMetadataCorrupt { id: String },

    /// The document was fetched and verified but could not be parsed.
    #[error("metadata for minecraft `{id}` could not be understood")]
    VersionMetadataInvalid {
        id: String,
        #[source]
        source: VersionParseError,
    },

    /// A verified document could not be read back off disk.
    #[error("cached metadata at {path} could not be read")]
    VersionMetadataUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The release's own language catalog could not be resolved.
    ///
    /// Distinct from a metadata failure because the consequence is narrow and
    /// never fatal: an instance is still created and launched, and only the
    /// managed language preference is left unapplied.
    #[error("the language catalog for minecraft `{id}` could not be resolved")]
    LanguageCatalogUnavailable { id: String },

    /// An asset index document could not be used.
    #[error(transparent)]
    AssetIndex(#[from] crate::assets::AssetIndexError),

    /// A required part of an otherwise valid document is missing.
    #[error(transparent)]
    IncompleteMetadata(#[from] VersionMetadataError),

    /// A URL in the metadata, or the manifest URL itself, is not usable.
    #[error(transparent)]
    Url(#[from] UrlPolicyError),

    /// A cache destination could not be formed.
    #[error(transparent)]
    CacheKey(#[from] ephervex_download::CacheKeyError),
}

impl MinecraftError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub fn message_id(&self) -> &'static str {
        match self {
            Self::ManifestUnavailable { .. } => "error-minecraft-manifest-unavailable",
            Self::ManifestInvalid { .. } => "error-minecraft-manifest-invalid",
            Self::UnknownVersion { .. } => "error-minecraft-version-unknown",
            Self::VersionUnsupported { .. } => "error-minecraft-version-unsupported",
            Self::VersionNotARelease { .. } => "error-minecraft-version-not-a-release",
            Self::VersionMetadataUnavailable { .. } => {
                "error-minecraft-version-metadata-unavailable"
            }
            Self::VersionMetadataCorrupt { .. } => "error-minecraft-version-metadata-corrupt",
            Self::VersionMetadataInvalid { .. } => "error-minecraft-version-metadata-invalid",
            Self::VersionMetadataUnreadable { .. } => "error-minecraft-version-metadata-unreadable",
            Self::LanguageCatalogUnavailable { .. } => {
                "error-minecraft-language-catalog-unavailable"
            }
            Self::AssetIndex(_) => "error-minecraft-asset-index-invalid",
            Self::IncompleteMetadata(_) => "error-minecraft-metadata-incomplete",
            Self::Url(_) => "error-minecraft-metadata-url-invalid",
            Self::CacheKey(_) => "error-minecraft-cache-key-invalid",
        }
    }

    /// The path this failure is about, where it names one.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::VersionMetadataUnreadable { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    /// Builds the "too old" refusal with the floor filled in.
    #[must_use]
    pub fn unsupported(id: impl Into<String>) -> Self {
        Self::VersionUnsupported {
            id: id.into(),
            minimum: MINIMUM_SUPPORTED_VERSION,
        }
    }

    /// Builds the "wrong channel" refusal.
    #[must_use]
    pub fn not_a_release(id: impl Into<String>, channel: &ReleaseType) -> Self {
        Self::VersionNotARelease {
            id: id.into(),
            channel: channel.to_string(),
        }
    }
}

/// Shorthand for a metadata result.
pub type MinecraftResult<T> = Result<T, MinecraftError>;

/// Anything the vanilla installation and launch pipeline can fail with
///
/// # Authentication and runtime failures deliberately live elsewhere
///
/// `LaunchError` includes `AuthenticationRequired` and `MissingJava`
/// alongside these. Both are produced *above* this crate: the session comes
/// from `ephervex-auth` and the runtime from `ephervex-java`, and both already
/// carry typed errors with the predicates a frontend needs -
/// `CoreError::requires_interactive_login()` is the typed `AuthenticationRequired` state,
/// and `CoreError::required_java_major()` with
/// `CoreError::managed_java_would_help()` is its `MissingJava`. Re-expressing
/// them here would mean two definitions of "the user must sign in", and the one
/// that drifts is the one a button is wired to.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// The version's metadata does not describe something launchable - a field
    /// a launch needs is missing, or the document is a loader child that has
    /// not been merged with its parent.
    #[error("the metadata for minecraft `{version}` cannot describe a launch")]
    InvalidVersionMetadata {
        version: String,
        #[source]
        source: crate::version::VersionMetadataError,
    },

    /// The instance asks for a loader this pipeline does not install. Refused
    /// rather than launched as vanilla, which would silently reproduce the
    /// wrong thing.
    #[error("`{loader}` instances cannot be launched by the vanilla pipeline")]
    LoaderNotSupported { loader: String },

    /// A required artifact could not be fetched or failed its hash.
    #[error("could not obtain `{artifact}`")]
    DownloadFailed {
        /// What was being fetched, in technical terms: a library coordinate, an
        /// asset name, `client.jar`.
        artifact: String,
        #[source]
        source: Box<ephervex_download::DownloadError>,
    },

    /// The asset index could not be read.
    #[error(transparent)]
    AssetIndex(#[from] crate::assets::AssetIndexError),

    /// A natives jar could not be unpacked.
    #[error(transparent)]
    Natives(#[from] crate::natives::NativesError),

    /// An argument template names a placeholder this build cannot fill in.
    ///
    /// Refused rather than left in place or blanked: an unsubstituted
    /// `${…}` reaches the game as a literal, and a blank one changes what the
    /// game is told without saying so. Either way the launch would no longer be
    /// the one the metadata described.
    #[error("`{argument}` names `${{{placeholder}}}`, which this build cannot resolve")]
    UnresolvedPlaceholder {
        placeholder: String,
        argument: String,
    },

    /// The instance's game directory could not be prepared.
    #[error("the game directory {path} could not be prepared")]
    GameDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The JVM could not be started.
    #[error("could not start {executable}")]
    ProcessStart {
        executable: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The game process could not be supervised after it started.
    #[error("lost track of the minecraft process")]
    ProcessLost {
        #[source]
        source: std::io::Error,
    },

    /// The launch was canceled before the game started.
    #[error("the launch was canceled")]
    Canceled,

    /// A destination path could not be formed from a validated key.
    #[error(transparent)]
    CacheKey(#[from] ephervex_download::CacheKeyError),

    /// A URL in the metadata is not one the launcher will fetch.
    #[error(transparent)]
    Url(#[from] UrlPolicyError),
}

impl LaunchError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::InvalidVersionMetadata { .. } => "error-launch-invalid-version-metadata",
            Self::LoaderNotSupported { .. } => "error-launch-loader-not-supported",
            // A digest that does not match is not a network problem and does
            // not have a network answer: what was served is wrong, and trying
            // the same address again will serve it again.
            Self::DownloadFailed { source, .. } if source.is_hash_mismatch() => {
                "error-launch-artifact-mismatched"
            }
            Self::DownloadFailed { .. } => "error-launch-download-failed",
            Self::AssetIndex(_) => "error-launch-asset-index-invalid",
            Self::Natives(error) => error.message_id(),
            Self::UnresolvedPlaceholder { .. } => "error-launch-unresolved-placeholder",
            Self::GameDirectory { .. } => "error-launch-game-directory",
            Self::ProcessStart { .. } => "error-launch-process-start",
            Self::ProcessLost { .. } => "error-launch-process-lost",
            Self::Canceled => "error-launch-canceled",
            Self::CacheKey(_) => "error-minecraft-cache-key-invalid",
            Self::Url(_) => "error-minecraft-metadata-url-invalid",
        }
    }

    /// The path this failure is about, where it names one.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::GameDirectory { path, .. } => Some(path.clone()),
            Self::ProcessStart { executable, .. } => Some(executable.clone()),
            Self::Natives(error) => Some(error.failing_path()),
            _ => None,
        }
    }

    /// Whether the user canceled rather than something failing.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled)
    }

    /// Wraps a download failure with the name of what was being fetched.
    #[must_use]
    pub fn download(artifact: impl Into<String>, source: ephervex_download::DownloadError) -> Self {
        Self::DownloadFailed {
            artifact: artifact.into(),
            source: Box::new(source),
        }
    }
}

/// Shorthand for a launch result.
pub type LaunchResult<T> = Result<T, LaunchError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_refusal_for_an_old_release_names_the_floor() {
        let error = MinecraftError::unsupported("1.7.10");
        assert!(matches!(
            error,
            MinecraftError::VersionUnsupported {
                minimum: "1.12.2",
                ..
            }
        ));
        assert_eq!(error.message_id(), "error-minecraft-version-unsupported");
    }

    #[test]
    fn no_variant_shares_a_message_id_with_another() {
        // Distinct ids are what let the frontend say something different about
        // "offline", "corrupt", and "too old"; a duplicate would collapse two
        // of those into one sentence without anyone noticing.
        let ids = [
            MinecraftError::UnknownVersion { id: String::new() }.message_id(),
            MinecraftError::unsupported("x").message_id(),
            MinecraftError::VersionMetadataCorrupt { id: String::new() }.message_id(),
            MinecraftError::not_a_release("x", &ReleaseType::Snapshot).message_id(),
        ];
        let mut unique = ids;
        unique.sort_unstable();
        let before = unique.len();
        let mut deduped = unique.to_vec();
        deduped.dedup();
        assert_eq!(deduped.len(), before);
    }
}
