//! Changing what an instance's `mods/` directory is supposed to contain.
//!
//! # The problem this module exists to solve
//!
//! Installing a mod only ever adds. Changing one's version, or removing it,
//! also *takes something away* - and the moment an operation deletes, the
//! failure modes get worse than "nothing happened". The states worth naming:
//!
//! * both the old jar and the new one are in `mods/`, so the loader sees two
//!   builds of one mod and the game does not start;
//! * neither is there, so the instance lost a mod nobody asked to remove;
//! * the files changed and the recipe did not, so "Create Server" builds
//!   something that no longer matches the client.
//!
//! # The order that avoids all three
//!
//! ```text
//! plan ──▶ fetch+verify ──▶ retire old jars ──▶ write new jars ──▶ write recipe
//!                                  │                  │                 │
//!                                  └────── undo ──────┴──── undo ───────┘
//! ```
//!
//! Retiring is a *move*, not a delete: the old jar goes to `.launcher/retired/`
//! inside the same instance, which is a rename on one filesystem and therefore
//! atomic and instant. That is the whole trick. Every step before the recipe is
//! published can be undone exactly - new files are removed, retired files are
//! moved back - so a failure anywhere leaves the instance as it was, with its
//! old jars present and its recipe describing them.
//!
//! Retiring comes *before* writing, which matters for the case that looks
//! easiest: a project that keeps one file name across releases. Writing first
//! would overwrite the old jar, and there would be nothing left to restore if a
//! later step failed. Nothing is lost by the earlier move, because by then
//! every replacement artifact is downloaded and verified in the shared cache -
//! "ready" means verified, not written.
//!
//! Only after the recipe is on disk are the retired files deleted, and that
//! deletion is allowed to fail: a leftover file in launcher-owned scratch is
//! invisible to the game, whereas a leftover in `mods/` is a crash.
//!
//! # What this refuses to touch
//!
//! A jar Ephervex did not install. Retirement moves exactly the files the
//! recipe records, so a local debug build sitting in `mods/` is not something
//! any mutation can reach - the same rule the installer applies to
//! destinations, applied to removals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ephervex_model::{ModOrigin, ModProvider};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::error::{PackError, PackResult};
use crate::install::{ArtifactFetch, copy_into_instance, fetch_all};
use crate::managed::{
    ManagedDestinations, RecordedJar, disabled_path, occupant, occupied, recorded_jar,
    recording_side, require_one_entry_per_project, settle_recorded_paths,
};
use crate::recipe::{PackFileRecipe, PackRecipe, PackSide, ProviderFileIdentity};
use crate::store::SourceStore;

/// One provider project, which is how a mutation names what it acts on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManagedProject {
    pub provider: ModProvider,
    pub project_id: String,
}

impl ManagedProject {
    #[must_use]
    pub fn new(provider: ModProvider, project_id: impl Into<String>) -> Self {
        Self {
            provider,
            project_id: project_id.into(),
        }
    }
}

/// What one mutation should make true of an instance.
///
/// Deliberately declarative. The caller says which mods the instance must have
/// and which projects it must not, and this works out which files that implies
/// writing, retiring, and leaving alone. A caller that had to say "delete this,
/// then write that" would be deciding the order in which an instance can be
/// left broken.
#[derive(Debug)]
pub struct ManagedMutation<'a> {
    /// The instance's desired state, updated in place on success.
    pub recipe: &'a mut PackRecipe,
    /// Mods the instance must have afterwards, each carrying full provider
    /// identity, hashes, and download addresses. An entry whose project is
    /// already recorded replaces that record rather than joining it.
    pub upserts: &'a [PackFileRecipe],
    /// Projects the instance must not have afterwards.
    pub removals: &'a [ManagedProject],
    /// The side of the game this instance is. A requested mod is installed on
    /// this side, so a recorded entry for the same release that does not take
    /// this side is brought in line with the request rather than left to
    /// disagree with the jar that was just written.
    pub side: PackSide,
    /// The directory the game runs in. Logical paths are relative to it.
    pub game_dir: &'a Path,
    /// Launcher-owned scratch inside the same instance, where replaced files
    /// wait until the recipe is published.
    pub retired_dir: &'a Path,
    pub store: &'a SourceStore,
    pub fetch: ArtifactFetch<'a>,
}

