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

use ephervex_download::{CacheLayout, OperationReporter};
use ephervex_mod_platform::{
    Compatibility, IncompatibilityReason, InstalledPolicy, InstanceTarget, ModInstallPlan,
    ModPlatformError, ModProject, ModVersionInfo, PlannedMod, SideSupport,
};
use ephervex_model::{InstanceId, LoaderSelection, ModProvider, ServerId};
use ephervex_pack::{
    ArtifactFetch, ManagedMutation, ManagedMutationReport, ManagedProject, OptionalSelection,
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
        /// The exact release to install. `None` takes the newest that fits the
        /// *client*, which is what choosing a mod by name in a browser means.
        ///
        /// A confirmation is produced from a plan and applied afterwards, and
        /// the catalog can publish a release in between. So once planning has
        /// resolved one, whatever applies that plan pins it here: a
        /// confirmation that named one release and installed another would be
        /// the substitution this launcher exists not to do, and it would hand
        /// every selected linked server a release the user never saw.
        version: Option<String>,
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
        self.with_artifact_cache(Box::pin(self.apply_linked_mod_operation_holding_the_cache(
            client, request, servers, reporter, cancel,
        )))
        .await
    }

    async fn apply_linked_mod_operation_holding_the_cache(
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
            LinkedModRequest::Install { project, version } => {
                let plan = self
                    .plan_mod_installation(client, &install_request(project, version.as_deref()))
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
            LinkedModRequest::Install { project, version } => {
                // Pinned when a confirmation named a release. The resolver
                // refuses a pin it cannot honor, so one that has been withdrawn
                // or that no longer fits fails the operation rather than
                // quietly becoming a neighboring release.
                let installed = self
                    .install_mods(
                        client,
                        &install_request(project, version.as_deref()),
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

        let manager = self.artifact_manager()?;

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
            fetch: ArtifactFetch {
                manager: &manager,
                cache: &CacheLayout::new(self.paths().cache_dir()),
                // Every address a recipe entry carries was already checked when
                // it was recorded. Whether fetching one needs a credential is
                // its provider's business, and is asked per file.
                credentials: &self.provider_credentials(),
                reporter,
                cancel,
            },
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

/// The catalog request one install names.
///
/// One builder for planning and for applying, so the two cannot disagree about
/// what was asked for.
fn install_request(project: &str, version: Option<&str>) -> ModInstallRequest {
    let request = ModInstallRequest::new(project.to_owned());
    match version {
        Some(version) => request.at_version(version.to_owned()),
        None => request,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::Write as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ephervex_model::{ModOrigin, ServerInstallation, ServerLaunch, ServerRelativePath};
    use ephervex_pack::SourceStore;
    use ephervex_storage::{NewServer, Paths};
    use serde_json::{Value, json};
    use sha1::{Digest as _, Sha1};
    use sha2::Sha512;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path as route, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const MINECRAFT: &str = "1.20.1";
    /// A release a linked server may have been moved to independently.
    const OTHER_MINECRAFT: &str = "1.20.2";
    const LOADER: &str = "neoforge";
    const LOADER_BUILD: &str = "47.1.106";

    // -- the catalog every test runs against -------------------------------

    /// The bytes standing in for one release's jar.
    fn jar(version_id: &str) -> Vec<u8> {
        format!("a fixture jar standing in for {version_id}").into_bytes()
    }

    fn sha1_of(bytes: &[u8]) -> String {
        let mut hasher = Sha1::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    fn sha512_of(bytes: &[u8]) -> String {
        let mut hasher = Sha512::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    fn project_json(id: &str, title: &str, client: &str, server: &str) -> Value {
        json!({
            "id": id,
            "slug": id.to_ascii_lowercase(),
            "title": title,
            "description": "a fixture project",
            "project_type": "mod",
            "client_side": client,
            "server_side": server,
            "icon_url": null,
        })
    }

    /// One release, published `minutes` after the fixture epoch so that
    /// "newest" is an explicit fact rather than an accident of ordering.
    fn version_json(
        base: &str,
        id: &str,
        project: &str,
        game_versions: &[&str],
        requires: &[&str],
        minutes: u32,
    ) -> Value {
        let body = jar(id);
        json!({
            "id": id,
            "project_id": project,
            "name": format!("{project} {id}"),
            "version_number": id,
            "version_type": "release",
            "game_versions": game_versions,
            "loaders": [LOADER],
            "date_published": format!("2026-01-01T00:{minutes:02}:00Z"),
            "files": [{
                "hashes": {"sha1": sha1_of(&body), "sha512": sha512_of(&body)},
                "url": format!("{base}/files/{id}.jar"),
                "filename": format!("{}-{id}.jar", project.to_ascii_lowercase()),
                "primary": true,
                "size": body.len(),
            }],
            "dependencies": requires
                .iter()
                .map(|id| json!({"project_id": id, "dependency_type": "required"}))
                .collect::<Vec<_>>(),
        })
    }

    /// Every project and release the tests below draw on.
    ///
    /// `ROOTV3` exists only on the other Minecraft release and is the newest of
    /// them all, which is what makes "the server picked its own newest" a
    /// visible mistake rather than an indistinguishable coincidence.
    fn fixtures(base: &str) -> (Vec<Value>, Vec<Value>) {
        let projects = vec![
            project_json("ROOTPROJ", "Root Mod", "required", "required"),
            project_json("LIBPROJ", "Server Library", "required", "required"),
            project_json("CLIENTLIB", "Client Library", "required", "unsupported"),
            project_json("CLIENTMOD", "Client Mod", "required", "unsupported"),
            project_json("OPTPROJ", "Optional Server Mod", "required", "optional"),
            project_json("UNKPROJ", "Undeclared Mod", "required", "unknown"),
            project_json("SECONDPROJ", "Second Root", "required", "required"),
        ];
        let versions = vec![
            version_json(base, "ROOTV1", "ROOTPROJ", &[MINECRAFT], &["CLIENTLIB"], 0),
            version_json(
                base,
                "ROOTV2",
                "ROOTPROJ",
                &[MINECRAFT],
                &["CLIENTLIB", "LIBPROJ"],
                10,
            ),
            version_json(base, "ROOTV3", "ROOTPROJ", &[OTHER_MINECRAFT], &[], 20),
            version_json(base, "LIBV1", "LIBPROJ", &[MINECRAFT], &[], 0),
            version_json(base, "CLIENTLIBV1", "CLIENTLIB", &[MINECRAFT], &[], 0),
            version_json(base, "CLIENTMODV1", "CLIENTMOD", &[MINECRAFT], &[], 0),
            version_json(base, "OPTV1", "OPTPROJ", &[MINECRAFT], &[], 0),
            version_json(base, "UNKV1", "UNKPROJ", &[MINECRAFT], &[], 0),
            version_json(
                base,
                "SECONDV1",
                "SECONDPROJ",
                &[MINECRAFT],
                &["LIBPROJ"],
                0,
            ),
        ];
        (projects, versions)
    }

    /// A release the catalog has not published yet.
    ///
    /// Serving the project's release list through a responder rather than a
    /// fixed body is what lets one test act out the case a confirmation exists
    /// for: the catalog gaining a newer compatible release between the moment a
    /// plan was shown and the moment it is applied.
    #[derive(Debug, Clone)]
    struct LateRelease {
        published: Arc<AtomicBool>,
    }

    impl LateRelease {
        fn publish(&self) {
            self.published.store(true, Ordering::SeqCst);
        }
    }

    /// The release list one project answers with, before and after publication.
    struct PublishedReleases {
        before: Vec<Value>,
        after: Vec<Value>,
        published: Arc<AtomicBool>,
    }

    impl wiremock::Respond for PublishedReleases {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let body = if self.published.load(Ordering::SeqCst) {
                &self.after
            } else {
                &self.before
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    }

    /// A loopback catalog serving the fixtures and their jars.
    async fn catalog() -> (MockServer, LateRelease) {
        let server = MockServer::start().await;
        let base = server.uri();
        let (projects, mut versions) = fixtures(&base);
        // Newer than every release of the root project, and compatible with the
        // client, so an unpinned resolution run after publication would take it.
        let late = version_json(
            &base,
            "ROOTV4",
            "ROOTPROJ",
            &[MINECRAFT],
            &["CLIENTLIB"],
            40,
        );
        let published = Arc::new(AtomicBool::new(false));

        for project in &projects {
            let id = project["id"].as_str().unwrap().to_owned();
            let owned: Vec<Value> = versions
                .iter()
                .filter(|version| version["project_id"] == id.as_str())
                .cloned()
                .collect();
            Mock::given(method("GET"))
                .and(route(format!("/project/{id}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(project.clone()))
                .mount(&server)
                .await;
            let mut after = owned.clone();
            if id == "ROOTPROJ" {
                after.push(late.clone());
            }
            Mock::given(method("GET"))
                .and(route(format!("/project/{id}/version")))
                .respond_with(PublishedReleases {
                    before: owned,
                    after,
                    published: Arc::clone(&published),
                })
                .mount(&server)
                .await;
        }

        // Reachable by exact id throughout, so a pin can always be resolved.
        versions.push(late);

        for version in &versions {
            let id = version["id"].as_str().unwrap().to_owned();
            // Answered per exact id: a batch that returned everything would let
            // a pinned lookup take whichever release happened to come first.
            Mock::given(method("GET"))
                .and(route("/versions"))
                .and(query_param("ids", format!("[\"{id}\"]")))
                .respond_with(ResponseTemplate::new(200).set_body_json(vec![version.clone()]))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(route(format!("/files/{id}.jar")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(jar(&id)))
                .mount(&server)
                .await;
        }

        Mock::given(method("GET"))
            .and(route("/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(projects))
            .mount(&server)
            .await;
        (server, LateRelease { published })
    }

    // -- an application with a client and its linked servers ---------------

    struct World {
        _base: TempDir,
        _catalog: MockServer,
        /// Publishes the release the catalog has been holding back.
        late: LateRelease,
        app: App,
        client: InstanceId,
    }

    impl World {
        /// A linked server, installed and stopped, holding a copy of the
        /// client's desired state exactly as reconstruction leaves it.
        fn link_server(&self, name: &str, minecraft: &str) -> ServerId {
            let mut record = self
                .app
                .registry_for_services()
                .prepare_server(NewServer::linked(name, self.client))
                .expect("prepare");
            record.minecraft_version = minecraft.to_owned();
            record.installation = ServerInstallation::Installed {
                launch: ServerLaunch::Jar {
                    path: ServerRelativePath::new("server.jar").expect("launch path"),
                },
            };

            let staging = record.root.with_extension("staging");
            let game = ephervex_storage::metadata::server_game_dir(&staging);
            std::fs::create_dir_all(game.join("mods")).expect("mods dir");
            let mut recipe = self
                .app
                .pack_recipe(self.client)
                .expect("recipe")
                .expect("an imported client has one");
            recipe.minecraft_version = minecraft.to_owned();
            SourceStore::new(ephervex_storage::metadata::source_dir(&staging))
                .write_recipe(&recipe)
                .expect("server recipe");
            self.app
                .registry_for_services()
                .publish_staged_server(&record, &staging)
                .expect("publish");
            record.id
        }

        fn server_root(&self, server: ServerId) -> PathBuf {
            self.app.server(server).expect("server").root
        }

        fn server_recipe(&self, server: ServerId) -> PackRecipe {
            SourceStore::new(ephervex_storage::metadata::source_dir(
                &self.server_root(server),
            ))
            .read_recipe()
            .expect("server recipe")
        }

        fn server_jars(&self, server: ServerId) -> Vec<String> {
            jars_in(&ephervex_storage::metadata::server_game_dir(
                &self.server_root(server),
            ))
        }

        fn client_jars(&self) -> Vec<String> {
            let root = self.app.client(self.client).expect("client").root;
            jars_in(&ephervex_storage::metadata::client_game_dir(&root))
        }

        /// Forgets every cached catalog document.
        ///
        /// Release lists are cached for `DEFAULT_METADATA_TTL`, so without this
        /// a second resolution inside one test would be answered from disk and
        /// could not see a release published in the meantime. Dropping the
        /// store is what a plan and an apply separated by more than the TTL —
        /// or by a restart — actually look like.
        fn forget_cached_catalog(&self) {
            let metadata = self.app.paths().cache_dir().join("metadata");
            if metadata.exists() {
                std::fs::remove_dir_all(&metadata).expect("clear the metadata cache");
            }
        }

        /// Marks a server as running by leaving the evidence a supervisor would.
        fn mark_running(&self, server: ServerId) {
            let root = self.server_root(server);
            let evidence = ephervex_server::RuntimeEvidence::for_current_supervisor()
                .expect("runtime evidence");
            let path = ephervex_server::runtime_path(&root);
            std::fs::create_dir_all(path.parent().expect("metadata dir")).expect("metadata dir");
            std::fs::write(&path, serde_json::to_vec(&evidence).expect("evidence")).expect("write");
        }
    }

    fn jars_in(game_dir: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(game_dir.join("mods")) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A pack with no files, so importing it contacts nothing.
    fn mrpack(path: &Path) {
        let file = std::fs::File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("modrinth.index.json", options)
            .expect("start index");
        writer
            .write_all(
                format!(
                    r#"{{"formatVersion":1,"game":"minecraft","versionId":"1.0.0",
                        "name":"Reproduction Pack","files":[],
                        "dependencies":{{"minecraft":"{MINECRAFT}","{LOADER}":"{LOADER_BUILD}"}}}}"#
                )
                .as_bytes(),
            )
            .expect("write index");
        writer.finish().expect("finish archive");
    }

    async fn world() -> World {
        let (catalog, late) = catalog().await;
        let base = tempfile::tempdir().expect("temp dir");

        let archive_dir = tempfile::tempdir().expect("temp dir");
        let archive = archive_dir.path().join("pack.mrpack");
        mrpack(&archive);

        let mut app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        app.use_loopback_catalog(&catalog.uri())
            .expect("loopback catalog");
        let imported = app
            .import_pack(
                &crate::import::ImportRequest::new(&archive),
                &OperationReporter::silent("test-import"),
                &CancellationToken::new(),
            )
            .await
            .expect("import");

        World {
            client: imported.record.id,
            app,
            late,
            _base: base,
            _catalog: catalog,
        }
    }

    fn silent() -> OperationReporter {
        OperationReporter::silent("linked-mod-test")
    }

    /// Where the fixture releases land in `mods/`.
    const ROOT_V1: &str = "mods/rootproj-ROOTV1.jar";
    const ROOT_V2: &str = "mods/rootproj-ROOTV2.jar";

    /// A browser install: the newest release that fits the client.
    fn install(project: &str) -> LinkedModRequest {
        LinkedModRequest::Install {
            project: project.to_owned(),
            version: None,
        }
    }

    /// The same install as a confirmation carries it, pinned to the release the
    /// plan resolved.
    fn confirmed(plan: &LinkedModPlan) -> LinkedModRequest {
        LinkedModRequest::Install {
            project: plan.root.project_id.clone(),
            version: plan.root.version_id.clone(),
        }
    }

    /// An install pinned to an exact release by the caller.
    fn pinned(project: &str, version: &str) -> LinkedModRequest {
        LinkedModRequest::Install {
            project: project.to_owned(),
            version: Some(version.to_owned()),
        }
    }

    fn change(target: &str, version: &str) -> LinkedModRequest {
        LinkedModRequest::ChangeVersion {
            target: PathBuf::from(target),
            version: version.to_owned(),
        }
    }

    fn update(target: &str) -> LinkedModRequest {
        LinkedModRequest::Update {
            target: PathBuf::from(target),
        }
    }

    fn remove(target: &str) -> LinkedModRequest {
        LinkedModRequest::Remove {
            target: PathBuf::from(target),
        }
    }

    fn target(plan: &LinkedModPlan, server: ServerId) -> &LinkedTargetPlan {
        plan.targets
            .iter()
            .find(|target| target.server == server)
            .expect("a target for this server")
    }

    /// The release a desired state records for the root project.
    fn installed_version(recipe: Option<&PackRecipe>) -> Option<String> {
        recipe?
            .managed_project(ModProvider::Modrinth, "ROOTPROJ")?
            .identity
            .as_ref()?
            .version_id
            .clone()
    }

    fn result_for(outcome: &LinkedModOutcome, server: ServerId) -> &LinkedTargetResult {
        &outcome
            .targets
            .iter()
            .find(|target| target.server == server)
            .expect("a result for this server")
            .result
    }

    async fn plan(world: &World, request: &LinkedModRequest) -> LinkedModPlan {
        world
            .app
            .plan_linked_mod_operation(world.client, request, &silent(), &CancellationToken::new())
            .await
            .expect("plan")
    }

    async fn apply(
        world: &World,
        request: &LinkedModRequest,
        servers: &[ServerId],
    ) -> LinkedModOutcome {
        world
            .app
            .apply_linked_mod_operation(
                world.client,
                request,
                servers,
                &silent(),
                &CancellationToken::new(),
            )
            .await
            .expect("apply")
    }

    // -- side semantics ----------------------------------------------------

    #[tokio::test]
    async fn a_client_only_mod_is_never_offered_to_a_linked_server() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        let plan = plan(&world, &install("CLIENTMOD")).await;

        assert_eq!(plan.root.server_side, SideRequirement::Unsupported);
        assert_eq!(
            target(&plan, server).state,
            LinkedTargetState::Blocked(LinkedTargetBlock::ServerUnsupported)
        );
        assert!(plan.actionable_targets().next().is_none());
        assert!(
            !plan.has_relevant_targets(),
            "a decision nobody can act on is not a decision to put in front of a user"
        );
    }

    #[tokio::test]
    async fn a_server_required_mod_is_offered_and_starts_selected() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        let plan = plan(&world, &install("ROOTPROJ")).await;

        let target = target(&plan, server);
        assert_eq!(target.state, LinkedTargetState::WillInstall);
        assert!(target.selected_by_default);
        assert_eq!(plan.default_selection(), vec![server]);
    }

    #[tokio::test]
    async fn optional_and_undeclared_server_support_are_offered_without_being_assumed() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        for (project, declared) in [
            ("OPTPROJ", SideRequirement::Optional),
            ("UNKPROJ", SideRequirement::Unknown),
        ] {
            let plan = plan(&world, &install(project)).await;
            assert_eq!(plan.root.server_side, declared, "{project}");
            let target = target(&plan, server);
            assert_eq!(
                target.state,
                LinkedTargetState::WillInstall,
                "{project} can be installed there"
            );
            assert!(
                !target.selected_by_default,
                "{project}: neither an author's `optional` nor their silence is a reason to \
                 write to a server nobody asked about"
            );
            assert!(plan.default_selection().is_empty(), "{project}");
        }
    }

    // -- the exact root release --------------------------------------------

    #[tokio::test]
    async fn a_selected_server_receives_the_exact_release_the_client_installed() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        let outcome = apply(&world, &install("ROOTPROJ"), &[server]).await;

        assert!(result_for(&outcome, server).succeeded());
        let recipe = world.server_recipe(server);
        let root = recipe
            .managed_project(ModProvider::Modrinth, "ROOTPROJ")
            .expect("the server records the synced root");
        assert_eq!(
            root.identity
                .as_ref()
                .and_then(|it| it.version_id.as_deref()),
            Some("ROOTV2"),
            "the release the client resolved is the release the server got"
        );
        assert_eq!(
            root.origin,
            ModOrigin::UserSelected,
            "a mod synced after reconstruction is an explicit choice, not pack history"
        );
        assert_eq!(
            world.server_jars(server),
            vec![
                "libproj-LIBV1.jar".to_owned(),
                "rootproj-ROOTV2.jar".to_owned()
            ],
            "the server takes the root and its server-side dependency and nothing else"
        );
        assert!(
            world
                .client_jars()
                .contains(&"clientlib-CLIENTLIBV1.jar".to_owned()),
            "the client keeps the dependency the server cannot run"
        );
        assert!(
            recipe
                .managed_project(ModProvider::Modrinth, "CLIENTLIB")
                .is_none(),
            "a dependency declared unsupported on servers is not recorded on one"
        );
    }

    #[tokio::test]
    async fn a_server_on_another_release_is_refused_without_being_offered_a_substitute() {
        let world = world().await;
        let compatible = world.link_server("Reproduction Server", MINECRAFT);
        let stale = world.link_server("Old Server", OTHER_MINECRAFT);

        let plan = plan(&world, &install("ROOTPROJ")).await;

        assert_eq!(
            target(&plan, compatible).state,
            LinkedTargetState::WillInstall
        );
        assert!(matches!(
            target(&plan, stale).state,
            LinkedTargetState::Blocked(LinkedTargetBlock::IncompatibleMinecraft { .. })
        ));
        assert_eq!(
            plan.default_selection(),
            vec![compatible],
            "one server that cannot take the release does not stop the one that can"
        );
    }

    #[tokio::test]
    async fn the_clients_update_candidate_is_the_only_release_a_server_is_offered() {
        let world = world().await;
        // Newer than everything and only on the other Minecraft release, so a
        // server choosing for itself would take it.
        let stale = world.link_server("Old Server", OTHER_MINECRAFT);
        apply(&world, &install("ROOTPROJ"), &[]).await;
        apply(&world, &change(ROOT_V2, "ROOTV1"), &[]).await;

        let plan = plan(&world, &update(ROOT_V1)).await;

        assert_eq!(
            plan.root.version_id.as_deref(),
            Some("ROOTV2"),
            "the update candidate is the newest release that fits the client"
        );
        assert!(
            matches!(
                target(&plan, stale).state,
                LinkedTargetState::Blocked(LinkedTargetBlock::IncompatibleMinecraft { .. })
            ),
            "the server is refused rather than given ROOTV3, which is newer and does fit it"
        );
    }

    #[tokio::test]
    async fn an_exact_version_change_moves_the_server_to_the_same_release() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);
        apply(&world, &install("ROOTPROJ"), &[server]).await;

        let plan = plan(&world, &change(ROOT_V2, "ROOTV1")).await;
        assert_eq!(
            target(&plan, server).state,
            LinkedTargetState::WillChangeVersion {
                from: "ROOTV2".to_owned(),
                to: "ROOTV1".to_owned(),
            }
        );
        assert!(
            target(&plan, server).selected_by_default,
            "a server that already manages this root stays in step by default"
        );

        let outcome = apply(&world, &change(ROOT_V2, "ROOTV1"), &[server]).await;

        assert!(result_for(&outcome, server).succeeded());
        assert_eq!(
            world.server_jars(server),
            vec!["rootproj-ROOTV1.jar".to_owned()],
            "the downgrade took the dependency the older release no longer needs"
        );
        assert!(
            world
                .server_recipe(server)
                .managed_project(ModProvider::Modrinth, "LIBPROJ")
                .is_none()
        );
    }

    // -- removal -----------------------------------------------------------

    #[tokio::test]
    async fn removing_a_synced_root_collects_its_orphan_and_leaves_everything_else() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);
        apply(&world, &install("ROOTPROJ"), &[server]).await;

        let mods =
            ephervex_storage::metadata::server_game_dir(&world.server_root(server)).join("mods");
        std::fs::write(mods.join("local-debug-build.jar"), b"jar").expect("write jar");

        let plan = plan(&world, &remove(ROOT_V2)).await;
        let target = target(&plan, server);
        assert_eq!(target.state, LinkedTargetState::WillRemove);
        assert!(target.selected_by_default);
        let collected: Vec<&str> = target
            .collected_dependencies()
            .map(|file| file.project_id.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(collected, vec!["LIBPROJ"]);

        let outcome = apply(&world, &remove(ROOT_V2), &[server]).await;

        assert!(result_for(&outcome, server).succeeded());
        assert_eq!(
            world.server_jars(server),
            vec!["local-debug-build.jar".to_owned()],
            "a jar Ephervex never recorded is not something a provider operation can reach"
        );
        assert!(world.server_recipe(server).managed_mods().is_empty());
    }

    #[tokio::test]
    async fn a_server_that_never_received_the_root_gains_no_removal_target() {
        let world = world().await;
        let synced = world.link_server("Reproduction Server", MINECRAFT);
        // Linked before the install, so its own desired state never records the
        // root. A server built afterwards would have reconstructed it.
        let untouched = world.link_server("Second Server", MINECRAFT);
        apply(&world, &install("ROOTPROJ"), &[synced]).await;

        let plan = plan(&world, &remove(ROOT_V2)).await;

        assert_eq!(target(&plan, synced).state, LinkedTargetState::WillRemove);
        assert_eq!(
            target(&plan, untouched).state,
            LinkedTargetState::NotPresent,
            "durable ownership is what proves the relationship, not a shared file name"
        );
        assert_eq!(plan.default_selection(), vec![synced]);
    }

    #[tokio::test]
    async fn a_server_the_pack_owns_this_mod_on_is_refused() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        // The server's pack declares the project, as an import would have.
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(
            &world.server_root(server),
        ));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe.files.push(PackFileRecipe {
            logical_path: PathBuf::from("mods/from-the-pack.jar"),
            hashes: Vec::new(),
            downloads: Vec::new(),
            size: None,
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: Some(ephervex_pack::ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some("ROOTPROJ".to_owned()),
                version_id: Some("ROOTV1".to_owned()),
                file_id: None,
            }),
        });
        store.write_recipe(&recipe).expect("write recipe");

        let plan = plan(&world, &install("ROOTPROJ")).await;

        assert_eq!(
            target(&plan, server).state,
            LinkedTargetState::Blocked(LinkedTargetBlock::PackOwned),
            "pack-owned content is reconstructed, never independently mutated"
        );
    }

    // -- runtime and stale targets -----------------------------------------

    #[tokio::test]
    async fn a_running_server_is_refused_rather_than_written_to() {
        let world = world().await;
        let server = world.link_server("Running Server", MINECRAFT);
        world.mark_running(server);

        let plan = plan(&world, &install("ROOTPROJ")).await;
        assert_eq!(
            target(&plan, server).state,
            LinkedTargetState::Blocked(LinkedTargetBlock::ServerRunning)
        );

        // Selected anyway, as a stale confirmation would.
        let outcome = apply(&world, &install("ROOTPROJ"), &[server]).await;

        assert!(matches!(
            result_for(&outcome, server),
            LinkedTargetResult::Skipped(LinkedTargetState::Blocked(
                LinkedTargetBlock::ServerRunning
            ))
        ));
        assert!(
            world.server_jars(server).is_empty(),
            "nothing is written under a live process, and nothing stops it either"
        );
        assert!(
            !world.client_jars().is_empty(),
            "the client operation is independent of the server that refused"
        );
    }

    #[tokio::test]
    async fn a_target_unlinked_between_planning_and_applying_fails_safely() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        let plan = plan(&world, &install("ROOTPROJ")).await;
        assert_eq!(plan.default_selection(), vec![server]);

        let mut record = world.app.server(server).expect("server");
        record.linked_client_id = None;
        world
            .app
            .registry_for_services()
            .update_server_committed(&mut record)
            .expect("unlink");

        let outcome = apply(&world, &install("ROOTPROJ"), &[server]).await;

        assert!(matches!(
            result_for(&outcome, server),
            LinkedTargetResult::Skipped(LinkedTargetState::Blocked(
                LinkedTargetBlock::NoLongerLinked
            ))
        ));
        assert!(world.server_jars(server).is_empty());
    }

    // -- the release a confirmation named ----------------------------------

    #[tokio::test]
    async fn a_confirmed_install_applies_the_release_it_confirmed() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        // What the user is shown, and what they agree to.
        let confirmation = plan(&world, &install("ROOTPROJ")).await;
        assert_eq!(confirmation.root.version_id.as_deref(), Some("ROOTV2"));
        assert_eq!(
            target(&confirmation, server).state,
            LinkedTargetState::WillInstall
        );

        // The catalog moves on while the confirmation is open.
        world.late.publish();
        world.forget_cached_catalog();
        assert_eq!(
            plan(&world, &install("ROOTPROJ"))
                .await
                .root
                .version_id
                .as_deref(),
            Some("ROOTV4"),
            "an unpinned resolution now takes the new release, which is exactly what \
             applying the confirmation must not do"
        );
        world.forget_cached_catalog();

        let outcome = apply(&world, &confirmed(&confirmation), &[server]).await;

        assert!(result_for(&outcome, server).succeeded());
        assert_eq!(
            outcome.root.version_id.as_deref(),
            Some("ROOTV2"),
            "the operation is about the release the user confirmed"
        );
        assert_eq!(
            installed_version(
                world
                    .app
                    .pack_recipe(world.client)
                    .expect("recipe")
                    .as_ref()
            ),
            Some("ROOTV2".to_owned()),
            "the client got what the confirmation said, not what the catalog now prefers"
        );
        assert_eq!(
            installed_version(Some(&world.server_recipe(server))),
            Some("ROOTV2".to_owned()),
            "and every selected server was evaluated against that same exact release"
        );
        assert_eq!(
            world.server_jars(server),
            vec![
                "libproj-LIBV1.jar".to_owned(),
                "rootproj-ROOTV2.jar".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn a_confirmed_release_that_no_longer_fits_fails_instead_of_substituting() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        // A release of the same project that this client cannot run. Standing
        // in for one withdrawn or invalidated between the confirmation and the
        // apply: the answer is the same either way.
        let error = world
            .app
            .apply_linked_mod_operation(
                world.client,
                &pinned("ROOTPROJ", "ROOTV3"),
                &[server],
                &silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a release that does not fit is refused");

        assert!(
            matches!(
                error,
                CoreError::ModPlatform(ModPlatformError::IncompatibleSelectedVersion { .. })
            ),
            "the refusal names the release, rather than the operation finding a near miss: \
             {error}"
        );
        assert!(
            world.client_jars().is_empty(),
            "nothing was installed on the way to refusing"
        );
        assert!(world.server_jars(server).is_empty());
    }

    // -- several targets ---------------------------------------------------

    #[tokio::test]
    async fn each_target_keeps_its_own_result_and_a_partial_outcome_says_so() {
        let world = world().await;
        let applied = world.link_server("Reproduction Server", MINECRAFT);
        let running = world.link_server("Running Server", MINECRAFT);
        let unselected = world.link_server("Second Server", MINECRAFT);
        world.mark_running(running);

        let outcome = apply(&world, &install("ROOTPROJ"), &[applied, running]).await;

        assert!(result_for(&outcome, applied).succeeded());
        assert!(!result_for(&outcome, running).succeeded());
        assert!(
            outcome.is_partial(),
            "one target failing does not make the successful ones fail"
        );
        assert_eq!(outcome.succeeded().count(), 1);
        assert_eq!(
            outcome.targets.len(),
            2,
            "results are keyed by the servers that were selected, by their own ids"
        );
        assert!(
            world.server_jars(unselected).is_empty(),
            "a linked server nobody selected is not touched"
        );
        assert!(!world.server_jars(applied).is_empty());
    }

    #[tokio::test]
    async fn one_verified_artifact_serves_the_client_and_every_server() {
        let world = world().await;
        let first = world.link_server("Reproduction Server", MINECRAFT);
        let second = world.link_server("Second Server", MINECRAFT);

        let outcome = apply(&world, &install("ROOTPROJ"), &[first, second]).await;

        for server in [first, second] {
            let LinkedTargetResult::Applied(report) = result_for(&outcome, server) else {
                panic!("both servers take part");
            };
            assert_eq!(
                report.reused_from_cache, 2,
                "the root and its dependency come out of the shared verified cache the client                  filled, never out of the client's own mods directory"
            );
        }
    }

    #[tokio::test]
    async fn a_submitted_target_is_acted_on_once_however_often_it_is_named() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        let outcome = apply(&world, &install("ROOTPROJ"), &[server, server, server]).await;

        assert_eq!(
            outcome.targets.len(),
            1,
            "one directory cannot be mutated three times by one operation"
        );
        assert!(result_for(&outcome, server).succeeded());
    }

    // -- persistence -------------------------------------------------------

    #[tokio::test]
    async fn synced_ownership_and_pack_history_survive_a_reopen_and_stay_distinct() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);

        let store = SourceStore::new(ephervex_storage::metadata::source_dir(
            &world.server_root(server),
        ));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe.files.push(PackFileRecipe {
            logical_path: PathBuf::from("mods/from-the-pack.jar"),
            hashes: Vec::new(),
            downloads: Vec::new(),
            size: None,
            client: SideRequirement::Required,
            server: SideRequirement::Required,
            selected: None,
            origin: ModOrigin::PackDeclared,
            identity: Some(ephervex_pack::ProviderFileIdentity {
                provider: Some(ModProvider::Modrinth),
                project_id: Some("OPTPROJ".to_owned()),
                version_id: Some("OPTV1".to_owned()),
                file_id: None,
            }),
        });
        store.write_recipe(&recipe).expect("write recipe");

        apply(&world, &install("ROOTPROJ"), &[server]).await;

        // Read back through a fresh application, so nothing below is reading
        // state this test happened to leave in memory.
        let reopened = App::open_at(world.app.paths().clone()).expect("app reopens");
        let root = reopened.server(server).expect("server").root;
        let recipe = SourceStore::new(ephervex_storage::metadata::source_dir(&root))
            .read_recipe()
            .expect("recipe");

        let synced = recipe
            .managed_project(ModProvider::Modrinth, "ROOTPROJ")
            .expect("the synced root survives");
        assert_eq!(synced.origin, ModOrigin::UserSelected);
        assert_eq!(
            synced
                .identity
                .as_ref()
                .and_then(|it| it.version_id.as_deref()),
            Some("ROOTV2")
        );
        assert!(
            !synced.hashes.is_empty(),
            "the hashes that verify it are kept"
        );
        assert_eq!(synced.size, Some(jar("ROOTV2").len() as u64));

        assert_eq!(
            recipe
                .managed_project(ModProvider::Modrinth, "LIBPROJ")
                .expect("its dependency survives")
                .origin,
            ModOrigin::RequiredDependency,
            "a dependency installed for a synced root keeps saying so"
        );
        assert_eq!(
            recipe
                .managed_project(ModProvider::Modrinth, "OPTPROJ")
                .expect("the pack's own entry survives")
                .origin,
            ModOrigin::PackDeclared,
            "a later sync does not rewrite what the pack declared"
        );
    }

    #[tokio::test]
    async fn a_dependency_another_synced_root_still_needs_is_kept() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);
        apply(&world, &install("ROOTPROJ"), &[server]).await;
        apply(&world, &install("SECONDPROJ"), &[server]).await;

        let shared = plan(&world, &remove(ROOT_V2)).await;
        assert_eq!(
            target(&shared, server).collected_dependencies().count(),
            0,
            "the library still has an owner on this server"
        );

        apply(&world, &remove(ROOT_V2), &[server]).await;

        assert_eq!(
            world.server_jars(server),
            vec![
                "libproj-LIBV1.jar".to_owned(),
                "secondproj-SECONDV1.jar".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn a_server_already_at_the_release_is_repaired_only_when_its_jar_is_gone() {
        let world = world().await;
        let server = world.link_server("Reproduction Server", MINECRAFT);
        apply(&world, &install("ROOTPROJ"), &[server]).await;

        let current = plan(&world, &install("ROOTPROJ")).await;
        let current = target(&current, server);
        assert_eq!(current.state, LinkedTargetState::AlreadyCurrent);
        assert!(
            !current.selected_by_default,
            "there is nothing to do, so there is nothing to preselect"
        );

        let jar = ephervex_storage::metadata::server_game_dir(&world.server_root(server))
            .join("mods/rootproj-ROOTV2.jar");
        std::fs::remove_file(&jar).expect("delete the jar, as bisecting a crash does");

        let repair = plan(&world, &install("ROOTPROJ")).await;
        assert_eq!(target(&repair, server).state, LinkedTargetState::WillRepair);

        let outcome = apply(&world, &install("ROOTPROJ"), &[server]).await;

        assert!(result_for(&outcome, server).succeeded());
        assert!(jar.is_file(), "the recorded release is put back");
        assert_eq!(
            world
                .server_recipe(server)
                .managed_project(ModProvider::Modrinth, "ROOTPROJ")
                .and_then(|entry| entry.identity.as_ref())
                .and_then(|identity| identity.version_id.as_deref()),
            Some("ROOTV2"),
            "a repair changes nothing about what the server records"
        );
    }

    // -- the policy, without a catalog -------------------------------------

    #[test]
    fn only_a_server_that_already_manages_the_root_or_declares_it_required_starts_selected() {
        let change = LinkedTargetState::WillChangeVersion {
            from: "A".to_owned(),
            to: "B".to_owned(),
        };
        for state in [
            &change,
            &LinkedTargetState::WillRepair,
            &LinkedTargetState::WillRemove,
        ] {
            for side in [
                SideRequirement::Required,
                SideRequirement::Optional,
                SideRequirement::Unknown,
            ] {
                assert!(
                    default_selection(state, side),
                    "a server already managing the root stays in step: {state:?} {side:?}"
                );
            }
        }

        assert!(default_selection(
            &LinkedTargetState::WillInstall,
            SideRequirement::Required
        ));
        for side in [SideRequirement::Optional, SideRequirement::Unknown] {
            assert!(
                !default_selection(&LinkedTargetState::WillInstall, side),
                "{side:?} is not a reason to write to a server nobody asked about"
            );
        }
        for state in [
            LinkedTargetState::AlreadyCurrent,
            LinkedTargetState::NotPresent,
            LinkedTargetState::Blocked(LinkedTargetBlock::ServerRunning),
        ] {
            assert!(!default_selection(&state, SideRequirement::Required));
        }
    }

    #[test]
    fn every_target_state_names_a_distinct_reason_and_a_distinct_message() {
        let blocks = [
            LinkedTargetBlock::ServerUnsupported,
            LinkedTargetBlock::IncompatibleMinecraft {
                supported: Vec::new(),
            },
            LinkedTargetBlock::IncompatibleLoader {
                supported: Vec::new(),
            },
            LinkedTargetBlock::ExactReleaseUnavailable,
            LinkedTargetBlock::ServerRunning,
            LinkedTargetBlock::ServerNotInstalled,
            LinkedTargetBlock::NoLongerLinked,
            LinkedTargetBlock::ProviderUnavailable,
            LinkedTargetBlock::StateUnavailable,
            LinkedTargetBlock::PackOwned,
            LinkedTargetBlock::ServerDependency,
        ];
        let ids: BTreeSet<&str> = blocks.iter().map(LinkedTargetBlock::message_id).collect();
        assert_eq!(ids.len(), blocks.len(), "two refusals share a sentence");
        let names: BTreeSet<&str> = blocks.iter().map(LinkedTargetBlock::as_str).collect();
        assert_eq!(names.len(), blocks.len());
        assert!(
            blocks
                .iter()
                .all(|block| !LinkedTargetState::Blocked(block.clone()).is_actionable())
        );
    }
}
