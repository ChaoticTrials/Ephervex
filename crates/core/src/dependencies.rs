//! What an instance's mods need from each other.
//!
//! A recipe entry records the relations its catalog declared about that file
//! (ADR 0034). Installing a mod and importing a CurseForge pack record them as
//! they go, because both already hold the catalog's answer. Everything else
//! does not: a Modrinth pack index declares no relations at all, and an
//! instance built before relations were recorded has none.
//!
//! This module is how those instances catch up. It is an explicit, user-invoked
//! refresh and never part of reading a mod list, which has to stay a directory
//! read that reaches no network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ephervex_download::OperationReporter;
use ephervex_model::{ModOrigin, ModProvider, ModRelationKind};
use ephervex_pack::{ModIdentity, PackRecipe, RecipeRelation, RelationTarget, SourceStore};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};
use crate::mods::{InstalledHost, ModPresence, recipe_relations};

/// What a dependency refresh learned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyRefreshReport {
    /// Mods whose recorded relations changed.
    pub recorded: usize,
    /// Mods whose catalog answer matched what was already recorded.
    pub unchanged: usize,
    /// Mods with no exact catalog release to ask about, so nothing could be
    /// asked and nothing is claimed.
    pub unidentified: usize,
    /// Catalogs that could not be reached. Their mods keep whatever they had,
    /// which for most of them is nothing recorded at all.
    pub providers_unavailable: Vec<ModProvider>,
}

impl DependencyRefreshReport {
    /// Whether anything about the host's recorded state changed.
    #[must_use]
    pub const fn changed_anything(&self) -> bool {
        self.recorded > 0
    }

    /// Whether every mod that could be asked about was answered.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.providers_unavailable.is_empty()
    }
}

/// How much is known about what one mod requires.
///
/// Separate from what it requires, because "this mod needs nothing" and "nobody
/// has asked what this mod needs" are different answers that a warning has to
/// give differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKnowledge {
    /// A catalog answered. The answer may be that it needs nothing.
    Recorded,
    /// Identified, but never asked. A refresh can answer this one.
    Unrecorded,
    /// The project is known and the exact release is not, so there is no
    /// release to ask about. A refresh cannot answer this one.
    Unidentified,
    /// Nothing a catalog can be asked about at all.
    Unidentifiable,
}

impl DependencyKnowledge {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Unrecorded => "unrecorded",
            Self::Unidentified => "unidentified",
            Self::Unidentifiable => "unidentifiable",
        }
    }

    /// Whether asking a catalog again could turn this into an answer.
    #[must_use]
    pub const fn a_refresh_would_help(self) -> bool {
        matches!(self, Self::Unrecorded)
    }
}

/// One mod named by a dependency question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactedMod {
    pub logical_path: PathBuf,
    /// What to call it: a provider title where one is known, else the file name.
    pub name: String,
    pub presence: ModPresence,
    pub origin: ModOrigin,
    /// The catalog project, where one is recorded.
    pub project_id: Option<String>,
    pub knowledge: DependencyKnowledge,
    /// How far from the mod that was asked about. One is a direct dependent.
    pub distance: u32,
}

/// What else stops loading when one mod does.
///
/// The answer to "if I turn this off, or delete its jar, what goes with it".
/// Deleting and disabling give the same answer, because the loader sees the
/// same absence either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependentMods {
    pub target: ImpactedMod,
    /// Enabled mods that do not load without the target, transitively. Proven.
    pub breaks: Vec<ImpactedMod>,
    /// Enabled mods that declare the target optional. Named, never cascaded:
    /// they keep loading and do less.
    pub degrades: Vec<ImpactedMod>,
    /// Enabled mods nothing has asked about yet, so whether they need the
    /// target is unknown. A refresh would answer.
    pub unknown: Vec<ImpactedMod>,
    /// The same, for mods no refresh can answer about.
    pub unidentifiable: Vec<ImpactedMod>,
}

impl DependentMods {
    /// Whether anything at all is worth putting in front of the user.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.breaks.is_empty()
            && self.degrades.is_empty()
            && self.unknown.is_empty()
            && self.unidentifiable.is_empty()
    }

    /// Whether every other installed mod gave a real answer.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unknown.is_empty() && self.unidentifiable.is_empty()
    }
}

/// What one mod needs before it can load.
///
/// The inverse question, asked when a mod is turned back on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredMods {
    pub target: ImpactedMod,
    /// Required mods that are installed but turned off. Turning them on is a
    /// rename and is exactly reversible, so it is offered.
    pub disabled: Vec<ImpactedMod>,
    /// Required projects with no jar in this host at all. Reported and never
    /// fetched: installing one reaches a catalog, which a toggle must not do.
    pub absent: Vec<ImpactedMod>,
    /// Installed mods the target declares it does not work alongside. A
    /// warning, never a refusal: reproducing exactly that is a reason to run
    /// this launcher.
    pub incompatible: Vec<ImpactedMod>,
}

impl RequiredMods {
    /// Whether anything at all is worth putting in front of the user.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.disabled.is_empty() && self.absent.is_empty() && self.incompatible.is_empty()
    }
}

/// One mod named by a recorded relation, and what that relation said.
///
/// A relation names whatever the catalog gave. Where a jar here matches it,
/// this carries that jar; where none does, it carries the catalog's own name
/// and no presence, because claiming a state for a file that is not here would
/// be inventing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedMod {
    pub kind: ModRelationKind,
    pub name: String,
    /// The jar this relation is about, when one is here.
    pub logical_path: Option<PathBuf>,
    /// How that jar stands, when one is here.
    pub presence: Option<ModPresence>,
    pub project_id: Option<String>,
}

/// What one mod declares, and what declares it.
///
/// Direct relations only. The closure a warning needs is
/// [`App::dependents_of`]; this is the inventory behind it, and showing a
/// mod's own edges as its own is what lets a reader check that answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModRelations {
    pub target: ImpactedMod,
    /// What this mod's entry declares, in the order the catalog stated it.
    pub requires: Vec<RelatedMod>,
    /// Mods of this host whose entries name this one.
    pub required_by: Vec<RelatedMod>,
}

impl ModRelations {
    /// Whether anything at all was recorded in either direction.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requires.is_empty() && self.required_by.is_empty()
    }
}

/// What a frontend does about a mod change that affects other mods.
///
/// One rule, in one place, for both frontends and both directions. Deriving it
/// from the two preferences at each of the six call sites is how the desktop
/// application and the command line would come to disagree about when a change
/// is quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyPolicy {
    /// Work out what else the change affects and stop for an answer.
    Confirm,
    /// Work out what else it affects, change those too, and report it.
    Apply,
    /// Change the named mod and nothing else.
    Ignore,
}

