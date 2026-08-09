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
//! choice — the launcher substituting a release nobody asked for, which is the
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
//! the smallest id among those needing work, and version candidates are ordered
//! by publication date and then by id. Reversing any response array changes
//! nothing about the result.
//!
//! # Nothing here writes
//!
//! Resolution produces a [`ModInstallPlan`] and touches no disk. Every refusal
//! — a conflicting pin, an incompatible relation, a required dependency that
//! names no project — happens before an installer is handed anything.

use std::collections::{BTreeMap, BTreeSet};

use ephervex_download::OperationReporter;
use ephervex_model::{ModOrigin, ModProvider};
use tokio_util::sync::CancellationToken;
use tracing::{debug, instrument};

use crate::error::{ModPlatformError, ModPlatformResult};
use crate::model::{
    Compatibility, DependencyKind, InstanceTarget, ModDependency, ModProject, ModVersionInfo,
};
use crate::provider::ModProviderApi;

/// How many resolution rounds are allowed before the resolver declares its own
/// invariant broken.
///
/// A project's pin can only move from "unstated" to "pinned" — a second,
/// different pin is a hard failure — so each project is resolved at most twice
/// and this ceiling is unreachable. It exists so that a bug in that reasoning
/// surfaces as a refused install rather than as a launcher that never finishes.
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
    /// here stays unselected — the product default.
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

/// One mod the plan would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedMod {
    pub project: ModProject,
    pub version: ModVersionInfo,
    pub origin: ModOrigin,
    /// Which mods asked for this one, sorted. `root` is the user's own choice.
    pub requested_by: Vec<String>,
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
    pub kind: DependencyKind,
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
}

