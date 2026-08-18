//! Browsing for mods and installing them into an instance.
//!
//! # What this layer adds over `ephervex-mod-platform`
//!
//! The provider crate reads a catalog and produces a plan. It does not know
//! what an instance is, where `mods/` lives, or that a recipe exists.
//! Sequencing those is this module's whole job:
//!
//! ```text
//! instance record ──▶ target (exact release + loader)
//!         │
//!         ├── resolve against the catalog        (no writes)
//!         ├── translate the plan into recipe entries
//!         ├── download, verify, copy             (rolled back on failure)
//!         └── record in the retained recipe      (atomic)
//! ```
//!
//! # The instance's versions are inputs, never outputs
//!
//! The target is built from the instance record and handed to the resolver by
//! reference. Nothing in this flow writes `minecraft_version` or `loader`; a
//! mod that does not fit is a mod that does not fit.
//!
//! # Why a hand-made instance gets a recipe
//!
//! A mod installed through the browser is part of what the instance is supposed
//! to contain, and "Create Server" builds from the retained recipe rather than
//! from the mutable `mods/` directory (ADR 0012, ADR 0013). An instance created
//! by hand has no recipe until something needs one, so the first install
//! creates one describing what that instance already is.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, DomainAllowlist, DownloadManager, DownloaderConfig, OperationReporter,
};
use ephervex_mod_platform::{
    AnnotatedVersion, InstallSelection, InstalledPolicy, InstalledProject, InstanceTarget,
    ModInstallPlan, ModPlatformError, ModProject, ModProjectDetails, ModProviderApi,
    ModSearchQuery, ModSearchResults, ModVersionInfo, ModrinthProvider, PlannedMod, SideSupport,
    annotate, newest_compatible,
};
use ephervex_model::{Hash, ImportSourceKind, InstanceId, ModOrigin, ModProvider};
use ephervex_pack::{
    ArtifactFetch, ManagedMod, ManagedModInstall, ManagedModInstallation, ModIdentity, PackError,
    PackFileRecipe, PackRecipe, PackSide, ProviderFileIdentity, RecordedMod, SideRequirement,
    SourceStore,
};
use ephervex_storage::ClientRecord;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::CoreResult;

/// What a caller wants installed.
#[derive(Debug, Clone)]
pub struct ModInstallRequest {
    /// Which catalog the reference below belongs to.
    ///
    /// Part of the request rather than an application-wide setting: a project
    /// id means nothing without the catalog it was read from, and an instance
    /// can hold mods from more than one.
    pub provider: ModProvider,
    /// The project, by id or by the provider's human-typable reference.
    pub project: String,
    /// An exact version. `None` takes the newest that fits the instance.
    pub version: Option<String>,
    /// Optional dependencies to take, by project id. Empty means none, which is
    /// the product default.
    pub optional: BTreeSet<String>,
}

impl ModInstallRequest {
    #[must_use]
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            provider: ModProvider::Modrinth,
            project: project.into(),
            version: None,
            optional: BTreeSet::new(),
        }
    }

    /// The catalog the project reference belongs to.
    #[must_use]
    pub const fn from_provider(mut self, provider: ModProvider) -> Self {
        self.provider = provider;
        self
    }

    #[must_use]
    pub fn at_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    #[must_use]
    pub fn selecting_optional<I, S>(mut self, projects: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.optional = projects.into_iter().map(Into::into).collect();
        self
    }
}

/// What an installation produced.
#[derive(Debug)]
pub struct InstalledMods {
    /// The plan that was carried out, so a caller can report what it involved.
    pub plan: ModInstallPlan,
    pub installation: ManagedModInstallation,
}

impl InstalledMods {
    /// The mods this instance now has, as the recipe records them.
    #[must_use]
    pub fn managed(&self) -> Vec<&PlannedMod> {
        self.plan.all_mods()
    }
}

/// The largest image the launcher will pull down for one project icon.
///
/// Generous for a list thumbnail and small enough that a provider serving
/// something unexpected cannot fill a cache directory.
const MAX_ARTWORK_BYTES: u64 = 2 * 1024 * 1024;

/// Whether the jar an entry describes is actually in the game directory.
///
/// A recorded mod whose file has gone missing is not forgotten and not
/// silently removed from the instance's desired state: the record is what
/// "Create Server" and a repair install are built on, so the honest report is
/// that the instance wants a mod it currently does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModPresence {
    Installed,
    /// Recorded as belonging to the instance, absent from `mods/`.
    Missing,
}

impl ModPresence {
    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Missing => "missing",
        }
    }

    /// Whether this state is something to put in front of a user.
    #[must_use]
    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::Missing)
    }
}

/// How much provider metadata a snapshot carries.
///
/// Three states rather than a flag, because "nobody asked yet", "here it is",
/// and "the provider could not be reached" are three different things to show,
/// and only the last one is worth mentioning to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataAvailability {
    /// Durable local state only. Nothing was fetched and nothing failed.
    Local,
    /// Provider metadata was applied, from the network or from its cache.
    Provider,
    /// The provider could not be reached and nothing was cached for it. The
    /// mods are still listed, with what the instance itself knows.
    Unavailable,
}

impl MetadataAvailability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Provider => "provider",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Display metadata for one project, as a provider published it.
///
/// Every field is enrichment. Losing all of it costs a nicer label and nothing
/// else, which is why nothing in the launcher's behavior reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDetails {
    pub title: String,
    pub slug: Option<String>,
    /// Attribution as the provider reports it. Modrinth's project endpoint
    /// reports a team rather than a person, so this is often absent — and
    /// absent rather than filled in from somewhere else.
    pub author: Option<String>,
    pub description: String,
    /// The installed version's published name, when the exact version is known.
    pub version_name: Option<String>,
    pub version_number: Option<String>,
    /// The Minecraft versions the installed release declares.
    pub game_versions: Vec<String>,
    /// The loaders the installed release declares.
    pub loaders: Vec<String>,
    pub client_side: SideSupport,
    pub server_side: SideSupport,
}

/// One mod of a client instance, with everything a frontend needs to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedModEntry {
    /// Where the jar belongs, relative to the game directory.
    pub logical_path: PathBuf,
    /// The jar's own name. Shown when nothing better is known, and never
    /// treated as evidence of what the mod is.
    pub file_name: String,
    /// Why this mod belongs to the instance.
    pub origin: ModOrigin,
    /// How much of its provider identity is proven.
    pub identity: ModIdentity,
    pub hashes: Vec<Hash>,
    pub size: Option<u64>,
    pub client: SideRequirement,
    pub server: SideRequirement,
    pub presence: ModPresence,
    /// Provider metadata, when it was available. `None` is an ordinary state.
    pub details: Option<ModDetails>,
    /// A cached project icon on disk, when one was fetched.
    pub icon: Option<PathBuf>,
}

