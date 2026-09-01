//! The byte budget and garbage collection for the shared reusable artifact
//! cache.
//!
//! # What this owns, and what it must never touch
//!
//! Exactly the stores for which [`ContentStore::holds_reusable_artifacts`] is
//! true: `downloads/` and `installer-cache/`. Both are addressed purely by a
//! verified content hash, both are written only by the download manager's
//! verify-then-rename publication, and everything in them can be fetched again
//! from the address it came from. Nothing installed depends on an entry staying
//! there — a pack file is copied into the instance, a managed JDK archive is
//! unpacked into the data root, a loader installer has already run — so removing
//! one costs a re-download and nothing else.
//!
//! Every other cache store is out of scope and stays out. `libraries/` is what a
//! retained server's own `libraries` link points at, `assets/` is the directory a
//! running game is handed, `natives/` is unpacked rather than downloaded,
//! `metadata/` is provider state with freshness envelopes, and `artwork/` holds
//! entries no provider publishes a hash for. See
//! [`ContentStore::holds_reusable_artifacts`] for the per-store reasoning.
//!
//! # The budget is soft
//!
//! A required download is never refused because the artifact is larger than the
//! budget. The limit decides what is worth *keeping*, which is a question that
//! only has an answer once the operation that wanted the artifact is finished.
//! An operation therefore holds a shared lease for as long as it may still need
//! a cache path, and collection needs the lock exclusively — so a cache can sit
//! over budget for a while, and that is the correct outcome rather than a bug.
//!
//! # Why a lease and not a "small enough" window
//!
//! A consumer that has been handed a cache path and has not opened the file yet
//! is exactly the case an unlinking collector breaks, and it is not a narrow
//! window: materializing a pack copies dozens of files one after another, and a
//! launch builds a command line from cache paths and then spawns a JVM that opens
//! them. Unlinking an already-open file is harmless on Linux; unlinking a file
//! somebody is *about to* open is a failed installation. The lease closes that
//! gap, and because it is a kernel advisory lock on a file under the cache root
//! it closes it for a second Ephervex process too.

use std::fs::{File, Metadata, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ephervex_model::ArtifactCacheLimit;
use tracing::{debug, trace, warn};

use crate::cache::{CacheLayout, ContentStore};

/// The maintenance lock's name under the cache root.
///
/// Fixed and launcher-owned. Nothing about an artifact, a project, a provider,
/// or anything else a remote party can influence appears in it, and it is at the
/// cache root rather than inside a swept store so maintenance can never be asked
/// to collect its own lock.
pub const MAINTENANCE_LOCK_FILE: &str = "artifact-cache.lock";

/// How many individual troubles one maintenance pass reports in detail.
///
/// A cache with a permissions problem can have thousands of entries, and a
/// report that names all of them is a report nobody reads. The count is always
/// exact; the paths are a sample.
const REPORTED_TROUBLES: usize = 8;

/// Which stores artifact-cache maintenance governs.
///
/// Derived from [`ContentStore::holds_reusable_artifacts`] rather than written
/// out, so adding a store cannot silently add it to the sweep or silently leave
/// it out: the predicate on the store is the single place that decision is made.
#[must_use]
pub fn reusable_artifact_stores() -> Vec<ContentStore> {
    [
        ContentStore::Assets,
        ContentStore::Libraries,
        ContentStore::InstallerCache,
        ContentStore::Downloads,
        ContentStore::Metadata,
        ContentStore::Artwork,
        ContentStore::Natives,
    ]
    .into_iter()
    .filter(|store| store.holds_reusable_artifacts())
    .collect()
}

/// Why artifact-cache maintenance could not be carried out.
///
/// Deliberately short. An unreadable or undeletable individual entry is *not*
/// here: those are outcomes a pass reports while still doing the rest of its
/// work, and turning one of them into a failure would mean a single stubborn
/// file could stop the cache being maintained at all.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArtifactCacheError {
    /// Another operation or another Ephervex process is using the cache.
    #[error("the artifact cache is in use; maintenance was not started")]
    Busy { path: PathBuf },
    /// The maintenance lock itself could not be created or taken.
    #[error("the artifact cache maintenance lock at {path} could not be used")]
    LockUnusable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A store root could not be listed, so nothing can be said about usage.
    #[error("the artifact cache store at {path} could not be read")]
    ScanFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl ArtifactCacheError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Busy { .. } => "error-artifact-cache-busy",
            Self::LockUnusable { .. } => "error-artifact-cache-lock-unusable",
            Self::ScanFailed { .. } => "error-artifact-cache-scan-failed",
        }
    }

    /// The cache path this failure is about.
    ///
    /// Always a launcher-owned path under the cache root, which is why it is
    /// safe to show: nothing here is derived from a URL, a credential, or a
    /// value a provider chose.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Busy { path }
            | Self::LockUnusable { path, .. }
            | Self::ScanFailed { path, .. } => Some(path.clone()),
        }
    }
}

/// Where a pass ran into a single entry it could not handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceStage {
    /// A directory inside a store could not be listed.
    Listing,
    /// An entry's size or publication time could not be read.
    Inspecting,
    /// A candidate could not be removed.
    Removing,
}

