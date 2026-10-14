//! Moving a client instance to a different build of the loader it already has.
//!
//! # A version change, not a conversion
//!
//! Forge to a different Forge, NeoForge to a different NeoForge. Nothing here
//! turns one loader into another, or a vanilla instance into a modded one:
//! those change what the instance *is*, invalidate every mod in it, and are a
//! different product decision from "try the next build to see if the bug moves".
//! A request that would cross a family is refused by name rather than
//! reinterpreted.
//!
//! # What is checked before anything is installed
//!
//! * the instance already has a loader family, so there is one to stay inside;
//! * the requested build is one the loader's own catalog offers *for this
//!   instance's exact Minecraft release*, so a build for another release cannot
//!   be typed in;
//! * every managed mod whose exact release is known is re-checked against the
//!   candidate.
//!
//! That last check is worth being precise about. A provider states which loader
//! *families* a release supports, not which builds, so a within-family move
//! cannot make a mod incompatible as far as Modrinth is concerned - and this
//! reports exactly that rather than implying it verified more than it did.
//! What it does report honestly is how many mods it could not check at all:
//! a mod with no proven release is not evidence of anything, and saying "all
//! clear" while three jars are unidentified would be the lie worth avoiding.
//!
//! # Mods are never changed to make a loader change work
//!
//! A loader change and a mod update are two decisions. If a mod is known not to
//! fit the candidate, this refuses unless the caller confirms, and it still does
//! not touch the mod. Silently updating mods to rescue a loader change would
//! move several things at once in an instance whose whole purpose is to isolate
//! one variable.

use ephervex_download::OperationReporter;
use ephervex_minecraft::Platform;
use ephervex_mod_platform::{IncompatibilityReason, InstanceTarget, ModVersionInfo};
use ephervex_model::{InstanceId, LoaderKind, LoaderSelection, ModProvider};
use ephervex_storage::{ClientCommit, ClientCommitWarning};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::{App, JavaPlanRequest, JavaTarget};
use crate::error::{CoreError, CoreResult};
use crate::host_recipe_gate::HostRecipeGate;
use crate::mods::InstalledHost;

/// One managed mod that does not fit a candidate loader build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatibleMod {
    pub logical_path: std::path::PathBuf,
    pub name: String,
    pub reasons: Vec<IncompatibilityReason>,
}

/// What moving an instance to a different loader build would involve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderVersionPlan {
    pub instance: InstanceId,
    pub loader: LoaderKind,
    pub minecraft_version: String,
    /// The build the instance runs now, when it names one.
    pub current_version: Option<String>,
    /// The build it would move to. Exactly what was asked for.
    pub candidate_version: String,
    /// Managed mods whose exact release was checked against the candidate.
    pub checked: usize,
    /// Managed mods that could not be checked, because nothing proves which
    /// release they are. Reported rather than counted as clear.
    pub unverifiable: usize,
    /// Mods known not to fit. Empty under a provider that describes loader
    /// support by family, which is every provider this build talks to.
    pub incompatible: Vec<IncompatibleMod>,
}

impl LoaderVersionPlan {
    /// Whether this would change anything.
    #[must_use]
    pub fn changes_anything(&self) -> bool {
        self.current_version.as_deref() != Some(self.candidate_version.as_str())
    }

    /// Whether carrying it out needs the user to accept a known problem.
    #[must_use]
    pub fn needs_confirmation(&self) -> bool {
        !self.incompatible.is_empty()
    }
}

/// What a carried-out loader change did.
#[derive(Debug, Clone)]
pub struct LoaderVersionChange {
    pub plan: LoaderVersionPlan,
    /// The selection the instance now records.
    pub loader: LoaderSelection,
    /// Conditions on the saved change the user should hear about.
    pub warnings: Vec<ClientCommitWarning>,
}

impl App {
    /// The exact builds this instance could move to.
    ///
    /// Its own family and its own Minecraft release, so nothing in the list is
    /// a family switch and nothing in it is for another version of the game.
    #[instrument(skip(self), fields(instance = %instance))]
    pub async fn loader_versions_for_instance(
        &self,
        instance: InstanceId,
    ) -> CoreResult<ephervex_loaders::LoaderVersionListing> {
        let record = self.current_client(instance)?;
        if record.loader.kind == LoaderKind::Vanilla {
            return Err(CoreError::LoaderFamilyFixed {
                loader: LoaderKind::Vanilla,
            });
        }
        self.loader_versions(record.loader.kind, &record.minecraft_version)
            .await
    }

