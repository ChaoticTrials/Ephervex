//! Browsing a mod catalog, and working out what installing a mod involves.
//!
//! ```text
//! provider API ──▶ neutral model ──▶ resolver ──▶ ModInstallPlan
//!      │                  │              │
//!  (Modrinth)      (no provider       (no provider
//!                   vocabulary)        vocabulary)
//! ```
//!
//! # The boundary this crate is built around
//!
//! Exactly one module knows what Modrinth is ([`modrinth`]). Everything
//! downstream — [`model`], [`resolve`], and every caller above this crate — is
//! provider-neutral, because a second catalog is a translation problem and not
//! a second launcher. Adding CurseForge is a module beside [`modrinth`] plus a
//! [`ModProvider`](ephervex_model::ModProvider) variant; it is not a change to
//! the resolver, the plan, or anything the services layer calls.
//!
//! # What this crate refuses to guess
//!
//! Nearly everything here is a refusal, and each one is a case where guessing
//! would produce an instance that reproduces something other than the bug being
//! chased:
//!
//! * an exactly chosen version that does not fit is never swapped for a
//!   neighbour;
//! * two mods pinning one project to different versions is a conflict, not an
//!   arbitration;
//! * a required dependency that names a bare file fails rather than being
//!   skipped or given an invented project identity;
//! * a version with two files claiming to be primary is malformed rather than a
//!   coin toss;
//! * the instance's Minecraft version and loader are read-only here. A graph
//!   that cannot be satisfied against them fails.
//!
//! # Nothing here writes
//!
//! This crate reads a catalog and produces a plan. Downloading, copying, and
//! recording belong to `ephervex-pack`, and sequencing them belongs to
//! `ephervex-core`.

pub mod error;
pub mod model;
pub mod modrinth;
pub mod provider;
pub mod resolve;

#[cfg(test)]
mod test_support;

pub use error::{ModPlatformError, ModPlatformResult};
pub use model::{
    AnnotatedVersion, Compatibility, DEFAULT_SEARCH_LIMIT, DependencyKind, IncompatibilityReason,
    InstanceTarget, MAX_SEARCH_LIMIT, ModDependency, ModFile, ModProject, ModProjectDetails,
    ModSearchHit, ModSearchQuery, ModSearchResults, ModVersionInfo, ReleaseChannel, SideSupport,
};
pub use modrinth::{MODRINTH_API_BASE, ModrinthProvider};
pub use provider::{ModProviderApi, loader_name_for};
pub use resolve::{
    EmbeddedNote, ExternalDependency, InstallSelection, InstalledProject, ModInstallPlan,
    OptionalCandidate, PlanWarning, PlannedMod, ResolveRequest, annotate, newest_compatible,
    resolve,
};
