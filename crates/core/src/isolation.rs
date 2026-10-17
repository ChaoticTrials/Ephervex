//! Dependency-aware mod isolation for one installed host.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_download::OperationReporter;
use ephervex_model::SnapshotId;
use ephervex_storage::{ClientCommitWarning, ServerCommitWarning};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::dependencies::ModGraph;
use crate::mods::SecondCopies;
use crate::{
    App, CoreError, CoreResult, DependencyKnowledge, InstalledHost, ModPresence, SnapshotError,
    SnapshotOrigin,
};

const ISOLATION_VERSION: u32 = 1;

/// A suspect the user can test, together with everything it requires.
///
/// Every enabled mod outside the pinned closure is its own candidate, including
/// one that another candidate requires. Only mods that require each other are
/// merged, under their lexicographically first path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IsolationCandidate {
    pub root: PathBuf,
    pub name: String,
    pub closure: Vec<PathBuf>,
}

/// A required dependency whose jar is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IsolationMissingDependency {
    pub required_by: PathBuf,
    pub dependency: String,
}

/// A linked installation that this single-host workflow does not change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IsolationLinkedServer {
    pub id: String,
    pub name: String,
}

/// An offline plan derived entirely from the shared dependency graph.
///
/// Two plans for one host are equal exactly when nothing the plan depends on
/// changed, which is what lets a frontend tell that a setup has gone stale.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IsolationPlan {
    pub pinned: Vec<PathBuf>,
    pub pinned_closure: Vec<PathBuf>,
    pub candidates: Vec<IsolationCandidate>,
    pub available: Vec<PathBuf>,
    pub initially_enabled: Vec<PathBuf>,
    pub unrecorded: Vec<PathBuf>,
    pub unidentifiable: Vec<PathBuf>,
    pub missing: Vec<IsolationMissingDependency>,
    pub linked_servers: Vec<IsolationLinkedServer>,
}

impl IsolationPlan {
    /// Recomputes a configuration as a union of complete required closures.
    /// A shared library stays enabled whenever any selected root requires it.
    pub fn enabled_for(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut enabled: BTreeSet<_> = self.pinned_closure.iter().cloned().collect();
        for candidate in &self.candidates {
            if roots.contains(&candidate.root) {
                enabled.extend(candidate.closure.iter().cloned());
            }
        }
        enabled.into_iter().collect()
    }

    /// The suspects that `selected` enables, itself included.
    fn required_by(&self, suspects: &[PathBuf], selected: &[PathBuf]) -> Vec<PathBuf> {
        let closures = self.closures();
        let required: BTreeSet<&Path> = selected
            .iter()
            .filter_map(|root| closures.get(root.as_path()))
            .flatten()
            .copied()
            .collect();
        suspects
            .iter()
            .filter(|root| required.contains(root.as_path()))
            .cloned()
            .collect()
    }

    /// The suspects left after removing `removed` and every suspect requiring one of them.
    fn without_dependents(&self, suspects: &[PathBuf], removed: &[PathBuf]) -> Vec<PathBuf> {
        let closures = self.closures();
        suspects
            .iter()
            .filter(|root| {
                !removed.contains(root)
                    && closures.get(root.as_path()).is_none_or(|closure| {
                        !removed.iter().any(|r| closure.contains(r.as_path()))
                    })
            })
            .cloned()
            .collect()
    }

    /// The members of `roots` that no other member requires.
    fn outermost(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let closures = self.closures();
        roots
            .iter()
            .filter(|root| {
                !roots.iter().any(|other| {
                    other != *root
                        && closures
                            .get(other.as_path())
                            .is_some_and(|closure| closure.contains(root.as_path()))
                })
            })
            .cloned()
            .collect()
    }

    fn closures(&self) -> BTreeMap<&Path, BTreeSet<&Path>> {
        self.candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.root.as_path(),
                    candidate.closure.iter().map(PathBuf::as_path).collect(),
                )
            })
            .collect()
    }
}

/// Settings chosen before an isolation session changes the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<PathBuf>,
    #[serde(default)]
    pub accept_unrecorded: bool,
    /// Continue even when a declared required dependency is unavailable.
    ///
    /// The missing edge cannot be included in a tested closure, so accepting
    /// it marks the session and result as reduced confidence.
    #[serde(default)]
    pub accept_missing: bool,
}

/// The observation made after testing one configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundAnswer {
    Reproduced,
    DidNotReproduce,
}

impl RoundAnswer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reproduced => "reproduced",
            Self::DidNotReproduce => "did_not_reproduce",
        }
    }
}

/// Which deterministic ddmin configuration a round represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationRoundKind {
    Subset,
    Complement,
    Confirmation,
}

impl IsolationRoundKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Subset => "subset",
            Self::Complement => "complement",
            Self::Confirmation => "confirmation",
        }
    }
}

/// One exact configuration the user should launch and test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationRound {
    pub index: u32,
    pub kind: IsolationRoundKind,
    pub granularity: usize,
    pub group_index: usize,
    pub group_count: usize,
    pub enabled_roots: Vec<PathBuf>,
    pub enabled: Vec<PathBuf>,
    pub disabled: Vec<PathBuf>,
}

/// A pair of observations that cannot both describe a monotonic reproduction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundContradiction {
    pub reproducing_round: u32,
    pub conflicting_round: u32,
}

/// A completed observation retained for audit and undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationRoundRecord {
    #[serde(flatten)]
    pub round: IsolationRound,
    pub answer: RoundAnswer,
    pub answered_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contradiction: Option<RoundContradiction>,
    search_before: SearchState,
}