impl DependencyPolicy {
    /// A stable identifier for `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Apply => "apply",
            Self::Ignore => "ignore",
        }
    }

    /// Whether a change carries the mods its target's relations name.
    #[must_use]
    pub const fn cascades(self) -> bool {
        matches!(self, Self::Confirm | Self::Apply)
    }

    /// Whether a change with something to report stops for an answer.
    #[must_use]
    pub const fn asks(self) -> bool {
        matches!(self, Self::Confirm)
    }
}

impl App {
    /// What a mod change that affects other mods does here.
    ///
    /// Both preferences resolve into one answer, because they are not
    /// independent: with the change acting on nothing else, there is nothing
    /// for a confirmation to be about.
    #[must_use]
    pub fn dependency_policy(&self) -> DependencyPolicy {
        if !self.follow_mod_dependencies() {
            DependencyPolicy::Ignore
        } else if self.warn_on_dependency_impact() {
            DependencyPolicy::Confirm
        } else {
            DependencyPolicy::Apply
        }
    }

    /// Whether a mod change acts on the mods its relations name.
    ///
    /// Off, a switch changes exactly the mod it belongs to. What each mod
    /// requires is still recorded and still shown: this decides whether a
    /// change follows those relations, not whether they are known.
    #[must_use]
    pub fn follow_mod_dependencies(&self) -> bool {
        self.config().mods.follow_mod_dependencies
    }

    /// Records whether mod changes follow recorded relations.
    pub fn set_follow_mod_dependencies(&mut self, follow: bool) -> CoreResult<()> {
        self.mutate_config(|config| config.mods.follow_mod_dependencies = follow)
    }

    /// Whether a mod change that affects other mods stops to be confirmed.
    ///
    /// The answer decides only whether a frontend asks. What the change does is
    /// the same either way, and so is the report of what it did. Read through
    /// [`Self::dependency_policy`] rather than on its own: it says nothing
    /// about a launcher that is not following relations at all.
    #[must_use]
    pub fn warn_on_dependency_impact(&self) -> bool {
        self.config().mods.warn_on_dependency_impact
    }

    /// Records whether that confirmation is wanted.
    pub fn set_warn_on_dependency_impact(&mut self, warn: bool) -> CoreResult<()> {
        self.mutate_config(|config| config.mods.warn_on_dependency_impact = warn)
    }

    /// What else stops loading if this mod does.
    ///
    /// The question behind every disable and every delete: both take the jar
    /// out of the loader's view, so both get the same answer.
    ///
    /// Offline and pure. It reads the recipe and the directory, and reaches no
    /// catalog, so a frontend may ask it on every toggle.
    ///
    /// Proven and unproven are kept apart. A mod is in `breaks` only because a
    /// recorded relation says so; a mod nothing has asked about is reported as
    /// unknown rather than assumed independent, which is the assumption that
    /// would make this warning confidently wrong.
    pub fn dependents_of(
        &self,
        host: InstalledHost,
        logical_path: &Path,
    ) -> CoreResult<DependentMods> {
        let graph = ModGraph::build(self, host)?;
        let index = graph
            .index_of(logical_path)
            .ok_or_else(|| CoreError::ModFileMissing {
                path: logical_path.to_path_buf(),
            })?;
        let target = graph.entries[index].mod_entry.clone();

        let breaks = graph.dependents_of(index);
        let named: BTreeSet<PathBuf> = breaks
            .iter()
            .map(|entry| entry.logical_path.clone())
            .collect();

        let mut degrades = Vec::new();
        let mut unknown = Vec::new();
        let mut unidentifiable = Vec::new();
        for (candidate, entry) in graph.entries.iter().enumerate() {
            if candidate == index
                || entry.mod_entry.presence != ModPresence::Installed
                || named.contains(&entry.mod_entry.logical_path)
            {
                continue;
            }
            match entry.mod_entry.knowledge {
                DependencyKnowledge::Recorded => {
                    if graph.declares(candidate, index, ModRelationKind::Optional) {
                        degrades.push(ImpactedMod {
                            distance: 1,
                            ..entry.mod_entry.clone()
                        });
                    }
                }
                DependencyKnowledge::Unrecorded => unknown.push(entry.mod_entry.clone()),
                DependencyKnowledge::Unidentified | DependencyKnowledge::Unidentifiable => {
                    unidentifiable.push(entry.mod_entry.clone());
                }
            }
        }

        Ok(DependentMods {
            target,
            breaks,
            degrades,
            unknown,
            unidentifiable,
        })
    }

    /// What this mod needs before it can load.
    ///
    /// The inverse of [`Self::dependents_of`], and what a frontend asks before
    /// turning a mod back on. Offline and pure for the same reasons.
    pub fn dependencies_of(
        &self,
        host: InstalledHost,
        logical_path: &Path,
    ) -> CoreResult<RequiredMods> {
        let graph = ModGraph::build(self, host)?;
        let index = graph
            .index_of(logical_path)
            .ok_or_else(|| CoreError::ModFileMissing {
                path: logical_path.to_path_buf(),
            })?;
        let target = graph.entries[index].mod_entry.clone();

        let mut disabled = Vec::new();
        let mut absent = Vec::new();
        let mut incompatible = Vec::new();
        // Transitively, because a dependency's own dependencies are needed just
        // as much: turning a mod on and leaving something two steps down it off
        // produces a set that still does not load.
        let mut seen: BTreeSet<usize> = BTreeSet::from([index]);
        let mut frontier = vec![index];
        let mut distance = 0;
        while !frontier.is_empty() {
            distance += 1;
            let mut next = Vec::new();
            for current in frontier {
                for relation in graph.entries[current].relations.iter().flatten() {
                    let found = graph.target_of(relation);
                    match (relation.kind, found) {
                        (ModRelationKind::Required, Some(found)) => {
                            if !seen.insert(found) {
                                continue;
                            }
                            let entry = &graph.entries[found].mod_entry;
                            match entry.presence {
                                // Already loading, and still walked: something
                                // it needs may itself be turned off.
                                ModPresence::Installed => {}
                                ModPresence::Disabled => disabled.push(ImpactedMod {
                                    distance,
                                    ..entry.clone()
                                }),
                                ModPresence::Missing => absent.push(ImpactedMod {
                                    distance,
                                    ..entry.clone()
                                }),
                            }
                            next.push(found);
                        }
                        // Named by whatever the catalog gave, because there is
                        // no jar here to name it any better. Nothing is fetched
                        // for it, and nothing below it can be walked.
                        (ModRelationKind::Required, None) => absent.push(ImpactedMod {
                            logical_path: PathBuf::new(),
                            name: relation.target.display_name().to_owned(),
                            presence: ModPresence::Missing,
                            origin: ModOrigin::RequiredDependency,
                            project_id: relation.target.project_id().map(ToOwned::to_owned),
                            knowledge: DependencyKnowledge::Recorded,
                            distance,
                        }),
                        (ModRelationKind::Incompatible, Some(found))
                            if graph.entries[found].mod_entry.presence
                                == ModPresence::Installed =>
                        {
                            incompatible.push(ImpactedMod {
                                distance,
                                ..graph.entries[found].mod_entry.clone()
                            });
                        }
                        _ => {}
                    }
                }
            }
            frontier = next;
        }

        Ok(RequiredMods {
            target,
            disabled,
            absent,
            incompatible,
        })
    }