/// What a mutation did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedMutationReport {
    /// Mods the instance did not have before.
    pub installed: Vec<PathBuf>,
    /// Mods whose version changed.
    pub replaced: Vec<ReplacedMod>,
    /// Mods that are no longer part of the instance.
    pub removed: Vec<PathBuf>,
    /// Recorded mods whose jar had gone missing and was put back at the same
    /// version. The recipe entry is kept as recorded, except that a record
    /// which did not take the requested side takes it.
    pub repaired: Vec<PathBuf>,
    /// Requested mods that were already installed at exactly the requested
    /// version, so nothing was written for them. The path is the one the recipe
    /// records, which is not necessarily the one the request named.
    pub unchanged: Vec<PathBuf>,
    /// Requested mods recorded at the requested version whose jar holds other
    /// bytes than the recorded ones. They were left exactly as they are: a
    /// changed jar is very often the local build the instance exists to test.
    pub modified: Vec<PathBuf>,
    /// Requested mods whose recorded jar was already in place but whose record
    /// did not take this side. The record now does; no file was written.
    pub recorded: Vec<PathBuf>,
    pub reused_from_cache: usize,
    pub bytes: u64,
    /// Cleanup that could not finish after the new recipe was published.
    ///
    /// The mutation itself succeeded, but these paths remain in
    /// launcher-owned recovery storage and need explicit reconciliation.
    pub recovery_issues: Vec<MutationRecoveryIssue>,
}

impl ManagedMutationReport {
    /// Whether this mutation changed the instance at all.
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        !self.installed.is_empty()
            || !self.replaced.is_empty()
            || !self.removed.is_empty()
            || !self.repaired.is_empty()
            || !self.recorded.is_empty()
    }
}

/// Which compensating filesystem action could not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationRecoveryAction {
    RemoveCreated,
    RestoreRetired,
    RemoveRetired,
}

/// One durable artifact left by an incomplete rollback or cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationRecoveryIssue {
    pub action: MutationRecoveryAction,
    pub path: PathBuf,
    pub error_kind: std::io::ErrorKind,
}

/// One mod that moved from one version to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedMod {
    /// Where the new jar is.
    pub logical_path: PathBuf,
    /// Where the old one was. The same path when the version's file name did
    /// not change, which is the case that most needs care: the new jar is
    /// written over nothing, because the old one is retired first.
    pub previous_path: PathBuf,
    pub previous_version: Option<String>,
    pub version: Option<String>,
}

/// What the pre-flight decided about one requested mod.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// Nothing is recorded for this project: an ordinary install.
    Install,
    /// The same version is already recorded and its jar is present.
    Unchanged,
    /// The same version is recorded with its jar present, but the record does
    /// not take the instance's side. Only the record changes.
    Record,
    /// The same version is recorded and its jar has gone missing.
    Repair,
    /// The same version is recorded and a jar is there, but not the recorded
    /// one. It is left alone.
    Altered,
    /// A different version is recorded. The old entry's path is carried so the
    /// old jar can be retired even when the file name did not change.
    Replace(Box<PackFileRecipe>),
}

