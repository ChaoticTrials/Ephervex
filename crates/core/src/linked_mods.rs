//! Carrying one explicit client mod decision to that client's linked servers.
//!
//! # The rule the whole module exists to hold
//!
//! > The exact root release the client ends up with is the exact root release
//! > every selected linked server is evaluated for.
//!
//! Not "the newest release that fits the server", and not "whatever the
//! server's dependency resolution happens to prefer". Running the newest-
//! compatible search once per target is how one click ends up producing client
//! v5, server A v4, and server B v6 — three different builds of the mod the
//! user was trying to reproduce a bug with, which is the opposite of
//! synchronization. A release that does not fit a server makes *that server*
//! ineligible and nothing else; the client operation carries on without it.
//!
//! Dependencies are the other half of the same rule, inverted. They are
//! resolved per target, against that server's own Minecraft release, loader,
//! and installed set, because a server legitimately needs things a client does
//! not. Only the root is pinned.
//!
//! # Provenance
//!
//! A server reconstructed from an imported pack gets the pack's content through
//! reconstruction, and those entries stay [`ModOrigin::PackDeclared`] — the
//! pack's declaration is a historical record, not a wish list, so nothing here
//! rewrites one to pretend a later mod or a later release shipped with it. A
//! mod synced afterwards is [`ModOrigin::UserSelected`] on the server, and what
//! it drags in is [`ModOrigin::RequiredDependency`], exactly as on a client. The
//! three stay tellable apart in the server's own retained recipe, which is
//! where the server's ownership is proven and re-read after a restart.
//!
//! # Not globally atomic, on purpose
//!
//! Each target's mutation is one transaction with the existing plan → fetch →
//! retire → write → publish → discard ordering, so a failure leaves *that*
//! target exactly as it was. Several targets are not one transaction and are
//! not pretended to be: there is no cross-instance filesystem transaction to
//! have, and compensating by undoing a target that succeeded would destroy
//! working state to make a report look tidier. The outcome is per target and
//! says which ones succeeded.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, DownloadManager, DownloaderConfig, OperationReporter};
use ephervex_mod_platform::{
    Compatibility, IncompatibilityReason, InstalledPolicy, InstanceTarget, ModInstallPlan,
    ModPlatformError, ModProject, ModVersionInfo, PlannedMod, SideSupport,
};
use ephervex_model::{InstanceId, LoaderSelection, ModProvider, ServerId};
use ephervex_pack::{
    ManagedMutation, ManagedMutationReport, ManagedProject, OptionalSelection, PackError,
    PackFileRecipe, PackRecipe, PackSide, SideRequirement, SourceStore,
};
use ephervex_server::classify_runtime;
use ephervex_storage::ServerRecord;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};
use crate::mods::{InstalledMods, ModInstallRequest};
use crate::mutation::{
    DependencyScope, ModMutation, ModMutationPlan, ModVersionChoice, PlannedFile,
};

/// Which client mod decision is being carried to linked servers.
///
/// One request type rather than four entry points, because everything after
/// "which exact release does the client end up with" is identical for all of
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedModRequest {
    /// A mod chosen by name that the client does not have yet.
    Install {
        project: String,
    },
    /// An exact release, for a mod the client already has.
    ChangeVersion {
        target: PathBuf,
        version: String,
    },
    /// The newest release that fits the *client*, for a mod it already has.
    Update {
        target: PathBuf,
    },
    Remove {
        target: PathBuf,
    },
}

impl LinkedModRequest {
    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Install { .. } => "install",
            Self::ChangeVersion { .. } => "change-version",
            Self::Update { .. } => "update",
            Self::Remove { .. } => "remove",
        }
    }

    /// The mod this acts on, for the operations that name one.
    #[must_use]
    pub fn target(&self) -> Option<&Path> {
        match self {
            Self::Install { .. } => None,
            Self::ChangeVersion { target, .. }
            | Self::Update { target }
            | Self::Remove { target } => Some(target),
        }
    }

    /// Whether this operation takes the mod away rather than putting one there.
    #[must_use]
    pub const fn is_removal(&self) -> bool {
        matches!(self, Self::Remove { .. })
    }
}

/// The one release a linked operation is about.
///
/// Provider-neutral: the provider is a field, not a type. Nothing below this
/// point knows which catalog it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedModRoot {
    pub provider: ModProvider,
    pub project_id: String,
    /// The project's title where the provider published one, and the mod's own
    /// file name otherwise.
    pub name: String,
    /// The exact release every selected server is evaluated for. `None` only
    /// for a removal, which names no release.
    pub version_id: Option<String>,
    pub version_number: Option<String>,
    /// What the project declares about the server side, verbatim. `Unknown`
    /// stays `Unknown`: it is not a synonym for `Required`.
    pub server_side: SideRequirement,
}

