//! Mojang/Piston version metadata and the vanilla installation and launch
//! pipeline.
//!
//! Minecraft metadata support built the metadata half: discovering which Minecraft releases
//! Ephervex offers, and resolving one release's complete description. Vanilla launch support
//! built the pipeline that acts on it - downloading the client jar, assets,
//! libraries and natives, constructing a launch, and supervising the game.
//!
//! # Installation pipeline
//!
//! ```text
//! VersionCatalog::resolve        version metadata, verified against its digest
//!   -> InstallationPlan::resolve every path and artifact, no I/O
//!   -> install::install          download, verify, unpack natives
//!   -> launch::build_command     rules, classpath, placeholders
//!   -> GameProcess::start/wait   spawn, stream, exit code
//! ```
//!
//! Choosing the Java runtime and refreshing the session are the two steps that
//! are *not* here: they belong to `ephervex-java` and `ephervex-auth`, and
//! `ephervex-core` sequences all four crates. This one deliberately does not
//! depend on either, which is what keeps the launch pipeline testable without a
//! JVM or a Microsoft account.
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
//! | [`version_id`] | [`MinecraftVersionId`] - parsing and ordering both published schemes |
//! | [`manifest`] | `version_manifest_v2.json` and the release filter |
//! | [`version`] | one version's JSON: downloads, assets, Java requirement, `inheritsFrom` |
//! | [`library`] | libraries, artifacts, and both eras of natives |
//! | [`arguments`] | the legacy string and the modern rule-bearing object, unified |
//! | [`rules`] | OS/architecture/feature conditions, and their evaluation against a launch |
//! | [`catalog`] | [`VersionCatalog`] - fetching and caching, on top of `ephervex-download` |
//! | [`platform`] | the OS, architecture, and OS version a launch targets, in Mojang's spelling |
//! | [`assets`] | the asset index, and the objects it names |
//! | [`language`] | per-release language catalogs, from Mojang's own asset metadata |
//! | [`install`] | [`InstallationPlan`] - every path and artifact - and fetching them |
//! | [`natives`] | unpacking pre-1.19 classifier jars within extraction limits |
//! | [`launch`] | classpath, argument rules, and placeholder substitution |
//! | [`process`] | spawning the game, streaming its output, tracking its exit |
//! | [`log`] | reassembling the log4j records Mojang's own logging config produces |
//!
//! # Boundaries
//!
//! * **`inheritsFrom` is explicit.** Forge and NeoForge publish *child* version
//!   JSONs that extend a vanilla parent. A raw child remains detectably
//!   incomplete; [`VersionMetadata::inherit`] validates and merges it before
//!   the shared installation and launch pipeline sees it.
//! * **The Java requirement is recorded, not acted on here.**
//!   [`VersionMetadata::java_major_version`] and the runtime `component` name
//!   are carried through for `ephervex-java`; this crate takes
//!   the chosen executable as an input.
//! * **The session is an input, not a chain.** [`launch::PlayerIdentity`] is
//!   six strings, so nothing here depends on `ephervex-auth` - and both account
//!   kinds are converted explicitly by `ephervex-core` rather than being
//!   inferred from whether a token is present.

pub mod arguments;
pub mod assets;
pub mod catalog;
pub mod error;
pub mod install;
pub mod language;
pub mod launch;
pub mod library;
pub mod log;
pub mod manifest;
pub mod natives;
pub mod platform;
pub mod process;
pub mod rules;
pub mod version;
pub mod version_id;

#[cfg(test)]
mod test_support;

pub use arguments::{ArgumentEntry, ArgumentForm, ArgumentGroup, Arguments};
pub use assets::{AssetIndex, AssetIndexError, AssetObject};
pub use catalog::{CachedManifest, ReleaseListing, VersionCatalog};
pub use error::{LaunchError, LaunchResult, MinecraftError, MinecraftResult};
pub use install::{
    ASSET_RESOURCE_BASE, ExcludedLibrary, ExclusionReason, Installation, InstallationPlan,
    PlannedArtifact, PlannedLogging, PlannedNatives, VanillaInstaller,
};
pub use language::{LANGUAGE_METADATA_OBJECT, LanguageCatalog, LanguageCatalogs, LanguageEntry};
pub use launch::{
    JvmOptions, LEGACY_JVM_ARGUMENTS, LaunchCommand, LaunchFeatures, LaunchSettings,
    OFFLINE_ACCESS_TOKEN, PlayerIdentity, build_command,
};
pub use library::{Artifact, ExtractRules, Library, LibraryDownloads, LibraryError};
pub use log::{Log4jReader, LogRecord};
pub use manifest::{
    LatestVersions, ManifestError, SupportBasis, VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL,
    VersionManifest, VersionRecord,
};
pub use natives::{NativesError, NativesReport};
pub use platform::Platform;
pub use process::{
    EARLY_OUTPUT_LINES, GameExit, GameProcess, LaunchEvent, LaunchEvents, LaunchObserver,
    OutputLine, OutputStream,
};
pub use rules::{
    OsCondition, OsName, OsVersionCondition, Rule, RuleAction, RuleContext, rules_allow,
};
pub use version::{
    AssetIndexRef, JavaRequirement, LoggingConfig, ReleaseType, VersionMetadata,
    VersionMetadataError, VersionParseError,
};
pub use version_id::{MINIMUM_SUPPORTED_VERSION, MinecraftVersionId};
