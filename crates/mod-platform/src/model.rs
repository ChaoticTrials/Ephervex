//! The provider-neutral mod model.
//!
//! Modrinth's vocabulary stops at [`crate::modrinth`]. Everything above this
//! module — the resolver, the services layer, both frontends — is written
//! against the types here, so a second provider is a new translation rather
//! than a second everything.
//!
//! # Compatibility is a verdict with reasons, not a boolean
//!
//! A launcher that only filtered incompatible results would leave the user
//! staring at a mod that is obviously the right one and no explanation for why
//! it is not offered. [`Compatibility`] therefore carries *why*, and an
//! incompatible version is something a caller can display and refuse to
//! install — rather than something it silently never sees.

use std::fmt;

use chrono::{DateTime, Utc};
use ephervex_model::{Hash, LoaderKind, ModProvider};

/// The instance a search or a resolution is for.
///
/// Both fields are exact and neither is ever written by this crate. A mod that
/// does not fit an instance is a mod that does not fit; the instance is not
/// adjusted to make it fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceTarget {
    pub minecraft_version: String,
    pub loader: LoaderKind,
}

impl InstanceTarget {
    #[must_use]
    pub fn new(minecraft_version: impl Into<String>, loader: LoaderKind) -> Self {
        Self {
            minecraft_version: minecraft_version.into(),
            loader,
        }
    }
}

/// How much of a side a project declares support for.
///
/// Four states, faithfully. [`Unknown`](Self::Unknown) is an upstream value an
/// author can leave in place, not a parsing failure and not a synonym for
/// `Required`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SideSupport {
    Required,
    Optional,
    Unsupported,
    /// The author declared nothing for this side.
    #[default]
    Unknown,
}

/// One project, as a search result or a detail view needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModProject {
    pub provider: ModProvider,
    /// The provider's own id. Stable, and what everything is keyed on.
    pub id: String,
    /// The human-typable name, where the provider has one.
    pub slug: Option<String>,
    pub title: String,
    pub description: String,
    pub author: Option<String>,
    pub client_side: SideSupport,
    pub server_side: SideSupport,
    /// Where the project's icon is published, when it has one.
    ///
    /// An address, not an image: fetching it is a decision for a caller that
    /// knows about caches and download policy, and a project with no icon is an
    /// ordinary state rather than a failure.
    pub icon_url: Option<String>,
}

/// One file belonging to a version — the one this launcher would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    /// The provider's own file name. Untrusted input: it is validated as a safe
    /// relative path before it is allowed to name a destination.
    pub file_name: String,
    pub url: String,
    pub size: Option<u64>,
    /// Strongest first, so the shared cache path is derived from the best hash
    /// the provider published.
    pub hashes: Vec<Hash>,
}

/// How stable a release claims to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReleaseChannel {
    Release,
    Beta,
    Alpha,
}

impl ReleaseChannel {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Beta => "beta",
            Self::Alpha => "alpha",
        }
    }
}

/// What a mod says about another mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DependencyKind {
    Required,
    Optional,
    Incompatible,
    Embedded,
}

impl DependencyKind {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Incompatible => "incompatible",
            Self::Embedded => "embedded",
        }
    }
}

/// One relation, exactly as the provider stated it.
///
/// All three identity fields are optional because the provider's own schema
/// makes them so. [`crate::resolve::normalize`] turns this into something with
/// one shape; nothing else should interpret these fields directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDependency {
    pub kind: DependencyKind,
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub file_name: Option<String>,
}

/// One version of a project, with the single file this launcher would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModVersionInfo {
    pub provider: ModProvider,
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub version_number: String,
    pub channel: ReleaseChannel,
    /// The Minecraft versions this *version* supports. Not the project's union
    /// of all versions, which cannot answer a compatibility question.
    pub game_versions: Vec<String>,
    /// The loaders this version supports, in the provider's own spelling.
    pub loaders: Vec<String>,
    pub published_at: DateTime<Utc>,
    pub file: ModFile,
    pub dependencies: Vec<ModDependency>,
}

impl ModVersionInfo {
    /// Whether this version fits an instance, and if not, why not.
    ///
    /// Both halves are checked and both reasons are reported, because "wrong
    /// Minecraft version" and "wrong loader" send a user to different places.
    #[must_use]
    pub fn compatibility(&self, target: &InstanceTarget, loader_name: &str) -> Compatibility {
        let mut reasons = Vec::new();
        if !self
            .game_versions
            .iter()
            .any(|version| version == &target.minecraft_version)
        {
            reasons.push(IncompatibilityReason::MinecraftVersion {
                required: target.minecraft_version.clone(),
                supported: self.game_versions.clone(),
            });
        }
        if !self
            .loaders
            .iter()
            .any(|loader| loader.eq_ignore_ascii_case(loader_name))
        {
            reasons.push(IncompatibilityReason::Loader {
                required: target.loader,
                supported: self.loaders.clone(),
            });
        }

        if reasons.is_empty() {
            Compatibility::Compatible
        } else {
            Compatibility::Incompatible(reasons)
        }
    }
}

/// Why a version or project does not fit an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncompatibilityReason {
    MinecraftVersion {
        required: String,
        supported: Vec<String>,
    },
    Loader {
        required: LoaderKind,
        supported: Vec<String>,
    },
}

impl IncompatibilityReason {
    /// The `ephervex-i18n` message id describing this reason.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::MinecraftVersion { .. } => "mod-incompatible-minecraft-version",
            Self::Loader { .. } => "mod-incompatible-loader",
        }
    }

    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MinecraftVersion { .. } => "minecraft_version",
            Self::Loader { .. } => "loader",
        }
    }
}

/// Whether something fits the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    Incompatible(Vec<IncompatibilityReason>),
}