/// Why a linked server cannot take part in this operation.
///
/// Typed rather than a sentence, because each of these is a different thing for
/// a user to do something about, and a frontend that had to infer them from a
/// failed mutation would be re-deciding policy that belongs here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedTargetBlock {
    /// The project declares that it does not support servers at all.
    ServerUnsupported,
    /// The exact release does not list this server's Minecraft version.
    IncompatibleMinecraft { supported: Vec<String> },
    /// The exact release does not list this server's loader.
    IncompatibleLoader { supported: Vec<String> },
    /// The release exists but cannot be installed here: withdrawn, a
    /// dependency that does not fit this server, or a conflicting pin. Never a
    /// reason to look for a different release.
    ExactReleaseUnavailable,
    /// Changing a running server's mods is not something this build can do
    /// safely, so it refuses rather than writing under a live process.
    ServerRunning,
    /// The server has no complete base installation yet.
    ServerNotInstalled,
    /// The relationship this plan assumed is gone.
    NoLongerLinked,
    /// The catalog could not be reached, so nothing about this target can be
    /// established. Its state is left exactly as it is.
    ProviderUnavailable,
    /// The server's own retained desired state could not be read, so there is
    /// nothing to prove ownership against.
    StateUnavailable,
    /// The server's pack declares this mod. Pack-owned content is reconstructed,
    /// never independently mutated.
    PackOwned,
    /// Another of this server's mods requires this one, so its release follows
    /// what needs it and its lifetime is not this operation's to end.
    ServerDependency,
}

impl LinkedTargetBlock {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::ServerUnsupported => "linked-target-server-unsupported",
            Self::IncompatibleMinecraft { .. } => "linked-target-incompatible-minecraft",
            Self::IncompatibleLoader { .. } => "linked-target-incompatible-loader",
            Self::ExactReleaseUnavailable => "linked-target-exact-release-unavailable",
            Self::ServerRunning => "linked-target-server-running",
            Self::ServerNotInstalled => "linked-target-server-not-installed",
            Self::NoLongerLinked => "linked-target-no-longer-linked",
            Self::ProviderUnavailable => "linked-target-provider-unavailable",
            Self::StateUnavailable => "linked-target-state-unavailable",
            Self::PackOwned => "linked-target-pack-owned",
            Self::ServerDependency => "linked-target-server-dependency",
        }
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ServerUnsupported => "server-unsupported",
            Self::IncompatibleMinecraft { .. } => "incompatible-minecraft",
            Self::IncompatibleLoader { .. } => "incompatible-loader",
            Self::ExactReleaseUnavailable => "exact-release-unavailable",
            Self::ServerRunning => "server-running",
            Self::ServerNotInstalled => "server-not-installed",
            Self::NoLongerLinked => "no-longer-linked",
            Self::ProviderUnavailable => "provider-unavailable",
            Self::StateUnavailable => "state-unavailable",
            Self::PackOwned => "pack-owned",
            Self::ServerDependency => "server-dependency",
        }
    }
}

/// What this operation would do to one linked server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedTargetState {
    /// The server does not have this project and would receive the exact root
    /// release the client chose.
    WillInstall,
    /// The server manages this project at another release and would move to the
    /// exact one the client chose.
    WillChangeVersion {
        from: String,
        to: String,
    },
    /// The server's record already names this exact release and its jar is
    /// gone, so the operation puts it back without changing what is recorded.
    WillRepair,
    /// The server durably manages this synced root and would give it up.
    WillRemove,
    /// The server already has exactly this release. Nothing is written.
    AlreadyCurrent,
    /// The server never received this root, so a removal has nothing to take
    /// from it.
    NotPresent,
    Blocked(LinkedTargetBlock),
}

impl LinkedTargetState {
    /// Whether carrying this out would write to the server.
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        matches!(
            self,
            Self::WillInstall
                | Self::WillChangeVersion { .. }
                | Self::WillRepair
                | Self::WillRemove
        )
    }

    /// Why the server cannot take part, when it cannot.
    #[must_use]
    pub const fn block(&self) -> Option<&LinkedTargetBlock> {
        match self {
            Self::Blocked(block) => Some(block),
            _ => None,
        }
    }

    /// The `ephervex-i18n` message id naming this state to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::WillInstall => "linked-target-will-install",
            Self::WillChangeVersion { .. } => "linked-target-will-change-version",
            Self::WillRepair => "linked-target-will-repair",
            Self::WillRemove => "linked-target-will-remove",
            Self::AlreadyCurrent => "linked-target-already-current",
            Self::NotPresent => "linked-target-not-present",
            Self::Blocked(block) => block.message_id(),
        }
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::WillInstall => "will-install",
            Self::WillChangeVersion { .. } => "will-change-version",
            Self::WillRepair => "will-repair",
            Self::WillRemove => "will-remove",
            Self::AlreadyCurrent => "already-current",
            Self::NotPresent => "not-present",
            Self::Blocked(block) => block.as_str(),
        }
    }
}

/// One linked server, as a confirmation has to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedTargetPlan {
    /// The stable identity everything is keyed on. Never a list position: a
    /// server deleted while a confirmation is open must not let the next one
    /// inherit its row.
    pub server: ServerId,
    pub name: String,
    /// The server's *own* current release and loader, not the client's.
    pub minecraft_version: String,
    pub loader: LoaderSelection,
    pub state: LinkedTargetState,
    /// Whether this target starts selected. See [`default_selection`] for the
    /// policy and why it is conservative.
    pub selected_by_default: bool,
    /// Mods this target would write, the root first.
    pub installs: Vec<PlannedFile>,
    /// Mods this target would take away: the root for a removal, plus whatever
    /// automatic dependency is left with no owner.
    pub removes: Vec<PlannedFile>,
    /// True when the catalog could not confirm which of this server's
    /// dependencies are still needed, so none were collected.
    pub orphans_unprovable: bool,
}

impl LinkedTargetPlan {
    /// Dependencies this target would take away, which is what a confirmation
    /// has to show before it is honest.
    pub fn collected_dependencies(&self) -> impl Iterator<Item = &PlannedFile> {
        self.removes
            .iter()
            .filter(|file| file.origin.is_dependency())
    }
}