/// Carries out one mutation.
#[instrument(
    skip_all,
    fields(
        game_dir = %request.game_dir.display(),
        upserts = request.upserts.len(),
        removals = request.removals.len(),
    )
)]
pub async fn mutate_managed(request: ManagedMutation<'_>) -> PackResult<ManagedMutationReport> {
    let ManagedMutation {
        recipe,
        upserts,
        removals,
        side,
        game_dir,
        retired_dir,
        store,
        fetch,
    } = request;
    let cancel = fetch.cancel;
    let settled = settle_recorded_paths(recipe, upserts, Some(side));
    let upserts = settled.as_slice();

    // Nothing below this line runs until the whole desired state is known to be
    // reachable: every destination safe, every removal something Ephervex
    // actually installed.
    let actions = plan(recipe, upserts, removals, side, game_dir).await?;
    let retirements = retirements(recipe, upserts, &actions, removals, game_dir);
    // The replacements whose predecessor was turned off, so the new build can
    // be written the same way round.
    let stay_disabled: BTreeSet<PathBuf> = retirements
        .iter()
        .filter(|retirement| retirement.disabled)
        .filter_map(|retirement| match &retirement.reason {
            RetirementReason::Replaced { new_path, .. } => Some(new_path.clone()),
            RetirementReason::Removed => None,
        })
        .collect();

    let mut report = ManagedMutationReport::default();
    let pending: Vec<(&PackFileRecipe, &Action)> = upserts
        .iter()
        .zip(&actions)
        .filter(|(file, action)| match action {
            Action::Unchanged => {
                report.unchanged.push(file.logical_path.clone());
                false
            }
            Action::Altered => {
                report.modified.push(file.logical_path.clone());
                false
            }
            Action::Record => {
                report.recorded.push(file.logical_path.clone());
                false
            }
            _ => true,
        })
        .collect();

    if pending.is_empty() && retirements.is_empty() && report.recorded.is_empty() {
        info!("the instance already matches the requested mod state");
        return Ok(report);
    }

    let wanted: Vec<&PackFileRecipe> = pending.iter().map(|(file, _)| *file).collect();
    let fetched = if wanted.is_empty() {
        Vec::new()
    } else {
        fetch_all(&wanted, fetch).await?
    };

    // Everything from here on can leave the instance changed, so each step
    // records exactly what it did and the whole lot is undone on failure.
    let mut journal = Journal::default();
    let outcome = retire(&retirements, game_dir, retired_dir, &mut journal)
        .and_then(|()| {
            write_new_files(
                &pending,
                &fetched,
                game_dir,
                &stay_disabled,
                cancel,
                &mut journal,
                &mut report,
            )
        })
        .and_then(|()| publish(recipe, upserts, &actions, removals, side, store));

    if let Err(error) = outcome {
        return Err(error.with_recovery_issues(journal.undo()));
    }

    for retirement in &retirements {
        if retirement.reason == RetirementReason::Removed {
            report.removed.push(retirement.logical_path.clone());
        }
    }

    // The recipe is on disk and agrees with `mods/`. What is left in the
    // retired directory is scratch the game cannot see, but a failure is part
    // of the typed outcome and stays visible to startup reconciliation.
    report.recovery_issues = journal.discard();

    info!(
        installed = report.installed.len(),
        replaced = report.replaced.len(),
        removed = report.removed.len(),
        repaired = report.repaired.len(),
        unchanged = report.unchanged.len(),
        recorded = report.recorded.len(),
        modified = report.modified.len(),
        "mutated managed mods"
    );
    Ok(report)
}