/// One entry a pass could not handle, and how far it got.
///
/// The cause is an [`std::io::ErrorKind`] rather than the error itself so a
/// report stays comparable and cheap to carry; the full error is logged where it
/// happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceTrouble {
    pub path: PathBuf,
    pub stage: MaintenanceStage,
    pub cause: std::io::ErrorKind,
}

/// What one maintenance pass measured and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactCacheMaintenance {
    /// Eligible bytes before the pass.
    pub usage_before: u64,
    /// Eligible bytes after it.
    pub usage_after: u64,
    /// Eligible entries after it.
    pub entries_after: u64,
    /// The low-water mark this pass aimed at, when it was collecting.
    pub target: Option<u64>,
    pub removed_entries: u64,
    pub freed_bytes: u64,
    /// Whether every directory in the swept stores could be listed and every
    /// entry inspected. A `false` makes the byte figures a lower bound.
    pub complete: bool,
    /// A bounded sample of what went wrong.
    pub troubles: Vec<MaintenanceTrouble>,
    /// How many entries were troublesome in total, sampled or not.
    pub troubled_entries: u64,
}

impl ArtifactCacheMaintenance {
    /// Whether the pass saw the whole store and hit nothing it could not handle.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        self.complete && self.troubled_entries == 0
    }

    fn record(&mut self, trouble: MaintenanceTrouble) {
        self.troubled_entries = self.troubled_entries.saturating_add(1);
        if self.troubles.len() < REPORTED_TROUBLES {
            self.troubles.push(trouble);
        }
    }
}

/// How much of the reusable artifact cache is in use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactCacheUsage {
    pub bytes: u64,
    pub entries: u64,
    /// Whether the figures cover the whole store. See
    /// [`ArtifactCacheMaintenance::complete`].
    pub complete: bool,
}

/// A held shared lease on the reusable artifact cache.
///
/// While one exists — in this process or any other — collection and manual
/// clearing cannot start. Released on drop, and released by the kernel if the
/// process dies, so there is no such thing as a stale lease.
///
/// Deliberately not `Clone`: one value is one open file description is one
/// advisory lock.
#[derive(Debug)]
pub struct ArtifactCacheLease {
    /// `None` only where the platform has no advisory locking, in which case
    /// there is nothing to release. See [`lock`].
    _held: Option<File>,
}

/// Maintenance for one cache root.
///
/// Cheap to construct and holds no state: the whole point is that there is no
/// index to keep in step with the filesystem, and no index to be wrong.
#[derive(Debug, Clone)]
pub struct ArtifactCache {
    layout: CacheLayout,
}

impl ArtifactCache {
    #[must_use]
    pub const fn new(layout: CacheLayout) -> Self {
        Self { layout }
    }

    #[must_use]
    pub const fn layout(&self) -> &CacheLayout {
        &self.layout
    }

    /// The maintenance lock's path.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.layout.root().join(MAINTENANCE_LOCK_FILE)
    }

    /// Takes a shared lease for an operation that may still need a cache path.
    ///
    /// Blocks while a collection is in progress, which is bounded work — a scan
    /// and some unlinking — and then proceeds. Waiting is the right behavior
    /// here: the alternative is either failing an installation because the cache
    /// was being tidied, or proceeding without protection, and neither is worth
    /// having.
    pub async fn lease(&self) -> Result<ArtifactCacheLease, ArtifactCacheError> {
        let path = self.lock_path();
        blocking(move || {
            lock(&path, Exclusivity::Shared).map(|held| ArtifactCacheLease { _held: held })
        })
        .await
    }

    /// Measures the reusable artifact cache.
    ///
    /// Uses the same eligibility rules as collection, because a usage figure a
    /// user cannot act on is worse than none: if a byte is counted here, a
    /// cleanup can free it.
    ///
    /// Takes the shared side of the maintenance lock, so measuring cannot
    /// observe a store halfway through somebody else's collection.
    pub async fn usage(&self) -> Result<ArtifactCacheUsage, ArtifactCacheError> {
        let layout = self.layout.clone();
        let path = self.lock_path();
        blocking(move || {
            let _lease = lock(&path, Exclusivity::Shared)?;
            let survey = survey(&layout)?;
            Ok(ArtifactCacheUsage {
                bytes: survey.bytes,
                entries: survey.entries.len() as u64,
                complete: survey.report.complete,
            })
        })
        .await
    }

    /// Enforces a budget, evicting oldest-first until the low-water mark is met.
    ///
    /// Does nothing at all for [`ArtifactCacheLimit::Unlimited`] — not even a
    /// scan, so an unlimited cache costs nothing after every operation. For a
    /// limited budget the pass is: take the lock exclusively, measure, stop if
    /// usage is within the high-water mark, otherwise remove eligible entries in
    /// publication order until usage is at or below
    /// [`ArtifactCacheLimit::cleanup_target`] or nothing removable is left.
    ///
    /// Measuring before removing is deliberate. A collector that unlinks first
    /// and totals afterwards cannot answer "was this necessary".
    pub async fn collect(
        &self,
        limit: ArtifactCacheLimit,
    ) -> Result<ArtifactCacheMaintenance, ArtifactCacheError> {
        let Some(high_water) = limit.high_water() else {
            trace!("artifact cache is unlimited; nothing to collect");
            return Ok(ArtifactCacheMaintenance {
                complete: true,
                ..ArtifactCacheMaintenance::default()
            });
        };
        let target = limit.cleanup_target();
        let layout = self.layout.clone();
        let path = self.lock_path();

        blocking(move || {
            let _guard = lock(&path, Exclusivity::Exclusive)?;
            let mut survey = survey(&layout)?;
            let mut report = std::mem::take(&mut survey.report);
            report.usage_before = survey.bytes;
            report.usage_after = survey.bytes;
            report.entries_after = survey.entries.len() as u64;

            if survey.bytes <= high_water {
                trace!(
                    usage = survey.bytes,
                    high_water, "artifact cache is within its budget"
                );
                return Ok(report);
            }
            report.target = target;
            // `high_water` is `Some`, so `cleanup_target` is too; falling back to
            // the high-water mark keeps this branch total without an unwrap.
            let target = target.unwrap_or(high_water);

            debug!(
                usage = survey.bytes,
                high_water, target, "artifact cache is over budget; collecting"
            );
            evict(&mut survey.entries, target, &mut report);
            Ok(report)
        })
        .await
    }

    /// Removes every reusable artifact the cache currently holds.
    ///
    /// The manual counterpart to [`Self::collect`], and subject to the same
    /// lock: a clear that ran while an installation was copying out of the cache
    /// would break the installation. A busy cache is reported as
    /// [`ArtifactCacheError::Busy`] rather than waited on, so a frontend never
    /// hangs on another process.
    pub async fn clear(&self) -> Result<ArtifactCacheMaintenance, ArtifactCacheError> {
        let layout = self.layout.clone();
        let path = self.lock_path();
        blocking(move || {
            let _guard = lock(&path, Exclusivity::Exclusive)?;
            let mut survey = survey(&layout)?;
            let mut report = std::mem::take(&mut survey.report);
            report.usage_before = survey.bytes;
            report.usage_after = survey.bytes;
            report.entries_after = survey.entries.len() as u64;
            report.target = Some(0);
            debug!(
                usage = survey.bytes,
                entries = survey.entries.len(),
                "clearing the reusable artifact cache"
            );
            evict(&mut survey.entries, 0, &mut report);
            Ok(report)
        })
        .await
    }
}

