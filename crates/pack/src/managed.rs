//! Installing a mod chosen through a provider, and recording it durably.
//!
//! # Why this is not just "download and copy"
//!
//! `mods/` belongs to the user. It holds jars Ephervex installed, jars the user
//! dropped in from a forum thread, and — for the developer this launcher is
//! built for — the local build being debugged. An installer that writes into
//! that directory optimistically can destroy the one file the instance existed
//! to test.
//!
//! So the whole destination set is validated before anything is written:
//!
//! * two planned mods that would land on the same file name are a
//!   [`PackError::DestinationCollision`], because installing them in sequence
//!   would silently leave one of them out;
//! * a destination that already exists and is *not* the managed entry being
//!   reused is a [`PackError::DestinationOccupied`]. It is never overwritten,
//!   never deleted, and never adopted into the recipe as though Ephervex had
//!   put it there.
//!
//! # An install either happens or leaves nothing new behind
//!
//! Every jar this operation creates is tracked, and any failure after the first
//! copy removes exactly those — including a failure to write the recipe, which
//! is as much an installation failure as a download that would not verify: an
//! instance whose mods are not recorded cannot rebuild its server. Rollback
//! touches only files this operation created. Anything that was already there,
//! including a file at a path the operation skipped, is left exactly as it was;
//! restoring by guesswork over pre-existing files is how an installer eats
//! somebody's work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, DownloadManager, OperationReporter};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::error::{PackError, PackResult};
use crate::install::{copy_into_instance, fetch_all};
use crate::recipe::{PackFileRecipe, PackRecipe};
use crate::store::SourceStore;

/// The directory managed mods are installed into, relative to the game
/// directory.
pub const MODS_DIR: &str = "mods";

/// What installing a set of managed mods produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedModInstallation {
    /// Files this operation created, as logical paths.
    pub installed: Vec<PathBuf>,
    /// Files already present and already recorded at the same version, so
    /// nothing was written for them.
    pub already_present: Vec<PathBuf>,
    /// Recorded mods whose jar had gone missing and was restored from the
    /// verified cache. Repair, not an update: the recipe entry is unchanged.
    pub repaired: Vec<PathBuf>,
    /// How many artifacts came from the verified shared cache rather than the
    /// network.
    pub reused_from_cache: usize,
    pub bytes: u64,
}

impl ManagedModInstallation {
    /// Whether this operation changed the instance at all.
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        !self.installed.is_empty() || !self.repaired.is_empty()
    }
}

/// One managed-mod installation.
#[derive(Debug)]
pub struct ManagedModInstall<'a> {
    /// The instance's desired state, updated in place on success.
    pub recipe: &'a mut PackRecipe,
    /// The files to install, each already carrying its provider identity,
    /// hashes, and download addresses.
    pub additions: &'a [PackFileRecipe],
    /// The directory the game runs in. Logical paths are relative to it.
    pub game_dir: &'a Path,
    /// Where the updated recipe is written.
    pub store: &'a SourceStore,
    pub manager: &'a DownloadManager,
    pub cache: &'a CacheLayout,
    pub reporter: &'a OperationReporter,
    pub cancel: &'a CancellationToken,
}

/// What the pre-flight decided to do with one planned file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Nothing is recorded here and nothing is on disk: install it.
    Install,
    /// The recipe already records this exact mod and the jar is present.
    AlreadyPresent,
    /// The recipe already records this exact mod but the jar has gone missing.
    Repair,
}

/// Installs mods and records them in the recipe.
///
/// The recipe is written last and only once every file is in place, so a
/// recipe on disk never names a mod that is not there. The reverse ordering —
/// recording first — would make "Create Server" build something that never
/// existed.
#[instrument(skip_all, fields(game_dir = %request.game_dir.display(), mods = request.additions.len()))]
pub async fn install_managed(request: ManagedModInstall<'_>) -> PackResult<ManagedModInstallation> {
    let ManagedModInstall {
        recipe,
        additions,
        game_dir,
        store,
        manager,
        cache,
        reporter,
        cancel,
    } = request;

    // Nothing below this line runs until every destination is known to be safe.
    let dispositions = plan_destinations(recipe, additions, game_dir)?;

    let mut report = ManagedModInstallation::default();
    let mut pending: Vec<(&PackFileRecipe, Disposition)> = Vec::new();
    for (file, disposition) in additions.iter().zip(&dispositions) {
        match disposition {
            Disposition::AlreadyPresent => report.already_present.push(file.logical_path.clone()),
            other => pending.push((file, *other)),
        }
    }
    if pending.is_empty() {
        info!(
            already_present = report.already_present.len(),
            "every requested mod was already installed at the requested version"
        );
        return Ok(report);
    }

    let wanted: Vec<&PackFileRecipe> = pending.iter().map(|(file, _)| *file).collect();
    let outcomes = fetch_all(&wanted, cache, manager, reporter, cancel).await?;

    // Everything from here on can leave a file behind, so it is undone on
    // failure. `created` is the exact set this operation wrote — never a guess
    // derived from the plan, which would include files that were already there.
    let mut created: Vec<PathBuf> = Vec::with_capacity(pending.len());
    let outcome = copy_all(
        &pending,
        &outcomes,
        game_dir,
        cancel,
        &mut created,
        &mut report,
    )
    .and_then(|()| commit(recipe, additions, &dispositions, store));

    if let Err(error) = outcome {
        roll_back(&created);
        return Err(error);
    }

    info!(
        installed = report.installed.len(),
        repaired = report.repaired.len(),
        already_present = report.already_present.len(),
        reused = report.reused_from_cache,
        "installed managed mods"
    );
    Ok(report)
}

