//! Installing a mod chosen through a provider, and recording it durably.
//!
//! # Why this is not just "download and copy"
//!
//! `mods/` belongs to the user. It holds jars Ephervex installed, jars the user
//! dropped in from a forum thread, and - for the developer this launcher is
//! built for - the local build being debugged. An installer that writes into
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
//! copy removes exactly those - including a failure to write the recipe, which
//! is as much an installation failure as a download that would not verify: an
//! instance whose mods are not recorded cannot rebuild its server. Rollback
//! touches only files this operation created. Anything that was already there,
//! including a file at a path the operation skipped, is left exactly as it was;
//! restoring by guesswork over pre-existing files is how an installer eats
//! somebody's work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ephervex_download::stored_file_matches;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::error::{PackError, PackResult};
use crate::install::{ArtifactFetch, copy_into_instance, fetch_all};
use crate::path::{DestinationLinks, contained_join, is_reserved, safe_relative_path};
use crate::recipe::{PackFileRecipe, PackRecipe, ProviderFileIdentity};
use crate::store::SourceStore;

/// The directory managed mods are installed into, relative to the game
/// directory.
pub const MODS_DIR: &str = "mods";

/// The suffix a mod carries while it is installed but not loaded.
///
/// Both FML and Fabric discover mods by scanning for `.jar`, so a file with any
/// other extension is present on disk and invisible to the loader. `.disabled`
/// is the spelling every other launcher uses, which is what makes a directory
/// disabled by Ephervex readable by one of them and the other way round.
pub const DISABLED_SUFFIX: &str = ".disabled";

/// Where a jar sits when it is disabled.
///
/// Appended to the whole file name rather than replacing the extension, so the
/// original name is recovered by removing the suffix and nothing has to guess
/// what it used to be.
#[must_use]
pub fn disabled_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(DISABLED_SUFFIX);
    std::path::PathBuf::from(name)
}

/// The directory a pack-declared resource pack is installed into, relative to
/// the game directory.
///
/// Never a managed mod's own directory: a provider-declared resource pack is
/// pack-owned desired state, routed here instead of [`MODS_DIR`] precisely so
/// it is never read back as one.
pub const RESOURCEPACKS_DIR: &str = "resourcepacks";

/// The directory a pack-declared shader pack is installed into, relative to
/// the game directory, on the same terms as [`RESOURCEPACKS_DIR`].
pub const SHADERPACKS_DIR: &str = "shaderpacks";

/// What installing a set of managed mods produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedModInstallation {
    /// Files this operation created, as logical paths.
    pub installed: Vec<PathBuf>,
    /// Files already present and already recorded at the same version, so
    /// nothing was written for them. A mod turned off still counts as present,
    /// and the path is the one the recipe records, which is not necessarily the
    /// one the request named.
    pub already_present: Vec<PathBuf>,
    /// Recorded files whose bytes are not the recorded ones. They were left
    /// exactly as they are: a changed jar is very often the local build the
    /// instance exists to test, and a reinstall is not entitled to overwrite
    /// it.
    pub modified: Vec<PathBuf>,
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
    pub fetch: ArtifactFetch<'a>,
}

/// Checks managed mod destinations before anything is written to or moved from
/// them.
///
/// A logical path is public, deserializable input, so it gets the rules an
/// ordinary pack file does: relative, free of `..`, not launcher-owned. A
/// destination at or beneath a link on disk is also refused, because the write
/// would land wherever the link points. One value per operation shares the
/// link lookups.
#[derive(Debug)]
pub(crate) struct ManagedDestinations<'a> {
    game_dir: &'a Path,
    links: DestinationLinks<'a>,
}

impl<'a> ManagedDestinations<'a> {
    pub(crate) const fn new(game_dir: &'a Path) -> Self {
        Self {
            game_dir,
            links: DestinationLinks::new(game_dir),
        }
    }

    /// The enabled path of a logical destination, once the logical path is a
    /// safe, unreserved path relative to the game directory.
    pub(crate) fn resolve(&self, logical: &Path) -> PackResult<PathBuf> {
        let unsafe_path = || PackError::ManagedPathUnsafe {
            path: logical.to_path_buf(),
        };
        let safe = logical
            .to_str()
            .and_then(|raw| safe_relative_path(raw.as_bytes(), 0))
            .filter(|safe| !safe.as_os_str().is_empty() && safe.as_os_str() == logical.as_os_str())
            .ok_or_else(unsafe_path)?;
        if is_reserved(&safe) {
            return Err(unsafe_path());
        }
        contained_join(self.game_dir, &safe).ok_or_else(unsafe_path)
    }