/// Decides what to do with every requested mod, and refuses the whole mutation
/// if any of it is unsafe.
async fn plan(
    recipe: &PackRecipe,
    upserts: &[PackFileRecipe],
    removals: &[ManagedProject],
    side: PackSide,
    game_dir: &Path,
) -> PackResult<Vec<Action>> {
    // Two requested mods claiming one file name is a plan that cannot be
    // carried out in any order.
    let mut claimed: BTreeSet<&Path> = BTreeSet::new();
    for file in upserts {
        if !claimed.insert(file.logical_path.as_path()) {
            return Err(PackError::DestinationCollision {
                name: file.display_name(),
            });
        }
    }

    require_one_entry_per_project(
        recipe,
        upserts,
        removals
            .iter()
            .map(|removal| (removal.provider, removal.project_id.as_str())),
    )?;

    // A removal must name something Ephervex installed. Anything else is a jar
    // the user owns, and no provider operation gets to delete one.
    //
    // Identity is not ownership: a hand-placed jar a catalog recognized carries
    // a provider and a project, so it answers `managed_project` exactly as an
    // installed mod does. Origin is the thing that says whose file it is, so
    // origin is what is checked.
    let mut destinations = ManagedDestinations::new(game_dir);
    for removal in removals {
        match recipe.managed_project(removal.provider, &removal.project_id) {
            None => {
                return Err(PackError::ManagedIdentityMissing {
                    path: removal.project_id.clone(),
                });
            }
            Some(entry) if entry.origin == ModOrigin::HandPlaced => {
                return Err(PackError::HandPlacedNotMutable {
                    path: entry.logical_path.clone(),
                });
            }
            Some(entry) => {
                destinations.retirable(&entry.logical_path)?;
            }
        }
    }

    // The paths this mutation frees. A new jar may land on one of them.
    let freed: BTreeSet<PathBuf> = freed_paths(recipe, upserts, removals);

    let mut actions = Vec::with_capacity(upserts.len());
    for file in upserts {
        actions.push(action_for(recipe, file, side, game_dir, &mut destinations, &freed).await?);
    }
    Ok(actions)
}

/// Every path this mutation vacates, whether by replacing a mod or removing it.
fn freed_paths(
    recipe: &PackRecipe,
    upserts: &[PackFileRecipe],
    removals: &[ManagedProject],
) -> BTreeSet<PathBuf> {
    let mut freed = BTreeSet::new();
    for removal in removals {
        if let Some(entry) = recipe.managed_project(removal.provider, &removal.project_id) {
            freed.insert(entry.logical_path.clone());
        }
    }
    for file in upserts {
        let Some((provider, project_id)) = project_of(file) else {
            continue;
        };
        if let Some(entry) = recipe.managed_project(provider, project_id) {
            freed.insert(entry.logical_path.clone());
        }
    }
    freed
}

/// What to do with one requested mod.
async fn action_for(
    recipe: &PackRecipe,
    file: &PackFileRecipe,
    side: PackSide,
    game_dir: &Path,
    destinations: &mut ManagedDestinations<'_>,
    freed: &BTreeSet<PathBuf>,
) -> PackResult<Action> {
    let Some((provider, project_id)) = project_of(file) else {
        return Err(PackError::ManagedIdentityMissing {
            path: file.display_name(),
        });
    };
    destinations.resolve(&file.logical_path)?;

    let existing = recipe.managed_project(provider, project_id);

    match existing {
        Some(entry) if version_of(entry) == version_of(file) => {
            // The same mod at the same version. Present with the recorded bytes
            // is a no-op - including present but turned off, which is a state
            // to leave alone rather than a jar to put back. Absent is a repair,
            // which restores the recorded jar. Present with other bytes is left
            // alone and reported. A record that does not take this side is
            // brought in line with the request, so a jar is never written
            // while the recipe still says the instance does not have it.
            destinations.resolve(&entry.logical_path)?;
            Ok(match recorded_jar(game_dir, entry).await {
                RecordedJar::Intact if recording_side(entry, file, side).is_some() => {
                    Action::Record
                }
                RecordedJar::Intact => Action::Unchanged,
                RecordedJar::Missing => {
                    destinations.writable(&entry.logical_path)?;
                    Action::Repair
                }
                RecordedJar::Altered => Action::Altered,
            })
        }
        Some(entry) if entry.origin == ModOrigin::HandPlaced => {
            // Installing this project would retire the user's own jar as
            // though it were a previous release the launcher had put there.
            // Adoption recorded what that file is; it did not hand it over.
            Err(PackError::HandPlacedNotMutable {
                path: entry.logical_path.clone(),
            })
        }
        Some(entry) => {
            let entry = entry.clone();
            destinations.retirable(&entry.logical_path)?;
            let destination = destinations.writable(&file.logical_path)?;
            // The new jar's own destination has to be free by the time it is
            // written. It is free if nothing is there, or if this mutation is
            // about to vacate it.
            if let Some(path) = occupant(&destination)
                && !freed.contains(&file.logical_path)
            {
                return Err(occupied(recipe, &file.logical_path, path));
            }
            Ok(Action::Replace(Box::new(entry)))
        }
        None => {
            let destination = destinations.writable(&file.logical_path)?;
            if let Some(path) = occupant(&destination)
                && !freed.contains(&file.logical_path)
            {
                // A jar Ephervex did not put there, almost always the local
                // build the instance exists to test, or another recorded mod.
                return Err(occupied(recipe, &file.logical_path, path));
            }
            Ok(Action::Install)
        }
    }
}

