//! Putting a jar the user already has into a mod host, and taking one out.
//!
//! # Why this is not an install
//!
//! Installing a mod means asking a catalog for a project, resolving a release
//! that fits the instance, verifying it against the hashes the catalog
//! published, and recording all of that so a server built later contains the
//! same file. None of that is available here. What a user hands over is a file,
//! and the only true thing about it is its path.
//!
//! So a file added this way arrives exactly as a jar copied in with a file
//! manager does: it lands in `mods/`, it is listed as [`ModOrigin::HandPlaced`],
//! and identifying it is left to the digest lookup that already runs on a
//! refresh. Nothing here writes a recipe entry, because nothing here has
//! learned anything a recipe could record.
//!
//! # Replacing a recorded file
//!
//! A recipe entry says which bytes belong at a path. A jar put over one makes
//! that entry false: verification would call the user's file corrupt, repair
//! would overwrite it, and a server built from the recipe would ship the
//! recorded release. So the entry goes with the file it described, whether a
//! catalog chose it or a digest lookup adopted it, and the user's jar is
//! hand-placed like any other. An entry the imported pack declares is the
//! pack's to keep, so that file is refused instead (ADR 0019).
//!
//! # What is checked
//!
//! The path is untrusted. Its name is put through the same guard a provider's
//! file name goes through, so nothing can name a destination outside `mods/`;
//! the file has to be a regular file, a `.jar`, a real archive, and under a
//! ceiling. Those checks are cheap and they run twice, once to tell the user
//! what will happen and again when it happens, because the two are separated by
//! however long the user takes to answer.

use std::path::{Path, PathBuf};

use ephervex_download::{CacheLayout, OperationReporter};
use ephervex_model::{Hash, ModOrigin};
use ephervex_pack::{
    ArtifactFetch, ExtractionLimits, MODS_DIR, PackArchive, PackRecipe, PackSide, SourceStore,
    safe_file_name,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::error::{CoreError, CoreResult};
use crate::mods::{InstalledHost, disabled_path};

/// The largest jar Ephervex will copy into a mods folder.
///
/// A mod jar with its assets can be very large, so this is generous. It exists
/// so that a mis-aimed drop of a disk image or a video file is refused with an
/// explanation instead of filling the instance.
pub const MAX_LOCAL_MOD_BYTES: u64 = 512 * 1024 * 1024;

/// What a mod jar's archive may contain before it is refused.
///
/// Tighter than the pack default, which is sized for a modpack carrying every
/// file of an instance. Nothing here extracts anything: the archive is opened
/// only to prove it is one, and the entry count is the cheap half of that.
fn jar_limits() -> ExtractionLimits {
    ExtractionLimits {
        max_entries: 200_000,
        max_total_bytes: MAX_LOCAL_MOD_BYTES,
        max_entry_bytes: MAX_LOCAL_MOD_BYTES,
        ..ExtractionLimits::default()
    }
}

/// Why one file cannot be added to a mods folder.
///
/// Typed rather than a sentence so the frontend localizes it, and one variant
/// per thing the user can actually do something about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LocalModRefusal {
    /// Not a regular file: a directory, a device, or something that vanished.
    NotAFile,
    /// The name does not end in `.jar`.
    NotAJar,
    /// The name cannot address a file inside `mods/` and nothing else.
    UnusableName,
    /// Not a readable archive, so no loader would read it either.
    NotAnArchive,
    /// Larger than [`MAX_LOCAL_MOD_BYTES`].
    TooLarge,
    /// The imported pack declares a file of that name, and its declaration
    /// would keep describing the release it shipped.
    RecordedByPack,
}

impl LocalModRefusal {
    /// The `ephervex-i18n` message id explaining this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::NotAFile => "local-mod-not-a-file",
            Self::NotAJar => "local-mod-not-a-jar",
            Self::UnusableName => "local-mod-unusable-name",
            Self::NotAnArchive => "local-mod-not-an-archive",
            Self::TooLarge => "local-mod-too-large",
            Self::RecordedByPack => "local-mod-recorded-by-pack",
        }
    }

    /// A stable identifier for `--json` output and frontend state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAFile => "not-a-file",
            Self::NotAJar => "not-a-jar",
            Self::UnusableName => "unusable-name",
            Self::NotAnArchive => "not-an-archive",
            Self::TooLarge => "too-large",
            Self::RecordedByPack => "recorded-by-pack",
        }
    }
}

/// What one offered file would do to the mods folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalModOffer {
    /// It can be added, and nothing of that name is there.
    Ready {
        source: PathBuf,
        /// The name it would take inside `mods/`.
        file_name: String,
    },
    /// It can be added, but a jar of that name is already there.
    ///
    /// Separate from [`Self::Ready`] because only a person can decide this:
    /// the jar in the folder may be the one being replaced or may be an
    /// unrelated mod that happens to share a name, and nothing on disk says
    /// which.
    Clashing {
        source: PathBuf,
        file_name: String,
        /// Whether the jar already there is one the loader is ignoring.
        existing_is_disabled: bool,
        /// Whether the instance records that jar as a catalog release or an
        /// identified file. Adding the new one drops that record, so the
        /// question put to the user has to say so.
        existing_is_recorded: bool,
    },
    /// It cannot be added at all.
    Refused {
        source: PathBuf,
        reason: LocalModRefusal,
    },
}

impl LocalModOffer {
    /// The file offered, whatever became of it.
    #[must_use]
    pub fn source(&self) -> &Path {
        match self {
            Self::Ready { source, .. }
            | Self::Clashing { source, .. }
            | Self::Refused { source, .. } => source,
        }
    }

    /// The name it would take in `mods/`, for anything that can take one.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        match self {
            Self::Ready { file_name, .. } | Self::Clashing { file_name, .. } => Some(file_name),
            Self::Refused { .. } => None,
        }
    }
}

/// What a set of offered files would do to one host's mods folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalModPlan {
    pub offers: Vec<LocalModOffer>,
}

impl LocalModPlan {
    /// The files whose names are already taken, which is the only question a
    /// frontend has to put to the user.
    pub fn clashing(&self) -> impl Iterator<Item = &LocalModOffer> {
        self.offers
            .iter()
            .filter(|offer| matches!(offer, LocalModOffer::Clashing { .. }))
    }

    /// Whether anything at all could be added.
    #[must_use]
    pub fn has_anything_to_add(&self) -> bool {
        self.offers
            .iter()
            .any(|offer| !matches!(offer, LocalModOffer::Refused { .. }))
    }

    /// The sources that need no question asked.
    pub fn ready_sources(&self) -> impl Iterator<Item = &Path> {
        self.offers.iter().filter_map(|offer| match offer {
            LocalModOffer::Ready { source, .. } => Some(source.as_path()),
            _ => None,
        })
    }
}

