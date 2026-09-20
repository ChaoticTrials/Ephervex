//! Dependency-aware mod isolation for one installed host.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_model::SnapshotId;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::dependencies::ModGraph;
use crate::{
    App, CoreError, CoreResult, DependencyKnowledge, InstalledHost, ModPresence, SnapshotError,
    SnapshotOrigin, SnapshotRestoreOptions,
};

const ISOLATION_VERSION: u32 = 1;
const ISOLATION_FILE: &str = "isolation.json";

/// A suspect the user can test, together with everything it requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationCandidate {
    pub root: PathBuf,
    pub name: String,
    pub closure: Vec<PathBuf>,
}

/// A required dependency whose jar is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IsolationMissingDependency {
    pub required_by: PathBuf,
    pub dependency: String,
}

/// A linked installation that this single-host workflow does not change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationLinkedServer {
    pub id: String,
    pub name: String,
}

/// An offline plan derived entirely from the shared dependency graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// Settings chosen before an isolation session changes the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<PathBuf>,
    #[serde(default)]
    pub accept_unrecorded: bool,
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
    /// Removing every remaining root was tested and stopped the issue.
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

/// Final report returned after the durable session marker is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsolationReport {
    pub safety_snapshot: SnapshotId,
    pub reproducing: Vec<IsolationResultCandidate>,
    pub excluded_roots: Vec<PathBuf>,
    pub enabled: Vec<PathBuf>,
    pub disabled: Vec<PathBuf>,
    pub rounds: usize,
    pub confidence: IsolationConfidence,
    pub reduced_confidence: bool,
    pub unrecorded: Vec<PathBuf>,
    pub unidentifiable: Vec<PathBuf>,
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
    #[error("the isolation result is not ready to finish")]
    NotFinished,
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
            Self::NotFinished => "error-isolation-not-finished",
            Self::HostMismatch { .. } => "error-isolation-host-mismatch",
            Self::Unreadable { .. } => "error-isolation-unreadable",
            Self::Unwritable { .. } => "error-isolation-unwritable",
            Self::Invalid { .. } => "error-isolation-invalid",
            Self::UnsupportedVersion { .. } => "error-isolation-version-newer",
            Self::Snapshot(error) => error.message_id(),
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
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        if path.exists() {
            return Err(IsolationError::AlreadyActive { host: host.id() }.into());
        }
        let plan = self.plan_isolation(host, &request.pinned)?;
        if !plan.missing.is_empty() {
            return Err(IsolationError::MissingDependencies {
                dependencies: plan.missing.clone(),
            }
            .into());
        }
        if !request.accept_unrecorded && !plan.unrecorded.is_empty() {
            return Err(IsolationError::UnrecordedDependencies {
                paths: plan.unrecorded.clone(),
            }
            .into());
        }
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
        let search = if suspects.len() <= 1 {
            SearchState {
                suspects: suspects.clone(),
                granularity: suspects.len().max(1),
                phase: SearchPhase::Confirmation,
                group_index: 0,
            }
        } else {
            SearchState {
                suspects: suspects.clone(),
                granularity: 2,
                phase: SearchPhase::Subsets,
                group_index: 0,
            }
        };
        let current_round = round_for(&plan, &search, 1);
        let session = IsolationSession {
            host_kind: host.kind().to_owned(),
            host_id: host.id(),
            started_at: Utc::now(),
            safety_snapshot: snapshot.id,
            accepted_unrecorded: request.accept_unrecorded && !plan.unrecorded.is_empty(),
            reduced_confidence: !plan.unidentifiable.is_empty()
                || (request.accept_unrecorded && !plan.unrecorded.is_empty()),
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
        Ok(step_for(&session))
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
        Ok(step_for(&session))
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

    /// Keeps the reproducing configuration and its safety snapshot, then
    /// removes the session marker.
    pub fn finish_isolation(&self, host: InstalledHost) -> CoreResult<IsolationReport> {
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        let session = self.read_required_session(host)?;
        let outcome = session.outcome.clone().ok_or(IsolationError::NotFinished)?;
        self.apply_isolation_configuration(host, &session, &outcome.enabled)?;
        self.rename_snapshot(
            host,
            session.safety_snapshot,
            &format!(
                "Before mod isolation, {}",
                session.started_at.format("%Y-%m-%d")
            ),
        )?;
        let report = report_for(&session, &outcome);
        remove_session(&path)?;
        Ok(report)
    }

    /// Restores the exact pre-isolation snapshot and removes the session marker.
    pub fn cancel_isolation(
        &self,
        host: InstalledHost,
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        self.refuse_isolation_while_running(host)?;
        let path = self.isolation_path(host)?;
        let session = self.read_required_session(host)?;
        self.restore_snapshot(
            host,
            session.safety_snapshot,
            SnapshotRestoreOptions {
                safety_snapshot: false,
            },
            cancel,
        )?;
        remove_session(&path)?;
        Ok(())
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
                .join(ISOLATION_FILE),
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
        self.set_mods_enabled(host, &changes)?;
        Ok(())
    }
}

