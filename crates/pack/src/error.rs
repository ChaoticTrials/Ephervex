//! What importing a pack can fail with.
//!
//! Typed per concern and carrying structured data rather than pre-formatted
//! English: every variant exposes a stable `message_id()` that a frontend
//! resolves through `ephervex-i18n`. Nothing in this crate knows what language
//! the user reads.
//!
//! Two families are worth naming explicitly because they define the trust
//! boundary:
//!
//! * **Refusals.** An unsupported loader, a pack declaring two loaders at once,
//!   an entry that escapes the extraction root. These are hard errors, not
//!   warnings, because the alternative is a launcher quietly approximating a
//!   reproduction that then reproduces something else.
//! * **Recognized but unsupported formats.** [`PackError::FormatUnsupported`]
//!   reports a CurseForge or Prism archive by its detected format. Detection is
//!   provider-independent, so the user is told what the file is rather than
//!   being told it is not a pack.

use std::path::PathBuf;

use ephervex_download::{DownloadError, UrlPolicyError};

use crate::detect::PackFormat;

/// Anything importing or reading back a pack can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// The archive file could not be opened or read.
    #[error("the pack archive {path} could not be read")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The file is not a readable ZIP container.
    #[error("{path} is not a readable archive")]
    ArchiveInvalid {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },

    /// Nothing in the archive identifies it as a pack this launcher knows.
    #[error("{path} is not a Modrinth, CurseForge, or Prism pack")]
    FormatUnrecognized { path: PathBuf },

    /// A recognized pack format that this build cannot import.
    #[error("importing {format} packs is not supported")]
    FormatUnsupported { format: PackFormat },

    /// The archive is missing the manifest its format is defined by.
    #[error("the archive has no `{entry}`")]
    ManifestMissing { entry: &'static str },

    /// The manifest is not the JSON document the format specifies.
    #[error("`{entry}` is not a valid pack manifest")]
    ManifestInvalid {
        entry: String,
        #[source]
        source: serde_json::Error,
    },

    /// The manifest declares a format revision this build does not implement.
    #[error("`{entry}` declares format version {found}, newer than the supported {supported}")]
    ManifestUnsupportedVersion {
        entry: &'static str,
        found: u64,
        supported: u64,
    },

    /// The pack is for something other than Minecraft.
    #[error("`{game}` is not a game Ephervex can import")]
    GameUnsupported { game: String },

    /// The pack does not say which Minecraft version it is for.
    #[error("the pack declares no Minecraft version")]
    MinecraftDependencyMissing,

    /// Fabric, Quilt, or anything else outside the supported set
    #[error("`{loader}` is not a mod loader Ephervex supports")]
    LoaderUnsupported { loader: String },

    /// A pack declaring both Forge and NeoForge is ambiguous rather than
    /// clever, so Ephervex treats it as invalid.
    #[error("the pack declares both `{first}` and `{second}`")]
    LoaderAmbiguous { first: String, second: String },

    /// An entry names a path that would land outside the destination.
    #[error("the archive entry `{entry}` escapes the extraction directory")]
    EntryPathUnsafe { entry: String },

    /// An entry would write into launcher-owned metadata.
    #[error("the archive entry `{entry}` would overwrite launcher metadata")]
    EntryReserved { entry: String },

    /// An entry is a symlink or another type a pack has no business carrying.
    #[error("the archive entry `{entry}` is a {kind}, which is never extracted")]
    EntryTypeUnsupported { entry: String, kind: &'static str },

    /// The archive exceeds one of the configured extraction limits.
    #[error("the archive exceeds the {limit} extraction limit")]
    LimitExceeded {
        limit: &'static str,
        allowed: u64,
        reached: u64,
    },

    /// A pack file carries no hash, so a download could not be verified
    #[error("the pack file `{path}` has no usable hash")]
    FileHashesMissing { path: String },

    /// A pack file carries no download address and no physical copy.
    #[error("the pack file `{path}` has no download address")]
    FileDownloadsMissing { path: String },

    /// Every download address for a file was refused by the URL policy
    #[error("no permitted download address for the pack file `{path}`")]
    DownloadUrlRejected {
        path: String,
        #[source]
        source: UrlPolicyError,
    },

    /// A pack file could not be fetched, or its hash did not match.
    #[error("the pack file `{path}` could not be downloaded")]
    DownloadFailed {
        path: String,
        #[source]
        source: Box<DownloadError>,
    },

    /// A file could not be written into the instance.
    #[error("could not write {path}")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The retained source recipe could not be read.
    #[error("the retained pack recipe at {path} could not be read")]
    RecipeUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The retained source recipe is not valid.
    #[error("the retained pack recipe at {path} is not valid")]
    RecipeInvalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// The retained recipe was written by a newer build.
    #[error("the retained pack recipe at {path} has version {found}, newer than {supported}")]
    RecipeUnsupportedVersion {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    /// The user canceled the import.
    #[error("the import was canceled")]
    Canceled,
}

impl PackError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "error-pack-archive-unreadable",
            Self::ArchiveInvalid { .. } => "error-pack-archive-invalid",
            Self::FormatUnrecognized { .. } => "error-pack-format-unrecognized",
            Self::FormatUnsupported { .. } => "error-pack-format-unsupported",
            Self::ManifestMissing { .. } => "error-pack-manifest-missing",
            Self::ManifestInvalid { .. } => "error-pack-manifest-invalid",
            Self::ManifestUnsupportedVersion { .. } => "error-pack-manifest-unsupported-version",
            Self::GameUnsupported { .. } => "error-pack-game-unsupported",
            Self::MinecraftDependencyMissing => "error-pack-minecraft-missing",
            Self::LoaderUnsupported { .. } => "error-pack-loader-unsupported",
            Self::LoaderAmbiguous { .. } => "error-pack-loader-ambiguous",
            Self::EntryPathUnsafe { .. } => "error-pack-entry-unsafe",
            Self::EntryReserved { .. } => "error-pack-entry-reserved",
            Self::EntryTypeUnsupported { .. } => "error-pack-entry-type-unsupported",
            Self::LimitExceeded { .. } => "error-pack-limit-exceeded",
            Self::FileHashesMissing { .. } => "error-pack-file-hashes-missing",
            Self::FileDownloadsMissing { .. } => "error-pack-file-downloads-missing",
            Self::DownloadUrlRejected { .. } => "error-pack-download-url-rejected",
            Self::DownloadFailed { .. } => "error-pack-download-failed",
            Self::Unwritable { .. } => "error-pack-write-failed",
            Self::RecipeUnreadable { .. } => "error-pack-recipe-unreadable",
            Self::RecipeInvalid { .. } => "error-pack-recipe-invalid",
            Self::RecipeUnsupportedVersion { .. } => "error-pack-recipe-unsupported-version",
            Self::Canceled => "error-pack-canceled",
        }
    }

    /// The file this failure is about, where it names one.
    ///
    /// Frontends want to tell the user which file to look at without matching
    /// on this enum, which is what keeps the pack error hierarchy off their
    /// compile-time surface.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Unreadable { path, .. }
            | Self::ArchiveInvalid { path, .. }
            | Self::FormatUnrecognized { path }
            | Self::Unwritable { path, .. }
            | Self::RecipeUnreadable { path, .. }
            | Self::RecipeInvalid { path, .. }
            | Self::RecipeUnsupportedVersion { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    /// Whether the user stopped this rather than something failing.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled)
    }

    /// The loader a pack asked for that this launcher does not support.
    ///
    /// an unsupported-loader error reaches the user as an attention reason
    /// naming the loader, so the name has to be reachable without matching on
    /// the error.
    #[must_use]
    pub fn unsupported_loader(&self) -> Option<&str> {
        match self {
            Self::LoaderUnsupported { loader } => Some(loader),
            _ => None,
        }
    }
}

