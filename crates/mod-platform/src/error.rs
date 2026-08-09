//! What browsing and resolving mods can fail with.
//!
//! Typed per concern and carrying structured data rather than pre-formatted
//! English: every variant exposes a stable `message_id()` that a frontend
//! resolves through `ephervex-i18n`.
//!
//! Most of these are *refusals*, and that is the point. A launcher whose job is
//! reproducing a bug must not paper over an ambiguous dependency graph: an
//! install that quietly picked one of two pinned versions, or that skipped a
//! dependency it could not identify, produces an instance that reproduces
//! something other than the report.

use ephervex_download::MetadataCacheError;
use ephervex_model::{LoaderKind, ModProvider};

/// Anything the mod platform can fail with.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModPlatformError {
    /// The provider could not be reached and nothing was cached.
    #[error("{provider} could not be reached")]
    Unreachable {
        provider: ModProvider,
        #[source]
        source: Box<MetadataCacheError>,
    },

    /// The provider answered with something this build cannot read.
    #[error("{provider} returned a response Ephervex could not read")]
    ResponseInvalid {
        provider: ModProvider,
        detail: String,
    },

    /// No such project.
    #[error("`{reference}` is not a project on {provider}")]
    ProjectUnknown {
        provider: ModProvider,
        reference: String,
    },

    /// The reference names something that is not a mod.
    #[error("`{reference}` is a {project_type}, not a mod")]
    ProjectTypeUnsupported {
        reference: String,
        project_type: String,
    },

    /// No such version.
    #[error("`{version_id}` is not a version of any project on {provider}")]
    VersionUnknown {
        provider: ModProvider,
        version_id: String,
    },

    /// A version has no file this launcher can install, or more than one that
    /// claims to be the primary.
    ///
    /// Upstream publishes at most one primary file per version. Two is a
    /// malformed version, and picking one would install a jar at random.
    #[error("version `{version_id}` does not name exactly one file to install")]
    AmbiguousPrimaryFile { version_id: String, found: usize },

    /// A file was published without a hash this launcher can verify.
    #[error("version `{version_id}` published no hash Ephervex can verify")]
    HashesMissing { version_id: String },

    /// The instance's loader cannot host mods at all.
    #[error("{loader} instances cannot install mods")]
    LoaderCannotHostMods { loader: LoaderKind },

    /// The exact version the user chose does not fit the instance.
    ///
    /// Never resolved by substituting a nearby release: an explicitly chosen
    /// version is the one thing this launcher must not second-guess.
    #[error("version `{version_id}` of `{project_id}` does not support {minecraft} with {loader}")]
    IncompatibleSelectedVersion {
        project_id: String,
        version_id: String,
        minecraft: String,
        loader: LoaderKind,
    },

    /// No version of a project fits the instance.
    #[error("no version of `{project_id}` supports {minecraft} with {loader}")]
    NoCompatibleVersion {
        project_id: String,
        minecraft: String,
        loader: LoaderKind,
    },

    /// Two mods require the same project pinned to different exact versions.
    #[error(
        "`{project_id}` is required at both `{first}` (by {first_source}) and `{second}` (by {second_source})"
    )]
    ConflictingPinnedVersions {
        project_id: String,
        first: String,
        first_source: String,
        second: String,
        second_source: String,
    },

    /// A mod in the graph declares another one incompatible.
    #[error("`{declared_by}` is incompatible with `{conflicting}`")]
    IncompatibleDependency {
        declared_by: String,
        conflicting: String,
    },

    /// A required dependency names a bare file rather than a project.
    ///
    /// Nothing can be resolved from a file name, and inventing a project
    /// identity for it would be worse than saying so.
    #[error("`{declared_by}` requires the file `{file_name}`, which names no project")]
    UnresolvableRequiredDependency {
        declared_by: String,
        file_name: String,
    },

    /// A dependency names both a project and a version that disagree.
    #[error(
        "a dependency of `{declared_by}` names project `{project_id}` and a version belonging to `{version_project_id}`"
    )]
    InconsistentDependency {
        declared_by: String,
        project_id: String,
        version_project_id: String,
    },

    /// A dependency names nothing at all.
    #[error("a dependency of `{declared_by}` identifies nothing to install")]
    MalformedDependency { declared_by: String },

    /// The instance already has this project at a different version.
    ///
    /// Replacing it is a mod *update*, which this build does not implement.
    /// Raised during planning, so the instance is never touched.
    #[error(
        "`{project_id}` is already installed from {provider} at `{installed}`, not `{requested}`"
    )]
    ProjectAlreadyInstalled {
        provider: ModProvider,
        project_id: String,
        installed: String,
        requested: String,
    },

    /// Resolution did not settle within its iteration budget.
    ///
    /// Should be unreachable — a pin strengthens at most once per project and a
    /// second distinct pin is a hard failure — so reaching it means an
    /// invariant is wrong. Failing loudly beats spinning forever inside a
    /// launcher.
    #[error("dependency resolution did not settle after {iterations} rounds")]
    ResolutionDidNotConverge { iterations: usize },

    /// The user canceled.
    #[error("the operation was canceled")]
    Canceled,
}