    /// What this mod declares, and what declares it.
    ///
    /// Offline and pure like the other two, and direct rather than transitive:
    /// it answers "what was recorded about this mod", which is the question a
    /// reader asks of one mod's own page.
    pub fn relations_of(
        &self,
        host: InstalledHost,
        logical_path: &Path,
    ) -> CoreResult<ModRelations> {
        let graph = ModGraph::build(self, host)?;
        let index = graph
            .index_of(logical_path)
            .ok_or_else(|| CoreError::ModFileMissing {
                path: logical_path.to_path_buf(),
            })?;
        Ok(graph.relations_of(index))
    }

    /// The same, for every mod of the host, in the order the list shows them.
    ///
    /// One graph answers all of them. Asking per mod would re-read the recipe
    /// and re-walk the directory once per jar, which on an instance with a
    /// couple of hundred is the difference between an answer and a pause.
    pub fn mod_relations(&self, host: InstalledHost) -> CoreResult<Vec<ModRelations>> {
        let graph = ModGraph::build(self, host)?;
        Ok((0..graph.entries.len())
            .map(|index| graph.relations_of(index))
            .collect())
    }

    /// Deletes several mods' jars in one call.
    ///
    /// Ordered exactly as a disable is, and for the same reason: what
    /// everything needs goes last, so an interrupted run never leaves a mod
    /// loading with its library already gone.
    ///
    /// The set to pass is the one [`Self::dependents_of`] proves, which is what
    /// keeps deleting and disabling from ever disagreeing about consequences.
    /// Each deletion is the same [`App::delete_mod_file`] the list offers, so
    /// what each origin allows, and what record survives the file, is decided
    /// in one place rather than two.
    pub fn delete_mod_files(
        &self,
        host: InstalledHost,
        paths: &[PathBuf],
    ) -> CoreResult<Vec<PathBuf>> {
        let graph = ModGraph::build(self, host)?;
        let mut ordered: Vec<&PathBuf> = paths.iter().collect();
        ordered.sort_by_key(|path| {
            graph
                .index_of(path)
                .map_or(0, |index| graph.dependents_of(index).len())
        });

        let mut deleted = Vec::with_capacity(ordered.len());
        for path in ordered {
            self.delete_mod_file(host, path)?;
            deleted.push(path.clone());
        }
        Ok(deleted)
    }

    /// Turns several mods on or off in one call.
    ///
    /// Ordered so that any prefix of it leaves a mod set the loader can start:
    /// disables go dependents first, enables go dependencies first. A frontend
    /// issuing one rename per row cannot promise that, and an operation that
    /// dies halfway through a cascade would otherwise leave a dependency gone
    /// and something that needs it still loading.
    ///
    /// Each change is the same rename [`App::set_mod_enabled`] performs, and is
    /// idempotent for the same reason.
    pub fn set_mods_enabled(
        &self,
        host: InstalledHost,
        changes: &[(PathBuf, bool)],
    ) -> CoreResult<Vec<(PathBuf, ModPresence)>> {
        let graph = ModGraph::build(self, host)?;
        let mut ordered: Vec<&(PathBuf, bool)> = changes.iter().collect();
        ordered.sort_by_key(|(path, enabled)| {
            let depth = graph
                .index_of(path)
                .map_or(0, |index| graph.dependents_of(index).len());
            // Disabling: fewest dependents first, so what everything needs goes
            // last and nothing loses what it needs while still loading.
            // Enabling: the same order reversed, so nothing loads before what
            // it needs.
            if *enabled { usize::MAX - depth } else { depth }
        });

        let mut applied = Vec::with_capacity(ordered.len());
        for (path, enabled) in ordered {
            applied.push((path.clone(), self.set_mod_enabled(host, path, *enabled)?));
        }
        Ok(applied)
    }

    /// Asks each catalog what this host's mods need, and records the answers.
    ///
    /// Explicit on purpose. It reaches the network, so it is something a user
    /// asks for rather than something a mod list does on its own.
    ///
    /// A catalog that cannot be reached is reported and costs only its own
    /// mods' answers: the rest are still recorded, and the ones that were not
    /// stay unasked rather than being written as needing nothing. A mod with no
    /// exact catalog release is counted and skipped - there is no version to
    /// ask about, and asking about the project would answer for a release the
    /// instance does not have.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn refresh_mod_dependencies(
        &self,
        host: InstalledHost,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<DependencyRefreshReport> {
        let layout = self.host_layout(host)?;
        let mut report = DependencyRefreshReport::default();
        // A host with no recipe records nothing and has nothing to refresh.
        let Some(mut recipe) = layout.recipe.clone() else {
            return Ok(report);
        };

        report.unidentified = recipe
            .files
            .iter()
            .filter(|file| file.is_mod_file())
            .filter(|file| {
                !matches!(
                    file.identity.as_ref().map_or(
                        ModIdentity::Unresolved,
                        ephervex_pack::ProviderFileIdentity::resolution
                    ),
                    ModIdentity::Version { .. }
                )
            })
            .count();

        for catalog in [ModProvider::Modrinth, ModProvider::CurseForge] {
            let versions = exact_versions(&recipe, catalog);
            if versions.is_empty() {
                continue;
            }
            let provider = match self.mod_provider(catalog) {
                Ok(provider) => provider,
                Err(error) => {
                    warn!(%error, provider = %catalog, "no client for this catalog");
                    report.providers_unavailable.push(catalog);
                    continue;
                }
            };
            let answers = match provider.versions_by_id(&versions, reporter, cancel).await {
                Ok(answers) => answers,
                Err(error) => {
                    warn!(%error, provider = %catalog, "dependencies could not be fetched");
                    report.providers_unavailable.push(catalog);
                    continue;
                }
            };

            for answer in answers {
                let relations = recipe_relations(&answer.dependencies);
                for file in &mut recipe.files {
                    if file
                        .identity
                        .as_ref()
                        .and_then(|identity| identity.version_id.as_deref())
                        != Some(answer.id.as_str())
                    {
                        continue;
                    }
                    if file.relations.as_ref() == Some(&relations) {
                        report.unchanged += 1;
                    } else {
                        file.relations = Some(relations.clone());
                        report.recorded += 1;
                    }
                }
            }
        }

        if report.changed_anything() {
            SourceStore::new(ephervex_storage::metadata::source_dir(&layout.root))
                .write_recipe(&recipe)?;
        }
        info!(
            recorded = report.recorded,
            unchanged = report.unchanged,
            unidentified = report.unidentified,
            "refreshed what this host's mods require"
        );
        Ok(report)
    }
}