impl ManagedModEntry {
    /// Builds an entry from durable state and the game directory.
    fn from_recorded(recorded: RecordedMod, game_dir: &Path) -> Self {
        let presence = if game_dir.join(&recorded.logical_path).is_file() {
            ModPresence::Installed
        } else {
            ModPresence::Missing
        };
        Self {
            logical_path: recorded.logical_path,
            file_name: recorded.file_name,
            origin: recorded.origin,
            identity: recorded.identity,
            hashes: recorded.hashes,
            size: recorded.size,
            client: recorded.client,
            server: recorded.server,
            presence,
            details: None,
            icon: None,
        }
    }

    /// The best label available, which is the project's title if one was
    /// fetched and the file's own name otherwise.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.details
            .as_ref()
            .map_or(self.file_name.as_str(), |details| details.title.as_str())
    }

    /// The installed version as something to show, or `None` when the launcher
    /// cannot prove which release this is.
    ///
    /// Never the file name: a jar called `library-1.2.3.jar` is not evidence
    /// that version 1.2.3 is installed.
    #[must_use]
    pub fn installed_version(&self) -> Option<&str> {
        let details = self.details.as_ref()?;
        details
            .version_number
            .as_deref()
            .or(details.version_name.as_deref())
    }

    /// Whether the instance's pack owns this mod, rather than a later decision
    /// made in this instance.
    #[must_use]
    pub const fn is_pack_owned(&self) -> bool {
        self.origin.is_pack_owned()
    }
}

/// Where an instance's mods came from, for the workspace that lists them.
///
/// Names the import rather than a provider project: a `.mrpack` index declares
/// its files, its Minecraft version, and its loader, and nothing that resolves
/// to a project in Modrinth's catalog. Pack artwork therefore stays the generic
/// presentation until a format that does identify its own project is supported,
/// rather than being guessed at from the pack's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstancePackSource {
    pub kind: ImportSourceKind,
    /// The pack's name as the provider spelled it.
    pub name: String,
    pub version: Option<String>,
    pub summary: Option<String>,
}

impl InstancePackSource {
    fn from_recipe(recipe: &PackRecipe) -> Self {
        Self {
            kind: recipe.source_kind,
            name: recipe.original_name.clone(),
            version: recipe.original_version.clone(),
            summary: recipe.summary.clone(),
        }
    }
}

/// Every mod a client instance has, as one coherent view.
///
/// The type both frontends render. It is assembled here rather than in a
/// frontend so that nothing above this layer has to read a retained recipe,
/// inspect a provider manifest, or decide what a file name implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedModSnapshot {
    pub instance: InstanceId,
    /// The import this instance came from, when it came from one.
    pub source: Option<InstancePackSource>,
    /// In recipe order, which is the order the pack declared and then the order
    /// mods were installed.
    pub mods: Vec<ManagedModEntry>,
    pub metadata: MetadataAvailability,
}

impl ManagedModSnapshot {
    /// How many mods the instance has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mods.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mods.is_empty()
    }

    /// Mods the instance wants but does not currently have.
    pub fn missing(&self) -> impl Iterator<Item = &ManagedModEntry> {
        self.mods
            .iter()
            .filter(|entry| entry.presence == ModPresence::Missing)
    }

    /// The distinct projects to look up, for one provider.
    fn identified_project_ids(&self, provider: ModProvider) -> Vec<String> {
        let ids: BTreeSet<&str> = self
            .mods
            .iter()
            .filter(|entry| entry.identity.provider() == Some(provider))
            .filter_map(|entry| entry.identity.project_id())
            .collect();
        ids.into_iter().map(ToOwned::to_owned).collect()
    }

    /// The distinct versions to look up, for one provider.
    fn identified_version_ids(&self, provider: ModProvider) -> Vec<String> {
        let ids: BTreeSet<&str> = self
            .mods
            .iter()
            .filter(|entry| entry.identity.provider() == Some(provider))
            .filter_map(|entry| entry.identity.version_id())
            .collect();
        ids.into_iter().map(ToOwned::to_owned).collect()
    }

    /// Applies fetched metadata to the entries it belongs to.
    ///
    /// An entry the index has nothing for keeps its durable state untouched,
    /// so a project the catalog dropped stays visible under its file name.
    fn apply(&mut self, index: &ModDetailIndex) {
        for entry in &mut self.mods {
            let (Some(provider), Some(project_id)) =
                (entry.identity.provider(), entry.identity.project_id())
            else {
                continue;
            };
            // Keyed by catalog as well as by id: two catalogs spell their own
            // ids their own way, and nothing guarantees one's project id cannot
            // read like another's.
            let key = (provider, project_id.to_owned());
            let Some(project) = index.projects.get(&key) else {
                continue;
            };
            let version = entry
                .identity
                .version_id()
                .and_then(|id| index.versions.get(&(provider, id.to_owned())));

            entry.icon = index.icons.get(&key).cloned();
            entry.details = Some(ModDetails {
                title: project.title.clone(),
                slug: project.slug.clone(),
                author: project.author.clone(),
                description: project.description.clone(),
                version_name: version.map(|version| version.name.clone()),
                version_number: version.map(|version| version.version_number.clone()),
                game_versions: version
                    .map(|version| version.game_versions.clone())
                    .unwrap_or_default(),
                loaders: version
                    .map(|version| version.loaders.clone())
                    .unwrap_or_default(),
                client_side: project.client_side,
                server_side: project.server_side,
            });
        }
    }
}

/// What one enrichment pass fetched, keyed for assembly.
#[derive(Debug, Default)]
struct ModDetailIndex {
    projects: BTreeMap<ProjectKey, ModProject>,
    versions: BTreeMap<ProjectKey, ModVersionInfo>,
    icons: BTreeMap<ProjectKey, PathBuf>,
}

/// One catalog's own identifier, which is only meaningful together with the
/// catalog it came from.
type ProjectKey = (ModProvider, String);

impl ModDetailIndex {
    /// Folds one catalog's answers in, leaving whatever another already
    /// contributed in place.
    fn absorb(
        &mut self,
        provider: ModProvider,
        projects: Vec<ModProject>,
        versions: Vec<ModVersionInfo>,
        icons: BTreeMap<String, PathBuf>,
    ) {
        self.projects.extend(
            projects
                .into_iter()
                .map(|project| ((provider, project.id.clone()), project)),
        );
        self.versions.extend(
            versions
                .into_iter()
                .map(|version| ((provider, version.id.clone()), version)),
        );
        self.icons
            .extend(icons.into_iter().map(|(id, path)| ((provider, id), path)));
    }
}

impl App {
    /// Searches a provider for mods that fit one instance.
    ///
    /// Compatibility is the instance's own exact Minecraft version and loader.
    /// By default only mods that actually fit are returned; asking for the rest
    /// returns them annotated with why they do not, rather than offering them
    /// as installable.
    #[instrument(skip(self, query), fields(instance = %instance, provider = %catalog))]
    pub async fn search_mods(
        &self,
        instance: InstanceId,
        catalog: ModProvider,
        query: &ModSearchQuery,
    ) -> CoreResult<ModSearchResults> {
        let record = self.client(instance)?;
        let target = target_for(&record);
        let provider = self.mod_provider(catalog)?;
        Ok(provider
            .search(
                &target,
                query,
                &OperationReporter::silent("browsing-mods"),
                &CancellationToken::new(),
            )
            .await?)
    }

