//! Hardened archive extraction, the normalized pack recipe, and the pack
//! importers.
//!
//! # What this crate is for
//!
//! Turning an untrusted archive out of a bug report into an instance that
//! reproduces what the reporter had - and into a *record* of what the pack said,
//! so the same import can produce a matching server months later without the
//! archive.
//!
//! ```text
//! archive ──▶ detect ──▶ parse+validate ──▶ PackRecipe ──▶ materialize
//!                                   │                              │
//!                              (provider)                  (provider-neutral)
//! ```
//!
//! # The boundary this crate is built around
//!
//! Exactly one module knows which provider produced an archive
//! ([`import::parse_with`], which dispatches on [`detect::PackFormat`]).
//! Everything downstream - [`recipe::PackRecipe`], [`install`], [`store`] - is
//! provider-neutral because provider-specific formats are not the shape of the
//! problem. [`modrinth`] and [`curseforge`] are the two arms of that dispatch,
//! and adding the second changed neither the recipe, the installer, the
//! retained-source layout, nor anything above this crate.
//!
//! The two formats differ in one way the boundary had to absorb. A Modrinth
//! index describes its own files completely; a CurseForge manifest names them
//! by catalog id and leaves the rest to an API. So parsing may also yield
//! [`recipe::PendingProviderFile`] entries, which a layer that can reach a
//! provider turns into ordinary recipe entries before anything is
//! materialized.
//!
//! Provider-specific state lives in two clearly-labeled places:
//! [`recipe::ProviderFileIdentity`] and [`recipe::ProviderRecipeMetadata`].
//! Nothing in the launcher branches on their contents.
//!
//! # Security posture
//!
//! Everything here treats its input as hostile:
//!
//! * entry names are validated by [`path`], the same rules `ephervex-java`'s
//!   tar reader uses, so there is one definition of "safe path" in the
//!   workspace rather than one per archive format;
//! * launcher-owned `.launcher/` metadata can never be written by pack
//!   contents;
//! * symlink entries are refused outright;
//! * entry count and uncompressed size are bounded against a budget shared
//!   across every read of one archive;
//! * download addresses are checked against an explicit host allowlist, and a
//!   file with no verifiable hash is never fetched.

pub mod archive;
pub mod credentials;
pub mod curseforge;
pub mod detect;
pub mod error;
pub mod import;
pub mod install;
pub mod managed;
pub mod modrinth;
pub mod mutate;
pub mod path;
pub mod policy;
pub mod recipe;
pub mod server_pack;
pub mod store;

pub use archive::{EntryDecision, ExtractionLimits, ExtractionReport, PackArchive};
pub use credentials::{ArtifactCredentials, CredentialUnavailable, OpenArtifacts};
pub use detect::{
    CURSEFORGE_MANIFEST, MODRINTH_INDEX, PRISM_PACK, PackFormat, has_mrpack_extension, identify,
};
pub use ephervex_model::ModOrigin;
pub use error::{PackError, PackResult};
pub use import::{
    MaterializeRequest, MaterializedPack, PackAnalysis, analyze, has_retained_recipe, materialize,
    retained_recipe,
};
pub use install::{
    AppliedLayer, ArtifactFetch, FileInstall, MaterializedFiles, MaterializedServerRecipe,
    ServerMaterializeRequest, apply_overrides, apply_retained_overrides, install_files,
    materialize_server_recipe, restore_file,
};
pub use managed::{
    DISABLED_SUFFIX, MODS_DIR, ManagedModInstall, ManagedModInstallation, RESOURCEPACKS_DIR,
    SHADERPACKS_DIR, disabled_path, install_managed,
};
pub use mutate::{
    ManagedMutation, ManagedMutationReport, ManagedProject, MutationRecoveryAction,
    MutationRecoveryIssue, ReplacedMod, mutate_managed, projected, unique_retired_path,
};
pub use path::{RESERVED_METADATA_DIR, safe_file_name};
pub use policy::PackPolicy;
pub use recipe::{
    BlockedProviderFile, DuplicateFileReason, DuplicateProviderFile, ManagedMod, ModIdentity,
    OptionalComponent, OptionalSelection, OverrideLayer, OverrideScope, PackFileRecipe, PackRecipe,
    PackSide, PendingProviderFile, ProviderFileIdentity, ProviderRecipeMetadata, RECIPE_VERSION,
    RecipeRelation, RecordedMod, RelationTarget, ServerReconstructionPlan, SidePlan,
    SideRequirement, UnknownServerFiles,
};
pub use server_pack::{
    EULA_FILE, InstalledServerPack, LIBRARIES_DIR, ServerPackInstall, ServerPackMod,
    install_server_pack, measure_installed_mods, server_pack_recipe,
};
pub use store::{RECIPE_FILE, SourceStore, provider_directory};