/// One host's mods, indexed for dependency questions.
///
/// Built from durable state alone: the recipe says what each mod requires and
/// the directory says which of them are loading. Reaches no network, so a
/// frontend may ask as often as it likes.
#[derive(Debug)]
struct ModGraph {
    /// Every mod of the host, in the order the snapshot listed them.
    entries: Vec<GraphEntry>,
    /// Where a catalog project sits in `entries`.
    by_project: BTreeMap<String, usize>,
    /// Where a jar name sits, lowercased, for a relation that named only one.
    by_file_name: BTreeMap<String, usize>,
}

#[derive(Debug)]
struct GraphEntry {
    mod_entry: ImpactedMod,
    /// What this mod's catalog said it needs, when anything asked.
    relations: Option<Vec<RecipeRelation>>,
}

impl ModGraph {
    /// Builds the index for one host.
    fn build(app: &App, host: InstalledHost) -> CoreResult<Self> {
        let layout = app.host_layout(host)?;
        let snapshot = app.host_mod_snapshot(host)?;
        let relations: BTreeMap<PathBuf, Option<Vec<RecipeRelation>>> = layout
            .recipe
            .as_ref()
            .map(|recipe| {
                recipe
                    .files
                    .iter()
                    .map(|file| (file.logical_path.clone(), file.relations.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut entries = Vec::with_capacity(snapshot.mods.len());
        let mut by_project = BTreeMap::new();
        let mut by_file_name = BTreeMap::new();
        for entry in &snapshot.mods {
            let recorded = relations.get(&entry.logical_path).cloned().flatten();
            let project_id = entry.identity.project_id().map(ToOwned::to_owned);
            if let Some(project) = project_id.clone() {
                by_project.insert(project, entries.len());
            }
            by_file_name.insert(entry.file_name.to_ascii_lowercase(), entries.len());
            entries.push(GraphEntry {
                mod_entry: ImpactedMod {
                    logical_path: entry.logical_path.clone(),
                    name: entry.display_name().to_owned(),
                    presence: entry.presence,
                    origin: entry.origin,
                    project_id,
                    knowledge: knowledge_of(&entry.identity, recorded.as_deref()),
                    distance: 0,
                },
                relations: recorded,
            });
        }
        Ok(Self {
            entries,
            by_project,
            by_file_name,
        })
    }

    /// The mod a relation is about, whichever way the catalog named it.
    fn target_of(&self, relation: &RecipeRelation) -> Option<usize> {
        match &relation.target {
            RelationTarget::Project { project_id, .. } => self.by_project.get(project_id),
            RelationTarget::File { file_name } => {
                self.by_file_name.get(&file_name.to_ascii_lowercase())
            }
        }
        .copied()
    }

    /// Where a mod sits, by the path a frontend named it with.
    fn index_of(&self, logical_path: &Path) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.mod_entry.logical_path == logical_path)
    }

    /// Whether this mod declares a relation of one kind on another mod.
    fn declares(&self, index: usize, about: usize, kind: ModRelationKind) -> bool {
        let target = &self.entries[about].mod_entry;
        let file_name = target
            .logical_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default();
        self.entries[index]
            .relations
            .as_ref()
            .is_some_and(|relations| {
                relations.iter().any(|relation| {
                    relation.kind == kind && relation.names(target.project_id.as_deref(), file_name)
                })
            })
    }

    /// What the mod at `index` declares, and what declares it.
    fn relations_of(&self, index: usize) -> ModRelations {
        let target = self.entries[index].mod_entry.clone();
        let requires = self.entries[index]
            .relations
            .iter()
            .flatten()
            .map(|relation| {
                self.target_of(relation).map_or_else(
                    || RelatedMod {
                        kind: relation.kind,
                        name: relation.target.display_name().to_owned(),
                        logical_path: None,
                        presence: None,
                        project_id: relation.target.project_id().map(ToOwned::to_owned),
                    },
                    |found| {
                        let entry = &self.entries[found].mod_entry;
                        RelatedMod {
                            kind: relation.kind,
                            name: entry.name.clone(),
                            logical_path: Some(entry.logical_path.clone()),
                            presence: Some(entry.presence),
                            project_id: entry.project_id.clone(),
                        }
                    },
                )
            })
            .collect();

        let file_name = target
            .logical_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default();
        let mut required_by = Vec::new();
        for (candidate, entry) in self.entries.iter().enumerate() {
            if candidate == index {
                continue;
            }
            for relation in entry.relations.iter().flatten() {
                if relation.names(target.project_id.as_deref(), file_name) {
                    required_by.push(RelatedMod {
                        kind: relation.kind,
                        name: entry.mod_entry.name.clone(),
                        logical_path: Some(entry.mod_entry.logical_path.clone()),
                        presence: Some(entry.mod_entry.presence),
                        project_id: entry.mod_entry.project_id.clone(),
                    });
                }
            }
        }

        ModRelations {
            target,
            requires,
            required_by,
        }
    }

    /// Every enabled mod that does not load without `start`, transitively.
    ///
    /// Breadth first, so the distance recorded on each mod is the shortest
    /// chain that reaches it, which is what makes a list read as an
    /// explanation rather than an accident of traversal order.
    fn dependents_of(&self, start: usize) -> Vec<ImpactedMod> {
        let mut found: BTreeMap<usize, u32> = BTreeMap::new();
        let mut frontier = vec![start];
        let mut distance = 0;
        while !frontier.is_empty() {
            distance += 1;
            let mut next = Vec::new();
            for index in frontier {
                for (candidate, entry) in self.entries.iter().enumerate() {
                    if candidate == start
                        || found.contains_key(&candidate)
                        || entry.mod_entry.presence != ModPresence::Installed
                        || !self.declares(candidate, index, ModRelationKind::Required)
                    {
                        continue;
                    }
                    found.insert(candidate, distance);
                    next.push(candidate);
                }
            }
            frontier = next;
        }
        found
            .into_iter()
            .map(|(index, distance)| ImpactedMod {
                distance,
                ..self.entries[index].mod_entry.clone()
            })
            .collect()
    }
}

/// How much is known about what one mod requires.
fn knowledge_of(
    identity: &ModIdentity,
    relations: Option<&[RecipeRelation]>,
) -> DependencyKnowledge {
    if relations.is_some() {
        return DependencyKnowledge::Recorded;
    }
    match identity {
        ModIdentity::Version { .. } => DependencyKnowledge::Unrecorded,
        ModIdentity::Project { .. } => DependencyKnowledge::Unidentified,
        ModIdentity::Unresolved => DependencyKnowledge::Unidentifiable,
    }
}

/// The exact catalog releases one catalog can be asked about.
///
/// Mods only, and only those whose recorded identity names a release. A project
/// without one has nothing to look up: asking about the project would answer
/// for whichever release the catalog considers current, which is not the one
/// installed here.
fn exact_versions(recipe: &PackRecipe, catalog: ModProvider) -> Vec<String> {
    let mut versions: Vec<String> = recipe
        .files
        .iter()
        .filter(|file| file.is_mod_file())
        .filter_map(|file| match file.identity.as_ref()?.resolution() {
            ModIdentity::Version {
                provider,
                version_id,
                ..
            } if provider == catalog => Some(version_id),
            _ => None,
        })
        .collect();
    versions.sort_unstable();
    versions.dedup();
    versions
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::{Path, PathBuf};

    use ephervex_model::{Hash, HashAlgorithm, InstanceId, LoaderKind, LoaderSelection, ModOrigin};
    use ephervex_pack::{
        PackFileRecipe, PackRecipe, ProviderFileIdentity, SideRequirement, SourceStore,
    };
    use ephervex_storage::{NewClient, Paths};
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path as route, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const LIBRARY: &str = "qEH6GYul";
    const LIBRARY_RELEASE: &str = "f5GBMQmv";
    const DEPENDENT: &str = "por2AZc5";
    const DEPENDENT_RELEASE: &str = "Kxvkni7I";

    /// An instance whose recipe records exact releases and no relations, which
    /// is every instance built before relations were recorded.
    fn instance_with_unasked_mods(base: &std::path::Path) -> (App, InstanceId, PathBuf) {
        let app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Dependency Aware Example",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
            ))
            .expect("create the instance");

        let mut recipe = PackRecipe::for_manual_instance(
            "Dependency Aware Example",
            "1.21.1",
            record.loader.clone(),
        );
        recipe.files = vec![
            recorded("LibX.jar", LIBRARY, LIBRARY_RELEASE),
            recorded("SkyblockBuilder.jar", DEPENDENT, DEPENDENT_RELEASE),
        ];
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        store.write_recipe(&recipe).expect("write the recipe");
        (app, record.id, record.root)
    }