impl ModPlatformError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreachable { .. } => "error-mod-provider-unreachable",
            Self::ResponseInvalid { .. } => "error-mod-provider-response-invalid",
            Self::ProjectUnknown { .. } => "error-mod-project-unknown",
            Self::ProjectTypeUnsupported { .. } => "error-mod-project-type-unsupported",
            Self::VersionUnknown { .. } => "error-mod-version-unknown",
            Self::AmbiguousPrimaryFile { .. } => "error-mod-primary-file-ambiguous",
            Self::HashesMissing { .. } => "error-mod-hashes-missing",
            Self::LoaderCannotHostMods { .. } => "error-mod-loader-cannot-host-mods",
            Self::IncompatibleSelectedVersion { .. } => "error-mod-version-incompatible",
            Self::NoCompatibleVersion { .. } => "error-mod-no-compatible-version",
            Self::ConflictingPinnedVersions { .. } => "error-mod-conflicting-pins",
            Self::IncompatibleDependency { .. } => "error-mod-dependency-incompatible",
            Self::UnresolvableRequiredDependency { .. } => "error-mod-dependency-unresolvable",
            Self::InconsistentDependency { .. } => "error-mod-dependency-inconsistent",
            Self::MalformedDependency { .. } => "error-mod-dependency-malformed",
            // Shares an id with the pack crate's equivalent refusal: one
            // sentence for "that mod is already installed at another version",
            // however the launcher arrived at it.
            Self::ProjectAlreadyInstalled { .. } => "error-mod-already-installed",
            Self::ResolutionDidNotConverge { .. } => "error-mod-resolution-did-not-converge",
            Self::Canceled => "error-mod-canceled",
        }
    }

    /// The values this failure's message and its next step take as arguments.
    ///
    /// All untranslated technical text — project ids, version ids, file names,
    /// provider and loader names. Returned as one list rather than through an
    /// accessor per field so that a new variant carries its arguments beside
    /// its message id, and a frontend never has to know which placeholders a
    /// given message happens to use.
    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        self.catalog_args()
            .or_else(|| self.resolution_args())
            .unwrap_or_default()
    }

    /// Arguments for the failures that come from reading a catalog.
    fn catalog_args(&self) -> Option<Vec<(&'static str, String)>> {
        let args = match self {
            Self::Unreachable { provider, .. } | Self::ResponseInvalid { provider, .. } => {
                vec![("provider", provider.to_string())]
            }
            Self::ProjectUnknown {
                provider,
                reference,
            } => vec![
                ("provider", provider.to_string()),
                ("reference", reference.clone()),
            ],
            Self::ProjectTypeUnsupported {
                reference,
                project_type,
            } => vec![
                ("reference", reference.clone()),
                ("project_type", project_type.clone()),
            ],
            Self::VersionUnknown { version_id, .. } => {
                vec![("version_id", version_id.clone())]
            }
            Self::AmbiguousPrimaryFile { version_id, found } => vec![
                ("version_id", version_id.clone()),
                ("found", found.to_string()),
            ],
            Self::HashesMissing { version_id } => vec![("version_id", version_id.clone())],
            Self::LoaderCannotHostMods { loader } => vec![("loader", loader.to_string())],
            _ => return None,
        };
        Some(args)
    }

    /// Arguments for the failures that come from resolving a graph.
    fn resolution_args(&self) -> Option<Vec<(&'static str, String)>> {
        let args = match self {
            Self::IncompatibleSelectedVersion {
                project_id,
                version_id,
                minecraft,
                loader,
            } => vec![
                ("project", project_id.clone()),
                ("version_id", version_id.clone()),
                ("minecraft", minecraft.clone()),
                ("loader", loader.to_string()),
            ],
            Self::NoCompatibleVersion {
                project_id,
                minecraft,
                loader,
            } => vec![
                ("project", project_id.clone()),
                ("minecraft", minecraft.clone()),
                ("loader", loader.to_string()),
            ],
            Self::ConflictingPinnedVersions {
                project_id,
                first,
                first_source,
                second,
                second_source,
            } => vec![
                ("project", project_id.clone()),
                ("first", first.clone()),
                ("first_source", first_source.clone()),
                ("second", second.clone()),
                ("second_source", second_source.clone()),
            ],
            Self::IncompatibleDependency {
                declared_by,
                conflicting,
            } => vec![
                ("declared_by", declared_by.clone()),
                ("conflicting", conflicting.clone()),
            ],
            Self::UnresolvableRequiredDependency {
                declared_by,
                file_name,
            } => vec![
                ("declared_by", declared_by.clone()),
                ("file_name", file_name.clone()),
            ],
            Self::InconsistentDependency {
                declared_by,
                project_id,
                version_project_id,
            } => vec![
                ("declared_by", declared_by.clone()),
                ("project_id", project_id.clone()),
                ("version_project_id", version_project_id.clone()),
            ],
            Self::MalformedDependency { declared_by } => {
                vec![("declared_by", declared_by.clone())]
            }
            Self::ProjectAlreadyInstalled {
                provider,
                project_id,
                installed,
                requested,
            } => vec![
                ("provider", provider.to_string()),
                ("project", project_id.clone()),
                ("installed", installed.clone()),
                ("requested", requested.clone()),
            ],
            Self::ResolutionDidNotConverge { iterations } => {
                vec![("iterations", iterations.to_string())]
            }
            _ => return None,
        };
        Some(args)
    }

    /// Whether the user stopped this rather than something failing.
    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Canceled)
    }

    /// Whether the provider could not be reached, as opposed to answering with
    /// something unusable.
    ///
    /// A frontend offers "try again" for the first and not for the second.
    #[must_use]
    pub const fn is_offline(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }
}

