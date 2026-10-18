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
//! # Plans that only a pin can justify
//!
//! A pass takes each project's newest release until a pin says otherwise, which
//! can go around in circles, end in a pin conflict or hit a refusal although a
//! consistent plan exists: a release can be needed only because of a pin that
//! the release itself brings in. When a pass stalls like that, the resolver
//! assumes pins on a few releases, within fixed bounds, and keeps an assumption
//! only if the settled graph pins that release for real. With no such plan the
//! pass's own refusal stands.
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

/// How many resolution rounds one pass is allowed before the resolver gives up.
///
/// A pass that repeats a state stops at once, so only a graph that keeps
/// needing new actions, such as a very long chain, reaches this.
const MAX_RESOLUTION_ROUNDS: usize = 512;

/// How many releases may be assumed pinned at once.
const MAX_PIN_ASSUMPTIONS: usize = 3;

/// How many assumption sets are tried before the first pass's refusal stands.
const MAX_ASSUMPTION_BRANCHES: usize = 64;

/// How many rounds all the assumption sets together may take.
const MAX_SEARCH_ROUNDS: usize = 2048;

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
    /// The project the relation names, or whose release it pins. `None` only
    /// for a relation that pins a release and names no project, when the
    /// provider could not say which project owns that release. Such an offer
    /// cannot be selected, because a selection names projects.
    pub project_id: Option<String>,
    /// The project's title, when the catalog described the project.
    pub title: Option<String>,
    /// The release the relation pins, when it pins one.
    pub pinned_version_id: Option<String>,
    /// The mod that offers it.
    pub declared_by: String,
    /// The version that would be installed if it were selected, when one fits.
    pub version: Option<ModVersionInfo>,
    pub status: OptionalStatus,
}

/// What is known about an optional dependency's fit with the instance.
///
/// A lookup that failed is not an absence of compatible releases, so the two
/// stay different states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionalStatus {
    /// A release that fits is offered.
    Compatible,
    /// The provider answered and no release fits.
    Incompatible(Vec<crate::model::IncompatibilityReason>),
    /// The provider's answer did not settle the question.
    Unverified(UnverifiedOptional),
}

impl OptionalStatus {
    #[must_use]
    pub const fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }

    /// Why no release fits, or an empty slice for any other state.
    #[must_use]
    pub fn reasons(&self) -> &[crate::model::IncompatibilityReason] {
        match self {
            Self::Incompatible(reasons) => reasons,
            Self::Compatible | Self::Unverified(_) => &[],
        }
    }
}

impl From<Compatibility> for OptionalStatus {
    fn from(compatibility: Compatibility) -> Self {
        match compatibility {
            Compatibility::Compatible => Self::Compatible,
            Compatibility::Incompatible(reasons) => Self::Incompatible(reasons),
        }
    }
}

/// Why an optional dependency could not be checked against the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnverifiedOptional {
    /// The provider could not be asked or refused to answer.
    Unavailable,
    /// The provider does not know the project or the release the relation names.
    UnknownIdentity,
    /// The release the relation pins belongs to a different project than the
    /// one the relation names.
    ConflictingIdentity { version_project_id: String },
}

impl UnverifiedOptional {
    /// A stable machine-readable name.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::UnknownIdentity => "unknown_identity",
            Self::ConflictingIdentity { .. } => "conflicting_identity",
        }
    }

    /// The `ephervex-i18n` message id describing this state.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unavailable => "mod-optional-unavailable",
            Self::UnknownIdentity => "mod-optional-unknown-identity",
            Self::ConflictingIdentity { .. } => "mod-optional-conflicting-identity",
        }
    }
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
    /// The release the relation pins, when it names no project and the release
    /// could not be read to learn which one owns it.
    pub version_id: Option<String>,
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
    /// A required dependency declares that it does not run on a server, and a
    /// server install still puts it there because the mod that needs it cannot
    /// load without it.
    ServerSideUnsupported {
        project_id: String,
        required_by: Vec<String>,
    },
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
            Self::ServerSideUnsupported { .. } => "mod-warning-server-side-unsupported",
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
    /// The mods this plan would install, root first, whose release and project
    /// both leave the server side undeclared. Installing one on a server is an
    /// explicit choice, not something the catalog confirmed, and one already
    /// installed was chosen when it was.
    #[must_use]
    pub fn undeclared_server_support(&self) -> Vec<&PlannedMod> {
        self.to_install()
            .into_iter()
            .filter(|planned| planned.server_side() == crate::model::SideSupport::Unknown)
            .collect()
    }

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
    /// An embedded or incompatible relation that pins a release and names no
    /// project, whose release the provider could not describe. Neither kind
    /// installs anything, so the plan carries on without the owner: an embedded
    /// one is noted, and an incompatible one is matched by release id.
    UnidentifiedRelease {
        kind: ModRelationKind,
        version_id: String,
    },
    /// An optional relation that pins a release and names no project, whose
    /// owning project could not be learned. It stays an offer that says so,
    /// because the extra is not what the user asked to install.
    UnverifiedRelease {
        version_id: String,
        unverified: UnverifiedOptional,
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

/// The refusal for a pin whose version belongs to another project. A mod that
/// declares the pin carries the inconsistent metadata, so mods are named in
/// preference to the user's own selection.
fn inconsistent_pin(
    project_id: &str,
    version_project_id: String,
    declarers: &BTreeSet<String>,
) -> ModPlatformError {
    let mods: BTreeSet<String> = declarers
        .iter()
        .filter(|declarer| declarer.as_str() != ROOT)
        .cloned()
        .collect();
    if mods.is_empty() {
        ModPlatformError::InconsistentSelectedVersion {
            project_id: project_id.to_owned(),
            version_project_id,
        }
    } else {
        ModPlatformError::InconsistentDependency {
            declared_by: join(&mods),
            project_id: project_id.to_owned(),
            version_project_id,
        }
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
    /// Projects the desired state records only for the other side of the game.
    ///
    /// The recipe still holds its one entry for each, so moving one to another
    /// release is a replacement and an unpinned one keeps its release. None of
    /// them is present, though: a planned one is installed rather than reused,
    /// and nothing conflicts with it.
    pub recorded_elsewhere: &'a [InstalledProject],
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
            | ModPlatformError::InconsistentSelectedVersion { .. }
            | ModPlatformError::MalformedDependency { .. }
    )
}

/// A copy of a catalog refusal, which carries only plain values. Other errors
/// are not copied, so they are asked for again.
fn copy_refusal(error: &ModPlatformError) -> Option<ModPlatformError> {
    Some(match error {
        ModPlatformError::IncompatibleSelectedVersion {
            project_id,
            version_id,
            minecraft,
            loader,
        } => ModPlatformError::IncompatibleSelectedVersion {
            project_id: project_id.clone(),
            version_id: version_id.clone(),
            minecraft: minecraft.clone(),
            loader: *loader,
        },
        ModPlatformError::NoCompatibleVersion {
            project_id,
            minecraft,
            loader,
        } => ModPlatformError::NoCompatibleVersion {
            project_id: project_id.clone(),
            minecraft: minecraft.clone(),
            loader: *loader,
        },
        ModPlatformError::ProjectUnknown {
            provider,
            reference,
        } => ModPlatformError::ProjectUnknown {
            provider: *provider,
            reference: reference.clone(),
        },
        ModPlatformError::VersionUnknown {
            provider,
            version_id,
        } => ModPlatformError::VersionUnknown {
            provider: *provider,
            version_id: version_id.clone(),
        },
        ModPlatformError::InconsistentDependency {
            declared_by,
            project_id,
            version_project_id,
        } => ModPlatformError::InconsistentDependency {
            declared_by: declared_by.clone(),
            project_id: project_id.clone(),
            version_project_id: version_project_id.clone(),
        },
        ModPlatformError::InconsistentSelectedVersion {
            project_id,
            version_project_id,
        } => ModPlatformError::InconsistentSelectedVersion {
            project_id: project_id.clone(),
            version_project_id: version_project_id.clone(),
        },
        ModPlatformError::MalformedDependency { declared_by } => {
            ModPlatformError::MalformedDependency {
                declared_by: declared_by.clone(),
            }
        }
        _ => return None,
    })
}

/// Whether an error describes the content of one release rather than the
/// provider failing: the release is unusable or unknown, not the lookup.
const fn is_release_content_refusal(error: &ModPlatformError) -> bool {
    matches!(
        error,
        ModPlatformError::VersionUnknown { .. }
            | ModPlatformError::FileUnavailable { .. }
            | ModPlatformError::HashesMissing { .. }
            | ModPlatformError::AmbiguousPrimaryFile { .. }
    )
}

/// Whether an error ends only the attempt, rather than the whole resolution.
const fn is_dead_end(error: &ModPlatformError) -> bool {
    is_graph_refusal(error)
        || is_release_content_refusal(error)
        || matches!(error, ModPlatformError::ResolutionDidNotConverge { .. })
}

/// A refusal, with the pin and the declarers it was reached under. It holds
/// only while the graph still asks for the project the same way, so a
/// refusal naming a declarer that has left the graph is not kept.
struct Refusal {
    pin: Option<String>,
    declarers: BTreeSet<String>,
    error: ModPlatformError,
}

/// Everything a pass's next action depends on: each resolved release with the
/// pin it was resolved under, and the refusals held. Meeting one twice means
/// the pass loops forever.
type ResolutionState = (
    Vec<(String, String, Option<String>)>,
    Vec<(String, Option<String>, BTreeSet<String>)>,
);

/// One project the resolver picked a release for, and the pin it was under.
#[derive(Debug, Clone)]
struct Step {
    project: String,
    pin: Option<String>,
    declarers: BTreeSet<String>,
}

/// A pass that ended without a plan, though other choices might produce one.
#[derive(Debug)]
struct Stalled {
    cause: StallCause,
    /// The projects whose releases kept changing, or that were pinned two ways
    /// and the mods that pinned them.
    involved: BTreeSet<String>,
    /// The involved projects and everything requiring them, at the stall.
    upstream: BTreeSet<String>,
    /// Every other project the pass held or had listed releases for.
    rest: BTreeSet<String>,
}

/// A group of assumptions to try.
#[derive(Debug)]
enum Candidates {
    /// These pairs.
    Pairs(Vec<(String, String)>),
    /// Every pin that a release read so far makes on one of these projects.
    PinsOn(BTreeSet<String>),
    /// Each release of this project that fits the instance.
    ReleasesOf(String),
}

#[derive(Debug)]
enum StallCause {
    /// The pass came back to an earlier state, after these steps.
    Repeating(Vec<Step>),
    /// The graph pins projects to two different versions.
    Conflicting(Vec<ModPlatformError>),
    /// The catalog refuses a project the graph reaches.
    Refused(ModPlatformError),
}

impl Stalled {
    /// The pins the pass kept switching between, or both sides of a conflict.
    fn hints(&self) -> Vec<(String, String)> {
        let hints: BTreeSet<(String, String)> = match &self.cause {
            StallCause::Refused(_) => BTreeSet::new(),
            StallCause::Repeating(steps) => steps
                .iter()
                .filter_map(|step| Some((step.project.clone(), step.pin.clone()?)))
                .collect(),
            StallCause::Conflicting(conflicts) => conflicts
                .iter()
                .filter_map(|conflict| match conflict {
                    ModPlatformError::ConflictingPinnedVersions {
                        project_id,
                        first,
                        second,
                        ..
                    } => Some([
                        (project_id.clone(), first.clone()),
                        (project_id.clone(), second.clone()),
                    ]),
                    _ => None,
                })
                .flatten()
                .collect(),
        };
        hints.into_iter().collect()
    }

    /// What to assume, most promising first.
    fn candidates(&self) -> Vec<Candidates> {
        let mut groups = vec![
            Candidates::Pairs(self.hints()),
            Candidates::PinsOn(self.upstream.clone()),
        ];
        groups.extend(self.involved.iter().cloned().map(Candidates::ReleasesOf));
        groups.extend(
            self.upstream
                .difference(&self.involved)
                .cloned()
                .map(Candidates::ReleasesOf),
        );
        // A refusal concerns one project, so only what leads to it is varied.
        if !matches!(self.cause, StallCause::Refused(_)) {
            groups.push(Candidates::PinsOn(self.rest.clone()));
            groups.extend(self.rest.iter().cloned().map(Candidates::ReleasesOf));
        }
        groups
    }

    /// The refusal that stands when no assumption produces a plan.
    fn into_error(self) -> ModPlatformError {
        match self.cause {
            StallCause::Repeating(steps) => {
                let projects: BTreeSet<String> =
                    steps.iter().map(|step| step.project.clone()).collect();
                let pins: BTreeSet<String> = steps
                    .iter()
                    .filter_map(|step| {
                        let pin = step.pin.as_ref()?;
                        Some(format!(
                            "{} = {pin} ({})",
                            step.project,
                            join(&step.declarers)
                        ))
                    })
                    .collect();
                ModPlatformError::NoConsistentVersions {
                    projects: join(&projects),
                    pins: pins.into_iter().collect::<Vec<_>>().join("; "),
                }
            }
            StallCause::Conflicting(mut conflicts) => conflicts.remove(0),
            StallCause::Refused(error) => error,
        }
    }
}

/// How a pass over the graph ended.
#[derive(Debug)]
enum Pass {
    /// Nothing is left to resolve and no pin conflicts.
    Settled,
    Stalled(Stalled),
}

struct Resolver<'a> {
    request: ResolveRequest<'a>,
    /// Rounds spent on assumption sets so far.
    search_rounds: usize,
    /// Releases assumed pinned while searching; each counts as a pin on its
    /// project until the graph is settled.
    assumed: BTreeMap<String, String>,
    /// Releases already chosen for a project under a pin.
    selected: BTreeMap<(String, Option<String>), ModVersionInfo>,
    /// Releases from a listing, for a pin that only an assumption makes. The
    /// release the user selected is always looked up by its id.
    listed_pins: BTreeMap<(String, String), ModVersionInfo>,
    /// Catalog refusals of a project's listing, so asking again is not needed.
    listing_refusals: BTreeMap<String, ModPlatformError>,
    /// Catalog refusals of a selection, by project, pin and the pin's declarers.
    selection_refusals: BTreeMap<(String, Option<String>, BTreeSet<String>), ModPlatformError>,
    /// Every release the provider lists for a project already asked about.
    listings: BTreeMap<String, Vec<ModVersionInfo>>,
    /// The relations of releases already read, by project and release.
    normalized: BTreeMap<(String, String), Vec<Normalized>>,
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
    refused: BTreeMap<String, Refusal>,
    /// Projects the current graph pins to two different versions. Reported only
    /// once nothing else is left to resolve, because resolving a project that
    /// has not been explored yet can retract either side.
    conflicts: BTreeMap<String, ModPlatformError>,
    optional: BTreeMap<String, OptionalCandidate>,
    /// Offers whose project is unknown, keyed by the release they pin.
    unowned_optional: BTreeMap<String, OptionalCandidate>,
    external: Vec<ExternalDependency>,
    embedded: Vec<EmbeddedNote>,
    warnings: Vec<PlanWarning>,
    /// Optional selections that have been honored, so unmatched ones can be
    /// reported.
    honored_optional: BTreeSet<String>,
    /// The project each release looked up for a version-only pin belongs to,
    /// so the plan's releases can state which project such a relation names.
    pinned_owners: BTreeMap<String, String>,
}