/// What adding a set of files actually did.
///
/// One bad file does not fail a batch: a user who drops six jars and one text
/// file wanted the six jars. Everything that did not happen is named, so the
/// frontend can say so rather than reporting a success that was partial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalModReport {
    /// Names now in `mods/` that were not there before.
    pub added: Vec<String>,
    /// Names that were already there and now hold the new file.
    pub replaced: Vec<String>,
    /// Files that could not be added, and why.
    pub refused: Vec<(PathBuf, LocalModRefusal)>,
}

impl LocalModReport {
    /// Whether the mods folder changed at all.
    #[must_use]
    pub fn changed_anything(&self) -> bool {
        !self.added.is_empty() || !self.replaced.is_empty()
    }

    /// How many files are now in the folder because of this.
    #[must_use]
    pub fn written(&self) -> usize {
        self.added.len() + self.replaced.len()
    }
}

/// The destination name for one offered file, or why it has none.
fn destination_name(source: &Path) -> Result<String, LocalModRefusal> {
    let metadata = std::fs::metadata(source).map_err(|_| LocalModRefusal::NotAFile)?;
    if !metadata.is_file() {
        return Err(LocalModRefusal::NotAFile);
    }
    if metadata.len() > MAX_LOCAL_MOD_BYTES {
        return Err(LocalModRefusal::TooLarge);
    }

    let raw = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(LocalModRefusal::UnusableName)?;
    // Checked before the safe-name guard so that dropping a `.zip` or a `.txt`
    // is told apart from dropping something that cannot name a file at all.
    if !Path::new(raw)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jar"))
    {
        return Err(LocalModRefusal::NotAJar);
    }
    let safe = safe_file_name(raw).ok_or(LocalModRefusal::UnusableName)?;
    let name = safe
        .to_str()
        .ok_or(LocalModRefusal::UnusableName)?
        .to_owned();

    // Last, because it is the only check that reads the file.
    PackArchive::open(source, jar_limits()).map_err(|error| {
        warn!(%error, source = %source.display(), "a file offered as a mod is not a readable archive");
        LocalModRefusal::NotAnArchive
    })?;
    Ok(name)
}

impl App {
    /// What copying these files into a host's mods folder would do.
    ///
    /// Reads and writes nothing. Its whole purpose is to let a frontend ask
    /// about the name clashes before it asks the filesystem to do anything, so
    /// that the decision is made by the person who can make it.
    pub fn plan_local_mods(
        &self,
        host: InstalledHost,
        sources: &[PathBuf],
    ) -> CoreResult<LocalModPlan> {
        let layout = self.host_layout(host)?;
        let dir = layout.game_dir.join(MODS_DIR);

        let mut offers = Vec::with_capacity(sources.len());
        for source in sources {
            let file_name = match destination_name(source) {
                Ok(name) => name,
                Err(reason) => {
                    offers.push(LocalModOffer::Refused {
                        source: source.clone(),
                        reason,
                    });
                    continue;
                }
            };
            let record = recorded_at(layout.recipe.as_ref(), host.side(), &file_name);
            if record == Recorded::ByPack {
                offers.push(LocalModOffer::Refused {
                    source: source.clone(),
                    reason: LocalModRefusal::RecordedByPack,
                });
                continue;
            }
            let installed = dir.join(&file_name);
            // The disabled spelling counts as taken. Writing beside it would
            // leave two copies of one mod in the folder with one of them
            // invisible to the loader and to anybody reading the directory.
            let disabled = disabled_path(&installed);
            offers.push(if installed.is_file() {
                LocalModOffer::Clashing {
                    source: source.clone(),
                    file_name,
                    existing_is_disabled: false,
                    existing_is_recorded: record == Recorded::Replaceable,
                }
            } else if disabled.is_file() {
                LocalModOffer::Clashing {
                    source: source.clone(),
                    file_name,
                    existing_is_disabled: true,
                    existing_is_recorded: record == Recorded::Replaceable,
                }
            } else {
                LocalModOffer::Ready {
                    source: source.clone(),
                    file_name,
                }
            });
        }
        Ok(LocalModPlan { offers })
    }

    /// Copies files into a host's mods folder, replacing what is in the way.
    ///
    /// Every source named here is written, so a caller that asked the user
    /// about a clash passes only what they agreed to. Each file is validated
    /// again first: the answer given to [`Self::plan_local_mods`] describes a
    /// folder as it was, and the user has been thinking since.
    ///
    /// Each file is staged inside the instance's own launcher directory and
    /// renamed into place, so an interrupted copy never leaves half a jar where
    /// a loader would find it.
    ///
    /// Allowed while the host is running: adding a new jar never touches a
    /// file the running loader already has open. The loader picks it up on
    /// the next launch.
    pub fn add_local_mods(
        &self,
        host: InstalledHost,
        sources: &[PathBuf],
    ) -> CoreResult<LocalModReport> {
        self.refuse_mod_change_during_isolation(host)?;
        // Held from reading the recipe to writing it, so a background write
        // cannot restore a record dropped here or be lost to this one.
        let _gate = self.hold_mod_change_blocking(host)?;
        let layout = self.host_layout(host)?;
        let dir = layout.game_dir.join(MODS_DIR);
        let staging = ephervex_storage::metadata::staging_dir(&layout.root);
        let mut recipe = layout.recipe.clone();

        let mut report = LocalModReport::default();
        for source in sources {
            let file_name = match destination_name(source) {
                Ok(name) => name,
                Err(reason) => {
                    report.refused.push((source.clone(), reason));
                    continue;
                }
            };
            let record = recorded_at(recipe.as_ref(), host.side(), &file_name);
            if record == Recorded::ByPack {
                report
                    .refused
                    .push((source.clone(), LocalModRefusal::RecordedByPack));
                continue;
            }
            let installed = dir.join(&file_name);
            let existed = installed.is_file() || disabled_path(&installed).is_file();
            // Before the file, so a record that cannot be dropped leaves the
            // folder as it was. A record dropped for a file that then fails to
            // copy only leaves the old jar unrecorded, which a refresh
            // identifies again.
            if record == Recorded::Replaceable {
                if let Some(recipe) = recipe.as_mut() {
                    forget_replaced_record(&layout, recipe, &file_name)?;
                }
            }
            publish_local_mod(source, &staging, &dir, &file_name)?;
            if existed {
                report.replaced.push(file_name);
            } else {
                report.added.push(file_name);
            }
        }

        if report.changed_anything() {
            info!(
                host = %host.id(),
                added = report.added.len(),
                replaced = report.replaced.len(),
                refused = report.refused.len(),
                "added mods from files the user supplied"
            );
        }
        Ok(report)
    }