/// What the client half of the operation would do.
///
/// Two shapes because they genuinely are two: an install resolves a graph, and
/// a change, update, or removal works out what the instance would lose. Both
/// already exist and both are what the frontends render.
#[derive(Debug, Clone)]
pub enum ClientModPlan {
    Install(Box<ModInstallPlan>),
    Mutate(Box<ModMutationPlan>),
}

/// Everything one linked operation would do, worked out without writing.
#[derive(Debug, Clone)]
pub struct LinkedModPlan {
    pub client: InstanceId,
    pub request: LinkedModRequest,
    pub root: LinkedModRoot,
    pub client_plan: ClientModPlan,
    /// One entry per linked server, whether or not it can take part. A target
    /// nothing can be done with is still shown, with the reason.
    pub targets: Vec<LinkedTargetPlan>,
}

impl LinkedModPlan {
    /// The targets a user may choose.
    pub fn actionable_targets(&self) -> impl Iterator<Item = &LinkedTargetPlan> {
        self.targets
            .iter()
            .filter(|target| target.state.is_actionable())
    }

    /// The targets that start selected.
    #[must_use]
    pub fn default_selection(&self) -> Vec<ServerId> {
        self.targets
            .iter()
            .filter(|target| target.selected_by_default)
            .map(|target| target.server)
            .collect()
    }

    /// Whether any linked server is worth putting in front of the user.
    ///
    /// False when the client has no linked servers, and when the mod is one no
    /// server could ever run: neither is a decision, and offering an empty
    /// section as one is noise.
    #[must_use]
    pub fn has_relevant_targets(&self) -> bool {
        !self.targets.is_empty()
            && !self.targets.iter().all(|target| {
                matches!(
                    target.state,
                    LinkedTargetState::Blocked(LinkedTargetBlock::ServerUnsupported)
                )
            })
    }
}

/// Why one target's mutation failed, in terms a frontend can localize.
///
/// Not the error itself: the outcome of a multi-target operation is reported
/// after the fact and has to be cloneable and storable, and every failure the
/// application layer produces already carries a stable message id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedTargetFailure {
    pub message_id: &'static str,
    /// The failure's own words, kept exact. Technical values such as paths,
    /// versions, and hashes are not translated.
    pub detail: String,
}

impl From<&CoreError> for LinkedTargetFailure {
    fn from(error: &CoreError) -> Self {
        Self {
            message_id: error.message_id(),
            detail: error.to_string(),
        }
    }
}

/// What actually happened to one linked server.
#[derive(Debug, Clone)]
pub enum LinkedTargetResult {
    /// The mutation ran. Carries what it changed.
    Applied(Box<ManagedMutationReport>),
    /// Revalidation at execution time found nothing to do here, or found the
    /// target no longer eligible. The server is untouched.
    Skipped(LinkedTargetState),
    /// The mutation failed. The server keeps the state it had.
    Failed(LinkedTargetFailure),
}

impl LinkedTargetResult {
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self, Self::Applied(_))
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Applied(_) => "applied",
            Self::Skipped(_) => "skipped",
            Self::Failed(_) => "failed",
        }
    }
}

/// One target's result, keyed by the server it belongs to.
#[derive(Debug, Clone)]
pub struct LinkedTargetOutcome {
    pub server: ServerId,
    pub name: String,
    pub result: LinkedTargetResult,
}

/// What the client half of the operation did.
#[derive(Debug)]
pub enum ClientModOutcome {
    Installed(Box<InstalledMods>),
    Mutated(Box<ModMutation>),
}

/// The result of one linked operation, per target.
///
/// Deliberately not reduced to a single success or failure. "The client and one
/// server succeeded and the other server was already running" is the truth, and
/// a frontend that showed one red toast would be hiding two successful targets
/// and inviting the user to run the whole thing again.
#[derive(Debug)]
pub struct LinkedModOutcome {
    pub client: ClientModOutcome,
    pub root: LinkedModRoot,
    /// One entry per server the caller selected, in the order they were given.
    pub targets: Vec<LinkedTargetOutcome>,
}

impl LinkedModOutcome {
    /// Servers this operation actually changed.
    pub fn succeeded(&self) -> impl Iterator<Item = &LinkedTargetOutcome> {
        self.targets
            .iter()
            .filter(|target| target.result.succeeded())
    }

    /// Servers this operation did not change, whether refused or failed.
    pub fn unfinished(&self) -> impl Iterator<Item = &LinkedTargetOutcome> {
        self.targets
            .iter()
            .filter(|target| !target.result.succeeded())
    }

    /// Whether some targets succeeded and others did not, which is the case a
    /// single-line report cannot tell the truth about.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.succeeded().count() > 0 && self.unfinished().count() > 0
    }
}

/// Whether a target starts selected.
///
/// The policy, in one place so it cannot be re-decided per screen:
///
/// * a server that **already manages this root** stays in step by default — a
///   version change, a repair, or a removal on such a server is the operation
///   the user just asked for, applied where they already applied it;
/// * a server that does **not** have the root yet joins by default only when
///   the project declares the server side `required`;
/// * `optional` and `unknown` are offered and never preselected. They are
///   different statements — one is the author saying "your choice", the other
///   is the author saying nothing — and neither is a reason to write to a
///   server the user did not ask about;
/// * nothing blocked is ever selected.
#[must_use]
fn default_selection(state: &LinkedTargetState, server_side: SideRequirement) -> bool {
    match state {
        LinkedTargetState::WillChangeVersion { .. }
        | LinkedTargetState::WillRepair
        | LinkedTargetState::WillRemove => true,
        LinkedTargetState::WillInstall => server_side == SideRequirement::Required,
        _ => false,
    }
}