/// One file to move out of the game directory before the recipe is published.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Retirement {
    logical_path: PathBuf,
    /// Whether the outgoing jar is at its disabled spelling, so that the file
    /// is found where it actually is and its replacement lands the same way
    /// round.
    disabled: bool,
    /// What the report should call this: a removal, or the old half of a
    /// replacement.
    reason: RetirementReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetirementReason {
    Removed,
    Replaced {
        new_path: PathBuf,
        previous_version: Option<String>,
        version: Option<String>,
    },
}

/// The files this mutation takes out of the game directory.
fn retirements(
    recipe: &PackRecipe,
    upserts: &[PackFileRecipe],
    actions: &[Action],
    removals: &[ManagedProject],
    game_dir: &Path,
) -> Vec<Retirement> {
    let mut retirements = Vec::new();
    for (file, action) in upserts.iter().zip(actions) {
        if let Action::Replace(previous) = action {
            retirements.push(Retirement {
                logical_path: previous.logical_path.clone(),
                disabled: is_disabled(game_dir, &previous.logical_path),
                reason: RetirementReason::Replaced {
                    new_path: file.logical_path.clone(),
                    previous_version: version_of(previous).map(ToOwned::to_owned),
                    version: version_of(file).map(ToOwned::to_owned),
                },
            });
        }
    }
    for removal in removals {
        if let Some(entry) = recipe.managed_project(removal.provider, &removal.project_id) {
            retirements.push(Retirement {
                logical_path: entry.logical_path.clone(),
                disabled: is_disabled(game_dir, &entry.logical_path),
                reason: RetirementReason::Removed,
            });
        }
    }
    retirements
}

/// Everything this mutation has done that can still be undone.
#[derive(Debug, Default)]
struct Journal {
    /// Files written into the game directory by this mutation.
    created: Vec<PathBuf>,
    /// Files moved out of the game directory by this mutation.
    retired: Vec<Moved>,
}

#[derive(Debug, Clone)]
struct Moved {
    from: PathBuf,
    to: PathBuf,
}

impl Journal {
    /// Puts the instance back the way it was.
    ///
    /// New files go first and retired ones come back second, which is the only
    /// order that works when a replacement reused its predecessor's file name:
    /// restoring first would land on a path the new jar still occupies.
    fn undo(&mut self) -> Vec<MutationRecoveryIssue> {
        let mut issues = Vec::new();
        for path in self.created.drain(..) {
            if let Err(error) = std::fs::remove_file(&path) {
                warn!(
                    path = %path.display(),
                    %error,
                    "could not remove a file left behind by a failed mutation"
                );
                issues.push(MutationRecoveryIssue {
                    action: MutationRecoveryAction::RemoveCreated,
                    path,
                    error_kind: error.kind(),
                });
            }
        }
        for moved in self.retired.drain(..) {
            if let Err(error) = std::fs::rename(&moved.to, &moved.from) {
                warn!(
                    path = %moved.from.display(),
                    %error,
                    "could not restore a mod set aside by a failed mutation"
                );
                issues.push(MutationRecoveryIssue {
                    action: MutationRecoveryAction::RestoreRetired,
                    path: moved.to,
                    error_kind: error.kind(),
                });
            }
        }
        issues
    }