/// Runs filesystem work off the async runtime.
///
/// Everything in this module is `stat`, `readdir`, `unlink`, and a blocking
/// `flock`, none of which has a useful asynchronous form; doing it on a blocking
/// thread is what keeps a collection from stalling a progress bar.
async fn blocking<T, F>(work: F) -> Result<T, ArtifactCacheError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ArtifactCacheError> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(ArtifactCacheError::LockUnusable {
            path: PathBuf::new(),
            source: std::io::Error::other(error),
        }),
    }
}

/// Which side of the maintenance lock is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exclusivity {
    /// Any number of operations may hold this at once; it only excludes
    /// maintenance.
    Shared,
    /// Held by one collector, and never waited for.
    Exclusive,
}

/// One finalized entry, as a collector needs to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactEntry {
    path: PathBuf,
    bytes: u64,
    /// When the artifact's bytes were last written, which for this cache is when
    /// it was published. See [`survey`].
    published: SystemTime,
}

/// The measured state of the reusable stores.
#[derive(Debug, Default)]
struct Survey {
    entries: Vec<ArtifactEntry>,
    bytes: u64,
    report: ArtifactCacheMaintenance,
}

/// Lists every finalized reusable artifact, oldest publication first.
///
/// # The ordering policy, stated exactly
///
/// Oldest **publication** first, where publication time is the entry's
/// modification time. This is not LRU and is not described as such anywhere: the
/// cache is content-addressed, so a hit opens the file and changes nothing about
/// it, and there is no access record to sort by. `atime` is not an alternative —
/// Linux desktops mount with `relatime` or `noatime`, so it is either coarse or
/// frozen, and a collector that trusted it would evict essentially at random.
/// Writing an access record on every hit was the other option and is worse: it
/// turns every cache read into a cache write, for a heuristic.
///
/// `mtime` is exactly right for what it is used for. The download manager writes
/// a partial sibling and renames it into place, and `rename` does not touch
/// `mtime`, so an entry's `mtime` is when its bytes arrived. Ties break on path,
/// so two artifacts published in the same timestamp granularity are still
/// collected in a defined order.
///
/// # What is counted
///
/// One entry per distinct file: recognized by [`CacheLayout::recognize_content`],
/// confirmed a regular file by `symlink_metadata`, and deduplicated by
/// filesystem identity so a hard link cannot be counted twice. Nothing else in
/// the store contributes — not a directory, not a symlink, not a transfer in
/// flight, and not a name this build does not recognize. Unrecognized names are
/// left strictly alone: guessing that an unknown file is an artifact is how a
/// collector deletes something that mattered.
fn survey(layout: &CacheLayout) -> Result<Survey, ArtifactCacheError> {
    let mut survey = Survey {
        report: ArtifactCacheMaintenance {
            complete: true,
            ..ArtifactCacheMaintenance::default()
        },
        ..Survey::default()
    };
    let mut seen: Vec<(u64, u64)> = Vec::new();

    for store in reusable_artifact_stores() {
        let root = layout.store_root(store);
        let algorithms = match read_directory(&root) {
            Ok(Some(entries)) => entries,
            // A store nothing has written to yet is empty, not broken.
            Ok(None) => continue,
            Err(source) => return Err(ArtifactCacheError::ScanFailed { path: root, source }),
        };

        for algorithm in algorithms {
            let Some(name) = directory_name(&algorithm, &mut survey.report) else {
                continue;
            };
            let shards = match read_directory(&algorithm) {
                Ok(Some(entries)) => entries,
                Ok(None) => continue,
                Err(source) => {
                    note_listing_failure(&algorithm, &source, &mut survey.report);
                    continue;
                }
            };

            for shard in shards {
                let Some(shard_name) = directory_name(&shard, &mut survey.report) else {
                    continue;
                };
                let files = match read_directory(&shard) {
                    Ok(Some(entries)) => entries,
                    Ok(None) => continue,
                    Err(source) => {
                        note_listing_failure(&shard, &source, &mut survey.report);
                        continue;
                    }
                };

                for file in files {
                    collect_entry(&file, &name, &shard_name, &mut seen, &mut survey);
                }
            }
        }
    }

    survey.entries.sort_by(|left, right| {
        left.published
            .cmp(&right.published)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(survey)
}

/// Considers one directory entry inside a shard.
fn collect_entry(
    path: &Path,
    algorithm: &str,
    shard: &str,
    seen: &mut Vec<(u64, u64)>,
    survey: &mut Survey,
) {
    let Some(file_name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return;
    };
    if CacheLayout::recognize_content(algorithm, shard, file_name).is_none() {
        trace!(path = %path.display(), "leaving an unrecognized cache file alone");
        return;
    }

    // `symlink_metadata` describes the entry itself, so a symlink is seen as a
    // symlink rather than as whatever it points at. A collector that followed
    // one could be pointed at anything on the filesystem.
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) => {
            note_entry_failure(
                path,
                MaintenanceStage::Inspecting,
                &source,
                &mut survey.report,
            );
            return;
        }
    };
    if !metadata.is_file() {
        return;
    }
    let Ok(published) = metadata.modified() else {
        // No publication time means no place in the eviction order and no honest
        // contribution to usage; both would have to be guessed.
        note_entry_failure(
            path,
            MaintenanceStage::Inspecting,
            &std::io::Error::from(std::io::ErrorKind::Unsupported),
            &mut survey.report,
        );
        return;
    };

    if let Some(identity) = file_identity(&metadata) {
        if seen.contains(&identity) {
            trace!(path = %path.display(), "a second name for one artifact is counted once");
            return;
        }
        seen.push(identity);
    }

    survey.bytes = survey.bytes.saturating_add(metadata.len());
    survey.entries.push(ArtifactEntry {
        path: path.to_path_buf(),
        bytes: metadata.len(),
        published,
    });
}