    fn recorded(name: &str, project: &str, version: &str) -> PackFileRecipe {
        PackFileRecipe {
            logical_path: PathBuf::from("mods").join(name),
            hashes: vec![
                Hash::new(
                    HashAlgorithm::Sha1,
                    "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                )
                .expect("valid sha1"),
            ],
            downloads: Vec::new(),
            size: Some(4096),
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::UserSelected,
            identity: Some(ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some(project.to_owned()),
                version_id: Some(version.to_owned()),
                file_id: None,
            }),
            original_identity: None,
            relations: None,
        }
    }

    fn version_json(id: &str, project: &str, requires: &[&str]) -> Value {
        json!({
            "id": id,
            "project_id": project,
            "name": format!("{project} {id}"),
            "version_number": "1.0.0",
            "version_type": "release",
            "game_versions": ["1.21.1"],
            "loaders": ["neoforge"],
            "date_published": "2026-01-01T00:00:00Z",
            "files": [{
                "hashes": {"sha1": "da39a3ee5e6b4b0d3255bfef95601890afd80709"},
                "url": "https://cdn.modrinth.com/data/x/a.jar",
                "filename": "a.jar",
                "primary": true,
                "size": 4096,
            }],
            "dependencies": requires
                .iter()
                .map(|id| json!({"project_id": id, "dependency_type": "required"}))
                .collect::<Vec<_>>(),
        })
    }