impl<'a> Resolver<'a> {
    fn new(request: ResolveRequest<'a>) -> Self {
        Self {
            request,
            search_rounds: 0,
            assumed: BTreeMap::new(),
            selected: BTreeMap::new(),
            listed_pins: BTreeMap::new(),
            listing_refusals: BTreeMap::new(),
            selection_refusals: BTreeMap::new(),
            listings: BTreeMap::new(),
            normalized: BTreeMap::new(),
            constraints: BTreeMap::new(),
            resolved: BTreeMap::new(),
            edges: BTreeMap::new(),
            refused: BTreeMap::new(),
            conflicts: BTreeMap::new(),
            optional: BTreeMap::new(),
            unowned_optional: BTreeMap::new(),
            external: Vec::new(),
            embedded: Vec::new(),
            warnings: Vec::new(),
            honored_optional: BTreeSet::new(),
            pinned_owners: BTreeMap::new(),
        }
    }

    async fn run(&mut self) -> ModPlatformResult<ModInstallPlan> {
        let root_id = self.request.selection.project_id.clone();

        match self.pass().await? {
            Pass::Settled => {}
            Pass::Stalled(stalled) => {
                if !self.search_assumptions(&stalled).await? {
                    return Err(stalled.into_error());
                }
            }
        }
        self.finish(&root_id).await
    }