/// Removes entries in order until usage is at or below `target`.
///
/// A removal that fails is recorded and the pass moves on to the next candidate:
/// one file the user has no permission to delete must not stop the rest of the
/// cache being collected, and it must not be retried in a loop either. An entry
/// that has already vanished counts as freed, because it is: another process got
/// there first.
fn evict(entries: &mut [ArtifactEntry], target: u64, report: &mut ArtifactCacheMaintenance) {
    for entry in entries {
        if report.usage_after <= target {
            break;
        }
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(
                    path = %entry.path.display(),
                    %error,
                    "could not remove a reusable artifact"
                );
                note_entry_failure(&entry.path, MaintenanceStage::Removing, &error, report);
                continue;
            }
        }
        report.removed_entries = report.removed_entries.saturating_add(1);
        report.freed_bytes = report.freed_bytes.saturating_add(entry.bytes);
        report.usage_after = report.usage_after.saturating_sub(entry.bytes);
        report.entries_after = report.entries_after.saturating_sub(1);
        prune_empty_shard(&entry.path);
    }
}

/// Removes the shard directory an evicted entry was the last member of.
///
/// Exactly one level, and only with `remove_dir`, which refuses a directory that
/// still has contents. Nothing walks upward from here: pruning further would
/// mean deleting a store root, and a store root is part of the layout rather
/// than part of its contents.
fn prune_empty_shard(entry: &Path) {
    let Some(shard) = entry.parent() else {
        return;
    };
    if let Err(error) = std::fs::remove_dir(shard) {
        trace!(path = %shard.display(), %error, "shard directory kept");
    }
}

/// Lists a directory. `Ok(None)` means it does not exist.
fn read_directory(path: &Path) -> std::io::Result<Option<Vec<PathBuf>>> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(entry?.path());
    }
    Ok(Some(paths))
}

/// The name of `path`, if it is a real directory rather than a link or a file.
fn directory_name(path: &Path, report: &mut ArtifactCacheMaintenance) -> Option<String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) => {
            note_entry_failure(path, MaintenanceStage::Inspecting, &source, report);
            return None;
        }
    };
    // A symlinked directory is not descended into, which is what keeps a
    // traversal inside the cache root it was given.
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return None;
    }
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_owned)
}