/// A relation after normalization: one shape, with the awkward cases named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Normalized {
    /// Resolvable against the provider's catalog.
    Project {
        kind: DependencyKind,
        project_id: String,
        pinned_version: Option<String>,
    },
    /// Names a file and nothing else. Deliberately *not* turned into a project
    /// identity: there is nothing to derive one from, and inventing one would
    /// make the plan claim something it cannot look up.
    External {
        kind: DependencyKind,
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
        ModOrigin::PackDeclared => 0,
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
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// Works out what installing the selected mod involves.
#[instrument(skip_all, fields(project = %request.selection.project_id))]
pub async fn resolve(request: ResolveRequest<'_>) -> ModPlatformResult<ModInstallPlan> {
    let mut state = Resolver::new(request);
    state.run().await
}

struct Resolver<'a> {
    request: ResolveRequest<'a>,
    constraints: BTreeMap<String, Constraint>,
    resolved: BTreeMap<String, Resolution>,
    /// Relations contributed by each resolved `(project, version)`. Keyed by
    /// version so that re-resolving a project retracts exactly what its
    /// previous version said.
    edges: BTreeMap<(String, String), Vec<Normalized>>,
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

            self.refold()?;
            let Some(next) = self.next_dirty() else { break };

            let pin = self
                .constraints
                .get(&next)
                .and_then(|constraint| constraint.pin.clone());
            let version = self.select_version(&next, pin.as_deref()).await?;
            debug!(project = %next, version = %version.id, "resolved a project");

            // A project's relations belong to the version that declared them,
            // so re-resolving retracts the old ones wholesale.
            self.edges.retain(|(project, _), _| project != &next);
            let normalized = self.normalize_all(&version).await?;
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

    /// Rebuilds the constraint map from the root plus every surviving edge.
    ///
    /// Recomputed from scratch rather than patched: a retracted edge must not
    /// be able to leave a constraint behind, and proving that of an incremental
    /// update is harder than doing the work.
    fn refold(&mut self) -> ModPlatformResult<()> {
        let selection = self.request.selection;
        let mut constraints: BTreeMap<String, Constraint> = BTreeMap::new();

        constraints
            .entry(selection.project_id.clone())
            .or_default()
            .merge(
                &selection.project_id,
                selection.version_id.as_deref(),
                ROOT,
                ModOrigin::UserSelected,
            )?;

        // Only edges from projects that are still in the graph count. A project
        // dropped by an earlier retraction contributes nothing.
        let edges: Vec<((String, String), Vec<Normalized>)> = self
            .edges
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        for ((source, _), relations) in edges {
            for relation in relations {
                let Normalized::Project {
                    kind,
                    project_id,
                    pinned_version,
                } = relation
                else {
                    continue;
                };
                let origin = match kind {
                    DependencyKind::Required => ModOrigin::RequiredDependency,
                    DependencyKind::Optional if selection.optional.contains(&project_id) => {
                        ModOrigin::SelectedOptionalDependency
                    }
                    // Optional and not taken, incompatible, or embedded: none
                    // of these put anything in the graph.
                    _ => continue,
                };
                constraints.entry(project_id.clone()).or_default().merge(
                    &project_id,
                    pinned_version.as_deref(),
                    &source,
                    origin,
                )?;
            }
        }

        // Anything no longer reachable stops being resolved, so a stale
        // resolution cannot reappear in the plan.
        self.resolved
            .retain(|project, _| constraints.contains_key(project));
        self.edges
            .retain(|(project, _), _| constraints.contains_key(project));
        self.constraints = constraints;
        Ok(())
    }

    /// The smallest project id that still needs resolving.
    ///
    /// "Needs resolving" is either never resolved, or resolved under a weaker
    /// constraint than it now carries — which is precisely the case a `visited`
    /// set gets wrong.
    fn next_dirty(&self) -> Option<String> {
        self.constraints
            .iter()
            .find(|(project, constraint)| match self.resolved.get(*project) {
                None => true,
                Some(resolution) => resolution.under != constraint.pin,
            })
            .map(|(project, _)| project.clone())
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
            // alternative — quietly taking a neighbouring release — is the
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

        newest_compatible(
            &self.versions(project_id).await?,
            target,
            self.request.loader_name,
        )
        .ok_or_else(|| ModPlatformError::NoCompatibleVersion {
            project_id: project_id.to_owned(),
            minecraft: target.minecraft_version.clone(),
            loader: target.loader,
        })
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

    async fn versions(&self, project_id: &str) -> ModPlatformResult<Vec<ModVersionInfo>> {
        self.request
            .provider
            .versions(project_id, self.request.reporter, self.request.cancel)
            .await
    }

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
            .next()
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
                if planned.project.client_side == crate::model::SideSupport::Unsupported {
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

        let already_installed = self.already_installed()?;

        Ok(ModInstallPlan {
            provider: self.request.provider.provider(),
            root,
            dependencies,
            optional: self.optional.values().cloned().collect(),
            external: std::mem::take(&mut self.external),
            embedded: std::mem::take(&mut self.embedded),
            already_installed,
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
                        kind: DependencyKind::Optional,
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
                        kind: DependencyKind::Embedded,
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
    /// whether it would even fit — but a failure to resolve is not fatal: an
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
        kind: DependencyKind,
        file_name: String,
    ) -> ModPlatformResult<()> {
        match kind {
            // Installing an incomplete graph would produce an instance that
            // crashes on launch for a reason the launcher already knew.
            DependencyKind::Required => {
                return Err(ModPlatformError::UnresolvableRequiredDependency {
                    declared_by: source.to_owned(),
                    file_name,
                });
            }
            DependencyKind::Incompatible => {
                self.warnings.push(PlanWarning::UncheckableConflict {
                    declared_by: source.to_owned(),
                    file_name: file_name.clone(),
                });
            }
            DependencyKind::Embedded => self.embedded.push(EmbeddedNote {
                project_id: None,
                file_name: Some(file_name.clone()),
                declared_by: source.to_owned(),
            }),
            DependencyKind::Optional => {}
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
        let installed: BTreeSet<&str> = self
            .request
            .installed
            .iter()
            .map(|entry| entry.project_id.as_str())
            .collect();

        for ((source, _), relations) in &self.edges {
            for relation in relations {
                let Normalized::Project {
                    kind: DependencyKind::Incompatible,
                    project_id,
                    ..
                } = relation
                else {
                    continue;
                };
                if self.resolved.contains_key(project_id) || installed.contains(project_id.as_str())
                {
                    return Err(ModPlatformError::IncompatibleDependency {
                        declared_by: source.clone(),
                        conflicting: project_id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Splits the resolved set into "already there" and "would be installed",
    /// refusing a version change.
    fn already_installed(&self) -> ModPlatformResult<Vec<InstalledProject>> {
        let mut present = Vec::new();
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
            // Replacing it is a mod update, which this build does not
            // implement. Refusing here means the instance is never touched.
            return Err(ModPlatformError::ProjectAlreadyInstalled {
                provider: self.request.provider.provider(),
                project_id: project_id.clone(),
                installed: installed.version_id.clone(),
                requested: resolution.version.id.clone(),
            });
        }
        Ok(present)
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
    }

    impl Catalog {
        fn with(mut self, project_id: &str, versions: Vec<ModVersionInfo>) -> Self {
            self.versions.insert(project_id.to_owned(), versions);
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
        resolve(ResolveRequest {
            provider: catalog,
            target: &target(),
            loader_name: "neoforge",
            selection: &selection,
            installed,
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
                    DependencyKind::Required,
                    "MID",
                )],
            )
            .with(
                "MID",
                vec![needs(
                    compatible("v1", "MID"),
                    DependencyKind::Required,
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
                    DependencyKind::Optional,
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
                    DependencyKind::Optional,
                    "EXTRA",
                )],
            )
            .with(
                "EXTRA",
                vec![needs(
                    compatible("v1", "EXTRA"),
                    DependencyKind::Required,
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
                    needs(compatible("v1", "ROOT"), DependencyKind::Required, "MID"),
                    DependencyKind::Incompatible,
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
                    DependencyKind::Required,
                    "MID",
                )],
            )
            .with(
                "MID",
                vec![needs(
                    compatible("v1", "MID"),
                    DependencyKind::Required,
                    "LEAF",
                )],
            )
            .with(
                "LEAF",
                vec![needs(
                    compatible("v1", "LEAF"),
                    DependencyKind::Incompatible,
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
                DependencyKind::Incompatible,
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

    #[tokio::test]
    async fn a_dependency_cycle_terminates() {
        let catalog = Catalog::default()
            .with(
                "A",
                vec![needs(compatible("v1", "A"), DependencyKind::Required, "B")],
            )
            .with(
                "B",
                vec![needs(compatible("v1", "B"), DependencyKind::Required, "A")],
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
                    needs(compatible("v1", "ROOT"), DependencyKind::Required, "LEFT"),
                    DependencyKind::Required,
                    "RIGHT",
                )],
            )
            .with(
                "LEFT",
                vec![needs(
                    compatible("v1", "LEFT"),
                    DependencyKind::Required,
                    "SHARED",
                )],
            )
            .with(
                "RIGHT",
                vec![needs(
                    compatible("v1", "RIGHT"),
                    DependencyKind::Required,
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
                    needs(compatible("v1", "ROOT"), DependencyKind::Required, "AAA"),
                    DependencyKind::Required,
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
                    DependencyKind::Required,
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

    #[tokio::test]
    async fn a_pin_is_not_weakened_by_a_later_unpinned_requirement() {
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    pins(compatible("v1", "ROOT"), DependencyKind::Required, "old"),
                    DependencyKind::Required,
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
                    DependencyKind::Required,
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
                    needs(compatible("v1", "ROOT"), DependencyKind::Required, "AAA"),
                    DependencyKind::Required,
                    "BBB",
                )],
            )
            .with(
                "AAA",
                vec![pins(
                    compatible("v1", "AAA"),
                    DependencyKind::Required,
                    "old",
                )],
            )
            .with(
                "BBB",
                vec![pins(
                    compatible("v1", "BBB"),
                    DependencyKind::Required,
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
                    needs(compatible("v1", "ROOT"), DependencyKind::Required, "LEFT"),
                    DependencyKind::Required,
                    "RIGHT",
                )],
            )
            .with(
                "LEFT",
                vec![needs(
                    compatible("v1", "LEFT"),
                    DependencyKind::Required,
                    "SHARED",
                )],
            )
            .with(
                "RIGHT",
                vec![needs(
                    compatible("v1", "RIGHT"),
                    DependencyKind::Required,
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
                    DependencyKind::Required,
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
            kind: DependencyKind::Required,
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
                DependencyKind::Required,
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
                DependencyKind::Optional,
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
                DependencyKind::Incompatible,
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
    async fn a_dependency_identifying_nothing_is_refused() {
        let mut root = compatible("v1", "ROOT");
        root.dependencies.push(ModDependency {
            kind: DependencyKind::Required,
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
                    DependencyKind::Embedded,
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
                    needs(compatible("v1", "ROOT"), DependencyKind::Embedded, "SHADED"),
                    DependencyKind::Required,
                    "OTHER",
                )],
            )
            .with(
                "OTHER",
                vec![needs(
                    compatible("v1", "OTHER"),
                    DependencyKind::Required,
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
                    DependencyKind::Required,
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
}