    /// Deletes one mod's jar, leaving the instance's desired state alone.
    ///
    /// Offered for the two kinds of file where taking the jar away is not a
    /// claim about what the instance should contain:
    ///
    /// * **a jar somebody put in `mods/` themselves**, which the launcher does
    ///   not own. There is no catalog to fetch it from and no record to repair
    ///   it from, so nothing but the user can decide it has served its purpose,
    ///   and the adopted recipe entry goes with it. That entry exists only to
    ///   remember what a digest lookup found out about a jar, so once the jar
    ///   is gone it describes nothing.
    /// * **a file the imported pack declares.** The declaration stays exactly
    ///   as imported and the jar is kept in the host's put-aside directory, so
    ///   the mod is restorable rather than gone ([`Self::restore_mod_file`]),
    ///   even when the recipe records no address to fetch it from, and a server
    ///   built from the recipe still contains it. Putting a pack's mod aside to
    ///   find which one crashes is the workflow this launcher exists for
    ///   (ADR 0019).
    ///
    /// Refuses everything else. Taking a mod the launcher chose out of an
    /// instance is a removal, which resolves what its dependencies leave
    /// orphaned and retires the files so the change can be undone; deleting the
    /// jar underneath that record would leave the instance describing a mod it
    /// does not have and nothing to say why.
    pub fn delete_mod_file(&self, host: InstalledHost, logical_path: &Path) -> CoreResult<()> {
        self.refuse_mod_change_during_isolation(host)?;
        self.delete_mod_files_together(host, &[logical_path])
    }

    /// Deletes several mods' jars as one change that either happens whole or
    /// not at all.
    ///
    /// Every path is checked before any file moves. A pack file's jar moves
    /// into the put-aside directory, where it stays until it is restored; a
    /// hand-placed one moves into the retired directory. Either way a failure
    /// partway through puts every one back, and only a batch that moved
    /// everything forgets the adopted records and drops the retired copies.
    /// A jar that cannot be put back stays where it was moved, which
    /// reconciliation reports.
    pub(crate) fn delete_mod_files_together(
        &self,
        host: InstalledHost,
        paths: &[&Path],
    ) -> CoreResult<()> {
        // Held until the records are forgotten, so a background write of the
        // recipe cannot restore one or be lost to this one.
        let _gate = self.hold_mod_change_blocking(host)?;
        let snapshot = self.host_mod_snapshot(host)?;
        let mut origins = Vec::with_capacity(paths.len());
        let mut hashes = Vec::with_capacity(paths.len());
        for logical_path in paths {
            let entry = snapshot
                .mods
                .iter()
                .find(|entry| entry.logical_path == *logical_path)
                .ok_or_else(|| CoreError::ModNotManaged {
                    path: logical_path.to_path_buf(),
                })?;
            if !crate::mutation::ModMutability::of(entry).can_delete_file {
                return Err(CoreError::ModNotHandPlaced {
                    path: logical_path.to_path_buf(),
                });
            }
            origins.push(entry.origin);
            hashes.push(entry.hashes.clone());
        }

        let layout = self.host_layout(host)?;
        let retired_dir = ephervex_storage::metadata::retired_dir(&layout.root);
        let put_aside_dir = ephervex_storage::metadata::put_aside_dir(&layout.root);
        let mut set_aside = Vec::new();
        for ((logical_path, origin), hashes) in paths.iter().zip(&origins).zip(&hashes) {
            // Kept whenever its record stays: that record promises the file can
            // come back.
            let keep_in = (*origin != ModOrigin::HandPlaced).then_some(KeepIn {
                dir: put_aside_dir.as_path(),
                hashes,
            });
            if let Err(error) =
                set_aside_jar(&layout, &retired_dir, keep_in, logical_path, &mut set_aside)
            {
                put_back(&mut set_aside);
                return Err(error);
            }
        }

        // Only for a jar nothing else describes. A pack's declaration is what
        // makes its file restorable, so removing it here would turn a reversible
        // deletion into a permanent one.
        let forgotten: Vec<&Path> = paths
            .iter()
            .zip(&origins)
            .filter(|(_, origin)| **origin == ModOrigin::HandPlaced)
            .map(|(path, _)| *path)
            .collect();
        forget_adopted_records(&layout, &forgotten);
        for moved in set_aside.iter().filter(|moved| !moved.kept) {
            if let Err(error) = std::fs::remove_file(&moved.to) {
                warn!(
                    %error,
                    path = %moved.to.display(),
                    "a deleted mod is still in the instance's retired directory"
                );
            }
        }
        for (logical_path, origin) in paths.iter().zip(&origins) {
            info!(
                host = %host.id(),
                path = %logical_path.display(),
                ?origin,
                "deleted a mod's jar"
            );
        }
        Ok(())
    }

    /// Puts a deleted mod's jar back, exactly as the recipe describes it.
    ///
    /// The other half of [`Self::delete_mod_file`] for a file the recipe still
    /// declares. Nothing about the instance's desired state changes, because
    /// nothing about it changed when the jar was deleted; only the directory is
    /// brought back into line with what the recipe already said.
    ///
    /// Deliberately not a mutation. It resolves no dependencies, asks no
    /// catalog and needs no provider identity, which is what lets it work for a
    /// pack file served from a mirror no catalog can name.
    #[instrument(skip(self, reporter, cancel), fields(host = %host.id()))]
    pub async fn restore_mod_file(
        &self,
        host: InstalledHost,
        logical_path: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        self.refuse_mod_change_during_isolation(host)?;
        // Held across the download and the write, so a session cannot start
        // in between and leave a jar it never considered.
        let _gate = self.hold_mod_change(host, cancel).await?;
        self.restore_mod_file_holding_the_gate(host, logical_path, reporter, cancel)
            .await
    }