    /// The enabled path of a destination a file is about to be written to,
    /// refused if either spelling is a link or sits beneath one.
    pub(crate) fn writable(&mut self, logical: &Path) -> PackResult<PathBuf> {
        let enabled = self.resolve(logical)?;
        if self.links.walks_a_link(logical) {
            return Err(PackError::ManagedDestinationLinked { path: enabled });
        }
        let disabled = disabled_path(logical);
        if self.links.walks_a_link(&disabled) {
            return Err(PackError::ManagedDestinationLinked {
                path: self.game_dir.join(disabled),
            });
        }
        Ok(enabled)
    }

    /// The enabled path of a recorded file about to be moved out of the game
    /// directory, refused if a directory above it is a link.
    ///
    /// A link at the file itself is not an obstacle: moving it moves the link,
    /// not whatever it points at.
    pub(crate) fn retirable(&mut self, logical: &Path) -> PackResult<PathBuf> {
        let enabled = self.resolve(logical)?;
        if logical
            .parent()
            .is_some_and(|parent| self.links.walks_a_link(parent))
        {
            return Err(PackError::ManagedDestinationLinked { path: enabled });
        }
        Ok(enabled)
    }
}

/// What stands at a path, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    Absent,
    File,
    /// A link, a directory, or something that could not be inspected.
    Other,
}

fn standing(path: &Path) -> Standing {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Standing::File,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Standing::Absent,
        Ok(_) | Err(_) => Standing::Other,
    }
}

/// Whatever already stands where a mod would go, under either spelling.
///
/// A link counts, dangling or not, and so does anything that could not be
/// inspected: a destination is free only when it is known to be absent.
pub(crate) fn occupant(enabled: &Path) -> Option<PathBuf> {
    let disabled = disabled_path(enabled);
    [enabled, disabled.as_path()]
        .into_iter()
        .find(|path| standing(path) != Standing::Absent)
        .map(Path::to_path_buf)
}

/// What the pre-flight decided to do with one planned file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Nothing is recorded here and nothing is on disk: install it.
    Install,
    /// The recipe already records this exact mod and the jar is present.
    AlreadyPresent,
    /// The recipe already records this exact mod but the jar has gone missing,
    /// under either spelling.
    Repair,
    /// The recipe already records this exact mod and a jar is there, but it is
    /// not the recorded artifact. It is neither rewritten nor reported as
    /// installed.
    Altered,
}

/// Installs mods and records them in the recipe.
///
/// The recipe is written last and only once every file is in place, so a
/// recipe on disk never names a mod that is not there. The reverse ordering -
/// recording first - would make "Create Server" build something that never
/// existed.
#[instrument(skip_all, fields(game_dir = %request.game_dir.display(), mods = request.additions.len()))]
pub async fn install_managed(request: ManagedModInstall<'_>) -> PackResult<ManagedModInstallation> {
    let ManagedModInstall {
        recipe,
        additions,
        game_dir,
        store,
        fetch,
    } = request;
    let cancel = fetch.cancel;
    let settled = settle_recorded_paths(recipe, additions);
    let additions = settled.as_slice();

    // Nothing below this line runs until every destination is known to be safe.
    let dispositions = plan_destinations(recipe, additions, game_dir).await?;

    let mut report = ManagedModInstallation::default();
    let mut pending: Vec<(&PackFileRecipe, Disposition)> = Vec::new();
    for (file, disposition) in additions.iter().zip(&dispositions) {
        match disposition {
            Disposition::AlreadyPresent => report.already_present.push(file.logical_path.clone()),
            Disposition::Altered => report.modified.push(file.logical_path.clone()),
            other => pending.push((file, *other)),
        }
    }
    if pending.is_empty() {
        info!(
            already_present = report.already_present.len(),
            modified = report.modified.len(),
            "nothing needed installing at the requested versions"
        );
        return Ok(report);
    }

    let wanted: Vec<&PackFileRecipe> = pending.iter().map(|(file, _)| *file).collect();
    let outcomes = fetch_all(&wanted, fetch).await?;

    // Everything from here on can leave a file behind, so it is undone on
    // failure. `created` is the exact set this operation wrote - never a guess
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
async fn plan_destinations(
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

    let mut destinations = ManagedDestinations::new(game_dir);
    let mut dispositions = Vec::with_capacity(additions.len());
    for file in additions {
        dispositions.push(disposition_for(recipe, file, game_dir, &mut destinations).await?);
    }
    Ok(dispositions)
}

/// What to do with one planned file.
async fn disposition_for(
    recipe: &PackRecipe,
    file: &PackFileRecipe,
    game_dir: &Path,
    destinations: &mut ManagedDestinations<'_>,
) -> PackResult<Disposition> {
    // A recorded mod is judged by what the recipe says about it, wherever that
    // is and however it is spelled on disk.
    if let Some(recorded) = recorded_entry(recipe, file) {
        destinations.resolve(&recorded.logical_path)?;
        return Ok(match recorded_jar(game_dir, recorded).await {
            // Gone, so restoring it from the verified cache is repair; the
            // recipe entry does not change.
            RecordedJar::Missing => {
                destinations.writable(&recorded.logical_path)?;
                Disposition::Repair
            }
            // An exactly repeated install, which does nothing rather than
            // rewriting a byte-identical file.
            RecordedJar::Intact => Disposition::AlreadyPresent,
            RecordedJar::Altered => Disposition::Altered,
        });
    }

    // Not recorded. Anything standing here is a file Ephervex did not put
    // there: the local build being debugged, or the jar from the bug report. It
    // is not ours to overwrite, and it is not ours to claim.
    let destination = destinations.writable(&file.logical_path)?;
    if let Some(path) = occupant(&destination) {
        return Err(PackError::DestinationOccupied { path });
    }
    Ok(Disposition::Install)
}

/// The recipe entry a request for `file` refers to: the same project at the
/// same version.
///
/// A different version of the project is deliberately not a match: changing it
/// is an update, which this module refuses and the mutation module performs.
pub(crate) fn recorded_entry<'a>(
    recipe: &'a PackRecipe,
    file: &PackFileRecipe,
) -> Option<&'a PackFileRecipe> {
    let (provider, project_id) = file
        .identity
        .as_ref()
        .and_then(ProviderFileIdentity::project)?;
    let existing = recipe.managed_project(provider, project_id)?;
    let version = |entry: &PackFileRecipe| {
        entry
            .identity
            .as_ref()
            .and_then(|identity| identity.version_id.clone())
    };
    (version(existing) == version(file)).then_some(existing)
}

