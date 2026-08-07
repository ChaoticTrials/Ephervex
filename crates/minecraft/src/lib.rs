//! Mojang/Piston version metadata and the vanilla installation and launch
//! pipeline .
//!
//! Minecraft metadata support built the metadata half: discovering which Minecraft releases
//! Ephervex offers, and resolving one release's complete description. Vanilla launch support
//! builds the pipeline that acts on it — downloading the client jar, assets,
//! libraries and natives, and constructing a launch.
//!
//! # What "supported" means
//!
//! Exactly two rules, and nothing else:
//!
//! ```text
//! type == "release"   AND   version >= 1.12.2
//! ```
//!
//! Never a filename pattern. See [`manifest`] for why that distinction has
//! already mattered once, and for how the floor is decided when Mojang
//! publishes an id in a scheme this build cannot parse.
//!
//! # Reading the crate
//!
//! | Module | Holds |
//! |---|---|
//! | [`version_id`] | [`MinecraftVersionId`] — parsing and ordering both published schemes |
//! | [`manifest`] | `version_manifest_v2.json` and the release filter |
//! | [`version`] | one version's JSON: downloads, assets, Java requirement, `inheritsFrom` |
//! | [`library`] | libraries, artifacts, and both eras of natives |
//! | [`arguments`] | the legacy string and the modern rule-bearing object, unified |
//! | [`rules`] | OS/architecture/feature conditions, preserved rather than evaluated |
//! | [`catalog`] | [`VersionCatalog`] — fetching and caching, on top of `ephervex-download` |
//!
//! # Deliberate gaps
//!
//! * **`inheritsFrom` is parsed but never merged.** Forge and NeoForge publish
//!   *child* version JSONs that extend a vanilla parent. The field is read and
//!   reported by [`VersionMetadata::inherits_from`], so a child document is
//!   detectable, but merging one onto its parent has loader-specific semantics
//!   and belongs to loader support. Until then a child parses and is explicitly *not*
//!   [`complete`](VersionMetadata::is_complete_vanilla).
//! * **Rules are not evaluated.** Feature conditions depend on how the user
//!   launches, which is not known here. Vanilla launch support evaluates them against a
//!   concrete launch context; see [`rules`].
//! * **The Java requirement is recorded, not acted on.**
//!   [`VersionMetadata::java_major_version`] and the runtime `component` name
//!   are carried through for Java runtime support, which discovers and selects runtimes
//!   .
//! * **The legacy argument form supplies no JVM arguments.** 1.12.2 declares
//!   none at all; the launcher is expected to know them. Vanilla launch support owns that
//!   default set — see [`arguments`].

pub mod arguments;
pub mod catalog;
pub mod error;
pub mod library;
pub mod manifest;
pub mod rules;
pub mod version;
pub mod version_id;

#[cfg(test)]
mod test_support;

pub use arguments::{ArgumentEntry, ArgumentForm, ArgumentGroup, Arguments};
pub use catalog::{CachedManifest, ReleaseListing, VersionCatalog};
pub use error::{MinecraftError, MinecraftResult};
pub use library::{Artifact, ExtractRules, Library, LibraryDownloads, LibraryError};
pub use manifest::{
    LatestVersions, ManifestError, SupportBasis, VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL,
    VersionManifest, VersionRecord,
};
pub use rules::{OsCondition, OsName, OsVersionCondition, Rule, RuleAction};
pub use version::{
    AssetIndexRef, JavaRequirement, LoggingConfig, ReleaseType, VersionMetadata,
    VersionMetadataError, VersionParseError,
};
pub use version_id::{MINIMUM_SUPPORTED_VERSION, MinecraftVersionId};