    /// Drops the retired files for good, after the recipe has been published.
    fn discard(&self) -> Vec<MutationRecoveryIssue> {
        let mut issues = Vec::new();
        for moved in &self.retired {
            if let Err(error) = std::fs::remove_file(&moved.to) {
                warn!(
                    path = %moved.to.display(),
                    %error,
                    "a replaced mod is still in the instance's retired directory"
                );
                issues.push(MutationRecoveryIssue {
                    action: MutationRecoveryAction::RemoveRetired,
                    path: moved.to.clone(),
                    error_kind: error.kind(),
                });
            }
        }
        issues
    }
}

/// Writes the new jars, recording each one so it can be removed again.
fn write_new_files(
    pending: &[(&PackFileRecipe, &Action)],
    fetched: &[ephervex_download::DownloadOutcome],
    game_dir: &Path,
    stay_disabled: &BTreeSet<PathBuf>,
    cancel: &CancellationToken,
    journal: &mut Journal,
    report: &mut ManagedMutationReport,
) -> PackResult<()> {
    for ((file, action), outcome) in pending.iter().zip(fetched) {
        if cancel.is_cancelled() {
            return Err(PackError::Canceled);
        }
        let enabled = game_dir.join(&file.logical_path);
        // A mod the user turned off stays off across a version change. The
        // recipe records the enabled name either way, so only the file moves.
        let destination = if stay_disabled.contains(&file.logical_path) {
            disabled_path(&enabled)
        } else {
            enabled
        };
        copy_into_instance(&outcome.path, &destination)?;
        journal.created.push(destination);

        if outcome.source == ephervex_download::ArtifactSource::AlreadyCached {
            report.reused_from_cache += 1;
        }
        report.bytes = report.bytes.saturating_add(outcome.bytes);
        match action {
            Action::Repair => report.repaired.push(file.logical_path.clone()),
            Action::Replace(previous) => report.replaced.push(ReplacedMod {
                logical_path: file.logical_path.clone(),
                previous_path: previous.logical_path.clone(),
                previous_version: version_of(previous).map(ToOwned::to_owned),
                version: version_of(file).map(ToOwned::to_owned),
            }),
            Action::Install => report.installed.push(file.logical_path.clone()),
            Action::Unchanged | Action::Altered | Action::Record => {}
        }
    }
    Ok(())
}

/// Moves the outgoing jars into launcher-owned scratch.
fn retire(
    retirements: &[Retirement],
    game_dir: &Path,
    retired_dir: &Path,
    journal: &mut Journal,
) -> PackResult<()> {
    if retirements.is_empty() {
        return Ok(());
    }

    std::fs::create_dir_all(retired_dir).map_err(|source| PackError::Unwritable {
        path: retired_dir.to_path_buf(),
        source,
    })?;

    for retirement in retirements {
        let enabled = game_dir.join(&retirement.logical_path);
        let from = if retirement.disabled {
            disabled_path(&enabled)
        } else {
            enabled
        };
        if !from.exists() {
            // The jar is already gone - deleted by hand while bisecting, most
            // likely. Removing the record is still the operation, and there is
            // nothing here to move.
            continue;
        }
        let to = unique_retired_path(retired_dir, &retirement.logical_path);
        std::fs::rename(&from, &to).map_err(|source| PackError::Unwritable {
            path: to.clone(),
            source,
        })?;
        journal.retired.push(Moved { from, to });
    }
    Ok(())
}