fn step_for(session: &IsolationSession) -> IsolationStep {
    if let Some(contradiction) = &session.contradiction {
        IsolationStep::Contradiction(contradiction.clone())
    } else if let Some(outcome) = &session.outcome {
        IsolationStep::Complete(outcome.clone())
    } else if let Some(round) = &session.current_round {
        IsolationStep::AwaitingAnswer(round.clone())
    } else {
        // A valid document always has exactly one of these states. If a newer
        // additive document does not, treating it as a non-finishable result is
        // safer than inventing a configuration.
        IsolationStep::Complete(IsolationOutcome {
            roots: Vec::new(),
            enabled: session.intended_enabled().to_vec(),
            disabled: Vec::new(),
            confidence: IsolationConfidence::SmallestReached,
        })
    }
}

fn advance_search(session: &mut IsolationSession, answer: RoundAnswer) {
    let current = session.current_round.take();
    let Some(current) = current else {
        return;
    };
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
        session.search.suspects = current.enabled_roots;
        session.suspects = session.search.suspects.clone();
        if session.search.suspects.len() <= 1 {
            complete_search(session, IsolationConfidence::SmallestReached);
            return;
        }
        session.search.granularity = 2;
        session.search.phase = SearchPhase::Subsets;
        session.search.group_index = 0;
    } else {
        let group_count = split_groups(&session.search.suspects, session.search.granularity).len();
        if session.search.group_index + 1 < group_count {
            session.search.group_index += 1;
        } else if session.search.phase == SearchPhase::Subsets {
            session.search.phase = SearchPhase::Complements;
            session.search.group_index = 0;
        } else if session.search.granularity < session.search.suspects.len() {
            session.search.granularity =
                (session.search.granularity * 2).min(session.search.suspects.len());
            session.search.phase = SearchPhase::Subsets;
            session.search.group_index = 0;
        } else {
            complete_search(session, IsolationConfidence::OneMinimal);
            return;
        }
    }
    session.suspects = session.search.suspects.clone();
    session.granularity = session.search.granularity;
    let index = session.history.len() as u32 + 1;
    session.current_round = Some(round_for(&session.plan, &session.search, index));
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
    session.suspects = roots.clone();
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
    let roots = match search.phase {
        SearchPhase::Confirmation => search.suspects.clone(),
        SearchPhase::Subsets => groups.get(search.group_index).cloned().unwrap_or_default(),
        SearchPhase::Complements => {
            let excluded: BTreeSet<_> = groups
                .get(search.group_index)
                .into_iter()
                .flat_map(|group| group.iter())
                .collect();
            search
                .suspects
                .iter()
                .filter(|root| !excluded.contains(root))
                .cloned()
                .collect()
        }
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
    let reproducing = session
        .plan
        .candidates
        .iter()
        .filter(|candidate| roots.contains(&candidate.root))
        .map(|candidate| IsolationResultCandidate {
            root: candidate.root.clone(),
            name: candidate.name.clone(),
            closure: candidate.closure.clone(),
        })
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
        reproducing,
        excluded_roots,
        enabled: outcome.enabled.clone(),
        disabled: outcome.disabled.clone(),
        rounds: session.history.len(),
        confidence: outcome.confidence,
        reduced_confidence: session.reduced_confidence,
        unrecorded: session.plan.unrecorded.clone(),
        unidentifiable: session.plan.unidentifiable.clone(),
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
    Ok(Some(document.session))
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
        // lexicographically first member represents it deterministically.
        let required_by_other = pool.iter().any(|other| {
            other != path
                && closures[other].1.contains(path)
                && (!closure.contains(other) || other < path)
        });
        if !required_by_other {
            plan.candidates.push(IsolationCandidate {
                root: path.clone(),
                name: name.clone(),
                closure: closure.iter().cloned().collect(),
            });
        }
    }
}
