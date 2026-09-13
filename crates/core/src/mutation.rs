//! Changing and removing the mods an instance already has.
//!
//! # Why this is not "install, but backwards"
//!
//! Installing answers one question: what does this mod need? Mutating answers a
//! second one the installer never has to — *what did the old state own that the
//! new one does not?* A mod dragged in as a dependency two operations ago has no
//! record of who wanted it; the recipe says it is a `RequiredDependency` and
//! nothing more. So "is this still needed" cannot be looked up, only recomputed.
//!
//! It is recomputed the only honest way: by re-resolving the instance's roots.
//! Every mod the user chose by name is resolved against the catalog, pinned to
//! the version it is actually at, and the union of everything those resolutions
//! reach is what the instance still requires. A dependency outside that union
//! has no remaining owner and may go. One inside it stays, however it got there.
//!
//! Two consequences follow, and both are deliberate:
//!
//! * **A provider that cannot be reached means nothing is removed.** An
//!   unprovable orphan is kept. Deleting a jar another mod needs breaks the
//!   instance; keeping one nothing needs wastes a megabyte.
//! * **Only automatic dependencies are ever collected.** A mod the user chose —
//!   including an optional dependency they explicitly ticked — is a root. Roots
//!   are removed when a human asks, never as a side effect.
//!
//! # What ownership decides
//!
//! [`ModOrigin`] is not a label here, it is the policy:
//!
//! | Origin | Change version | Update | Remove |
//! |---|---|---|---|
//! | `UserSelected` | yes | yes | yes |
//! | `SelectedOptionalDependency` | no | no | yes |
//! | `RequiredDependency` | no | no | no |
//! | `PackDeclared` | no | no | no |
//!
//! An optional dependency sits where it does because both halves of its name
//! are true. It is a *dependency*: the resolver picks its version from what its
//! dependents require, and letting a user move it independently would let them
//! break the mod that asked for it. And it was *selected*: a human ticked it, so
//! un-ticking it is theirs to do, and nothing else may collect it.
//!
//! A required dependency has no independent life at all. Its version follows its
//! dependents, and it disappears when the last of them does.
//!
//! Pack-owned mods are refused, and [`MutationRestriction::PackOwned`] says so
//! rather than the frontend guessing. See that variant for why.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, OperationReporter};
use ephervex_mod_platform::{
    AnnotatedVersion, InstalledPolicy, InstanceTarget, ModInstallPlan, ModProject, PlanWarning,
    PlannedMod, annotate, newest_compatible,
};
use ephervex_model::{InstanceId, ModOrigin, ModProvider};
use ephervex_pack::{
    ArtifactFetch, ManagedMutation, ManagedProject, PackError, PackFileRecipe, PackRecipe,
    RecordedMod, SourceStore,
};
use ephervex_storage::ClientRecord;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};
use crate::mods::{ManagedModEntry, ManagedModSnapshot};

/// Why a mod cannot be mutated.
///
/// Typed because each of these needs different words in front of a user, and
/// because a frontend that had to infer them from an origin and an identity
/// would be re-deciding policy that belongs here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationRestriction {
    /// The imported pack declares this mod, and the pack's declaration is a
    /// historical record rather than a wish list.
    ///
    /// Moving it independently would mean the instance's mods no longer match
    /// any version of the pack, and the recipe has nowhere to say so: an entry
    /// records one version, and overwriting it would make the launcher claim
    /// the pack shipped a release it never shipped. Representing the deviation
    /// honestly needs a second version per entry and an answer to which one a
    /// reconstructed server gets, which is a change to how packs are recorded
    /// rather than a mutation feature. Until that exists this is refused, and
    /// the mod's available versions are still shown.
    PackOwned,
    /// Something else asked for this mod, so its version is not the user's to
    /// choose and its lifetime is not theirs to end.
    Dependency,
    /// Nothing about this file can be resolved against a catalog. There is no
    /// project to list versions of and no identity to replace.
    IdentityUnresolved,
    /// Somebody put this jar in `mods/` themselves, so it is theirs rather than
    /// the launcher's: nothing here installed it, nothing here recorded it, and
    /// nothing here will replace it or change which release it is. Deleting it
    /// outright is the user's to ask for and is not a mutation of this kind.
    HandPlaced,
    /// The project is known but the installed release is not, so the launcher
    /// cannot tell an update from a no-op or prove what it would be replacing.
    VersionUnknown,
}

impl MutationRestriction {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::PackOwned => "mod-restriction-pack-owned",
            Self::Dependency => "mod-restriction-dependency",
            Self::IdentityUnresolved => "mod-restriction-identity-unresolved",
            Self::HandPlaced => "mod-restriction-hand-placed",
            Self::VersionUnknown => "mod-restriction-version-unknown",
        }
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PackOwned => "pack-owned",
            Self::Dependency => "dependency",
            Self::IdentityUnresolved => "identity-unresolved",
            Self::HandPlaced => "hand-placed",
            Self::VersionUnknown => "version-unknown",
        }
    }
}