/// How strong a completed result is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationConfidence {
    /// Removing any one remaining candidate, together with the candidates that
    /// require it, stopped the issue.
    OneMinimal,
    /// The search reached zero or one candidate, so no smaller candidate set exists.
    SmallestReached,
}

impl IsolationConfidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneMinimal => "one_minimal",
            Self::SmallestReached => "smallest_reached",
        }
    }
}

/// The configuration ddmin has finished narrowing to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationOutcome {
    /// Every candidate still enabled, including those only required by another.
    pub roots: Vec<PathBuf>,
    pub enabled: Vec<PathBuf>,
    pub disabled: Vec<PathBuf>,
    pub confidence: IsolationConfidence,
}

/// The persisted session. Its intended configuration is sufficient to repair
/// an interrupted batch by applying it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationSession {
    pub host_kind: String,
    pub host_id: String,
    pub started_at: DateTime<Utc>,
    pub safety_snapshot: SnapshotId,
    pub plan: IsolationPlan,
    pub accepted_unrecorded: bool,
    #[serde(default)]
    pub accepted_missing: bool,
    pub reduced_confidence: bool,
    pub suspects: Vec<PathBuf>,
    pub granularity: usize,
    pub history: Vec<IsolationRoundRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_round: Option<IsolationRound>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contradiction: Option<RoundContradiction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<IsolationOutcome>,
    search: SearchState,
}

impl IsolationSession {
    /// The exact configuration that resume must idempotently reapply.
    #[must_use]
    pub fn intended_enabled(&self) -> &[PathBuf] {
        if let Some(round) = &self.current_round {
            &round.enabled
        } else if let Some(outcome) = &self.outcome {
            &outcome.enabled
        } else if let Some(round) = self.history.last() {
            &round.round.enabled
        } else {
            &self.plan.initially_enabled
        }
    }

    /// The report `finish` would return for the outcome waiting to be kept.
    #[must_use]
    pub fn pending_report(&self) -> Option<IsolationReport> {
        self.outcome
            .as_ref()
            .map(|outcome| report_for(self, outcome))
    }
}

/// What an operation leaves the workflow waiting for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum IsolationStep {
    AwaitingAnswer(IsolationRound),
    Complete(IsolationOutcome),
    Contradiction(RoundContradiction),
}

/// One root and the closure that is part of the final reproduction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationResultCandidate {
    pub root: PathBuf,
    pub name: String,
    pub closure: Vec<PathBuf>,
}

/// What reconciling a host with its isolation session had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationReconciliation {
    /// The host's mods already matched the intended configuration.
    AlreadyApplied,
    /// The intended configuration was reapplied through resume.
    Reapplied,
}

/// What canceling a session restored, and what the user should hear about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationCancel {
    /// Conditions on the restored client metadata. The restore is in effect
    /// either way.
    pub warnings: Vec<ClientCommitWarning>,
    /// Conditions on the restored server metadata.
    pub server_warnings: Vec<ServerCommitWarning>,
}

/// Final report returned after the durable session marker is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationReport {
    pub safety_snapshot: SnapshotId,
    /// Remaining candidates that no other remaining candidate requires.
    pub remaining_candidates: Vec<IsolationResultCandidate>,
    /// Roots that stayed enabled throughout and were never tested for removal.
    pub pinned: Vec<PathBuf>,
    /// Files the pinned roots require. They were never tested for removal.
    pub pinned_dependencies: Vec<PathBuf>,
    /// Files kept only because a remaining candidate requires them.
    pub required_dependencies: Vec<PathBuf>,
    pub excluded_roots: Vec<PathBuf>,
    pub enabled: Vec<PathBuf>,
    pub disabled: Vec<PathBuf>,
    pub rounds: usize,
    pub confidence: IsolationConfidence,
    pub reduced_confidence: bool,
    pub unrecorded: Vec<PathBuf>,
    pub unidentifiable: Vec<PathBuf>,
    pub missing: Vec<IsolationMissingDependency>,
    pub linked_servers: Vec<IsolationLinkedServer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SearchPhase {
    Subsets,
    Complements,
    Confirmation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SearchState {
    suspects: Vec<PathBuf>,
    granularity: usize,
    phase: SearchPhase,
    group_index: usize,
}

#[derive(Serialize, Deserialize)]
struct IsolationDocument {
    isolation_version: u32,
    #[serde(default)]
    written_by: String,
    #[serde(flatten)]
    session: IsolationSession,
}

impl IsolationDocument {
    fn new(session: IsolationSession) -> Self {
        Self {
            isolation_version: ISOLATION_VERSION,
            written_by: format!(
                "{} {}",
                ephervex_model::APP_NAME,
                ephervex_model::APP_VERSION
            ),
            session,
        }
    }
}

/// Failures specific to the durable isolation workflow.
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("{host} is running")]
    HostRunning { host: String },
    #[error("an isolation session already exists for {host}")]
    AlreadyActive { host: String },
    #[error("no isolation session exists for {host}")]
    NotActive { host: String },
    #[error("dependency knowledge must be refreshed before isolation can start")]
    UnrecordedDependencies { paths: Vec<PathBuf> },
    #[error("required dependencies are missing")]
    MissingDependencies {
        dependencies: Vec<IsolationMissingDependency>,
    },
    #[error("the current isolation round has already been answered")]
    NotAwaitingAnswer,
    #[error("the mods of {host} no longer match the current isolation round")]
    NotApplied { host: String },
    #[error("the isolation result is not ready to finish")]
    NotFinished,
    #[error("the isolation session has no current round, contradiction, or result")]
    InconsistentState,
    #[error("snapshot {snapshot} is the safety snapshot of an active isolation session")]
    SafetySnapshotInUse { snapshot: SnapshotId },
    #[error("the isolation session belongs to {found}, not {expected}")]
    HostMismatch { expected: String, found: String },
    #[error("isolation state at {path} could not be read")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("isolation state at {path} could not be written")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("isolation state at {path} is invalid")]
    Invalid {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("isolation state at {path} has version {found}, newer than {supported}")]
    UnsupportedVersion {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
}