/// Shorthand for a mod platform result.
pub type ModPlatformResult<T> = Result<T, ModPlatformError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// One of each variant.
    ///
    /// Hand-maintained, and the one place that is: `message_id` and
    /// `message_args` both match every variant explicitly, so a new one fails
    /// to compile until it is handled there. Adding it here is what puts it in
    /// front of the two tests below.
    fn every_variant() -> Vec<ModPlatformError> {
        vec![
            ModPlatformError::ResponseInvalid {
                provider: ModProvider::Modrinth,
                detail: String::new(),
            },
            ModPlatformError::ProjectUnknown {
                provider: ModProvider::Modrinth,
                reference: "nope".to_owned(),
            },
            ModPlatformError::ProjectTypeUnsupported {
                reference: "nope".to_owned(),
                project_type: "modpack".to_owned(),
            },
            ModPlatformError::VersionUnknown {
                provider: ModProvider::Modrinth,
                version_id: "nope".to_owned(),
            },
            ModPlatformError::AmbiguousPrimaryFile {
                version_id: "v".to_owned(),
                found: 2,
            },
            ModPlatformError::HashesMissing {
                version_id: "v".to_owned(),
            },
            ModPlatformError::LoaderCannotHostMods {
                loader: LoaderKind::Vanilla,
            },
            ModPlatformError::IncompatibleSelectedVersion {
                project_id: "p".to_owned(),
                version_id: "v".to_owned(),
                minecraft: "1.20.1".to_owned(),
                loader: LoaderKind::NeoForge,
            },
            ModPlatformError::NoCompatibleVersion {
                project_id: "p".to_owned(),
                minecraft: "1.20.1".to_owned(),
                loader: LoaderKind::NeoForge,
            },
            ModPlatformError::ConflictingPinnedVersions {
                project_id: "p".to_owned(),
                first: "a".to_owned(),
                first_source: "x".to_owned(),
                second: "b".to_owned(),
                second_source: "y".to_owned(),
            },
            ModPlatformError::IncompatibleDependency {
                declared_by: "a".to_owned(),
                conflicting: "b".to_owned(),
            },
            ModPlatformError::UnresolvableRequiredDependency {
                declared_by: "a".to_owned(),
                file_name: "b.jar".to_owned(),
            },
            ModPlatformError::InconsistentDependency {
                declared_by: "a".to_owned(),
                project_id: "p".to_owned(),
                version_project_id: "q".to_owned(),
            },
            ModPlatformError::MalformedDependency {
                declared_by: "a".to_owned(),
            },
            ModPlatformError::ProjectAlreadyInstalled {
                provider: ModProvider::Modrinth,
                project_id: "p".to_owned(),
                installed: "a".to_owned(),
                requested: "b".to_owned(),
            },
            ModPlatformError::Unreachable {
                provider: ModProvider::Modrinth,
                source: Box::new(MetadataCacheError::Io {
                    path: std::path::PathBuf::from("/cache"),
                    source: std::io::Error::other("unreachable"),
                }),
            },
            ModPlatformError::ResolutionDidNotConverge { iterations: 100 },
            ModPlatformError::Canceled,
        ]
    }

    #[test]
    fn every_message_id_is_bundled_in_the_english_resource() {
        // An id with no message renders as a placeholder in the user's face.
        for error in every_variant() {
            assert!(
                ephervex_i18n::has(error.message_id()),
                "missing message id: {}",
                error.message_id()
            );
        }
    }

    #[test]
    fn every_message_and_next_step_renders_with_the_arguments_its_error_supplies() {
        // A message that names a value its error does not supply renders a
        // literal `{$…}` in the user's face — or, worse, is dropped entirely,
        // costing the user the one sentence telling them what to do next.
        for error in every_variant() {
            let args = error.message_args();
            let supplied: Vec<(&str, ephervex_i18n::Arg<'_>)> = args
                .iter()
                .map(|(key, value)| (*key, ephervex_i18n::Arg::from(value.as_str())))
                .collect();

            let summary = ephervex_i18n::lookup_args(error.message_id(), supplied.clone());
            assert!(
                !summary.contains("{$"),
                "`{}` has an unsupplied argument: {summary}",
                error.message_id()
            );

            let action =
                ephervex_i18n::lookup_attribute_args(error.message_id(), "action", supplied);
            if let Some(action) = action {
                assert!(
                    !action.contains("{$"),
                    "the next step for `{}` has an unsupplied argument: {action}",
                    error.message_id()
                );
            }
        }
    }

    #[test]
    fn cancellation_and_offline_are_tellable_apart_without_matching_on_the_enum() {
        assert!(ModPlatformError::Canceled.is_cancellation());
        assert!(!ModPlatformError::Canceled.is_offline());
        assert!(
            !ModPlatformError::MalformedDependency {
                declared_by: "a".to_owned()
            }
            .is_cancellation()
        );
    }
}
