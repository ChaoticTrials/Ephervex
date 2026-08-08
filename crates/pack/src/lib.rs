//! Hardened archive extraction, the normalized pack recipe, and the pack
//! importers.
//!
//! # What this crate is for
//!
//! Turning an untrusted archive out of a bug report into an instance that
//! reproduces what the reporter had — and into a *record* of what the pack said,
//! so the same import can produce a matching server months later without the
//! archive.
//!
//! ```text
//! .mrpack ──▶ detect ──▶ parse+validate ──▶ PackRecipe ──▶ materialize
//!                                   │                              │
//!                              (provider)                  (provider-neutral)
//! ```
//!
//! # The boundary this crate is built around
//!
//! Exactly one module knows which provider produced an archive
//! ([`import::parse_with`], which dispatches on [`detect::PackFormat`]).
//! Everything downstream — [`recipe::PackRecipe`], [`install`], [`store`] — is
//! provider-neutral, because Modrinth is the first pack provider implemented
//! and not the shape of the problem. Adding CurseForge is a module beside
//! [`modrinth`] plus an arm in that dispatch; it is not a change to the recipe,
//! the installer, the retained-source layout, or anything above this crate.
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
pub mod detect;
pub mod error;
pub mod import;
pub mod install;
pub mod modrinth;
pub mod path;
pub mod policy;
pub mod recipe;
pub mod store;

pub use archive::{ExtractionLimits, ExtractionReport, PackArchive};
pub use detect::{
    CURSEFORGE_MANIFEST, MODRINTH_INDEX, PRISM_PACK, PackFormat, has_mrpack_extension, identify,
};
pub use error::{PackError, PackResult};
pub use import::{
    MaterializeRequest, MaterializedPack, PackAnalysis, analyze, has_retained_recipe, materialize,
    retained_recipe,
};
pub use install::{AppliedLayer, FileInstall, MaterializedFiles, apply_overrides, install_files};
pub use path::RESERVED_METADATA_DIR;
pub use policy::PackPolicy;
pub use recipe::{
    OptionalComponent, OptionalSelection, OverrideLayer, OverrideScope, PackFileRecipe, PackRecipe,
    PackSide, ProviderFileIdentity, ProviderRecipeMetadata, RECIPE_VERSION, SidePlan,
    SideRequirement,
};
pub use store::{RECIPE_FILE, SourceStore, provider_directory};