impl IsolationError {
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::HostRunning { .. } => "error-isolation-host-running",
            Self::AlreadyActive { .. } => "error-isolation-already-active",
            Self::NotActive { .. } => "error-isolation-not-active",
            Self::UnrecordedDependencies { .. } => "error-isolation-unrecorded",
            Self::MissingDependencies { .. } => "error-isolation-missing-dependencies",
            Self::NotAwaitingAnswer => "error-isolation-not-awaiting-answer",
            Self::NotApplied { .. } => "error-isolation-not-applied",
            Self::NotFinished => "error-isolation-not-finished",
            Self::SafetySnapshotInUse { .. } => "error-isolation-snapshot-in-use",
            Self::HostMismatch { .. } => "error-isolation-host-mismatch",
            Self::Unreadable { .. } => "error-isolation-unreadable",
            Self::Unwritable { .. } => "error-isolation-unwritable",
            Self::Invalid { .. } | Self::InconsistentState => "error-isolation-invalid",
            Self::UnsupportedVersion { .. } => "error-isolation-version-newer",
            Self::Snapshot(error) => error.message_id(),
        }
    }

    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::HostRunning { host }
            | Self::AlreadyActive { host }
            | Self::NotActive { host }
            | Self::NotApplied { host } => vec![("host", host.clone())],
            Self::SafetySnapshotInUse { snapshot } => {
                vec![("snapshot", snapshot.to_string())]
            }
            Self::UnrecordedDependencies { paths } => {
                vec![("count", paths.len().to_string())]
            }
            Self::MissingDependencies { dependencies } => {
                vec![("count", dependencies.len().to_string())]
            }
            Self::HostMismatch { expected, found } => {
                vec![("expected", expected.clone()), ("found", found.clone())]
            }
            Self::UnsupportedVersion {
                found, supported, ..
            } => vec![
                ("found", found.to_string()),
                ("supported", supported.to_string()),
            ],
            Self::Snapshot(error) => error.message_args(),
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Unreadable { path, .. }
            | Self::Unwritable { path, .. }
            | Self::Invalid { path, .. }
            | Self::UnsupportedVersion { path, .. } => Some(path.clone()),
            Self::Snapshot(error) => error.failing_path(),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Snapshot(error) if error.is_cancellation())
    }
}