/// Shorthand for a pack result.
pub type PackResult<T> = Result<T, PackError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn every_message_id_is_bundled_in_the_english_resource() {
        // An id with no message renders as a placeholder in the user's face.
        for error in [
            PackError::Unreadable {
                path: PathBuf::from("/tmp/a.mrpack"),
                source: std::io::Error::other("x"),
            },
            PackError::FormatUnrecognized {
                path: PathBuf::from("/tmp/a.zip"),
            },
            PackError::FormatUnsupported {
                format: PackFormat::CurseForgePack,
            },
            PackError::ManifestMissing {
                entry: "modrinth.index.json",
            },
            PackError::ManifestUnsupportedVersion {
                entry: "modrinth.index.json",
                found: 2,
                supported: 1,
            },
            PackError::GameUnsupported {
                game: "terraria".to_owned(),
            },
            PackError::MinecraftDependencyMissing,
            PackError::LoaderUnsupported {
                loader: "fabric-loader".to_owned(),
            },
            PackError::LoaderAmbiguous {
                first: "forge".to_owned(),
                second: "neoforge".to_owned(),
            },
            PackError::EntryPathUnsafe {
                entry: "../x".to_owned(),
            },
            PackError::EntryReserved {
                entry: ".launcher/instance.json".to_owned(),
            },
            PackError::EntryTypeUnsupported {
                entry: "mods/link".to_owned(),
                kind: "symlink",
            },
            PackError::LimitExceeded {
                limit: "entry-count",
                allowed: 1,
                reached: 2,
            },
            PackError::FileHashesMissing {
                path: "mods/a.jar".to_owned(),
            },
            PackError::FileDownloadsMissing {
                path: "mods/a.jar".to_owned(),
            },
            PackError::Canceled,
        ] {
            assert!(
                ephervex_i18n::has(error.message_id()),
                "missing message id: {}",
                error.message_id()
            );
        }
    }

    #[test]
    fn an_unsupported_loader_names_itself_without_a_match_on_the_enum() {
        let error = PackError::LoaderUnsupported {
            loader: "quilt-loader".to_owned(),
        };
        assert_eq!(error.unsupported_loader(), Some("quilt-loader"));
        assert_eq!(PackError::Canceled.unsupported_loader(), None);
        assert!(PackError::Canceled.is_cancellation());
    }
}