impl Compatibility {
    #[must_use]
    pub const fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }

    /// Why this does not fit, or an empty slice when it does.
    #[must_use]
    pub fn reasons(&self) -> &[IncompatibilityReason] {
        match self {
            Self::Compatible => &[],
            Self::Incompatible(reasons) => reasons,
        }
    }
}

/// What a caller is searching for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSearchQuery {
    pub text: String,
    pub limit: u8,
    pub offset: u32,
    /// Whether to return projects that do not fit the instance, annotated with
    /// why. Off by default: the common case is "show me what I can install".
    pub include_incompatible: bool,
}

/// The largest page this launcher asks a provider for.
///
/// A search costs one version-list request per hit — compatibility cannot be
/// decided from search output alone — so the page size is also a request
/// budget. Twenty-five keeps one search comfortably inside a provider's rate
/// limit while still filling a screen.
pub const MAX_SEARCH_LIMIT: u8 = 25;

/// The default page size.
pub const DEFAULT_SEARCH_LIMIT: u8 = 10;

impl ModSearchQuery {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: DEFAULT_SEARCH_LIMIT,
            offset: 0,
            include_incompatible: false,
        }
    }

    /// Sets the page size, clamped to [`MAX_SEARCH_LIMIT`].
    ///
    /// Clamped rather than refused: a caller asking for a hundred results wants
    /// a lot of results, not an error, and the ceiling is this launcher's
    /// business rather than theirs.
    #[must_use]
    pub fn limited(mut self, limit: u8) -> Self {
        self.limit = limit.clamp(1, MAX_SEARCH_LIMIT);
        self
    }

    #[must_use]
    pub const fn from_offset(mut self, offset: u32) -> Self {
        self.offset = offset;
        self
    }

    #[must_use]
    pub const fn including_incompatible(mut self, include: bool) -> Self {
        self.include_incompatible = include;
        self
    }
}

/// One search result, with the verdict already worked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSearchHit {
    pub project: ModProject,
    /// The newest version that fits the instance, where one exists. This is
    /// what an install would default to.
    pub best_version: Option<ModVersionInfo>,
    /// Why the project does not fit, when nothing of it does.
    pub compatibility: Compatibility,
}

/// A page of search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSearchResults {
    pub provider: ModProvider,
    pub hits: Vec<ModSearchHit>,
    /// How many the provider says match, before this launcher's own
    /// version-level verification narrows them.
    pub total_hits: u64,
    pub offset: u32,
}

/// A project with every version, annotated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModProjectDetails {
    pub project: ModProject,
    /// Newest first, so the head of the list is what a version picker opens on.
    pub versions: Vec<AnnotatedVersion>,
    /// The version an install would choose if the user picked nothing.
    pub default_version: Option<String>,
}

/// One version with its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotatedVersion {
    pub version: ModVersionInfo,
    pub compatibility: Compatibility,
}

impl fmt::Display for SideSupport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::test_support::version;

    fn target() -> InstanceTarget {
        InstanceTarget::new("1.20.1", LoaderKind::NeoForge)
    }

    #[test]
    fn a_version_supporting_both_the_release_and_the_loader_is_compatible() {
        let version = version("v1", "PROJ", &["1.20.1"], &["neoforge"]);
        assert!(version.compatibility(&target(), "neoforge").is_compatible());
    }

    #[test]
    fn the_wrong_minecraft_version_says_so_and_lists_what_is_supported() {
        let version = version("v1", "PROJ", &["1.21.1"], &["neoforge"]);
        let verdict = version.compatibility(&target(), "neoforge");
        assert!(!verdict.is_compatible());
        assert_eq!(verdict.reasons().len(), 1);
        assert!(matches!(
            &verdict.reasons()[0],
            IncompatibilityReason::MinecraftVersion { required, supported }
                if required == "1.20.1" && supported == &["1.21.1"]
        ));
    }

    #[test]
    fn the_wrong_loader_says_so_separately() {
        let version = version("v1", "PROJ", &["1.20.1"], &["fabric"]);
        let verdict = version.compatibility(&target(), "neoforge");
        assert!(matches!(
            &verdict.reasons()[0],
            IncompatibilityReason::Loader {
                required: LoaderKind::NeoForge,
                ..
            }
        ));
    }

    #[test]
    fn both_reasons_are_reported_rather_than_the_first_one_found() {
        // "Wrong Minecraft version" and "wrong loader" send a user to different
        // places, so a version that is both must not report only one.
        let version = version("v1", "PROJ", &["1.21.1"], &["fabric"]);
        let verdict = version.compatibility(&target(), "neoforge");
        assert_eq!(verdict.reasons().len(), 2);
    }

    #[test]
    fn a_projects_union_of_versions_cannot_be_mistaken_for_one_versions_support() {
        // The case facets get wrong: the project supports 1.20.1, and it
        // supports NeoForge, but no single version supports both.
        let old = version("old", "PROJ", &["1.20.1"], &["forge"]);
        let new = version("new", "PROJ", &["1.21.1"], &["neoforge"]);
        assert!(!old.compatibility(&target(), "neoforge").is_compatible());
        assert!(!new.compatibility(&target(), "neoforge").is_compatible());
    }

    #[test]
    fn a_search_limit_is_clamped_rather_than_refused() {
        assert_eq!(
            ModSearchQuery::new("jei").limited(200).limit,
            MAX_SEARCH_LIMIT
        );
        assert_eq!(ModSearchQuery::new("jei").limited(0).limit, 1);
        assert_eq!(ModSearchQuery::new("jei").limit, DEFAULT_SEARCH_LIMIT);
        assert!(!ModSearchQuery::new("jei").include_incompatible);
    }
}