impl App {
    /// Plans an isolation session without modifying files or reaching a catalog.
    /// Unrecorded and missing dependencies are returned for an explicit decision.
    pub fn plan_isolation(
        &self,
        host: InstalledHost,
        keep: &[PathBuf],
    ) -> CoreResult<IsolationPlan> {
        let graph = ModGraph::build(self, host)?;
        let mut plan = IsolationPlan {
            pinned: keep
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            pinned_closure: Vec::new(),
            candidates: Vec::new(),
            available: Vec::new(),
            initially_enabled: Vec::new(),
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
            linked_servers: Vec::new(),
        };
        for path in &plan.pinned {
            let Some(index) = graph.index_of(path) else {
                return Err(CoreError::ModFileMissing { path: path.clone() });
            };
            if graph.entries[index].mod_entry.presence == ModPresence::Missing {
                return Err(CoreError::ModFileMissing { path: path.clone() });
            }
        }
        let mut closures = BTreeMap::new();
        let mut relevant = BTreeSet::new();
        let mut missing = BTreeSet::new();
        for (index, entry) in graph.entries.iter().enumerate() {
            let entry = &entry.mod_entry;
            if entry.presence == ModPresence::Missing {
                continue;
            }
            plan.available.push(entry.logical_path.clone());
            if entry.presence == ModPresence::Installed {
                plan.initially_enabled.push(entry.logical_path.clone());
            }
            if entry.presence != ModPresence::Installed
                && !plan.pinned.contains(&entry.logical_path)
            {
                continue;
            }
            let closure = graph.required_closure(index);
            let paths: BTreeSet<_> = closure
                .present
                .iter()
                .map(|entry| entry.logical_path.clone())
                .collect();
            relevant.extend(paths.iter().cloned());
            missing.extend(
                closure
                    .absent
                    .into_iter()
                    .map(|absent| IsolationMissingDependency {
                        required_by: entry.logical_path.clone(),
                        dependency: absent.name,
                    }),
            );
            if plan.pinned.contains(&entry.logical_path) {
                plan.pinned_closure.extend(paths.iter().cloned());
            }
            closures.insert(entry.logical_path.clone(), (entry.name.clone(), paths));
        }
        plan.pinned_closure.sort();
        plan.pinned_closure.dedup();
        populate_candidates(&mut plan, &closures);
        for entry in &graph.entries {
            let entry = &entry.mod_entry;
            if !relevant.contains(&entry.logical_path) {
                continue;
            }
            match entry.knowledge {
                DependencyKnowledge::Recorded => {}
                DependencyKnowledge::Unrecorded => plan.unrecorded.push(entry.logical_path.clone()),
                DependencyKnowledge::Unidentified | DependencyKnowledge::Unidentifiable => {
                    plan.unidentifiable.push(entry.logical_path.clone());
                }
            }
        }
        plan.available.sort();
        plan.initially_enabled.sort();
        plan.unrecorded.sort();
        plan.unidentifiable.sort();
        plan.candidates.sort_by(|a, b| a.root.cmp(&b.root));
        plan.missing = missing.into_iter().collect();
        if let InstalledHost::Instance(id) = host {
            plan.linked_servers = self
                .list_servers()?
                .into_iter()
                .filter(|server| server.linked_client_id == Some(id))
                .map(|server| IsolationLinkedServer {
                    id: server.id.to_string(),
                    name: server.name,
                })
                .collect();
            plan.linked_servers.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Ok(plan)
    }

    /// Starts a durable session, captures its safety snapshot, and applies the
    /// first test configuration.
    pub fn begin_isolation(
        &self,
        host: InstalledHost,
        request: IsolationRequest,
        cancel: &CancellationToken,
    ) -> CoreResult<IsolationRound> {
        self.begin_isolation_reporting(
            host,
            request,
            &OperationReporter::silent("operation-starting-isolation"),
            cancel,
        )
    }

    /// Starts a durable session while reporting each meaningful preparation
    /// stage to the caller's existing operation.
    pub fn begin_isolation_reporting(
        &self,
        host: InstalledHost,
        request: IsolationRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<IsolationRound> {
        self.refuse_isolation_while_running(host)?;
        // Held until the session file exists, so a mod change either finishes
        // before the safety snapshot is taken or sees the session and is
        // refused. The wait ends if the caller cancels.
        let _gate =
            crate::host_recipe_gate::hold_blocking_for_change(self.registry(), host, cancel)?;
        let path = self.isolation_path(host)?;
        if path.exists() {
            return Err(IsolationError::AlreadyActive { host: host.id() }.into());
        }
        let IsolationRequest {
            pinned,
            accept_unrecorded,
            accept_missing,
        } = request;
        reporter.set_stage(Some("operation-stage-planning-isolation".to_owned()));
        let plan = self.plan_isolation(host, &pinned)?;
        if !accept_missing && !plan.missing.is_empty() {
            return Err(IsolationError::MissingDependencies {
                dependencies: plan.missing.clone(),
            }
            .into());
        }
        if !accept_unrecorded && !plan.unrecorded.is_empty() {
            return Err(IsolationError::UnrecordedDependencies {
                paths: plan.unrecorded.clone(),
            }
            .into());
        }
        reporter.set_stage(Some("operation-stage-snapshot-isolation".to_owned()));
        let snapshot = self.create_snapshot(
            host,
            "Before mod isolation",
            SnapshotOrigin::BeforeIsolation,
            cancel,
        )?;
        let suspects: Vec<_> = plan
            .candidates
            .iter()
            .map(|candidate| candidate.root.clone())
            .collect();
        let confirmation = SearchState {
            suspects: suspects.clone(),
            granularity: suspects.len().max(1),
            phase: SearchPhase::Confirmation,
            group_index: 0,
        };
        let mut search = SearchState {
            suspects: suspects.clone(),
            granularity: 2,
            phase: SearchPhase::Subsets,
            group_index: 0,
        };
        // Two or more candidates always leave a smaller dependency-closed set
        // to test, so settling only falls back to confirmation for zero or one.
        let current_round = match (suspects.len() > 1).then(|| settle(&plan, &[], &mut search, 1)) {
            Some(Settled::Round(round)) => round,
            Some(Settled::Complete(_)) | None => {
                search = confirmation;
                round_for(&plan, &search, 1)
            }
        };
        let session = IsolationSession {
            host_kind: host.kind().to_owned(),
            host_id: host.id(),
            started_at: Utc::now(),
            safety_snapshot: snapshot.id,
            accepted_unrecorded: accept_unrecorded && !plan.unrecorded.is_empty(),
            accepted_missing: accept_missing && !plan.missing.is_empty(),
            reduced_confidence: !plan.unidentifiable.is_empty()
                || (accept_unrecorded && !plan.unrecorded.is_empty())
                || (accept_missing && !plan.missing.is_empty()),
            suspects,
            granularity: search.granularity,
            history: Vec::new(),
            current_round: Some(current_round.clone()),
            contradiction: None,
            outcome: None,
            search,
            plan,
        };
        write_session(&path, &session)?;
        reporter.set_stage(Some("operation-stage-applying-isolation".to_owned()));
        self.apply_isolation_configuration(host, &session, &current_round.enabled)?;
        Ok(current_round)
    }

    /// Reads a session without changing the host or repairing an interrupted round.
    pub fn isolation_state(&self, host: InstalledHost) -> CoreResult<Option<IsolationSession>> {
        let path = self.isolation_path(host)?;
        let Some(session) = read_session_if_present(&path)? else {
            return Ok(None);
        };
        validate_session_host(host, &session)?;
        Ok(Some(session))
    }

    /// Reapplies the intended configuration after a restart or interrupted batch.
    pub fn resume_isolation(&self, host: InstalledHost) -> CoreResult<IsolationStep> {
        self.refuse_isolation_while_running(host)?;
        let session = self.read_required_session(host)?;
        self.apply_isolation_configuration(host, &session, session.intended_enabled())?;
        Ok(step_for(&session)?)
    }

    /// Makes the host's mods match the session's intended configuration.
    ///
    /// The session document, not the mods folder, is authoritative. A host
    /// that already matches is left untouched; otherwise the configuration is
    /// reapplied through [`Self::resume_isolation`], which refuses while the
    /// host is running and leaves the session unchanged on failure.
    pub fn reconcile_isolation(&self, host: InstalledHost) -> CoreResult<IsolationReconciliation> {
        let session = self.read_required_session(host)?;
        if self.isolation_configuration_applied(host, &session)? {
            return Ok(IsolationReconciliation::AlreadyApplied);
        }
        self.resume_isolation(host)?;
        Ok(IsolationReconciliation::Reapplied)
    }

    /// Records an observation and advances deterministic ddmin.
    pub fn answer_isolation_round(
        &self,
        host: InstalledHost,
        answer: RoundAnswer,
    ) -> CoreResult<IsolationStep> {
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        let mut session = self.read_required_session(host)?;
        let round = session
            .current_round
            .clone()
            .ok_or(IsolationError::NotAwaitingAnswer)?;
        // An answer describes the configuration that was tested. If the host
        // no longer has the round's mods, it was not that configuration.
        if !self.isolation_configuration_applied(host, &session)? {
            return Err(IsolationError::NotApplied { host: host.id() }.into());
        }
        let contradiction = contradiction_for(&session.history, &round, answer);
        session.history.push(IsolationRoundRecord {
            round: round.clone(),
            answer,
            answered_at: Utc::now(),
            contradiction: contradiction.clone(),
            search_before: session.search.clone(),
        });
        if let Some(contradiction) = contradiction {
            session.current_round = None;
            session.contradiction = Some(contradiction.clone());
            write_session(&path, &session)?;
            return Ok(IsolationStep::Contradiction(contradiction));
        }
        advance_search(&mut session, answer);
        write_session(&path, &session)?;
        self.apply_isolation_configuration(host, &session, session.intended_enabled())?;
        Ok(step_for(&session)?)
    }

    /// Removes one answer, restores that exact round, and reapplies its configuration.
    pub fn undo_isolation_round(&self, host: InstalledHost) -> CoreResult<IsolationRound> {
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        let mut session = self.read_required_session(host)?;
        let record = session
            .history
            .pop()
            .ok_or(IsolationError::NotAwaitingAnswer)?;
        session.search = record.search_before;
        session.suspects = session.search.suspects.clone();
        session.granularity = session.search.granularity;
        session.current_round = Some(record.round.clone());
        session.contradiction = None;
        session.outcome = None;
        write_session(&path, &session)?;
        self.apply_isolation_configuration(host, &session, &record.round.enabled)?;
        Ok(record.round)
    }

    /// Keeps the smallest tested configuration and its safety snapshot, then
    /// removes the session marker.
    pub fn finish_isolation(&self, host: InstalledHost) -> CoreResult<IsolationReport> {
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        let session = self.read_required_session(host)?;
        let outcome = session.outcome.clone().ok_or(IsolationError::NotFinished)?;
        self.apply_isolation_configuration(host, &session, &outcome.enabled)?;
        // A session from before deletion was refused may have lost its snapshot;
        // the outcome is already applied, so there is nothing left to label.
        match self.rename_snapshot(
            host,
            session.safety_snapshot,
            &format!(
                "Before mod isolation, {}",
                session.started_at.format("%Y-%m-%d")
            ),
        ) {
            Ok(_) | Err(SnapshotError::NotFound { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        let report = report_for(&session, &outcome);
        remove_session(&path)?;
        Ok(report)
    }

    /// Restores the exact pre-isolation snapshot and removes the session marker.
    pub fn cancel_isolation(
        &self,
        host: InstalledHost,
        cancel: &CancellationToken,
    ) -> CoreResult<IsolationCancel> {
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        let session = self.read_required_session(host)?;
        let (warnings, server_warnings) =
            match self.restore_isolation_snapshot(host, session.safety_snapshot, cancel) {
                Ok(restore) => (restore.warnings, restore.server_warnings),
                // The snapshot was deleted before deletion was refused. Re-enabling
                // the recorded starting mods is the closest remaining restore.
                Err(SnapshotError::NotFound { .. }) => {
                    self.apply_isolation_configuration(
                        host,
                        &session,
                        &session.plan.initially_enabled,
                    )?;
                    (Vec::new(), Vec::new())
                }
                Err(error) => return Err(error.into()),
            };
        remove_session(&path)?;
        Ok(IsolationCancel {
            warnings,
            server_warnings,
        })
    }

    fn read_required_session(&self, host: InstalledHost) -> CoreResult<IsolationSession> {
        let path = self.isolation_path(host)?;
        let session = read_session_if_present(&path)?
            .ok_or_else(|| IsolationError::NotActive { host: host.id() })?;
        validate_session_host(host, &session)?;
        Ok(session)
    }

    fn isolation_path(&self, host: InstalledHost) -> CoreResult<PathBuf> {
        Ok(
            ephervex_storage::metadata::metadata_dir(&self.host_layout(host)?.root)
                .join(ephervex_storage::metadata::ISOLATION_FILE),
        )
    }

    fn refuse_isolation_while_running(&self, host: InstalledHost) -> CoreResult<()> {
        let running = match host {
            InstalledHost::Instance(id) => self.launch_state(id)?.is_some(),
            InstalledHost::Server(id) => self.server_status(id)?.runtime.may_be_running(),
        };
        if running {
            Err(IsolationError::HostRunning { host: host.id() }.into())
        } else {
            Ok(())
        }
    }

    /// Whether every mod the session manages is enabled exactly when intended.
    fn isolation_configuration_applied(
        &self,
        host: InstalledHost,
        session: &IsolationSession,
    ) -> CoreResult<bool> {
        let graph = ModGraph::build(self, host)?;
        let intended: BTreeSet<_> = session.intended_enabled().iter().collect();
        Ok(session.plan.available.iter().all(|path| {
            graph.index_of(path).is_some_and(|index| {
                match graph.entries[index].mod_entry.presence {
                    ModPresence::Installed => intended.contains(path),
                    ModPresence::Disabled => !intended.contains(path),
                    ModPresence::Missing => false,
                }
            })
        }))
    }

    fn apply_isolation_configuration(
        &self,
        host: InstalledHost,
        session: &IsolationSession,
        enabled: &[PathBuf],
    ) -> CoreResult<()> {
        let enabled: BTreeSet<_> = enabled.iter().collect();
        let changes: Vec<_> = session
            .plan
            .available
            .iter()
            .map(|path| (path.clone(), enabled.contains(path)))
            .collect();
        self.apply_mod_toggles(host, &changes, SecondCopies::Tolerated)?;
        Ok(())
    }
}

fn step_for(session: &IsolationSession) -> Result<IsolationStep, IsolationError> {
    if let Some(contradiction) = &session.contradiction {
        Ok(IsolationStep::Contradiction(contradiction.clone()))
    } else if let Some(outcome) = &session.outcome {
        Ok(IsolationStep::Complete(outcome.clone()))
    } else if let Some(round) = &session.current_round {
        Ok(IsolationStep::AwaitingAnswer(round.clone()))
    } else {
        Err(IsolationError::InconsistentState)
    }
}

fn advance_search(session: &mut IsolationSession, answer: RoundAnswer) {
    let current = session.current_round.take();
    let Some(current) = current else {
        return;
    };
    let next_index = current.index.saturating_add(1);
    if session.search.phase == SearchPhase::Confirmation {
        if answer == RoundAnswer::Reproduced {
            complete_search(session, IsolationConfidence::SmallestReached);
        } else {
            // There is no smaller candidate configuration to try. Keeping the
            // round current would invite repeated answers to the same facts;
            // the recorded non-reproduction remains undoable, but cannot be
            // presented as a successful result.
            session.contradiction = Some(RoundContradiction {
                reproducing_round: 0,
                conflicting_round: current.index,
            });
        }
        return;
    }

    if answer == RoundAnswer::Reproduced {
        let next_granularity = if current.kind == IsolationRoundKind::Complement {
            session.search.granularity.saturating_sub(1).max(2)
        } else {
            2
        };
        // A round's roots are dependency-closed, so the reproducing set keeps
        // every candidate it actually enabled rather than only the ones chosen.
        session.search.suspects = current.enabled_roots;
        session.suspects = session.search.suspects.clone();
        if session.search.suspects.len() <= 1 {
            complete_search(session, IsolationConfidence::SmallestReached);
            return;
        }
        session.search.granularity = next_granularity.min(session.search.suspects.len());
        session.search.phase = SearchPhase::Subsets;
        session.search.group_index = 0;
    } else if !move_past_non_reproducing(&mut session.search) {
        complete_search(session, IsolationConfidence::OneMinimal);
        return;
    }
    match settle(
        &session.plan,
        &session.history,
        &mut session.search,
        next_index,
    ) {
        Settled::Round(round) => {
            session.suspects = session.search.suspects.clone();
            session.granularity = session.search.granularity;
            session.current_round = Some(round);
        }
        Settled::Complete(confidence) => complete_search(session, confidence),
    }
}

/// Moves to the next ddmin configuration after one that did not reproduce.
/// Returns `false` once every complement at full granularity is exhausted.
fn move_past_non_reproducing(search: &mut SearchState) -> bool {
    let group_count = split_groups(&search.suspects, search.granularity).len();
    if search.group_index + 1 < group_count {
        search.group_index += 1;
    } else if search.phase == SearchPhase::Subsets {
        search.phase = SearchPhase::Complements;
        search.group_index = 0;
    } else if search.granularity < search.suspects.len() {
        search.granularity = (search.granularity * 2).min(search.suspects.len());
        search.phase = SearchPhase::Subsets;
        search.group_index = 0;
    } else {
        return false;
    }
    true
}

enum Settled {
    Round(IsolationRound),
    Complete(IsolationConfidence),
}

/// Advances past configurations whose answer is already known, so the user
/// is only asked about one that can narrow the search.
fn settle(
    plan: &IsolationPlan,
    history: &[IsolationRoundRecord],
    search: &mut SearchState,
    index: u32,
) -> Settled {
    loop {
        let round = round_for(plan, search, index);
        if round_is_informative(search, history, &round) {
            return Settled::Round(round);
        }
        if !move_past_non_reproducing(search) {
            return Settled::Complete(IsolationConfidence::OneMinimal);
        }
    }
}

/// A round is skipped, and treated as not reproducing, when it would enable no
/// candidate (ddmin's usual assumption), every current suspect (no progress),
/// or only files an earlier non-reproducing round already enabled.
fn round_is_informative(
    search: &SearchState,
    history: &[IsolationRoundRecord],
    round: &IsolationRound,
) -> bool {
    if search.phase == SearchPhase::Confirmation {
        return true;
    }
    if round.enabled_roots.is_empty() || round.enabled_roots.len() >= search.suspects.len() {
        return false;
    }
    let enabled: BTreeSet<_> = round.enabled.iter().collect();
    !history.iter().any(|record| {
        record.answer == RoundAnswer::DidNotReproduce
            && enabled.is_subset(&record.round.enabled.iter().collect())
    })
}

fn complete_search(session: &mut IsolationSession, confidence: IsolationConfidence) {
    let roots = session.search.suspects.clone();
    let enabled = session.plan.enabled_for(&roots);
    let enabled_set: BTreeSet<_> = enabled.iter().collect();
    let disabled = session
        .plan
        .available
        .iter()
        .filter(|path| !enabled_set.contains(path))
        .cloned()
        .collect();
    session.suspects.clone_from(&roots);
    session.granularity = session.search.granularity;
    session.current_round = None;
    session.outcome = Some(IsolationOutcome {
        roots,
        enabled,
        disabled,
        confidence,
    });
}

fn round_for(plan: &IsolationPlan, search: &SearchState, index: u32) -> IsolationRound {
    let groups = split_groups(&search.suspects, search.granularity);
    let group = groups.get(search.group_index).cloned().unwrap_or_default();
    // Both kinds of round stay closed under required dependencies: a subset
    // brings in the suspects its group requires, and a complement also drops
    // every suspect requiring something in the removed group.
    let roots = match search.phase {
        SearchPhase::Confirmation => search.suspects.clone(),
        SearchPhase::Subsets => plan.required_by(&search.suspects, &group),
        SearchPhase::Complements => plan.without_dependents(&search.suspects, &group),
    };
    let enabled = plan.enabled_for(&roots);
    let enabled_set: BTreeSet<_> = enabled.iter().collect();
    let disabled = plan
        .available
        .iter()
        .filter(|path| !enabled_set.contains(path))
        .cloned()
        .collect();
    IsolationRound {
        index,
        kind: match search.phase {
            SearchPhase::Subsets => IsolationRoundKind::Subset,
            SearchPhase::Complements => IsolationRoundKind::Complement,
            SearchPhase::Confirmation => IsolationRoundKind::Confirmation,
        },
        granularity: search.granularity,
        group_index: search.group_index,
        group_count: groups.len().max(1),
        enabled_roots: roots,
        enabled,
        disabled,
    }
}

fn split_groups(items: &[PathBuf], granularity: usize) -> Vec<Vec<PathBuf>> {
    if items.is_empty() {
        return vec![Vec::new()];
    }
    let count = granularity.clamp(1, items.len());
    let base = items.len() / count;
    let remainder = items.len() % count;
    let mut groups = Vec::with_capacity(count);
    let mut start = 0;
    for index in 0..count {
        let length = base + usize::from(index < remainder);
        groups.push(items[start..start + length].to_vec());
        start += length;
    }
    groups
}

fn contradiction_for(
    history: &[IsolationRoundRecord],
    current: &IsolationRound,
    answer: RoundAnswer,
) -> Option<RoundContradiction> {
    if answer != RoundAnswer::DidNotReproduce {
        return None;
    }
    let conflicting_round = current.index;
    let current: BTreeSet<_> = current.enabled.iter().collect();
    history.iter().find_map(|record| {
        if record.answer != RoundAnswer::Reproduced {
            return None;
        }
        let reproduced: BTreeSet<_> = record.round.enabled.iter().collect();
        reproduced
            .is_subset(&current)
            .then_some(RoundContradiction {
                reproducing_round: record.round.index,
                conflicting_round,
            })
    })
}

fn report_for(session: &IsolationSession, outcome: &IsolationOutcome) -> IsolationReport {
    let roots: BTreeSet<_> = outcome.roots.iter().collect();
    let outermost: BTreeSet<_> = session.plan.outermost(&outcome.roots).into_iter().collect();
    let remaining_candidates = session
        .plan
        .candidates
        .iter()
        .filter(|candidate| outermost.contains(&candidate.root))
        .map(|candidate| IsolationResultCandidate {
            root: candidate.root.clone(),
            name: candidate.name.clone(),
            closure: candidate.closure.clone(),
        })
        .collect();
    let pinned: BTreeSet<_> = session.plan.pinned.iter().collect();
    let pinned_closure: BTreeSet<_> = session.plan.pinned_closure.iter().collect();
    let pinned_dependencies = session
        .plan
        .pinned_closure
        .iter()
        .filter(|path| !pinned.contains(path))
        .cloned()
        .collect();
    let required_dependencies = outcome
        .enabled
        .iter()
        .filter(|path| !outermost.contains(*path) && !pinned_closure.contains(path))
        .cloned()
        .collect();
    let excluded_roots = session
        .plan
        .candidates
        .iter()
        .filter(|candidate| !roots.contains(&candidate.root))
        .map(|candidate| candidate.root.clone())
        .collect();
    IsolationReport {
        safety_snapshot: session.safety_snapshot,
        remaining_candidates,
        pinned: session.plan.pinned.clone(),
        pinned_dependencies,
        required_dependencies,
        excluded_roots,
        enabled: outcome.enabled.clone(),
        disabled: outcome.disabled.clone(),
        rounds: session.history.len(),
        confidence: outcome.confidence,
        reduced_confidence: session.reduced_confidence,
        unrecorded: session.plan.unrecorded.clone(),
        unidentifiable: session.plan.unidentifiable.clone(),
        missing: session.plan.missing.clone(),
        linked_servers: session.plan.linked_servers.clone(),
    }
}

fn validate_session_host(
    host: InstalledHost,
    session: &IsolationSession,
) -> Result<(), IsolationError> {
    let expected = format!("{}:{}", host.kind(), host.id());
    let found = format!("{}:{}", session.host_kind, session.host_id);
    if session.host_kind == host.kind() && session.host_id == host.id() {
        Ok(())
    } else {
        Err(IsolationError::HostMismatch { expected, found })
    }
}

fn write_session(path: &Path, session: &IsolationSession) -> Result<(), IsolationError> {
    ephervex_storage::write_atomic(
        path,
        ephervex_storage::AtomicWriteOptions::private(),
        |file| {
            serde_json::to_writer(file, &IsolationDocument::new(session.clone()))
                .map_err(std::io::Error::other)
        },
    )
    .map_err(|source| IsolationError::Unwritable {
        path: path.to_path_buf(),
        source,
    })
}

fn read_session_if_present(path: &Path) -> Result<Option<IsolationSession>, IsolationError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(IsolationError::Unreadable {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let document: IsolationDocument =
        serde_json::from_slice(&bytes).map_err(|source| IsolationError::Invalid {
            path: path.to_path_buf(),
            source,
        })?;
    if document.isolation_version > ISOLATION_VERSION {
        return Err(IsolationError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: document.isolation_version,
            supported: ISOLATION_VERSION,
        });
    }
    validate_session_shape(path, &document.session)?;
    Ok(Some(document.session))
}

fn validate_session_shape(path: &Path, session: &IsolationSession) -> Result<(), IsolationError> {
    let terminal_states = usize::from(session.current_round.is_some())
        + usize::from(session.contradiction.is_some())
        + usize::from(session.outcome.is_some());
    if terminal_states == 1 {
        return Ok(());
    }
    Err(IsolationError::Invalid {
        path: path.to_path_buf(),
        source: serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "expected exactly one current round, contradiction, or outcome",
        )),
    })
}

fn remove_session(path: &Path) -> Result<(), IsolationError> {
    std::fs::remove_file(path).map_err(|source| IsolationError::Unwritable {
        path: path.to_path_buf(),
        source,
    })?;
    ephervex_storage::sync_parent(path).map_err(|source| IsolationError::Unwritable {
        path: path.to_path_buf(),
        source,
    })
}

fn populate_candidates(
    plan: &mut IsolationPlan,
    closures: &BTreeMap<PathBuf, (String, BTreeSet<PathBuf>)>,
) {
    let pool: Vec<_> = plan
        .initially_enabled
        .iter()
        .filter(|path| !plan.pinned_closure.contains(path))
        .cloned()
        .collect();
    for path in &pool {
        let (name, closure) = &closures[path];
        // Mutually required mods form an inseparable component. Its
        // lexicographically first member represents it deterministically. A
        // mod that is merely required by another stays its own candidate, so
        // a dependency can still be tested without the mods requiring it.
        let merged_into_other = pool.iter().any(|other| {
            other < path && closures[other].1.contains(path) && closure.contains(other)
        });
        if !merged_into_other {
            plan.candidates.push(IsolationCandidate {
                root: path.clone(),
                name: name.clone(),
                closure: closure.iter().cloned().collect(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::time::Duration;

    use ephervex_model::LoaderSelection;
    use ephervex_storage::{NewClient, Paths};

    use super::*;
    use crate::error::CoreError;
    use crate::verify::RepairRefusal;

    fn client_without_mods(root: &Path) -> (Paths, InstalledHost) {
        let paths = Paths::rooted_at(root);
        let app = App::open_at(paths.clone()).expect("open app");
        let client = app
            .create_client_for_test(NewClient::new(
                "Isolation",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create client");
        let store =
            ephervex_pack::SourceStore::new(ephervex_storage::metadata::source_dir(&client.root));
        store
            .write_recipe(&ephervex_pack::PackRecipe::for_manual_instance(
                "Isolation",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("write the recipe");
        (paths, InstalledHost::Instance(client.id))
    }

    fn request() -> IsolationRequest {
        IsolationRequest {
            pinned: Vec::new(),
            accept_unrecorded: false,
            accept_missing: false,
        }
    }

    #[test]
    fn starting_a_session_waits_for_a_mod_change_in_progress() {
        let base = tempfile::tempdir().expect("temp dir");
        let (paths, host) = client_without_mods(base.path());
        let holder = App::open_at(paths.clone()).expect("open app");
        let change =
            crate::host_recipe_gate::hold_blocking(holder.registry(), host).expect("the gate");

        let starting = std::thread::spawn({
            let paths = paths.clone();
            move || {
                App::open_at(paths)
                    .expect("open app")
                    .begin_isolation(host, request(), &CancellationToken::new())
                    .map(drop)
            }
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !starting.is_finished(),
            "the session started under a change"
        );

        drop(change);
        starting
            .join()
            .expect("the starting thread finishes")
            .expect("the session starts once the change is done");
        let app = App::open_at(paths).expect("open app");
        assert!(app.isolation_state(host).expect("state").is_some());
    }

    #[test]
    fn starting_a_session_stops_waiting_for_a_mod_change_when_canceled() {
        let base = tempfile::tempdir().expect("temp dir");
        let (paths, host) = client_without_mods(base.path());
        let holder = App::open_at(paths.clone()).expect("open app");
        let _change =
            crate::host_recipe_gate::hold_blocking(holder.registry(), host).expect("the gate");
        let cancel = CancellationToken::new();

        let starting = std::thread::spawn({
            let cancel = cancel.clone();
            let paths = paths.clone();
            move || {
                App::open_at(paths)
                    .expect("open app")
                    .begin_isolation(host, request(), &cancel)
                    .map(drop)
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();

        let result = starting.join().expect("the starting thread finishes");
        assert!(result.is_err_and(|error| error.is_cancellation()));
        let app = App::open_at(paths).expect("open app");
        assert!(app.isolation_state(host).expect("state").is_none());
    }

    #[test]
    fn a_mod_change_that_waited_for_the_gate_is_refused_once_a_session_exists() {
        let base = tempfile::tempdir().expect("temp dir");
        let (paths, host) = client_without_mods(base.path());
        let app = App::open_at(paths).expect("open app");
        app.begin_isolation(host, request(), &CancellationToken::new())
            .expect("the session starts");

        let refused = app.hold_mod_change_blocking(host);

        assert!(matches!(
            refused,
            Err(CoreError::ChangeRefused {
                reason: RepairRefusal::IsolationActive
            })
        ));
        // The refusal released the gate it took.
        drop(crate::host_recipe_gate::hold_blocking(app.registry(), host).expect("the gate"));
    }
}