    /// Works out what moving to one exact build would involve.
    #[instrument(skip(self), fields(instance = %instance, candidate = exact))]
    pub async fn plan_loader_version_change(
        &self,
        instance: InstanceId,
        exact: &str,
    ) -> CoreResult<LoaderVersionPlan> {
        let record = self.current_client(instance)?;
        if record.loader.kind == LoaderKind::Vanilla {
            return Err(CoreError::LoaderFamilyFixed {
                loader: LoaderKind::Vanilla,
            });
        }

        // A build the loader's own catalog does not offer for this exact
        // release is refused here, before anything is downloaded, and named as
        // unavailable rather than as an installation failure.
        let candidate = exact.trim();
        ephervex_loaders::require_supported_installer_format(
            record.loader.kind,
            &record.minecraft_version,
            candidate,
        )?;
        let listing = self
            .loader_versions(record.loader.kind, &record.minecraft_version)
            .await?;
        if !listing
            .builds
            .iter()
            .any(|build| build.version == candidate)
        {
            return Err(ephervex_loaders::LoaderError::VersionUnavailable {
                loader: record.loader.kind,
                minecraft: record.minecraft_version.clone(),
                mode: "exact",
            }
            .into());
        }

        let target = InstanceTarget::new(
            record.minecraft_version.clone(),
            // The family does not change, which is the whole point of the
            // operation and the reason a mod's declared support cannot move.
            record.loader.kind,
        );
        let (checked, unverifiable, incompatible) = self.check_mods(instance, &target).await?;

        Ok(LoaderVersionPlan {
            instance,
            loader: record.loader.kind,
            minecraft_version: record.minecraft_version.clone(),
            current_version: record.loader.version.clone(),
            candidate_version: candidate.to_owned(),
            checked,
            unverifiable,
            incompatible,
        })
    }