    /// One batched answer, which is how the refresh asks: every release the
    /// instance names, in one request per catalog.
    async fn catalog_answering() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(route("/versions"))
            // Sorted and deduplicated by the refresh, so the request is stable
            // whatever order the recipe happens to list its mods in.
            .and(query_param(
                "ids",
                format!("[\"{DEPENDENT_RELEASE}\",\"{LIBRARY_RELEASE}\"]"),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![
                version_json(LIBRARY_RELEASE, LIBRARY, &[]),
                version_json(DEPENDENT_RELEASE, DEPENDENT, &[LIBRARY]),
            ]))
            .mount(&server)
            .await;
        server
    }

    fn recipe_at(root: &std::path::Path) -> PackRecipe {
        SourceStore::new(ephervex_storage::metadata::source_dir(root))
            .read_recipe()
            .expect("the retained recipe")
    }

    const SKYGUIS: &str = "3Gdi7Qgq";
    const UTILITIX: &str = "MMMWYsjm";

    /// The real shape the feature exists for: one library, three mods that
    /// need it, and one optional relation among them that must not cascade.
    fn library_and_its_dependents(base: &std::path::Path) -> (App, InstanceId, PathBuf) {
        let (app, instance, root) = instance_with_unasked_mods(base);
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe.files = vec![
            with_relations(recorded("LibX.jar", LIBRARY, LIBRARY_RELEASE), &[]),
            with_relations(
                recorded("SkyblockBuilder.jar", DEPENDENT, DEPENDENT_RELEASE),
                &[
                    (LIBRARY, ModRelationKind::Required),
                    (SKYGUIS, ModRelationKind::Optional),
                ],
            ),
            with_relations(
                recorded("SkyGUIs.jar", SKYGUIS, "zypaCGkp"),
                &[
                    (LIBRARY, ModRelationKind::Required),
                    (DEPENDENT, ModRelationKind::Required),
                ],
            ),
            with_relations(
                recorded("UtilitiX.jar", UTILITIX, "WyTAmDcK"),
                &[(LIBRARY, ModRelationKind::Required)],
            ),
        ];
        store.write_recipe(&recipe).expect("write");

        let game = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        std::fs::create_dir_all(&game).expect("mods dir");
        for jar in [
            "LibX.jar",
            "SkyblockBuilder.jar",
            "SkyGUIs.jar",
            "UtilitiX.jar",
        ] {
            std::fs::write(game.join(jar), b"jar").expect("write jar");
        }
        (app, instance, root)
    }

    fn with_relations(
        mut file: PackFileRecipe,
        relations: &[(&str, ModRelationKind)],
    ) -> PackFileRecipe {
        file.relations = Some(
            relations
                .iter()
                .map(|(project_id, kind)| RecipeRelation {
                    kind: *kind,
                    target: RelationTarget::Project {
                        project_id: (*project_id).to_owned(),
                        version_id: None,
                    },
                })
                .collect(),
        );
        file
    }

    fn names(mods: &[ImpactedMod]) -> Vec<&str> {
        mods.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn the_two_preferences_resolve_into_one_rule() {
        let base = tempfile::tempdir().expect("temp dir");
        let (mut app, _instance, _root) = library_and_its_dependents(base.path());

        assert_eq!(app.dependency_policy(), DependencyPolicy::Confirm);
        assert!(app.dependency_policy().cascades() && app.dependency_policy().asks());

        app.set_warn_on_dependency_impact(false).expect("record");
        assert_eq!(
            app.dependency_policy(),
            DependencyPolicy::Apply,
            "not asking is not the same as not acting"
        );
        assert!(app.dependency_policy().cascades());

        app.set_follow_mod_dependencies(false).expect("record");
        assert_eq!(app.dependency_policy(), DependencyPolicy::Ignore);
        assert!(!app.dependency_policy().cascades());

        // The confirmation's own answer is still recorded, and comes back when
        // relations are followed again: one preference must not silently
        // rewrite the other.
        app.set_warn_on_dependency_impact(true).expect("record");
        assert_eq!(
            app.dependency_policy(),
            DependencyPolicy::Ignore,
            "with nothing else changing there is nothing to confirm"
        );
        app.set_follow_mod_dependencies(true).expect("record");
        assert_eq!(app.dependency_policy(), DependencyPolicy::Confirm);
    }

    #[test]
    fn disabling_a_library_names_every_installed_mod_that_requires_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, _root) = library_and_its_dependents(base.path());

        let impact = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/LibX.jar"),
            )
            .expect("impact");

        assert_eq!(
            names(&impact.breaks),
            ["SkyblockBuilder.jar", "SkyGUIs.jar", "UtilitiX.jar"],
            "all three stop loading, and the launcher can prove it"
        );
        assert!(impact.is_complete(), "every other mod gave a real answer");
        assert!(!impact.is_silent());
    }

    #[test]
    fn a_mod_names_its_own_relations_in_both_directions() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, _root) = library_and_its_dependents(base.path());

        let relations = app
            .relations_of(
                InstalledHost::Instance(instance),
                Path::new("mods/SkyblockBuilder.jar"),
            )
            .expect("relations");

        assert_eq!(
            relations
                .requires
                .iter()
                .map(|related| (related.name.as_str(), related.kind))
                .collect::<Vec<_>>(),
            [
                ("LibX.jar", ModRelationKind::Required),
                ("SkyGUIs.jar", ModRelationKind::Optional),
            ],
            "an optional relation is shown as its own kind rather than left out"
        );
        assert_eq!(
            relations
                .required_by
                .iter()
                .map(|related| (related.name.as_str(), related.kind))
                .collect::<Vec<_>>(),
            [("SkyGUIs.jar", ModRelationKind::Required)],
            "direct dependents only: UtilitiX needs the library, not this"
        );
        assert_eq!(
            relations.requires[0].presence,
            Some(ModPresence::Installed),
            "a relation the instance holds carries how that jar stands"
        );
    }

    #[test]
    fn every_mod_of_a_host_is_answered_from_one_graph() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, _root) = library_and_its_dependents(base.path());

        let all = app
            .mod_relations(InstalledHost::Instance(instance))
            .expect("relations");

        assert_eq!(
            all.iter()
                .map(|entry| entry.target.name.as_str())
                .collect::<Vec<_>>(),
            [
                "LibX.jar",
                "SkyblockBuilder.jar",
                "SkyGUIs.jar",
                "UtilitiX.jar"
            ],
            "every mod of the host is listed, in the order the list shows them"
        );
        let library = all
            .iter()
            .find(|entry| entry.target.name == "LibX.jar")
            .expect("the library");
        assert!(library.requires.is_empty());
        assert_eq!(
            library
                .required_by
                .iter()
                .map(|related| related.name.as_str())
                .collect::<Vec<_>>(),
            ["SkyblockBuilder.jar", "SkyGUIs.jar", "UtilitiX.jar"]
        );
        for entry in &all {
            assert_eq!(
                *entry,
                app.relations_of(
                    InstalledHost::Instance(instance),
                    &entry.target.logical_path
                )
                .expect("one mod"),
                "the whole-host answer and the per-mod answer are the same answer"
            );
        }
    }

    #[test]
    fn a_relation_no_jar_here_matches_carries_a_name_and_no_state() {
        // Claiming a presence for a file that is not installed would invent
        // one. The catalog's own name is all there is to show.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe
            .files
            .iter_mut()
            .find(|file| file.file_name() == "UtilitiX.jar")
            .expect("the mod")
            .relations = Some(vec![RecipeRelation {
            kind: ModRelationKind::Required,
            target: RelationTarget::Project {
                project_id: "AABBCCDD".to_owned(),
                version_id: None,
            },
        }]);
        store.write_recipe(&recipe).expect("write");

        let relations = app
            .relations_of(
                InstalledHost::Instance(instance),
                Path::new("mods/UtilitiX.jar"),
            )
            .expect("relations");

        assert_eq!(relations.requires.len(), 1);
        assert_eq!(relations.requires[0].name, "AABBCCDD");
        assert_eq!(relations.requires[0].presence, None);
        assert_eq!(relations.requires[0].logical_path, None);
    }

    #[test]
    fn a_relation_that_names_only_a_jar_still_matches_the_installed_file() {
        // Modrinth may state a dependency by file name and no project. That is
        // still an answer, and the mod it names still stops loading.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe
            .files
            .iter_mut()
            .find(|file| file.file_name() == "UtilitiX.jar")
            .expect("the mod")
            .relations = Some(vec![RecipeRelation {
            kind: ModRelationKind::Required,
            target: RelationTarget::File {
                file_name: "libx.JAR".to_owned(),
            },
        }]);
        store.write_recipe(&recipe).expect("write");

        let impact = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/LibX.jar"),
            )
            .expect("impact");
        assert!(
            names(&impact.breaks).contains(&"UtilitiX.jar"),
            "a jar name is matched case-insensitively, like the filesystem entry it names"
        );
    }

    #[test]
    fn a_mod_reached_only_through_an_optional_relation_never_breaks() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, _root) = library_and_its_dependents(base.path());

        // Skyblock Builder declares Sky GUIs optional. Turning Sky GUIs off
        // costs a feature, so nothing may cascade from it.
        let impact = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/SkyGUIs.jar"),
            )
            .expect("impact");

        assert!(impact.breaks.is_empty(), "nothing requires it");
        assert_eq!(names(&impact.degrades), ["SkyblockBuilder.jar"]);
    }

    #[test]
    fn the_break_set_is_transitive() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, _root) = library_and_its_dependents(base.path());

        // Sky GUIs requires Skyblock Builder, which requires nothing else here,
        // so turning Skyblock Builder off reaches Sky GUIs at distance one.
        let impact = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/SkyblockBuilder.jar"),
            )
            .expect("impact");
        assert_eq!(names(&impact.breaks), ["SkyGUIs.jar"]);
        assert_eq!(impact.breaks[0].distance, 1);

        // And from the library, Sky GUIs is reachable both directly and through
        // Skyblock Builder. The shorter chain is the one reported.
        let from_library = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/LibX.jar"),
            )
            .expect("impact");
        let skyguis = from_library
            .breaks
            .iter()
            .find(|entry| entry.name == "SkyGUIs.jar")
            .expect("reached");
        assert_eq!(skyguis.distance, 1, "the shortest chain explains it best");
    }

    #[test]
    fn an_already_disabled_dependent_is_not_reported_as_breaking() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        std::fs::rename(
            mods.join("UtilitiX.jar"),
            mods.join("UtilitiX.jar.disabled"),
        )
        .expect("disable it");

        let impact = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/LibX.jar"),
            )
            .expect("impact");
        assert_eq!(
            names(&impact.breaks),
            ["SkyblockBuilder.jar", "SkyGUIs.jar"],
            "a mod that is already not loading cannot stop loading"
        );
    }

    #[test]
    fn a_mod_nothing_has_asked_about_is_unknown_rather_than_independent() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe
            .files
            .iter_mut()
            .find(|file| file.logical_path == Path::new("mods/UtilitiX.jar"))
            .expect("the mod")
            .relations = None;
        store.write_recipe(&recipe).expect("write");

        let impact = app
            .dependents_of(
                InstalledHost::Instance(instance),
                Path::new("mods/LibX.jar"),
            )
            .expect("impact");

        assert_eq!(
            names(&impact.breaks),
            ["SkyblockBuilder.jar", "SkyGUIs.jar"]
        );
        assert_eq!(names(&impact.unknown), ["UtilitiX.jar"]);
        assert!(
            !impact.is_complete(),
            "a warning that claimed completeness here would be wrong"
        );
        assert!(impact.unknown[0].knowledge.a_refresh_would_help());
    }

    /// Turns every jar of the instance off, the way a disable cascade leaves it.
    fn disable_all(root: &std::path::Path) {
        let mods = ephervex_storage::metadata::client_game_dir(root).join("mods");
        for jar in [
            "LibX.jar",
            "SkyblockBuilder.jar",
            "SkyGUIs.jar",
            "UtilitiX.jar",
        ] {
            std::fs::rename(mods.join(jar), mods.join(format!("{jar}.disabled")))
                .expect("disable it");
        }
    }

    #[test]
    fn enabling_a_mod_names_every_mod_it_needs_and_no_others() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        disable_all(&root);

        let needs = app
            .dependencies_of(
                InstalledHost::Instance(instance),
                Path::new("mods/SkyGUIs.jar"),
            )
            .expect("requirements");
        let mut named = names(&needs.disabled);
        named.sort_unstable();
        assert_eq!(
            named,
            ["LibX.jar", "SkyblockBuilder.jar"],
            "Sky GUIs needs both, and UtilitiX is nobody's business here"
        );

        let needs = app
            .dependencies_of(
                InstalledHost::Instance(instance),
                Path::new("mods/UtilitiX.jar"),
            )
            .expect("requirements");
        assert_eq!(
            names(&needs.disabled),
            ["LibX.jar"],
            "UtilitiX needs the library and nothing else"
        );
    }

    #[test]
    fn enabling_a_mod_reaches_a_dependency_of_its_dependency() {
        // The case one level of walking gets wrong. Sky GUIs is made to need
        // only Skyblock Builder, which still needs the library; turning Sky
        // GUIs on without the library produces a set that does not load.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe
            .files
            .iter_mut()
            .find(|file| file.file_name() == "SkyGUIs.jar")
            .expect("the addon")
            .relations = Some(vec![RecipeRelation {
            kind: ModRelationKind::Required,
            target: RelationTarget::Project {
                project_id: DEPENDENT.to_owned(),
                version_id: None,
            },
        }]);
        store.write_recipe(&recipe).expect("write");
        disable_all(&root);

        let needs = app
            .dependencies_of(
                InstalledHost::Instance(instance),
                Path::new("mods/SkyGUIs.jar"),
            )
            .expect("requirements");
        assert_eq!(
            names(&needs.disabled),
            ["SkyblockBuilder.jar", "LibX.jar"],
            "the library is two steps away and needed all the same"
        );
        assert_eq!(needs.disabled[0].distance, 1);
        assert_eq!(needs.disabled[1].distance, 2);
    }

    #[test]
    fn a_dependency_that_is_already_loading_is_not_offered() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        std::fs::rename(mods.join("LibX.jar"), mods.join("LibX.jar.disabled"))
            .expect("disable the library");

        let needs = app
            .dependencies_of(
                InstalledHost::Instance(instance),
                Path::new("mods/SkyGUIs.jar"),
            )
            .expect("requirements");
        assert_eq!(
            names(&needs.disabled),
            ["LibX.jar"],
            "Skyblock Builder is already loading, so it is not offered"
        );
        assert!(needs.absent.is_empty());
        assert!(!needs.is_silent());
    }

    #[test]
    fn a_required_project_with_no_jar_here_is_reported_and_never_fetched() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe
            .files
            .retain(|file| file.logical_path != Path::new("mods/LibX.jar"));
        store.write_recipe(&recipe).expect("write");

        let needs = app
            .dependencies_of(
                InstalledHost::Instance(instance),
                Path::new("mods/UtilitiX.jar"),
            )
            .expect("requirements");

        assert!(needs.disabled.is_empty());
        assert_eq!(names(&needs.absent), [LIBRARY], "named by project alone");
        assert_eq!(needs.absent[0].presence, ModPresence::Missing);
    }

    #[test]
    fn deleting_a_library_takes_what_needed_it_and_takes_it_first() {
        // The same order a disable uses, for the same reason: no interrupted
        // prefix may leave a mod loading with its library already gone.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let host = InstalledHost::Instance(instance);

        let impact = app
            .dependents_of(host, Path::new("mods/LibX.jar"))
            .expect("impact");
        let mut paths: Vec<PathBuf> = impact
            .breaks
            .iter()
            .map(|entry| entry.logical_path.clone())
            .collect();
        paths.push(PathBuf::from("mods/LibX.jar"));

        let deleted = app.delete_mod_files(host, &paths).expect("delete");
        assert_eq!(
            deleted.last(),
            Some(&PathBuf::from("mods/LibX.jar")),
            "the library goes last"
        );

        let mods = ephervex_storage::metadata::client_game_dir(&root).join("mods");
        assert!(
            std::fs::read_dir(&mods).expect("mods dir").next().is_none(),
            "every jar the cascade named is gone"
        );
    }

    #[test]
    fn a_deleted_dependency_keeps_its_record_so_it_can_come_back() {
        // Deleting is not removing. The recipe still wants the mod, which is
        // what makes the row offer to restore it and what keeps a server built
        // from this recipe complete.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, root) = library_and_its_dependents(base.path());
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));

        let mut recipe = store.read_recipe().expect("recipe");
        recipe
            .files
            .iter_mut()
            .find(|file| file.file_name() == "LibX.jar")
            .expect("the library")
            .origin = ModOrigin::RequiredDependency;
        store.write_recipe(&recipe).expect("write");

        app.delete_mod_files(
            InstalledHost::Instance(instance),
            &[PathBuf::from("mods/LibX.jar")],
        )
        .expect("a dependency's jar may be deleted");

        assert!(
            store
                .read_recipe()
                .expect("recipe")
                .files
                .iter()
                .any(|file| file.file_name() == "LibX.jar"),
            "the record outlives the file, so the file can come back"
        );
    }

    #[test]
    fn a_batch_disable_turns_dependents_off_before_what_they_require() {
        // Any prefix of the batch has to leave a mod set that still starts.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, instance, _root) = library_and_its_dependents(base.path());

        let applied = app
            .set_mods_enabled(
                InstalledHost::Instance(instance),
                &[
                    (PathBuf::from("mods/LibX.jar"), false),
                    (PathBuf::from("mods/SkyblockBuilder.jar"), false),
                    (PathBuf::from("mods/SkyGUIs.jar"), false),
                ],
            )
            .expect("batch");

        let order: Vec<&str> = applied
            .iter()
            .map(|(path, _)| path.file_name().expect("name").to_str().expect("utf8"))
            .collect();
        assert_eq!(
            order,
            ["SkyGUIs.jar", "SkyblockBuilder.jar", "LibX.jar"],
            "the library goes last, after everything that needs it"
        );
        assert!(
            applied
                .iter()
                .all(|(_, presence)| *presence == ModPresence::Disabled)
        );
    }

    #[tokio::test]
    async fn a_refresh_records_what_the_catalog_says_each_mod_needs() {
        let base = tempfile::tempdir().expect("temp dir");
        let catalog = catalog_answering().await;
        let (mut app, instance, root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");

        assert!(
            recipe_at(&root)
                .files
                .iter()
                .all(|file| file.recorded_relations().is_none()),
            "the instance starts with nothing asked"
        );

        let report = app
            .refresh_mod_dependencies(
                InstalledHost::Instance(instance),
                &OperationReporter::silent("test-refresh"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert_eq!(report.recorded, 2);
        assert_eq!(report.unidentified, 0);
        assert!(report.is_complete());

        let recipe = recipe_at(&root);
        let library = recipe
            .files
            .iter()
            .find(|file| file.file_name() == "LibX.jar")
            .expect("the library");
        let dependents = recipe.required_by(library);
        assert_eq!(dependents.len(), 1);
        assert_eq!(
            dependents[0].logical_path,
            PathBuf::from("mods/SkyblockBuilder.jar"),
            "the reverse index is what a disable warning reads"
        );
        let library = recipe
            .files
            .iter()
            .find(|file| file.logical_path == Path::new("mods/LibX.jar"))
            .expect("the library");
        assert_eq!(
            library.recorded_relations(),
            Some(&[][..]),
            "the catalog answered that it needs nothing, which is not the same as unasked"
        );
    }

    #[tokio::test]
    async fn refreshing_twice_records_nothing_the_second_time() {
        let base = tempfile::tempdir().expect("temp dir");
        let catalog = catalog_answering().await;
        let (mut app, instance, _root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");

        let reporter = OperationReporter::silent("test-refresh");
        let cancel = CancellationToken::new();
        let first = app
            .refresh_mod_dependencies(InstalledHost::Instance(instance), &reporter, &cancel)
            .await
            .expect("first");
        assert_eq!(first.recorded, 2);
        let second = app
            .refresh_mod_dependencies(InstalledHost::Instance(instance), &reporter, &cancel)
            .await
            .expect("second");
        assert_eq!(second.recorded, 0);
        assert_eq!(second.unchanged, 2);
    }

    #[tokio::test]
    async fn a_catalog_that_cannot_answer_leaves_every_mod_unasked() {
        // Recording "needs nothing" because a request failed is the one
        // outcome that turns a missing answer into a confident wrong one.
        let base = tempfile::tempdir().expect("temp dir");
        let unreachable = MockServer::start().await;
        Mock::given(method("GET"))
            .and(route("/versions"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&unreachable)
            .await;
        let (mut app, instance, root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&unreachable.uri())
            .expect("loopback catalog");

        let report = app
            .refresh_mod_dependencies(
                InstalledHost::Instance(instance),
                &OperationReporter::silent("test-refresh"),
                &CancellationToken::new(),
            )
            .await
            .expect("a refresh reports rather than fails");

        assert_eq!(report.recorded, 0);
        assert!(!report.is_complete());
        assert_eq!(report.providers_unavailable, vec![ModProvider::Modrinth]);
        assert!(
            recipe_at(&root)
                .files
                .iter()
                .all(|file| file.recorded_relations().is_none()),
            "an unreachable catalog leaves every mod unasked rather than empty"
        );
    }

    #[tokio::test]
    async fn a_mod_with_no_exact_release_is_counted_and_not_asked_about() {
        let base = tempfile::tempdir().expect("temp dir");
        let catalog = catalog_answering().await;
        let (mut app, instance, root) = instance_with_unasked_mods(base.path());
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");

        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&root));
        let mut recipe = store.read_recipe().expect("recipe");
        let mut project_only = recorded("UtilitiX.jar", "MMMWYsjm", "unused");
        project_only.identity.as_mut().expect("identity").version_id = None;
        recipe.files.push(project_only);
        store.write_recipe(&recipe).expect("write");

        let report = app
            .refresh_mod_dependencies(
                InstalledHost::Instance(instance),
                &OperationReporter::silent("test-refresh"),
                &CancellationToken::new(),
            )
            .await
            .expect("refresh");

        assert_eq!(report.unidentified, 1);
        assert_eq!(
            report.recorded, 2,
            "the two that name a release are recorded"
        );
    }
}