    /// [`Self::restore_mod_file`] for a caller that already holds the host's
    /// recipe gate and has checked for an isolation session under it.
    pub(crate) async fn restore_mod_file_holding_the_gate(
        &self,
        host: InstalledHost,
        logical_path: &Path,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<()> {
        let layout = self.host_layout(host)?;
        let recipe = layout
            .recipe
            .as_ref()
            .ok_or_else(|| CoreError::ModNotManaged {
                path: logical_path.to_path_buf(),
            })?;

        let restored = self
            .with_artifact_cache(Box::pin(async {
                let manager = self.artifact_manager()?;
                let cache = CacheLayout::new(self.paths().cache_dir());
                ephervex_pack::restore_file(
                    recipe,
                    logical_path,
                    &layout.game_dir,
                    &ephervex_storage::metadata::put_aside_dir(&layout.root),
                    ArtifactFetch {
                        manager: &manager,
                        cache: &cache,
                        // Asked from the file's own provider identity, exactly
                        // as a mutation asks, so a restore offers a secret only
                        // to the artifact it belongs to.
                        credentials: &self.provider_credentials(),
                        reporter,
                        cancel,
                    },
                )
                .await?;
                Ok(())
            }))
            .await;
        if let Err(error) = restored {
            // A jar whose release forbids third-party downloads has no address
            // to come back from, and that is something the user can act on
            // rather than a fault.
            return Err(self
                .as_recorded_manual_acquisition(error, recipe, logical_path, reporter, cancel)
                .await);
        }

        info!(
            host = %host.id(),
            path = %logical_path.display(),
            "restored a mod's jar from the retained recipe"
        );
        Ok(())
    }
}

/// Drops the recipe entries adoption wrote for hand-placed jars, if there are any.
///
/// One write for all of them: each entry is forgotten from the same copy of the
/// recipe, so a later write cannot bring back what an earlier one dropped.
///
/// Best effort on purpose. The files are already gone, and a recipe that could
/// not be rewritten is a stale record rather than a lost one: the next refresh
/// finds no jar behind it and the list does not show it either way.
fn forget_adopted_records(layout: &crate::mods::HostLayout, logical_paths: &[&Path]) {
    let Some(mut recipe) = layout.recipe.clone() else {
        return;
    };
    let mut forgot_any = false;
    for logical_path in logical_paths {
        forgot_any |= recipe.forget_hand_placed(logical_path);
    }
    if !forgot_any {
        return;
    }
    let store = SourceStore::new(ephervex_storage::metadata::source_dir(&layout.root));
    if let Err(error) = store.write_recipe(&recipe) {
        warn!(%error, "a deleted jar's record could not be dropped");
    }
}

/// A jar moved out of the way, and where it came from.
struct SetAside {
    from: PathBuf,
    to: PathBuf,
    /// Moved into the put-aside directory to stay, rather than into the
    /// retired directory to be dropped once the batch succeeds.
    kept: bool,
}

/// Moves both spellings of a mod's jar out of `mods/`.
///
/// Both, because a jar the user turned off is still their jar and a deletion
/// that left the disabled twin behind would put the row straight back on the
/// next refresh. A spelling that is not there is skipped; one that is not a
/// file is a refusal rather than something to move.
///
/// With `keep_in`, the first spelling found is kept there under the logical
/// path and any other goes to the retired directory, as does a copy already
/// kept there. The one exception keeps the recorded release: a kept copy that
/// holds the recorded bytes stays when the jar being put aside does not.
fn set_aside_jar(
    layout: &crate::mods::HostLayout,
    retired_dir: &Path,
    keep_in: Option<KeepIn<'_>>,
    logical_path: &Path,
    set_aside: &mut Vec<SetAside>,
) -> CoreResult<()> {
    let failed = |source: std::io::Error| CoreError::ModFileNotDeleted {
        path: logical_path.to_path_buf(),
        source,
    };
    let hashes = keep_in.map_or(&[][..], |keep| keep.hashes);
    let mut keep_at = keep_in
        .map(|keep| ephervex_pack::put_aside_path(keep.dir, logical_path))
        .transpose()?;
    let installed = layout.game_dir.join(logical_path);
    let disabled = disabled_path(&installed);
    for from in [installed, disabled] {
        match std::fs::symlink_metadata(&from) {
            Ok(metadata) if metadata.is_dir() => {
                return Err(failed(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a directory stands where the jar should be",
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failed(error)),
        }
        if let Some(to) = keep_at.take() {
            if std::fs::symlink_metadata(&to).is_ok() {
                if keeps_the_recorded_release(&to, &from, hashes) {
                    retire(&from, retired_dir, set_aside).map_err(failed)?;
                    continue;
                }
                retire(&to, retired_dir, set_aside).map_err(failed)?;
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(failed)?;
            }
            std::fs::rename(&from, &to).map_err(failed)?;
            set_aside.push(SetAside {
                from,
                to,
                kept: true,
            });
        } else {
            retire(&from, retired_dir, set_aside).map_err(failed)?;
        }
    }
    Ok(())
}

/// Where a jar whose record stays is kept, and the digests its record holds.
#[derive(Clone, Copy)]
struct KeepIn<'a> {
    dir: &'a Path,
    hashes: &'a [Hash],
}

/// Whether the copy already kept holds the recorded bytes and the incoming
/// jar does not, so keeping the incoming one would lose the recorded release.
fn keeps_the_recorded_release(kept: &Path, incoming: &Path, hashes: &[Hash]) -> bool {
    !hashes.is_empty()
        && std::fs::symlink_metadata(kept).is_ok_and(|metadata| metadata.is_file())
        && crate::verify::holds_bytes(kept, hashes)
        && !crate::verify::holds_bytes(incoming, hashes)
}

/// Moves one file into the retired directory, to be dropped once the batch
/// succeeds or put back if it fails.
fn retire(from: &Path, retired_dir: &Path, set_aside: &mut Vec<SetAside>) -> std::io::Result<()> {
    std::fs::create_dir_all(retired_dir)?;
    // A leftover from an interrupted change can sit under the same name, and
    // is retained evidence that a deletion must neither replace nor delete.
    let to = ephervex_pack::unique_retired_path(retired_dir, from);
    std::fs::rename(from, &to)?;
    set_aside.push(SetAside {
        from: from.to_path_buf(),
        to,
        kept: false,
    });
    Ok(())
}

/// Puts every jar a failed batch set aside back where it was, newest first.
///
/// The deletion has already failed by the time this runs, so a jar that cannot
/// come back is logged and left in the retired directory, where reconciliation
/// reports it, rather than replacing the error that matters.
fn put_back(set_aside: &mut Vec<SetAside>) {
    while let Some(moved) = set_aside.pop() {
        if let Err(error) = std::fs::rename(&moved.to, &moved.from) {
            warn!(
                %error,
                path = %moved.from.display(),
                "a mod set aside by a failed deletion could not be put back"
            );
        }
    }
}

/// What the recipe records at one destination in `mods/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// No entry claims the path.
    Nothing,
    /// The imported pack declares it, so the declaration has to stay true.
    ByPack,
    /// Something else wrote the entry: a catalog install, a dependency, or the
    /// adoption of a jar that was hand-placed.
    Replaceable,
}

/// Which kind of entry, if any, claims `mods/<file_name>` for this host.
fn recorded_at(recipe: Option<&PackRecipe>, side: PackSide, file_name: &str) -> Recorded {
    let Some(recipe) = recipe else {
        return Recorded::Nothing;
    };
    let path = Path::new(MODS_DIR).join(file_name);
    if recipe.pack_declares_for(&path, side) {
        Recorded::ByPack
    } else if recipe.records_replaceable_at(&path) {
        Recorded::Replaceable
    } else {
        Recorded::Nothing
    }
}

/// Drops the entries a user's own jar makes false, and saves the recipe.
fn forget_replaced_record(
    layout: &crate::mods::HostLayout,
    recipe: &mut PackRecipe,
    file_name: &str,
) -> CoreResult<()> {
    let logical_path = Path::new(MODS_DIR).join(file_name);
    if !recipe.forget_replaced_records(&logical_path) {
        return Ok(());
    }
    let store = SourceStore::new(ephervex_storage::metadata::source_dir(&layout.root));
    store.write_recipe(recipe)?;
    drop_put_aside_copies(&layout.root, [logical_path.as_path()]);
    Ok(())
}

/// Drops the put-aside copies kept for these logical paths.
///
/// For files whose record changed or went away, so a copy is kept exactly as
/// long as the record that promises it can come back. Best effort: a leftover
/// only costs space, and putting the same file aside again replaces it.
pub(crate) fn drop_put_aside_copies<'a>(
    root: &Path,
    logical_paths: impl IntoIterator<Item = &'a Path>,
) {
    let put_aside_dir = ephervex_storage::metadata::put_aside_dir(root);
    for logical_path in logical_paths {
        let Ok(kept) = ephervex_pack::put_aside_path(&put_aside_dir, logical_path) else {
            continue;
        };
        match std::fs::remove_file(&kept) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                %error,
                path = %kept.display(),
                "a put-aside copy whose record changed could not be removed"
            ),
        }
    }
}

