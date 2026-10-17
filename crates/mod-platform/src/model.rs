//! The provider-neutral mod model.
//!
//! Modrinth's vocabulary stops at [`crate::modrinth`]. Everything above this
//! module - the resolver, the services layer, both frontends - is written
//! against the types here, so a second provider is a new translation rather
//! than a second everything.
//!
//! # Compatibility is a verdict with reasons, not a boolean
//!
//! A launcher that only filtered incompatible results would leave the user
//! staring at a mod that is obviously the right one and no explanation for why
//! it is not offered. [`Compatibility`] therefore carries *why*, and an
//! incompatible version is something a caller can display and refuse to
//! install - rather than something it silently never sees.

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use ephervex_model::{Hash, LoaderKind, ModProvider, ModRelationKind};

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

/// The answer to a batch project lookup: the mods, and the ids that are not.
///
/// A catalog can hold projects that are not mods (resource packs, modpacks,
/// shader packs). A caller that only describes projects keeps the mods and
/// reports the rest, rather than losing every description to one such id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectBatch {
    pub mods: Vec<ModProject>,
    pub not_mods: Vec<NotModProject>,
}

/// A project a batch lookup found that is not a mod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotModProject {
    /// The provider's own id.
    pub id: String,
    /// What the provider calls the project's kind.
    pub project_type: String,
}

/// Whether this launcher may fetch a release's bytes, and from where.
///
/// A CurseForge project's owner can turn third-party distribution off. That
/// leaves the release fully described and undownloadable at the same time,
/// which is a fact about acquisition rather than a gap in what the provider
/// said - so it is carried here instead of by discarding the release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distribution {
    /// An address this launcher is allowed to fetch, already checked against
    /// the hosts the provider delivers from.
    Download(String),
    /// The provider described the file and forbids this launcher from
    /// fetching it. The bytes can only arrive by the user's own hand, and no
    /// address is invented to route around that.
    Restricted,
}

impl Distribution {
    /// The address to fetch, or `None` when there is none this launcher may
    /// use.
    #[must_use]
    pub fn download_url(&self) -> Option<&str> {
        match self {
            Self::Download(url) => Some(url),
            Self::Restricted => None,
        }
    }

    /// Whether acquiring this file needs the user rather than the launcher.
    #[must_use]
    pub const fn is_restricted(&self) -> bool {
        matches!(self, Self::Restricted)
    }

    /// The addresses a recipe entry records, which is none for a file this
    /// launcher may not fetch.
    #[must_use]
    pub fn addresses(&self) -> Vec<String> {
        match self {
            Self::Download(url) => vec![url.clone()],
            Self::Restricted => Vec::new(),
        }
    }
}

/// One file belonging to a version - the one this launcher would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    /// The provider's own file name. Untrusted input: it is validated as a safe
    /// relative path before it is allowed to name a destination.
    pub file_name: String,
    /// Where the bytes come from, or that they may not be fetched at all.
    pub distribution: Distribution,
    pub size: Option<u64>,
    /// Strongest first, so the shared cache path is derived from the best hash
    /// the provider published.
    pub hashes: Vec<Hash>,
}

/// A catalog release a file on disk turned out to be.
///
/// The answer to "what is this jar?" for a file that arrived without any
/// identity of its own -- out of a published server archive, most of all. It
/// carries what identifies the release and what that release declares about
/// other projects, because that is what a recipe entry records. No address is
/// kept: one can always be resolved from the identity later, and one recorded
/// now would be a second copy of something the catalog already owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognizedFile {
    pub provider: ModProvider,
    pub project_id: String,
    /// The release, as this provider names it. Modrinth's version id, or
    /// CurseForge's file id.
    pub version_id: String,
    /// CurseForge's separate file id, where the provider has one distinct from
    /// the version. Modrinth does not.
    pub file_id: Option<String>,
    /// What this exact release declares, read the same way the mod browser
    /// reads it. Empty when the catalog answered that it declares nothing.
    pub dependencies: Vec<ModDependency>,
    /// The sides this exact release declares. `Unknown` means the catalog
    /// stated nothing about the side for this file.
    pub client_side: SideSupport,
    pub server_side: SideSupport,
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

/// One relation, exactly as the provider stated it.
///
/// All three identity fields are optional because the provider's own schema
/// makes them so. [`crate::resolve::normalize`] turns this into something with
/// one shape; nothing else should interpret these fields directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDependency {
    pub kind: ModRelationKind,
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub file_name: Option<String>,
}

impl ModDependency {
    /// Whether this relation pins a release and names no project, so the
    /// project can only be learned from the release.
    #[must_use]
    pub const fn pins_only_a_version(&self) -> bool {
        self.project_id.is_none() && self.version_id.is_some()
    }