/// A free name inside the retired directory for a file about to be moved there.
///
/// Two mutations can retire files with the same name, and one instance can have
/// leftovers from an earlier one, so the name is made unique rather than
/// assumed to be. The candidate is built from the final component of `path`
/// only, so an untrusted directory part never reaches the retired directory.
#[must_use]
pub fn unique_retired_path(retired_dir: &Path, path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| PathBuf::from("retired"), PathBuf::from);
    let candidate = retired_dir.join(&name);
    if !candidate.exists() {
        return candidate;
    }
    for attempt in 1u32..u32::MAX {
        let candidate = retired_dir.join(format!("{}.{attempt}", name.display()));
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

/// Builds the new desired state and writes it.
///
/// Written last and only once every jar is where the new recipe says it is, so
/// a recipe on disk never names a mod that is not there.
fn publish(
    recipe: &mut PackRecipe,
    upserts: &[PackFileRecipe],
    actions: &[Action],
    removals: &[ManagedProject],
    side: PackSide,
    store: &SourceStore,
) -> PackResult<()> {
    let mut updated = recipe.clone();

    let dropped: BTreeSet<(ModProvider, &str)> = removals
        .iter()
        .map(|removal| (removal.provider, removal.project_id.as_str()))
        .collect();
    updated.files.retain(|file| {
        project_of(file)
            .is_none_or(|(provider, project_id)| !dropped.contains(&(provider, project_id)))
    });

    // Replacements are applied in place so that the entry keeps its position in
    // the recipe and, more importantly, so that one project can never end up
    // with two entries.
    let mut additions: Vec<PackFileRecipe> = Vec::new();
    for (file, action) in upserts.iter().zip(actions) {
        match action {
            Action::Install => additions.push(file.clone()),
            Action::Replace(_) => {
                let Some((provider, project_id)) = project_of(file) else {
                    return Err(PackError::ManagedIdentityMissing {
                        path: file.display_name(),
                    });
                };
                let position = updated
                    .files
                    .iter()
                    .position(|existing| project_of(existing) == Some((provider, project_id)))
                    .ok_or_else(|| PackError::ManagedIdentityMissing {
                        path: file.display_name(),
                    })?;
                updated.files[position] = replacement_for(&updated.files[position], file);
            }
            // Putting a jar back restores what is recorded, except that a
            // record which did not take this side takes it now.
            Action::Repair | Action::Record => {
                let Some((provider, project_id)) = project_of(file) else {
                    return Err(PackError::ManagedIdentityMissing {
                        path: file.display_name(),
                    });
                };
                if let Some(entry) = updated
                    .files
                    .iter_mut()
                    .find(|existing| project_of(existing) == Some((provider, project_id)))
                    && let Some(recorded) = recording_side(entry, file, side)
                {
                    *entry = recorded;
                }
            }
            // An unchanged mod is already exactly what is asked for. A modified
            // one is left as it is, record included.
            Action::Unchanged | Action::Altered => {}
        }
    }
    for addition in additions {
        updated.record_managed(addition)?;
    }

    // Written before the in-memory recipe is replaced, so a caller that carries
    // on after a failed write is not holding a recipe that disagrees with disk.
    store.write_recipe(&updated)?;
    *recipe = updated;
    Ok(())
}

/// Whether a recorded mod's jar is currently turned off.
fn is_disabled(game_dir: &Path, logical_path: &Path) -> bool {
    !game_dir.join(logical_path).is_file() && disabled_path(&game_dir.join(logical_path)).is_file()
}

/// The provider and project an entry names, if it names both.
fn project_of(file: &PackFileRecipe) -> Option<(ModProvider, &str)> {
    file.identity
        .as_ref()
        .and_then(ProviderFileIdentity::project)
}

/// The exact version an entry names, if it names one.
fn version_of(file: &PackFileRecipe) -> Option<&str> {
    file.identity
        .as_ref()
        .and_then(|identity| identity.version_id.as_deref())
}

/// The entry that stands in for `current` when the request names another version.
fn replacement_for(current: &PackFileRecipe, request: &PackFileRecipe) -> PackFileRecipe {
    // The origin is a fact about why the instance has this mod and survives a
    // version change. Rewriting it would turn a dependency into a root, or a
    // pack's own mod into a manual one, on the strength of an unrelated
    // operation.
    let mut replacement = request.clone();
    replacement.origin = current.origin;
    // A pack-owned entry's first move away from what the pack declared is the
    // only chance to remember what that was; every later move must not
    // overwrite it.
    if current.origin == ModOrigin::PackDeclared {
        replacement.original_identity = current
            .original_identity
            .clone()
            .or_else(|| current.identity.clone());
    }
    replacement
}

/// The mods an instance would have if this mutation succeeded, without doing it.
///
/// Exists so a caller can show the outcome before committing to it, using the
/// same rules the mutation itself applies rather than a second description of
/// them.
///
/// The disk is not consulted, so a recorded jar that holds other bytes (which
/// the mutation leaves, record included) is previewed as though it were intact.
#[must_use]
pub fn projected(
    recipe: &PackRecipe,
    upserts: &[PackFileRecipe],
    removals: &[ManagedProject],
    side: PackSide,
) -> PackRecipe {
    let mut projected = recipe.clone();
    let dropped: BTreeSet<(ModProvider, String)> = removals
        .iter()
        .map(|removal| (removal.provider, removal.project_id.clone()))
        .collect();
    projected.files.retain(|file| {
        project_of(file).is_none_or(|(provider, project_id)| {
            !dropped.contains(&(provider, project_id.to_owned()))
        })
    });

    let mut positions: BTreeMap<(ModProvider, String), usize> = BTreeMap::new();
    for (index, file) in projected.files.iter().enumerate() {
        if let Some((provider, project_id)) = project_of(file) {
            positions.insert((provider, project_id.to_owned()), index);
        }
    }
    for file in upserts {
        let Some((provider, project_id)) = project_of(file) else {
            continue;
        };
        match positions.get(&(provider, project_id.to_owned())) {
            // The recorded version is what the mutation keeps, whatever path or
            // sides the request spelled. Only the instance's own side is
            // taken from the request, and only when the record did not take it.
            Some(&index) if version_of(&projected.files[index]) == version_of(file) => {
                if let Some(recorded) = recording_side(&projected.files[index], file, side) {
                    projected.files[index] = recorded;
                }
            }
            Some(&index) => {
                projected.files[index] = replacement_for(&projected.files[index], file);
            }
            None => projected.files.push(file.clone()),
        }
    }
    projected
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn a_retired_name_never_reuses_an_existing_leftover_or_follows_a_directory_part() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let retired = temporary.path();

        let first = unique_retired_path(retired, Path::new("mods/a.jar"));
        assert_eq!(first, retired.join("a.jar"));
        std::fs::write(&first, b"first").expect("leftover");

        let second = unique_retired_path(retired, Path::new("elsewhere/../a.jar"));
        assert_eq!(second, retired.join("a.jar.1"));
        std::fs::write(&second, b"second").expect("leftover");

        assert_eq!(
            unique_retired_path(retired, Path::new("a.jar")),
            retired.join("a.jar.2")
        );
    }

    #[test]
    fn rollback_failures_are_returned_as_typed_recovery_issues() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let created_directory = temporary.path().join("created-as-directory");
        std::fs::create_dir(&created_directory).expect("created directory");
        let absent_retired = temporary.path().join("retired-but-absent.jar");
        let restore_target = temporary.path().join("mods/original.jar");
        let mut journal = Journal {
            created: vec![created_directory.clone()],
            retired: vec![Moved {
                from: restore_target,
                to: absent_retired.clone(),
            }],
        };

        let issues = journal.undo();

        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].action, MutationRecoveryAction::RemoveCreated);
        assert_eq!(issues[0].path, created_directory);
        assert_eq!(issues[1].action, MutationRecoveryAction::RestoreRetired);
        assert_eq!(issues[1].path, absent_retired);
    }

    #[test]
    fn post_commit_cleanup_failures_are_part_of_the_reportable_outcome() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let retired_directory = temporary.path().join("retired-as-directory");
        std::fs::create_dir(&retired_directory).expect("retired directory");
        let journal = Journal {
            created: Vec::new(),
            retired: vec![Moved {
                from: temporary.path().join("mods/original.jar"),
                to: retired_directory.clone(),
            }],
        };

        let issues = journal.discard();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].action, MutationRecoveryAction::RemoveRetired);
        assert_eq!(issues[0].path, retired_directory);
    }
}