/// What one managed mod may have done to it.
///
/// Independent permissions rather than one "level": they genuinely do not
/// order. An optional dependency may be removed and not moved; a pack mod may
/// have its versions listed and nothing else; a hand-placed jar may be deleted
/// and nothing else.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModMutability {
    pub can_change_version: bool,
    pub can_update: bool,
    /// Whether this mod may be taken out of the instance's desired state.
    ///
    /// A provider operation, addressed by project: it resolves what the mod's
    /// dependencies leave orphaned and retires the files it removes. Never true
    /// for a jar the launcher did not install, which has no desired state to be
    /// taken out of. See [`Self::can_delete_file`] for that.
    pub can_remove: bool,
    /// Whether the jar itself may simply be deleted.
    ///
    /// The one thing that can be done to a hand-placed jar and to nothing else.
    /// Addressed by path rather than by project, because a jar nothing
    /// recognizes has no project, and it is the user's file: deleting it
    /// resolves nothing, retires nothing, and asks no catalog.
    pub can_delete_file: bool,
    /// Whether the mod's other releases can be listed at all. True for anything
    /// with a project, including mods nothing may mutate: seeing what else
    /// exists is not a mutation.
    pub can_list_versions: bool,
    /// Why the mutating actions are unavailable, when they are.
    pub restriction: Option<MutationRestriction>,
}

impl ModMutability {
    /// No mutation may be done, for this reason.
    const fn restricted(restriction: MutationRestriction, can_list_versions: bool) -> Self {
        Self {
            can_change_version: false,
            can_update: false,
            can_remove: false,
            can_delete_file: false,
            can_list_versions,
            restriction: Some(restriction),
        }
    }

    /// What one entry's ownership and identity allow.
    ///
    /// Ownership is checked before identity: "the pack owns this" is a more
    /// useful thing to be told than "its version is unknown", and for a pack
    /// mod both are often true at once.
    #[must_use]
    pub fn of(entry: &ManagedModEntry) -> Self {
        let has_project = entry.identity.project_id().is_some();
        let has_version = entry.identity.is_fully_resolved();

        match entry.origin {
            // Ephervex did not put this file here and does not own it, so no
            // mutation may reach for it. Deleting the jar is the exception:
            // the user owns it, and the user may throw it away.
            ModOrigin::HandPlaced => Self {
                can_delete_file: true,
                ..Self::restricted(MutationRestriction::HandPlaced, has_project)
            },
            ModOrigin::PackDeclared => {
                Self::restricted(MutationRestriction::PackOwned, has_project)
            }
            ModOrigin::RequiredDependency => {
                Self::restricted(MutationRestriction::Dependency, has_project)
            }
            ModOrigin::SelectedOptionalDependency if has_project => Self {
                can_change_version: false,
                can_update: false,
                // The one thing an explicit optional choice keeps: the person
                // who ticked it may untick it.
                can_remove: true,
                can_delete_file: false,
                can_list_versions: true,
                restriction: Some(MutationRestriction::Dependency),
            },
            ModOrigin::UserSelected if has_version => Self {
                can_change_version: true,
                can_update: true,
                can_remove: true,
                can_delete_file: false,
                can_list_versions: true,
                restriction: None,
            },
            ModOrigin::UserSelected if has_project => {
                Self::restricted(MutationRestriction::VersionUnknown, true)
            }
            // Nothing left that names a project, whatever chose it.
            ModOrigin::UserSelected | ModOrigin::SelectedOptionalDependency => {
                Self::restricted(MutationRestriction::IdentityUnresolved, false)
            }
        }
    }
}

/// Which release a mutation should move a mod to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModVersionChoice {
    /// Exactly this provider version. Preserved literally: if it does not fit
    /// the instance, the operation fails rather than finding a near miss.
    Exact(String),
    /// The newest release that fits this instance's exact Minecraft version and
    /// loader. Never "the newest release", which is a different and wrong thing.
    NewestCompatible,
}

/// The releases of one managed mod's project, as a picker needs them.
#[derive(Debug, Clone)]
pub struct ModVersionChoices {
    /// The mod these belong to, by its place in the instance.
    pub logical_path: PathBuf,
    pub provider: ModProvider,
    pub project: ModProject,
    /// What the instance has now, when it is known.
    pub installed_version_id: Option<String>,
    /// Every release that fits this instance, newest first.
    pub compatible: Vec<AnnotatedVersion>,
    /// The release an update would move to, when one exists.
    pub newest_compatible: Option<String>,
    pub mutability: ModMutability,
}

impl ModVersionChoices {
    /// Whether the installed release is already the newest compatible one.
    #[must_use]
    pub fn is_current(&self) -> bool {
        match (&self.installed_version_id, &self.newest_compatible) {
            (Some(installed), Some(newest)) => installed == newest,
            _ => false,
        }
    }
}

/// What a planned mutation would do to the mod it acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedOutcome {
    /// The instance already has exactly this, and its jar is there. Nothing is
    /// downloaded and the recipe is not rewritten.
    AlreadyCurrent,
    /// The recorded release is right and its jar is missing, so the operation
    /// puts it back rather than reporting that there is nothing to do.
    Repair,
    /// The mod moves from one release to another.
    ChangeVersion { from: String, to: String },
    /// The mod leaves the instance.
    Remove,
}

impl PlannedOutcome {
    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::AlreadyCurrent => "already-current",
            Self::Repair => "repair",
            Self::ChangeVersion { .. } => "change-version",
            Self::Remove => "remove",
        }
    }

    /// Whether carrying this out would change the instance.
    #[must_use]
    pub const fn changes_anything(&self) -> bool {
        !matches!(self, Self::AlreadyCurrent)
    }
}

/// One file a mutation would write or take away, as something to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub logical_path: PathBuf,
    /// The project's title where the provider supplied one, and the file's own
    /// name otherwise. Never a name parsed for meaning.
    pub name: String,
    pub project_id: Option<String>,
    pub version_id: Option<String>,
    pub origin: ModOrigin,
}