/// Decides what to do with every planned file, and refuses the whole operation
/// if any destination is unsafe.
///
/// Runs to completion before anything is fetched or written: a collision found
/// halfway through a copy loop would already have overwritten something.
fn plan_destinations(
    recipe: &PackRecipe,
    additions: &[PackFileRecipe],
    game_dir: &Path,
) -> PackResult<Vec<Disposition>> {
    // Two planned mods claiming one file name is a plan that cannot be
    // installed, whichever order it is attempted in.
    let mut claimed: BTreeMap<&Path, usize> = BTreeMap::new();
    for file in additions {
        if claimed.insert(file.logical_path.as_path(), 0).is_some() {
            return Err(PackError::DestinationCollision {
                name: file.display_name(),
            });
        }
    }

    additions
        .iter()
        .map(|file| disposition_for(recipe, file, game_dir))
        .collect()
}

/// What to do with one planned file.
fn disposition_for(
    recipe: &PackRecipe,
    file: &PackFileRecipe,
    game_dir: &Path,
) -> PackResult<Disposition> {
    let destination = game_dir.join(&file.logical_path);
    let recorded = recorded_identically(recipe, file);

    match (destination.exists(), recorded) {
        // Nothing there, nothing recorded: an ordinary install.
        (false, false) => Ok(Disposition::Install),
        // Recorded, but the jar is gone. Restoring it from the verified cache
        // is repair; the recipe entry does not change.
        (false, true) => Ok(Disposition::Repair),
        // Recorded and present: an exactly repeated install, which does
        // nothing rather than rewriting a byte-identical file.
        (true, true) => Ok(Disposition::AlreadyPresent),
        // Present, and Ephervex did not put it there. This is the local build
        // being debugged, or the jar from the bug report. It is not ours to
        // overwrite, and it is not ours to claim.
        (true, false) => Err(PackError::DestinationOccupied { path: destination }),
    }
}

/// Whether the recipe already records this exact mod — same project *and* same
/// version — at this exact path.
///
/// A different version at the same path is deliberately not "recorded": it is
/// refused earlier, during planning, because changing it is an update.
fn recorded_identically(recipe: &PackRecipe, file: &PackFileRecipe) -> bool {
    let Some((provider, project_id)) = file
        .identity
        .as_ref()
        .and_then(crate::recipe::ProviderFileIdentity::project)
    else {
        return false;
    };
    recipe
        .managed_project(provider, project_id)
        .is_some_and(|existing| {
            existing.logical_path == file.logical_path
                && existing
                    .identity
                    .as_ref()
                    .and_then(|it| it.version_id.as_deref())
                    == file
                        .identity
                        .as_ref()
                        .and_then(|it| it.version_id.as_deref())
        })
}

/// Copies each fetched artifact into the instance, recording what was created.
fn copy_all(
    pending: &[(&PackFileRecipe, Disposition)],
    outcomes: &[ephervex_download::DownloadOutcome],
    game_dir: &Path,
    cancel: &CancellationToken,
    created: &mut Vec<PathBuf>,
    report: &mut ManagedModInstallation,
) -> PackResult<()> {
    for ((file, disposition), fetched) in pending.iter().zip(outcomes) {
        if cancel.is_cancelled() {
            return Err(PackError::Canceled);
        }
        let destination = game_dir.join(&file.logical_path);
        copy_into_instance(&fetched.path, &destination)?;
        created.push(destination);

        if fetched.source == ephervex_download::ArtifactSource::AlreadyCached {
            report.reused_from_cache += 1;
        }
        report.bytes = report.bytes.saturating_add(fetched.bytes);
        if *disposition == Disposition::Repair {
            report.repaired.push(file.logical_path.clone());
        } else {
            report.installed.push(file.logical_path.clone());
        }
    }
    Ok(())
}

/// Records the new mods and writes the recipe.
///
/// A repair records nothing: the entry it would add is already there.
fn commit(
    recipe: &mut PackRecipe,
    additions: &[PackFileRecipe],
    dispositions: &[Disposition],
    store: &SourceStore,
) -> PackResult<()> {
    let mut updated = recipe.clone();
    for (file, disposition) in additions.iter().zip(dispositions) {
        if *disposition == Disposition::Install {
            updated.record_managed(file.clone())?;
        }
    }

    // Written before the in-memory recipe is replaced, so a caller that
    // continues after a failed write is not holding a recipe that disagrees
    // with the disk.
    store.write_recipe(&updated)?;
    *recipe = updated;
    Ok(())
}

/// Removes the files this operation created, and nothing else.
fn roll_back(created: &[PathBuf]) {
    for path in created {
        if let Err(error) = std::fs::remove_file(path) {
            // Reported rather than propagated: the failure that triggered the
            // rollback is the one worth returning, and a leftover jar is a
            // smaller problem than replacing that error with this one.
            warn!(
                path = %path.display(),
                %error,
                "could not remove a file left behind by a failed mod installation"
            );
        }
    }
}