    /// Names the project of a relation that pinned only a release, from the
    /// project each release belongs to.
    ///
    /// A relation that already names a project, or whose release is not in
    /// `owners`, is left as stated.
    pub fn name_pinned_project(&mut self, owners: &BTreeMap<String, String>) {
        if !self.pins_only_a_version() {
            return;
        }
        if let Some(owner) = self.version_id.as_ref().and_then(|id| owners.get(id)) {
            self.project_id = Some(owner.clone());
        }
    }
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
    /// The sides this release itself declares. `Unknown` means the release
    /// declared nothing and the project's declaration applies instead; see
    /// [`ModVersionInfo::effective_sides`].
    pub client_side: SideSupport,
    pub server_side: SideSupport,
}

impl ModVersionInfo {
    /// The client and server support of this release, in that order.
    ///
    /// A side the release declares wins over the project's, because a
    /// project's declaration summarizes its releases and a release can
    /// disagree with it. The project's value is used only for a side the
    /// release leaves undeclared.
    #[must_use]
    pub fn effective_sides(&self, project: &ModProject) -> (SideSupport, SideSupport) {
        let pick = |release: SideSupport, project: SideSupport| match release {
            SideSupport::Unknown => project,
            declared => declared,
        };
        (
            pick(self.client_side, project.client_side),
            pick(self.server_side, project.server_side),
        )
    }

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
    /// Declared not to run on a server, for a search on behalf of one.
    ClientOnly,
}

impl IncompatibilityReason {
    /// The `ephervex-i18n` message id describing this reason.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::MinecraftVersion { .. } => "mod-incompatible-minecraft-version",
            Self::Loader { .. } => "mod-incompatible-loader",
            Self::ClientOnly => "mod-incompatible-client-only",
        }
    }

    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MinecraftVersion { .. } => "minecraft_version",
            Self::Loader { .. } => "loader",
            Self::ClientOnly => "client_only",
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

/// The side of the game a search installs onto.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum HostSide {
    /// A client instance. Results are not narrowed by declared side support.
    #[default]
    Client,
    /// A dedicated server. Mods declared client-only are left out.
    Server,
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
    /// The side the results are for. Narrows results only while incompatible
    /// results are not requested, like the version and loader filters.
    pub side: HostSide,
}

/// The largest page this launcher asks a provider for.
///
/// A search costs one version-list request per hit - compatibility cannot be
/// decided from search output alone - so the page size is also a request
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
            side: HostSide::Client,
        }
    }

    #[must_use]
    pub const fn for_side(mut self, side: HostSide) -> Self {
        self.side = side;
        self
    }

    /// Whether results declared client-only are left out of this search.
    #[must_use]
    pub const fn excludes_client_only(&self) -> bool {
        matches!(self.side, HostSide::Server) && !self.include_incompatible
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

impl ModSearchHit {
    /// A hit judged for the side it would be installed on.
    ///
    /// A mod declared client-only fits no server, however well its releases
    /// match the Minecraft version and loader, so for a server it is
    /// incompatible for that reason as well.
    pub(crate) fn judged(
        project: ModProject,
        best_version: Option<ModVersionInfo>,
        compatibility: Compatibility,
        side: HostSide,
    ) -> Self {
        let mut hit = Self {
            project,
            best_version,
            compatibility,
        };
        if side == HostSide::Server && hit.is_client_only() {
            let mut reasons = hit.compatibility.reasons().to_vec();
            reasons.push(IncompatibilityReason::ClientOnly);
            hit.compatibility = Compatibility::Incompatible(reasons);
        }
        hit
    }

    /// The client and server support of the release an install would default
    /// to, or of the project where no release fits.
    #[must_use]
    pub fn effective_sides(&self) -> (SideSupport, SideSupport) {
        self.best_version.as_ref().map_or(
            (self.project.client_side, self.project.server_side),
            |version| version.effective_sides(&self.project),
        )
    }

    /// Whether the hit declares that it does not run on a server.
    #[must_use]
    pub fn is_client_only(&self) -> bool {
        self.effective_sides().1 == SideSupport::Unsupported
    }
}

/// A page of search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSearchResults {
    pub provider: ModProvider,
    pub hits: Vec<ModSearchHit>,
    /// How many the provider says match, before this launcher's own
    /// version-level verification narrows them.
    pub total_hits: u64,
    /// The offset this page was requested at.
    pub offset: u32,
    /// Where the provider says its next page begins, `None` once it has no
    /// more to offer.
    ///
    /// Read from how much of the raw page the provider actually returned, not
    /// from how many of `hits` survived this launcher's own compatibility and
    /// class filtering: a page the provider considers fully consumed is fully
    /// consumed even when this launcher declines to show some of it, and
    /// asking for the next page at the count of what was shown would re-read
    /// entries already seen. Can be `Some` even when `hits` is empty: a page
    /// every entry of which was filtered out still has a next page when the
    /// provider says so.
    pub next_offset: Option<u32>,
}