    /// One project and its versions, annotated against the instance.
    ///
    /// `include_incompatible` decides what the version list contains, not
    /// whether the incompatible ones are installable: an incompatible version
    /// carries its reasons and is never the default selection.
    #[instrument(skip(self), fields(instance = %instance, provider = %catalog, project = reference))]
    pub async fn mod_project(
        &self,
        instance: InstanceId,
        catalog: ModProvider,
        reference: &str,
        include_incompatible: bool,
    ) -> CoreResult<ModProjectDetails> {
        let record = self.client(instance)?;
        let target = target_for(&record);
        let provider = self.mod_provider(catalog)?;
        let loader = ephervex_mod_platform::loader_name_for(provider, &target)?;

        let reporter = OperationReporter::silent("browsing-mods");
        let cancel = CancellationToken::new();
        let project: ModProject = provider.project(reference, &reporter, &cancel).await?;
        let versions = provider
            .versions(&project.id, &target, &reporter, &cancel)
            .await?;

        let default_version =
            newest_compatible(&versions, &target, loader).map(|version| version.id);
        let annotated: Vec<AnnotatedVersion> = annotate(&versions, &target, loader)
            .into_iter()
            .filter(|entry| include_incompatible || entry.compatibility.is_compatible())
            .collect();

        Ok(ModProjectDetails {
            project,
            versions: annotated,
            default_version,
        })
    }

    /// Works out what installing a mod would involve, without writing anything.
    ///
    /// Every refusal an installation can hit — a conflicting pin, an
    /// incompatible relation, a required dependency that names no project, a
    /// project already installed at another version — happens here, before the
    /// instance is touched.
    #[instrument(skip(self, request), fields(instance = %instance, project = %request.project))]
    pub async fn plan_mod_installation(
        &self,
        instance: InstanceId,
        request: &ModInstallRequest,
    ) -> CoreResult<ModInstallPlan> {
        let record = self.client(instance)?;
        self.plan_for(
            &record,
            request,
            InstalledPolicy::Preserve,
            &OperationReporter::silent("resolving-mods"),
        )
        .await
    }