    /// Installs one exact loader build and records it as what this instance is.
    ///
    /// The order is the point: nothing about the instance changes until the
    /// build is on disk. A failed install leaves the previous selection
    /// running, which is what makes trying a build cheap enough to be useful.
    #[instrument(skip(self, reporter, cancel), fields(instance = %instance, candidate = exact))]
    pub async fn change_loader_version(
        &self,
        instance: InstanceId,
        exact: &str,
        accept_incompatible_mods: bool,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<LoaderVersionChange> {
        self.with_artifact_cache(Box::pin(self.change_loader_version_holding_the_cache(
            instance,
            exact,
            accept_incompatible_mods,
            reporter,
            cancel,
        )))
        .await
    }

    async fn change_loader_version_holding_the_cache(
        &self,
        instance: InstanceId,
        exact: &str,
        accept_incompatible_mods: bool,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<LoaderVersionChange> {
        // What a running game was started with cannot be changed underneath
        // it, and the record would then misdescribe the reproduction.
        self.refuse_while_running(instance)?;
        self.refuse_mod_change_during_isolation(InstalledHost::Instance(instance))?;
        let plan = self.plan_loader_version_change(instance, exact).await?;
        let record = self.current_client(instance)?;

        if plan.needs_confirmation() && !accept_incompatible_mods {
            return Err(CoreError::LoaderChangeBlockedByMods {
                incompatible: plan.incompatible.len(),
            });
        }
        let selection = LoaderSelection::exact(plan.loader, plan.candidate_version.clone());
        if !plan.changes_anything() {
            let gate = self
                .hold_mod_change(InstalledHost::Instance(instance), cancel)
                .await?;
            return Self::settle_unchanged_build(&record, plan, selection, &gate);
        }

        let metadata = self.minecraft_version(&record.minecraft_version).await?;
        let java = self
            .plan_java(
                &record.minecraft_version,
                &JavaPlanRequest {
                    target: JavaTarget::Client(instance),
                    explicit_path: None,
                    install_if_missing: true,
                    cancel: cancel.clone(),
                    reporter: Some(reporter.clone()),
                },
            )
            .await?;

        self.loaders()?
            .provider(plan.loader)
            .install_client(ephervex_loaders::ClientInstallRequest {
                minecraft: &record.minecraft_version,
                exact_loader: Some(plan.candidate_version.as_str()),
                parent: &metadata,
                java: &java.runtime.executable,
                context: &ephervex_minecraft::LaunchFeatures::default()
                    .rule_context(Platform::host()),
                reporter,
                cancel,
            })
            .await?;

        // The installation can take long enough for a launch to begin. The
        // claim is kept to the end of the change, so none begins before it.
        let _claim = self.claim_stopped_client(instance)?;

        // Held from the last check to the recipe write, so a session cannot
        // start between them.
        let gate = self
            .hold_mod_change(InstalledHost::Instance(instance), cancel)
            .await?;

        // Only now. Everything above can fail without the instance having
        // changed at all.
        let ClientCommit { record, warnings } =
            self.registry()
                .modify_client_committed(instance, |current| {
                    current.loader = selection.clone();
                })?;
        Self::align_recipe_loader(&record, &gate)?;

        info!(
            loader = %plan.loader,
            from = plan.current_version.as_deref().unwrap_or("none"),
            to = %plan.candidate_version,
            "changed a client instance's loader build"
        );
        Ok(LoaderVersionChange {
            plan,
            loader: selection,
            warnings,
        })
    }

    /// Completes a request for the build the instance already records.
    ///
    /// The recipe is re-aligned even though nothing else changes: the record is
    /// committed before the recipe, so an earlier attempt whose recipe write
    /// failed leaves exactly this state, and repeating the request is how the
    /// user repairs it.
    fn settle_unchanged_build(
        record: &ephervex_storage::ClientRecord,
        plan: LoaderVersionPlan,
        selection: LoaderSelection,
        gate: &HostRecipeGate,
    ) -> CoreResult<LoaderVersionChange> {
        info!("the instance already runs this loader build");
        Self::align_recipe_loader(record, gate)?;
        Ok(LoaderVersionChange {
            plan,
            loader: selection,
            warnings: Vec::new(),
        })
    }

    /// Points the retained recipe at the build the instance now runs.
    ///
    /// The recipe describes the instance's desired state, and a linked server
    /// is refused outright when its client's loader and the recipe's disagree.
    /// Only the target moves: the pack's name, its version, and every file it
    /// declared are untouched, so what the pack shipped is still recorded
    /// exactly as it was imported.
    ///
    /// The caller holds the instance's recipe gate, which the parameter proves.
    fn align_recipe_loader(
        record: &ephervex_storage::ClientRecord,
        _gate: &HostRecipeGate,
    ) -> CoreResult<()> {
        let store =
            ephervex_pack::SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        if !store.has_recipe() {
            return Ok(());
        }
        let mut recipe = store.read_recipe()?;
        if recipe.loader == record.loader {
            return Ok(());
        }
        recipe.loader = record.loader.clone();
        store.write_recipe(&recipe)?;
        Ok(())
    }

    /// Re-checks every managed mod whose exact release is known.
    ///
    /// Returns how many were checked, how many could not be, and which do not
    /// fit. A catalog that cannot be reached makes everything unverifiable
    /// rather than failing the plan: a user trying loader builds while offline
    /// should be told what is unknown, not stopped.
    async fn check_mods(
        &self,
        instance: InstanceId,
        target: &InstanceTarget,
    ) -> CoreResult<(usize, usize, Vec<IncompatibleMod>)> {
        let snapshot = self.managed_mod_snapshot(instance)?;
        if snapshot.is_empty() {
            return Ok((0, 0, Vec::new()));
        }
        let checkable: Vec<&crate::mods::ManagedModEntry> = snapshot
            .mods
            .iter()
            .filter(|entry| entry.identity.is_fully_resolved())
            .collect();
        let unverifiable = snapshot.len() - checkable.len();
        if checkable.is_empty() {
            return Ok((0, unverifiable, Vec::new()));
        }

        let reporter = OperationReporter::silent("checking-mod-compatibility");
        // Each catalog is asked about its own mods, and each answers in its own
        // spelling of the loader. An instance holding mods from two of them is
        // checked against both rather than having one catalog's ids sent to the
        // other.
        let mut versions: Vec<(ModProvider, ModVersionInfo, &'static str)> = Vec::new();
        for catalog in [ModProvider::Modrinth, ModProvider::CurseForge] {
            let ids: Vec<String> = checkable
                .iter()
                .filter(|entry| entry.identity.provider() == Some(catalog))
                .filter_map(|entry| entry.identity.version_id().map(ToOwned::to_owned))
                .collect();
            if ids.is_empty() {
                continue;
            }
            let provider = self.mod_provider(catalog)?;
            let loader_name = ephervex_mod_platform::loader_name_for(provider, target)?;
            match provider
                .versions_by_id(&ids, &reporter, &CancellationToken::new())
                .await
            {
                Ok(found) => versions.extend(
                    found
                        .into_iter()
                        .map(|version| (catalog, version, loader_name)),
                ),
                Err(error) => {
                    warn!(%error, provider = %catalog, "no mod was checked against the candidate loader build");
                    return Ok((0, snapshot.len(), Vec::new()));
                }
            }
        }

        let mut incompatible = Vec::new();
        let mut checked = 0;
        for entry in checkable {
            let Some((_, version, loader_name)) = versions.iter().find(|(catalog, version, _)| {
                entry.identity.provider() == Some(*catalog)
                    && Some(version.id.as_str()) == entry.identity.version_id()
            }) else {
                continue;
            };
            checked += 1;
            let compatibility = version.compatibility(target, loader_name);
            if !compatibility.is_compatible() {
                incompatible.push(IncompatibleMod {
                    logical_path: entry.logical_path.clone(),
                    name: entry.display_name().to_owned(),
                    reasons: compatibility.reasons().to_vec(),
                });
            }
        }
        let unchecked = snapshot.len() - checked;
        Ok((checked, unchecked, incompatible))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_storage::{NewClient, Paths};

    use super::*;

    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn plan(loader: LoaderKind, current: Option<&str>, candidate: &str) -> LoaderVersionPlan {
        LoaderVersionPlan {
            instance: InstanceId::new(),
            loader,
            minecraft_version: "1.20.1".to_owned(),
            current_version: current.map(ToOwned::to_owned),
            candidate_version: candidate.to_owned(),
            checked: 0,
            unverifiable: 0,
            incompatible: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_vanilla_instance_has_no_loader_build_to_change() {
        // Turning it into a modded one is a different operation, and refusing
        // by name is what keeps this from quietly becoming it.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .create_client_for_test(NewClient::new(
                "Vanilla repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");

        for result in [
            app.loader_versions_for_instance(record.id).await.err(),
            app.plan_loader_version_change(record.id, "47.1.106")
                .await
                .err(),
        ] {
            assert!(
                matches!(
                    result,
                    Some(CoreError::LoaderFamilyFixed {
                        loader: LoaderKind::Vanilla
                    })
                ),
                "expected a refusal naming the family: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn loader_builds_are_planned_for_the_committed_family_while_the_index_row_lags() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .create_client_for_test(NewClient::new(
                "Lagging",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100"),
            ))
            .expect("create");
        app.commit_client_without_index_for_test(record.id, |committed| {
            committed.loader = LoaderSelection::vanilla();
        })
        .expect("commit");
        assert_eq!(
            app.client(record.id).expect("row").loader.kind,
            LoaderKind::NeoForge
        );

        let reporter = OperationReporter::silent("test");
        let cancel = CancellationToken::new();
        for result in [
            app.loader_versions_for_instance(record.id).await.err(),
            app.plan_loader_version_change(record.id, "21.1.172")
                .await
                .err(),
            app.change_loader_version(record.id, "21.1.172", true, &reporter, &cancel)
                .await
                .err(),
        ] {
            assert!(
                matches!(
                    result,
                    Some(CoreError::LoaderFamilyFixed {
                        loader: LoaderKind::Vanilla
                    })
                ),
                "expected the committed vanilla selection to be honored: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn loader_builds_are_listed_for_the_committed_minecraft_version_while_the_index_row_lags()
    {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .create_client_for_test(NewClient::new(
                "Lagging",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100"),
            ))
            .expect("create");
        app.commit_client_without_index_for_test(record.id, |committed| {
            committed.minecraft_version = "1.12.2".to_owned();
        })
        .expect("commit");

        let error = app
            .loader_versions_for_instance(record.id)
            .await
            .expect_err("NeoForge was never published for the committed release");

        assert!(
            matches!(
                &error,
                CoreError::Loader(ephervex_loaders::LoaderError::LoaderNeverPublished {
                    minecraft,
                    ..
                })
                    if minecraft == "1.12.2"
            ),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_change_leaves_the_recorded_build_alone() {
        // The candidate is not a build any catalog offers, so planning refuses
        // before Java, metadata, or an installer is touched.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .create_client_for_test(NewClient::new(
                "Forge repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.1.106"),
            ))
            .expect("create");

        let result = app
            .change_loader_version(
                record.id,
                "not-a-build",
                false,
                &OperationReporter::silent("test"),
                &CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "an unavailable build must not install");
        assert_eq!(
            app.client(record.id).expect("record").loader,
            LoaderSelection::exact(LoaderKind::Forge, "47.1.106"),
            "the instance still runs what it ran before"
        );
    }

    #[tokio::test]
    async fn a_running_instance_refuses_a_loader_build_change() {
        // Refused before the catalog is consulted, so the refusal names the
        // running game rather than whatever planning would have reported.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .create_client_for_test(NewClient::new(
                "Forge repro",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.1.106"),
            ))
            .expect("create");
        let _lock = ephervex_storage::LaunchLock::acquire(&record.root).expect("hold the lock");

        let result = app
            .change_loader_version(
                record.id,
                "47.1.107",
                true,
                &OperationReporter::silent("test"),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(result, Err(CoreError::InstanceRunning { .. })),
            "expected a running-instance refusal: {result:?}"
        );
        assert_eq!(
            app.client(record.id).expect("record").loader,
            LoaderSelection::exact(LoaderKind::Forge, "47.1.106"),
            "the record still describes what the running game uses"
        );
    }

    #[test]
    fn repeating_a_change_to_the_recorded_build_realigns_a_recipe_left_behind() {
        // A recipe write that failed after the record moved leaves the recipe
        // on the old build; asking for the recorded build again must repair it
        // instead of reporting that there is nothing to do.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let old = LoaderSelection::exact(LoaderKind::Forge, "47.1.106");
        let new = LoaderSelection::exact(LoaderKind::Forge, "47.1.107");
        let record = app
            .create_client_for_test(NewClient::new("Forge repro", "1.20.1", new.clone()))
            .expect("create");
        let store =
            ephervex_pack::SourceStore::new(ephervex_storage::metadata::source_dir(&record.root));
        store
            .write_recipe(&ephervex_pack::PackRecipe::for_manual_instance(
                "Repro", "1.20.1", old,
            ))
            .expect("write the recipe");

        let mut unchanged = plan(LoaderKind::Forge, Some("47.1.107"), "47.1.107");
        unchanged.instance = record.id;
        let gate = crate::host_recipe_gate::hold_blocking(
            app.registry(),
            InstalledHost::Instance(record.id),
        )
        .expect("the gate");
        let change = App::settle_unchanged_build(&record, unchanged, new.clone(), &gate)
            .expect("the request completes");
        drop(gate);

        assert_eq!(change.loader, new);
        assert_eq!(
            store.read_recipe().expect("recipe").loader,
            new,
            "the recipe follows the record"
        );
    }

    #[test]
    fn moving_to_the_build_already_running_changes_nothing() {
        assert!(!plan(LoaderKind::NeoForge, Some("47.1.106"), "47.1.106").changes_anything());
        assert!(plan(LoaderKind::NeoForge, Some("47.1.106"), "47.1.107").changes_anything());
        assert!(
            plan(LoaderKind::NeoForge, None, "47.1.106").changes_anything(),
            "an instance with no recorded build is moving to one"
        );
    }

    #[test]
    fn a_known_incompatible_mod_is_surfaced_rather_than_repaired() {
        let mut plan = plan(LoaderKind::Forge, Some("47.1.106"), "47.4.0");
        assert!(!plan.needs_confirmation());

        plan.incompatible.push(IncompatibleMod {
            logical_path: std::path::PathBuf::from("mods/thing.jar"),
            name: "Thing".to_owned(),
            reasons: Vec::new(),
        });
        assert!(
            plan.needs_confirmation(),
            "the user decides; nothing here updates a mod to make the change fit"
        );
    }

    #[test]
    fn mods_that_cannot_be_checked_are_counted_rather_than_called_compatible() {
        let mut plan = plan(LoaderKind::Forge, Some("47.1.106"), "47.4.0");
        plan.checked = 2;
        plan.unverifiable = 3;
        assert!(
            !plan.needs_confirmation(),
            "an unverifiable mod is not a known problem"
        );
        assert_eq!(
            plan.unverifiable, 3,
            "but it is reported, rather than folded into the checked count"
        );
    }
}