fn note_listing_failure(
    path: &Path,
    error: &std::io::Error,
    report: &mut ArtifactCacheMaintenance,
) {
    warn!(path = %path.display(), %error, "could not list part of the artifact cache");
    report.complete = false;
    report.record(MaintenanceTrouble {
        path: path.to_path_buf(),
        stage: MaintenanceStage::Listing,
        cause: error.kind(),
    });
}

fn note_entry_failure(
    path: &Path,
    stage: MaintenanceStage,
    error: &std::io::Error,
    report: &mut ArtifactCacheMaintenance,
) {
    if stage == MaintenanceStage::Inspecting {
        // An entry whose size is unknown is missing from the totals, so the
        // totals are a lower bound.
        report.complete = false;
    }
    report.record(MaintenanceTrouble {
        path: path.to_path_buf(),
        stage,
        cause: error.kind(),
    });
}

/// The filesystem identity of a file, where the platform exposes one.
///
/// `None` means "cannot tell", and a caller treats that as a distinct file —
/// counting a shared inode twice overstates usage, which is the harmless
/// direction compared with silently dropping an entry.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the platform without an identity to report is the other branch"
)]
fn file_identity(metadata: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &Metadata) -> Option<(u64, u64)> {
    None
}

/// Takes one side of the maintenance lock.
///
/// `flock` rather than a lock file recording a pid: an advisory lock is released
/// by the kernel when the holder's last descriptor closes, including when the
/// holder is killed, so there is no stale-lock case to reason about and no
/// liveness check that can be wrong about a recycled pid.
///
/// The exclusive side is always non-blocking. Two reasons: a frontend must never
/// hang waiting on another process, and a collection triggered inside an
/// operation that still holds its own shared lease has to fail fast rather than
/// wait for itself.
#[cfg(unix)]
fn lock(path: &Path, exclusivity: Exclusivity) -> Result<Option<File>, ArtifactCacheError> {
    use rustix::fs::{FlockOperation, flock};

    let unusable = |source: std::io::Error| ArtifactCacheError::LockUnusable {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(unusable)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(unusable)?;

    let operation = match exclusivity {
        Exclusivity::Shared => FlockOperation::LockShared,
        Exclusivity::Exclusive => FlockOperation::NonBlockingLockExclusive,
    };
    match flock(&file, operation) {
        Ok(()) => Ok(Some(file)),
        Err(errno) if is_contention(errno) => Err(ArtifactCacheError::Busy {
            path: path.to_path_buf(),
        }),
        Err(errno) => Err(unusable(errno.into())),
    }
}

#[cfg(unix)]
fn is_contention(errno: rustix::io::Errno) -> bool {
    errno == rustix::io::Errno::WOULDBLOCK || errno == rustix::io::Errno::AGAIN
}

/// Without advisory locking there is nothing that can make collection safe
/// against a second process, so collection is refused and ordinary operations
/// carry on unaffected.
#[cfg(not(unix))]
fn lock(path: &Path, exclusivity: Exclusivity) -> Result<Option<File>, ArtifactCacheError> {
    match exclusivity {
        Exclusivity::Shared => Ok(None),
        Exclusivity::Exclusive => Err(ArtifactCacheError::LockUnusable {
            path: path.to_path_buf(),
            source: std::io::Error::from(std::io::ErrorKind::Unsupported),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::time::Duration;

    use ephervex_model::{GIB, Hash, HashAlgorithm, MIB};

    use super::*;
    use crate::cache::{FileExtension, PART_SUFFIX};

    /// A cache root that only this test can see.
    struct Fixture {
        _root: tempfile::TempDir,
        cache: ArtifactCache,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temp dir");
            let cache = ArtifactCache::new(CacheLayout::new(root.path()));
            Self { _root: root, cache }
        }

        fn layout(&self) -> &CacheLayout {
            self.cache.layout()
        }

        /// Publishes a recognizable artifact of `bytes` bytes, published `age`
        /// ago, and returns its path.
        ///
        /// The digest is derived from `seed` rather than from the contents: these
        /// tests are about the collector's bookkeeping, and the collector never
        /// re-hashes anything.
        fn publish(&self, store: ContentStore, seed: u8, bytes: usize, age: Duration) -> PathBuf {
            let hash = digest(seed);
            let extension = FileExtension::new("jar").expect("jar");
            let path = self.layout().content(store, &hash, Some(&extension));
            write_file(&path, bytes);
            set_published(&path, age);
            path
        }
    }

    fn digest(seed: u8) -> Hash {
        let digest = format!("{seed:02x}").repeat(20);
        Hash::new(HashAlgorithm::Sha1, &digest).expect("valid sha1")
    }

    fn write_file(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, vec![b'a'; bytes]).expect("write");
    }

    /// Backdates a file's publication time, which is what the collector orders
    /// by. Done explicitly rather than by sleeping so ordering is deterministic.
    fn set_published(path: &Path, age: Duration) {
        let when = SystemTime::now() - age;
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(when)
            .expect("set mtime");
    }

    fn hour(count: u64) -> Duration {
        Duration::from_secs(3_600 * count)
    }

    #[tokio::test]
    async fn an_empty_cache_reports_no_usage_and_collects_nothing() {
        let fixture = Fixture::new();
        let usage = fixture.cache.usage().await.expect("usage");
        assert_eq!(
            usage,
            ArtifactCacheUsage {
                bytes: 0,
                entries: 0,
                complete: true
            }
        );

        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::default())
            .await
            .expect("collect");
        assert_eq!(report.removed_entries, 0);
        assert!(report.is_clean());
    }

    #[tokio::test]
    async fn finalized_entries_in_both_reusable_stores_are_counted() {
        let fixture = Fixture::new();
        fixture.publish(ContentStore::Downloads, 0x11, 300, hour(1));
        fixture.publish(ContentStore::InstallerCache, 0x22, 700, hour(1));

        let usage = fixture.cache.usage().await.expect("usage");
        assert_eq!(usage.bytes, 1_000);
        assert_eq!(usage.entries, 2);
        assert!(usage.complete);
    }

    #[tokio::test]
    async fn no_other_cache_store_contributes_to_artifact_usage() {
        let fixture = Fixture::new();
        let hash = digest(0x33);
        let json = FileExtension::new("json").expect("json");
        // A content-addressed metadata document, an artwork entry, a library, an
        // asset object, and an unpacked native: every one of them lives under
        // the same cache root and none of them is a reusable artifact.
        write_file(
            &fixture
                .layout()
                .content(ContentStore::Metadata, &hash, Some(&json)),
            4_000,
        );
        write_file(
            &fixture
                .layout()
                .project_artwork("modrinth", "abcdefgh")
                .expect("artwork path"),
            5_000,
        );
        write_file(
            &fixture.layout().library(
                &crate::cache::MavenCoordinate::parse("net.example:lib:1.0").expect("coordinate"),
            ),
            6_000,
        );
        write_file(
            &fixture.layout().asset_object(&hash).expect("asset path"),
            7_000,
        );
        write_file(
            &fixture
                .layout()
                .natives("1.21.1", "linux-x86_64")
                .expect("natives path")
                .join("libexample.so"),
            8_000,
        );

        let usage = fixture.cache.usage().await.expect("usage");
        assert_eq!(
            usage,
            ArtifactCacheUsage {
                bytes: 0,
                entries: 0,
                complete: true
            }
        );

        let report = fixture
            .cache
            .clear()
            .await
            .expect("clearing an artifact cache with no artifacts");
        assert_eq!(report.removed_entries, 0);
        assert!(
            fixture
                .layout()
                .content(ContentStore::Metadata, &hash, Some(&json))
                .is_file(),
            "the metadata cache is not governed by the artifact budget"
        );
        assert!(
            fixture
                .layout()
                .asset_object(&hash)
                .expect("asset path")
                .is_file()
        );
    }

    #[tokio::test]
    async fn a_transfer_in_flight_is_neither_counted_nor_removed() {
        let fixture = Fixture::new();
        let published = fixture.publish(ContentStore::Downloads, 0x44, 100, hour(1));
        let mut in_flight = published.clone().into_os_string();
        in_flight.push(format!(".{PART_SUFFIX}.{}", "0".repeat(32)));
        let in_flight = PathBuf::from(in_flight);
        write_file(&in_flight, 9_999);

        let usage = fixture.cache.usage().await.expect("usage");
        assert_eq!(usage.bytes, 100, "staging bytes are not artifact usage");

        fixture.cache.clear().await.expect("clear");
        assert!(!published.exists());
        assert!(
            in_flight.is_file(),
            "a download in flight must survive a clear"
        );
    }

    #[tokio::test]
    async fn an_unrecognized_file_is_left_alone_rather_than_guessed_at() {
        let fixture = Fixture::new();
        let shard = fixture
            .layout()
            .store_root(ContentStore::Downloads)
            .join("sha1")
            .join("aa");
        let stranger = shard.join("notes.txt");
        write_file(&stranger, 1_234);
        // A digest-shaped name filed under the wrong shard is also not ours.
        let misfiled = shard.join(format!("{}.jar", digest(0x55).digest()));
        write_file(&misfiled, 4_321);

        let usage = fixture.cache.usage().await.expect("usage");
        assert_eq!(
            usage,
            ArtifactCacheUsage {
                bytes: 0,
                entries: 0,
                complete: true
            }
        );

        fixture.cache.clear().await.expect("clear");
        assert!(stranger.is_file());
        assert!(misfiled.is_file());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_symlinked_shard_is_not_followed_out_of_the_cache() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().expect("outside");
        let bait = outside
            .path()
            .join(format!("{}.jar", digest(0x66).digest()));
        write_file(&bait, 5_000);

        let algorithm = fixture
            .layout()
            .store_root(ContentStore::Downloads)
            .join("sha1");
        std::fs::create_dir_all(&algorithm).expect("mkdir");
        std::os::unix::fs::symlink(outside.path(), algorithm.join("66")).expect("symlink");

        let usage = fixture.cache.usage().await.expect("usage");
        assert_eq!(
            usage.bytes, 0,
            "a traversal must not leave the cache root through a link"
        );
        fixture.cache.clear().await.expect("clear");
        assert!(bait.is_file(), "nothing outside the cache may be removed");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn a_symlinked_entry_is_not_an_artifact() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("real.jar");
        write_file(&target, 2_048);

        let hash = digest(0x77);
        let extension = FileExtension::new("jar").expect("jar");
        let link = fixture
            .layout()
            .content(ContentStore::Downloads, &hash, Some(&extension));
        std::fs::create_dir_all(link.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        assert_eq!(fixture.cache.usage().await.expect("usage").bytes, 0);
        fixture.cache.clear().await.expect("clear");
        assert!(link.symlink_metadata().is_ok(), "the link is left alone");
        assert!(target.is_file());
    }

    #[tokio::test]
    async fn usage_at_or_below_the_limit_removes_nothing() {
        for usage in [900_usize, 1_000] {
            let fixture = Fixture::new();
            let path = fixture.publish(ContentStore::Downloads, 0x88, usage, hour(9));
            let report = fixture
                .cache
                .collect(ArtifactCacheLimit::Limited(1_000))
                .await
                .expect("collect");
            assert_eq!(report.removed_entries, 0, "usage {usage} is within budget");
            assert_eq!(report.usage_before, usage as u64);
            assert_eq!(report.usage_after, usage as u64);
            assert_eq!(report.target, None);
            assert!(path.is_file());
        }
    }

    #[tokio::test]
    async fn usage_above_the_limit_collects_down_to_the_low_water_mark() {
        let fixture = Fixture::new();
        // Ten entries of 100 bytes each against a 500-byte limit: the target is
        // 400, so six have to go.
        let mut paths = Vec::new();
        for index in 0..10_u8 {
            paths.push(fixture.publish(
                ContentStore::Downloads,
                index,
                100,
                hour(u64::from(10 - index)),
            ));
        }

        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::Limited(500))
            .await
            .expect("collect");
        assert_eq!(report.usage_before, 1_000);
        assert_eq!(report.target, Some(400));
        assert_eq!(report.usage_after, 400);
        assert_eq!(report.removed_entries, 6);
        assert_eq!(report.freed_bytes, 600);
        assert_eq!(report.entries_after, 4);
        assert!(report.is_clean());

        // Oldest publication first: the first six were backdated furthest.
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                path.exists(),
                index >= 6,
                "entry {index} survived the wrong way round"
            );
        }
        assert_eq!(fixture.cache.usage().await.expect("usage").bytes, 400);
    }

    #[tokio::test]
    async fn entries_published_in_the_same_instant_are_collected_in_path_order() {
        let fixture = Fixture::new();
        let same = hour(4);
        let first = fixture.publish(ContentStore::Downloads, 0x0a, 100, same);
        let second = fixture.publish(ContentStore::Downloads, 0x0b, 100, same);
        let third = fixture.publish(ContentStore::Downloads, 0x0c, 100, same);
        assert!(
            first < second && second < third,
            "shard order is path order"
        );

        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::Limited(200))
            .await
            .expect("collect");
        assert_eq!(report.removed_entries, 2);
        assert!(!first.exists() && !second.exists());
        assert!(third.is_file(), "ties break deterministically on path");
    }

    #[tokio::test]
    async fn a_very_large_limit_neither_overflows_nor_collects() {
        let fixture = Fixture::new();
        fixture.publish(ContentStore::Downloads, 0x0d, 4_096, hour(1));
        for limit in [
            ArtifactCacheLimit::Limited(500 * GIB),
            ArtifactCacheLimit::Limited(ephervex_model::MAX_ARTIFACT_CACHE_LIMIT_BYTES),
            ArtifactCacheLimit::Limited(u64::MAX),
        ] {
            let report = fixture.cache.collect(limit).await.expect("collect");
            assert_eq!(report.removed_entries, 0, "{limit} should collect nothing");
            assert_eq!(report.usage_before, 4_096);
        }
    }

    #[tokio::test]
    async fn an_unlimited_cache_is_never_collected_but_can_still_be_measured_and_cleared() {
        let fixture = Fixture::new();
        let path = fixture.publish(ContentStore::Downloads, 0x0e, 8 * 1024 * 1024, hour(50));

        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::Unlimited)
            .await
            .expect("collect");
        assert_eq!(report.removed_entries, 0);
        assert_eq!(
            report.usage_before, 0,
            "an unlimited cache is not even scanned"
        );
        assert!(path.is_file());

        assert_eq!(
            fixture.cache.usage().await.expect("usage").bytes,
            8 * MIB,
            "usage is still measurable when unlimited"
        );
        let cleared = fixture.cache.clear().await.expect("clear");
        assert_eq!(cleared.removed_entries, 1);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn an_artifact_larger_than_the_whole_budget_is_evicted_rather_than_kept_forever() {
        let fixture = Fixture::new();
        // The soft budget cannot be met while a single object this size is
        // retained, so retaining it is not the answer: it goes, and the next
        // operation that needs it fetches it again.
        let huge = fixture.publish(ContentStore::Downloads, 0x0f, 3_000, hour(1));
        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::Limited(2_000))
            .await
            .expect("collect");
        assert_eq!(report.removed_entries, 1);
        assert_eq!(report.usage_after, 0);
        assert!(!huge.exists());
    }

    /// Whether making a directory read-only actually stops this process removing
    /// what is inside it.
    ///
    /// The condition under test is "the launcher tried to remove a file and was
    /// refused". A privileged process is not subject to the mode bits at all, and
    /// neither is a process on a filesystem that does not enforce them, so on those
    /// the barrier this test builds is not a barrier and there is nothing to
    /// observe. Probed rather than inferred from the user id, because privilege is
    /// not the only way it fails to hold.
    #[cfg(unix)]
    fn a_read_only_directory_refuses_this_process() -> bool {
        use std::os::unix::fs::PermissionsExt as _;

        let probe = tempfile::tempdir().expect("tempdir");
        let file = probe.path().join("entry");
        std::fs::write(&file, b"probe").expect("probe file");
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o500))
            .expect("read-only probe directory");
        let refused = std::fs::remove_file(&file).is_err();
        // Restored so the temporary directory can still clean itself up.
        std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restore the probe directory");
        refused
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn collection_stops_when_nothing_removable_is_left() {
        use std::os::unix::fs::PermissionsExt as _;

        if !a_read_only_directory_refuses_this_process() {
            eprintln!(
                "skipped: this process can remove files from a read-only directory, \
                 so a refusal cannot be staged for it to report"
            );
            return;
        }

        let fixture = Fixture::new();
        let kept = fixture.publish(ContentStore::Downloads, 0x10, 100, hour(1));
        // A shard directory with no write permission cannot have entries
        // unlinked from it, which stands in for anything that refuses removal.
        let shard = kept.parent().expect("shard").to_path_buf();
        std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o500))
            .expect("read-only shard");

        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::Limited(10))
            .await
            .expect("collection reports trouble rather than failing");

        // Restored before the assertions, so a failing one still leaves a
        // directory the temporary root can remove.
        std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o700))
            .expect("restore the shard");

        assert_eq!(report.removed_entries, 0);
        assert_eq!(
            report.usage_after, 100,
            "an over-budget cache stays over budget"
        );
        assert_eq!(report.troubled_entries, 1);
        assert_eq!(report.troubles[0].stage, MaintenanceStage::Removing);
        assert!(!report.is_clean());
        assert!(kept.is_file());
    }

    #[tokio::test]
    async fn a_held_lease_blocks_collection_instead_of_racing_it() {
        let fixture = Fixture::new();
        let path = fixture.publish(ContentStore::Downloads, 0x12, 4_000, hour(3));

        let lease = fixture.cache.lease().await.expect("lease");
        let error = fixture
            .cache
            .collect(ArtifactCacheLimit::Limited(MIB))
            .await
            .expect_err("collection must not run while an operation holds a lease");
        assert!(matches!(error, ArtifactCacheError::Busy { .. }));
        assert_eq!(error.message_id(), "error-artifact-cache-busy");
        assert!(path.is_file(), "a leased artifact is still there");

        let error = fixture
            .cache
            .clear()
            .await
            .expect_err("a manual clear is refused the same way");
        assert!(matches!(error, ArtifactCacheError::Busy { .. }));

        drop(lease);
        let report = fixture
            .cache
            .collect(ArtifactCacheLimit::Limited(MIB))
            .await
            .expect("collection runs once the lease is gone");
        assert_eq!(report.removed_entries, 0, "4 KiB is well inside 1 MiB");
        assert_eq!(report.usage_before, 4_000);
    }

    #[tokio::test]
    async fn several_operations_hold_leases_at_once() {
        let fixture = Fixture::new();
        let first = fixture.cache.lease().await.expect("first lease");
        let second = fixture.cache.lease().await.expect("second lease");
        let third = fixture.cache.lease().await.expect("third lease");
        assert!(
            fixture.cache.usage().await.is_ok(),
            "measuring shares the lock with the operations it measures around"
        );
        drop((first, second));
        assert!(matches!(
            fixture.cache.clear().await,
            Err(ArtifactCacheError::Busy { .. })
        ));
        drop(third);
        assert!(fixture.cache.clear().await.is_ok());
    }

    #[tokio::test]
    async fn a_clear_reports_the_remaining_usage_truthfully() {
        let fixture = Fixture::new();
        fixture.publish(ContentStore::Downloads, 0x13, 1_500, hour(2));
        fixture.publish(ContentStore::InstallerCache, 0x14, 2_500, hour(1));

        let report = fixture.cache.clear().await.expect("clear");
        assert_eq!(report.usage_before, 4_000);
        assert_eq!(report.usage_after, 0);
        assert_eq!(report.removed_entries, 2);
        assert_eq!(report.entries_after, 0);
        assert!(report.is_clean());
        assert_eq!(fixture.cache.usage().await.expect("usage").bytes, 0);
    }

    #[tokio::test]
    async fn an_emptied_shard_directory_is_pruned_and_its_store_root_is_not() {
        let fixture = Fixture::new();
        let path = fixture.publish(ContentStore::Downloads, 0x15, 64, hour(1));
        let shard = path.parent().expect("shard").to_path_buf();
        let algorithm = shard.parent().expect("algorithm").to_path_buf();

        fixture.cache.clear().await.expect("clear");
        assert!(!shard.exists(), "an empty shard is tidied away");
        assert!(algorithm.is_dir(), "the store hierarchy itself stays");
        assert!(
            fixture
                .layout()
                .store_root(ContentStore::Downloads)
                .is_dir()
        );
    }
}