/// Everything a mutation would do, worked out without touching the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModMutationPlan {
    pub instance: InstanceId,
    /// The mod the user acted on.
    pub target: PathBuf,
    pub name: String,
    /// The catalog the acted-on mod came from, when its identity names one.
    ///
    /// Carried on the plan because everything downstream of it (resolving the
    /// replacement, collecting orphans, offering the change to a linked server)
    /// has to ask the same catalog the mod was installed from, and a project
    /// id alone does not say which that is.
    pub provider: Option<ModProvider>,
    /// The acted-on mod's project, when it has one.
    pub project_id: Option<String>,
    /// The exact release this would move the mod to. `None` for a removal.
    pub candidate_version: Option<String>,
    pub outcome: PlannedOutcome,
    /// Mods this would install or move, the target included.
    pub installs: Vec<PlannedFile>,
    /// Mods this would take out of the instance.
    pub removes: Vec<PlannedFile>,
    /// True when the provider could not confirm which dependencies are still
    /// needed, so none were collected. The mutation is still safe; it just
    /// leaves more behind than it might.
    pub orphans_unprovable: bool,
    pub warnings: Vec<PlanWarning>,
}

impl ModMutationPlan {
    /// Whether anything would actually be written or deleted.
    #[must_use]
    pub fn changes_anything(&self) -> bool {
        self.outcome.changes_anything() || !self.removes.is_empty()
    }

    /// Dependencies this would take away, which is what a confirmation has to
    /// show before it is honest.
    pub fn collected_dependencies(&self) -> impl Iterator<Item = &PlannedFile> {
        self.removes
            .iter()
            .filter(|file| file.logical_path != self.target)
    }
}

/// What a carried-out mutation did.
#[derive(Debug, Clone)]
pub struct ModMutation {
    pub plan: ModMutationPlan,
    pub report: ephervex_pack::ManagedMutationReport,
    /// The instance's mods afterwards, so a caller can render the result
    /// without asking again.
    pub snapshot: ManagedModSnapshot,
}