/// Where a provider says its next page begins, given what one raw page
/// proved.
///
/// `start` and `raw_count` describe the raw page as the provider returned it,
/// before any caller-side filtering; `total` is how many entries the provider
/// says the whole listing has. All three are the provider's own numbers.
/// Passing how many entries a caller went on to keep instead of `raw_count`
/// would tell two different pages to start at the same offset.
///
/// Guaranteed to be either `None` or strictly greater than `start`. A
/// provider that reports `raw_count: 0` while claiming more exist (or one
/// whose numbers overflow the neutral offset type) has not proven where a
/// next page would begin; repeating `start` would make "Load More" request
/// the same page forever, so this reports no continuation rather than one
/// that cannot advance.
#[must_use]
pub fn next_search_offset(start: u32, raw_count: u64, total: u64) -> Option<u32> {
    let next = u64::from(start).saturating_add(raw_count);
    if next >= total {
        return None;
    }
    let next = u32::try_from(next).ok()?;
    (next > start).then_some(next)
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

    #[test]
    fn the_next_offset_is_the_raw_page_that_was_consumed_not_what_a_caller_kept() {
        // A page starting at 0 that consumed 3 raw entries continues at 3,
        // whatever a caller went on to filter out of them.
        assert_eq!(next_search_offset(0, 3, 100), Some(3));
    }

    #[test]
    fn a_raw_page_reaching_the_providers_own_total_has_no_continuation() {
        assert_eq!(next_search_offset(0, 3, 3), None);
        assert_eq!(next_search_offset(7, 3, 10), None);
    }

    #[test]
    fn zero_raw_results_never_repeat_the_page_just_asked_for() {
        // The provider claims ninety more exist but consumed none of them
        // here. Reporting `Some(10)` would ask for the same page forever;
        // reporting no continuation is honest about what this page proved.
        assert_eq!(next_search_offset(10, 0, 100), None);
    }

    #[test]
    fn ordinary_pages_keep_advancing_by_what_they_actually_consumed() {
        assert_eq!(next_search_offset(0, 3, 100), Some(3));
        assert_eq!(next_search_offset(10, 5, 100), Some(15));
    }

    #[test]
    fn a_continuation_that_would_not_advance_is_never_returned() {
        // Every case here either reaches the provider's own total or would
        // report the offset a caller already has, and both are refused the
        // same way: `next_search_offset` never sends a caller back to where
        // it started.
        for (start, raw_count, total) in [
            // Exactly reaches the total: no more to prove.
            (0_u32, 3_u64, 3_u64),
            (7, 3, 10),
            // Nothing consumed, even though the provider claims more exist.
            (10, 0, 100),
            (0, 0, 1),
            // The raw count is large enough that the next offset would not
            // fit the neutral `u32` offset type at all.
            (u32::MAX, 5, u64::MAX),
            (u32::MAX - 1, u64::from(u32::MAX), u64::MAX),
        ] {
            let continuation = next_search_offset(start, raw_count, total);
            assert!(
                continuation.is_none_or(|next| next > start),
                "next_search_offset({start}, {raw_count}, {total}) = {continuation:?} \
                 does not advance past {start}"
            );
        }
    }

    fn project(client: SideSupport, server: SideSupport) -> ModProject {
        ModProject {
            provider: ModProvider::CurseForge,
            id: "1".to_owned(),
            slug: None,
            title: "P".to_owned(),
            description: String::new(),
            author: None,
            client_side: client,
            server_side: server,
            icon_url: None,
        }
    }

    #[test]
    fn a_release_that_disagrees_with_its_project_decides_its_own_sides() {
        let mut release = crate::test_support::compatible("v1", "1");
        release.client_side = SideSupport::Required;
        release.server_side = SideSupport::Unsupported;
        assert_eq!(
            release.effective_sides(&project(SideSupport::Required, SideSupport::Required)),
            (SideSupport::Required, SideSupport::Unsupported)
        );
    }

    #[test]
    fn a_side_the_release_leaves_undeclared_takes_the_projects() {
        let mut release = crate::test_support::compatible("v1", "1");
        release.server_side = SideSupport::Optional;
        assert_eq!(
            release.effective_sides(&project(SideSupport::Required, SideSupport::Required)),
            (SideSupport::Required, SideSupport::Optional)
        );
    }
}