/// Copies one file through staging and renames it into `mods/`.
fn publish_local_mod(source: &Path, staging: &Path, dir: &Path, file_name: &str) -> CoreResult<()> {
    let failed = |error: std::io::Error| CoreError::ModFileNotAdded {
        path: source.to_path_buf(),
        source: error,
    };

    std::fs::create_dir_all(staging).map_err(failed)?;
    std::fs::create_dir_all(dir).map_err(failed)?;

    // Unique per copy, so two adds of the same name, or a leftover from an
    // interrupted run, never share bytes with this one.
    let staged = staging.join(format!("{file_name}.{}", uuid::Uuid::new_v4().simple()));
    if let Err(error) = std::fs::copy(source, &staged) {
        let _ = std::fs::remove_file(&staged);
        return Err(failed(error));
    }

    let installed = dir.join(file_name);
    // The disabled twin goes first. Leaving it would hide a second copy of the
    // mod the user just added behind a suffix, where the loader ignores it and
    // the list shows it once.
    let _ = std::fs::remove_file(disabled_path(&installed));
    if let Err(error) = std::fs::rename(&staged, &installed) {
        let _ = std::fs::remove_file(&staged);
        return Err(CoreError::ModFileNotAdded {
            path: source.to_path_buf(),
            source: error,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Write as _;

    use ephervex_model::{LoaderSelection, ModOrigin};
    use ephervex_storage::{NewClient, Paths};

    use super::*;
    use crate::mods::ModPresence;

    /// An instance with an empty mods folder, and the paths a test needs.
    struct Fixture {
        _base: tempfile::TempDir,
        app: App,
        host: InstalledHost,
        root: PathBuf,
        mods_dir: PathBuf,
    }

    fn fixture() -> Fixture {
        let base = tempfile::tempdir().expect("temp dir");
        let app = App::open_at(Paths::rooted_at(base.path())).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("create");
        let mods_dir = ephervex_storage::metadata::client_game_dir(&record.root).join(MODS_DIR);
        std::fs::create_dir_all(&mods_dir).expect("mods dir");
        Fixture {
            _base: base,
            app,
            host: InstalledHost::Instance(record.id),
            root: record.root,
            mods_dir,
        }
    }

    /// A real, if empty, zip archive under whatever name is asked for.
    ///
    /// The validation opens the file, so a test fixture that is only the word
    /// "jar" would be refused for the right reason and prove nothing about the
    /// behavior under test.
    fn jar_at(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "META-INF/MANIFEST.MF",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("entry");
        zip.write_all(b"Manifest-Version: 1.0\n").expect("write");
        zip.finish().expect("finish");
        path
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_jar_the_user_chose_is_copied_in_and_listed_as_added_by_hand() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "from-a-forum-post.jar");

        let report = host
            .app
            .add_local_mods(host.host, &[source])
            .expect("add the jar");

        assert_eq!(report.added, vec!["from-a-forum-post.jar".to_owned()]);
        assert!(report.replaced.is_empty());
        assert!(report.refused.is_empty());
        assert_eq!(names_in(&host.mods_dir), vec!["from-a-forum-post.jar"]);

        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");
        assert_eq!(snapshot.mods.len(), 1);
        assert_eq!(snapshot.mods[0].origin, ModOrigin::HandPlaced);
    }

    #[test]
    fn adding_a_local_mod_while_the_instance_is_running_still_succeeds() {
        // Copying in a new jar never touches a file the running loader
        // already has open, so this is never refused.
        let host = fixture();
        let _lock = ephervex_storage::LaunchLock::acquire(&host.root).expect("acquire lock");
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "added-while-running.jar");

        let report = host
            .app
            .add_local_mods(host.host, &[source])
            .expect("add the jar even while running");

        assert_eq!(report.added, vec!["added-while-running.jar".to_owned()]);
        assert_eq!(names_in(&host.mods_dir), vec!["added-while-running.jar"]);
    }

    #[test]
    fn adding_a_jar_records_nothing_in_the_recipe() {
        // Nothing here learned what the file is, so there is nothing a recipe
        // could honestly say about it. Identification is a separate run.
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "unknown.jar");

        host.app
            .add_local_mods(host.host, &[source])
            .expect("add the jar");

        let layout = host.app.host_layout(host.host).expect("layout");
        assert!(
            layout.recipe.is_none(),
            "an instance created by hand gains no recipe from a copied-in jar"
        );
    }

    #[test]
    fn nothing_but_a_readable_jar_archive_is_copied_in() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");

        let not_a_jar = chosen.path().join("notes.txt");
        std::fs::write(&not_a_jar, b"not a mod").expect("write");
        let not_an_archive = chosen.path().join("truncated.jar");
        std::fs::write(&not_an_archive, b"this is not a zip").expect("write");
        let a_directory = chosen.path().join("a-directory.jar");
        std::fs::create_dir(&a_directory).expect("mkdir");
        let good = jar_at(chosen.path(), "real.jar");

        let report = host
            .app
            .add_local_mods(
                host.host,
                &[
                    not_a_jar.clone(),
                    not_an_archive.clone(),
                    a_directory.clone(),
                    good,
                ],
            )
            .expect("the batch itself succeeds");

        assert_eq!(
            report.added,
            vec!["real.jar".to_owned()],
            "one bad file must not cost the user the good ones"
        );
        let refused: Vec<(PathBuf, LocalModRefusal)> = report.refused;
        assert_eq!(
            refused,
            vec![
                (not_a_jar, LocalModRefusal::NotAJar),
                (not_an_archive, LocalModRefusal::NotAnArchive),
                (a_directory, LocalModRefusal::NotAFile),
            ]
        );
        assert_eq!(names_in(&host.mods_dir), vec!["real.jar"]);
    }

    #[test]
    fn a_name_that_would_leave_the_mods_folder_is_refused() {
        // The path comes from a desktop drag, which is untrusted input like any
        // other. Nothing is sanitized into a name that still writes somewhere.
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let nested = chosen.path().join("nested");
        std::fs::create_dir_all(&nested).expect("mkdir");
        let source = jar_at(&nested, "fine.jar");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(
            matches!(plan.offers.as_slice(), [LocalModOffer::Ready { file_name, .. }] if file_name == "fine.jar"),
            "a jar in a subdirectory is still just a file name: {:?}",
            plan.offers
        );

        host.app.add_local_mods(host.host, &[source]).expect("add");
        assert_eq!(names_in(&host.mods_dir), vec!["fine.jar"]);
        assert!(
            !host.mods_dir.join("nested").exists(),
            "nothing below `mods/` is created"
        );
    }

    #[test]
    fn a_name_already_taken_is_reported_rather_than_overwritten_unasked() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "library.jar");
        std::fs::write(host.mods_dir.join("library.jar"), b"the one already here")
            .expect("existing");

        let plan = host
            .app
            .plan_local_mods(host.host, &[source])
            .expect("plan");

        assert_eq!(plan.clashing().count(), 1);
        assert!(
            !matches!(plan.offers[0], LocalModOffer::Ready { .. }),
            "a taken name is a question, not a decision this layer makes"
        );
        assert_eq!(
            std::fs::read(host.mods_dir.join("library.jar")).expect("read"),
            b"the one already here",
            "planning writes nothing"
        );
    }

    #[test]
    fn a_jar_that_is_turned_off_still_owns_its_name() {
        // Writing beside it would leave two copies of one mod in the folder,
        // one of them invisible to the loader and to anybody reading it.
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "library.jar");
        std::fs::write(host.mods_dir.join("library.jar.disabled"), b"turned off")
            .expect("existing");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(
            matches!(
                plan.offers.as_slice(),
                [LocalModOffer::Clashing {
                    existing_is_disabled: true,
                    ..
                }]
            ),
            "{:?}",
            plan.offers
        );

        let report = host.app.add_local_mods(host.host, &[source]).expect("add");
        assert_eq!(report.replaced, vec!["library.jar".to_owned()]);
        assert_eq!(
            names_in(&host.mods_dir),
            vec!["library.jar"],
            "the disabled twin goes with it rather than staying hidden"
        );
    }

    #[test]
    fn deleting_a_jar_added_by_hand_removes_both_of_its_spellings() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("dropped-in.jar.disabled"), b"jar").expect("jar");

        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");
        assert_eq!(snapshot.mods[0].presence, ModPresence::Disabled);

        host.app
            .delete_mod_file(host.host, Path::new("mods/dropped-in.jar"))
            .expect("delete");

        assert!(names_in(&host.mods_dir).is_empty());
        assert!(
            host.app
                .host_mod_snapshot(host.host)
                .expect("snapshot")
                .mods
                .is_empty()
        );
    }

    #[test]
    fn a_recorded_mod_whose_jar_is_gone_is_marked_absent_and_kept_in_the_recipe() {
        // Which is what the mod list filters on, and what a repair reads. The
        // record staying is deliberate: the instance still wants the mod
        // (ADR 0013), so only an explicit deletion forgets it.
        let host = fixture();
        std::fs::write(host.mods_dir.join("still-here.jar"), b"jar").expect("jar");
        record_mod(&host, "still-here.jar", ModOrigin::UserSelected);
        record_mod_appending(
            &host,
            "deleted-while-bisecting.jar",
            ModOrigin::PackDeclared,
        );

        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");

        let absent: Vec<&str> = snapshot
            .missing()
            .map(|entry| entry.file_name.as_str())
            .collect();
        assert_eq!(absent, vec!["deleted-while-bisecting.jar"]);
        assert_eq!(
            snapshot
                .mods
                .iter()
                .filter(|entry| entry.presence != ModPresence::Missing)
                .count(),
            1,
            "and exactly one row is a file that is actually there"
        );

        let named = host
            .app
            .absent_recorded_mods(host.host)
            .expect("absent mods");
        assert_eq!(named.len(), 1, "so a copy can ask about it by name");
        assert_eq!(named[0].name, "deleted-while-bisecting.jar");
    }

    #[test]
    fn deleting_a_path_no_mod_claims_is_refused() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("dropped-in.jar"), b"jar").expect("jar");

        let error = host
            .app
            .delete_mod_file(host.host, Path::new("mods/nothing-here.jar"))
            .expect_err("a path nothing claims is refused");
        assert!(
            matches!(error, CoreError::ModNotManaged { .. }),
            "{error:?}"
        );
    }

    /// Writes a recipe recording one mod at `name`, with the given ownership.
    fn record_mod(host: &Fixture, name: &str, origin: ModOrigin) {
        record_into(host, name, origin, false);
    }

    /// The same, joining whatever the recipe already records.
    fn record_mod_appending(host: &Fixture, name: &str, origin: ModOrigin) {
        record_into(host, name, origin, true);
    }

    fn record_into(host: &Fixture, name: &str, origin: ModOrigin, keep_existing: bool) {
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&host.root));
        let mut recipe = if keep_existing && store.has_recipe() {
            store.read_recipe().expect("the recipe")
        } else {
            ephervex_pack::PackRecipe::for_manual_instance(
                "Repro",
                "1.20.1",
                LoaderSelection::vanilla(),
            )
        };
        recipe
            .record_managed(ephervex_pack::PackFileRecipe {
                logical_path: Path::new(MODS_DIR).join(name),
                hashes: Vec::new(),
                downloads: Vec::new(),
                size: Some(3),
                client: ephervex_pack::SideRequirement::Required,
                server: ephervex_pack::SideRequirement::Unknown,
                selected: None,
                origin,
                identity: Some(ephervex_pack::ProviderFileIdentity {
                    provider: Some(ephervex_model::ModProvider::Modrinth),
                    project_id: Some(format!("PROJ-{name}")),
                    version_id: Some("vvvv".to_owned()),
                    file_id: None,
                }),
                original_identity: None,
                installed_despite_unsupported: None,
                relations: None,
            })
            .expect("record");
        store.write_recipe(&recipe).expect("write the recipe");
    }

    #[test]
    fn deleting_a_jar_the_launcher_installed_keeps_the_record_that_wants_it() {
        // Deleting is not removing. The instance still says it should have
        // this mod, which is what makes the row offer to put it back, keeps a
        // server built from this recipe complete, and lets a repair fetch it.
        let host = fixture();
        std::fs::write(host.mods_dir.join("installed.jar"), b"jar").expect("jar");
        record_mod(&host, "installed.jar", ModOrigin::UserSelected);

        host.app
            .delete_mod_file(host.host, Path::new("mods/installed.jar"))
            .expect("putting a jar aside is how a crash gets bisected");

        assert!(names_in(&host.mods_dir).is_empty());
        let recipe = SourceStore::new(ephervex_storage::metadata::source_dir(&host.root))
            .read_recipe()
            .expect("recipe");
        assert!(
            recipe
                .files
                .iter()
                .any(|file| file.logical_path == Path::new("mods/installed.jar")),
            "the record outlives the file"
        );
    }

    /// A pack's own file is the other side of that rule. The declaration is
    /// what makes the deletion reversible, so it has to survive it.
    #[test]
    fn deleting_a_pack_declared_jar_keeps_the_declaration_that_can_restore_it() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("from-pack.jar"), b"jar").expect("jar");
        record_mod(&host, "from-pack.jar", ModOrigin::PackDeclared);

        host.app
            .delete_mod_file(host.host, Path::new("mods/from-pack.jar"))
            .expect("a pack's mod may be put aside");

        assert!(names_in(&host.mods_dir).is_empty(), "the jar is gone");
        assert_eq!(
            std::fs::read(kept_copy(&host, "from-pack.jar")).expect("kept"),
            b"jar",
            "and kept, so it can come back with no address to fetch it from"
        );
        let layout = host.app.host_layout(host.host).expect("layout");
        assert!(
            layout
                .recipe
                .expect("the recipe survives")
                .files
                .iter()
                .any(|file| file.logical_path == Path::new("mods/from-pack.jar")),
            "the pack still declares the mod, which is what makes this undoable"
        );
    }

    #[test]
    fn deleting_a_jar_added_by_hand_forgets_the_record_adoption_wrote() {
        // The ghost row this exists to prevent: adoption records an identified
        // hand-placed jar, and a record left behind by a deleted file comes
        // back as a mod the instance cannot produce.
        let host = fixture();
        std::fs::write(host.mods_dir.join("identified.jar"), b"jar").expect("jar");
        record_mod(&host, "identified.jar", ModOrigin::HandPlaced);

        let before = host.app.host_mod_snapshot(host.host).expect("snapshot");
        assert_eq!(before.mods.len(), 1, "recorded, and its jar is there");

        host.app
            .delete_mod_file(host.host, Path::new("mods/identified.jar"))
            .expect("delete");

        let layout = host.app.host_layout(host.host).expect("layout");
        assert!(
            layout
                .recipe
                .expect("the recipe survives")
                .recorded_mods()
                .is_empty(),
            "the record goes with the file"
        );
        assert!(
            !kept_copy(&host, "identified.jar").exists(),
            "nothing promises a hand-placed jar back"
        );
        assert!(
            host.app
                .host_mod_snapshot(host.host)
                .expect("snapshot")
                .mods
                .is_empty(),
            "and no row comes back"
        );
    }

    #[test]
    fn deleting_several_jars_added_by_hand_forgets_every_record() {
        // Each record is dropped from the same copy of the recipe, so the last
        // write cannot bring back what an earlier one forgot.
        let host = fixture();
        for name in ["first.jar", "second.jar"] {
            std::fs::write(host.mods_dir.join(name), b"jar").expect("jar");
            record_mod_appending(&host, name, ModOrigin::HandPlaced);
        }

        host.app
            .delete_mod_files(
                host.host,
                &[
                    PathBuf::from("mods/first.jar"),
                    PathBuf::from("mods/second.jar"),
                ],
            )
            .expect("delete");

        assert!(names_in(&host.mods_dir).is_empty());
        let layout = host.app.host_layout(host.host).expect("layout");
        assert!(
            layout
                .recipe
                .expect("the recipe survives")
                .recorded_mods()
                .is_empty(),
            "both records go with their files"
        );
    }

    #[test]
    fn a_failed_batch_delete_keeps_every_jar_and_its_adopted_record() {
        let host = fixture();
        for name in ["first.jar", "second.jar"] {
            std::fs::write(host.mods_dir.join(name), b"jar").expect("jar");
            record_mod_appending(&host, name, ModOrigin::HandPlaced);
        }
        // The second jar cannot be moved: a directory stands where its
        // disabled spelling would be.
        std::fs::create_dir(host.mods_dir.join("second.jar.disabled")).expect("directory");

        let result = host.app.delete_mod_files(
            host.host,
            &[
                PathBuf::from("mods/first.jar"),
                PathBuf::from("mods/second.jar"),
            ],
        );

        assert!(
            matches!(result, Err(CoreError::ModFileNotDeleted { .. })),
            "{result:?}"
        );
        assert!(host.mods_dir.join("first.jar").is_file());
        assert!(host.mods_dir.join("second.jar").is_file());
        let layout = host.app.host_layout(host.host).expect("layout");
        assert_eq!(
            layout.recipe.expect("recipe").recorded_mods().len(),
            2,
            "no record is forgotten for a batch that did not finish"
        );
    }

    fn kept_copy(host: &Fixture, name: &str) -> PathBuf {
        ephervex_storage::metadata::put_aside_dir(&host.root)
            .join(MODS_DIR)
            .join(name)
    }

    #[test]
    fn a_failed_batch_put_aside_puts_every_pack_jar_back() {
        let host = fixture();
        for name in ["first.jar", "second.jar"] {
            std::fs::write(host.mods_dir.join(name), name).expect("jar");
            record_mod_appending(&host, name, ModOrigin::PackDeclared);
        }
        std::fs::create_dir(host.mods_dir.join("second.jar.disabled")).expect("directory");

        let result = host.app.delete_mod_files(
            host.host,
            &[
                PathBuf::from("mods/first.jar"),
                PathBuf::from("mods/second.jar"),
            ],
        );

        assert!(
            matches!(result, Err(CoreError::ModFileNotDeleted { .. })),
            "{result:?}"
        );
        for name in ["first.jar", "second.jar"] {
            assert_eq!(
                std::fs::read(host.mods_dir.join(name)).expect("back in place"),
                name.as_bytes()
            );
            assert!(!kept_copy(&host, name).exists(), "{name} is not also kept");
        }
    }

    #[test]
    fn putting_a_pack_jar_aside_again_keeps_the_jar_it_was_given_last() {
        // A snapshot restore can bring the jar back while an older copy is
        // still kept. The jar the user puts aside now is the one to keep.
        let host = fixture();
        record_mod(&host, "from-pack.jar", ModOrigin::PackDeclared);
        let path = Path::new("mods/from-pack.jar");
        std::fs::write(host.mods_dir.join("from-pack.jar"), b"older").expect("jar");
        host.app
            .delete_mod_file(host.host, path)
            .expect("put aside");
        std::fs::write(host.mods_dir.join("from-pack.jar"), b"newer").expect("jar");

        host.app
            .delete_mod_file(host.host, path)
            .expect("put aside again");

        assert_eq!(
            std::fs::read(kept_copy(&host, "from-pack.jar")).expect("kept"),
            b"newer"
        );
        let retired = ephervex_storage::metadata::retired_dir(&host.root);
        assert!(
            !retired.exists() || names_in(&retired).is_empty(),
            "the older copy is dropped, not left as a leftover"
        );
    }

    #[test]
    fn a_batch_delete_refuses_before_moving_anything_when_one_path_is_not_deletable() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("mine.jar"), b"jar").expect("jar");

        let result = host.app.delete_mod_files(
            host.host,
            &[
                PathBuf::from("mods/mine.jar"),
                PathBuf::from("mods/nothing-here.jar"),
            ],
        );

        assert!(matches!(result, Err(CoreError::ModNotManaged { .. })));
        assert!(host.mods_dir.join("mine.jar").is_file());
    }

    fn recorded_paths(host: &Fixture) -> Vec<PathBuf> {
        host.app
            .host_layout(host.host)
            .expect("layout")
            .recipe
            .map(|recipe| {
                recipe
                    .files
                    .iter()
                    .map(|file| file.logical_path.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_jar_dropped_over_a_catalog_mod_replaces_its_record_with_nothing() {
        // A record that outlives the bytes it described is what made repair
        // overwrite the user's jar and a built server ship the old release.
        let host = fixture();
        std::fs::write(host.mods_dir.join("library.jar"), b"catalog release").expect("jar");
        record_mod(&host, "library.jar", ModOrigin::UserSelected);
        record_mod_appending(&host, "unrelated.jar", ModOrigin::UserSelected);
        std::fs::write(host.mods_dir.join("unrelated.jar"), b"other").expect("jar");
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "library.jar");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(
            matches!(
                plan.offers.as_slice(),
                [LocalModOffer::Clashing {
                    existing_is_recorded: true,
                    ..
                }]
            ),
            "the question has to say the record goes too: {:?}",
            plan.offers
        );

        let report = host.app.add_local_mods(host.host, &[source]).expect("add");
        assert_eq!(report.replaced, vec!["library.jar".to_owned()]);

        assert_eq!(
            recorded_paths(&host),
            vec![PathBuf::from("mods/unrelated.jar")],
            "only the replaced file's record goes"
        );
        let snapshot = host.app.host_mod_snapshot(host.host).expect("snapshot");
        let replaced = snapshot
            .mods
            .iter()
            .find(|entry| entry.logical_path == Path::new("mods/library.jar"))
            .expect("the user's jar is listed");
        assert_eq!(replaced.origin, ModOrigin::HandPlaced);
    }

    #[test]
    fn a_jar_dropped_over_an_identified_hand_placed_jar_drops_the_old_identity() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("identified.jar"), b"old bytes").expect("jar");
        record_mod(&host, "identified.jar", ModOrigin::HandPlaced);
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "identified.jar");

        host.app.add_local_mods(host.host, &[source]).expect("add");

        assert!(
            recorded_paths(&host).is_empty(),
            "the adopted identity described the old file"
        );
    }

    #[test]
    fn a_jar_dropped_where_a_recorded_mod_was_put_aside_replaces_the_record() {
        // Restoring the recorded release over the file the user then supplied
        // would be the same loss as replacing a present one.
        let host = fixture();
        record_mod(&host, "put-aside.jar", ModOrigin::UserSelected);
        std::fs::write(host.mods_dir.join("put-aside.jar"), b"recorded").expect("jar");
        host.app
            .delete_mod_file(host.host, Path::new("mods/put-aside.jar"))
            .expect("put aside");
        assert!(kept_copy(&host, "put-aside.jar").is_file());
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "put-aside.jar");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(matches!(
            plan.offers.as_slice(),
            [LocalModOffer::Ready { .. }]
        ));

        host.app.add_local_mods(host.host, &[source]).expect("add");

        assert!(recorded_paths(&host).is_empty());
        assert_eq!(names_in(&host.mods_dir), vec!["put-aside.jar"]);
        assert!(
            !kept_copy(&host, "put-aside.jar").exists(),
            "the copy goes with the record that promised it back"
        );
    }

    #[test]
    fn a_jar_dropped_over_a_file_the_pack_declares_is_refused_and_changes_nothing() {
        let host = fixture();
        std::fs::write(host.mods_dir.join("from-pack.jar"), b"pack bytes").expect("jar");
        record_mod(&host, "from-pack.jar", ModOrigin::PackDeclared);
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "from-pack.jar");

        let plan = host
            .app
            .plan_local_mods(host.host, std::slice::from_ref(&source))
            .expect("plan");
        assert!(
            matches!(
                plan.offers.as_slice(),
                [LocalModOffer::Refused {
                    reason: LocalModRefusal::RecordedByPack,
                    ..
                }]
            ),
            "{:?}",
            plan.offers
        );

        let report = host
            .app
            .add_local_mods(host.host, std::slice::from_ref(&source))
            .expect("the batch itself succeeds");
        assert_eq!(
            report.refused,
            vec![(source, LocalModRefusal::RecordedByPack)]
        );
        assert!(!report.changed_anything());
        assert_eq!(
            std::fs::read(host.mods_dir.join("from-pack.jar")).expect("read"),
            b"pack bytes"
        );
        assert_eq!(
            recorded_paths(&host),
            vec![PathBuf::from("mods/from-pack.jar")]
        );
    }

    #[test]
    fn a_copied_jar_leaves_nothing_behind_in_staging() {
        let host = fixture();
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "tidy.jar");

        host.app.add_local_mods(host.host, &[source]).expect("add");

        let staging = ephervex_storage::metadata::staging_dir(&host.root);
        assert!(names_in(&staging).is_empty(), "{:?}", names_in(&staging));
    }

    #[test]
    fn a_jar_dropped_over_a_declaration_this_host_never_installs_is_added() {
        // A server-only mod puts no file at the path on a client, so the
        // declaration has nothing there to protect.
        let host = fixture();
        record_mod(&host, "server-only.jar", ModOrigin::PackDeclared);
        let store = SourceStore::new(ephervex_storage::metadata::source_dir(&host.root));
        let mut recipe = store.read_recipe().expect("recipe");
        recipe.files[0].client = ephervex_pack::SideRequirement::Unsupported;
        recipe.files[0].server = ephervex_pack::SideRequirement::Required;
        store.write_recipe(&recipe).expect("write the recipe");
        let chosen = tempfile::tempdir().expect("temp dir");
        let source = jar_at(chosen.path(), "server-only.jar");

        let report = host.app.add_local_mods(host.host, &[source]).expect("add");

        assert_eq!(report.added, vec!["server-only.jar".to_owned()]);
        assert_eq!(
            recorded_paths(&host),
            vec![PathBuf::from("mods/server-only.jar")],
            "the declaration for the other side is untouched"
        );
    }
}