impl App {
    /// The releases of one managed mod, annotated against this instance.
    ///
    /// Offered for any mod with a project, including ones nothing may mutate: a
    /// pack's mod has versions worth seeing even though this build will not
    /// change it.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance, target = %target.display()))]
    pub async fn managed_mod_versions(
        &self,
        instance: InstanceId,
        target: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModVersionChoices> {
        let record = self.client(instance)?;
        let snapshot = self.managed_mod_snapshot(instance)?;
        let entry = entry_at(&snapshot, target)?;
        let mutability = ModMutability::of(entry);
        if !mutability.can_list_versions {
            return Err(CoreError::ModMutationRestricted {
                path: target.to_path_buf(),
                restriction: MutationRestriction::IdentityUnresolved,
            });
        }

        let (provider_kind, project_id) = entry
            .identity
            .provider()
            .zip(entry.identity.project_id())
            .ok_or_else(|| CoreError::ModMutationRestricted {
                path: target.to_path_buf(),
                restriction: MutationRestriction::IdentityUnresolved,
            })?;

        // The catalog the mod was installed from, never a default: another
        // catalog's ids resolve to something else or to nothing.
        let provider = self.mod_provider(provider_kind)?;
        let instance_target = target_for(&record);
        let loader = ephervex_mod_platform::loader_name_for(provider, &instance_target)?;
        let project = provider.project(project_id, reporter, cancel).await?;
        let versions: Vec<_> = provider
            .versions(&project.id, &instance_target, reporter, cancel)
            .await?;

        let newest = newest_compatible(&versions, &instance_target, loader).map(|it| it.id);
        let compatible: Vec<AnnotatedVersion> = annotate(&versions, &instance_target, loader)
            .into_iter()
            .filter(|annotated| annotated.compatibility.is_compatible())
            .collect();

        Ok(ModVersionChoices {
            logical_path: entry.logical_path.clone(),
            provider: provider_kind,
            project,
            installed_version_id: entry.identity.version_id().map(ToOwned::to_owned),
            compatible,
            newest_compatible: newest,
            mutability,
        })
    }

    /// Works out what changing one mod's version would involve.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance, target = %target.display()))]
    pub async fn plan_mod_version_change(
        &self,
        instance: InstanceId,
        target: &Path,
        choice: &ModVersionChoice,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutationPlan> {
        let context = self.mutation_context(instance, target)?;
        self.version_change_plan(&context, choice, reporter, cancel)
            .await
    }

    /// Changes one mod to an exact release, or to the newest one that fits.
    ///
    /// The plan is recomputed here rather than taken from the caller. A release
    /// that has been withdrawn or that no longer fits between planning and
    /// applying must fail the operation, and the only way to notice is to ask
    /// again with the same exact pin.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance, target = %target.display()))]
    pub async fn change_managed_mod_version(
        &self,
        instance: InstanceId,
        target: &Path,
        choice: &ModVersionChoice,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutation> {
        self.with_artifact_cache(Box::pin(self.change_managed_mod_version_holding_the_cache(
            instance, target, choice, reporter, cancel,
        )))
        .await
    }

    async fn change_managed_mod_version_holding_the_cache(
        &self,
        instance: InstanceId,
        target: &Path,
        choice: &ModVersionChoice,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutation> {
        let mut context = self.mutation_context(instance, target)?;
        let plan = self
            .version_change_plan(&context, choice, reporter, cancel)
            .await?;

        if !plan.changes_anything() {
            info!(target = %target.display(), "the mod is already at the requested version");
            return Ok(ModMutation {
                report: ephervex_pack::ManagedMutationReport::default(),
                snapshot: self.managed_mod_snapshot(instance)?,
                plan,
            });
        }

        let upserts = self.upserts_for(&context, &plan, reporter).await?;
        let removals = removals_for(&plan);
        self.commit(&mut context, &upserts, &removals, reporter, cancel)
            .await?;

        Ok(ModMutation {
            report: context.report,
            snapshot: self.managed_mod_snapshot(instance)?,
            plan,
        })
    }

    /// Moves one mod to the newest release that fits this instance.
    ///
    /// A thin naming of [`Self::change_managed_mod_version`], because "update"
    /// is a word for one particular choice of version rather than a different
    /// operation. Reporting it as a no-op when there is nothing newer is the
    /// point of having it.
    pub async fn update_managed_mod(
        &self,
        instance: InstanceId,
        target: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutation> {
        self.change_managed_mod_version(
            instance,
            target,
            &ModVersionChoice::NewestCompatible,
            reporter,
            cancel,
        )
        .await
    }

    /// Works out what removing one mod would take with it.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance, target = %target.display()))]
    pub async fn plan_mod_removal(
        &self,
        instance: InstanceId,
        target: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutationPlan> {
        let context = self.mutation_context(instance, target)?;
        self.removal_plan(&context, reporter, cancel).await
    }

    /// Removes one mod, and the automatic dependencies nothing else needs.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance, target = %target.display()))]
    pub async fn remove_managed_mod(
        &self,
        instance: InstanceId,
        target: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutation> {
        let mut context = self.mutation_context(instance, target)?;
        let plan = self.removal_plan(&context, reporter, cancel).await?;
        let removals = removals_for(&plan);
        self.commit(&mut context, &[], &removals, reporter, cancel)
            .await?;

        Ok(ModMutation {
            report: context.report,
            snapshot: self.managed_mod_snapshot(instance)?,
            plan,
        })
    }

    // -- planning ---------------------------------------------------------

    /// Loads everything a mutation needs and checks that it is allowed at all.
    fn mutation_context(&self, instance: InstanceId, target: &Path) -> CoreResult<MutationContext> {
        let record = self.client(instance)?;
        let snapshot = self.managed_mod_snapshot(instance)?;
        let entry = entry_at(&snapshot, target)?.clone();

        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        let recipe = if store.has_recipe() {
            store.read_recipe()?
        } else {
            // A mod cannot be recorded without a recipe, so reaching here means
            // the entry came from one that has since gone.
            return Err(CoreError::ModNotManaged {
                path: target.to_path_buf(),
            });
        };

        Ok(MutationContext {
            target: target_for(&record),
            record,
            entry,
            recipe,
            store,
            report: ephervex_pack::ManagedMutationReport::default(),
        })
    }

    /// The plan for moving one mod to a chosen release.
    async fn version_change_plan(
        &self,
        context: &MutationContext,
        choice: &ModVersionChoice,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutationPlan> {
        let mutability = ModMutability::of(&context.entry);
        if !mutability.can_change_version {
            return Err(CoreError::ModMutationRestricted {
                path: context.entry.logical_path.clone(),
                restriction: mutability
                    .restriction
                    .unwrap_or(MutationRestriction::IdentityUnresolved),
            });
        }
        let project_id = context
            .entry
            .identity
            .project_id()
            .unwrap_or_default()
            .to_owned();
        let installed_version = context
            .entry
            .identity
            .version_id()
            .unwrap_or_default()
            .to_owned();

        let request = match choice {
            ModVersionChoice::Exact(version) => {
                crate::mods::ModInstallRequest::new(project_id.clone())
                    .from_provider(context.provider())
                    .at_version(version.clone())
            }
            ModVersionChoice::NewestCompatible => {
                crate::mods::ModInstallRequest::new(project_id.clone())
                    .from_provider(context.provider())
            }
        };
        let resolved = self
            .resolve_replacement(&context.record, &request, reporter)
            .await?;

        let candidate = resolved.root.version.id.clone();
        let outcome = if candidate == installed_version {
            // The recorded release is already the right one. Whether that is a
            // no-op depends on the game directory, not on the recipe: a jar the
            // user deleted while bisecting is a repair, not "nothing to do".
            // A jar they turned off is neither — it is there, and putting it
            // back would turn it on again.
            if context.entry.presence == crate::mods::ModPresence::Missing {
                PlannedOutcome::Repair
            } else {
                PlannedOutcome::AlreadyCurrent
            }
        } else {
            PlannedOutcome::ChangeVersion {
                from: installed_version.clone(),
                to: candidate.clone(),
            }
        };

        let installs = planned_files(&resolved, &context.recipe);
        let (removes, orphans_unprovable) = self
            .orphaned_dependencies(
                &context.dependency_scope(),
                Some((&project_id, &candidate)),
                &[],
                reporter,
                cancel,
            )
            .await;

        Ok(ModMutationPlan {
            instance: context.record.id,
            target: context.entry.logical_path.clone(),
            name: display_name(&context.entry, &resolved),
            provider: context.entry.identity.provider(),
            project_id: Some(project_id.clone()),
            candidate_version: Some(candidate.clone()),
            outcome,
            installs,
            removes,
            orphans_unprovable,
            warnings: resolved.warnings.clone(),
        })
    }

    /// The plan for taking one mod out of the instance.
    async fn removal_plan(
        &self,
        context: &MutationContext,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<ModMutationPlan> {
        let mutability = ModMutability::of(&context.entry);
        if !mutability.can_remove {
            return Err(CoreError::ModMutationRestricted {
                path: context.entry.logical_path.clone(),
                restriction: mutability
                    .restriction
                    .unwrap_or(MutationRestriction::IdentityUnresolved),
            });
        }
        let project_id = context
            .entry
            .identity
            .project_id()
            .unwrap_or_default()
            .to_owned();

        // The mod itself goes first, then whatever nothing else needs once it
        // is gone.
        let mut removes = vec![planned_from_entry(&context.entry)];
        let (orphans, orphans_unprovable) = self
            .orphaned_dependencies(
                &context.dependency_scope(),
                None,
                &[project_id.as_str()],
                reporter,
                cancel,
            )
            .await;
        removes.extend(orphans);

        Ok(ModMutationPlan {
            instance: context.record.id,
            target: context.entry.logical_path.clone(),
            name: context.entry.display_name().to_owned(),
            provider: context.entry.identity.provider(),
            project_id: Some(project_id.clone()),
            candidate_version: None,
            outcome: PlannedOutcome::Remove,
            installs: Vec::new(),
            removes,
            orphans_unprovable,
            warnings: Vec::new(),
        })
    }

    /// Resolves the replacement graph, allowing installed mods to move.
    async fn resolve_replacement(
        &self,
        record: &ClientRecord,
        request: &crate::mods::ModInstallRequest,
        reporter: &OperationReporter,
    ) -> CoreResult<ModInstallPlan> {
        self.plan_for(record, request, InstalledPolicy::Replace, reporter)
            .await
    }

    /// Which automatic dependencies would have no owner left.
    ///
    /// Returns the orphans and whether the answer is trustworthy. An untrusted
    /// answer is an empty one: a dependency that cannot be proven unnecessary is
    /// kept, because the cost of being wrong is an instance that will not start.
    ///
    /// `moving` names a root that is changing version, so it is resolved at its
    /// new release rather than its current one. `dropping` names roots that are
    /// going away and contribute nothing.
    ///
    /// Takes a desired state and a target rather than a client, because a
    /// linked server asks exactly this question about its own mods and must not
    /// get a second implementation of the answer.
    pub(crate) async fn orphaned_dependencies(
        &self,
        scope: &DependencyScope<'_>,
        moving: Option<(&str, &str)>,
        dropping: &[&str],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> (Vec<PlannedFile>, bool) {
        let recorded = scope.recipe.recorded_mods_for(scope.side);
        let candidates: Vec<&RecordedMod> = recorded
            .iter()
            .filter(|entry| entry.identity.provider() == Some(scope.provider))
            .filter(|entry| entry.origin == ModOrigin::RequiredDependency)
            .filter(|entry| entry.identity.project_id().is_some())
            .collect();
        if candidates.is_empty() {
            return (Vec::new(), false);
        }

        let required = match self
            .required_projects(scope, &recorded, moving, dropping, reporter, cancel)
            .await
        {
            Ok(required) => required,
            Err(error) => {
                warn!(
                    %error,
                    "keeping every dependency; the catalog could not confirm which are still needed"
                );
                return (Vec::new(), true);
            }
        };

        let orphans = candidates
            .into_iter()
            .filter(|entry| {
                entry
                    .identity
                    .project_id()
                    .is_some_and(|project| !required.contains(project))
            })
            .map(planned_from_recorded)
            .collect();
        (orphans, false)
    }

    /// Every project this desired state's own roots still reach.
    ///
    /// A root is a mod a human chose: one they picked by name, or an optional
    /// dependency they ticked. A pack's own mods are not roots and are not
    /// resolved — a pack import records every file it declares, and never runs
    /// the resolver, so no `RequiredDependency` entry can belong to one. Walking
    /// two hundred pack mods to learn that would cost two hundred lookups to
    /// confirm something the import already guarantees.
    async fn required_projects(
        &self,
        scope: &DependencyScope<'_>,
        recorded: &[RecordedMod],
        moving: Option<(&str, &str)>,
        dropping: &[&str],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<BTreeSet<String>> {
        let mut roots: BTreeMap<String, Option<String>> = BTreeMap::new();
        for entry in recorded {
            if entry.identity.provider() != Some(scope.provider) {
                continue;
            }
            if !matches!(
                entry.origin,
                ModOrigin::UserSelected | ModOrigin::SelectedOptionalDependency
            ) {
                continue;
            }
            let Some(project) = entry.identity.project_id() else {
                // A root whose project cannot be named cannot be resolved, and
                // a partial answer would look like proof. Refuse the whole
                // question instead.
                return Err(CoreError::ModIdentityIncomplete {
                    path: entry.logical_path.clone(),
                });
            };
            roots.insert(
                project.to_owned(),
                entry.identity.version_id().map(ToOwned::to_owned),
            );
        }

        for project in dropping {
            roots.remove(*project);
        }
        if let Some((project, version)) = moving {
            roots.insert(project.to_owned(), Some(version.to_owned()));
        }

        let installed = crate::mods::installed_projects_of(Some(scope.recipe), scope.provider);
        let mut required = BTreeSet::new();
        for (project, version) in roots {
            let mut request =
                crate::mods::ModInstallRequest::new(project).from_provider(scope.provider);
            if let Some(version) = version {
                request = request.at_version(version);
            }
            let plan = self
                .resolve_against(
                    scope.target,
                    &installed,
                    &request,
                    InstalledPolicy::Replace,
                    reporter,
                    cancel,
                )
                .await?;
            if cancel.is_cancelled() {
                return Err(PackError::Canceled.into());
            }
            for project in plan.resolved_projects() {
                required.insert(project.to_owned());
            }
        }
        Ok(required)
    }

    // -- carrying it out --------------------------------------------------

    /// Turns a plan into the recipe entries the mutation writes.
    ///
    /// Resolved once more so that the exact version the plan names is the exact
    /// version that gets installed, with its own hashes and addresses rather
    /// than any carried over from the release being replaced.
    async fn upserts_for(
        &self,
        context: &MutationContext,
        plan: &ModMutationPlan,
        reporter: &OperationReporter,
    ) -> CoreResult<Vec<PackFileRecipe>> {
        let (Some(project_id), Some(version)) = (&plan.project_id, &plan.candidate_version) else {
            return Ok(Vec::new());
        };

        // Pinned to the exact release the plan named. If that release has been
        // withdrawn or no longer fits, the resolver refuses here — which is the
        // required behavior, and the reason the plan is not simply trusted.
        let request = crate::mods::ModInstallRequest::new(project_id.clone())
            .from_provider(context.provider())
            .at_version(version.clone());
        let resolved = self
            .resolve_replacement(&context.record, &request, reporter)
            .await?;

        resolved
            .all_mods()
            .into_iter()
            .map(|planned| crate::mods::recipe_entry(planned, resolved.provider))
            .collect()
    }

    /// Runs the mutation transaction.
    async fn commit(
        &self,
        context: &mut MutationContext,
        upserts: &[PackFileRecipe],
        removals: &[ManagedProject],
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        let manager = self.artifact_manager()?;

        let retired = ephervex_storage::metadata::retired_dir(&context.record.root);
        // Anything an interrupted mutation could not clean up goes now, so the
        // directory cannot grow across operations.
        ephervex_pack::clear_retired(&retired);

        context.report = ephervex_pack::mutate_managed(ManagedMutation {
            recipe: &mut context.recipe,
            upserts,
            removals,
            game_dir: &ephervex_storage::metadata::client_game_dir(&context.record.root),
            retired_dir: &retired,
            store: &context.store,
            fetch: ArtifactFetch {
                manager: &manager,
                cache: &CacheLayout::new(self.paths().cache_dir()),
                // Asked per file from its own provider identity, so a mutation
                // touching mods from two catalogs offers each secret only to the
                // artifacts it belongs to.
                credentials: &self.provider_credentials(),
                reporter,
                cancel,
            },
        })
        .await?;
        Ok(())
    }
}

/// Everything one mutation reads and writes.
#[derive(Debug)]
struct MutationContext {
    record: ClientRecord,
    entry: ManagedModEntry,
    recipe: PackRecipe,
    target: InstanceTarget,
    store: SourceStore,
    report: ephervex_pack::ManagedMutationReport,
}

impl MutationContext {
    fn dependency_scope(&self) -> DependencyScope<'_> {
        DependencyScope {
            recipe: &self.recipe,
            side: ephervex_pack::PackSide::Client,
            target: &self.target,
            provider: self.provider(),
        }
    }

    /// The catalog this mod came from.
    ///
    /// Every mutation this context reaches has a resolved identity (the
    /// mutability check refuses before a plan is built otherwise), so the
    /// fallback is unreachable and exists only so that a refusal is a refusal
    /// rather than a panic.
    fn provider(&self) -> ModProvider {
        self.entry
            .identity
            .provider()
            .unwrap_or(ModProvider::Modrinth)
    }
}

/// One desired state, and the exact release and loader it is resolved against.
///
/// The pair every dependency question needs. Naming it keeps a client and a
/// linked server asking the same question of the same code rather than each
/// carrying its own idea of what "still required" means.
#[derive(Debug)]
pub(crate) struct DependencyScope<'a> {
    pub recipe: &'a PackRecipe,
    /// Which side's recorded desired state the mods are read from.
    pub side: ephervex_pack::PackSide,
    pub target: &'a InstanceTarget,
    /// The catalog this question is about.
    ///
    /// Dependency graphs do not cross catalogs: a project id belongs to one
    /// provider, and only that provider's mods can be kept alive or orphaned by
    /// its own roots. Asking one catalog about another's ids would resolve the
    /// wrong projects and collect mods that are still needed.
    pub provider: ModProvider,
}

/// The entry at one logical path, or a refusal naming it.
fn entry_at<'a>(
    snapshot: &'a ManagedModSnapshot,
    target: &Path,
) -> CoreResult<&'a ManagedModEntry> {
    snapshot
        .mods
        .iter()
        .find(|entry| entry.logical_path == target)
        .ok_or_else(|| CoreError::ModNotManaged {
            path: target.to_path_buf(),
        })
}

/// The projects a plan says the instance must no longer have.
fn removals_for(plan: &ModMutationPlan) -> Vec<ManagedProject> {
    let Some(provider) = plan.provider else {
        // Nothing with an unproven identity reaches a plan, so this is a
        // refusal that cannot happen rather than a case to guess at.
        return Vec::new();
    };
    plan.removes
        .iter()
        .filter_map(|file| Some(ManagedProject::new(provider, file.project_id.clone()?)))
        .collect()
}

/// A resolved plan's mods as something to show, carrying the ownership each one
/// will end up with.
fn planned_files(plan: &ModInstallPlan, recipe: &PackRecipe) -> Vec<PlannedFile> {
    plan.all_mods()
        .into_iter()
        .map(|planned| PlannedFile {
            logical_path: planned_path(planned),
            name: planned.project.title.clone(),
            project_id: Some(planned.version.project_id.clone()),
            version_id: Some(planned.version.id.clone()),
            // A mod that is already recorded keeps the ownership it has. A
            // dependency does not become a root because an unrelated update
            // resolved it again.
            origin: recorded_origin(recipe, plan.provider, &planned.version.project_id)
                .unwrap_or(planned.origin),
        })
        .collect()
}

fn planned_path(planned: &PlannedMod) -> PathBuf {
    let name = ephervex_pack::safe_file_name(&planned.version.file.file_name)
        .unwrap_or_else(|| PathBuf::from(&planned.version.id));
    PathBuf::from(ephervex_pack::MODS_DIR).join(name)
}

fn recorded_origin(
    recipe: &PackRecipe,
    provider: ModProvider,
    project_id: &str,
) -> Option<ModOrigin> {
    recipe
        .managed_project(provider, project_id)
        .map(|entry| entry.origin)
}

fn planned_from_entry(entry: &ManagedModEntry) -> PlannedFile {
    PlannedFile {
        logical_path: entry.logical_path.clone(),
        name: entry.display_name().to_owned(),
        project_id: entry.identity.project_id().map(ToOwned::to_owned),
        version_id: entry.identity.version_id().map(ToOwned::to_owned),
        origin: entry.origin,
    }
}

/// The same, for a mod read straight out of a desired state.
///
/// A recorded mod carries no provider metadata, so its label is its file name.
/// That is the honest answer: nothing here has asked a catalog what the project
/// is called.
pub(crate) fn planned_from_recorded(entry: &RecordedMod) -> PlannedFile {
    PlannedFile {
        logical_path: entry.logical_path.clone(),
        name: entry.file_name.clone(),
        project_id: entry.identity.project_id().map(ToOwned::to_owned),
        version_id: entry.identity.version_id().map(ToOwned::to_owned),
        origin: entry.origin,
    }
}

/// The best name for the mod being acted on.
fn display_name(entry: &ManagedModEntry, plan: &ModInstallPlan) -> String {
    if plan.root.project.title.is_empty() {
        entry.display_name().to_owned()
    } else {
        plan.root.project.title.clone()
    }
}

/// The instance's exact Minecraft version and loader.
fn target_for(record: &ClientRecord) -> InstanceTarget {
    InstanceTarget::new(record.minecraft_version.clone(), record.loader.kind)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_model::{Hash, HashAlgorithm};
    use ephervex_pack::{ModIdentity, SideRequirement};

    use super::*;
    use crate::mods::ModPresence;

    fn entry(origin: ModOrigin, identity: ModIdentity) -> ManagedModEntry {
        ManagedModEntry {
            logical_path: PathBuf::from("mods/thing.jar"),
            file_name: "thing.jar".to_owned(),
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
            presence: ModPresence::Installed,
            details: None,
            cached: None,
            icon: None,
        }
    }

    fn resolved() -> ModIdentity {
        ModIdentity::Version {
            provider: ModProvider::Modrinth,
            project_id: "ROOTPROJ".to_owned(),
            version_id: "ROOTV1".to_owned(),
            file_id: None,
        }
    }

    fn project_only() -> ModIdentity {
        ModIdentity::Project {
            provider: ModProvider::Modrinth,
            project_id: "ROOTPROJ".to_owned(),
        }
    }

    #[test]
    fn a_mod_the_user_chose_may_be_moved_or_taken_away() {
        let mutability = ModMutability::of(&entry(ModOrigin::UserSelected, resolved()));
        assert!(mutability.can_change_version);
        assert!(mutability.can_update);
        assert!(mutability.can_remove);
        assert!(mutability.can_list_versions);
        assert_eq!(mutability.restriction, None);
    }

    #[test]
    fn an_automatic_dependency_has_no_independent_life() {
        // Its version follows its dependents, and removing it by hand would
        // leave whatever asked for it broken.
        let mutability = ModMutability::of(&entry(ModOrigin::RequiredDependency, resolved()));
        assert!(!mutability.can_change_version);
        assert!(!mutability.can_update);
        assert!(!mutability.can_remove);
        assert!(
            mutability.can_list_versions,
            "seeing what else exists is not a mutation"
        );
        assert_eq!(
            mutability.restriction,
            Some(MutationRestriction::Dependency)
        );
    }

    #[test]
    fn an_optional_dependency_may_be_untaken_but_not_moved() {
        // Both halves of its name are true: a human ticked it, so unticking it
        // is theirs; the resolver picks its version, so moving it is not.
        let mutability =
            ModMutability::of(&entry(ModOrigin::SelectedOptionalDependency, resolved()));
        assert!(!mutability.can_change_version);
        assert!(!mutability.can_update);
        assert!(mutability.can_remove);
        assert_eq!(
            mutability.restriction,
            Some(MutationRestriction::Dependency)
        );
    }

    #[test]
    fn a_pack_owned_mod_is_refused_and_says_which_refusal_it_is() {
        let mutability = ModMutability::of(&entry(ModOrigin::PackDeclared, resolved()));
        assert!(!mutability.can_change_version);
        assert!(!mutability.can_update);
        assert!(!mutability.can_remove);
        assert!(
            mutability.can_list_versions,
            "a pack's mod still has versions worth showing"
        );
        assert_eq!(mutability.restriction, Some(MutationRestriction::PackOwned));
    }

    #[test]
    fn ownership_is_reported_before_identity_when_both_would_refuse() {
        // "The pack owns this" is a more useful thing to be told than "its
        // version is unknown", and for a pack mod both are often true.
        let mutability = ModMutability::of(&entry(ModOrigin::PackDeclared, project_only()));
        assert_eq!(mutability.restriction, Some(MutationRestriction::PackOwned));
        assert!(mutability.can_list_versions);
    }

    #[test]
    fn a_mod_whose_installed_release_is_unproven_is_not_updated_on_a_guess() {
        // Without the current version there is no way to tell an update from a
        // no-op, or to know what is being replaced.
        let mutability = ModMutability::of(&entry(ModOrigin::UserSelected, project_only()));
        assert!(!mutability.can_change_version);
        assert!(!mutability.can_update);
        assert!(!mutability.can_remove);
        assert!(mutability.can_list_versions);
        assert_eq!(
            mutability.restriction,
            Some(MutationRestriction::VersionUnknown)
        );
    }

    #[test]
    fn a_mod_with_no_provenance_offers_nothing_at_all() {
        for origin in [
            ModOrigin::UserSelected,
            ModOrigin::RequiredDependency,
            ModOrigin::SelectedOptionalDependency,
        ] {
            let mutability = ModMutability::of(&entry(origin, ModIdentity::Unresolved));
            assert!(!mutability.can_change_version, "{origin:?}");
            assert!(!mutability.can_update, "{origin:?}");
            assert!(!mutability.can_remove, "{origin:?}");
            assert!(
                !mutability.can_list_versions,
                "there is no project to list versions of: {origin:?}"
            );
        }
        assert_eq!(
            ModMutability::of(&entry(ModOrigin::UserSelected, ModIdentity::Unresolved)).restriction,
            Some(MutationRestriction::IdentityUnresolved)
        );
    }

    #[test]
    fn deleting_the_jar_outright_is_offered_for_a_hand_placed_mod_and_no_other() {
        // The permission is about who owns the file, not about what is known
        // about it, so an identified hand-placed jar keeps it and a fully
        // resolved user-selected one never gains it.
        for identity in [resolved(), project_only(), ModIdentity::Unresolved] {
            assert!(
                ModMutability::of(&entry(ModOrigin::HandPlaced, identity.clone())).can_delete_file
            );
        }
        for origin in [
            ModOrigin::PackDeclared,
            ModOrigin::UserSelected,
            ModOrigin::RequiredDependency,
            ModOrigin::SelectedOptionalDependency,
        ] {
            assert!(
                !ModMutability::of(&entry(origin, resolved())).can_delete_file,
                "{origin:?}"
            );
        }
    }

    #[test]
    fn every_refusal_names_a_distinct_reason_and_a_distinct_message() {
        let all = [
            MutationRestriction::PackOwned,
            MutationRestriction::Dependency,
            MutationRestriction::IdentityUnresolved,
            MutationRestriction::VersionUnknown,
        ];
        let ids: BTreeSet<&str> = all.iter().map(|it| it.message_id()).collect();
        assert_eq!(ids.len(), all.len(), "two refusals share a sentence");
        let names: BTreeSet<&str> = all.iter().map(|it| it.as_str()).collect();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn a_plan_that_changes_nothing_says_so() {
        let plan = ModMutationPlan {
            provider: Some(ModProvider::Modrinth),
            instance: InstanceId::new(),
            target: PathBuf::from("mods/thing.jar"),
            name: "Thing".to_owned(),
            project_id: Some("ROOTPROJ".to_owned()),
            candidate_version: Some("ROOTV1".to_owned()),
            outcome: PlannedOutcome::AlreadyCurrent,
            installs: Vec::new(),
            removes: Vec::new(),
            orphans_unprovable: false,
            warnings: Vec::new(),
        };
        assert!(!plan.changes_anything());
        assert_eq!(plan.outcome.as_str(), "already-current");
    }

    #[test]
    fn a_removal_shows_the_dependencies_it_would_take_with_it() {
        let target = PathBuf::from("mods/root.jar");
        let plan = ModMutationPlan {
            provider: Some(ModProvider::Modrinth),
            instance: InstanceId::new(),
            target: target.clone(),
            name: "Root".to_owned(),
            project_id: Some("ROOTPROJ".to_owned()),
            candidate_version: None,
            outcome: PlannedOutcome::Remove,
            installs: Vec::new(),
            removes: vec![
                PlannedFile {
                    logical_path: target,
                    name: "Root".to_owned(),
                    project_id: Some("ROOTPROJ".to_owned()),
                    version_id: Some("ROOTV1".to_owned()),
                    origin: ModOrigin::UserSelected,
                },
                PlannedFile {
                    logical_path: PathBuf::from("mods/library.jar"),
                    name: "Library".to_owned(),
                    project_id: Some("LIBPROJ".to_owned()),
                    version_id: Some("LIBV1".to_owned()),
                    origin: ModOrigin::RequiredDependency,
                },
            ],
            orphans_unprovable: false,
            warnings: Vec::new(),
        };

        let collected: Vec<&str> = plan
            .collected_dependencies()
            .map(|file| file.name.as_str())
            .collect();
        assert_eq!(
            collected,
            vec!["Library"],
            "the mod being removed is not one of its own consequences"
        );
        assert!(plan.changes_anything());
    }
}