    /// Resolves again with some releases assumed pinned, fewest assumptions
    /// first and within fixed bounds. Leaves the resolver on the settled graph
    /// and returns `true` when one set is pinned by the graph itself.
    async fn search_assumptions(&mut self, stalled: &Stalled) -> ModPlatformResult<bool> {
        let mut tried: BTreeSet<BTreeMap<String, String>> = BTreeSet::new();
        let mut attempts = 0;
        let mut frontier = vec![(BTreeMap::new(), None)];
        for _ in 0..MAX_PIN_ASSUMPTIONS {
            let mut deeper = Vec::new();
            for (base, own) in frontier {
                for group in own.as_ref().unwrap_or(stalled).candidates() {
                    for (project, release) in self.expand(group).await? {
                        if base.contains_key(&project) {
                            continue;
                        }
                        let mut assumed = base.clone();
                        assumed.insert(project, release);
                        if !tried.insert(assumed.clone()) {
                            continue;
                        }
                        if attempts == MAX_ASSUMPTION_BRANCHES
                            || self.search_rounds >= MAX_SEARCH_ROUNDS
                        {
                            return Ok(false);
                        }
                        attempts += 1;
                        self.restart(assumed.clone());
                        match self.pass().await {
                            Ok(Pass::Settled) if self.assumptions_hold() => return Ok(true),
                            Ok(Pass::Settled) => {}
                            Ok(Pass::Stalled(stalled)) => deeper.push((assumed, Some(stalled))),
                            Err(error) if is_dead_end(&error) => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            frontier = deeper;
        }
        Ok(false)
    }

    /// The assumptions a group stands for; only a reached group asks the provider.
    async fn expand(&mut self, group: Candidates) -> ModPlatformResult<Vec<(String, String)>> {
        match group {
            Candidates::Pairs(pairs) => Ok(pairs),
            Candidates::PinsOn(projects) => Ok(self
                .pinnable()
                .into_iter()
                .filter(|(project, _)| projects.contains(project))
                .collect()),
            Candidates::ReleasesOf(project) => {
                let Some(mut releases) = self.listed_releases(&project).await? else {
                    return Ok(Vec::new());
                };
                releases.sort_by(|left, right| {
                    (right.published_at, &right.id).cmp(&(left.published_at, &left.id))
                });
                releases.retain(|release| {
                    release
                        .compatibility(self.request.target, self.request.loader_name)
                        .is_compatible()
                });
                // Assuming a listed release needs no lookup of its own.
                for release in &releases {
                    self.listed_pins
                        .entry((project.clone(), release.id.clone()))
                        .or_insert_with(|| release.clone());
                }
                Ok(releases
                    .into_iter()
                    .map(|release| (project.clone(), release.id))
                    .collect())
            }
        }
    }

    /// The projects, and everything that requires them directly or not.
    fn requesters_of(&self, projects: &BTreeSet<String>) -> BTreeSet<String> {
        let mut found = projects.clone();
        let mut pending: Vec<String> = projects.iter().cloned().collect();
        while let Some(project) = pending.pop() {
            let Some(constraint) = self.constraints.get(&project) else {
                continue;
            };
            for requester in &constraint.requesters {
                if found.insert(requester.clone()) {
                    pending.push(requester.clone());
                }
            }
        }
        found
    }

    /// A project's releases, or `None` when the catalog cannot list them.
    async fn listed_releases(
        &mut self,
        project_id: &str,
    ) -> ModPlatformResult<Option<Vec<ModVersionInfo>>> {
        if let Some(listed) = self.listings.get(project_id) {
            return Ok(Some(listed.clone()));
        }
        if self.listing_refusals.contains_key(project_id) {
            return Ok(None);
        }
        match self.versions(project_id).await {
            Ok(listed) => {
                self.listings.insert(project_id.to_owned(), listed.clone());
                Ok(Some(listed))
            }
            Err(error) if is_graph_refusal(&error) => {
                if let Some(copy) = copy_refusal(&error) {
                    self.listing_refusals.insert(project_id.to_owned(), copy);
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Every `(project, release)` that a release read so far pins.
    fn pinnable(&self) -> BTreeSet<(String, String)> {
        self.listings
            .values()
            .flatten()
            .chain(self.selected.values())
            .flat_map(|release| &release.dependencies)
            .filter(|dependency| {
                dependency.kind == ModRelationKind::Required
                    || (dependency.kind == ModRelationKind::Optional
                        && dependency.project_id.as_ref().is_some_and(|project| {
                            self.request.selection.optional.contains(project)
                        }))
            })
            .filter_map(|dependency| {
                let release = dependency.version_id.as_ref()?;
                let project = dependency
                    .project_id
                    .as_ref()
                    .or_else(|| self.pinned_owners.get(release))?;
                Some((project.clone(), release.clone()))
            })
            .collect()
    }

    /// Clears the graph for another attempt; what the provider said is kept.
    fn restart(&mut self, assumed: BTreeMap<String, String>) {
        self.assumed = assumed;
        self.constraints.clear();
        self.resolved.clear();
        self.edges.clear();
        self.refused.clear();
        self.conflicts.clear();
    }

    /// Whether the graph pins every assumed release without the assumption.
    fn assumptions_hold(&self) -> bool {
        let (constraints, _) = self.fold_assuming(&BTreeSet::new(), &BTreeMap::new());
        self.assumed.iter().all(|(project, release)| {
            constraints
                .get(project)
                .and_then(|constraint| constraint.pin.as_deref())
                == Some(release.as_str())
        })
    }

    fn state(&self) -> ResolutionState {
        (
            self.resolved
                .iter()
                .map(|(project, resolution)| {
                    (
                        project.clone(),
                        resolution.version.id.clone(),
                        resolution.under.clone(),
                    )
                })
                .collect(),
            self.refused
                .iter()
                .map(|(project, refusal)| {
                    (
                        project.clone(),
                        refusal.pin.clone(),
                        refusal.declarers.clone(),
                    )
                })
                .collect(),
        )
    }

    /// The stall a pass ends in when the graph pins projects two ways.
    fn conflict_stall(&mut self) -> Stalled {
        let conflicts = std::mem::take(&mut self.conflicts);
        let mut involved = BTreeSet::new();
        for (project, conflict) in &conflicts {
            involved.insert(project.clone());
            if let Some(constraint) = self.constraints.get(project) {
                involved.extend(constraint.pin_sources.iter().cloned());
            }
            if let ModPlatformError::ConflictingPinnedVersions { second_source, .. } = conflict {
                involved.insert(second_source.clone());
            }
        }
        self.stall(
            StallCause::Conflicting(conflicts.into_values().collect()),
            involved,
        )
    }

    fn stall(&self, cause: StallCause, mut involved: BTreeSet<String>) -> Stalled {
        // The root's own selection and an assumption's source name no project.
        involved.retain(|project| project != ROOT && !project.is_empty());
        let mut upstream = self.requesters_of(&involved);
        upstream.retain(|project| project != ROOT && !project.is_empty());
        Stalled {
            rest: self
                .constraints
                .keys()
                .chain(self.listings.keys())
                .filter(|project| !upstream.contains(*project))
                .cloned()
                .collect(),
            upstream,
            cause,
            involved,
        }
    }

    /// Counts a round against the search's budget while assumptions are in play.
    fn spend_search_round(&mut self, round: usize) -> ModPlatformResult<()> {
        if self.assumed.is_empty() {
            return Ok(());
        }
        self.search_rounds += 1;
        if self.search_rounds > MAX_SEARCH_ROUNDS {
            return Err(ModPlatformError::ResolutionDidNotConverge { iterations: round });
        }
        Ok(())
    }

    /// Resolves one project at a time until nothing is left to resolve.
    async fn pass(&mut self) -> ModPlatformResult<Pass> {
        let mut seen: BTreeMap<ResolutionState, usize> = BTreeMap::new();
        let mut steps: Vec<Step> = Vec::new();

        for round in 0..=MAX_RESOLUTION_ROUNDS {
            if self.request.cancel.is_cancelled() {
                return Err(ModPlatformError::Canceled);
            }
            self.spend_search_round(round)?;

            self.refold();
            self.refused.retain(|project, refusal| {
                self.constraints.get(project).is_some_and(|constraint| {
                    constraint.pin == refusal.pin && constraint.pin_sources == refusal.declarers
                })
            });
            let Some(next) = self.next_dirty() else {
                if !self.conflicts.is_empty() {
                    return Ok(Pass::Stalled(self.conflict_stall()));
                }
                if let Some((project, refusal)) = self.refused.pop_first() {
                    let mut involved = refusal.declarers;
                    involved.insert(project);
                    return Ok(Pass::Stalled(
                        self.stall(StallCause::Refused(refusal.error), involved),
                    ));
                }
                return Ok(Pass::Settled);
            };
            if let Some(first) = seen.insert(self.state(), steps.len()) {
                let cycle = steps.split_off(first);
                let involved = cycle.iter().map(|step| step.project.clone()).collect();
                return Ok(Pass::Stalled(
                    self.stall(StallCause::Repeating(cycle), involved),
                ));
            }
            // The last round only observes: it allows no further action.
            if round == MAX_RESOLUTION_ROUNDS {
                return Err(ModPlatformError::ResolutionDidNotConverge { iterations: round });
            }

            let (pin, declarers) = self.constraints.get(&next).map_or_else(
                || (None, BTreeSet::new()),
                |constraint| (constraint.pin.clone(), constraint.pin_sources.clone()),
            );
            steps.push(Step {
                project: next.clone(),
                pin: pin.clone(),
                declarers: declarers.clone(),
            });
            let version = match self
                .selected_version(&next, pin.as_deref(), &declarers)
                .await
            {
                Ok(version) => version,
                Err(error) if is_graph_refusal(&error) => {
                    self.refused.insert(
                        next,
                        Refusal {
                            pin,
                            declarers,
                            error,
                        },
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            debug!(project = %next, version = %version.id, "resolved a project");

            // A project's relations belong to the version that declared them,
            // so re-resolving retracts the old ones wholesale.
            self.edges.retain(|(project, _), _| project != &next);
            let normalized = match self.relations_of(&version).await {
                Ok(normalized) => normalized,
                Err(error) if is_graph_refusal(&error) => {
                    // The old release's relations are gone, so it must not stay
                    // resolved without them.
                    self.resolved.remove(&next);
                    self.refused.insert(
                        next,
                        Refusal {
                            pin,
                            declarers,
                            error,
                        },
                    );
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

        Err(ModPlatformError::ResolutionDidNotConverge {
            iterations: MAX_RESOLUTION_ROUNDS,
        })
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
            let independent = without_own.get(&project).and_then(|constraint| {
                constraint
                    .pin
                    .clone()
                    .map(|pin| (pin, constraint.pin_sources.clone()))
            });
            let Some((pin, pin_sources)) = independent else {
                continue;
            };
            if still_conflicted.contains_key(&project)
                || self
                    .resolved
                    .get(&project)
                    .is_some_and(|resolution| resolution.under.as_deref() == Some(pin.as_str()))
            {
                continue;
            }
            if let Some(constraint) = constraints.get_mut(&project) {
                // The sources travel with the pin they declared.
                constraint.pin = Some(pin);
                constraint.pin_sources = pin_sources;
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
        self.fold_assuming(skipped, &self.assumed)
    }

    /// [`Self::fold`] with the given releases counted as pins, for the projects
    /// the graph reaches.
    fn fold_assuming(
        &self,
        skipped: &BTreeSet<String>,
        assumed: &BTreeMap<String, String>,
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

        for (project_id, release) in assumed {
            let Some(constraint) = constraints.get_mut(project_id) else {
                continue;
            };
            match constraint.pin.as_deref() {
                None => constraint.pin = Some(release.clone()),
                Some(existing) if existing == release => {}
                Some(existing) => {
                    conflicts.entry(project_id.clone()).or_insert_with(|| {
                        ModPlatformError::ConflictingPinnedVersions {
                            project_id: project_id.clone(),
                            first: existing.to_owned(),
                            first_source: join(&constraint.pin_sources),
                            second: release.clone(),
                            second_source: String::new(),
                        }
                    });
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

    /// [`Self::select_version`], remembering a successful answer so that a
    /// project resolved the same way again is not looked up again.
    async fn selected_version(
        &mut self,
        project_id: &str,
        pin: Option<&str>,
        declarers: &BTreeSet<String>,
    ) -> ModPlatformResult<ModVersionInfo> {
        let key = (project_id.to_owned(), pin.map(str::to_owned));
        if let Some(version) = self.selected.get(&key) {
            return Ok(version.clone());
        }
        let refusal_key = (key.0.clone(), key.1.clone(), declarers.clone());
        if let Some(refusal) = self
            .selection_refusals
            .get(&refusal_key)
            .and_then(copy_refusal)
        {
            return Err(refusal);
        }
        if let (true, Some(pin)) = (declarers.is_empty(), pin) {
            if let Some(version) = self
                .listed_pins
                .get(&(project_id.to_owned(), pin.to_owned()))
            {
                return Ok(version.clone());
            }
        }
        match self.select_version(project_id, pin, declarers).await {
            Ok(version) => {
                self.selected.insert(key, version.clone());
                Ok(version)
            }
            Err(error) => {
                if let Some(copy) = copy_refusal(&error) {
                    self.selection_refusals.insert(refusal_key, copy);
                }
                Err(error)
            }
        }
    }

    /// [`Self::normalize_all`], remembering a successful answer for the release.
    async fn relations_of(
        &mut self,
        version: &ModVersionInfo,
    ) -> ModPlatformResult<Vec<Normalized>> {
        let key = (version.project_id.clone(), version.id.clone());
        if let Some(relations) = self.normalized.get(&key) {
            return Ok(relations.clone());
        }
        let relations = self.normalize_all(version).await?;
        self.normalized.insert(key, relations.clone());
        Ok(relations)
    }

    /// Picks the version one project resolves to. `declarers` are the sources
    /// of `pin`, so a refusal of the pin can name who asked for it.
    async fn select_version(
        &mut self,
        project_id: &str,
        pin: Option<&str>,
        declarers: &BTreeSet<String>,
    ) -> ModPlatformResult<ModVersionInfo> {
        let target = self.request.target;

        if let Some(version_id) = pin {
            let version = self.fetch_version(version_id).await?;
            if version.project_id != project_id {
                return Err(inconsistent_pin(project_id, version.project_id, declarers));
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

        let versions = match self.listed_releases(project_id).await? {
            Some(listed) => listed,
            None => match self.listing_refusals.get(project_id).and_then(copy_refusal) {
                Some(refusal) => return Err(refusal),
                None => self.versions(project_id).await?,
            },
        };
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
        let Some((installed, _)) = self.recorded(project_id) else {
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
        // would otherwise spend a rate-limit budget on round trips. An optional
        // extra is looked up apart from the rest, because failing to judge it
        // must not fail the install. A selection that such a relation might
        // have offered is refused once the graph has settled.
        let (optional_pins, other_pins): (Vec<&ModDependency>, Vec<&ModDependency>) = version
            .dependencies
            .iter()
            .filter(|dependency| dependency.pins_only_a_version())
            .partition(|dependency| dependency.kind == ModRelationKind::Optional);
        let (required_pins, inert_pins): (Vec<&ModDependency>, Vec<&ModDependency>) = other_pins
            .into_iter()
            .partition(|dependency| dependency.kind == ModRelationKind::Required);
        let pinned: Vec<String> = required_pins
            .iter()
            .filter_map(|dependency| dependency.version_id.clone())
            .collect();
        let mut looked_up = if pinned.is_empty() {
            Vec::new()
        } else {
            self.request
                .provider
                .versions_by_id(&pinned, self.request.reporter, self.request.cancel)
                .await?
        };
        // An embedded or incompatible relation installs nothing, so a release
        // the provider refuses to describe must not refuse the plan.
        let inert_pinned: Vec<String> = inert_pins
            .iter()
            .filter_map(|dependency| dependency.version_id.clone())
            .filter(|version_id| !pinned.contains(version_id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        looked_up.extend(self.lookup_owners(&inert_pinned).await?);
        let optional_pinned: BTreeSet<String> = optional_pins
            .iter()
            .filter_map(|dependency| dependency.version_id.clone())
            .filter(|version_id| !pinned.contains(version_id))
            .collect();
        let (optional_found, optional_failed) =
            self.lookup_optional_owners(&optional_pinned).await?;
        looked_up.extend(optional_found);

        self.pinned_owners.extend(
            looked_up
                .iter()
                .map(|looked| (looked.id.clone(), looked.project_id.clone())),
        );

        // An embedded or incompatible relation that names both a project and a
        // version needs the version's owner to confirm they agree. Required
        // relations are checked when their release is selected, and an optional
        // one when it is offered, where a failed lookup is reported instead of
        // failing the install.
        let named: Vec<String> = version
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.project_id.is_some()
                    && matches!(
                        dependency.kind,
                        ModRelationKind::Embedded | ModRelationKind::Incompatible
                    )
            })
            .filter_map(|dependency| dependency.version_id.clone())
            .collect();
        looked_up.extend(self.lookup_owners(&named).await?);

        version
            .dependencies
            .iter()
            .map(|dependency| {
                let unverified = dependency
                    .version_id
                    .as_ref()
                    .filter(|_| {
                        dependency.pins_only_a_version()
                            && dependency.kind == ModRelationKind::Optional
                    })
                    .and_then(|version_id| {
                        optional_failed.get(version_id).map(|u| (version_id, u))
                    });
                match unverified {
                    Some((version_id, unverified)) => Ok(Normalized::UnverifiedRelease {
                        version_id: version_id.clone(),
                        unverified: unverified.clone(),
                    }),
                    None => normalize(dependency, &version.project_id, &looked_up),
                }
            })
            .collect()
    }

    /// Looks up the releases that optional relations pin without naming their
    /// project, to learn which project each belongs to.
    ///
    /// A release that cannot be looked up is reported with the reason instead
    /// of failing the plan: an optional extra that cannot be judged is a fact to
    /// display. A failed batch is retried one release at a time, so one bad
    /// release does not hide the owners of the others. Canceling stops the plan.
    async fn lookup_optional_owners(
        &self,
        version_ids: &BTreeSet<String>,
    ) -> ModPlatformResult<(Vec<ModVersionInfo>, BTreeMap<String, UnverifiedOptional>)> {
        let mut found = Vec::new();
        let mut failed = BTreeMap::new();
        if version_ids.is_empty() {
            return Ok((found, failed));
        }
        let requested: Vec<String> = version_ids.iter().cloned().collect();
        let batch = self
            .request
            .provider
            .versions_by_id(&requested, self.request.reporter, self.request.cancel)
            .await;
        match batch {
            Ok(versions) => found = versions,
            Err(error) => {
                self.unverified(&error)?;
                for version_id in &requested {
                    match self.fetch_version(version_id).await {
                        Ok(version) => found.push(version),
                        Err(error) => {
                            failed.insert(version_id.clone(), self.unverified(&error)?);
                        }
                    }
                }
            }
        }
        // A provider that answered without a release does not know it.
        for version_id in &requested {
            if !failed.contains_key(version_id) && !found.iter().any(|v| &v.id == version_id) {
                failed.insert(version_id.clone(), UnverifiedOptional::UnknownIdentity);
            }
        }
        Ok((found, failed))
    }

    /// Looks up releases only to learn which project owns them.
    ///
    /// A release that cannot be installed (no usable hash, no single primary
    /// file, unknown to the provider) still has an owner, and the relation that
    /// names it is not an installation, so it is left unchecked rather than
    /// refusing the plan. The batch is retried one release at a time when
    /// such a release hides the owners of the others.
    async fn lookup_owners(
        &self,
        version_ids: &[String],
    ) -> ModPlatformResult<Vec<ModVersionInfo>> {
        if version_ids.is_empty() {
            return Ok(Vec::new());
        }
        let batch = self
            .request
            .provider
            .versions_by_id(version_ids, self.request.reporter, self.request.cancel)
            .await;
        match batch {
            Err(error) if is_release_content_refusal(&error) => {
                let mut found = Vec::new();
                for version_id in version_ids {
                    match self.fetch_version(version_id).await {
                        Ok(version) => found.push(version),
                        Err(error) if is_release_content_refusal(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(found)
            }
            other => other,
        }
    }

    /// A project's candidate versions, limited to releases that project owns.
    async fn versions(&self, project_id: &str) -> ModPlatformResult<Vec<ModVersionInfo>> {
        self.request
            .provider
            .project_versions(
                project_id,
                self.request.target,
                self.request.reporter,
                self.request.cancel,
            )
            .await
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
        self.refuse_unverifiable_selection().await?;

        for project in &self.request.selection.optional {
            if !self.honored_optional.contains(project) {
                self.warnings.push(PlanWarning::OptionalNotOffered {
                    project_id: project.clone(),
                });
            }
        }

        let projects = self.fetch_projects().await?;
        self.name_optional().await?;
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
            let mut version = resolution.version.clone();
            for dependency in &mut version.dependencies {
                dependency.name_pinned_project(&self.pinned_owners);
            }
            let planned = PlannedMod {
                project,
                version,
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
            optional: self
                .optional
                .values()
                .chain(self.unowned_optional.values())
                .cloned()
                .collect(),
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
                            .await?;
                    }
                    Normalized::Project {
                        kind: ModRelationKind::Embedded,
                        project_id,
                        ..
                    } => self.embedded.push(EmbeddedNote {
                        project_id: Some(project_id),
                        version_id: None,
                        file_name: None,
                        declared_by: source.clone(),
                    }),
                    Normalized::External { kind, file_name } => {
                        self.record_external(&source, kind, file_name)?;
                    }
                    Normalized::UnverifiedRelease {
                        version_id,
                        unverified,
                    } => {
                        self.unowned_optional
                            .entry(version_id.clone())
                            .or_insert_with(|| OptionalCandidate {
                                project_id: None,
                                title: None,
                                pinned_version_id: Some(version_id),
                                declared_by: source.clone(),
                                version: None,
                                status: OptionalStatus::Unverified(unverified),
                            });
                    }
                    Normalized::UnidentifiedRelease {
                        kind: ModRelationKind::Embedded,
                        version_id,
                    } => self.embedded.push(EmbeddedNote {
                        project_id: None,
                        version_id: Some(version_id),
                        file_name: None,
                        declared_by: source.clone(),
                    }),
                    // Required relations are edges, and incompatible ones are
                    // checked against the settled graph rather than listed.
                    Normalized::Project { .. } | Normalized::UnidentifiedRelease { .. } => {}
                }
            }
        }
        Ok(())
    }

    /// Refuses a selected optional that no relation honored while a relation
    /// that pins only a release could not say which project it offers.
    ///
    /// The selection might have been that relation, and dropping it behind a
    /// "nothing offers it" warning would misreport why. The lookup is repeated
    /// strictly to give the plan the failure itself, and a lookup that now
    /// succeeds for a selected project's release still refuses the plan.
    async fn refuse_unverifiable_selection(&self) -> ModPlatformResult<()> {
        let dropped = self
            .request
            .selection
            .optional
            .iter()
            .any(|project| !self.honored_optional.contains(project));
        if !dropped {
            return Ok(());
        }
        for version_id in self.unowned_optional.keys() {
            let release = self.fetch_version(version_id).await?;
            // The earlier failure was momentary, but the plan was built
            // without this release, so it cannot be trusted to include it.
            if self
                .request
                .selection
                .optional
                .contains(&release.project_id)
            {
                return Err(ModPlatformError::RequestFailed {
                    provider: self.request.provider.provider(),
                    detail: format!(
                        "the release `{version_id}` could not be looked up while the plan was built"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Describes an optional dependency the user has not taken.
    ///
    /// Its version is resolved so the offer can say what would be installed and
    /// whether it would even fit - but a failure to resolve is not fatal: an
    /// optional extra that cannot be judged is a fact to display, not a reason
    /// to refuse the install the user actually asked for. The one failure that
    /// does stop the plan is the user canceling it.
    async fn record_optional(
        &mut self,
        source: &str,
        project_id: String,
        pinned_version: Option<String>,
    ) -> ModPlatformResult<()> {
        if self.optional.contains_key(&project_id) {
            return Ok(());
        }
        let outcome = match &pinned_version {
            Some(version_id) => match self.fetch_version(version_id).await {
                Ok(version) if version.project_id == project_id => Ok(Some(version)),
                Ok(version) => Err(UnverifiedOptional::ConflictingIdentity {
                    version_project_id: version.project_id,
                }),
                Err(error) => Err(self.unverified(&error)?),
            },
            None => match self.versions(&project_id).await {
                Ok(versions) => Ok(newest_compatible(
                    &versions,
                    self.request.target,
                    self.request.loader_name,
                )),
                Err(error) => Err(self.unverified(&error)?),
            },
        };
        let (resolved, status) = match outcome {
            Ok(Some(version)) => {
                let status = OptionalStatus::from(
                    version.compatibility(self.request.target, self.request.loader_name),
                );
                (Some(version), status)
            }
            Ok(None) => (
                None,
                OptionalStatus::Incompatible(vec![
                    crate::model::IncompatibilityReason::MinecraftVersion {
                        required: self.request.target.minecraft_version.clone(),
                        supported: Vec::new(),
                    },
                ]),
            ),
            Err(unverified) => (None, OptionalStatus::Unverified(unverified)),
        };

        self.optional.insert(
            project_id.clone(),
            OptionalCandidate {
                project_id: Some(project_id),
                title: None,
                pinned_version_id: pinned_version,
                declared_by: source.to_owned(),
                version: resolved.filter(|_| status.is_compatible()),
                status,
            },
        );
        Ok(())
    }

    /// Classifies a failed lookup for an optional dependency, or stops the plan
    /// when the lookup failed because the user canceled it.
    ///
    /// The token is consulted as well as the error: a provider may report a
    /// canceled request as an ordinary failure.
    fn unverified(&self, error: &ModPlatformError) -> ModPlatformResult<UnverifiedOptional> {
        if error.is_cancellation() || self.request.cancel.is_cancelled() {
            return Err(ModPlatformError::Canceled);
        }
        Ok(match error {
            ModPlatformError::ProjectUnknown { .. } | ModPlatformError::VersionUnknown { .. } => {
                UnverifiedOptional::UnknownIdentity
            }
            _ => UnverifiedOptional::Unavailable,
        })
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
                version_id: None,
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
                let conflicting = match relation {
                    Normalized::Project {
                        kind: ModRelationKind::Incompatible,
                        project_id,
                        pinned_version,
                    } => self
                        .is_present(project_id, pinned_version.as_deref())
                        .then(|| project_id.clone()),
                    Normalized::UnidentifiedRelease {
                        kind: ModRelationKind::Incompatible,
                        version_id,
                    } => self.owner_of_present_release(version_id),
                    _ => None,
                };
                if let Some(conflicting) = conflicting {
                    return Err(ModPlatformError::IncompatibleDependency {
                        declared_by: source.clone(),
                        conflicting,
                    });
                }
            }
        }
        Ok(())
    }

    /// The project whose release `version_id` the finished plan leaves on the
    /// instance, for a relation that pinned the release without naming a project.
    ///
    /// Release ids are unique to the provider, so matching one needs no owner.
    fn owner_of_present_release(&self, version_id: &str) -> Option<String> {
        self.resolved
            .iter()
            .find(|(_, resolution)| resolution.version.id == version_id)
            .map(|(project_id, _)| project_id.clone())
            .or_else(|| {
                self.request
                    .installed
                    .iter()
                    .find(|entry| {
                        entry.version_id == version_id
                            && !self.resolved.contains_key(&entry.project_id)
                    })
                    .map(|entry| entry.project_id.clone())
            })
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

    /// The desired state's entry for a project, and whether it is on this
    /// instance's side.
    fn recorded(&self, project_id: &str) -> Option<(&InstalledProject, bool)> {
        let find = |entries: &'a [InstalledProject]| {
            entries.iter().find(|entry| entry.project_id == project_id)
        };
        find(self.request.installed)
            .map(|entry| (entry, true))
            .or_else(|| find(self.request.recorded_elsewhere).map(|entry| (entry, false)))
    }

    /// Splits the resolved set into "already there" and "would be installed",
    /// refusing a version change.
    fn already_installed(
        &self,
    ) -> ModPlatformResult<(Vec<InstalledProject>, Vec<ProjectReplacement>)> {
        let mut present = Vec::new();
        let mut replacements = Vec::new();
        for (project_id, resolution) in &self.resolved {
            let Some((installed, here)) = self.recorded(project_id) else {
                continue;
            };
            if installed.version_id == resolution.version.id {
                // Recorded only for the other side, the release is the right
                // one but its jar is not here, so it is installed.
                if here {
                    present.push(installed.clone());
                }
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

    /// Gives each optional offer its project's title.
    ///
    /// A lenient lookup of its own rather than part of the resolved projects'
    /// strict one: an offer is not installed, so a project the catalog does not
    /// describe keeps its id instead of failing the plan. Only canceling stops
    /// it.
    async fn name_optional(&mut self) -> ModPlatformResult<()> {
        let ids: Vec<String> = self.optional.keys().cloned().collect();
        if ids.is_empty() {
            return Ok(());
        }
        match self
            .request
            .provider
            .project_batch_by_id(&ids, self.request.reporter, self.request.cancel)
            .await
        {
            Ok(batch) => {
                for project in batch.mods {
                    if let Some(candidate) = self.optional.get_mut(&project.id) {
                        candidate.title = Some(project.title).filter(|title| !title.is_empty());
                    }
                }
            }
            Err(error) if error.is_cancellation() || self.request.cancel.is_cancelled() => {
                return Err(ModPlatformError::Canceled);
            }
            Err(error) => debug!(%error, "optional dependencies keep their ids"),
        }
        Ok(())
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
            let Some(version) = looked_up
                .iter()
                .find(|candidate| candidate.id == version_id)
            else {
                return match dependency.kind {
                    ModRelationKind::Embedded | ModRelationKind::Incompatible => {
                        Ok(Normalized::UnidentifiedRelease {
                            kind: dependency.kind,
                            version_id: version_id.to_owned(),
                        })
                    }
                    _ => Err(ModPlatformError::VersionUnknown {
                        provider: ModProvider::Modrinth,
                        version_id: version_id.to_owned(),
                    }),
                };
            };
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
    use crate::test_support::{at, compatible, needs, needs_file, pins, pins_of, project, version};

    /// A catalog held in memory, so a resolver test states the graph it is
    /// about rather than a fixture directory.
    #[derive(Debug, Default)]
    struct Catalog {
        versions: BTreeMap<String, Vec<ModVersionInfo>>,
        /// What `versions_by_id` answers with whatever was asked, for a
        /// provider that returns something other than the requested ids.
        by_id_response: Option<Vec<ModVersionInfo>>,
        /// Lookups for these ids (project listings and by-id version lookups)
        /// fail with the error built by `failure` instead of answering.
        failing_ids: Vec<String>,
        failure: Option<fn() -> ModPlatformError>,
        /// Canceled when a lookup fails, for a provider that reports a canceled
        /// request as an ordinary failure.
        cancel_on_failure: Option<CancellationToken>,
        /// When set, only this many failing lookups happen before the ids
        /// answer normally, for a failure that clears up.
        failures_before_recovery: Option<std::sync::atomic::AtomicUsize>,
        /// How many times the resolver asked for versions, to show a refusal
        /// did not cost a lookup per round.
        lookups: std::sync::atomic::AtomicUsize,
        /// Every project whose releases were listed.
        asked: std::sync::Mutex<BTreeSet<String>>,
    }

    impl Catalog {
        fn lookups(&self) -> usize {
            self.lookups.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn failing_for(mut self, failure: fn() -> ModPlatformError, ids: &[&str]) -> Self {
            self.failure = Some(failure);
            self.failing_ids = ids.iter().map(|id| (*id).to_owned()).collect();
            self
        }

        fn failure_for(&self, ids: &[&str]) -> Option<ModPlatformError> {
            let failure = self.failure?;
            ids.iter()
                .any(|id| self.failing_ids.iter().any(|failing| failing == id))
                .then_some(())
                .filter(|()| {
                    self.failures_before_recovery.as_ref().is_none_or(|left| {
                        left.try_update(
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                            |left| left.checked_sub(1),
                        )
                        .is_ok()
                    })
                })
                .map(|()| {
                    if let Some(token) = &self.cancel_on_failure {
                        token.cancel();
                    }
                    failure()
                })
        }

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
                failing_ids: self.failing_ids.clone(),
                failure: self.failure,
                cancel_on_failure: self.cancel_on_failure.clone(),
                failures_before_recovery: self.failures_before_recovery.as_ref().map(|left| {
                    std::sync::atomic::AtomicUsize::new(
                        left.load(std::sync::atomic::Ordering::SeqCst),
                    )
                }),
                lookups: std::sync::atomic::AtomicUsize::new(0),
                asked: std::sync::Mutex::default(),
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
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.asked
                .lock()
                .expect("not poisoned")
                .insert(project_id.to_owned());
            if let Some(error) = self.failure_for(&[project_id]) {
                return Err(error);
            }
            self.versions
                .get(project_id)
                .cloned()
                .ok_or_else(|| ModPlatformError::ProjectUnknown {
                    provider: ModProvider::Modrinth,
                    reference: project_id.to_owned(),
                })
        }

        async fn project_batch_by_id(
            &self,
            ids: &[String],
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<crate::model::ProjectBatch> {
            Ok(crate::model::ProjectBatch {
                mods: ids.iter().map(|id| project(id)).collect(),
                not_mods: Vec::new(),
            })
        }

        async fn versions_by_id(
            &self,
            ids: &[String],
            _reporter: &OperationReporter,
            _cancel: &CancellationToken,
        ) -> ModPlatformResult<Vec<ModVersionInfo>> {
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let requested: Vec<&str> = ids.iter().map(String::as_str).collect();
            if let Some(error) = self.failure_for(&requested) {
                return Err(error);
            }
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
        plan_recording(catalog, selection, installed, &[], installed_policy).await
    }

    async fn plan_recording(
        catalog: &Catalog,
        selection: InstallSelection,
        installed: &[InstalledProject],
        recorded_elsewhere: &[InstalledProject],
        installed_policy: InstalledPolicy,
    ) -> ModPlatformResult<ModInstallPlan> {
        resolve(ResolveRequest {
            provider: catalog,
            target: &target(),
            loader_name: "neoforge",
            selection: &selection,
            installed,
            recorded_elsewhere,
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
        assert_eq!(plan.optional[0].project_id.as_deref(), Some("EXTRA"));
        assert!(plan.optional[0].status.is_compatible());
        assert!(
            plan.optional[0].version.is_some(),
            "the offer says what would be installed if it were taken"
        );
    }

    fn optional_of(root: ModVersionInfo) -> Catalog {
        Catalog::default().with("ROOT", vec![root])
    }

    fn unreachable() -> ModPlatformError {
        ModPlatformError::RequestFailed {
            provider: ModProvider::Modrinth,
            detail: "connection reset".to_owned(),
        }
    }

    fn rejected() -> ModPlatformError {
        ModPlatformError::CredentialRejected {
            provider: ModProvider::Modrinth,
            status: 401,
        }
    }

    fn canceled() -> ModPlatformError {
        ModPlatformError::Canceled
    }

    fn optional_status(plan: &ModInstallPlan) -> &OptionalStatus {
        assert_eq!(plan.optional.len(), 1, "the optional dependency is offered");
        &plan.optional[0].status
    }

    #[tokio::test]
    async fn an_optional_dependency_whose_listing_cannot_be_fetched_is_unverified_not_incompatible()
    {
        for failure in [unreachable as fn() -> ModPlatformError, rejected] {
            let catalog = optional_of(needs(
                compatible("v1", "ROOT"),
                ModRelationKind::Optional,
                "EXTRA",
            ))
            .with("EXTRA", vec![compatible("v1", "EXTRA")])
            .failing_for(failure, &["EXTRA"]);

            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                optional_status(&plan),
                &OptionalStatus::Unverified(UnverifiedOptional::Unavailable)
            );
            assert!(plan.optional[0].version.is_none());
        }
    }

    #[tokio::test]
    async fn an_optional_dependency_pinning_an_unfetchable_version_is_unverified() {
        let catalog = optional_of(pins_of(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "EXTRA",
            "extra-v1",
        ))
        .with("EXTRA", vec![compatible("extra-v1", "EXTRA")])
        .failing_for(unreachable, &["extra-v1"]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            optional_status(&plan),
            &OptionalStatus::Unverified(UnverifiedOptional::Unavailable)
        );
    }

    #[tokio::test]
    async fn an_optional_dependency_the_provider_does_not_know_is_an_unknown_identity() {
        let unknown_project = optional_of(needs(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "GHOST",
        ));
        let plan_of_project = plan(&unknown_project, InstallSelection::new("ROOT")).await;
        assert_eq!(
            optional_status(&plan_of_project),
            &OptionalStatus::Unverified(UnverifiedOptional::UnknownIdentity)
        );

        let unknown_pinned_version = optional_of(pins_of(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "EXTRA",
            "ghost-v1",
        ))
        .with("EXTRA", vec![compatible("extra-v1", "EXTRA")]);
        let plan_of_version = plan(&unknown_pinned_version, InstallSelection::new("ROOT")).await;
        assert_eq!(
            optional_status(&plan_of_version),
            &OptionalStatus::Unverified(UnverifiedOptional::UnknownIdentity)
        );
    }

    #[tokio::test]
    async fn an_optional_dependency_with_no_release_for_the_instance_is_incompatible() {
        let catalog = optional_of(needs(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "EXTRA",
        ))
        .with(
            "EXTRA",
            vec![version("v1", "EXTRA", &["1.21.1"], &["neoforge"])],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert!(matches!(
            optional_status(&plan),
            OptionalStatus::Incompatible(reasons) if !reasons.is_empty()
        ));
    }

    #[tokio::test]
    async fn an_optional_dependency_pinning_a_release_of_another_project_is_a_conflicting_identity()
    {
        let catalog = optional_of(pins_of(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "EXTRA",
            "other-v1",
        ))
        .with("EXTRA", vec![compatible("extra-v1", "EXTRA")])
        .with("OTHER", vec![compatible("other-v1", "OTHER")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            optional_status(&plan),
            &OptionalStatus::Unverified(UnverifiedOptional::ConflictingIdentity {
                version_project_id: "OTHER".to_owned()
            })
        );
        assert!(
            plan.optional[0].version.is_none(),
            "a release of another project is never offered as this one"
        );
    }

    #[tokio::test]
    async fn a_canceled_optional_dependency_lookup_stops_the_plan() {
        for pinned in [false, true] {
            let root = if pinned {
                pins_of(
                    compatible("v1", "ROOT"),
                    ModRelationKind::Optional,
                    "EXTRA",
                    "extra-v1",
                )
            } else {
                needs(compatible("v1", "ROOT"), ModRelationKind::Optional, "EXTRA")
            };
            let catalog = optional_of(root)
                .with("EXTRA", vec![compatible("extra-v1", "EXTRA")])
                .failing_for(canceled, &["EXTRA", "extra-v1"]);

            let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
                .await
                .expect_err("a canceled lookup is not an offer");
            assert!(error.is_cancellation(), "pinned: {pinned}");
        }
    }

    #[tokio::test]
    async fn a_lookup_failure_reported_after_the_user_canceled_stops_the_plan() {
        let token = CancellationToken::new();
        let mut catalog = optional_of(needs(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "EXTRA",
        ))
        .with("EXTRA", vec![compatible("v1", "EXTRA")])
        .failing_for(unreachable, &["EXTRA"]);
        catalog.cancel_on_failure = Some(token.clone());

        let error = resolve(ResolveRequest {
            provider: &catalog,
            target: &target(),
            loader_name: "neoforge",
            selection: &InstallSelection::new("ROOT"),
            installed: &[],
            recorded_elsewhere: &[],
            installed_policy: InstalledPolicy::Preserve,
            reporter: &OperationReporter::silent("resolving-mods"),
            cancel: &token,
        })
        .await
        .expect_err("a plan abandoned by the user is not a plan");
        assert!(error.is_cancellation());
    }

    fn version_only_optional(catalog_extra: &[(&str, &str)]) -> Catalog {
        let mut catalog = optional_of(pins(
            compatible("v1", "ROOT"),
            ModRelationKind::Optional,
            "extra-v1",
        ));
        for (project_id, version_id) in catalog_extra {
            catalog = catalog.with(project_id, vec![compatible(version_id, project_id)]);
        }
        catalog
    }

    #[tokio::test]
    async fn an_optional_dependency_pinning_only_a_version_that_cannot_be_looked_up_is_unverified()
    {
        for failure in [
            unreachable as fn() -> ModPlatformError,
            rejected,
            hashes_missing,
        ] {
            let catalog =
                version_only_optional(&[("EXTRA", "extra-v1")]).failing_for(failure, &["extra-v1"]);

            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(plan.root.version.id, "v1");
            assert_eq!(
                optional_status(&plan),
                &OptionalStatus::Unverified(UnverifiedOptional::Unavailable)
            );
            assert_eq!(plan.optional[0].project_id, None);
            assert_eq!(
                plan.optional[0].pinned_version_id.as_deref(),
                Some("extra-v1")
            );
            assert!(plan.optional[0].version.is_none());
        }
    }

    #[tokio::test]
    async fn an_optional_dependency_pinning_only_a_version_the_provider_does_not_know_is_an_unknown_identity()
     {
        let catalog = version_only_optional(&[]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(plan.root.version.id, "v1");
        assert_eq!(
            optional_status(&plan),
            &OptionalStatus::Unverified(UnverifiedOptional::UnknownIdentity)
        );
    }

    #[tokio::test]
    async fn an_optional_dependency_pinning_only_a_version_is_offered_with_its_project() {
        let catalog = version_only_optional(&[("EXTRA", "extra-v1")]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(plan.optional.len(), 1);
        assert_eq!(plan.optional[0].project_id.as_deref(), Some("EXTRA"));
        assert!(plan.optional[0].status.is_compatible());
    }

    #[tokio::test]
    async fn one_unavailable_optional_release_does_not_hide_the_projects_of_the_others() {
        let root = pins(
            pins(
                compatible("v1", "ROOT"),
                ModRelationKind::Optional,
                "bad-v1",
            ),
            ModRelationKind::Optional,
            "good-v1",
        );
        let catalog = optional_of(root)
            .with("BAD", vec![compatible("bad-v1", "BAD")])
            .with("GOOD", vec![compatible("good-v1", "GOOD")])
            .failing_for(hashes_missing, &["bad-v1"]);

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(plan.optional.len(), 2);
        let good = plan
            .optional
            .iter()
            .find(|candidate| candidate.project_id.as_deref() == Some("GOOD"))
            .expect("the release that could be looked up names its project");
        assert!(good.status.is_compatible());
        assert!(plan.optional.iter().any(|candidate| {
            candidate.project_id.is_none()
                && candidate.pinned_version_id.as_deref() == Some("bad-v1")
                && candidate.status == OptionalStatus::Unverified(UnverifiedOptional::Unavailable)
        }));
    }

    #[tokio::test]
    async fn a_selected_optional_dependency_pinning_only_a_version_is_installed() {
        let catalog = version_only_optional(&[("EXTRA", "extra-v1")]);

        let plan = plan(
            &catalog,
            InstallSelection::new("ROOT").selecting_optional(["EXTRA".to_owned()]),
        )
        .await;
        assert_eq!(ids(&plan.dependencies), vec!["EXTRA"]);
        assert!(plan.optional.is_empty());
    }

    #[tokio::test]
    async fn a_selection_a_failed_version_only_optional_might_have_offered_fails_the_plan() {
        let selection = || InstallSelection::new("ROOT").selecting_optional(["EXTRA".to_owned()]);
        for failure in [
            unreachable as fn() -> ModPlatformError,
            rejected,
            hashes_missing,
        ] {
            let catalog =
                version_only_optional(&[("EXTRA", "extra-v1")]).failing_for(failure, &["extra-v1"]);
            plan_with(&catalog, selection(), &[])
                .await
                .expect_err("a selected optional is never dropped behind an offer");
        }

        let unknown = version_only_optional(&[]);
        plan_with(&unknown, selection(), &[])
            .await
            .expect_err("a selected optional the provider does not know is refused");
    }

    #[tokio::test]
    async fn a_selection_is_refused_when_its_release_recovers_after_the_plan_was_built() {
        // The batch and the single-release retry fail, then the strict lookup
        // made once the graph has settled succeeds.
        let mut catalog =
            version_only_optional(&[("EXTRA", "extra-v1")]).failing_for(unreachable, &["extra-v1"]);
        catalog.failures_before_recovery = Some(std::sync::atomic::AtomicUsize::new(2));

        let error = plan_with(
            &catalog,
            InstallSelection::new("ROOT").selecting_optional(["EXTRA".to_owned()]),
            &[],
        )
        .await
        .expect_err("a selection must not be dropped behind a warning");
        assert!(matches!(error, ModPlatformError::RequestFailed { .. }));
    }

    #[tokio::test]
    async fn a_failed_version_only_optional_does_not_fail_a_plan_whose_selection_is_honored() {
        let root = needs(
            pins(
                compatible("v1", "ROOT"),
                ModRelationKind::Optional,
                "extra-v1",
            ),
            ModRelationKind::Optional,
            "OTHER",
        );
        let catalog = optional_of(root)
            .with("OTHER", vec![compatible("other-v1", "OTHER")])
            .with("EXTRA", vec![compatible("extra-v1", "EXTRA")])
            .failing_for(unreachable, &["extra-v1"]);

        let plan = plan(
            &catalog,
            InstallSelection::new("ROOT").selecting_optional(["OTHER".to_owned()]),
        )
        .await;
        assert_eq!(ids(&plan.dependencies), vec!["OTHER"]);
        assert_eq!(
            optional_status(&plan),
            &OptionalStatus::Unverified(UnverifiedOptional::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_required_dependency_pinning_only_a_version_that_cannot_be_looked_up_fails_the_plan()
    {
        for failure in [
            unreachable as fn() -> ModPlatformError,
            rejected,
            hashes_missing,
        ] {
            let catalog = optional_of(pins(
                compatible("v1", "ROOT"),
                ModRelationKind::Required,
                "lib-v1",
            ))
            .with("LIB", vec![compatible("lib-v1", "LIB")])
            .failing_for(failure, &["lib-v1"]);

            plan_with(&catalog, InstallSelection::new("ROOT"), &[])
                .await
                .expect_err("a required relation that cannot be resolved is not optional");
        }
    }

    #[tokio::test]
    async fn a_canceled_lookup_of_an_optional_release_pinned_without_a_project_stops_the_plan() {
        let catalog =
            version_only_optional(&[("EXTRA", "extra-v1")]).failing_for(canceled, &["extra-v1"]);
        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("a canceled lookup is not an offer");
        assert!(error.is_cancellation());

        let token = CancellationToken::new();
        let mut catalog =
            version_only_optional(&[("EXTRA", "extra-v1")]).failing_for(unreachable, &["extra-v1"]);
        catalog.cancel_on_failure = Some(token.clone());
        let error = resolve(ResolveRequest {
            provider: &catalog,
            target: &target(),
            loader_name: "neoforge",
            selection: &InstallSelection::new("ROOT"),
            installed: &[],
            recorded_elsewhere: &[],
            installed_policy: InstalledPolicy::Preserve,
            reporter: &OperationReporter::silent("resolving-mods"),
            cancel: &token,
        })
        .await
        .expect_err("a plan abandoned by the user is not a plan");
        assert!(error.is_cancellation());
    }

    #[tokio::test]
    async fn embedded_and_incompatible_relations_naming_disagreeing_identities_are_refused() {
        for kind in [ModRelationKind::Embedded, ModRelationKind::Incompatible] {
            let catalog = optional_of(pins_of(compatible("v1", "ROOT"), kind, "LIB", "other-v1"))
                .with("LIB", vec![compatible("lib-v1", "LIB")])
                .with("OTHER", vec![compatible("other-v1", "OTHER")]);

            let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
                .await
                .expect_err("must refuse");
            assert!(
                matches!(error, ModPlatformError::InconsistentDependency { .. }),
                "{kind:?}: {error:?}"
            );
        }
    }

    fn hashes_missing() -> ModPlatformError {
        ModPlatformError::HashesMissing {
            version_id: "lib-v1".to_owned(),
        }
    }

    #[tokio::test]
    async fn an_unusable_release_named_by_an_embedded_or_incompatible_relation_does_not_refuse_the_plan()
     {
        for kind in [ModRelationKind::Embedded, ModRelationKind::Incompatible] {
            let catalog = optional_of(pins_of(compatible("v1", "ROOT"), kind, "LIB", "lib-v1"))
                .with("LIB", vec![compatible("lib-v1", "LIB")])
                .failing_for(hashes_missing, &["lib-v1"]);

            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(plan.root.version.id, "v1", "{kind:?}");
        }
    }

    fn primary_file_ambiguous() -> ModPlatformError {
        ModPlatformError::AmbiguousPrimaryFile {
            version_id: "lib-v1".to_owned(),
            found: 2,
        }
    }

    fn file_unavailable() -> ModPlatformError {
        ModPlatformError::FileUnavailable {
            provider: ModProvider::Modrinth,
            project_id: "LIB".to_owned(),
            file_id: "lib-file".to_owned(),
        }
    }

    #[tokio::test]
    async fn an_unusable_release_pinned_without_a_project_by_an_embedded_or_incompatible_relation_does_not_refuse_the_plan()
     {
        for failure in [
            hashes_missing as fn() -> ModPlatformError,
            primary_file_ambiguous,
            file_unavailable,
        ] {
            for kind in [ModRelationKind::Embedded, ModRelationKind::Incompatible] {
                let catalog = optional_of(pins(compatible("v1", "ROOT"), kind, "lib-v1"))
                    .with("LIB", vec![compatible("lib-v1", "LIB")])
                    .failing_for(failure, &["lib-v1"]);

                let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
                assert_eq!(plan.root.version.id, "v1", "{kind:?}");
                assert_eq!(
                    plan.embedded.len(),
                    usize::from(kind == ModRelationKind::Embedded),
                    "{kind:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_failed_lookup_of_a_release_pinned_without_a_project_by_an_embedded_relation_is_not_a_refusal_of_the_release()
     {
        let catalog = optional_of(pins(
            compatible("v1", "ROOT"),
            ModRelationKind::Embedded,
            "lib-v1",
        ))
        .with("LIB", vec![compatible("lib-v1", "LIB")])
        .failing_for(unreachable, &["lib-v1"]);

        plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("a provider that cannot answer says nothing about the release");
    }

    #[tokio::test]
    async fn a_release_pinned_without_a_project_that_the_catalog_does_not_know_is_noted_when_embedded()
     {
        let catalog = optional_of(pins(
            compatible("v1", "ROOT"),
            ModRelationKind::Embedded,
            "gone-v1",
        ));

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            plan.embedded,
            vec![EmbeddedNote {
                project_id: None,
                version_id: Some("gone-v1".to_owned()),
                file_name: None,
                declared_by: "ROOT".to_owned(),
            }]
        );

        let incompatible = optional_of(pins(
            compatible("v1", "ROOT"),
            ModRelationKind::Incompatible,
            "gone-v1",
        ));
        let plan = plan_with(&incompatible, InstallSelection::new("ROOT"), &[])
            .await
            .expect("an incompatibility with an unknown release cannot be present");
        assert!(plan.embedded.is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_incompatible_release_still_refuses_an_installed_copy_of_it() {
        let catalog = optional_of(pins(
            compatible("v1", "ROOT"),
            ModRelationKind::Incompatible,
            "lib-v1",
        ))
        .with("LIB", vec![compatible("lib-v1", "LIB")])
        .failing_for(hashes_missing, &["lib-v1"]);

        let installed = [InstalledProject {
            project_id: "LIB".to_owned(),
            version_id: "lib-v1".to_owned(),
        }];
        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &installed)
            .await
            .expect_err("the conflict must still be found");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. } if conflicting == "LIB"
        ));

        let other = [InstalledProject {
            project_id: "LIB".to_owned(),
            version_id: "lib-v2".to_owned(),
        }];
        plan_with(&catalog, InstallSelection::new("ROOT"), &other)
            .await
            .expect("another release of the project does not conflict");
    }

    #[tokio::test]
    async fn an_unreadable_incompatible_release_still_refuses_the_same_release_in_the_plan() {
        // The owner lookup and its single-release retry fail, then the project's
        // own release list resolves the pinned release.
        let mut catalog = declares_b1_incompatible(vec![at(compatible("b1", "B"), 0)])
            .failing_for(hashes_missing, &["b1"]);
        catalog.failures_before_recovery = Some(std::sync::atomic::AtomicUsize::new(2));

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("the conflict must still be found");
        assert!(matches!(
            error,
            ModPlatformError::IncompatibleDependency { ref conflicting, .. } if conflicting == "B"
        ));
    }

    #[tokio::test]
    async fn a_canceled_lookup_of_a_release_pinned_without_a_project_by_an_embedded_relation_stops_the_plan()
     {
        let catalog = optional_of(pins(
            compatible("v1", "ROOT"),
            ModRelationKind::Embedded,
            "lib-v1",
        ))
        .failing_for(canceled, &["lib-v1"]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("a canceled lookup stops the plan");
        assert!(error.is_cancellation());
    }

    #[tokio::test]
    async fn an_unusable_release_does_not_hide_an_ownership_mismatch_beside_it() {
        let root = pins_of(
            pins_of(
                compatible("v1", "ROOT"),
                ModRelationKind::Embedded,
                "LIB",
                "lib-v1",
            ),
            ModRelationKind::Embedded,
            "OTHERLIB",
            "other-v1",
        );
        let catalog = optional_of(root)
            .with("LIB", vec![compatible("lib-v1", "LIB")])
            .with("OTHER", vec![compatible("other-v1", "OTHER")])
            .failing_for(hashes_missing, &["lib-v1"]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(error, ModPlatformError::InconsistentDependency { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn embedded_and_incompatible_relations_naming_agreeing_identities_are_accepted() {
        for kind in [ModRelationKind::Embedded, ModRelationKind::Incompatible] {
            let catalog = optional_of(pins_of(compatible("v1", "ROOT"), kind, "LIB", "lib-v1"))
                .with("LIB", vec![compatible("lib-v1", "LIB")]);

            let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
            assert_eq!(
                plan.embedded.len(),
                usize::from(kind == ModRelationKind::Embedded)
            );
        }
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

    // -- resolution ceiling ------------------------------------------------

    /// A chain of `length` projects where each requires the next, so resolving
    /// it takes exactly `length` actions. The last project's release is
    /// incompatible with the instance when `broken_tail` is set.
    fn chain_of(length: usize, broken_tail: bool) -> Catalog {
        (0..length).fold(Catalog::default(), |catalog, index| {
            let id = format!("P{index}");
            let release = if broken_tail && index + 1 == length {
                version("v1", &id, &["1.21.1"], &["neoforge"])
            } else {
                compatible("v1", &id)
            };
            let release = if index + 1 < length {
                needs(
                    release,
                    ModRelationKind::Required,
                    &format!("P{}", index + 1),
                )
            } else {
                release
            };
            catalog.with(&id, vec![release])
        })
    }

    #[tokio::test]
    async fn a_graph_needing_fewer_actions_than_the_ceiling_resolves() {
        let catalog = chain_of(MAX_RESOLUTION_ROUNDS - 1, false);

        let plan = plan(&catalog, InstallSelection::new("P0")).await;
        assert_eq!(plan.dependencies.len(), MAX_RESOLUTION_ROUNDS - 2);
    }

    #[tokio::test]
    async fn a_graph_completing_on_the_last_allowed_action_is_converged() {
        let catalog = chain_of(MAX_RESOLUTION_ROUNDS, false);

        let plan = plan(&catalog, InstallSelection::new("P0")).await;
        assert_eq!(plan.dependencies.len(), MAX_RESOLUTION_ROUNDS - 1);
    }

    #[tokio::test]
    async fn a_graph_needing_one_action_beyond_the_ceiling_is_refused() {
        let catalog = chain_of(MAX_RESOLUTION_ROUNDS + 1, false);

        let error = plan_with(&catalog, InstallSelection::new("P0"), &[])
            .await
            .expect_err("must refuse");
        assert!(matches!(
            error,
            ModPlatformError::ResolutionDidNotConverge {
                iterations: MAX_RESOLUTION_ROUNDS
            }
        ));
    }

    #[tokio::test]
    async fn a_refusal_recorded_by_the_last_allowed_action_is_reported_as_itself() {
        let catalog = chain_of(MAX_RESOLUTION_ROUNDS, true);

        let error = plan_with(&catalog, InstallSelection::new("P0"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::NoCompatibleVersion { ref project_id, .. }
                    if *project_id == format!("P{}", MAX_RESOLUTION_ROUNDS - 1)
            ),
            "{error:?}"
        );
    }

    // -- choices that undo each other --------------------------------------

    /// ROOT requires SSS. The newest SSS requires NNN, which pins the older SSS,
    /// and the older SSS does not need NNN, so the pin would disappear with it.
    fn a_pin_that_removes_itself() -> Catalog {
        Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    compatible("root-1", "ROOT"),
                    ModRelationKind::Required,
                    "SSS",
                )],
            )
            .with(
                "SSS",
                vec![
                    needs(
                        at(compatible("s-old", "SSS"), 0),
                        ModRelationKind::Required,
                        "CCC",
                    ),
                    needs(
                        at(compatible("s-new", "SSS"), 10),
                        ModRelationKind::Required,
                        "NNN",
                    ),
                ],
            )
            .with("CCC", vec![compatible("c-1", "CCC")])
            .with(
                "NNN",
                vec![pins_of(
                    compatible("n-1", "NNN"),
                    ModRelationKind::Required,
                    "SSS",
                    "s-old",
                )],
            )
    }

    #[tokio::test]
    async fn a_graph_whose_choices_undo_each_other_is_refused_naming_the_projects() {
        let catalog = a_pin_that_removes_itself();

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::NoConsistentVersions { ref projects, ref pins }
                    if projects == "CCC, NNN, SSS" && pins == "SSS = s-old (NNN)"
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_graph_whose_choices_undo_each_other_is_refused_without_a_lookup_per_round() {
        let catalog = a_pin_that_removes_itself();

        plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            catalog.lookups() < 40,
            "{} lookups for a four-project graph",
            catalog.lookups()
        );
    }

    /// ROOT's newest release requires K. K's newest release pins an older ROOT
    /// that does not need K, so it cannot stay. K's older release requires PIN,
    /// and PIN pins that older release: the plan holds only because PIN is
    /// there, and PIN is there only because of the release it pins.
    fn a_pin_that_sustains_itself() -> Catalog {
        Catalog::default()
            .with(
                "ROOT",
                vec![
                    at(compatible("root-old", "ROOT"), 0),
                    needs(
                        at(compatible("root-new", "ROOT"), 10),
                        ModRelationKind::Required,
                        "K",
                    ),
                ],
            )
            .with(
                "K",
                vec![
                    needs(
                        at(compatible("k-old", "K"), 0),
                        ModRelationKind::Required,
                        "PIN",
                    ),
                    pins_of(
                        at(compatible("k-new", "K"), 10),
                        ModRelationKind::Required,
                        "ROOT",
                        "root-old",
                    ),
                ],
            )
            .with(
                "PIN",
                vec![pins_of(
                    compatible("pin-1", "PIN"),
                    ModRelationKind::Required,
                    "K",
                    "k-old",
                )],
            )
    }

    #[tokio::test]
    async fn a_plan_held_up_only_by_the_pin_its_own_release_brings_in_is_found() {
        let catalog = a_pin_that_sustains_itself();

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            chosen_versions(&plan),
            pairs([("K", "k-old"), ("PIN", "pin-1"), ("ROOT", "root-new")])
        );
        let k = plan
            .dependencies
            .iter()
            .find(|planned| planned.version.project_id == "K")
            .expect("present");
        assert_eq!(k.requested_by, vec!["PIN".to_owned(), "ROOT".to_owned()]);
    }

    #[tokio::test]
    async fn the_plan_a_self_sustaining_pin_allows_does_not_depend_on_response_order() {
        let catalog = a_pin_that_sustains_itself();

        let forward = plan(&catalog, InstallSelection::new("ROOT")).await;
        let backward = plan(&catalog.reversed(), InstallSelection::new("ROOT")).await;
        assert_eq!(decisions(&forward), decisions(&backward));
    }

    #[tokio::test]
    async fn a_release_the_user_selected_is_not_moved_to_find_a_plan() {
        // K pins the newest ROOT, so the only plan runs that release. Selecting
        // the older one is refused rather than answered with that plan.
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![
                    needs(
                        at(compatible("root-old", "ROOT"), 0),
                        ModRelationKind::Required,
                        "K",
                    ),
                    needs(
                        at(compatible("root-new", "ROOT"), 10),
                        ModRelationKind::Required,
                        "K",
                    ),
                ],
            )
            .with(
                "K",
                vec![pins_of(
                    compatible("k-1", "K"),
                    ModRelationKind::Required,
                    "ROOT",
                    "root-new",
                )],
            );

        let unselected = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            chosen_versions(&unselected),
            pairs([("K", "k-1"), ("ROOT", "root-new")])
        );
        let error = plan_with(
            &catalog,
            InstallSelection::new("ROOT").at_version("root-old"),
            &[],
        )
        .await
        .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::ConflictingPinnedVersions { ref project_id, .. }
                    if project_id == "ROOT"
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_plan_is_found_when_the_newest_release_of_a_project_is_refused() {
        // K's newest release needs a project the catalog does not know. Its
        // older release is held up by a pin of its own, and avoids it.
        let catalog = a_pin_that_sustains_itself().with(
            "K",
            vec![
                needs(
                    at(compatible("k-old", "K"), 0),
                    ModRelationKind::Required,
                    "PIN",
                ),
                needs(
                    at(compatible("k-new", "K"), 10),
                    ModRelationKind::Required,
                    "GONE",
                ),
            ],
        );

        let plan = plan(&catalog, InstallSelection::new("ROOT")).await;
        assert_eq!(
            chosen_versions(&plan),
            pairs([("K", "k-old"), ("PIN", "pin-1"), ("ROOT", "root-new")])
        );
    }

    #[tokio::test]
    async fn a_dependency_the_catalog_does_not_know_is_refused_after_one_lookup_of_it() {
        let releases: Vec<ModVersionInfo> = (0..80)
            .map(|index| {
                needs(
                    at(compatible(&format!("r-{index}"), "ROOT"), index),
                    ModRelationKind::Required,
                    "GONE",
                )
            })
            .collect();
        let catalog = Catalog::default().with("ROOT", releases);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::ProjectUnknown { ref reference, .. } if reference == "GONE"
            ),
            "{error:?}"
        );
        assert!(
            catalog.lookups() < 10,
            "{} lookups to refuse one unknown dependency",
            catalog.lookups()
        );
    }

    #[tokio::test]
    async fn a_release_that_cannot_be_fetched_ends_only_the_attempt_that_assumed_it() {
        fn unusable() -> ModPlatformError {
            ModPlatformError::HashesMissing {
                version_id: "s-ancient".to_owned(),
            }
        }
        let mut catalog = a_pin_that_removes_itself().failing_for(unusable, &["s-ancient"]);
        catalog.versions.get_mut("NNN").expect("present").insert(
            0,
            pins_of(
                at(compatible("n-0", "NNN"), -10),
                ModRelationKind::Required,
                "SSS",
                "s-ancient",
            ),
        );

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(error, ModPlatformError::NoConsistentVersions { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn the_search_asks_the_provider_only_about_real_projects() {
        let catalog = a_pin_that_removes_itself();

        plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        let asked = catalog.asked.lock().expect("not poisoned");
        assert!(
            !asked.contains("root") && !asked.contains(""),
            "asked about {asked:?}"
        );
    }

    #[tokio::test]
    async fn a_pin_conflict_with_no_way_out_is_still_reported_as_the_conflict() {
        // Two mods pin a library to different releases of its many. No
        // assumption can make both true, and the refusal must say so rather
        // than describing a search.
        let releases: Vec<ModVersionInfo> = (0..200)
            .map(|index| at(compatible(&format!("l-{index}"), "L"), index))
            .collect();
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    needs(compatible("root-1", "ROOT"), ModRelationKind::Required, "A"),
                    ModRelationKind::Required,
                    "B",
                )],
            )
            .with(
                "A",
                vec![pins_of(
                    compatible("a-1", "A"),
                    ModRelationKind::Required,
                    "L",
                    "l-3",
                )],
            )
            .with(
                "B",
                vec![pins_of(
                    compatible("b-1", "B"),
                    ModRelationKind::Required,
                    "L",
                    "l-7",
                )],
            )
            .with("L", releases);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::ConflictingPinnedVersions { ref project_id, .. }
                    if project_id == "L"
            ),
            "{error:?}"
        );
        assert!(
            catalog.lookups() < 100,
            "{} lookups to refuse one conflict",
            catalog.lookups()
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
    async fn a_planned_release_states_the_project_of_a_relation_that_only_pinned_a_version() {
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
        let declared = &plan.root.version.dependencies;
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].project_id.as_deref(), Some("LIB"));
        assert_eq!(
            declared[0].version_id.as_deref(),
            Some("lib-v1"),
            "the pin itself is kept beside the project it was derived for"
        );
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
        assert!(
            matches!(
                error,
                ModPlatformError::InconsistentDependency {
                    ref declared_by,
                    ref project_id,
                    ref version_project_id,
                } if declared_by == "ROOT" && project_id == "LIB" && version_project_id == "OTHER"
            ),
            "the mod that carries the pin is named, not the pinned project: {error:?}"
        );
    }

    #[tokio::test]
    async fn every_mod_declaring_a_pin_that_belongs_to_another_project_is_named() {
        let pin_other = |mut release: ModVersionInfo| {
            release.dependencies.push(ModDependency {
                kind: ModRelationKind::Required,
                project_id: Some("LIB".to_owned()),
                version_id: Some("other-v1".to_owned()),
                file_name: None,
            });
            release
        };
        let catalog = Catalog::default()
            .with(
                "ROOT",
                vec![needs(
                    pin_other(compatible("v1", "ROOT")),
                    ModRelationKind::Required,
                    "MID",
                )],
            )
            .with("MID", vec![pin_other(compatible("mid-v1", "MID"))])
            .with("LIB", vec![compatible("lib-v1", "LIB")])
            .with("OTHER", vec![compatible("other-v1", "OTHER")]);

        let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
            .await
            .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::InconsistentDependency { ref declared_by, .. }
                    if declared_by == "MID, ROOT"
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_pin_kept_over_one_a_moving_release_brought_in_names_the_mod_that_declared_it() {
        // AAA, reached only through BBB's newest release, pins that release.
        // KKK pins a version of another project as BBB's, and that pin is the
        // one that survives once BBB moves, so KKK carries the bad metadata.
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
                vec![needs(
                    compatible("b-new", "BBB"),
                    ModRelationKind::Required,
                    "AAA",
                )],
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
                vec![pins_of(
                    compatible("k1", "KKK"),
                    ModRelationKind::Required,
                    "BBB",
                    "b-old",
                )],
            )
            .with("OTHER", vec![compatible("b-old", "OTHER")]);

        for catalog in [catalog.reversed(), catalog] {
            let error = plan_with(&catalog, InstallSelection::new("ROOT"), &[])
                .await
                .expect_err("must refuse");
            assert!(
                matches!(
                    error,
                    ModPlatformError::InconsistentDependency { ref declared_by, .. }
                        if declared_by == "KKK"
                ),
                "{error:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_selected_version_belonging_to_another_project_is_reported_as_the_selection() {
        let catalog = Catalog::default()
            .with("ROOT", vec![compatible("v1", "ROOT")])
            .with("OTHER", vec![compatible("other-v1", "OTHER")]);

        let error = plan_with(
            &catalog,
            InstallSelection::new("ROOT").at_version("other-v1"),
            &[],
        )
        .await
        .expect_err("must refuse");
        assert!(
            matches!(
                error,
                ModPlatformError::InconsistentSelectedVersion {
                    ref project_id,
                    ref version_project_id,
                } if project_id == "ROOT" && version_project_id == "OTHER"
            ),
            "{error:?}"
        );
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
    async fn a_dependency_recorded_only_for_the_other_side_is_installed_rather_than_reused() {
        let plan = plan_recording(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[],
            &[installed("LIBPROJ", "LIBV1")],
            InstalledPolicy::Preserve,
        )
        .await
        .expect("a plan");

        assert!(plan.already_installed.is_empty());
        assert_eq!(
            ids(plan.to_install()),
            vec!["ROOTPROJ".to_owned(), "LIBPROJ".to_owned()],
            "the other side's record puts no jar on this one"
        );
    }

    #[tokio::test]
    async fn a_release_change_of_a_mod_recorded_for_the_other_side_is_still_refused_by_an_install()
    {
        let error = plan_recording(
            &upgradeable(),
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[],
            &[installed("ROOTPROJ", "ROOTV1")],
            InstalledPolicy::Preserve,
        )
        .await
        .expect_err("the recipe holds one entry per project");
        assert!(matches!(
            error,
            ModPlatformError::ProjectAlreadyInstalled { .. }
        ));
    }

    #[tokio::test]
    async fn an_unpinned_dependency_recorded_for_the_other_side_keeps_its_release() {
        let catalog = upgradeable().with(
            "LIBPROJ",
            vec![
                at(compatible("LIBV1", "LIBPROJ"), 0),
                at(compatible("LIBV2", "LIBPROJ"), 30),
            ],
        );
        let plan = plan_recording(
            &catalog,
            InstallSelection::new("ROOTPROJ").at_version("ROOTV2"),
            &[installed("ROOTPROJ", "ROOTV1")],
            &[installed("LIBPROJ", "LIBV1")],
            InstalledPolicy::Replace,
        )
        .await
        .expect("a plan");

        let library = plan
            .dependencies
            .iter()
            .find(|planned| planned.version.project_id == "LIBPROJ")
            .expect("planned");
        assert_eq!(library.version.id, "LIBV1");
        assert!(
            plan.to_install()
                .iter()
                .any(|planned| planned.version.project_id == "LIBPROJ"),
            "kept at its release, and still written on this side"
        );
    }

    #[tokio::test]
    async fn a_mod_recorded_for_the_other_side_conflicts_with_nothing_here() {
        let catalog = Catalog::default().with(
            "ROOT",
            vec![needs(
                compatible("v1", "ROOT"),
                ModRelationKind::Incompatible,
                "ELSEWHERE",
            )],
        );

        let plan = plan_recording(
            &catalog,
            InstallSelection::new("ROOT"),
            &[],
            &[installed("ELSEWHERE", "whatever")],
            InstalledPolicy::Preserve,
        )
        .await
        .expect("a mod this side does not have is not in the way");
        assert_eq!(plan.root.version.id, "v1");
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

    // -- exhaustive agreement ------------------------------------------------

    /// A small deterministic generator, so a failing catalog can be replayed
    /// from its seed.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let bound = u64::try_from(bound).expect("a small bound");
            usize::try_from((self.0 >> 33) % bound).expect("below a small bound")
        }
    }

    /// A random catalog of `projects` projects, each with one to three
    /// releases that require and pin one another, and sometimes a project the
    /// catalog does not know.
    fn random_catalog(seed: u64, projects: usize) -> Catalog {
        let mut rng = Lcg(seed);
        let counts: Vec<usize> = (0..projects).map(|_| 1 + rng.below(3)).collect();
        let mut catalog = Catalog::default();
        for (index, count) in counts.iter().enumerate() {
            let project_id = format!("P{index}");
            let mut releases = Vec::new();
            for release in 0..*count {
                let mut candidate = at(
                    compatible(&format!("p{index}v{release}"), &project_id),
                    i64::try_from(release).expect("a small index") * 10,
                );
                for (other, other_count) in counts.iter().enumerate() {
                    if other == index || rng.below(100) >= 35 {
                        continue;
                    }
                    let other_id = format!("P{other}");
                    candidate = if rng.below(100) < 40 {
                        let pinned = rng.below(*other_count);
                        pins_of(
                            candidate,
                            ModRelationKind::Required,
                            &other_id,
                            &format!("p{other}v{pinned}"),
                        )
                    } else {
                        needs(candidate, ModRelationKind::Required, &other_id)
                    };
                }
                if rng.below(100) < 3 {
                    candidate = needs(candidate, ModRelationKind::Required, "GONE");
                }
                releases.push(candidate);
            }
            catalog = catalog.with(&project_id, releases);
        }
        catalog
    }

    /// Every set of `(project, release)` choices that is consistent: each
    /// project the root reaches runs the release the reached graph pins it to,
    /// or its newest when nothing pins it.
    fn consistent_plans(
        catalog: &Catalog,
        root_pin: Option<&str>,
    ) -> BTreeSet<Vec<(String, String)>> {
        let projects: Vec<&String> = catalog.versions.keys().collect();
        let mut found = BTreeSet::new();
        let mut choice = vec![0_usize; projects.len()];
        loop {
            let chosen = |project: &str| -> Option<&ModVersionInfo> {
                let index = projects.iter().position(|id| id.as_str() == project)?;
                catalog.versions[projects[index]].get(choice[index])
            };
            let mut reached = BTreeSet::from(["P0".to_owned()]);
            let mut pins: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            if let Some(pin) = root_pin {
                pins.entry("P0".to_owned())
                    .or_default()
                    .insert(pin.to_owned());
            }
            let mut queue = vec!["P0".to_owned()];
            let mut valid = true;
            while let Some(project) = queue.pop() {
                let Some(release) = chosen(&project) else {
                    valid = false;
                    break;
                };
                for dependency in &release.dependencies {
                    let Some(target) = dependency.project_id.clone() else {
                        continue;
                    };
                    if let Some(pin) = &dependency.version_id {
                        pins.entry(target.clone()).or_default().insert(pin.clone());
                    }
                    if reached.insert(target.clone()) {
                        queue.push(target);
                    }
                }
            }
            if valid {
                valid = reached.iter().all(|project| {
                    let Some(release) = chosen(project) else {
                        return false;
                    };
                    match pins.get(project) {
                        Some(pinned) if pinned.len() > 1 => false,
                        Some(pinned) => pinned.contains(&release.id),
                        None => {
                            let newest = catalog.versions[project]
                                .iter()
                                .max_by_key(|version| (version.published_at, version.id.clone()))
                                .map(|version| version.id.clone());
                            newest.as_deref() == Some(release.id.as_str())
                        }
                    }
                });
            }
            if valid {
                found.insert(
                    reached
                        .iter()
                        .filter_map(|project| {
                            chosen(project).map(|release| (project.clone(), release.id.clone()))
                        })
                        .collect(),
                );
            }
            // Advance the odometer.
            let mut position = 0;
            loop {
                if position == projects.len() {
                    return found;
                }
                choice[position] += 1;
                if choice[position] < catalog.versions[projects[position]].len() {
                    break;
                }
                choice[position] = 0;
                position += 1;
            }
        }
    }

    #[tokio::test]
    async fn the_resolver_finds_a_plan_exactly_when_a_consistent_one_exists() {
        let mut disagreements = Vec::new();
        for seed in 0..600_u64 {
            let projects = 3 + usize::try_from(seed % 4).expect("a small remainder");
            let catalog = random_catalog(seed, projects);
            let root_pin = (seed % 7 == 0).then_some("p0v0");
            let mut selection = InstallSelection::new("P0");
            if let Some(pin) = root_pin {
                selection = selection.at_version(pin);
            }
            let consistent = consistent_plans(&catalog, root_pin);
            let verdict = match plan_with(&catalog, selection, &[]).await {
                Ok(plan) if consistent.contains(&chosen_versions(&plan)) => None,
                Ok(plan) => Some(format!("planned {:?}", chosen_versions(&plan))),
                Err(_) if consistent.is_empty() => None,
                Err(error) => Some(format!(
                    "refused with {error:?} though {consistent:?} exist"
                )),
            };
            if let Some(verdict) = verdict {
                disagreements.push(format!("seed {seed} ({projects} projects): {verdict}"));
            }
        }
        assert!(
            disagreements.is_empty(),
            "{} disagreements, first: {:#?}",
            disagreements.len(),
            &disagreements[..disagreements.len().min(5)]
        );
    }
}
