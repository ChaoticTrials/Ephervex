//! Working out what installing a mod actually involves.
//!
//! # Why this is not a graph walk with a `visited` set
//!
//! The obvious implementation marks each project as it is resolved and never
//! looks at it again. That is wrong here, and wrong in a way that produces
//! exactly the failure this launcher exists to avoid.
//!
//! Consider A, which requires B with no version stated, and C, which requires B
//! pinned to an exact version. If A is expanded first, B is resolved to its
//! newest compatible release and marked visited; when C's requirement arrives,
//! the guard skips it. The install then silently ignores an explicit version
//! choice - the launcher substituting a release nobody asked for, which is the
//! one thing it must never do.
//!
//! So a project is not "visited"; it carries a *constraint*, and the resolver
//! runs to a fixpoint. A constraint can strengthen (nothing stated → pinned)
//! and a project whose constraint strengthened is resolved again. Two different
//! pins on one project do not arbitrate: they fail, naming both requesters.
//!
//! # Determinism
//!
//! Everything is keyed in `BTreeMap`/`BTreeSet`, the next project to resolve is
//! the smallest id among the most informative of those needing work, and
//! version candidates are ordered by publication date and then by id.
//! Reversing any response array changes nothing about the result.
//!
//! # Nothing here writes
//!
//! Resolution produces a [`ModInstallPlan`] and touches no disk. Every
//! refusal - a conflicting pin, an incompatible relation, a required dependency
//! that names no project - happens before an installer is handed anything.

use std::collections::{BTreeMap, BTreeSet};

use ephervex_download::OperationReporter;
use ephervex_model::{ModOrigin, ModProvider, ModRelationKind};
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{Compatibility, InstanceTarget, ModDependency, ModProject, ModVersionInfo};
use crate::provider::ModProviderApi;

/// How many resolution rounds are allowed before the resolver gives up.
///
/// Resolution follows only what the root's current selection reaches, so a
/// project can be resolved again when the release that pinned it is replaced.
/// A graph with no consistent choice of versions can keep replacing releases
/// indefinitely, and this ceiling turns that into a refused install rather than
/// a launcher that never finishes.
const MAX_RESOLUTION_ROUNDS: usize = 512;

/// The name the plan gives the mod the user asked for.
const ROOT: &str = "root";

/// What the caller wants installed.
#[derive(Debug, Clone)]
pub struct InstallSelection {
    /// The project the user chose, by provider id.
    pub project_id: String,
    /// An exact version, when the user picked one from the list. `None` means
    /// "the newest that fits".
    pub version_id: Option<String>,
    /// Optional dependencies the user ticked, by project id. Anything not named
    /// here stays unselected - the product default.
    pub optional: BTreeSet<String>,
}

impl InstallSelection {
    #[must_use]
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            version_id: None,
            optional: BTreeSet::new(),
        }
    }

    /// Pins an exact version. Never substituted for another.
    #[must_use]
    pub fn at_version(mut self, version_id: impl Into<String>) -> Self {
        self.version_id = Some(version_id.into());
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

/// A mod the instance already has, as far as resolution needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledProject {
    pub project_id: String,
    pub version_id: String,
}

/// What resolution does when a project it resolved is already installed at a
/// different version.
///
/// The two answers are opposite and both are right for their own operation.
/// Installing a new mod must not quietly move a mod that is already there - the
/// user asked for one thing and would get two. Changing a mod's version is a
/// request to do exactly that, and refusing it would refuse the operation.
///
/// Making it a parameter rather than a second resolver keeps one implementation
/// of "what does this mod need", which is the part that is hard to get right.
///
/// `Replace` moves the root and nothing else on its own account: an installed
/// dependency that nothing pins stays at its release while that release still
/// fits. Only a pin, or a release that no longer fits, moves one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstalledPolicy {
    /// Refuse to plan anything that would change an installed project.
    #[default]
    Preserve,
    /// Report such a project as a replacement and carry on.
    Replace,
}

/// An installed project this plan would move to a different version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectReplacement {
    pub project_id: String,
    /// The version the instance has now.
    pub from: String,
    /// The version this plan resolved to.
    pub to: String,
}

/// One mod the plan would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMod {
    pub project: ModProject,
    pub version: ModVersionInfo,
    pub origin: ModOrigin,
    /// Which mods asked for this one, sorted. `root` is the user's own choice.
    pub requested_by: Vec<String>,
}

impl PlannedMod {
    /// Client support of the planned release, falling back to its project.
    #[must_use]
    pub fn client_side(&self) -> crate::model::SideSupport {
        self.version.effective_sides(&self.project).0
    }

    /// Server support of the planned release, falling back to its project.
    #[must_use]
    pub fn server_side(&self) -> crate::model::SideSupport {
        self.version.effective_sides(&self.project).1
    }
}

/// An optional dependency the user has not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalCandidate {
    pub project_id: String,
    /// The mod that offers it.
    pub declared_by: String,
    /// The version that would be installed if it were selected, when one fits.
    pub version: Option<ModVersionInfo>,
    pub compatibility: Compatibility,
}

/// A relation naming a bare file rather than a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalDependency {
    pub kind: ModRelationKind,
    pub file_name: String,
    pub declared_by: String,
}

/// A dependency the provider says is shipped inside its dependent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedNote {
    pub project_id: Option<String>,
    pub file_name: Option<String>,
    pub declared_by: String,
}

/// Something worth telling the user that is not a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanWarning {
    /// The project says it does not support the client, and the user asked for
    /// it on a client anyway. Reported rather than refused: they asked by name,
    /// and second-guessing a deliberate reproduction step is not this
    /// launcher's job.
    ClientSideUnsupported { project_id: String },
    /// An `incompatible` relation naming a bare file, which cannot be checked
    /// against the graph.
    UncheckableConflict {
        declared_by: String,
        file_name: String,
    },
    /// The user selected an optional dependency nothing in the graph offers.
    OptionalNotOffered { project_id: String },
}

impl PlanWarning {
    /// The `ephervex-i18n` message id describing this warning.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::ClientSideUnsupported { .. } => "mod-warning-client-side-unsupported",
            Self::UncheckableConflict { .. } => "mod-warning-uncheckable-conflict",
            Self::OptionalNotOffered { .. } => "mod-warning-optional-not-offered",
        }
    }
}

/// Everything installing one mod would involve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModInstallPlan {
    pub provider: ModProvider,
    /// The mod the user asked for.
    pub root: PlannedMod,
    /// Everything else that would be installed, sorted by project id.
    pub dependencies: Vec<PlannedMod>,
    /// Offered but not taken.
    pub optional: Vec<OptionalCandidate>,
    pub external: Vec<ExternalDependency>,
    pub embedded: Vec<EmbeddedNote>,
    /// Planned mods the instance already has at the same version. Nothing is
    /// downloaded for these.
    pub already_installed: Vec<InstalledProject>,
    /// Installed projects this plan moves to a different version. Always empty
    /// under [`InstalledPolicy::Preserve`], which refuses instead.
    pub replacements: Vec<ProjectReplacement>,
    pub warnings: Vec<PlanWarning>,
}

impl ModInstallPlan {
    /// The root plus its dependencies, in installation order.
    #[must_use]
    pub fn all_mods(&self) -> Vec<&PlannedMod> {
        std::iter::once(&self.root)
            .chain(self.dependencies.iter())
            .collect()
    }

    /// The mods that would actually be fetched and written.
    #[must_use]
    pub fn to_install(&self) -> Vec<&PlannedMod> {
        let present: BTreeSet<&str> = self
            .already_installed
            .iter()
            .map(|installed| installed.project_id.as_str())
            .collect();
        self.all_mods()
            .into_iter()
            .filter(|planned| !present.contains(planned.version.project_id.as_str()))
            .collect()
    }

    /// Whether this plan would change anything about the instance.
    #[must_use]
    pub fn changes_anything(&self) -> bool {
        !self.to_install().is_empty()
    }

    /// Every project this plan says the instance should end up with.
    pub fn resolved_projects(&self) -> impl Iterator<Item = &str> {
        self.all_mods()
            .into_iter()
            .map(|planned| planned.version.project_id.as_str())
            .collect::<Vec<_>>()
            .into_iter()
    }
}

/// A relation after normalization: one shape, with the awkward cases named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Normalized {
    /// Resolvable against the provider's catalog.
    Project {
        kind: ModRelationKind,
        project_id: String,
        pinned_version: Option<String>,
    },
    /// Names a file and nothing else. Deliberately *not* turned into a project
    /// identity: there is nothing to derive one from, and inventing one would
    /// make the plan claim something it cannot look up.
    External {
        kind: ModRelationKind,
        file_name: String,
    },
}

/// What one project is currently required to be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Constraint {
    /// The exact version required, if anything requires one.
    pin: Option<String>,
    /// Who pinned it. The conflict message needs both sides, so this is a set
    /// rather than the first one seen.
    pin_sources: BTreeSet<String>,
    requesters: BTreeSet<String>,
    origin: ModOrigin,
}

