use std::path::PathBuf;

/// Failures produced while discovering or installing a loader.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoaderError {
    #[error("loader metadata for `{provider}` is unavailable")]
    MetadataUnavailable {
        provider: &'static str,
        #[source]
        source: Box<ephervex_download::MetadataCacheError>,
    },
    #[error("loader metadata for `{provider}` is invalid")]
    MetadataInvalid {
        provider: &'static str,
        detail: String,
    },
    #[error("{loader} has no {mode} build for minecraft `{minecraft}`")]
    VersionUnavailable {
        loader: ephervex_model::LoaderKind,
        minecraft: String,
        mode: &'static str,
    },
    #[error("{loader} has never had a build for minecraft `{minecraft}`")]
    LoaderNeverPublished {
        loader: ephervex_model::LoaderKind,
        minecraft: String,
        /// The oldest Minecraft release the loader was published for.
        since: &'static str,
    },
    #[error("{loader} requires an exact loader version")]
    ExactVersionRequired { loader: ephervex_model::LoaderKind },
    #[error("loader installer identity does not match the requested version: {detail}")]
    IdentityMismatch { detail: String },
    #[error("installer profile spec {spec} is not supported")]
    UnsupportedProfile { spec: u32 },
    #[error("the loader installer at {path} is invalid")]
    InstallerInvalid { path: PathBuf, detail: String },
    /// The install profile and the version metadata publish different files for one library.
    #[error(
        "the loader installer at {path} declares library `{library}` as {profile} in install_profile.json and as {version} in version.json"
    )]
    InstallerLibraryConflict {
        path: PathBuf,
        library: String,
        profile: ephervex_model::Hash,
        version: ephervex_model::Hash,
    },
    #[error("could not obtain loader artifact `{artifact}`")]
    Download {
        artifact: String,
        #[source]
        source: Box<ephervex_download::DownloadError>,
    },
    #[error("loader metadata contains an unsafe URL")]
    Url(#[from] ephervex_download::UrlPolicyError),
    #[error("loader metadata contains an unsafe cache key")]
    Cache(#[from] ephervex_download::CacheKeyError),
    #[error("loader version metadata is invalid")]
    Version(#[from] ephervex_minecraft::VersionParseError),
    #[error("loader version inheritance is invalid")]
    Inheritance(#[from] ephervex_minecraft::VersionMetadataError),
    #[error("vanilla installation failed")]
    Minecraft(#[from] ephervex_minecraft::LaunchError),
    #[error("io failure at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("loader processor `{processor}` failed with exit code {code:?}")]
    ProcessorFailed {
        processor: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("loader processor `{processor}` has no main class")]
    ProcessorMainClass { processor: String },
    #[error("loader processor output `{path}` was not produced")]
    ProcessorOutputMissing { path: PathBuf },
    #[error("server target {path} is not empty")]
    ServerTargetOccupied { path: PathBuf },
    #[error("loader installation was canceled")]
    Canceled,
}

impl LoaderError {
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::MetadataUnavailable { .. } => "error-loader-metadata-unavailable",
            Self::MetadataInvalid { .. } | Self::Url(_) | Self::Cache(_) => {
                "error-loader-metadata-invalid"
            }
            Self::VersionUnavailable { .. } | Self::ExactVersionRequired { .. } => {
                "error-loader-version-unavailable"
            }
            Self::LoaderNeverPublished { .. } => "error-loader-never-published",
            Self::IdentityMismatch { .. } => "error-loader-identity-mismatch",
            Self::UnsupportedProfile { .. } | Self::InstallerInvalid { .. } => {
                "error-loader-installer-invalid"
            }
            Self::InstallerLibraryConflict { .. } => "error-loader-installer-library-conflict",
            Self::Download { .. } => "error-loader-download-failed",
            Self::Version(_) | Self::Inheritance(_) => "error-loader-version-metadata-invalid",
            Self::Minecraft(error) => error.message_id(),
            Self::Io { .. } => "error-loader-io",
            Self::ProcessorFailed { .. }
            | Self::ProcessorMainClass { .. }
            | Self::ProcessorOutputMissing { .. } => "error-loader-processor-failed",
            Self::ServerTargetOccupied { .. } => "error-loader-server-target-occupied",
            Self::Canceled => "error-launch-canceled",
        }
    }

    /// The values this error's localized message needs.
    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::LoaderNeverPublished {
                loader,
                minecraft,
                since,
            } => vec![
                ("loader", loader.to_string()),
                ("minecraft", minecraft.clone()),
                ("since", (*since).to_owned()),
            ],
            Self::InstallerLibraryConflict { library, .. } => vec![("library", library.clone())],
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled)
    }

    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::InstallerInvalid { path, .. }
            | Self::InstallerLibraryConflict { path, .. }
            | Self::Io { path, .. }
            | Self::ProcessorOutputMissing { path }
            | Self::ServerTargetOccupied { path } => Some(path.clone()),
            _ => None,
        }
    }
}

pub type LoaderResult<T> = Result<T, LoaderError>;