    /// Installs a mod and everything it requires.
    #[instrument(
        skip(self, request, reporter, cancel),
        fields(instance = %instance, project = %request.project, operation = %reporter.id())
    )]
    pub async fn install_mods(
        &self,
        instance: InstanceId,
        request: &ModInstallRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledMods> {
        self.with_artifact_cache(Box::pin(
            self.install_mods_holding_the_cache(instance, request, reporter, cancel),
        ))
        .await
    }

    async fn install_mods_holding_the_cache(
        &self,
        instance: InstanceId,
        request: &ModInstallRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<InstalledMods> {
        let record = self.client(instance)?;
        let plan = self
            .plan_for(&record, request, InstalledPolicy::Preserve, reporter)
            .await?;

        // An instance created by hand has no recipe until something needs one.
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        let mut recipe = if store.has_recipe() {
            store.read_recipe()?
        } else {
            PackRecipe::for_manual_instance(
                record.name.clone(),
                record.minecraft_version.clone(),
                record.loader.clone(),
            )
        };

        // Every planned mod is handed over, including the ones already
        // installed. Filtering them out here would duplicate the installer's
        // own pre-flight — which decides per file whether it is an install, a
        // repair, or already present — and would lose the "already present"
        // half of its report, leaving a caller unable to tell "nothing to do"
        // from "nothing was requested".
        let additions: Vec<PackFileRecipe> = plan
            .all_mods()
            .into_iter()
            .map(|planned| recipe_entry(planned, plan.provider))
            .collect::<Result<_, _>>()?;

        let manager = self.artifact_manager()?;

        let installation = ephervex_pack::install_managed(ManagedModInstall {
            recipe: &mut recipe,
            additions: &additions,
            game_dir: &ephervex_storage::metadata::client_game_dir(&record.root),
            store: &store,
            fetch: ArtifactFetch {
                manager: &manager,
                cache: &CacheLayout::new(self.paths().cache_dir()),
                // Asked per file, from each entry's own provider identity: a
                // batch may install a Modrinth mod beside a repair of a
                // CurseForge one, and only the latter may be offered the
                // CurseForge key.
                credentials: &self.provider_credentials(),
                reporter,
                cancel,
            },
        })
        .await?;

        info!(
            instance_id = %record.id,
            installed = installation.installed.len(),
            repaired = installation.repaired.len(),
            already_present = installation.already_present.len(),
            "installed mods"
        );
        Ok(InstalledMods { plan, installation })
    }

    /// The mods this instance installed through a provider.
    ///
    /// Read from the instance's own retained recipe, never by looking at what
    /// is in `mods/`: a jar someone dropped in by hand has no provider, no
    /// project, and no version, and inventing them would produce a record
    /// nothing can re-resolve.
    ///
    /// Narrower than [`Self::managed_mod_snapshot`] on purpose: this is what
    /// the resolver keys on, so it contains only mods with a complete provider
    /// identity. A frontend wants the snapshot.
    pub fn managed_mods(&self, instance: InstanceId) -> CoreResult<Vec<ManagedMod>> {
        Ok(self
            .pack_recipe(instance)?
            .map(|recipe| recipe.managed_mods())
            .unwrap_or_default())
    }

    /// Every mod this client instance has, from durable state alone.
    ///
    /// Reaches no network and needs none: the recipe says which mods belong to
    /// the instance and where they came from, and the game directory says
    /// whether each jar is still there. That makes this the view a frontend can
    /// open instantly, offline, and as often as it likes — provider metadata is
    /// decoration applied on top by [`Self::enriched_managed_mods`], never the
    /// proof that a mod exists.
    #[instrument(skip(self), fields(instance = %instance))]
    pub fn managed_mod_snapshot(&self, instance: InstanceId) -> CoreResult<ManagedModSnapshot> {
        let record = self.client(instance)?;
        let recipe = self.pack_recipe(instance)?;
        let game_dir = ephervex_storage::metadata::client_game_dir(&record.root);

        let mods = recipe
            .as_ref()
            .map(PackRecipe::recorded_mods)
            .unwrap_or_default()
            .into_iter()
            .map(|recorded| ManagedModEntry::from_recorded(recorded, &game_dir))
            .collect();

        Ok(ManagedModSnapshot {
            instance,
            source: recipe.as_ref().map(InstancePackSource::from_recipe),
            mods,
            metadata: MetadataAvailability::Local,
        })
    }

    /// The same view with provider metadata and artwork applied where it is
    /// available.
    ///
    /// Best effort by construction. Every failure below — an unreachable
    /// provider, a project the catalog no longer serves, an icon that will not
    /// download — leaves the entry exactly as durable state described it, with
    /// [`ManagedModSnapshot::metadata`] saying so. Nothing here can remove a
    /// mod from the list or change what the launcher believes is installed.
    ///
    /// Provider responses are served through the metadata cache, so repeated
    /// opens inside its TTL make no request at all, and an earlier response is
    /// still shown when the provider is down.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance))]
    pub async fn enriched_managed_mods(
        &self,
        instance: InstanceId,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ManagedModSnapshot> {
        let mut snapshot = self.managed_mod_snapshot(instance)?;
        if snapshot.mods.is_empty() {
            return Ok(snapshot);
        }

        match self.mod_details(&snapshot, reporter, cancel).await {
            Ok(details) => {
                snapshot.apply(&details);
                snapshot.metadata = MetadataAvailability::Provider;
            }
            Err(error) => {
                // Deliberately not propagated. The mods are installed whether
                // or not their titles could be fetched, and a frontend that
                // showed an error instead of the list would be hiding the one
                // thing it was asked for.
                warn!(
                    %error,
                    "showing managed mods without provider metadata; the catalog was unavailable"
                );
                snapshot.metadata = MetadataAvailability::Unavailable;
            }
        }
        Ok(snapshot)
    }

    /// Looks up the display metadata for everything in a snapshot that has a
    /// provider identity.
    ///
    /// Two batched requests rather than one per mod: a pack with two hundred
    /// mods must not become two hundred round trips.
    async fn mod_details(
        &self,
        snapshot: &ManagedModSnapshot,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModDetailIndex> {
        let mut index = ModDetailIndex::default();
        // An instance can hold mods from more than one catalog, and each one is
        // asked only about its own. A catalog nothing in this instance came
        // from is never contacted, which is what keeps a Modrinth-only instance
        // from needing a CurseForge key to show its mod list.
        for catalog in [ModProvider::Modrinth, ModProvider::CurseForge] {
            let projects: Vec<String> = snapshot.identified_project_ids(catalog);
            if projects.is_empty() {
                continue;
            }
            let versions: Vec<String> = snapshot.identified_version_ids(catalog);
            let provider = self.mod_provider(catalog)?;

            let projects = provider.projects_by_id(&projects, reporter, cancel).await?;
            // A version lookup that fails is survivable in a way a project
            // lookup is not: without it the list loses version names, with no
            // project it loses everything. So it is asked for separately and
            // its failure is absorbed here.
            let versions = match provider.versions_by_id(&versions, reporter, cancel).await {
                Ok(versions) => versions,
                Err(error) => {
                    warn!(%error, provider = %catalog, "managed mod versions could not be fetched");
                    Vec::new()
                }
            };

            let icons = self
                .cache_artwork(catalog, &projects, reporter, cancel)
                .await;
            index.absorb(catalog, projects, versions, icons);
        }
        Ok(index)
    }

    /// Fetches any project artwork that is not already cached.
    ///
    /// Returns only what succeeded. An icon is decoration: a project whose
    /// image will not download keeps its entry, its identity, and its place in
    /// the list, and the frontend draws its fallback.
    async fn cache_artwork(
        &self,
        catalog: ModProvider,
        projects: &[ModProject],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> BTreeMap<String, PathBuf> {
        let wanted: Vec<(&ModProject, &str)> = projects
            .iter()
            .filter_map(|project| Some((project, project.icon_url.as_deref()?)))
            .collect();
        if wanted.is_empty() {
            return BTreeMap::new();
        }

        let manager = match DownloadManager::new(DownloaderConfig {
            http: self.http_config(),
            // Artwork is untrusted content off a CDN and nothing verifies it,
            // so what it may cost is capped rather than trusted.
            max_bytes: Some(MAX_ARTWORK_BYTES),
            ..DownloaderConfig::default()
        }) {
            Ok(manager) => manager,
            Err(error) => {
                warn!(%error, "mod artwork could not be fetched");
                return BTreeMap::new();
            }
        };
        let cache = CacheLayout::new(self.paths().cache_dir());
        // The same host list this catalog's own download addresses are held to.
        // An icon address arrives from a provider response rather than from an
        // archive, but it is still remote input naming a machine to talk to,
        // and one provider's list does not vouch for another's hosts.
        let allowlist = match catalog {
            ModProvider::Modrinth => DomainAllowlist::modrinth_pack_defaults(),
            ModProvider::CurseForge => self.curseforge_artifact_hosts(),
            // A catalog with no list of its own vouches for no host, so its
            // artwork is simply not fetched.
            _ => DomainAllowlist::empty(),
        };

        let mut cached = BTreeMap::new();
        for (project, url) in wanted {
            if cancel.is_cancelled() {
                break;
            }
            match crate::artwork::cache_project_artwork(
                &manager, &cache, &allowlist, project, url, reporter, cancel,
            )
            .await
            {
                Ok(path) => {
                    cached.insert(project.id.clone(), path);
                }
                // Decoration. A project whose image will not download, will not
                // decode, or is too large to be an icon keeps its entry, its
                // identity, and its place in the list.
                Err(error) => crate::artwork::report_failure(&project.id, &error),
            }
        }
        cached
    }

    /// Resolves a plan against an instance's exact versions.
    ///
    /// `policy` is what separates installing from mutating: an install refuses
    /// to move a mod that is already there, and a version change is a request
    /// to do exactly that.
    pub(crate) async fn plan_for(
        &self,
        record: &ClientRecord,
        request: &ModInstallRequest,
        policy: InstalledPolicy,
        reporter: &OperationReporter,
    ) -> CoreResult<ModInstallPlan> {
        let installed = self.installed_projects(record, request.provider)?;
        self.resolve_against(
            &target_for(record),
            &installed,
            request,
            policy,
            reporter,
            &CancellationToken::new(),
        )
        .await
    }

    /// Resolves a request against an arbitrary exact target and desired state.
    ///
    /// Split out from [`Self::plan_for`] because a linked server is the same
    /// question asked of a different Minecraft release, loader, and installed
    /// set. Sharing the implementation is what keeps a server from arriving at
    /// a different answer to "what does this mod need" than the client did.
    pub(crate) async fn resolve_against(
        &self,
        target: &InstanceTarget,
        installed: &[InstalledProject],
        request: &ModInstallRequest,
        policy: InstalledPolicy,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModInstallPlan> {
        let provider = self.mod_provider(request.provider)?;
        let loader = ephervex_mod_platform::loader_name_for(provider, target)?;

        // A human-typable slug has to become an id before anything is keyed on
        // it, or the same project could enter the graph twice under two names.
        let project = provider.project(&request.project, reporter, cancel).await?;

        let mut selection =
            InstallSelection::new(project.id).selecting_optional(request.optional.iter().cloned());
        if let Some(version) = &request.version {
            selection = selection.at_version(version.clone());
        }

        Ok(
            ephervex_mod_platform::resolve(ephervex_mod_platform::ResolveRequest {
                provider,
                target,
                loader_name: loader,
                selection: &selection,
                installed,
                installed_policy: policy,
                reporter,
                cancel,
            })
            .await?,
        )
    }

    /// What the instance already has, for the provider this browse is against.
    fn installed_projects(
        &self,
        record: &ClientRecord,
        provider: ModProvider,
    ) -> CoreResult<Vec<InstalledProject>> {
        Ok(installed_projects_of(
            self.pack_recipe(record.id)?.as_ref(),
            provider,
        ))
    }

    /// One mod catalog, built on first use.
    ///
    /// The catalog is chosen per operation rather than configured once: an
    /// instance can hold mods from more than one, and a project id means
    /// nothing without the catalog it was read from. Both are built lazily for
    /// the same reason the version catalog is: constructing one builds an HTTP
    /// client and a TLS stack, which listing instances has no business paying
    /// for.
    pub(crate) fn mod_provider(&self, catalog: ModProvider) -> CoreResult<&dyn ModProviderApi> {
        match catalog {
            ModProvider::Modrinth => self.modrinth_provider(),
            ModProvider::CurseForge => self.curseforge_provider(),
            // `ModProvider` is non-exhaustive so a catalog can be recorded in a
            // retained recipe before this launcher can read it. Falling back to
            // one of the two above would browse the wrong service and resolve
            // its project ids against it.
            other => Err(ModPlatformError::ProviderNotBrowsable { provider: other }.into()),
        }
    }

    fn modrinth_provider(&self) -> CoreResult<&dyn ModProviderApi> {
        if let Some(provider) = self.mods.get() {
            return Ok(provider);
        }
        let manager = DownloadManager::new(DownloaderConfig {
            http: self.http_config(),
            ..DownloaderConfig::default()
        })
        .map_err(|source| ModPlatformError::ResponseInvalid {
            provider: ModProvider::Modrinth,
            detail: source.to_string(),
        })?;
        let provider = ModrinthProvider::new(manager, CacheLayout::new(self.paths().cache_dir()))?;
        Ok(self.mods.get_or_init(|| provider))
    }

    /// The CurseForge catalog, over the same authenticated transport a pack
    /// import uses.
    ///
    /// Built without reading the credential: the key is fetched from the secret
    /// store at the moment a request needs one, so a key set or removed
    /// mid-session takes effect without rebuilding anything, and a machine that
    /// has configured nothing can still open the browser and be told what is
    /// missing.
    ///
    /// The artifact host list travels with the provider, so an address a search
    /// answered with is held to the same allowlist a pack import's addresses
    /// are.
    fn curseforge_provider(&self) -> CoreResult<&dyn ModProviderApi> {
        if let Some(provider) = self.curseforge_mods.get() {
            return Ok(provider);
        }
        let provider = ephervex_mod_platform::CurseForgeProvider::with_artifact_hosts(
            self.curseforge_api()?.clone(),
            self.curseforge_artifact_hosts(),
        );
        Ok(self.curseforge_mods.get_or_init(|| provider))
    }
}

/// The provider projects one desired state already records, for resolution.
///
/// Takes the recipe rather than an instance id so that a client and a linked
/// server produce this the same way.
pub(crate) fn installed_projects_of(
    recipe: Option<&PackRecipe>,
    provider: ModProvider,
) -> Vec<InstalledProject> {
    recipe
        .map(PackRecipe::managed_mods)
        .unwrap_or_default()
        .into_iter()
        // Only this catalog's mods. Project ids are the provider's own and two
        // catalogs can spell different projects the same way, so mixing them
        // would let one provider's install believe another's mod was already
        // there.
        .filter(|managed| managed.provider == provider)
        .map(|managed| InstalledProject {
            project_id: managed.project_id,
            version_id: managed.version_id,
        })
        .collect()
}

/// The instance's exact Minecraft version and loader.
///
/// Cloned out of the record rather than borrowed so that nothing downstream can
/// hold a reference into the record and be tempted to write through it.
fn target_for(record: &ClientRecord) -> InstanceTarget {
    InstanceTarget::new(record.minecraft_version.clone(), record.loader.kind)
}

/// Turns one planned mod into the recipe entry that records it.
///
/// The client requirement is `Required` regardless of what the project declares:
/// the user asked for this mod on this client, which is a fact about the
/// instance rather than a claim about the project. The *server* requirement is
/// the project's own declaration, verbatim — including `unknown`, which stays
/// undeclared rather than being resolved into a yes.
pub(crate) fn recipe_entry(
    planned: &PlannedMod,
    provider: ModProvider,
) -> CoreResult<PackFileRecipe> {
    recipe_entry_for(planned, provider, PackSide::Client)
}

/// The same entry for whichever side is being written.
///
/// A linked server's recipe is that server's desired state, not a copy of the
/// client's: the side the operation is for is the one recorded as `Required` by
/// the decision, and the other keeps the project's own declaration. On the
/// server an `optional` or undeclared server side is answered here rather than
/// left open, because agreeing to sync the mod to this server *is* that answer
/// — and without it the server's own record would not say the jar belongs
/// there.
pub(crate) fn recipe_entry_for(
    planned: &PlannedMod,
    provider: ModProvider,
    side: PackSide,
) -> CoreResult<PackFileRecipe> {
    let file = &planned.version.file;

    // The file name comes from a provider and is untrusted: it is validated as
    // a safe relative path before it is allowed to name a destination.
    let name = ephervex_pack::safe_file_name(&file.file_name).ok_or_else(|| {
        PackError::EntryPathUnsafe {
            entry: file.file_name.clone(),
        }
    })?;

    let client = match side {
        // The user asked for this mod on this client, which is a fact about the
        // instance rather than a claim about the project.
        PackSide::Client => SideRequirement::Required,
        PackSide::Server => side_requirement(planned.project.client_side),
    };
    let server = side_requirement(planned.project.server_side);
    let selected = match side {
        // Nobody has been asked about the server yet. `None` is what lets a
        // later prompt tell "unanswered" from "the user said no".
        PackSide::Client => None,
        PackSide::Server => server.needs_decision().then_some(true),
    };

    Ok(PackFileRecipe {
        logical_path: PathBuf::from(ephervex_pack::MODS_DIR).join(name),
        hashes: file.hashes.clone(),
        downloads: vec![file.url.clone()],
        size: file.size,
        client,
        server,
        selected,
        origin: planned.origin,
        identity: Some(ProviderFileIdentity {
            provider: Some(provider),
            project_id: Some(planned.version.project_id.clone()),
            version_id: Some(planned.version.id.clone()),
            file_id: exact_file_id(provider, &planned.version.id),
        }),
    })
}

/// The exact provider file this release is, where the provider names one.
///
/// CurseForge's release identifier *is* its file id, so both fields carry it:
/// the same shape a pack import records, which is what lets one reconstruction
/// path read either. Modrinth names a version and lets that version name its
/// files, so there is no separate file id to record and none is invented.
fn exact_file_id(provider: ModProvider, version_id: &str) -> Option<String> {
    match provider {
        ModProvider::CurseForge => Some(version_id.to_owned()),
        // Modrinth, and any catalog that has not said its release identifier
        // is a file: nothing is recorded rather than an id invented.
        _ => None,
    }
}

/// Maps a provider's declared side support onto the recipe's own four states.
///
/// One-to-one, deliberately. Collapsing `unknown` into `required` would assert
/// something the author declined to assert, and would put the mod into a
/// reconstructed server on that assertion.
const fn side_requirement(support: SideSupport) -> SideRequirement {
    match support {
        SideSupport::Required => SideRequirement::Required,
        SideSupport::Optional => SideRequirement::Optional,
        SideSupport::Unsupported => SideRequirement::Unsupported,
        SideSupport::Unknown => SideRequirement::Unknown,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_mod_platform::{ModFile, ModVersionInfo, ReleaseChannel};

    /// One catalog's answers as the index that applies them.
    fn detail_index(
        projects: Vec<ModProject>,
        versions: Vec<ModVersionInfo>,
        icons: BTreeMap<String, PathBuf>,
    ) -> ModDetailIndex {
        let mut index = ModDetailIndex::default();
        index.absorb(ModProvider::Modrinth, projects, versions, icons);
        index
    }

    use ephervex_model::{Hash, HashAlgorithm, ModOrigin};

    use super::*;

    fn planned(server_side: SideSupport, file_name: &str) -> PlannedMod {
        PlannedMod {
            project: ModProject {
                provider: ModProvider::Modrinth,
                id: "LIBPROJ".to_owned(),
                slug: Some("lib".to_owned()),
                title: "Library".to_owned(),
                description: String::new(),
                author: None,
                client_side: SideSupport::Required,
                server_side,
                icon_url: None,
            },
            version: ModVersionInfo {
                provider: ModProvider::Modrinth,
                id: "LIBV1".to_owned(),
                project_id: "LIBPROJ".to_owned(),
                name: "Library 1.0.0".to_owned(),
                version_number: "1.0.0".to_owned(),
                channel: ReleaseChannel::Release,
                game_versions: vec!["1.20.1".to_owned()],
                loaders: vec!["neoforge".to_owned()],
                published_at: chrono::Utc::now(),
                file: ModFile {
                    file_name: file_name.to_owned(),
                    url: "https://cdn.modrinth.com/data/LIBPROJ/library.jar".to_owned(),
                    size: Some(10),
                    hashes: vec![
                        Hash::new(
                            HashAlgorithm::Sha1,
                            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                        )
                        .expect("valid sha1"),
                    ],
                },
                dependencies: Vec::new(),
            },
            origin: ModOrigin::RequiredDependency,
            requested_by: vec!["root".to_owned()],
        }
    }

    #[test]
    fn a_recipe_entry_carries_everything_needed_to_resolve_the_mod_again() {
        let entry = recipe_entry(
            &planned(SideSupport::Required, "library-1.0.0.jar"),
            ModProvider::Modrinth,
        )
        .expect("entry");

        assert_eq!(
            entry.logical_path,
            PathBuf::from("mods/library-1.0.0.jar"),
            "mods land in the game directory's mods folder and nowhere else"
        );
        let managed = entry.managed_mod().expect("a managed mod");
        assert_eq!(managed.provider, ModProvider::Modrinth);
        assert_eq!(managed.project_id, "LIBPROJ");
        assert_eq!(managed.version_id, "LIBV1");
        assert_eq!(managed.origin, ModOrigin::RequiredDependency);
        assert!(!managed.hashes.is_empty());
    }

    #[test]
    fn an_undeclared_server_side_is_recorded_as_undeclared() {
        let entry = recipe_entry(
            &planned(SideSupport::Unknown, "a.jar"),
            ModProvider::Modrinth,
        )
        .expect("entry");
        assert_eq!(
            entry.client,
            SideRequirement::Required,
            "the user asked for it"
        );
        assert_eq!(
            entry.server,
            SideRequirement::Unknown,
            "the author said nothing, and neither does the recipe"
        );
        assert_eq!(
            entry.selected, None,
            "nobody has been asked about the server"
        );
    }

    #[test]
    fn each_declared_side_maps_to_itself() {
        for (declared, expected) in [
            (SideSupport::Required, SideRequirement::Required),
            (SideSupport::Optional, SideRequirement::Optional),
            (SideSupport::Unsupported, SideRequirement::Unsupported),
            (SideSupport::Unknown, SideRequirement::Unknown),
        ] {
            assert_eq!(side_requirement(declared), expected);
        }
    }

    #[test]
    fn a_file_name_from_a_provider_cannot_escape_the_mods_directory() {
        // Provider output is untrusted. A name that climbs out, names an
        // absolute path, or aims at launcher metadata is refused rather than
        // sanitized into something that still writes somewhere.
        for hostile in [
            "../../../../home/dev/.bashrc",
            "/etc/passwd",
            "..",
            "",
            "nested/path.jar",
        ] {
            let result = recipe_entry(
                &planned(SideSupport::Required, hostile),
                ModProvider::Modrinth,
            );
            assert!(
                result.is_err(),
                "`{hostile}` should not be able to name a destination"
            );
        }
    }

    // -- the managed mod view ---------------------------------------------

    /// One recorded mod, with as much identity as the caller says it has.
    fn recorded(name: &str, origin: ModOrigin, identity: ModIdentity) -> RecordedMod {
        RecordedMod {
            logical_path: PathBuf::from("mods").join(name),
            file_name: name.to_owned(),
            origin,
            identity,
            hashes: vec![
                Hash::new(
                    HashAlgorithm::Sha1,
                    "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                )
                .expect("valid sha1"),
            ],
            size: Some(1024),
            client: SideRequirement::Required,
            server: SideRequirement::Unknown,
        }
    }

    fn version_identity(project: &str, version: &str) -> ModIdentity {
        ModIdentity::Version {
            provider: ModProvider::Modrinth,
            project_id: project.to_owned(),
            version_id: version.to_owned(),
            file_id: None,
        }
    }

    /// A game directory holding exactly the named jars.
    fn game_dir_with(jars: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("mods")).expect("mods dir");
        for jar in jars {
            std::fs::write(dir.path().join("mods").join(jar), b"jar").expect("write jar");
        }
        dir
    }

    fn snapshot_of(entries: Vec<ManagedModEntry>) -> ManagedModSnapshot {
        ManagedModSnapshot {
            instance: InstanceId::new(),
            source: None,
            mods: entries,
            metadata: MetadataAvailability::Local,
        }
    }

    fn catalog_project(id: &str, title: &str, icon_url: Option<&str>) -> ModProject {
        ModProject {
            provider: ModProvider::Modrinth,
            id: id.to_owned(),
            slug: Some(title.to_ascii_lowercase()),
            title: title.to_owned(),
            description: format!("{title}, from a fixture"),
            author: Some("a team".to_owned()),
            client_side: SideSupport::Required,
            server_side: SideSupport::Unsupported,
            icon_url: icon_url.map(ToOwned::to_owned),
        }
    }

    fn catalog_version(id: &str, project: &str, number: &str) -> ModVersionInfo {
        ModVersionInfo {
            provider: ModProvider::Modrinth,
            id: id.to_owned(),
            project_id: project.to_owned(),
            name: format!("Release {number}"),
            version_number: number.to_owned(),
            channel: ReleaseChannel::Release,
            game_versions: vec!["1.20.1".to_owned()],
            loaders: vec!["neoforge".to_owned()],
            published_at: chrono::Utc::now(),
            file: ModFile {
                file_name: "fixture.jar".to_owned(),
                url: "https://cdn.modrinth.com/data/AAAAAAAA/versions/BBBBBBBB/fixture.jar"
                    .to_owned(),
                size: Some(10),
                hashes: Vec::new(),
            },
            dependencies: Vec::new(),
        }
    }

    // -- the whole path, from an imported instance to the view -------------

    /// The pack index an offline import can be driven with: no files to fetch,
    /// so nothing here contacts Modrinth.
    fn offline_index() -> Vec<u8> {
        br#"{
            "formatVersion": 1,
            "game": "minecraft",
            "versionId": "3.2.1",
            "name": "Reproduction Pack",
            "files": [],
            "dependencies": {"minecraft": "1.20.1", "neoforge": "47.1.106"}
        }"#
        .to_vec()
    }

    fn mrpack(path: &Path) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("modrinth.index.json", options)
            .expect("start index");
        writer.write_all(&offline_index()).expect("write index");
        writer.finish().expect("finish archive");
    }

    /// The recipe entry a pack import writes for one of its own mods.
    fn pack_mod(name: &str, project: &str, version: &str) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from("mods").join(name),
            hashes: vec![
                Hash::new(
                    HashAlgorithm::Sha1,
                    "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                )
                .expect("valid sha1"),
            ],
            downloads: vec![format!(
                "https://cdn.modrinth.com/data/{project}/versions/{version}/{name}"
            )],
            size: Some(4096),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: None,
        }
    }

    /// Imports an offline pack and gives its instance the mods a pack with
    /// downloadable files would have left behind.
    ///
    /// The files are placed and recorded directly because an ordinary test run
    /// must not fetch anything; everything after that point — reading the
    /// retained recipe, reconstructing identity, checking the game directory —
    /// is the production path.
    async fn imported_instance(base: &Path, jars_on_disk: &[&str]) -> (App, InstanceId) {
        let archive_dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_dir.path().join("pack.mrpack");
        mrpack(&archive);

        let app = App::open_at(ephervex_storage::Paths::rooted_at(base)).expect("app opens");
        let imported = app
            .import_pack(
                &crate::import::ImportRequest::new(&archive),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("import the pack");
        let instance = imported.record.id;

        let store = SourceStore::new(ephervex_storage::metadata::source_dir(
            &imported.record.root,
        ));
        let mut recipe = store.read_recipe().expect("the retained recipe");
        recipe
            .files
            .push(pack_mod("journeymap.jar", "lfHFW1mp", "GKQEbfSt"));
        recipe
            .files
            .push(pack_mod("architectury.jar", "lhGA9TYQ", "Va9L5Nqe"));
        store.write_recipe(&recipe).expect("write the recipe");

        let mods_dir =
            ephervex_storage::metadata::client_game_dir(&imported.record.root).join("mods");
        std::fs::create_dir_all(&mods_dir).expect("mods dir");
        for jar in jars_on_disk {
            std::fs::write(mods_dir.join(jar), b"jar").expect("write jar");
        }

        // Reopened rather than reused, so nothing below is reading state this
        // function happened to leave in memory.
        let app = App::open_at(ephervex_storage::Paths::rooted_at(base)).expect("app reopens");
        (app, instance)
    }

    #[tokio::test]
    async fn an_imported_packs_mods_reach_the_view_a_frontend_reads() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance) =
            imported_instance(base.path(), &["journeymap.jar", "architectury.jar"]).await;

        let snapshot = app.managed_mod_snapshot(instance).expect("snapshot");

        assert_eq!(snapshot.len(), 2, "the pack's mods are the instance's mods");
        assert!(
            snapshot.mods.iter().all(ManagedModEntry::is_pack_owned),
            "and they are visibly the pack's"
        );
        assert_eq!(
            snapshot.mods[0].identity,
            version_identity("lfHFW1mp", "GKQEbfSt"),
            "identity is reconstructed from the pack's own addresses on the way in"
        );
        assert_eq!(snapshot.metadata, MetadataAvailability::Local);
        assert_eq!(snapshot.missing().count(), 0);

        let source = snapshot
            .source
            .expect("an imported instance names its pack");
        assert_eq!(source.kind, ImportSourceKind::ModrinthPack);
        assert_eq!(source.name, "Reproduction Pack");
        assert_eq!(source.version.as_deref(), Some("3.2.1"));
    }

    #[tokio::test]
    async fn a_pack_mod_deleted_from_the_game_directory_is_flagged_not_dropped() {
        let base = tempfile::tempdir().expect("temp dir");
        // Bisecting a crash by deleting a jar is the workflow this launcher is
        // for. The instance still wants the mod, and says so.
        let (app, instance) = imported_instance(base.path(), &["journeymap.jar"]).await;

        let snapshot = app.managed_mod_snapshot(instance).expect("snapshot");

        assert_eq!(snapshot.len(), 2);
        let missing: Vec<&str> = snapshot
            .missing()
            .map(|entry| entry.file_name.as_str())
            .collect();
        assert_eq!(missing, vec!["architectury.jar"]);
        assert!(snapshot.mods[1].identity.is_fully_resolved());
    }

    #[tokio::test]
    async fn a_jar_nobody_recorded_is_left_alone_rather_than_adopted() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance) = imported_instance(
            base.path(),
            &[
                "journeymap.jar",
                "architectury.jar",
                "local-debug-build.jar",
            ],
        )
        .await;

        let snapshot = app.managed_mod_snapshot(instance).expect("snapshot");

        assert_eq!(
            snapshot.len(),
            2,
            "a jar the user dropped in is not given an invented record"
        );
        assert!(
            !snapshot
                .mods
                .iter()
                .any(|entry| entry.file_name == "local-debug-build.jar")
        );
        assert!(
            ephervex_storage::metadata::client_game_dir(&app.client(instance).unwrap().root)
                .join("mods/local-debug-build.jar")
                .is_file(),
            "and it is still there"
        );
    }

    #[tokio::test]
    async fn an_instance_that_was_never_imported_has_an_empty_view_rather_than_an_error() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance) = imported_instance(base.path(), &[]).await;
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(
            &app.client(instance).expect("record").root,
        ));
        std::fs::remove_file(store.dir().join(ephervex_pack::RECIPE_FILE)).expect("remove recipe");

        let snapshot = app.managed_mod_snapshot(instance).expect("snapshot");
        assert!(snapshot.is_empty());
        assert_eq!(snapshot.source, None);
    }

    #[test]
    fn a_recorded_mod_whose_jar_is_gone_is_reported_rather_than_forgotten() {
        let dir = game_dir_with(&["present.jar"]);
        let present = ManagedModEntry::from_recorded(
            recorded(
                "present.jar",
                ModOrigin::PackDeclared,
                version_identity("AAAAAAAA", "BBBBBBBB"),
            ),
            dir.path(),
        );
        let gone = ManagedModEntry::from_recorded(
            recorded(
                "gone.jar",
                ModOrigin::PackDeclared,
                version_identity("CCCCCCCC", "DDDDDDDD"),
            ),
            dir.path(),
        );

        assert_eq!(present.presence, ModPresence::Installed);
        assert_eq!(gone.presence, ModPresence::Missing);
        assert!(
            gone.presence.needs_attention(),
            "the instance wants a mod it does not have, which is worth saying"
        );
        assert_eq!(
            gone.identity,
            version_identity("CCCCCCCC", "DDDDDDDD"),
            "a missing file loses none of what the instance recorded about it"
        );
    }

    #[test]
    fn provider_metadata_fills_in_the_display_fields_of_the_mods_it_names() {
        let dir = game_dir_with(&["jei.jar"]);
        let mut snapshot = snapshot_of(vec![ManagedModEntry::from_recorded(
            recorded(
                "jei.jar",
                ModOrigin::UserSelected,
                version_identity("u6dRKJwZ", "Orv4ObRj"),
            ),
            dir.path(),
        )]);

        let icons = BTreeMap::from([(
            "u6dRKJwZ".to_owned(),
            PathBuf::from("/cache/artwork/jei.png"),
        )]);
        snapshot.apply(&detail_index(
            vec![catalog_project("u6dRKJwZ", "Just Enough Items", None)],
            vec![catalog_version("Orv4ObRj", "u6dRKJwZ", "15.2.0.27")],
            icons,
        ));

        let entry = &snapshot.mods[0];
        assert_eq!(entry.display_name(), "Just Enough Items");
        assert_eq!(entry.installed_version(), Some("15.2.0.27"));
        let details = entry.details.as_ref().expect("details");
        assert_eq!(details.author.as_deref(), Some("a team"));
        assert_eq!(details.game_versions, vec!["1.20.1".to_owned()]);
        assert_eq!(details.loaders, vec!["neoforge".to_owned()]);
        assert_eq!(details.server_side, SideSupport::Unsupported);
        assert_eq!(entry.icon, Some(PathBuf::from("/cache/artwork/jei.png")));
    }

    #[test]
    fn a_mod_the_catalog_says_nothing_about_keeps_its_row_and_invents_nothing() {
        let dir = game_dir_with(&["mystery.jar", "jei.jar"]);
        let mut snapshot = snapshot_of(vec![
            ManagedModEntry::from_recorded(
                recorded(
                    "mystery.jar",
                    ModOrigin::PackDeclared,
                    ModIdentity::Unresolved,
                ),
                dir.path(),
            ),
            ManagedModEntry::from_recorded(
                recorded(
                    "jei.jar",
                    ModOrigin::UserSelected,
                    version_identity("u6dRKJwZ", "Orv4ObRj"),
                ),
                dir.path(),
            ),
        ]);

        // A response that covers neither of them: one has no project to look
        // up, and the other's project is simply absent from the answer.
        snapshot.apply(&ModDetailIndex::default());

        assert_eq!(snapshot.len(), 2, "both mods are still installed");
        for entry in &snapshot.mods {
            assert!(entry.details.is_none());
            assert!(entry.icon.is_none());
            assert_eq!(
                entry.installed_version(),
                None,
                "no version is claimed that the launcher cannot prove"
            );
        }
        assert_eq!(
            snapshot.mods[0].display_name(),
            "mystery.jar",
            "a file name is an acceptable label and nothing more"
        );
        assert_eq!(snapshot.mods[0].identity.provider(), None);
    }

    #[test]
    fn a_known_project_with_an_unknown_release_gets_a_title_but_no_version() {
        let dir = game_dir_with(&["older.jar"]);
        let mut snapshot = snapshot_of(vec![ManagedModEntry::from_recorded(
            recorded(
                "older.jar",
                ModOrigin::PackDeclared,
                ModIdentity::Project {
                    provider: ModProvider::Modrinth,
                    project_id: "lfHFW1mp".to_owned(),
                },
            ),
            dir.path(),
        )]);

        snapshot.apply(&detail_index(
            vec![catalog_project("lfHFW1mp", "JourneyMap", None)],
            vec![catalog_version("GKQEbfSt", "lfHFW1mp", "5.9.18")],
            BTreeMap::new(),
        ));

        let entry = &snapshot.mods[0];
        assert_eq!(entry.display_name(), "JourneyMap");
        assert_eq!(
            entry.installed_version(),
            None,
            "the catalog knows a release; nothing proves it is the installed one"
        );
        assert!(entry.is_pack_owned());
    }

    #[test]
    fn artwork_that_never_arrives_costs_the_row_nothing() {
        let dir = game_dir_with(&["jei.jar"]);
        let mut snapshot = snapshot_of(vec![ManagedModEntry::from_recorded(
            recorded(
                "jei.jar",
                ModOrigin::UserSelected,
                version_identity("u6dRKJwZ", "Orv4ObRj"),
            ),
            dir.path(),
        )]);

        // The project publishes an icon and the fetch produced nothing.
        snapshot.apply(&detail_index(
            vec![catalog_project(
                "u6dRKJwZ",
                "Just Enough Items",
                Some("https://cdn.modrinth.com/data/u6dRKJwZ/icon.png"),
            )],
            vec![catalog_version("Orv4ObRj", "u6dRKJwZ", "15.2.0.27")],
            BTreeMap::new(),
        ));

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.mods[0].icon, None);
        assert_eq!(snapshot.mods[0].display_name(), "Just Enough Items");
    }

    #[test]
    fn only_projects_this_provider_can_resolve_are_asked_about() {
        let dir = game_dir_with(&[]);
        let snapshot = snapshot_of(vec![
            ManagedModEntry::from_recorded(
                recorded(
                    "a.jar",
                    ModOrigin::PackDeclared,
                    version_identity("AAAAAAAA", "1111aaaa"),
                ),
                dir.path(),
            ),
            ManagedModEntry::from_recorded(
                recorded(
                    "b.jar",
                    ModOrigin::PackDeclared,
                    ModIdentity::Project {
                        provider: ModProvider::Modrinth,
                        project_id: "AAAAAAAA".to_owned(),
                    },
                ),
                dir.path(),
            ),
            ManagedModEntry::from_recorded(
                recorded("c.jar", ModOrigin::PackDeclared, ModIdentity::Unresolved),
                dir.path(),
            ),
        ]);

        assert_eq!(
            snapshot.identified_project_ids(ModProvider::Modrinth),
            vec!["AAAAAAAA".to_owned()],
            "one project, asked for once, however many jars point at it"
        );
        assert_eq!(
            snapshot.identified_version_ids(ModProvider::Modrinth),
            vec!["1111aaaa".to_owned()],
            "a mod with no proven release contributes no version to look up"
        );
        assert_eq!(snapshot.missing().count(), 3);
    }
}