/// Everything one target's transaction needs, kept from planning so that
/// executing does not resolve the same graph twice.
#[derive(Debug)]
struct TargetMutation {
    game_dir: PathBuf,
    retired_dir: PathBuf,
    store: SourceStore,
    recipe: PackRecipe,
    upserts: Vec<PackFileRecipe>,
    removals: Vec<ManagedProject>,
}

/// A planned target, plus what carrying it out would take.
#[derive(Debug)]
struct ResolvedTarget {
    plan: LinkedTargetPlan,
    /// Present exactly when [`LinkedTargetState::is_actionable`] is true.
    mutation: Option<TargetMutation>,
}

impl App {
    /// Works out what one linked mod operation would do to the client and to
    /// every server linked to it.
    ///
    /// Nothing is written. Every server is evaluated against its own current
    /// Minecraft release, loader, runtime state, and desired state — the
    /// client's compatibility result is never taken as proof for a server,
    /// because a linked server's exact loader build can be changed
    /// independently and often has been.
    #[instrument(skip(self, reporter, cancel), fields(client = %client, operation = request.as_str()))]
    pub async fn plan_linked_mod_operation(
        &self,
        client: InstanceId,
        request: &LinkedModRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<LinkedModPlan> {
        let (client_plan, root, release) = self.client_half(client, request, reporter).await?;

        let mut targets = Vec::new();
        for server in self.linked_server_ids(client)? {
            if let Some(resolved) = self
                .resolve_target(client, server, &root, release.as_ref(), reporter, cancel)
                .await
            {
                targets.push(resolved.plan);
            }
        }

        Ok(LinkedModPlan {
            client,
            request: request.clone(),
            root,
            client_plan,
            targets,
        })
    }

    /// Carries out one linked mod operation on the client and the chosen
    /// servers.
    ///
    /// The client runs first and its failure aborts the whole operation: the
    /// exact release the servers are evaluated for is defined by what the
    /// client ends up with, so there is nothing to synchronize to.
    ///
    /// After that, each server is revalidated from scratch and mutated in its
    /// own transaction. A target that has become ineligible is skipped and one
    /// whose mutation fails keeps its previous state; neither stops the rest.
    #[instrument(
        skip(self, request, servers, reporter, cancel),
        fields(client = %client, operation = request.as_str(), targets = servers.len())
    )]
    pub async fn apply_linked_mod_operation(
        &self,
        client: InstanceId,
        request: &LinkedModRequest,
        servers: &[ServerId],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<LinkedModOutcome> {
        let (outcome, root, release) = self
            .apply_client_half(client, request, reporter, cancel)
            .await?;

        // Deduplicated so a frontend that sends the same server twice cannot
        // schedule two mutations of one directory, and ordered as the caller
        // gave them so the report reads the way the confirmation did.
        let mut seen = BTreeSet::new();
        let mut targets = Vec::new();
        for server in servers.iter().copied().filter(|id| seen.insert(*id)) {
            targets.push(
                self.apply_target(client, server, &root, release.as_ref(), reporter, cancel)
                    .await,
            );
        }

        info!(
            applied = targets.iter().filter(|it| it.result.succeeded()).count(),
            unfinished = targets.iter().filter(|it| !it.result.succeeded()).count(),
            "carried a mod decision to linked servers"
        );
        Ok(LinkedModOutcome {
            client: outcome,
            root,
            targets,
        })
    }

    // -- the client half --------------------------------------------------

    /// Plans the client operation and works out the exact release it names.
    async fn client_half(
        &self,
        client: InstanceId,
        request: &LinkedModRequest,
        reporter: &OperationReporter,
    ) -> CoreResult<(ClientModPlan, LinkedModRoot, Option<ModVersionInfo>)> {
        let cancel = CancellationToken::new();
        match request {
            LinkedModRequest::Install { project } => {
                let plan = self
                    .plan_mod_installation(client, &ModInstallRequest::new(project.clone()))
                    .await?;
                let root = root_of_install(&plan);
                let release = plan.root.version.clone();
                Ok((ClientModPlan::Install(Box::new(plan)), root, Some(release)))
            }
            LinkedModRequest::ChangeVersion { target, version } => {
                let plan = self
                    .plan_mod_version_change(
                        client,
                        target,
                        &ModVersionChoice::Exact(version.clone()),
                        reporter,
                        &cancel,
                    )
                    .await?;
                self.mutation_half(plan, reporter, &cancel).await
            }
            LinkedModRequest::Update { target } => {
                let plan = self
                    .plan_mod_version_change(
                        client,
                        target,
                        &ModVersionChoice::NewestCompatible,
                        reporter,
                        &cancel,
                    )
                    .await?;
                self.mutation_half(plan, reporter, &cancel).await
            }
            LinkedModRequest::Remove { target } => {
                let plan = self
                    .plan_mod_removal(client, target, reporter, &cancel)
                    .await?;
                let root = root_of_removal(&plan);
                Ok((ClientModPlan::Mutate(Box::new(plan)), root, None))
            }
        }
    }

    /// Completes a change or update plan with the release and project facts the
    /// linked half needs.
    ///
    /// A mutation plan names the exact release it chose; what it does not carry
    /// is that release's own compatibility and the project's declared server
    /// support, and both are needed before a server can be evaluated. Both come
    /// from the catalog's metadata cache, so a repeated open costs nothing.
    async fn mutation_half(
        &self,
        plan: ModMutationPlan,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<(ClientModPlan, LinkedModRoot, Option<ModVersionInfo>)> {
        let (Some(project_id), Some(version_id)) = (&plan.project_id, &plan.candidate_version)
        else {
            // Nothing with an unproven identity gets here: the mutation plan
            // refuses first. Reaching this arm means there is no release to
            // synchronize, so no server is offered one.
            let root = root_of_removal(&plan);
            return Ok((ClientModPlan::Mutate(Box::new(plan)), root, None));
        };

        let provider = self.mod_provider()?;
        let project = provider.project(project_id, reporter, cancel).await?;
        let release = provider
            .versions_by_id(std::slice::from_ref(version_id), reporter, cancel)
            .await?
            .into_iter()
            .find(|version| &version.id == version_id)
            .ok_or_else(|| ModPlatformError::VersionUnknown {
                provider: provider.provider(),
                version_id: version_id.clone(),
            })?;

        let root = LinkedModRoot {
            provider: project.provider,
            project_id: project.id.clone(),
            name: display_name(&project, &plan.name),
            version_id: Some(release.id.clone()),
            version_number: Some(release.version_number.clone()),
            server_side: side_requirement(project.server_side),
        };
        Ok((ClientModPlan::Mutate(Box::new(plan)), root, Some(release)))
    }

    /// Runs the client operation and reports the exact release it settled on.
    async fn apply_client_half(
        &self,
        client: InstanceId,
        request: &LinkedModRequest,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<(ClientModOutcome, LinkedModRoot, Option<ModVersionInfo>)> {
        match request {
            LinkedModRequest::Install { project } => {
                let installed = self
                    .install_mods(
                        client,
                        &ModInstallRequest::new(project.clone()),
                        reporter,
                        cancel,
                    )
                    .await?;
                let root = root_of_install(&installed.plan);
                let release = installed.plan.root.version.clone();
                Ok((
                    ClientModOutcome::Installed(Box::new(installed)),
                    root,
                    Some(release),
                ))
            }
            LinkedModRequest::ChangeVersion { target, version } => {
                let mutation = self
                    .change_managed_mod_version(
                        client,
                        target,
                        &ModVersionChoice::Exact(version.clone()),
                        reporter,
                        cancel,
                    )
                    .await?;
                self.applied_mutation_half(mutation, reporter, cancel).await
            }
            LinkedModRequest::Update { target } => {
                let mutation = self
                    .update_managed_mod(client, target, reporter, cancel)
                    .await?;
                self.applied_mutation_half(mutation, reporter, cancel).await
            }
            LinkedModRequest::Remove { target } => {
                let mutation = self
                    .remove_managed_mod(client, target, reporter, cancel)
                    .await?;
                let root = root_of_removal(&mutation.plan);
                Ok((ClientModOutcome::Mutated(Box::new(mutation)), root, None))
            }
        }
    }

    async fn applied_mutation_half(
        &self,
        mutation: ModMutation,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<(ClientModOutcome, LinkedModRoot, Option<ModVersionInfo>)> {
        let (_, root, release) = self
            .mutation_half(mutation.plan.clone(), reporter, cancel)
            .await?;
        Ok((ClientModOutcome::Mutated(Box::new(mutation)), root, release))
    }

    // -- one linked server ------------------------------------------------

    /// Evaluates one linked server against the exact root release.
    ///
    /// `None` when the server is gone entirely, which is the honest answer to
    /// "what would this do to it": nothing, because it is not there. Everything
    /// else — unlinked, running, incompatible, unreadable — is a target with a
    /// reason, because those are states a user can act on.
    async fn resolve_target(
        &self,
        client: InstanceId,
        server: ServerId,
        root: &LinkedModRoot,
        release: Option<&ModVersionInfo>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> Option<ResolvedTarget> {
        let record = match self.server(server) {
            Ok(record) => record,
            Err(error) => {
                warn!(%server, %error, "a linked server disappeared while its target was being planned");
                return None;
            }
        };

        if record.linked_client_id != Some(client) {
            return Some(blocked(&record, root, LinkedTargetBlock::NoLongerLinked));
        }
        if !record.installation.is_installed() {
            return Some(blocked(
                &record,
                root,
                LinkedTargetBlock::ServerNotInstalled,
            ));
        }
        // An unreadable runtime state counts as "possibly running" for the same
        // reason `server_status` treats it that way: writing into a live
        // server's mods directory is the failure worth avoiding, and a refusal
        // costs a retry.
        let stopped = classify_runtime(&record.root).is_ok_and(|runtime| !runtime.may_be_running());
        if !stopped {
            return Some(blocked(&record, root, LinkedTargetBlock::ServerRunning));
        }

        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        if !store.has_recipe() {
            return Some(blocked(&record, root, LinkedTargetBlock::StateUnavailable));
        }
        let recipe = match store.read_recipe() {
            Ok(recipe) => recipe,
            Err(error) => {
                warn!(%server, %error, "a linked server's retained desired state could not be read");
                return Some(blocked(&record, root, LinkedTargetBlock::StateUnavailable));
            }
        };

        // The server's own values, never the client's. A linked server's exact
        // loader build can be changed independently, so its record is the only
        // thing that says what it currently is.
        let target = InstanceTarget::new(record.minecraft_version.clone(), record.loader.kind);
        let existing = recipe
            .managed_project(root.provider, &root.project_id)
            .cloned();
        if let Some(entry) = &existing {
            if entry.origin.is_pack_owned() {
                return Some(blocked(&record, root, LinkedTargetBlock::PackOwned));
            }
            if entry.origin.is_dependency() {
                return Some(blocked(&record, root, LinkedTargetBlock::ServerDependency));
            }
        }

        Some(match release {
            Some(release) => {
                self.resolve_installing_target(
                    &record, root, release, &target, recipe, store, existing, reporter, cancel,
                )
                .await
            }
            None => {
                self.resolve_removing_target(
                    &record, root, &target, recipe, store, existing, reporter, cancel,
                )
                .await
            }
        })
    }

    /// The target for an operation that puts an exact release on the server.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_installing_target(
        &self,
        record: &ServerRecord,
        root: &LinkedModRoot,
        release: &ModVersionInfo,
        target: &InstanceTarget,
        recipe: PackRecipe,
        store: SourceStore,
        existing: Option<PackFileRecipe>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ResolvedTarget {
        if root.server_side == SideRequirement::Unsupported {
            return blocked(record, root, LinkedTargetBlock::ServerUnsupported);
        }

        let Ok(provider) = self.mod_provider() else {
            return blocked(record, root, LinkedTargetBlock::ProviderUnavailable);
        };
        let Some(loader_name) = provider.loader_name(target.loader) else {
            // A server with no mod loader cannot host mods at all. That is a
            // loader answer, and the release's own loader list is what the user
            // needs in front of it.
            return blocked(
                record,
                root,
                LinkedTargetBlock::IncompatibleLoader {
                    supported: release.loaders.clone(),
                },
            );
        };

        // The exact release, checked against this server. Never a search for a
        // release that would fit better.
        if let Compatibility::Incompatible(reasons) = release.compatibility(target, loader_name) {
            return blocked(record, root, incompatibility(&reasons, release));
        }

        let installed = crate::mods::installed_projects_of(Some(&recipe));
        let request = ModInstallRequest::new(root.project_id.clone())
            .at_version(root.version_id.clone().unwrap_or_default());
        let resolved = match self
            .resolve_against(
                target,
                &installed,
                &request,
                InstalledPolicy::Replace,
                reporter,
                cancel,
            )
            .await
        {
            Ok(resolved) => resolved,
            Err(error) => return blocked(record, root, resolution_block(&error)),
        };

        let game_dir = ephervex_storage::metadata::server_game_dir(&record.root);
        let state = installing_state(existing.as_ref(), root, &game_dir);

        let mut upserts = Vec::new();
        for planned in server_applicable(&resolved) {
            match crate::mods::recipe_entry_for(planned, resolved.provider, PackSide::Server) {
                Ok(entry) => upserts.push(entry),
                // A provider file name that cannot safely name a destination is
                // refused rather than sanitized into one that writes somewhere.
                Err(_) => {
                    return blocked(record, root, LinkedTargetBlock::ExactReleaseUnavailable);
                }
            }
        }

        // Orphans are only collected when something actually leaves the graph.
        // An install adds; making it also sweep an unrelated dependency the
        // server has carried for months would be a second operation nobody
        // asked for.
        let (removes, orphans_unprovable) =
            if matches!(state, LinkedTargetState::WillChangeVersion { .. }) {
                self.orphaned_dependencies(
                    &DependencyScope {
                        recipe: &recipe,
                        side: PackSide::Server,
                        target,
                    },
                    root.version_id
                        .as_deref()
                        .map(|version| (root.project_id.as_str(), version)),
                    &[],
                    reporter,
                    cancel,
                )
                .await
            } else {
                (Vec::new(), false)
            };

        let installs = upserts
            .iter()
            .map(|entry| planned_from_entry(entry, &resolved))
            .collect();
        let removals = removals_of(&removes);
        let plan = LinkedTargetPlan {
            server: record.id,
            name: record.name.clone(),
            minecraft_version: record.minecraft_version.clone(),
            loader: record.loader.clone(),
            selected_by_default: default_selection(&state, root.server_side),
            installs,
            removes,
            orphans_unprovable,
            state: state.clone(),
        };
        let mutation = state.is_actionable().then(|| TargetMutation {
            game_dir,
            retired_dir: ephervex_storage::metadata::retired_dir(&record.root),
            store,
            recipe,
            upserts,
            removals,
        });
        ResolvedTarget { plan, mutation }
    }

    /// The target for an operation that takes a synced root off the server.
    #[allow(clippy::too_many_arguments)]
    async fn resolve_removing_target(
        &self,
        record: &ServerRecord,
        root: &LinkedModRoot,
        target: &InstanceTarget,
        recipe: PackRecipe,
        store: SourceStore,
        existing: Option<PackFileRecipe>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> ResolvedTarget {
        // Ownership has to be proven, not inferred. A server that never
        // received this root gains no removal target, and a jar that merely
        // shares the client's file name is not evidence of anything.
        let Some(entry) = existing
            .filter(|entry| entry.is_installed_on(PackSide::Server, &OptionalSelection::none()))
        else {
            return unactionable(record, root, LinkedTargetState::NotPresent);
        };
        let Some(recorded) = entry.recorded_mod() else {
            return unactionable(record, root, LinkedTargetState::NotPresent);
        };

        let mut removes = vec![crate::mutation::planned_from_recorded(&recorded)];
        let (orphans, orphans_unprovable) = self
            .orphaned_dependencies(
                &DependencyScope {
                    recipe: &recipe,
                    side: PackSide::Server,
                    target,
                },
                None,
                &[root.project_id.as_str()],
                reporter,
                cancel,
            )
            .await;
        removes.extend(orphans);

        let removals = removals_of(&removes);
        let state = LinkedTargetState::WillRemove;
        let plan = LinkedTargetPlan {
            server: record.id,
            name: record.name.clone(),
            minecraft_version: record.minecraft_version.clone(),
            loader: record.loader.clone(),
            selected_by_default: default_selection(&state, root.server_side),
            installs: Vec::new(),
            removes,
            orphans_unprovable,
            state,
        };
        ResolvedTarget {
            plan,
            mutation: Some(TargetMutation {
                game_dir: ephervex_storage::metadata::server_game_dir(&record.root),
                retired_dir: ephervex_storage::metadata::retired_dir(&record.root),
                store,
                recipe,
                upserts: Vec::new(),
                removals,
            }),
        }
    }

    /// Revalidates one target and, if it still holds, mutates it.
    async fn apply_target(
        &self,
        client: InstanceId,
        server: ServerId,
        root: &LinkedModRoot,
        release: Option<&ModVersionInfo>,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> LinkedTargetOutcome {
        // Planned again from scratch rather than trusted. A server can be
        // started, unlinked, deleted, or moved to another loader build between
        // a confirmation opening and the user pressing apply, and asking again
        // with the same exact pin is the only way to notice.
        let Some(resolved) = self
            .resolve_target(client, server, root, release, reporter, cancel)
            .await
        else {
            return LinkedTargetOutcome {
                server,
                name: server.to_string(),
                result: LinkedTargetResult::Skipped(LinkedTargetState::Blocked(
                    LinkedTargetBlock::NoLongerLinked,
                )),
            };
        };

        let ResolvedTarget { plan, mutation } = resolved;
        let Some(mutation) = mutation else {
            return LinkedTargetOutcome {
                server,
                name: plan.name,
                result: LinkedTargetResult::Skipped(plan.state),
            };
        };

        let result = match self.commit_target(mutation, reporter, cancel).await {
            Ok(report) => LinkedTargetResult::Applied(Box::new(report)),
            Err(error) => {
                warn!(%server, %error, "a linked server kept its previous mods after a failed mutation");
                LinkedTargetResult::Failed(LinkedTargetFailure::from(&error))
            }
        };
        LinkedTargetOutcome {
            server,
            name: plan.name,
            result,
        }
    }

    /// Runs one server's mutation transaction.
    ///
    /// The same transaction a client mutation uses, against the server's own
    /// desired state, game directory, and scratch. Artifacts come out of the
    /// shared verified cache, so a root both sides need is fetched once and the
    /// client's installed jar — mutable instance state — is never the source.
    async fn commit_target(
        &self,
        mutation: TargetMutation,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ManagedMutationReport> {
        let TargetMutation {
            game_dir,
            retired_dir,
            store,
            mut recipe,
            upserts,
            removals,
        } = mutation;

        let manager = DownloadManager::new(DownloaderConfig {
            http: self.http_config(),
            ..DownloaderConfig::default()
        })
        .map_err(|source| PackError::DownloadFailed {
            path: game_dir.display().to_string(),
            source: Box::new(source),
        })?;

        // Anything an interrupted mutation could not clean up goes now, so the
        // directory cannot grow across operations.
        ephervex_pack::clear_retired(&retired_dir);

        Ok(ephervex_pack::mutate_managed(ManagedMutation {
            recipe: &mut recipe,
            upserts: &upserts,
            removals: &removals,
            game_dir: &game_dir,
            retired_dir: &retired_dir,
            store: &store,
            manager: &manager,
            cache: &CacheLayout::new(self.paths().cache_dir()),
            reporter,
            cancel,
        })
        .await?)
    }
}

/// A target that cannot take part, with the reason.
fn blocked(
    record: &ServerRecord,
    root: &LinkedModRoot,
    block: LinkedTargetBlock,
) -> ResolvedTarget {
    unactionable(record, root, LinkedTargetState::Blocked(block))
}

/// A target with nothing to do, whatever the reason.
fn unactionable(
    record: &ServerRecord,
    root: &LinkedModRoot,
    state: LinkedTargetState,
) -> ResolvedTarget {
    ResolvedTarget {
        plan: LinkedTargetPlan {
            server: record.id,
            name: record.name.clone(),
            minecraft_version: record.minecraft_version.clone(),
            loader: record.loader.clone(),
            selected_by_default: default_selection(&state, root.server_side),
            installs: Vec::new(),
            removes: Vec::new(),
            orphans_unprovable: false,
            state,
        },
        mutation: None,
    }
}

/// What the exact release would do to a server that has, or does not have, the
/// project already.
fn installing_state(
    existing: Option<&PackFileRecipe>,
    root: &LinkedModRoot,
    game_dir: &Path,
) -> LinkedTargetState {
    let wanted = root.version_id.as_deref().unwrap_or_default();
    // An entry the server's own record does not take is not one of its mods,
    // however it came to be in the recipe. Reconstruction copies the client's
    // desired state, so a client-only file has an entry here and no jar.
    let Some(entry) = existing
        .filter(|entry| entry.is_installed_on(PackSide::Server, &OptionalSelection::none()))
    else {
        return LinkedTargetState::WillInstall;
    };

    let installed = entry
        .identity
        .as_ref()
        .and_then(|identity| identity.version_id.as_deref())
        .unwrap_or_default();
    if installed != wanted {
        return LinkedTargetState::WillChangeVersion {
            from: installed.to_owned(),
            to: wanted.to_owned(),
        };
    }
    if game_dir.join(&entry.logical_path).is_file() {
        LinkedTargetState::AlreadyCurrent
    } else {
        // The record is right and the jar is gone. Putting it back is a repair,
        // not a change: nothing about what the server records moves.
        LinkedTargetState::WillRepair
    }
}

/// The planned mods a server may actually take.
///
/// A dependency the project declares `unsupported` on servers is left out —
/// that is the author saying it cannot run there. `unknown` is not: an
/// undeclared side is not a statement that the mod is client-only, and a
/// required dependency of something the server is running is needed by it.
fn server_applicable(plan: &ModInstallPlan) -> Vec<&PlannedMod> {
    plan.all_mods()
        .into_iter()
        .filter(|planned| planned.project.server_side != SideSupport::Unsupported)
        .collect()
}

/// The first incompatibility, as a target block.
fn incompatibility(
    reasons: &[IncompatibilityReason],
    release: &ModVersionInfo,
) -> LinkedTargetBlock {
    match reasons.first() {
        Some(IncompatibilityReason::Loader { .. }) => LinkedTargetBlock::IncompatibleLoader {
            supported: release.loaders.clone(),
        },
        // A Minecraft mismatch is the more useful of the two to lead with when
        // both are true: a server on another release is a different situation
        // from one on another loader.
        _ => LinkedTargetBlock::IncompatibleMinecraft {
            supported: release.game_versions.clone(),
        },
    }
}

/// Why resolution against a server failed, as something to show.
///
/// A catalog that cannot be reached is not the same answer as a release that
/// does not fit, and only one of them is a reason to change anything.
fn resolution_block(error: &CoreError) -> LinkedTargetBlock {
    match error {
        CoreError::ModPlatform(
            ModPlatformError::Unreachable { .. } | ModPlatformError::ResponseInvalid { .. },
        ) => LinkedTargetBlock::ProviderUnavailable,
        _ => LinkedTargetBlock::ExactReleaseUnavailable,
    }
}

/// The projects a set of planned removals names.
fn removals_of(removes: &[PlannedFile]) -> Vec<ManagedProject> {
    removes
        .iter()
        .filter_map(|file| {
            Some(ManagedProject::new(
                ModProvider::Modrinth,
                file.project_id.clone()?,
            ))
        })
        .collect()
}

/// One entry a target would write, labeled with the project's own title.
fn planned_from_entry(entry: &PackFileRecipe, plan: &ModInstallPlan) -> PlannedFile {
    let project_id = entry
        .identity
        .as_ref()
        .and_then(|identity| identity.project_id.clone());
    let name = project_id
        .as_deref()
        .and_then(|id| {
            plan.all_mods()
                .into_iter()
                .find(|planned| planned.version.project_id == id)
                .map(|planned| planned.project.title.clone())
        })
        .unwrap_or_else(|| entry.display_name());
    PlannedFile {
        logical_path: entry.logical_path.clone(),
        name,
        project_id,
        version_id: entry
            .identity
            .as_ref()
            .and_then(|identity| identity.version_id.clone()),
        origin: entry.origin,
    }
}

/// The root an install plan names.
fn root_of_install(plan: &ModInstallPlan) -> LinkedModRoot {
    LinkedModRoot {
        provider: plan.provider,
        project_id: plan.root.version.project_id.clone(),
        name: display_name(&plan.root.project, &plan.root.version.file.file_name),
        version_id: Some(plan.root.version.id.clone()),
        version_number: Some(plan.root.version.version_number.clone()),
        server_side: side_requirement(plan.root.project.server_side),
    }
}

/// The root a removal plan names.
///
/// A removal needs no release and asks the catalog for nothing: the server's
/// own record is what proves ownership, so this stays usable offline.
fn root_of_removal(plan: &ModMutationPlan) -> LinkedModRoot {
    LinkedModRoot {
        provider: ModProvider::Modrinth,
        project_id: plan.project_id.clone().unwrap_or_default(),
        name: plan.name.clone(),
        version_id: None,
        version_number: None,
        // Support declarations decide whether a server is *offered* an install.
        // A removal is decided by proven ownership instead, so nothing here
        // claims to know what the project says.
        server_side: SideRequirement::Unknown,
    }
}

/// The project's title, or the fallback the caller supplied.
fn display_name(project: &ModProject, fallback: &str) -> String {
    if project.title.is_empty() {
        fallback.to_owned()
    } else {
        project.title.clone()
    }
}

/// Maps a provider's declared side support onto the recipe's own four states.
///
/// One-to-one. Collapsing `unknown` into `required` would assert something the
/// author declined to assert, and would put a mod onto a server on that
/// assertion.
const fn side_requirement(support: SideSupport) -> SideRequirement {
    match support {
        SideSupport::Required => SideRequirement::Required,
        SideSupport::Optional => SideRequirement::Optional,
        SideSupport::Unsupported => SideRequirement::Unsupported,
        SideSupport::Unknown => SideRequirement::Unknown,
    }
}
