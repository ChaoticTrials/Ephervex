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

use crate::credentials::CredentialUnavailable;
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

    /// The manifest describes something other than a Minecraft modpack.
    ///
    /// A CurseForge export manifest states the kind of pack it is, and an
    /// export for another game or another product resolves perfectly well
    /// against this parser while meaning nothing to a Minecraft instance.
    #[error("`{entry}` describes `{found}`, which is not a Minecraft modpack")]
    ManifestTypeUnsupported { entry: &'static str, found: String },

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

    /// A declared entry names a catalog id that cannot identify anything.
    ///
    /// Zero, negative, or too large for the id space. Refused rather than
    /// coerced: an approximated id names another project's file.
    #[error("the pack entry's `{field}` is `{value}`, which names nothing")]
    PackEntryIdInvalid { field: &'static str, value: String },

    /// One project is declared with two different files.
    ///
    /// There is no honest way to install both: they are two versions of one
    /// mod, and the desired state has room for one.
    #[error(
        "the pack declares project `{project_id}` as both file `{first_file}` and `{second_file}`"
    )]
    PackEntryConflict {
        project_id: String,
        first_file: String,
        second_file: String,
    },

    /// One file is declared both as part of the pack and as an optional choice.
    #[error("the pack declares file `{file_id}` of `{project_id}` as both required and optional")]
    PackEntryInclusionConflict { project_id: String, file_id: String },

    /// A pack declares a project whose provider-managed content class this
    /// build does not import.
    ///
    /// Distinct from a mod browser refusing the same project: a modpack
    /// import legitimately imports more than mods (a CurseForge Resource Pack,
    /// for instance), so this names the *pack's* refusal of a class it still
    /// does not know how to place, rather than reusing the mod browser's "not
    /// a mod" wording.
    #[error(
        "the pack declares project `{reference}` as {provider} content of type `{project_type}`, which Ephervex cannot import"
    )]
    ProviderContentUnsupported {
        provider: ephervex_model::ModProvider,
        reference: String,
        project_type: String,
    },

    /// Materialization was asked for while the pack still had files only its
    /// provider can describe.
    ///
    /// A programming error rather than a bad pack, and it fails loudly for the
    /// reason every other refusal here does: laying down a pack missing the
    /// files nobody resolved would record an instance's desired state as
    /// something the pack never said.
    #[error("{remaining} of the pack's files have not been resolved with their provider")]
    ProviderFilesUnresolved { remaining: usize, first: String },

    /// Materialization was asked for while the pack still had files a
    /// provider refused to serve, and nothing said what to do about them.
    ///
    /// Distinct from [`Self::ProviderFilesUnresolved`]: those files were never
    /// asked about at all, while these were asked about and refused. Neither
    /// is laid down silently — a caller resolves a refusal by finding a
    /// verified copy in the shared cache or by an explicit skip, and reaching
    /// here means neither happened for every one of them.
    #[error("{remaining} of the pack's files are not available for automatic download")]
    ProviderFilesBlocked { remaining: usize, first: String },

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

    /// A file the launcher just installed could not be read back to be
    /// recorded.
    ///
    /// Distinct from [`Self::Unreadable`], which is about the archive coming
    /// in. This one is about the installed tree going out: without a digest
    /// there is nothing to record the file as, so the installation is refused
    /// rather than written down as content whose identity is a guess.
    #[error("the installed file {path} could not be measured")]
    FileUnmeasurable {
        path: PathBuf,
        #[source]
        source: Box<DownloadError>,
    },

    /// An artifact's provider serves it only to an authenticated request, the
    /// verified copy is not in the shared cache, and no credential could be
    /// obtained.
    ///
    /// Deliberately not a download failure. Nothing was sent and nothing was
    /// refused: the missing piece is a setting, and the provider layer's own
    /// error says which of "absent", "unreadable", or "rejected" it is so a
    /// frontend can send the user to the right place.
    #[error("`{path}` needs a provider credential that is not available")]
    ArtifactCredentialUnavailable {
        path: String,
        #[source]
        source: CredentialUnavailable,
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

    /// A linked imported client no longer has the retained recipe required to
    /// reconstruct its server without the original archive.
    #[error("the client has no retained pack recipe at {path}")]
    RecipeMissing { path: PathBuf },

    /// A retained override layer named by the recipe is absent or unusable.
    #[error("the retained {scope} override layer is unavailable at {path}")]
    RetainedLayerUnavailable { scope: &'static str, path: PathBuf },

    /// A caller tried to confirm an entry that is not unknown on the server.
    #[error("`{path}` is not a recipe entry with unknown server applicability")]
    UnknownServerSelection { path: PathBuf },

    /// The recipe and linked client disagree about their exact target.
    #[error("the retained recipe target does not match the linked client")]
    RecipeTargetMismatch {
        client_minecraft: String,
        recipe_minecraft: String,
        client_loader: String,
        recipe_loader: String,
    },

    /// Two selected recipe entries claim one server path.
    #[error("two selected recipe files both install as `{path}`")]
    RecipeDestinationCollision { path: PathBuf },

    /// A selected recipe file would overwrite a file from the base server.
    #[error("the selected recipe file would overwrite {path}")]
    RecipeDestinationOccupied { path: PathBuf },

    /// A file was recorded as a managed mod without the identity that makes it
    /// one. A record nothing can re-resolve is worse than no record.
    #[error("`{path}` cannot be recorded as a managed mod without a provider and project")]
    ManagedIdentityMissing { path: String },

    /// A provider operation named a jar the user put in `mods/` themselves.
    ///
    /// Reachable because a hand-placed jar a catalog recognized carries the
    /// same provider and project id an installed mod does, so it answers a
    /// lookup by project exactly as one. What separates them is origin, and
    /// origin is why this refuses: adoption recorded what the file is, it did
    /// not make the file the launcher's to retire or replace.
    #[error("`{}` was added by hand, so no provider operation may replace or remove it", path.display())]
    HandPlacedNotMutable { path: PathBuf },

    /// The project is already installed at a different version.
    ///
    /// Changing an installed mod's version is a mod *update*, which this build
    /// does not implement — so this refuses rather than replacing a jar the
    /// user did not ask about.
    #[error(
        "`{project_id}` is already installed from {provider} at version `{installed}`, not `{requested}`"
    )]
    ManagedVersionConflict {
        provider: ephervex_model::ModProvider,
        project_id: String,
        installed: String,
        requested: String,
    },

    /// Something is already at a planned destination, and it is not the managed
    /// mod being reused.
    ///
    /// Almost always a jar the user put there themselves. Overwriting it would
    /// destroy the local build or the attached bug-report file that is very
    /// likely the whole point of the instance.
    #[error("`{path}` already exists and was not installed by Ephervex")]
    DestinationOccupied { path: PathBuf },

    /// Two mods in one installation resolve to the same file name.
    ///
    /// Installing them in sequence would leave whichever landed last, silently
    /// producing an instance missing a mod the plan promised.
    #[error("two mods in this installation both install as `{name}`")]
    DestinationCollision { name: String },

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
            Self::ManifestTypeUnsupported { .. } => "error-pack-manifest-type-unsupported",
            Self::GameUnsupported { .. } => "error-pack-game-unsupported",
            Self::MinecraftDependencyMissing => "error-pack-minecraft-missing",
            Self::LoaderUnsupported { .. } => "error-pack-loader-unsupported",
            Self::LoaderAmbiguous { .. } => "error-pack-loader-ambiguous",
            Self::EntryPathUnsafe { .. } => "error-pack-entry-unsafe",
            Self::EntryReserved { .. } => "error-pack-entry-reserved",
            Self::EntryTypeUnsupported { .. } => "error-pack-entry-type-unsupported",
            Self::LimitExceeded { .. } => "error-pack-limit-exceeded",
            Self::PackEntryIdInvalid { .. } => "error-pack-entry-id-invalid",
            Self::PackEntryConflict { .. } => "error-pack-entry-conflict",
            Self::PackEntryInclusionConflict { .. } => "error-pack-entry-inclusion-conflict",
            Self::ProviderContentUnsupported { .. } => "error-pack-provider-content-unsupported",
            Self::ProviderFilesUnresolved { .. } => "error-pack-provider-files-unresolved",
            Self::ProviderFilesBlocked { .. } => "error-pack-provider-files-blocked",
            Self::FileHashesMissing { .. } => "error-pack-file-hashes-missing",
            Self::FileDownloadsMissing { .. } => "error-pack-file-downloads-missing",
            Self::DownloadUrlRejected { .. } => "error-pack-download-url-rejected",
            Self::DownloadFailed { .. } => "error-pack-download-failed",
            Self::FileUnmeasurable { .. } => "error-pack-file-unmeasurable",
            Self::ArtifactCredentialUnavailable { .. } => {
                "error-pack-artifact-credential-unavailable"
            }
            Self::Unwritable { .. } => "error-pack-write-failed",
            Self::RecipeUnreadable { .. } => "error-pack-recipe-unreadable",
            Self::RecipeInvalid { .. } => "error-pack-recipe-invalid",
            Self::RecipeUnsupportedVersion { .. } => "error-pack-recipe-unsupported-version",
            Self::RecipeMissing { .. } => "error-pack-recipe-missing",
            Self::RetainedLayerUnavailable { .. } => "error-pack-retained-layer-unavailable",
            Self::UnknownServerSelection { .. } => "error-pack-unknown-server-selection",
            Self::RecipeTargetMismatch { .. } => "error-pack-recipe-target-mismatch",
            Self::RecipeDestinationCollision { .. } => "error-pack-recipe-destination-collision",
            Self::RecipeDestinationOccupied { .. } => "error-pack-recipe-destination-occupied",
            Self::ManagedIdentityMissing { .. } => "error-mod-managed-identity-missing",
            Self::HandPlacedNotMutable { .. } => "error-mod-hand-placed-not-mutable",
            Self::ManagedVersionConflict { .. } => "error-mod-already-installed",
            Self::DestinationOccupied { .. } => "error-mod-destination-occupied",
            Self::DestinationCollision { .. } => "error-mod-destination-collision",
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
            | Self::RecipeUnsupportedVersion { path, .. }
            | Self::RecipeMissing { path }
            | Self::RetainedLayerUnavailable { path, .. }
            | Self::UnknownServerSelection { path }
            | Self::RecipeDestinationCollision { path }
            | Self::RecipeDestinationOccupied { path }
            | Self::DestinationOccupied { path }
            | Self::HandPlacedNotMutable { path }
            | Self::FileUnmeasurable { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    /// The values this failure's message and its next step take as arguments,
    /// beyond the path [`Self::failing_path`] already exposes.
    ///
    /// One list per variant rather than an accessor per field, so a new
    /// refusal carries its arguments beside its message id. Split across two
    /// helpers purely for length; the split has no meaning a caller depends
    /// on.
    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        self.manifest_and_entry_args()
            .or_else(|| self.file_and_recipe_args())
            .unwrap_or_default()
    }

    /// Arguments for refusals about the manifest and the entries it declares.
    fn manifest_and_entry_args(&self) -> Option<Vec<(&'static str, String)>> {
        let args = match self {
            // Names the detected format so the user is told what the file
            // *is*, rather than only that it is not importable.
            Self::FormatUnsupported { format } => vec![("format", format.to_string())],
            Self::ManifestMissing { entry } => vec![("entry", (*entry).to_owned())],
            Self::ManifestUnsupportedVersion {
                entry,
                found,
                supported,
            } => vec![
                ("entry", (*entry).to_owned()),
                ("found", found.to_string()),
                ("supported", supported.to_string()),
            ],
            Self::ManifestTypeUnsupported { entry, found } => {
                vec![("entry", (*entry).to_owned()), ("found", found.clone())]
            }
            Self::GameUnsupported { game } => vec![("game", game.clone())],
            Self::PackEntryIdInvalid { field, value } => {
                vec![("field", (*field).to_owned()), ("value", value.clone())]
            }
            Self::PackEntryConflict {
                project_id,
                first_file,
                second_file,
            } => vec![
                ("project", project_id.clone()),
                ("first", first_file.clone()),
                ("second", second_file.clone()),
            ],
            Self::PackEntryInclusionConflict {
                project_id,
                file_id,
            } => vec![
                ("project", project_id.clone()),
                ("file_id", file_id.clone()),
            ],
            Self::ProviderContentUnsupported {
                provider,
                reference,
                project_type,
            } => vec![
                ("provider", provider.to_string()),
                ("reference", reference.clone()),
                ("project_type", project_type.clone()),
            ],
            Self::ProviderFilesUnresolved { remaining, first }
            | Self::ProviderFilesBlocked { remaining, first } => vec![
                ("remaining", remaining.to_string()),
                ("project", first.clone()),
            ],
            Self::LoaderAmbiguous { first, second } => {
                vec![("first", first.clone()), ("second", second.clone())]
            }
            Self::ManifestInvalid { entry, .. }
            | Self::EntryPathUnsafe { entry }
            | Self::EntryReserved { entry }
            | Self::EntryTypeUnsupported { entry, .. } => vec![("entry", entry.clone())],
            Self::LimitExceeded {
                limit,
                allowed,
                reached,
            } => vec![
                ("limit", (*limit).to_owned()),
                ("allowed", allowed.to_string()),
                ("reached", reached.to_string()),
            ],
            _ => return None,
        };
        Some(args)
    }

    /// Arguments for refusals about a pack's own files, its retained state,
    /// and its managed mods.
    fn file_and_recipe_args(&self) -> Option<Vec<(&'static str, String)>> {
        let args = match self {
            Self::ManagedIdentityMissing { path } => vec![("path", path.clone())],
            Self::ManagedVersionConflict {
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
            Self::DestinationCollision { name } => vec![("name", name.clone())],
            Self::FileHashesMissing { path }
            | Self::FileDownloadsMissing { path }
            | Self::DownloadUrlRejected { path, .. }
            | Self::DownloadFailed { path, .. }
            | Self::ArtifactCredentialUnavailable { path, .. } => vec![("file", path.clone())],
            Self::RecipeUnsupportedVersion {
                found, supported, ..
            } => vec![
                ("found", found.to_string()),
                ("supported", supported.to_string()),
            ],
            Self::RetainedLayerUnavailable { scope, .. } => {
                vec![("scope", (*scope).to_owned())]
            }
            Self::RecipeTargetMismatch {
                client_minecraft,
                recipe_minecraft,
                client_loader,
                recipe_loader,
            } => vec![
                ("client_minecraft", client_minecraft.clone()),
                ("recipe_minecraft", recipe_minecraft.clone()),
                ("client_loader", client_loader.clone()),
                ("recipe_loader", recipe_loader.clone()),
            ],
            _ => return None,
        };
        Some(args)
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

    /// One of each variant whose message takes arguments, plus the plain ones.
    ///
    /// Hand-maintained, and the only hand-maintained part: `message_id` matches
    /// every variant explicitly, so a new one fails to compile until it is
    /// handled there. Listing it here is what puts it in front of these tests.
    fn every_variant() -> Vec<PackError> {
        let mut variants = archive_and_manifest_variants();
        variants.extend(file_and_recipe_variants());
        variants
    }

    /// The refusals about the archive itself and the manifest inside it.
    fn archive_and_manifest_variants() -> Vec<PackError> {
        vec![
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
            PackError::ManifestTypeUnsupported {
                entry: "manifest.json",
                found: "wowAddonPack".to_owned(),
            },
            PackError::GameUnsupported {
                game: "terraria".to_owned(),
            },
            PackError::PackEntryIdInvalid {
                field: "projectID",
                value: "0".to_owned(),
            },
            PackError::PackEntryConflict {
                project_id: "238222".to_owned(),
                first_file: "1".to_owned(),
                second_file: "2".to_owned(),
            },
            PackError::PackEntryInclusionConflict {
                project_id: "238222".to_owned(),
                file_id: "1".to_owned(),
            },
            PackError::ProviderContentUnsupported {
                provider: ephervex_model::ModProvider::CurseForge,
                reference: "580181".to_owned(),
                project_type: "Worlds".to_owned(),
            },
            PackError::ProviderFilesUnresolved {
                remaining: 3,
                first: "238222".to_owned(),
            },
            PackError::ProviderFilesBlocked {
                remaining: 1,
                first: "238222".to_owned(),
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
        ]
    }

    /// The refusals about a pack's own files, its retained state, and its mods.
    ///
    /// Split from [`archive_and_manifest_variants`] only for length: the two
    /// together are the complete list the tests below run over.
    fn file_and_recipe_variants() -> Vec<PackError> {
        vec![
            PackError::FileHashesMissing {
                path: "mods/a.jar".to_owned(),
            },
            PackError::FileDownloadsMissing {
                path: "mods/a.jar".to_owned(),
            },
            PackError::RecipeMissing {
                path: PathBuf::from("/instance/.launcher/source/recipe.json"),
            },
            PackError::RetainedLayerUnavailable {
                scope: "server",
                path: PathBuf::from("/instance/.launcher/source/modrinth/server-overrides"),
            },
            PackError::UnknownServerSelection {
                path: PathBuf::from("mods/not-unknown.jar"),
            },
            PackError::RecipeTargetMismatch {
                client_minecraft: "1.20.1".to_owned(),
                recipe_minecraft: "1.20.2".to_owned(),
                client_loader: "Forge 47.2.0".to_owned(),
                recipe_loader: "Forge 48.0.0".to_owned(),
            },
            PackError::RecipeDestinationCollision {
                path: PathBuf::from("mods/same.jar"),
            },
            PackError::RecipeDestinationOccupied {
                path: PathBuf::from("/server/minecraft_server.jar"),
            },
            PackError::ManagedIdentityMissing {
                path: "mods/a.jar".to_owned(),
            },
            PackError::ManagedVersionConflict {
                provider: ephervex_model::ModProvider::Modrinth,
                project_id: "p".to_owned(),
                installed: "a".to_owned(),
                requested: "b".to_owned(),
            },
            PackError::DestinationOccupied {
                path: PathBuf::from("/instance/minecraft/mods/theirs.jar"),
            },
            PackError::DestinationCollision {
                name: "mods/a.jar".to_owned(),
            },
            PackError::ArtifactCredentialUnavailable {
                path: "mods/a.jar".to_owned(),
                source: crate::credentials::CredentialUnavailable::new(std::io::Error::other(
                    "a provider layer failure",
                )),
            },
            PackError::Canceled,
        ]
    }

    /// Everything the frontend supplies for one failure: the dedicated
    /// accessors plus whatever the variant states for itself.
    fn supplied_args(error: &PackError) -> Vec<(&'static str, String)> {
        let mut supplied = error.message_args();
        if let Some(path) = error.failing_path() {
            supplied.push(("path", path.display().to_string()));
        }
        if let Some(loader) = error.unsupported_loader() {
            supplied.push(("loader", loader.to_owned()));
        }
        supplied
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
        // A message naming a value its error does not supply renders a literal
        // `{$…}`, or costs the user the sentence telling them what to do next.
        for error in every_variant() {
            let args = supplied_args(&error);
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
            if let Some(action) =
                ephervex_i18n::lookup_attribute_args(error.message_id(), "action", supplied)
            {
                assert!(
                    !action.contains("{$"),
                    "the next step for `{}` has an unsupplied argument: {action}",
                    error.message_id()
                );
            }
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