impl Constraint {
    /// Folds another requirement into this one.
    ///
    /// The merge rules, in one place:
    ///
    /// * nothing stated + nothing stated → unchanged;
    /// * nothing stated + a pin → strengthens to the pin;
    /// * a pin + nothing stated → unchanged, because an explicit choice is not
    ///   weakened by a vague one;
    /// * the same pin twice → unchanged;
    /// * two different pins → a refusal, never an arbitration.
    fn merge(
        &mut self,
        project_id: &str,
        pin: Option<&str>,
        requester: &str,
        origin: ModOrigin,
    ) -> ModPlatformResult<()> {
        self.requesters.insert(requester.to_owned());
        if stronger_origin(origin, self.origin) {
            self.origin = origin;
        }

        let Some(incoming) = pin else {
            return Ok(());
        };
        match self.pin.as_deref() {
            None => {
                self.pin = Some(incoming.to_owned());
                self.pin_sources.insert(requester.to_owned());
            }
            Some(existing) if existing == incoming => {
                self.pin_sources.insert(requester.to_owned());
            }
            Some(existing) => {
                return Err(ModPlatformError::ConflictingPinnedVersions {
                    project_id: project_id.to_owned(),
                    first: existing.to_owned(),
                    first_source: join(&self.pin_sources),
                    second: incoming.to_owned(),
                    second_source: requester.to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// Which of two origins describes a more deliberate choice.
const fn stronger_origin(candidate: ModOrigin, current: ModOrigin) -> bool {
    origin_rank(candidate) > origin_rank(current)
}

const fn origin_rank(origin: ModOrigin) -> u8 {
    match origin {
        // Nothing chose a hand-placed jar through this crate, so it makes the
        // same weakest claim a pack's own declaration does on a path a
        // resolution is deciding between.
        ModOrigin::HandPlaced | ModOrigin::PackDeclared | ModOrigin::PackOverride => 0,
        ModOrigin::RequiredDependency => 1,
        ModOrigin::SelectedOptionalDependency => 2,
        ModOrigin::UserSelected => 3,
    }
}

fn join(values: &BTreeSet<String>) -> String {
    values.iter().cloned().collect::<Vec<_>>().join(", ")
}

/// One resolved project, and the pin it was resolved under.
#[derive(Debug, Clone)]
struct Resolution {
    version: ModVersionInfo,
    under: Option<String>,
}

/// What resolution needs from its caller.
#[derive(Debug)]
pub struct ResolveRequest<'a> {
    pub provider: &'a dyn ModProviderApi,
    pub target: &'a InstanceTarget,
    /// The provider's own name for the instance's loader.
    pub loader_name: &'a str,
    pub selection: &'a InstallSelection,
    /// What the instance already has, so an install can be idempotent rather
    /// than a duplicate.
    pub installed: &'a [InstalledProject],
    /// What to do about an installed project this resolution moves.
    pub installed_policy: InstalledPolicy,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// Works out what installing the selected mod involves.
#[instrument(skip_all, fields(project = %request.selection.project_id))]
pub async fn resolve(request: ResolveRequest<'_>) -> ModPlatformResult<ModInstallPlan> {
    let mut state = Resolver::new(request);
    state.run().await
}

/// Whether an error describes the catalog's content for one project, so that
/// it holds only for as long as the graph keeps asking for that project, as
/// opposed to the provider failing.
const fn is_graph_refusal(error: &ModPlatformError) -> bool {
    matches!(
        error,
        ModPlatformError::IncompatibleSelectedVersion { .. }
            | ModPlatformError::NoCompatibleVersion { .. }
            | ModPlatformError::ProjectUnknown { .. }
            | ModPlatformError::VersionUnknown { .. }
            | ModPlatformError::InconsistentDependency { .. }
            | ModPlatformError::MalformedDependency { .. }
    )
}

struct Resolver<'a> {
    request: ResolveRequest<'a>,
    constraints: BTreeMap<String, Constraint>,
    resolved: BTreeMap<String, Resolution>,
    /// Relations contributed by each resolved `(project, version)`. Keyed by
    /// version so that re-resolving a project retracts exactly what its
    /// previous version said.
    edges: BTreeMap<(String, String), Vec<Normalized>>,
    /// Projects whose version could not be selected or whose release could not
    /// be read under the pin they carried, with that pin. The refusal is final
    /// only once nothing else can change what the project is asked for, so it
    /// is kept rather than returned.
    refused: BTreeMap<String, (Option<String>, ModPlatformError)>,
    /// Projects the current graph pins to two different versions. Reported only
    /// once nothing else is left to resolve, because resolving a project that
    /// has not been explored yet can retract either side.
    conflicts: BTreeMap<String, ModPlatformError>,
    optional: BTreeMap<String, OptionalCandidate>,
    external: Vec<ExternalDependency>,
    embedded: Vec<EmbeddedNote>,
    warnings: Vec<PlanWarning>,
    /// Optional selections that have been honored, so unmatched ones can be
    /// reported.
    honored_optional: BTreeSet<String>,
}

impl<'a> Resolver<'a> {
    fn new(request: ResolveRequest<'a>) -> Self {
        Self {
            request,
            constraints: BTreeMap::new(),
            resolved: BTreeMap::new(),
            edges: BTreeMap::new(),
            refused: BTreeMap::new(),
            conflicts: BTreeMap::new(),
            optional: BTreeMap::new(),
            external: Vec::new(),
            embedded: Vec::new(),
            warnings: Vec::new(),
            honored_optional: BTreeSet::new(),
        }
    }

    async fn run(&mut self) -> ModPlatformResult<ModInstallPlan> {
        let root_id = self.request.selection.project_id.clone();

        for round in 0..=MAX_RESOLUTION_ROUNDS {
            if self.request.cancel.is_cancelled() {
                return Err(ModPlatformError::Canceled);
            }
            if round == MAX_RESOLUTION_ROUNDS {
                return Err(ModPlatformError::ResolutionDidNotConverge { iterations: round });
            }

            self.refold();
            self.refused.retain(|project, (pin, _)| {
                self.constraints
                    .get(project)
                    .is_some_and(|constraint| &constraint.pin == pin)
            });
            let Some(next) = self.next_dirty() else {
                if let Some((_, conflict)) = self.conflicts.pop_first() {
                    return Err(conflict);
                }
                if let Some((_, (_, refusal))) = self.refused.pop_first() {
                    return Err(refusal);
                }
                break;
            };

            let pin = self
                .constraints
                .get(&next)
                .and_then(|constraint| constraint.pin.clone());
            let version = match self.select_version(&next, pin.as_deref()).await {
                Ok(version) => version,
                Err(refusal) if is_graph_refusal(&refusal) => {
                    self.refused.insert(next, (pin, refusal));
                    continue;
                }
                Err(error) => return Err(error),
            };
            debug!(project = %next, version = %version.id, "resolved a project");

            // A project's relations belong to the version that declared them,
            // so re-resolving retracts the old ones wholesale.
            self.edges.retain(|(project, _), _| project != &next);
            let normalized = match self.normalize_all(&version).await {
                Ok(normalized) => normalized,
                Err(refusal) if is_graph_refusal(&refusal) => {
                    // The old release's relations are gone, so it must not stay
                    // resolved without them.
                    self.resolved.remove(&next);
                    self.refused.insert(next, (pin, refusal));
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.edges
                .insert((next.clone(), version.id.clone()), normalized);
            self.resolved.insert(
                next,
                Resolution {
                    version,
                    under: pin,
                },
            );
        }

        self.finish(&root_id).await
    }

    /// Rebuilds the constraint map from the root plus the edges reachable from it.
    ///
    /// Recomputed from scratch rather than patched: a retracted edge must not
    /// be able to leave a constraint behind, and proving that of an incremental
    /// update is harder than doing the work.
    ///
    /// A pin conflict is recorded rather than returned, see `conflicts`.
    fn refold(&mut self) {
        let (mut constraints, mut conflicts) = self.fold(&BTreeSet::new());

        // A pin that only the conflicted project's own release brings in does
        // not outvote one declared independently of it: moving the project
        // retracts the former.
        for project in conflicts.keys().cloned().collect::<Vec<_>>() {
            let (without_own, still_conflicted) = self.fold(&BTreeSet::from([project.clone()]));
            let independent = without_own
                .get(&project)
                .and_then(|constraint| constraint.pin.clone());
            let Some(pin) = independent else { continue };
            if still_conflicted.contains_key(&project)
                || self
                    .resolved
                    .get(&project)
                    .is_some_and(|resolution| resolution.under.as_deref() == Some(pin.as_str()))
            {
                continue;
            }
            if let Some(constraint) = constraints.get_mut(&project) {
                constraint.pin = Some(pin);
                conflicts.remove(&project);
            }
        }

        // Anything no longer reachable stops being resolved, so a stale
        // resolution cannot reappear in the plan.
        self.resolved
            .retain(|project, _| constraints.contains_key(project));
        self.edges
            .retain(|(project, _), _| constraints.contains_key(project));
        self.constraints = constraints;
        self.conflicts = conflicts;
    }

    /// The resolved projects the graph now pins to a version other than the
    /// one they were resolved under, so their current release is about to be
    /// replaced.
    fn superseded_projects(&self) -> BTreeSet<String> {
        self.resolved
            .iter()
            .filter(|(project, resolution)| {
                self.constraints.get(*project).is_some_and(|constraint| {
                    constraint.pin.is_some() && constraint.pin != resolution.under
                })
            })
            .map(|(project, _)| project.clone())
            .collect()
    }

    /// Folds the root and every reachable edge into constraints, leaving out
    /// the edges of the `skipped` projects.
    ///
    /// Pin conflicts are returned beside the constraints rather than aborting
    /// the fold, because the caller decides when they count.
    fn fold(
        &self,
        skipped: &BTreeSet<String>,
    ) -> (
        BTreeMap<String, Constraint>,
        BTreeMap<String, ModPlatformError>,
    ) {
        let selection = self.request.selection;
        let reachable = self.reachable_projects(skipped);
        let mut constraints: BTreeMap<String, Constraint> = BTreeMap::new();
        let mut conflicts = BTreeMap::new();

        let root = constraints.entry(selection.project_id.clone()).or_default();
        if let Err(error) = root.merge(
            &selection.project_id,
            selection.version_id.as_deref(),
            ROOT,
            ModOrigin::UserSelected,
        ) {
            conflicts
                .entry(selection.project_id.clone())
                .or_insert(error);
        }

        for ((source, _), relations) in &self.edges {
            if !reachable.contains(source) || skipped.contains(source) {
                continue;
            }
            for relation in relations {
                let Normalized::Project {
                    kind,
                    project_id,
                    pinned_version,
                } = relation
                else {
                    continue;
                };
                let Some(origin) = self.graph_origin(*kind, project_id) else {
                    continue;
                };
                if let Err(error) = constraints.entry(project_id.clone()).or_default().merge(
                    project_id,
                    pinned_version.as_deref(),
                    source,
                    origin,
                ) {
                    conflicts.entry(project_id.clone()).or_insert(error);
                }
            }
        }
        (constraints, conflicts)
    }

    /// The projects the root's current selection reaches through the stored
    /// edges, following only relations that put something in the graph.
    fn reachable_projects(&self, skipped: &BTreeSet<String>) -> BTreeSet<String> {
        let mut reachable = BTreeSet::from([self.request.selection.project_id.clone()]);
        let mut grew = true;
        while grew {
            grew = false;
            for ((source, _), relations) in &self.edges {
                if !reachable.contains(source) || skipped.contains(source) {
                    continue;
                }
                for relation in relations {
                    let Normalized::Project {
                        kind, project_id, ..
                    } = relation
                    else {
                        continue;
                    };
                    if self.graph_origin(*kind, project_id).is_some() {
                        grew |= reachable.insert(project_id.clone());
                    }
                }
            }
        }
        reachable
    }

    /// The origin a relation gives its target, or `None` when the relation puts
    /// nothing in the graph: optional and not taken, incompatible, or embedded.
    fn graph_origin(&self, kind: ModRelationKind, project_id: &str) -> Option<ModOrigin> {
        match kind {
            ModRelationKind::Required => Some(ModOrigin::RequiredDependency),
            ModRelationKind::Optional if self.request.selection.optional.contains(project_id) => {
                Some(ModOrigin::SelectedOptionalDependency)
            }
            _ => None,
        }
    }

    /// The project to resolve next: the smallest id of the most informative
    /// kind.
    ///
    /// "Needs resolving" is either never resolved, or resolved under a weaker
    /// constraint than it now carries - which is precisely the case a `visited`
    /// set gets wrong. What a superseded project's current release declares is
    /// about to be replaced, so the order follows what survives without it:
    ///
    /// 0. a project pinned by a release that is not itself superseded, since
    ///    that choice is forced;
    /// 1. a project nothing pins, reached the same way, which takes its newest
    ///    release (or, under `Replace`, the one it already has);
    /// 2. a project pinned only through superseded releases;
    /// 3. a project whose pin has gone, which keeps its release in the graph
    ///    until nothing else can still be explored through it;
    /// 4. a project reached only through a superseded release.
    ///
    /// A project whose last attempt was refused, or that the graph pins to two
    /// versions, is left alone until its pin changes.
    fn next_dirty(&self) -> Option<String> {
        let settled = self.fold(&self.superseded_projects()).0;
        self.constraints
            .iter()
            .filter(|(project, _)| {
                !self.refused.contains_key(*project) && !self.conflicts.contains_key(*project)
            })
            .filter_map(|(project, constraint)| {
                let resolution = self.resolved.get(project);
                if resolution.is_some_and(|resolution| resolution.under == constraint.pin) {
                    return None;
                }
                let rank = match (settled.get(project), resolution) {
                    (None, _) => 4,
                    (Some(live), _) if constraint.pin.is_some() && live.pin == constraint.pin => 0,
                    (Some(_), None) if constraint.pin.is_none() => 1,
                    (Some(_), _) if constraint.pin.is_some() => 2,
                    (Some(_), _) => 3,
                };
                Some((rank, project))
            })
            .min()
            .map(|(_, project)| project.clone())
    }

    /// Picks the version one project resolves to.
    async fn select_version(
        &self,
        project_id: &str,
        pin: Option<&str>,
    ) -> ModPlatformResult<ModVersionInfo> {
        let target = self.request.target;

        if let Some(version_id) = pin {
            let version = self.fetch_version(version_id).await?;
            if version.project_id != project_id {
                return Err(ModPlatformError::InconsistentDependency {
                    declared_by: project_id.to_owned(),
                    project_id: project_id.to_owned(),
                    version_project_id: version.project_id,
                });
            }
            // An explicitly chosen version that does not fit is a refusal. The
            // alternative - quietly taking a neighbouring release - is the
            // substitution this launcher exists not to do.
            if !version
                .compatibility(target, self.request.loader_name)
                .is_compatible()
            {
                return Err(ModPlatformError::IncompatibleSelectedVersion {
                    project_id: project_id.to_owned(),
                    version_id: version.id,
                    minecraft: target.minecraft_version.clone(),
                    loader: target.loader,
                });
            }
            return Ok(version);
        }

        let versions = self.versions(project_id).await?;
        if let Some(kept) = self
            .installed_release_to_keep(project_id, &versions)
            .await?
        {
            return Ok(kept);
        }

        newest_compatible(&versions, target, self.request.loader_name).ok_or_else(|| {
            ModPlatformError::NoCompatibleVersion {
                project_id: project_id.to_owned(),
                minecraft: target.minecraft_version.clone(),
                loader: target.loader,
            }
        })
    }

    /// The release a non-root project already has, when nothing asks it to move.
    ///
    /// Under [`InstalledPolicy::Replace`] only the root is being changed on
    /// purpose. An unpinned dependency has no opinion about its release, so
    /// moving it to the newest one would be a side effect nobody asked for.
    ///
    /// A provider's listing can leave out a release that is still installed
    /// (unlisted, or beyond the pages read), so one missing from it is looked up
    /// by id before it is treated as gone.
    async fn installed_release_to_keep(
        &self,
        project_id: &str,
        versions: &[ModVersionInfo],
    ) -> ModPlatformResult<Option<ModVersionInfo>> {
        if self.request.installed_policy != InstalledPolicy::Replace
            || project_id == self.request.selection.project_id
        {
            return Ok(None);
        }
        let Some(installed) = self
            .request
            .installed
            .iter()
            .find(|entry| entry.project_id == project_id)
        else {
            return Ok(None);
        };
        let release = match versions
            .iter()
            .find(|version| version.id == installed.version_id)
        {
            Some(listed) => listed.clone(),
            None => match self.fetch_version(&installed.version_id).await {
                Ok(fetched) if fetched.project_id == project_id => fetched,
                Ok(_) => return Ok(None),
                // The provider describing the release as unusable is an answer
                // about the release, not a failure to ask.
                Err(error)
                    if is_graph_refusal(&error)
                        || matches!(
                            error,
                            ModPlatformError::FileUnavailable { .. }
                                | ModPlatformError::HashesMissing { .. }
                        ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            },
        };
        Ok(release
            .compatibility(self.request.target, self.request.loader_name)
            .is_compatible()
            .then_some(release))
    }

    /// Turns one version's raw relations into a single shape.
    async fn normalize_all(
        &mut self,
        version: &ModVersionInfo,
    ) -> ModPlatformResult<Vec<Normalized>> {
        // Relations that pin a version but name no project need one lookup each
        // to learn the project. Batched, because a mod with several of them
        // would otherwise spend a rate-limit budget on round trips.
        let pinned: Vec<String> = version
            .dependencies
            .iter()
            .filter(|dependency| dependency.version_id.is_some() && dependency.project_id.is_none())
            .filter_map(|dependency| dependency.version_id.clone())
            .collect();
        let looked_up = if pinned.is_empty() {
            Vec::new()
        } else {
            self.request
                .provider
                .versions_by_id(&pinned, self.request.reporter, self.request.cancel)
                .await?
        };

        version
            .dependencies
            .iter()
            .map(|dependency| normalize(dependency, &version.project_id, &looked_up))
            .collect()
    }

    /// A project's candidate versions, limited to releases that project owns.
    ///
    /// An entry owned by another project would put that project's artifact and
    /// dependencies under this project's identity.
    async fn versions(&self, project_id: &str) -> ModPlatformResult<Vec<ModVersionInfo>> {
        let mut versions = self
            .request
            .provider
            .versions(
                project_id,
                self.request.target,
                self.request.reporter,
                self.request.cancel,
            )
            .await?;
        versions.retain(|version| version.project_id == project_id);
        Ok(versions)
    }

    /// The version with exactly this id.
    ///
    /// Matched by id rather than taken by position, so a response carrying a
    /// different release can never stand in for the one that was pinned.
    async fn fetch_version(&self, version_id: &str) -> ModPlatformResult<ModVersionInfo> {
        self.request
            .provider
            .versions_by_id(
                std::slice::from_ref(&version_id.to_owned()),
                self.request.reporter,
                self.request.cancel,
            )
            .await?
            .into_iter()
            .find(|version| version.id == version_id)
            .ok_or_else(|| ModPlatformError::VersionUnknown {
                provider: self.request.provider.provider(),
                version_id: version_id.to_owned(),
            })
    }

    /// Turns the settled graph into a plan, after the checks that need the
    /// whole graph to be settled.
    async fn finish(&mut self, root_id: &str) -> ModPlatformResult<ModInstallPlan> {
        self.collect_relations().await?;
        self.check_conflicts()?;

        for project in &self.request.selection.optional {
            if !self.honored_optional.contains(project) {
                self.warnings.push(PlanWarning::OptionalNotOffered {
                    project_id: project.clone(),
                });
            }
        }

        let projects = self.fetch_projects().await?;
        let mut root = None;
        let mut dependencies = Vec::new();
        for (project_id, resolution) in &self.resolved {
            let project = projects.get(project_id).cloned().ok_or_else(|| {
                ModPlatformError::ProjectUnknown {
                    provider: self.request.provider.provider(),
                    reference: project_id.clone(),
                }
            })?;
            let constraint = self.constraints.get(project_id);
            let planned = PlannedMod {
                project,
                version: resolution.version.clone(),
                origin: constraint.map_or(ModOrigin::RequiredDependency, |it| it.origin),
                requested_by: constraint
                    .map(|it| it.requesters.iter().cloned().collect())
                    .unwrap_or_default(),
            };
            if project_id == root_id {
                if planned.client_side() == crate::model::SideSupport::Unsupported {
                    self.warnings.push(PlanWarning::ClientSideUnsupported {
                        project_id: project_id.clone(),
                    });
                }
                root = Some(planned);
            } else {
                dependencies.push(planned);
            }
        }

        let root = root.ok_or_else(|| ModPlatformError::ProjectUnknown {
            provider: self.request.provider.provider(),
            reference: root_id.to_owned(),
        })?;

        let (already_installed, replacements) = self.already_installed()?;

        Ok(ModInstallPlan {
            provider: self.request.provider.provider(),
            root,
            dependencies,
            optional: self.optional.values().cloned().collect(),
            external: std::mem::take(&mut self.external),
            embedded: std::mem::take(&mut self.embedded),
            already_installed,
            replacements,
            warnings: std::mem::take(&mut self.warnings),
        })
    }

    /// Sorts the relations that are not graph edges into the plan's lists.
    async fn collect_relations(&mut self) -> ModPlatformResult<()> {
        let edges: Vec<(String, Vec<Normalized>)> = self
            .edges
            .iter()
            .map(|((source, _), relations)| (source.clone(), relations.clone()))
            .collect();

        for (source, relations) in edges {
            for relation in relations {
                match relation {
                    Normalized::Project {
                        kind: ModRelationKind::Optional,
                        project_id,
                        pinned_version,
                    } => {
                        if self.request.selection.optional.contains(&project_id) {
                            self.honored_optional.insert(project_id);
                            continue;
                        }
                        self.record_optional(&source, project_id, pinned_version)
                            .await;
                    }
                    Normalized::Project {
                        kind: ModRelationKind::Embedded,
                        project_id,
                        ..
                    } => self.embedded.push(EmbeddedNote {
                        project_id: Some(project_id),
                        file_name: None,
                        declared_by: source.clone(),
                    }),
                    Normalized::External { kind, file_name } => {
                        self.record_external(&source, kind, file_name)?;
                    }
                    // Required relations are edges, and incompatible ones are
                    // checked against the settled graph rather than listed.
                    Normalized::Project { .. } => {}
                }
            }
        }
        Ok(())
    }

    /// Describes an optional dependency the user has not taken.
    ///
    /// Its version is resolved so the offer can say what would be installed and
    /// whether it would even fit - but a failure to resolve is not fatal: an
    /// optional extra that has no compatible release is a fact to display, not
    /// a reason to refuse the install the user actually asked for.
    async fn record_optional(
        &mut self,
        source: &str,
        project_id: String,
        pinned_version: Option<String>,
    ) {
        if self.optional.contains_key(&project_id) {
            return;
        }
        let resolved = match &pinned_version {
            Some(version_id) => self.fetch_version(version_id).await.ok(),
            None => self.versions(&project_id).await.ok().and_then(|versions| {
                newest_compatible(&versions, self.request.target, self.request.loader_name)
            }),
        };
        let compatibility = resolved.as_ref().map_or_else(
            || {
                Compatibility::Incompatible(vec![
                    crate::model::IncompatibilityReason::MinecraftVersion {
                        required: self.request.target.minecraft_version.clone(),
                        supported: Vec::new(),
                    },
                ])
            },
            |version| version.compatibility(self.request.target, self.request.loader_name),
        );

        self.optional.insert(
            project_id.clone(),
            OptionalCandidate {
                project_id,
                declared_by: source.to_owned(),
                version: resolved.filter(|_| compatibility.is_compatible()),
                compatibility,
            },
        );
    }

    /// Handles a relation that names a file and nothing else.
    fn record_external(
        &mut self,
        source: &str,
        kind: ModRelationKind,
        file_name: String,
    ) -> ModPlatformResult<()> {
        match kind {
            // Installing an incomplete graph would produce an instance that
            // crashes on launch for a reason the launcher already knew.
            ModRelationKind::Required => {
                return Err(ModPlatformError::UnresolvableRequiredDependency {
                    declared_by: source.to_owned(),
                    file_name,
                });
            }
            ModRelationKind::Incompatible => {
                self.warnings.push(PlanWarning::UncheckableConflict {
                    declared_by: source.to_owned(),
                    file_name: file_name.clone(),
                });
            }
            ModRelationKind::Embedded => self.embedded.push(EmbeddedNote {
                project_id: None,
                file_name: Some(file_name.clone()),
                declared_by: source.to_owned(),
            }),
            ModRelationKind::Optional => {}
        }
        self.external.push(ExternalDependency {
            kind,
            file_name,
            declared_by: source.to_owned(),
        });
        Ok(())
    }

    /// Refuses a graph that contains a mod another mod declares incompatible.
    ///
    /// Checked once the graph has settled, against both the resolved set and
    /// what the instance already has. Checking earlier could clear a conflict
    /// that a later re-resolution reintroduces.
    fn check_conflicts(&self) -> ModPlatformResult<()> {
        for ((source, _), relations) in &self.edges {
            for relation in relations {
                let Normalized::Project {
                    kind: ModRelationKind::Incompatible,
                    project_id,
                    pinned_version,
                } = relation
                else {
                    continue;
                };
                if self.is_present(project_id, pinned_version.as_deref()) {
                    return Err(ModPlatformError::IncompatibleDependency {
                        declared_by: source.clone(),
                        conflicting: project_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Whether the finished plan leaves `project_id` on the instance, at
    /// `version` when one is given and at any release otherwise.
    ///
    /// A release the plan resolved supersedes the installed one, so only it
    /// counts for a project the plan touches.
    fn is_present(&self, project_id: &str, version: Option<&str>) -> bool {
        let matches = |present: &str| version.is_none_or(|pinned| pinned == present);
        match self.resolved.get(project_id) {
            Some(resolution) => matches(&resolution.version.id),
            None => self
                .request
                .installed
                .iter()
                .any(|entry| entry.project_id == project_id && matches(&entry.version_id)),
        }
    }

    /// Splits the resolved set into "already there" and "would be installed",
    /// refusing a version change.
    fn already_installed(
        &self,
    ) -> ModPlatformResult<(Vec<InstalledProject>, Vec<ProjectReplacement>)> {
        let mut present = Vec::new();
        let mut replacements = Vec::new();
        for (project_id, resolution) in &self.resolved {
            let Some(installed) = self
                .request
                .installed
                .iter()
                .find(|entry| &entry.project_id == project_id)
            else {
                continue;
            };
            if installed.version_id == resolution.version.id {
                present.push(installed.clone());
                continue;
            }
            match self.request.installed_policy {
                // The caller asked to change this mod's version, so moving it
                // is the operation rather than a surprise. It is still reported
                // separately, because the caller has to retire the old file.
                InstalledPolicy::Replace => replacements.push(ProjectReplacement {
                    project_id: project_id.clone(),
                    from: installed.version_id.clone(),
                    to: resolution.version.id.clone(),
                }),
                // An install must not move a mod that is already there.
                // Refusing here means the instance is never touched.
                InstalledPolicy::Preserve => {
                    return Err(ModPlatformError::ProjectAlreadyInstalled {
                        provider: self.request.provider.provider(),
                        project_id: project_id.clone(),
                        installed: installed.version_id.clone(),
                        requested: resolution.version.id.clone(),
                    });
                }
            }
        }
        Ok((present, replacements))
    }

    async fn fetch_projects(&self) -> ModPlatformResult<BTreeMap<String, ModProject>> {
        let ids: Vec<String> = self.resolved.keys().cloned().collect();
        Ok(self
            .request
            .provider
            .projects_by_id(&ids, self.request.reporter, self.request.cancel)
            .await?
            .into_iter()
            .map(|project| (project.id.clone(), project))
            .collect())
    }
}

/// Turns one raw relation into a single shape.
///
/// Split out and total: every combination of the three optional identity fields
/// has an answer here, so no caller has to invent one.
fn normalize(
    dependency: &ModDependency,
    declared_by: &str,
    looked_up: &[ModVersionInfo],
) -> ModPlatformResult<Normalized> {
    match (
        dependency.project_id.as_deref(),
        dependency.version_id.as_deref(),
    ) {
        // A pinned version and no project: the project is *derived* from the
        // version rather than guessed.
        (None, Some(version_id)) => {
            let version = looked_up
                .iter()
                .find(|candidate| candidate.id == version_id)
                .ok_or_else(|| ModPlatformError::VersionUnknown {
                    provider: ModProvider::Modrinth,
                    version_id: version_id.to_owned(),
                })?;
            Ok(Normalized::Project {
                kind: dependency.kind,
                project_id: version.project_id.clone(),
                pinned_version: Some(version_id.to_owned()),
            })
        }
        (Some(project_id), None) => Ok(Normalized::Project {
            kind: dependency.kind,
            project_id: project_id.to_owned(),
            pinned_version: None,
        }),
        // Both stated: they have to agree. A relation whose version belongs to
        // another project does not describe what it claims to.
        (Some(project_id), Some(version_id)) => {
            if let Some(version) = looked_up
                .iter()
                .find(|candidate| candidate.id == version_id)
            {
                if version.project_id != project_id {
                    return Err(ModPlatformError::InconsistentDependency {
                        declared_by: declared_by.to_owned(),
                        project_id: project_id.to_owned(),
                        version_project_id: version.project_id.clone(),
                    });
                }
            }
            Ok(Normalized::Project {
                kind: dependency.kind,
                project_id: project_id.to_owned(),
                pinned_version: Some(version_id.to_owned()),
            })
        }
        (None, None) => match dependency.file_name.as_deref() {
            Some(file_name) => Ok(Normalized::External {
                kind: dependency.kind,
                file_name: file_name.to_owned(),
            }),
            None => Err(ModPlatformError::MalformedDependency {
                declared_by: declared_by.to_owned(),
            }),
        },
    }
}

/// The newest version that fits, by publication date then by id.
///
/// A total order rather than "whatever the provider listed first": two versions
/// published in the same second must still resolve the same way on every run.
#[must_use]
pub fn newest_compatible(
    versions: &[ModVersionInfo],
    target: &InstanceTarget,
    loader_name: &str,
) -> Option<ModVersionInfo> {
    versions
        .iter()
        .filter(|version| version.compatibility(target, loader_name).is_compatible())
        .max_by(|left, right| {
            left.published_at
                .cmp(&right.published_at)
                .then_with(|| left.id.cmp(&right.id))
        })
        .cloned()
}

/// Every version, newest first, annotated against the instance.
#[must_use]
pub fn annotate(
    versions: &[ModVersionInfo],
    target: &InstanceTarget,
    loader_name: &str,
) -> Vec<crate::model::AnnotatedVersion> {
    let mut annotated: Vec<crate::model::AnnotatedVersion> = versions
        .iter()
        .map(|version| crate::model::AnnotatedVersion {
            compatibility: version.compatibility(target, loader_name),
            version: version.clone(),
        })
        .collect();
    annotated.sort_by(|left, right| {
        right
            .version
            .published_at
            .cmp(&left.version.published_at)
            .then_with(|| right.version.id.cmp(&left.version.id))
    });
    annotated
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use async_trait::async_trait;
    use ephervex_model::LoaderKind;

    use super::*;
    use crate::model::{ModSearchQuery, ModSearchResults};
    use crate::test_support::{at, compatible, needs, needs_file, pins, project, version};

    /// A catalog held in memory, so a resolver test states the graph it is
    /// about rather than a fixture directory.
    #[derive(Debug, Default)]
    struct Catalog {
        versions: BTreeMap<String, Vec<ModVersionInfo>>,
        /// What `versions_by_id` answers with whatever was asked, for a
        /// provider that returns something other than the requested ids.
        by_id_response: Option<Vec<ModVersionInfo>>,
    }

    impl Catalog {
        fn with(mut self, project_id: &str, versions: Vec<ModVersionInfo>) -> Self {
            self.versions.insert(project_id.to_owned(), versions);
            self
        }

        fn answering_by_id_with(mut self, response: Vec<ModVersionInfo>) -> Self {
            self.by_id_response = Some(response);
            self
        }

        /// The same catalog with every array reversed.
        ///
        /// Response order is not part of any provider's contract, so a result
        /// that changes under this is a result that depends on luck.
        fn reversed(&self) -> Self {
            Self {
                versions: self
                    .versions
                    .iter()
                    .map(|(project, versions)| {
                        let mut versions = versions.clone();
                        versions.reverse();
                        for version in &mut versions {
                            version.dependencies.reverse();
                        }
                        (project.clone(), versions)
                    })
                    .collect(),
                by_id_response: self.by_id_response.clone().map(|mut response| {
                    response.reverse();
                    response
                }),
            }
        }
    }

    #[async_trait]
    impl ModProviderApi for Catalog {
        fn provider(&self) -> ModProvider {
            ModProvider::Modrinth
        }

        fn loader_name(&self, loader: LoaderKind) -> Option<&'static str> {
            match loader {
                LoaderKind::Vanilla => None,
                LoaderKind::Forge => Some("forge"),
                LoaderKind::NeoForge => Some("neoforge"),
            }
        }

        async fn search(
            &self,
            _target: &InstanceTarget,
            _query: &ModSearchQuery,
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<ModSearchResults> {
            Err(ModPlatformError::Canceled)
        }

        async fn project(
            &self,
            reference: &str,
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<ModProject> {
            Ok(project(reference))
        }

        async fn versions(
            &self,
            project_id: &str,
            _target: &InstanceTarget,
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<Vec<ModVersionInfo>> {
            self.versions
                .get(project_id)
                .cloned()
                .ok_or_else(|| ModPlatformError::ProjectUnknown {
                    provider: ModProvider::Modrinth,
                    reference: project_id.to_owned(),
                })
        }

        async fn projects_by_id(
            &self,
            ids: &[String],
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<Vec<ModProject>> {
            Ok(ids.iter().map(|id| project(id)).collect())
        }

        async fn versions_by_id(
            &self,
            ids: &[String],
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<Vec<ModVersionInfo>> {
            if let Some(response) = &self.by_id_response {
                return Ok(response.clone());
            }
            Ok(self
                .versions
                .values()
                .flatten()
                .filter(|version| ids.contains(&version.id))
                .cloned()
                .collect())
        }
    }

    fn target() -> InstanceTarget {
        InstanceTarget::new("1.20.1", LoaderKind::NeoForge)
    }

    async fn plan_with(
        catalog: &Catalog,
        selection: InstallSelection,
        installed: &[InstalledProject],
    ) -> ModPlatformResult<ModInstallPlan> {
        plan_under(catalog, selection, installed, InstalledPolicy::Preserve).await
    }

    async fn plan_under(
        catalog: &Catalog,
        selection: InstallSelection,
        installed: &[InstalledProject],
        installed_policy: InstalledPolicy,
    ) -> ModPlatformResult<ModInstallPlan> {
        resolve(ResolveRequest {
            provider: catalog,
            target: &target(),
            loader_name: "neoforge",
            selection: &selection,
            installed,
            installed_policy,
            reporter: &OperationReporter::silent("resolving-mods"),
            cancel: &CancellationToken::new(),
        })
        .await
    }

    async fn plan(catalog: &Catalog, selection: InstallSelection) -> ModInstallPlan {
        plan_with(catalog, selection, &[]).await.expect("a plan")
    }

    fn ids<'a>(mods: impl IntoIterator<Item = &'a PlannedMod>) -> Vec<String> {
        mods.into_iter()
            .map(|planned| planned.version.project_id.clone())
            .collect()
    }

    /// Everything the resolver *decided*, as opposed to the upstream arrays it
    /// copies through. A version's own `dependencies` list is verbatim provider
    /// data and echoes whatever order the response had; comparing it would test
    /// the fixture rather than the resolver.
    fn decisions(plan: &ModInstallPlan) -> Vec<(String, String, ModOrigin, Vec<String>)> {
        plan.all_mods()
            .into_iter()
            .map(|planned| {
                (
                    planned.version.project_id.clone(),
                    planned.version.id.clone(),
                    planned.origin,
                    planned.requested_by.clone(),
                )
            })
            .collect()
    }

    /// The `(project, version)` pairs a plan ends up with, sorted.
    fn chosen_versions(plan: &ModInstallPlan) -> Vec<(String, String)> {
        let mut chosen: Vec<(String, String)> = plan
            .all_mods()
            .into_iter()
            .map(|planned| {
                (
                    planned.version.project_id.clone(),
                    planned.version.id.clone(),
                )
            })
            .collect();
        chosen.sort();
        chosen
    }

    fn pairs<const N: usize>(expected: [(&str, &str); N]) -> Vec<(String, String)> {
        expected
            .map(|(project, version)| (project.to_owned(), version.to_owned()))
            .to_vec()
    }

    // -- version selection -------------------------------------------------

    #[tokio::test]
    async fn the_newest_compatible_version_is_chosen_by_default() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![
                at(compatible("old", "ROOT"), 0),
                at(compatible("new", "ROOT"), 60),
                // Newer still, and for the wrong release: recency does not
                // outrank fitting the instance.
                at(version("newest", "ROOT", &["1.21.1"], &["neoforge"]), 120),
            ],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(plan.root.version.id, "new");
        assert_eq!(plan.root.origin, ModOrigin::UserSelected);
    }

    #[tokio::test]
    async fn an_explicitly_chosen_older_version_is_installed_as_chosen() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![
                at(compatible("old", "ROOT"), 0),
                at(compatible("new", "ROOT"), 60),
            ],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT").at_version("old")).await;
        assert_eq!(
            plan.root.version.id, "old",
            "an explicit choice is never quietly upgraded"
        );
    }

    #[tokio::test]
    async fn a_pinned_version_is_found_by_id_whatever_order_the_response_has() {
        let pinned = at(compatible("pinned", "ROOT"), 0);
        let other = at(compatible("other", "ROOT"), 60);

        for response in [
            vec![other.clone(), pinned.clone()],
            vec![pinned.clone(), other.clone()],
        ] {
            let catalog = Catalog::default()
                .with("ROOT", vec![pinned.clone(), other.clone()])
                .answering_by_id_with(response);
            let plan = plan(&catalog, InstallSelection::new("ROOT").at_version("pinned")).await;
            assert_eq!(plan.root.version.id, "pinned");
        }
    }

    #[tokio::test]
    async fn a_pinned_version_the_provider_does_not_return_is_unknown_not_substituted() {
        let catalog = Catalog::default()
            .with("ROOT", vec![compatible("pinned", "ROOT")])
            .answering_by_id_with(vec![compatible("other", "ROOT")]);

        let error = plan_with(
            &catalog,
            InstallSelection::new("ROOT").at_version("pinned"),
            &[],
        )
        .await
        .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::VersionUnknown { ref version_id, .. } if version_id == "pinned"
        ));
    }

    #[tokio::test]
    async fn a_dependency_pin_is_not_satisfied_by_a_different_release() {
        let mut root = compatible("v1", "ROOT");
        root.dependencies.push(ModDependency {
            kind: ModRelationKind::Required,
            project_id: Some("LIB".to_owned()),
            version_id: Some("pinned".to_owned()),
            file_name: None,
        });
        let catalog = Catalog::default()
            .with("ROOT", vec![root])
            .with("LIB", vec![compatible("other", "LIB")])
            .answering_by_id_with(vec![compatible("other", "LIB")]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::VersionUnknown { ref version_id, .. } if version_id == "pinned"
        ));
    }

    #[tokio::test]
    async fn a_listing_entry_owned_by_another_project_is_never_selected() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![
                at(compatible("own", "ROOT"), 0),
                // Newer and compatible, but it belongs to a different project.
                at(compatible("foreign", "OTHER"), 60),
            ],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(plan.root.version.id, "own");
        assert_eq!(plan.root.version.project_id, "ROOT");
    }

    #[tokio::test]
    async fn a_listing_with_only_foreign_entries_has_no_compatible_version() {
        let catalog = Catalog::default().with("ROOT", vec![compatible("foreign", "OTHER")]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::NoCompatibleVersion { ref project_id, .. } if project_id == "ROOT"
        ));
    }

    #[tokio::test]
    async fn an_explicitly_chosen_incompatible_version_is_refused_not_substituted() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![
                at(compatible("fits", "ROOT"), 0),
                at(version("wrong", "ROOT", &["1.21.1"], &["neoforge"]), 60),
            ],
        );

        let error = plan_with(
            &catalog,
            InstallSelection::new("ROOT").at_version("wrong"),
            &[],
        )
        .await
        .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleSelectedVersion { ref version_id, .. }
                if version_id == "wrong"
        ));
    }

    #[tokio::test]
    async fn a_project_with_nothing_compatible_is_refused_rather_than_approximated() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![version("only", "ROOT", &["1.21.1"], &["fabric"])],
        );
        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::NoCompatibleVersion { ref project_id, .. } if project_id == "ROOT"
        ));
    }

    #[tokio::test]
    async fn nothing_in_a_plan_changes_the_instances_minecraft_version_or_loader() {
        // The resolver borrows the target and has no way to write it; this
        // pins the property that the type system already gives us, so a future
        // change that adds a setter fails here rather than in production.
        let catalog = Catalog::default().with("ROOT", vec![compatible("v1", "ROOT")]);
        let target = target();
        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;

        assert_eq!(target.minecraft_version, "1.20.1");
        assert_eq!(target.loader, LoaderKind::NeoForge);
        assert!(
            plan.root
                .version
                .game_versions
                .contains(&"1.20.1".to_owned())
        );
        assert!(plan.root.version.loaders.contains(&"neoforge".to_owned()));
    }

    // -- dependencies ------------------------------------------------------

    #[tokio::test]
    async fn a_required_dependency_is_resolved_recursively() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Required,
                    "MID",
                )],
            )
            .with(
                "MID",
                vec![needs(
                    compatible("v1", "MID"),
                    ModRelationKind::Required,
                    "LEAF",
                )],
            )
            .with("LEAF", vec![compatible("v1", "LEAF")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(ids(&plan.dependencies), vec!["LEAF", "MID"]);
        assert!(
            plan.dependencies
                .iter()
                .all(|planned| planned.origin == ModOrigin::RequiredDependency)
        );
    }

    #[tokio::test]
    async fn an_optional_dependency_is_offered_and_not_installed() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Optional,
                    "EXTRA",
                )],
            )
            .with("EXTRA", vec![compatible("v1", "EXTRA")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(
            plan.dependencies.is_empty(),
            "unselected optionals install nothing"
        );
        assert_eq!(plan.optional.len(), 1);
        assert_eq!(plan.optional[0].project_id, "EXTRA");
        assert!(plan.optional[0].compatibility.is_compatible());
        assert!(
            plan.optional[0].version.is_some(),
            "the offer says what would be installed if it were taken"
        );
    }

    #[tokio::test]
    async fn a_selected_optional_dependency_is_installed_with_its_own_requirements() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Optional,
                    "EXTRA",
                )],
            )
            .with(
                "EXTRA",
                vec![needs(
                    compatible("v1", "EXTRA"),
                    ModRelationKind::Required,
                    "LEAF",
                )],
            )
            .with("LEAF", vec![compatible("v1", "LEAF")]);

        let plan = plan(
            &catalog,
            InstallSelection::new("ROOT").selecting_optional(["EXTRA"]),
        )
        .await;
        assert_eq!(ids(&plan.dependencies), vec!["EXTRA", "LEAF"]);
        let extra = plan
            .dependencies
            .iter()
            .find(|planned| planned.version.project_id == "EXTRA")
            .expect("present");
        assert_eq!(extra.origin, ModOrigin::SelectedOptionalDependency);
        assert!(
            plan.optional.is_empty(),
            "a taken offer is no longer an offer"
        );
    }

    #[tokio::test]
    async fn selecting_an_optional_nothing_offers_is_reported_rather_than_silently_ignored() {
        let catalog = Catalog::default().with("ROOT", vec![compatible("v1", "ROOT")]);
        let plan = plan(
            &catalog,
            InstallSelection::new("ROOT").selecting_optional(["NOPE"]),
        )
        .await;
        assert!(matches!(
            plan.warnings.as_slice(),
            [PlanWarning::OptionalNotOffered { project_id }] if project_id == "NOPE"
        ));
    }

    #[tokio::test]
    async fn a_directly_incompatible_dependency_blocks_the_installation() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "MID"),
                    ModRelationKind::Incompatible,
                    "MID",
                )],
            )
            .with("MID", vec![compatible("v1", "MID")]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. } if conflicting == "MID"
        ));
    }

    #[tokio::test]
    async fn an_incompatible_relation_deeper_in_the_graph_blocks_it_too() {
        // ROOT needs MID, MID needs LEAF, and LEAF says it cannot coexist with
        // MID. Nothing at the root says anything is wrong.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Required,
                    "MID",
                )],
            )
            .with(
                "MID",
                vec![needs(
                    compatible("v1", "MID"),
                    ModRelationKind::Required,
                    "LEAF",
                )],
            )
            .with(
                "LEAF",
                vec![needs(
                    compatible("v1", "LEAF"),
                    ModRelationKind::Incompatible,
                    "MID",
                )],
            );

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. } if conflicting == "MID"
        ));
    }

    #[tokio::test]
    async fn a_mod_incompatible_with_something_already_installed_is_blocked() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![needs(
                compatible("v1", "ROOT"),
                ModRelationKind::Incompatible,
                "PRESENT",
            )],
        );

        let installed = [InstalledProject {
            project_id: "PRESENT".to_owned(),
            version_id: "whatever".to_owned(),
        }];
        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. }
                if conflicting == "PRESENT"
        ));
    }

    /// ROOT requires `B` and declares an incompatibility with `B`'s release `b1`.
    fn declares_b1_incompatible(b_versions: Vec<ModVersionInfo>) -> Catalog {
        Catalog::default()
            .with(
                "ROOT",
                vec![pins(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "B"),
                    ModRelationKind::Incompatible,
                    "b1",
                )],
            )
            .with("B", b_versions)
    }

    #[tokio::test]
    async fn a_pinned_incompatibility_does_not_refuse_another_release_in_the_plan() {
        let catalog = declares_b1_incompatible(vec![
            at(compatible("b1", "B"), 0),
            at(compatible("b2", "B"), 10),
        ]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(plan.dependencies[0].version.id, "b2");
    }

    #[tokio::test]
    async fn a_pinned_incompatibility_refuses_the_pinned_release_in_the_plan() {
        let catalog = declares_b1_incompatible(vec![at(compatible("b1", "B"), 0)]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. } if conflicting == "B"
        ));
    }

    #[tokio::test]
    async fn a_pinned_incompatibility_does_not_refuse_another_installed_release() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![pins(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Incompatible,
                    "b1",
                )],
            )
            .with("B", vec![compatible("b1", "B"), compatible("b2", "B")]);

        let installed = [InstalledProject {
            project_id: "B".to_owned(),
            version_id: "b2".to_owned(),
        }];
        plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect("an unrelated release of the project is fine");
    }

    #[tokio::test]
    async fn a_pinned_incompatibility_refuses_the_pinned_installed_release() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![pins(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Incompatible,
                    "b1",
                )],
            )
            .with("B", vec![compatible("b1", "B"), compatible("b2", "B")]);

        let installed = [InstalledProject {
            project_id: "B".to_owned(),
            version_id: "b1".to_owned(),
        }];
        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. } if conflicting == "B"
        ));
    }

    #[tokio::test]
    async fn a_dependency_cycle_terminates() {
        let catalog = Catalog::default()
            .with(
                "A",
                vec![needs(compatible("v1", "A"), ModRelationKind::Required, "B")],
            )
            .with(
                "B",
                vec![needs(compatible("v1", "B"), ModRelationKind::Required, "A")],
            );

        let plan = plan(&catalog, InstallSelection::new("A")).await;
        assert_eq!(plan.root.version.project_id, "A");
        assert_eq!(ids(&plan.dependencies), vec!["B"]);
    }

    #[tokio::test]
    async fn a_diamond_materializes_one_copy_of_the_shared_dependency() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "LEFT"),
                    ModRelationKind::Required,
                    "RIGHT",
                )],
            )
            .with(
                "LEFT",
                vec![needs(
                    compatible("v1", "LEFT"),
                    ModRelationKind::Required,
                    "SHARED",
                )],
            )
            .with(
                "RIGHT",
                vec![needs(
                    compatible("v1", "RIGHT"),
                    ModRelationKind::Required,
                    "SHARED",
                )],
            )
            .with("SHARED", vec![compatible("v1", "SHARED")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(ids(&plan.dependencies), vec!["LEFT", "RIGHT", "SHARED"]);

        let shared = plan
            .dependencies
            .iter()
            .find(|planned| planned.version.project_id == "SHARED")
            .expect("present");
        assert_eq!(
            shared.requested_by,
            vec!["LEFT".to_owned(), "RIGHT".to_owned()],
            "both requesters are recorded even though one file is installed"
        );
    }

    // -- constraint merging ------------------------------------------------

    #[tokio::test]
    async fn a_pin_discovered_after_a_project_was_already_resolved_wins() {
        // The case a one-shot `visited` set gets wrong. AAA is reached first
        // with no version stated and resolves to its newest; ZZZ then pins it
        // to an older one. Resolution has to go back and honor the pin.
        //
        // The ids are chosen so AAA is resolved before ZZZ is even seen.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "AAA"),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(compatible("old", "AAA"), 0),
                    at(compatible("new", "AAA"), 60),
                ],
            )
            .with(
                "ZZZ",
                vec![pins(
                    compatible("v1", "ZZZ"),
                    ModRelationKind::Required,
                    "old",
                )],
            );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        let shared = plan
            .dependencies
            .iter()
            .find(|planned| planned.version.project_id == "AAA")
            .expect("present");
        assert_eq!(
            shared.version.id, "old",
            "an exact requirement discovered later must not be discarded"
        );
    }

    /// ROOT requires AAA and ZZZ. The newest AAA requires BBB, which requires
    /// CCC, but ZZZ pins an older AAA with no dependencies. AAA resolves first
    /// to its newest, so the superseded branch has already been folded when the
    /// pin arrives.
    fn superseded_branch_catalog(ccc: Vec<ModVersionInfo>) -> Catalog {
        Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "AAA"),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(compatible("old", "AAA"), 0),
                    at(
                        needs(compatible("new", "AAA"), ModRelationKind::Required, "BBB"),
                        60,
                    ),
                ],
            )
            .with(
                "BBB",
                vec![needs(
                    compatible("v1", "BBB"),
                    ModRelationKind::Required,
                    "CCC",
                )],
            )
            .with("CCC", ccc)
            .with(
                "ZZZ",
                vec![pins(
                    compatible("v1", "ZZZ"),
                    ModRelationKind::Required,
                    "old",
                )],
            )
    }

    #[tokio::test]
    async fn a_superseded_branch_leaves_none_of_its_transitive_mods_in_the_plan() {
        let catalog = superseded_branch_catalog(vec![compatible("v1", "CCC")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        let mut projects = ids(plan.all_mods());
        projects.sort();
        assert_eq!(projects, ["AAA", "ROOT", "ZZZ"]);
    }

    #[tokio::test]
    async fn a_conflict_declared_by_an_unreachable_mod_does_not_refuse_the_plan() {
        let catalog = superseded_branch_catalog(vec![needs(
            compatible("v1", "CCC"),
            ModRelationKind::Incompatible,
            "ROOT",
        )]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(!ids(plan.all_mods()).contains(&"CCC".to_owned()));
    }

    #[tokio::test]
    async fn a_pin_declared_by_a_superseded_branch_does_not_conflict_with_a_live_pin() {
        // BBB is only reachable through the superseded AAA, so its pin on CCC
        // must not clash with the different pin ZZZ states.
        let catalog = superseded_branch_catalog(vec![
            at(compatible("ccc-old", "CCC"), 0),
            at(compatible("ccc-new", "CCC"), 60),
        ])
        .with(
            "BBB",
            vec![pins(
                compatible("v1", "BBB"),
                ModRelationKind::Required,
                "ccc-new",
            )],
        )
        .with(
            "ZZZ",
            vec![pins(
                pins(compatible("v1", "ZZZ"), ModRelationKind::Required, "old"),
                ModRelationKind::Required,
                "ccc-old",
            )],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        let chosen = chosen_versions(&plan);
        assert!(chosen.contains(&("CCC".to_owned(), "ccc-old".to_owned())));
        assert!(!chosen.iter().any(|(project, _)| project == "BBB"));
    }

    #[tokio::test]
    async fn a_pin_held_by_a_dependency_of_the_project_it_pins_keeps_that_dependency() {
        // Both releases of SSS need NNN, and NNN pins the older one. Resolving
        // SSS to its newest makes it stale, but the pin that says so comes from
        // a dependency of that very release, so retracting the release's edges
        // must not also retract the reason to resolve it again.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Required,
                    "SSS",
                )],
            )
            .with(
                "SSS",
                vec![
                    at(
                        needs(compatible("s-old", "SSS"), ModRelationKind::Required, "NNN"),
                        0,
                    ),
                    at(
                        needs(compatible("s-new", "SSS"), ModRelationKind::Required, "NNN"),
                        60,
                    ),
                ],
            )
            .with(
                "NNN",
                vec![pins(
                    compatible("n1", "NNN"),
                    ModRelationKind::Required,
                    "s-old",
                )],
            );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            chosen_versions(&plan),
            pairs([("NNN", "n1"), ("ROOT", "v1"), ("SSS", "s-old")])
        );
    }

    #[tokio::test]
    async fn a_stale_project_is_resolved_again_before_those_pinned_through_it() {
        // DDD pins BBB through PPP's newest release, and ZZZ pins PPP to its
        // older one. Retracting PPP's edges hides DDD's pin on BBB, which must
        // not send BBB back to its newest release ahead of PPP.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(
                        needs(compatible("v1", "ROOT"), ModRelationKind::Required, "BBB"),
                        ModRelationKind::Required,
                        "PPP",
                    ),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "BBB",
                vec![
                    at(compatible("b-old", "BBB"), 0),
                    at(compatible("b-new", "BBB"), 60),
                ],
            )
            .with(
                "PPP",
                vec![
                    at(
                        needs(compatible("p-old", "PPP"), ModRelationKind::Required, "DDD"),
                        0,
                    ),
                    at(
                        needs(compatible("p-new", "PPP"), ModRelationKind::Required, "DDD"),
                        60,
                    ),
                ],
            )
            .with(
                "DDD",
                vec![pins(
                    compatible("d1", "DDD"),
                    ModRelationKind::Required,
                    "b-old",
                )],
            )
            .with(
                "ZZZ",
                vec![pins(
                    compatible("z1", "ZZZ"),
                    ModRelationKind::Required,
                    "p-old",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("BBB", "b-old"),
                    ("DDD", "d1"),
                    ("PPP", "p-old"),
                    ("ROOT", "v1"),
                    ("ZZZ", "z1"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_pin_that_only_a_superseded_release_declares_does_not_hold_its_target() {
        // KKK's newest release needs BBB, and BBB pins KKK's older one, which
        // ZZZ pins too. Once KKK is back on its older release BBB is gone, and
        // ZZZ is what keeps KKK there. ZZZ sorts after KKK, so it must be
        // resolved before KKK is moved again.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "KKK"),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "KKK",
                vec![
                    at(compatible("k-old", "KKK"), 0),
                    at(
                        needs(compatible("k-new", "KKK"), ModRelationKind::Required, "BBB"),
                        60,
                    ),
                ],
            )
            .with(
                "BBB",
                vec![pins(
                    compatible("b1", "BBB"),
                    ModRelationKind::Required,
                    "k-old",
                )],
            )
            .with(
                "ZZZ",
                vec![pins(
                    compatible("z1", "ZZZ"),
                    ModRelationKind::Required,
                    "k-old",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("KKK", "k-old"), ("ROOT", "v1"), ("ZZZ", "z1")])
            );
        }
    }

    #[tokio::test]
    async fn a_project_whose_pin_vanishes_with_its_own_release_still_reaches_its_new_dependencies()
    {
        // ZZZ's older release needs BBB, and its newer one pins a version of
        // QQQ that pins the older ZZZ. Settling on the older release removes
        // the only source of that pin, but the older release's own
        // dependencies still belong in the plan.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "ZZZ",
                vec![
                    at(
                        needs(compatible("z-old", "ZZZ"), ModRelationKind::Required, "BBB"),
                        0,
                    ),
                    at(
                        pins(compatible("z-new", "ZZZ"), ModRelationKind::Required, "q1"),
                        60,
                    ),
                ],
            )
            .with(
                "QQQ",
                vec![pins(
                    compatible("q1", "QQQ"),
                    ModRelationKind::Required,
                    "z-old",
                )],
            )
            .with(
                "BBB",
                vec![pins(
                    compatible("b1", "BBB"),
                    ModRelationKind::Required,
                    "q1",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("BBB", "b1"),
                    ("QQQ", "q1"),
                    ("ROOT", "v1"),
                    ("ZZZ", "z-old"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_pin_declared_only_behind_another_waiting_project_is_not_acted_on_first() {
        // MMM's older release does not fit the instance. PPP's newest release
        // pins it, but AAA pins PPP to its older release, so that pin is about
        // to disappear. PPP has to move first.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "MMM"),
                    ModRelationKind::Required,
                    "PPP",
                )],
            )
            .with(
                "MMM",
                vec![
                    at(version("m-old", "MMM", &["1.19.2"], &["neoforge"]), 0),
                    at(compatible("m-new", "MMM"), 60),
                ],
            )
            .with(
                "PPP",
                vec![
                    at(
                        needs(compatible("p-old", "PPP"), ModRelationKind::Required, "AAA"),
                        0,
                    ),
                    at(
                        pins(
                            needs(compatible("p-new", "PPP"), ModRelationKind::Required, "AAA"),
                            ModRelationKind::Required,
                            "m-old",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "AAA",
                vec![pins(
                    compatible("a1", "AAA"),
                    ModRelationKind::Required,
                    "p-old",
                )],
            );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            chosen_versions(&plan),
            pairs([
                ("AAA", "a1"),
                ("MMM", "m-new"),
                ("PPP", "p-old"),
                ("ROOT", "v1"),
            ])
        );
    }

    #[tokio::test]
    async fn a_pin_from_a_waiting_root_release_is_not_acted_on_before_it_is_replaced() {
        // ROOT's newest release pins an unusable ZZZ, but BBB pins ROOT's
        // older one, so that pin is about to disappear. ZZZ must not be
        // resolved under it.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![
                    at(
                        pins(
                            compatible("r-old", "ROOT"),
                            ModRelationKind::Required,
                            "b-old",
                        ),
                        0,
                    ),
                    at(
                        pins(
                            pins(
                                compatible("r-new", "ROOT"),
                                ModRelationKind::Required,
                                "z-new",
                            ),
                            ModRelationKind::Required,
                            "b-old",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "BBB",
                vec![pins(
                    compatible("b-old", "BBB"),
                    ModRelationKind::Required,
                    "r-old",
                )],
            )
            .with(
                "ZZZ",
                vec![
                    at(compatible("z-old", "ZZZ"), 0),
                    at(version("z-new", "ZZZ", &["1.19.2"], &["neoforge"]), 60),
                ],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("BBB", "b-old"), ("ROOT", "r-old")])
            );
        }
    }

    #[tokio::test]
    async fn a_project_whose_pin_vanishes_is_resolved_after_one_that_still_has_a_pin() {
        // ROOT's older release needs ZZZ and its newer one needs BBB. BBB's
        // older release pins ROOT's older one, and ZZZ pins BBB's older one.
        // Once BBB settles on its older release its own pin on ZZZ is gone
        // and ROOT's pin is what keeps the graph together.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![
                    at(
                        needs(
                            compatible("r-old", "ROOT"),
                            ModRelationKind::Required,
                            "ZZZ",
                        ),
                        0,
                    ),
                    at(
                        needs(
                            compatible("r-new", "ROOT"),
                            ModRelationKind::Required,
                            "BBB",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "BBB",
                vec![
                    at(
                        pins(
                            compatible("b-old", "BBB"),
                            ModRelationKind::Required,
                            "r-old",
                        ),
                        0,
                    ),
                    at(
                        pins(
                            compatible("b-new", "BBB"),
                            ModRelationKind::Required,
                            "z-old",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "ZZZ",
                vec![pins(
                    compatible("z-old", "ZZZ"),
                    ModRelationKind::Required,
                    "b-old",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("BBB", "b-old"), ("ROOT", "r-old"), ("ZZZ", "z-old")])
            );
        }
    }

    #[tokio::test]
    async fn a_forced_pin_is_followed_before_an_unexplored_project_adds_a_conflicting_one() {
        // MMM pins KKK's older release, which pins ZZZ's older release. ZZZ's
        // newest release would pin KKK's newest, so ZZZ must not be explored
        // until KKK has moved.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(
                        needs(compatible("r1", "ROOT"), ModRelationKind::Required, "KKK"),
                        ModRelationKind::Required,
                        "MMM",
                    ),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "KKK",
                vec![
                    at(
                        pins(
                            compatible("k-old", "KKK"),
                            ModRelationKind::Required,
                            "z-old",
                        ),
                        0,
                    ),
                    at(compatible("k-new", "KKK"), 60),
                ],
            )
            .with(
                "MMM",
                vec![pins(
                    compatible("m1", "MMM"),
                    ModRelationKind::Required,
                    "k-old",
                )],
            )
            .with(
                "ZZZ",
                vec![
                    at(compatible("z-old", "ZZZ"), 0),
                    at(
                        pins(
                            compatible("z-new", "ZZZ"),
                            ModRelationKind::Required,
                            "k-new",
                        ),
                        60,
                    ),
                ],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("KKK", "k-old"),
                    ("MMM", "m1"),
                    ("ROOT", "r1"),
                    ("ZZZ", "z-old"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_conflict_inside_a_branch_only_a_superseded_release_reaches_is_never_reported() {
        // AAA's newest release needs ZCC, whose only release pins two versions
        // of MMM. YYY's newest release pins AAA's older one, so that branch is
        // dropped and its conflict never applies.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("r1", "ROOT"), ModRelationKind::Required, "AAA"),
                    ModRelationKind::Required,
                    "YYY",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(compatible("a-old", "AAA"), 0),
                    at(
                        needs(compatible("a-new", "AAA"), ModRelationKind::Required, "ZCC"),
                        60,
                    ),
                ],
            )
            .with(
                "ZCC",
                vec![pins(
                    pins(compatible("z1", "ZCC"), ModRelationKind::Required, "m1"),
                    ModRelationKind::Required,
                    "m2",
                )],
            )
            .with(
                "MMM",
                vec![
                    at(compatible("m1", "MMM"), 0),
                    at(
                        pins(compatible("m2", "MMM"), ModRelationKind::Required, "y-free"),
                        60,
                    ),
                ],
            )
            .with(
                "YYY",
                vec![
                    at(compatible("y-free", "YYY"), 0),
                    at(
                        pins(
                            compatible("y-pin", "YYY"),
                            ModRelationKind::Required,
                            "a-old",
                        ),
                        60,
                    ),
                ],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("AAA", "a-old"), ("ROOT", "r1"), ("YYY", "y-pin")])
            );
        }
    }

    #[tokio::test]
    async fn a_dependency_that_cannot_be_read_inside_a_superseded_branch_does_not_refuse_the_plan()
    {
        // BBB's newest release needs a project whose release names something
        // unusable. YYY pins BBB's older release, so that branch is dropped,
        // whichever of the two projects sorts first.
        let unreadable = |what: &str, project: &str| match what {
            "a project nobody lists" => {
                needs(compatible("z1", project), ModRelationKind::Required, "GONE")
            }
            "a version nobody lists" => {
                pins(compatible("z1", project), ModRelationKind::Required, "gone")
            }
            _ => {
                let mut release = compatible("z1", project);
                release.dependencies.push(crate::model::ModDependency {
                    kind: ModRelationKind::Required,
                    project_id: None,
                    version_id: None,
                    file_name: None,
                });
                release
            }
        };

        for what in [
            "a project nobody lists",
            "a version nobody lists",
            "a relation naming nothing",
        ] {
            for branch in ["ACC", "ZCC"] {
                let catalog = Catalog::default()
                    .with(
                        "ROOT",
                        vec![needs(
                            needs(compatible("r1", "ROOT"), ModRelationKind::Required, "BBB"),
                            ModRelationKind::Required,
                            "YYY",
                        )],
                    )
                    .with(
                        "BBB",
                        vec![
                            at(compatible("b-old", "BBB"), 0),
                            at(
                                needs(
                                    compatible("b-new", "BBB"),
                                    ModRelationKind::Required,
                                    branch,
                                ),
                                60,
                            ),
                        ],
                    )
                    .with(branch, vec![unreadable(what, branch)])
                    .with(
                        "YYY",
                        vec![pins(
                            compatible("y1", "YYY"),
                            ModRelationKind::Required,
                            "b-old",
                        )],
                    );

                for catalog in [catalog.reversed(), catalog] {
                    let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
                    assert_eq!(
                        chosen_versions(&plan),
                        pairs([("BBB", "b-old"), ("ROOT", "r1"), ("YYY", "y1")]),
                        "{what} behind {branch}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn a_pin_conflict_waits_for_projects_that_can_still_retract_a_side() {
        // ZZZ's release pins QQQ's newest release, and BBB's newest release
        // pins an older one. KKK pins BBB's older release, which declares no
        // pin, but KKK is only explored after BBB. The conflict must not be
        // reported on sight.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![pins(
                    needs(compatible("r1", "ROOT"), ModRelationKind::Required, "KKK"),
                    ModRelationKind::Required,
                    "z1",
                )],
            )
            .with(
                "ZZZ",
                vec![needs(
                    pins(compatible("z1", "ZZZ"), ModRelationKind::Required, "q-new"),
                    ModRelationKind::Required,
                    "BBB",
                )],
            )
            .with(
                "BBB",
                vec![
                    at(compatible("b-old", "BBB"), 0),
                    at(
                        pins(
                            compatible("b-new", "BBB"),
                            ModRelationKind::Required,
                            "q-old",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "KKK",
                vec![pins(
                    compatible("k1", "KKK"),
                    ModRelationKind::Required,
                    "b-old",
                )],
            )
            .with(
                "QQQ",
                vec![
                    at(compatible("q-old", "QQQ"), 0),
                    at(compatible("q-new", "QQQ"), 60),
                ],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("BBB", "b-old"),
                    ("KKK", "k1"),
                    ("QQQ", "q-new"),
                    ("ROOT", "r1"),
                    ("ZZZ", "z1"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_pin_only_a_projects_own_release_brings_in_does_not_outvote_an_independent_pin() {
        // BBB's newest release needs AAA, which pins BBB's newest release, but
        // KKK pins BBB's older one. Moving BBB drops AAA and with it the pin
        // that was holding BBB in place.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("r1", "ROOT"), ModRelationKind::Required, "BBB"),
                    ModRelationKind::Required,
                    "KKK",
                )],
            )
            .with(
                "BBB",
                vec![
                    at(compatible("b-old", "BBB"), 0),
                    at(
                        needs(compatible("b-new", "BBB"), ModRelationKind::Required, "AAA"),
                        60,
                    ),
                ],
            )
            .with(
                "AAA",
                vec![pins(
                    compatible("a1", "AAA"),
                    ModRelationKind::Required,
                    "b-new",
                )],
            )
            .with(
                "KKK",
                vec![pins(
                    compatible("k1", "KKK"),
                    ModRelationKind::Required,
                    "b-old",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("BBB", "b-old"), ("KKK", "k1"), ("ROOT", "r1")])
            );
        }
    }

    #[tokio::test]
    async fn a_superseded_pin_that_sorts_first_does_not_decide_a_later_projects_version() {
        // AAA's newest release pins an unusable PPP, but BBB pins AAA's older
        // release and the usable PPP. The superseded pin is seen first and
        // must not be acted on.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("r1", "ROOT"), ModRelationKind::Required, "AAA"),
                    ModRelationKind::Required,
                    "BBB",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(compatible("a-old", "AAA"), 0),
                    at(
                        pins(
                            compatible("a-new", "AAA"),
                            ModRelationKind::Required,
                            "p-bad",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "BBB",
                vec![pins(
                    pins(compatible("b1", "BBB"), ModRelationKind::Required, "a-old"),
                    ModRelationKind::Required,
                    "p-good",
                )],
            )
            .with(
                "PPP",
                vec![
                    at(compatible("p-good", "PPP"), 0),
                    at(version("p-bad", "PPP", &["1.19.2"], &["neoforge"]), 60),
                ],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("AAA", "a-old"),
                    ("BBB", "b1"),
                    ("PPP", "p-good"),
                    ("ROOT", "r1"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_pin_declared_through_a_waiting_project_is_not_acted_on_first() {
        // AAA's newest release needs SSS, whose newest pins an unusable XXX,
        // but QQQ pins AAA's older release. SSS is only reached through AAA,
        // so it must not be resolved, and XXX must not be pinned, before AAA
        // moves.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("r1", "ROOT"), ModRelationKind::Required, "AAA"),
                    ModRelationKind::Required,
                    "QQQ",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(
                        pins(
                            compatible("a-old", "AAA"),
                            ModRelationKind::Required,
                            "s-old",
                        ),
                        0,
                    ),
                    at(
                        needs(compatible("a-new", "AAA"), ModRelationKind::Required, "SSS"),
                        60,
                    ),
                ],
            )
            .with(
                "SSS",
                vec![
                    at(compatible("s-old", "SSS"), 0),
                    at(
                        pins(
                            compatible("s-new", "SSS"),
                            ModRelationKind::Required,
                            "x-old",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "XXX",
                vec![
                    at(version("x-old", "XXX", &["1.19.2"], &["neoforge"]), 0),
                    at(compatible("x-new", "XXX"), 60),
                ],
            )
            .with(
                "QQQ",
                vec![pins(
                    compatible("q1", "QQQ"),
                    ModRelationKind::Required,
                    "a-old",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("AAA", "a-old"),
                    ("QQQ", "q1"),
                    ("ROOT", "r1"),
                    ("SSS", "s-old"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_requirement_only_a_waiting_release_declares_is_not_resolved_first() {
        // ROOT's newest release needs a project with no usable version, but
        // MMM pins ROOT's older one, which does not.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![
                    at(
                        pins(compatible("r-old", "ROOT"), ModRelationKind::Required, "m1"),
                        0,
                    ),
                    at(
                        needs(
                            pins(compatible("r-new", "ROOT"), ModRelationKind::Required, "m1"),
                            ModRelationKind::Required,
                            "ZZZ",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "MMM",
                vec![pins(
                    compatible("m1", "MMM"),
                    ModRelationKind::Required,
                    "r-old",
                )],
            )
            .with(
                "ZZZ",
                vec![version("z1", "ZZZ", &["1.19.2"], &["neoforge"])],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("MMM", "m1"), ("ROOT", "r-old")])
            );
        }
    }

    #[tokio::test]
    async fn a_pin_conflict_inside_a_waiting_branch_does_not_depend_on_relation_order() {
        // ZCC pins two releases of MMM, one unusable, but it is only reached
        // through BBB's newest release, which YYY's pin replaces.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("r1", "ROOT"), ModRelationKind::Required, "BBB"),
                    ModRelationKind::Required,
                    "YYY",
                )],
            )
            .with(
                "BBB",
                vec![
                    at(compatible("b-old", "BBB"), 0),
                    at(
                        needs(compatible("b-new", "BBB"), ModRelationKind::Required, "ZCC"),
                        60,
                    ),
                ],
            )
            .with(
                "ZCC",
                vec![pins(
                    pins(compatible("z1", "ZCC"), ModRelationKind::Required, "m-good"),
                    ModRelationKind::Required,
                    "m-bad",
                )],
            )
            .with(
                "MMM",
                vec![
                    at(version("m-bad", "MMM", &["1.19.2"], &["neoforge"]), 0),
                    at(compatible("m-good", "MMM"), 60),
                ],
            )
            .with(
                "YYY",
                vec![pins(
                    compatible("y1", "YYY"),
                    ModRelationKind::Required,
                    "b-old",
                )],
            );

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([("BBB", "b-old"), ("ROOT", "r1"), ("YYY", "y1")])
            );
        }
    }

    #[tokio::test]
    async fn a_pin_is_not_weakened_by_a_later_unpinned_requirement() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    pins(compatible("v1", "ROOT"), ModRelationKind::Required, "old"),
                    ModRelationKind::Required,
                    "ZZZ",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(compatible("old", "AAA"), 0),
                    at(compatible("new", "AAA"), 60),
                ],
            )
            .with(
                "ZZZ",
                vec![needs(
                    compatible("v1", "ZZZ"),
                    ModRelationKind::Required,
                    "AAA",
                )],
            );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        let shared = plan
            .dependencies
            .iter()
            .find(|planned| planned.version.project_id == "AAA")
            .expect("present");
        assert_eq!(
            shared.version.id, "old",
            "a vague requirement does not overrule an exact one"
        );
    }

    #[tokio::test]
    async fn two_different_pins_on_one_project_fail_and_name_both_requesters() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "AAA"),
                    ModRelationKind::Required,
                    "BBB",
                )],
            )
            .with(
                "AAA",
                vec![pins(
                    compatible("v1", "AAA"),
                    ModRelationKind::Required,
                    "old",
                )],
            )
            .with(
                "BBB",
                vec![pins(
                    compatible("v1", "BBB"),
                    ModRelationKind::Required,
                    "new",
                )],
            )
            .with(
                "SHARED",
                vec![
                    at(compatible("old", "SHARED"), 0),
                    at(compatible("new", "SHARED"), 60),
                ],
            );

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse rather than arbitrate");
        let ModPlatformError::ConflictingPinnedVersions {
            first_source,
            second_source,
            ..
        } = &error
        else {
            panic!("expected a pin conflict, got {error:?}");
        };
        assert!(
            [first_source.as_str(), second_source.as_str()].contains(&"AAA"),
            "the message names who asked: {error}"
        );
        assert!([first_source.as_str(), second_source.as_str()].contains(&"BBB"));
    }

    // -- determinism -------------------------------------------------------

    #[tokio::test]
    async fn the_same_graph_resolves_identically_whichever_order_it_arrives_in() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("v1", "ROOT"), ModRelationKind::Required, "LEFT"),
                    ModRelationKind::Required,
                    "RIGHT",
                )],
            )
            .with(
                "LEFT",
                vec![needs(
                    compatible("v1", "LEFT"),
                    ModRelationKind::Required,
                    "SHARED",
                )],
            )
            .with(
                "RIGHT",
                vec![needs(
                    compatible("v1", "RIGHT"),
                    ModRelationKind::Required,
                    "SHARED",
                )],
            )
            .with(
                "SHARED",
                vec![
                    at(compatible("a", "SHARED"), 0),
                    at(compatible("b", "SHARED"), 60),
                ],
            );

        let forward = plan(&catalog, InstallSelection::new("ROOT")).await;
        let backward = plan(&catalog.reversed(), InstallSelection::new("ROOT")).await;

        assert_eq!(ids(&forward.dependencies), ids(&backward.dependencies));
        assert_eq!(forward.root.version.id, backward.root.version.id);
        // Every decision, not just the order of the list: which version each
        // project resolved to, why it is there, and who asked for it.
        assert_eq!(decisions(&forward), decisions(&backward));
        assert_eq!(forward.optional, backward.optional);
        assert_eq!(forward.external, backward.external);
        assert_eq!(forward.embedded, backward.embedded);
        assert_eq!(forward.warnings, backward.warnings);
        assert_eq!(forward.already_installed, backward.already_installed);
    }

    #[tokio::test]
    async fn two_versions_published_at_the_same_moment_still_resolve_one_way() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![
                at(compatible("aaa", "ROOT"), 0),
                at(compatible("bbb", "ROOT"), 0),
            ],
        );
        let forward = plan(&catalog, InstallSelection::new("ROOT")).await;
        let backward = plan(&catalog.reversed(), InstallSelection::new("ROOT")).await;
        assert_eq!(forward.root.version.id, "bbb", "the id breaks the tie");
        assert_eq!(forward.root.version.id, backward.root.version.id);
    }

    // -- dependency normalization ------------------------------------------

    #[tokio::test]
    async fn a_dependency_that_only_pins_a_version_derives_its_project_from_it() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![pins(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Required,
                    "lib-v1",
                )],
            )
            .with("LIB", vec![compatible("lib-v1", "LIB")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(ids(&plan.dependencies), vec!["LIB"]);
        assert_eq!(plan.dependencies[0].version.id, "lib-v1");
    }

    #[tokio::test]
    async fn a_dependency_naming_a_project_and_a_version_that_disagree_is_refused() {
        let mut root = compatible("v1", "ROOT");
        root.dependencies.push(ModDependency {
            kind: ModRelationKind::Required,
            project_id: Some("LIB".to_owned()),
            version_id: Some("other-v1".to_owned()),
            file_name: None,
        });
        let catalog = Catalog::default()
            .with("ROOT", vec![root])
            .with("LIB", vec![compatible("lib-v1", "LIB")])
            .with("OTHER", vec![compatible("other-v1", "OTHER")]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::InconsistentDependency { .. },
        ));
    }

    #[tokio::test]
    async fn a_required_dependency_naming_only_a_file_fails_actionably() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![needs_file(
                compatible("v1", "ROOT"),
                ModRelationKind::Required,
                "some-library.jar",
            )],
        );

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::UnresolvableRequiredDependency { ref file_name, ref declared_by }
                if file_name == "some-library.jar" && declared_by == "ROOT"
        ));
    }

    #[tokio::test]
    async fn an_optional_dependency_naming_only_a_file_is_reported_not_fetched() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![needs_file(
                compatible("v1", "ROOT"),
                ModRelationKind::Optional,
                "nice-to-have.jar",
            )],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(plan.dependencies.is_empty());
        assert!(plan.optional.is_empty(), "there is nothing to select");
        assert_eq!(plan.external.len(), 1);
        assert_eq!(plan.external[0].file_name, "nice-to-have.jar");
    }

    #[tokio::test]
    async fn an_incompatible_relation_naming_only_a_file_is_flagged_as_uncheckable() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![needs_file(
                compatible("v1", "ROOT"),
                ModRelationKind::Incompatible,
                "conflicts-with-this.jar",
            )],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(matches!(
            plan.warnings.as_slice(),
            [PlanWarning::UncheckableConflict { file_name, .. }]
                if file_name == "conflicts-with-this.jar"
        ));
    }

    #[tokio::test]
    async fn a_release_that_cannot_be_read_does_not_strand_the_one_it_replaced() {
        // BBB's newest release pins an AAA release whose relations are
        // malformed, but YYY pins BBB's older one, so that pin goes away. AAA
        // must still bring its own dependency.
        let mut unreadable = compatible("a-bad", "AAA");
        unreadable.dependencies.push(ModDependency {
            kind: ModRelationKind::Required,
            project_id: None,
            version_id: None,
            file_name: None,
        });
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(
                        needs(compatible("r1", "ROOT"), ModRelationKind::Required, "AAA"),
                        ModRelationKind::Required,
                        "BBB",
                    ),
                    ModRelationKind::Required,
                    "YYY",
                )],
            )
            .with(
                "AAA",
                vec![
                    at(unreadable, 0),
                    at(
                        needs(compatible("a-new", "AAA"), ModRelationKind::Required, "DEP"),
                        60,
                    ),
                ],
            )
            .with(
                "BBB",
                vec![
                    at(compatible("b-old", "BBB"), 0),
                    at(
                        pins(
                            compatible("b-new", "BBB"),
                            ModRelationKind::Required,
                            "a-bad",
                        ),
                        60,
                    ),
                ],
            )
            .with(
                "YYY",
                vec![pins(
                    compatible("y1", "YYY"),
                    ModRelationKind::Required,
                    "b-old",
                )],
            )
            .with("DEP", vec![compatible("d1", "DEP")]);

        for catalog in [catalog.reversed(), catalog] {
            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                chosen_versions(&plan),
                pairs([
                    ("AAA", "a-new"),
                    ("BBB", "b-old"),
                    ("DEP", "d1"),
                    ("ROOT", "r1"),
                    ("YYY", "y1"),
                ])
            );
        }
    }

    #[tokio::test]
    async fn a_dependency_identifying_nothing_is_refused() {
        let mut root = compatible("v1", "ROOT");
        root.dependencies.push(ModDependency {
            kind: ModRelationKind::Required,
            project_id: None,
            version_id: None,
            file_name: None,
        });
        let catalog = Catalog::default().with("ROOT", vec![root]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::MalformedDependency { .. }
        ));
    }

    #[tokio::test]
    async fn an_embedded_dependency_is_noted_and_never_installed() {
        // Jar-in-jar carries no guarantee about the version range a third mod
        // needs, so it is reported rather than treated as satisfied.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Embedded,
                    "SHADED",
                )],
            )
            .with("SHADED", vec![compatible("v1", "SHADED")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(
            plan.dependencies.is_empty(),
            "an embedded mod is already inside"
        );
        assert_eq!(plan.embedded.len(), 1);
        assert_eq!(plan.embedded[0].project_id.as_deref(), Some("SHADED"));
    }

    #[tokio::test]
    async fn an_embedded_relation_does_not_satisfy_another_mods_requirement() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(
                        compatible("v1", "ROOT"),
                        ModRelationKind::Embedded,
                        "SHADED",
                    ),
                    ModRelationKind::Required,
                    "OTHER",
                )],
            )
            .with(
                "OTHER",
                vec![needs(
                    compatible("v1", "OTHER"),
                    ModRelationKind::Required,
                    "SHADED",
                )],
            )
            .with("SHADED", vec![compatible("v1", "SHADED")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(
            ids(&plan.dependencies).contains(&"SHADED".to_owned()),
            "a real requirement is installed even though something embeds it"
        );
    }

    // -- already installed -------------------------------------------------

    #[tokio::test]
    async fn a_project_already_installed_at_the_same_version_is_reported_not_reinstalled() {
        let catalog = Catalog::default().with("ROOT", vec![compatible("v1", "ROOT")]);
        let installed = [InstalledProject {
            project_id: "ROOT".to_owned(),
            version_id: "v1".to_owned(),
        }];

        let plan = plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect("a plan");
        assert_eq!(plan.already_installed.len(), 1);
        assert!(
            plan.to_install().is_empty(),
            "nothing is fetched for a mod that is already there"
        );
    }

    #[tokio::test]
    async fn installing_a_different_version_of_an_installed_project_is_refused() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![
                at(compatible("old", "ROOT"), 0),
                at(compatible("new", "ROOT"), 60),
            ],
        );
        let installed = [InstalledProject {
            project_id: "ROOT".to_owned(),
            version_id: "old".to_owned(),
        }];

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect_err("changing an installed mod's version is a mod update");
        assert!(matches!(
            error,
            ModPlatformError::ProjectAlreadyInstalled { ref installed, ref requested, .. }
                if installed == "old" && requested == "new"
        ));
    }

    #[tokio::test]
    async fn a_dependency_resolving_past_an_installed_version_refuses_the_whole_install() {
        // The user asked about ROOT and said nothing about LIB. Quietly moving
        // LIB to another version is exactly the surprise this refuses.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Required,
                    "LIB",
                )],
            )
            .with(
                "LIB",
                vec![
                    at(compatible("old", "LIB"), 0),
                    at(compatible("new", "LIB"), 60),
                ],
            );
        let installed = [InstalledProject {
            project_id: "LIB".to_owned(),
            version_id: "old".to_owned(),
        }];

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::ProjectAlreadyInstalled { ref project_id, .. } if project_id == "LIB"
        ));
    }

    // -- resolving a change rather than an install -------------------------

    fn installed(project: &str, version: &str) -> InstalledProject {
        InstalledProject {
            project_id: project.to_owned(),
            version_id: version.to_owned(),
        }
    }

    /// One mod at two releases, the newer one needing a library.
    fn upgradeable() -> Catalog {
        Catalog::default()
            .with(
                "ROOTPROJ",
                vec![
                    at(compatible("ROOTV1", "ROOTPROJ"), 0),
                    at(
                        needs(
                            compatible("ROOTV2", "ROOTPROJ"),
                            ModRelationKind::Required,
                            "LIBPROJ",
                        ),
                        10,
                    ),
                ],
            )
            .with("LIBPROJ", vec![at(compatible("LIBV1", "LIBPROJ"), 0)])
    }

    #[tokio::test]
    async fn an_install_still_refuses_to_move_a_mod_that_is_already_there() {
        let error = plan_with(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[installed("ROOTPROJ", "ROOTV1")],
        )
        .await
        .expect_err("an install must not change an installed mod");
        assert!(matches!(
            error,
            ModPlatformError::ProjectAlreadyInstalled { .. }
        ));
    }

    #[tokio::test]
    async fn a_change_reports_the_move_it_was_asked_to_make() {
        let plan = plan_under(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(
            plan.replacements,
            vec![ProjectReplacement {
                project_id: "ROOTPROJ".to_owned(),
                from: "ROOTV1".to_owned(),
                to: "ROOTV2".to_owned(),
            }]
        );
        assert_eq!(plan.root.version.id, "ROOTV2");
        assert!(
            plan.already_installed.is_empty(),
            "a mod that moved is not a mod that was left alone"
        );
    }

    #[tokio::test]
    async fn an_upgrade_pulls_in_what_the_new_release_newly_requires() {
        let plan = plan_under(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(ids(&plan.dependencies), vec!["LIBPROJ".to_owned()]);
        assert_eq!(
            plan.dependencies[0].origin,
            ModOrigin::RequiredDependency,
            "a dependency an upgrade dragged in is a dependency"
        );
    }

    #[tokio::test]
    async fn a_dependency_already_installed_at_the_resolved_version_is_reused() {
        let plan = plan_under(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[
                installed("ROOTPROJ", "ROOTV1"),
                installed("LIBPROJ", "LIBV1"),
            ],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(
            plan.already_installed,
            vec![installed("LIBPROJ", "LIBV1")],
            "the library is already exactly right"
        );
        assert_eq!(
            ids(plan.to_install()),
            vec!["ROOTPROJ".to_owned()],
            "so only the mod that actually moved is fetched"
        );
    }

    /// A root at two releases and a library at two, where only the newer root
    /// release needs the library.
    fn root_needing_a_library_with_a_newer_release() -> Catalog {
        Catalog::default()
            .with(
                "ROOTPROJ",
                vec![
                    at(compatible("ROOTV1", "ROOTPROJ"), 0),
                    at(
                        needs(
                            compatible("ROOTV2", "ROOTPROJ"),
                            ModRelationKind::Required,
                            "LIBPROJ",
                        ),
                        10,
                    ),
                ],
            )
            .with(
                "LIBPROJ",
                vec![
                    at(compatible("LIBV1", "LIBPROJ"), 0),
                    at(compatible("LIBV2", "LIBPROJ"), 20),
                ],
            )
    }

    #[tokio::test]
    async fn an_unpinned_installed_dependency_stays_at_its_release_when_the_root_moves() {
        let plan = plan_under(
            &root_needing_a_library_with_a_newer_release(),
            InstallSelection::new("ROOTPROJ"),
            &[
                installed("ROOTPROJ", "ROOTV1"),
                installed("LIBPROJ", "LIBV1"),
            ],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(plan.root.version.id, "ROOTV2");
        assert_eq!(
            chosen_versions(&plan),
            pairs([("LIBPROJ", "LIBV1"), ("ROOTPROJ", "ROOTV2")])
        );
        assert_eq!(
            plan.replacements
                .iter()
                .map(|replacement| replacement.project_id.as_str())
                .collect::<Vec<_>>(),
            vec!["ROOTPROJ"],
            "only the mod that was asked to move is reported as moving"
        );
        assert_eq!(plan.already_installed, vec![installed("LIBPROJ", "LIBV1")]);
        assert_eq!(ids(plan.to_install()), vec!["ROOTPROJ".to_owned()]);
    }

    #[tokio::test]
    async fn an_installed_dependency_that_no_longer_fits_moves_to_one_that_does() {
        let catalog = Catalog::default()
            .with(
                "ROOTPROJ",
                vec![at(
                    needs(
                        compatible("ROOTV2", "ROOTPROJ"),
                        ModRelationKind::Required,
                        "LIBPROJ",
                    ),
                    10,
                )],
            )
            .with(
                "LIBPROJ",
                vec![
                    at(version("LIBV1", "LIBPROJ", &["1.21.4"], &["neoforge"]), 0),
                    at(compatible("LIBV2", "LIBPROJ"), 20),
                ],
            );

        let plan = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ"),
            &[installed("LIBPROJ", "LIBV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(
            plan.replacements,
            vec![ProjectReplacement {
                project_id: "LIBPROJ".to_owned(),
                from: "LIBV1".to_owned(),
                to: "LIBV2".to_owned(),
            }],
            "keeping it would leave a release the instance cannot run, and the move is reported"
        );
    }

    fn root_needing_a_library_the_listing_omits() -> Catalog {
        Catalog::default()
            .with(
                "ROOTPROJ",
                vec![at(
                    needs(
                        compatible("ROOTV2", "ROOTPROJ"),
                        ModRelationKind::Required,
                        "LIBPROJ",
                    ),
                    10,
                )],
            )
            .with("LIBPROJ", vec![at(compatible("LIBV2", "LIBPROJ"), 20)])
    }

    #[tokio::test]
    async fn an_installed_dependency_the_listing_omits_is_kept_when_it_still_fits() {
        let catalog = root_needing_a_library_the_listing_omits()
            .answering_by_id_with(vec![at(compatible("LIBV1", "LIBPROJ"), 0)]);

        let plan = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ"),
            &[installed("LIBPROJ", "LIBV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert!(
            plan.replacements.is_empty(),
            "a release missing from a listing is not a release that is gone"
        );
        assert_eq!(plan.already_installed, vec![installed("LIBPROJ", "LIBV1")]);
    }

    #[tokio::test]
    async fn an_installed_dependency_the_provider_no_longer_knows_moves_and_is_reported() {
        let catalog = root_needing_a_library_the_listing_omits().answering_by_id_with(Vec::new());

        let plan = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ"),
            &[installed("LIBPROJ", "LIBV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(
            plan.replacements,
            vec![ProjectReplacement {
                project_id: "LIBPROJ".to_owned(),
                from: "LIBV1".to_owned(),
                to: "LIBV2".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn a_dependency_pinned_to_another_release_is_moved_and_reported() {
        let catalog = Catalog::default()
            .with(
                "ROOTPROJ",
                vec![at(
                    pins(
                        compatible("ROOTV2", "ROOTPROJ"),
                        ModRelationKind::Required,
                        "LIBV2",
                    ),
                    10,
                )],
            )
            .with(
                "LIBPROJ",
                vec![
                    at(compatible("LIBV1", "LIBPROJ"), 0),
                    at(compatible("LIBV2", "LIBPROJ"), 20),
                ],
            );

        let plan = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ"),
            &[installed("LIBPROJ", "LIBV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(
            plan.replacements,
            vec![ProjectReplacement {
                project_id: "LIBPROJ".to_owned(),
                from: "LIBV1".to_owned(),
                to: "LIBV2".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn an_update_takes_the_newest_release_that_fits_and_not_the_newest_one() {
        // The release that would win on date alone is for another Minecraft
        // version. "Latest" that ignores compatibility is the wrong answer.
        let catalog = Catalog::default().with(
            "ROOTPROJ",
            vec![
                at(compatible("ROOTV1", "ROOTPROJ"), 0),
                at(compatible("ROOTV2", "ROOTPROJ"), 10),
                at(
                    version("ROOTV3", "ROOTPROJ", &["1.21.4"], &["neoforge"]),
                    20,
                ),
                at(version("ROOTV4", "ROOTPROJ", &["1.20.1"], &["forge"]), 30),
            ],
        );

        let plan = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ"),
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(plan.root.version.id, "ROOTV2");
    }

    #[tokio::test]
    async fn an_update_with_nothing_newer_resolves_to_what_is_already_installed() {
        let catalog = Catalog::default().with(
            "ROOTPROJ",
            vec![
                at(compatible("ROOTV1", "ROOTPROJ"), 0),
                at(
                    version("ROOTV2", "ROOTPROJ", &["1.21.4"], &["neoforge"]),
                    10,
                ),
            ],
        );

        let plan = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ"),
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        assert_eq!(plan.root.version.id, "ROOTV1");
        assert!(
            plan.replacements.is_empty(),
            "nothing moved, so nothing is reported as moving"
        );
        assert!(
            !plan.changes_anything(),
            "which is what makes an update a no-op rather than a rewrite"
        );
    }

    #[tokio::test]
    async fn an_exact_version_that_does_not_fit_is_refused_rather_than_neighboured() {
        let catalog = Catalog::default().with(
            "ROOTPROJ",
            vec![
                at(compatible("ROOTV1", "ROOTPROJ"), 0),
                at(
                    version("ROOTV2", "ROOTPROJ", &["1.21.4"], &["neoforge"]),
                    10,
                ),
            ],
        );

        let error = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleSelectedVersion { ref version_id, .. }
                if version_id == "ROOTV2"
        ));
    }

    #[tokio::test]
    async fn a_change_whose_dependencies_cannot_agree_fails_without_a_plan() {
        // The new release pins the library to one version; another mod in the
        // graph pins it to a different one. Arbitrating would substitute a
        // release nobody asked for.
        let catalog = Catalog::default()
            .with(
                "ROOTPROJ",
                vec![at(
                    pins(
                        needs(
                            compatible("ROOTV2", "ROOTPROJ"),
                            ModRelationKind::Required,
                            "MIDPROJ",
                        ),
                        ModRelationKind::Required,
                        "LIBV1",
                    ),
                    10,
                )],
            )
            .with(
                "MIDPROJ",
                vec![at(
                    pins(
                        compatible("MIDV1", "MIDPROJ"),
                        ModRelationKind::Required,
                        "LIBV2",
                    ),
                    0,
                )],
            )
            .with(
                "LIBPROJ",
                vec![
                    at(compatible("LIBV1", "LIBPROJ"), 0),
                    at(compatible("LIBV2", "LIBPROJ"), 10),
                ],
            );

        let error = plan_under(
            &catalog,
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::ConflictingPinnedVersions { .. }
        ));
    }

    #[tokio::test]
    async fn the_projects_a_change_still_reaches_are_what_ownership_is_proven_from() {
        let plan = plan_under(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV1"),
            &[installed("ROOTPROJ", "ROOTV2")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        let reached: Vec<&str> = plan.resolved_projects().collect();
        assert_eq!(
            reached,
            vec!["ROOTPROJ"],
            "the older release needs no library, so nothing else is reached"
        );
    }
}