/// The request, with every mod the recipe already records at the requested
/// version replaced by that recorded entry.
///
/// A request for the same project and version names the recorded artifact,
/// wherever its caller would have put it and whichever of the version's files
/// the caller picked. Where the artifact lives and which bytes it is are
/// decisions the recipe has already made. Following the request instead would
/// write a second copy beside the recorded one, which the loader reads as two
/// builds of one mod, or put bytes on disk that the recipe's hashes do not
/// describe.
///
/// The request's download addresses are kept after the recorded ones. A
/// recorded entry may have none (a hand-placed jar) or only a stale one, while
/// the provider being asked serves the recorded bytes. Every address is still
/// verified against the recorded hashes, so an address serving anything else
/// fails rather than installs.
pub(crate) fn settle_recorded_paths(
    recipe: &PackRecipe,
    requested: &[PackFileRecipe],
) -> Vec<PackFileRecipe> {
    requested
        .iter()
        .map(|file| {
            let Some(recorded) = recorded_entry(recipe, file) else {
                return file.clone();
            };
            let mut settled = recorded.clone();
            for address in &file.downloads {
                if !settled.downloads.contains(address) {
                    settled.downloads.push(address.clone());
                }
            }
            settled
        })
        .collect()
}

/// What a recorded mod's jar is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordedJar {
    /// Neither the enabled nor the disabled spelling exists.
    Missing,
    /// Present, under either spelling, with the recorded bytes.
    Intact,
    /// Present, but not provably the recorded artifact: other bytes, a link, a
    /// directory, or a file that could not be read back.
    Altered,
}

/// Looks for a recorded mod's jar and checks it against the recorded hashes.
///
/// One answer to "is this recorded artifact installed?" for both the installer
/// and the mutation, so the two cannot disagree about a mod that is turned off
/// or one that was replaced locally.
pub(crate) async fn recorded_jar(game_dir: &Path, recorded: &PackFileRecipe) -> RecordedJar {
    let enabled = game_dir.join(&recorded.logical_path);
    let disabled = disabled_path(&enabled);
    // Inspected without following links, so a dangling link is something
    // standing there rather than a jar that went missing, and a repair never
    // writes through it.
    let present = match (standing(&enabled), standing(&disabled)) {
        (Standing::File, _) => enabled,
        (Standing::Other, _) | (Standing::Absent, Standing::Other) => return RecordedJar::Altered,
        (Standing::Absent, Standing::File) => disabled,
        (Standing::Absent, Standing::Absent) => return RecordedJar::Missing,
    };
    match stored_file_matches(&present, recorded.size, &recorded.hashes).await {
        Ok(true) => RecordedJar::Intact,
        Ok(false) | Err(_) => RecordedJar::Altered,
    }
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
