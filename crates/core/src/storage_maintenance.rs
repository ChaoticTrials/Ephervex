//! Safe maintenance of the shared, regenerable stores.
//!
//! The stores under the cache root hold what a launch downloads or derives:
//! game jars and libraries, assets, unpacked natives, pack files, and loader
//! installers (ADR 0011). All of it can be fetched or rebuilt, and none of it
//! belongs to an instance or a server. This module inspects those stores,
//! checks them for damage, recovers damaged entries through the ordinary
//! acquisition path, and removes what no installed host needs, after a preview
//! the user confirms (ADR 0053).
//!
//! # Unused means nothing installed names it
//!
//! An entry is unused only when the stores' recorded references say so. Every
//! client and server, persistent or temporary, contributes: a client's game
//! version resolves, from the version metadata already on disk, to the client
//! jar, libraries, natives, logging configuration, asset index, and every asset
//! object that index lists. The selected host is never the only source of truth,
//! and age is never evidence of anything.
//!
//! What a host needs cannot always be known, and an entry is then kept rather
//! than judged:
//!
//! * a Forge or NeoForge host also needs libraries and an installer that
//!   Ephervex does not record per host, so libraries and installer files are
//!   kept while one exists;
//! * a host whose version metadata or asset index is present but unreadable
//!   leaves everything undecided;
//! * a cached file that is also a hard link inside a host would free no space,
//!   so it is kept and left to the host.
//!
//! # A cleanup is a plan, then a confirmed application
//!
//! [`CleanupPlan`] can only be made by [`App::plan_storage_cleanup`], and
//! [`App::apply_storage_cleanup`] accepts nothing else, so a frontend cannot
//! delete without having shown a plan. Applying takes the cache lock
//! exclusively, which waits for no operation: a launch, an installation, or a
//! running game holds a lease and makes it refuse. It then decides again, from
//! the references as they are at that moment, and removes only entries that are
//! in the plan and still unused. Whatever it did not remove is reported with the
//! reason, and bytes are credited only for entries that are really gone.
//!
//! Nothing here touches instance or server directories, mods, configuration,
//! worlds, snapshots, or recovery state: no stretch of storage outside the shared
//! stores can be named by a plan.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_download::{
    CacheLayout, ContentStore, DownloadManager, EntryAddress, EntryRemoval, OperationReporter,
    StoreEntry, StoreInventory,
};
use ephervex_minecraft::{
    AssetIndex, InstallationPlan, LaunchError, LaunchFeatures, PlannedArtifact, Platform,
    VanillaInstaller,
};
use ephervex_model::{Hash, LoaderKind, Progress};
use ephervex_pack::PackError;
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::app::App;
use crate::disk_usage::StorageCategory;
use crate::error::{CoreError, CoreResult, ErrorChain};
use crate::verify::{Integrity, check};

/// How many individual failures an outcome names. The count is always exact.
const REPORTED_FAILURES: usize = 20;
/// How many of a plan item's largest entries are named for the preview.
const EXAMPLES_PER_ITEM: usize = 5;

/// A kind of managed entry: the unit a report and a plan are grouped by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ManagedStorageKind {
    /// Files in the download store: game jars, natives jars, logging
    /// configuration, pack files, and runtime archives.
    DownloadedFiles,
    /// Loader installers.
    InstallerFiles,
    /// Mojang asset objects.
    AssetObjects,
    /// The launch view of each asset index.
    AssetIndexes,
    /// Minecraft and loader libraries.
    Libraries,
    /// Natives unpacked for one version and platform.
    UnpackedNatives,
}

impl ManagedStorageKind {
    /// Every kind, in the order a report lists them.
    pub const ALL: [Self; 6] = [
        Self::DownloadedFiles,
        Self::InstallerFiles,
        Self::AssetObjects,
        Self::AssetIndexes,
        Self::Libraries,
        Self::UnpackedNatives,
    ];

    /// A stable identifier for frontends and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DownloadedFiles => "downloaded_files",
            Self::InstallerFiles => "installer_files",
            Self::AssetObjects => "asset_objects",
            Self::AssetIndexes => "asset_indexes",
            Self::Libraries => "libraries",
            Self::UnpackedNatives => "unpacked_natives",
        }
    }

    /// The Disk Usage category this kind is part of.
    #[must_use]
    pub const fn category(self) -> StorageCategory {
        match self {
            Self::DownloadedFiles | Self::InstallerFiles => StorageCategory::DownloadCache,
            Self::AssetObjects | Self::AssetIndexes | Self::Libraries | Self::UnpackedNatives => {
                StorageCategory::SharedGameFiles
            }
        }
    }

    fn of(entry: &StoreEntry) -> Self {
        match (entry.store, &entry.address) {
            (ContentStore::InstallerCache, _) => Self::InstallerFiles,
            (ContentStore::Assets, EntryAddress::AssetIndex { .. }) => Self::AssetIndexes,
            (ContentStore::Assets, _) => Self::AssetObjects,
            (ContentStore::Libraries, _) => Self::Libraries,
            (ContentStore::Natives, _) => Self::UnpackedNatives,
            _ => Self::DownloadedFiles,
        }
    }
}

/// Why an entry no installed host needs is still kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WithheldReason {
    /// A host that runs a loader needs libraries and an installer that are not
    /// recorded per host, so none can be told apart from the ones it needs.
    LoaderFilesNotRecorded,
    /// A host's game version or asset index is present and cannot be read, so
    /// what it needs is unknown.
    HostReferencesUnknown,
    /// The file is also a hard link inside a host, so removing it from the
    /// store would free no space.
    LinkedIntoHost,
    /// The user supplied the file by hand because its provider does not allow
    /// downloading it, so it cannot be fetched again once removed.
    SuppliedByHand,
}

/// A count of entries and what they weigh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub entries: u64,
    pub files: u64,
    pub bytes: u64,
}

impl Usage {
    fn add(&mut self, entry: &StoreEntry) {
        self.entries += 1;
        self.files = self.files.saturating_add(entry.files);
        self.bytes = self.bytes.saturating_add(entry.bytes);
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries == 0
    }
}

/// What one kind of managed entry holds, split by whether anything needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedStorageGroup {
    pub kind: ManagedStorageKind,
    /// Named by an installed client's game version.
    pub required: Usage,
    /// Named by nothing recorded. This is what a cleanup removes.
    pub unused: Usage,
    /// Not needed as far as is recorded, and kept anyway, by reason.
    pub withheld: Vec<(WithheldReason, Usage)>,
}

impl ManagedStorageGroup {
    /// Everything this kind holds.
    #[must_use]
    pub fn total(&self) -> Usage {
        let mut total = Usage {
            entries: self.required.entries + self.unused.entries,
            files: self.required.files.saturating_add(self.unused.files),
            bytes: self.required.bytes.saturating_add(self.unused.bytes),
        };
        for (_, usage) in &self.withheld {
            total.entries += usage.entries;
            total.files = total.files.saturating_add(usage.files);
            total.bytes = total.bytes.saturating_add(usage.bytes);
        }
        total
    }
}

/// What the shared stores hold and what could be reclaimed from them.
///
/// Every entry of every store is in exactly one group and one bucket. Storage
/// outside the shared stores, which is all of it that belongs to a host, is
/// never listed here and never offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageInspection {
    /// Every kind, in [`ManagedStorageKind::ALL`] order.
    pub groups: Vec<ManagedStorageGroup>,
    /// Installed clients and servers whose needs were taken into account.
    pub clients: usize,
    pub servers: usize,
    /// Whether every directory of the stores could be read. A `false` makes
    /// every figure a lower bound.
    pub complete: bool,
    pub inspected_at: DateTime<Utc>,
}

impl StorageInspection {
    /// What a cleanup could free right now.
    #[must_use]
    pub fn reclaimable_bytes(&self) -> u64 {
        self.groups
            .iter()
            .map(|group| group.unused.bytes)
            .fold(0, u64::saturating_add)
    }

    /// One kind's group.
    #[must_use]
    pub fn group(&self, kind: ManagedStorageKind) -> Option<&ManagedStorageGroup> {
        self.groups.iter().find(|group| group.kind == kind)
    }
}

/// What a cleanup of one kind would remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupItem {
    pub kind: ManagedStorageKind,
    pub usage: Usage,
    /// The largest of its entries, relative to the cache root.
    pub largest: Vec<PathBuf>,
}

/// An entry the plan leaves in place, with why, for the preview to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptStorage {
    pub kind: ManagedStorageKind,
    pub reason: KeptReason,
    pub usage: Usage,
}

/// Why an entry is not in a cleanup plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptReason {
    /// An installed client's game version names it.
    Required,
    /// Not named, and withheld.
    Withheld(WithheldReason),
}

/// What a cleanup would remove, and what it would leave.
///
/// The only way to apply a cleanup. It carries the exact entries it was made
/// from, and applying removes at most those.
#[derive(Debug, Clone)]
pub struct CleanupPlan {
    items: Vec<CleanupItem>,
    kept: Vec<KeptStorage>,
    complete: bool,
    planned_at: DateTime<Utc>,
    entries: Vec<StoreEntry>,
    cache_root: PathBuf,
}

impl CleanupPlan {
    /// What would be removed, by kind, in [`ManagedStorageKind::ALL`] order.
    #[must_use]
    pub fn items(&self) -> &[CleanupItem] {
        &self.items
    }

    /// What the plan leaves in place, with why.
    #[must_use]
    pub fn kept(&self) -> &[KeptStorage] {
        &self.kept
    }

    /// The most a cleanup can free, which it frees only if every entry goes.
    #[must_use]
    pub fn reclaimable_bytes(&self) -> u64 {
        self.items
            .iter()
            .map(|item| item.usage.bytes)
            .fold(0, u64::saturating_add)
    }

    /// How many entries would be removed.
    #[must_use]
    pub fn entries(&self) -> u64 {
        self.items.iter().map(|item| item.usage.entries).sum()
    }

    /// Whether there is nothing to remove.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether the stores could be read in full. A `false` means more may be
    /// removable than the plan shows, never less.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    #[must_use]
    pub const fn planned_at(&self) -> DateTime<Utc> {
        self.planned_at
    }
}

#[cfg(any(test, feature = "test-support"))]
impl CleanupPlan {
    /// A plan with these items and nothing behind them, for a frontend test
    /// that only reviews one.
    pub(crate) fn for_review_tests(
        items: Vec<CleanupItem>,
        kept: Vec<KeptStorage>,
        complete: bool,
    ) -> Self {
        Self {
            items,
            kept,
            complete,
            planned_at: Utc::now(),
            entries: Vec::new(),
            cache_root: PathBuf::new(),
        }
    }
}

/// One entry a cleanup could not remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupFailure {
    pub kind: ManagedStorageKind,
    /// Relative to the cache root.
    pub path: PathBuf,
    pub cause: std::io::ErrorKind,
}

/// What an applied cleanup did.
///
/// Every planned entry is in exactly one of `removed`, `already_gone`,
/// `no_longer_unused`, `failed`, or `not_attempted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupOutcome {
    /// What the plan set out to remove.
    pub planned: Usage,
    /// What this cleanup removed. `bytes` is what it really freed: an entry
    /// counts only once every name it had is gone.
    pub removed: Usage,
    /// Entries something else had already removed.
    pub already_gone: u64,
    /// Entries not removed because the references, read again when applying,
    /// no longer showed them unused, or they had changed since the plan.
    pub no_longer_unused: u64,
    /// Entries that could not be removed. They are still on disk.
    pub failed_entries: u64,
    pub failed_bytes: u64,
    /// A bounded sample of the failures.
    pub failures: Vec<CleanupFailure>,
    /// Entries not reached because the cleanup was canceled.
    pub not_attempted: u64,
    pub canceled: bool,
}

impl CleanupOutcome {
    /// Whether the plan was carried out in full: everything it named is gone or
    /// was kept for a stated reason.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.failed_entries == 0 && self.not_attempted == 0 && !self.canceled
    }

    /// Whether another cleanup could remove more. Always safe to run: it makes
    /// a new plan from what is on disk and in use at that moment.
    #[must_use]
    pub const fn can_retry(&self) -> bool {
        self.failed_entries > 0 || self.not_attempted > 0
    }
}

/// How one checked file turned out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageFaultKind {
    /// An installed version that has been launched needs it and it is not there.
    Missing,
    /// Present, and its bytes are not the ones its name or its metadata state.
    Corrupt,
    /// Present, and could not be read.
    Unreadable,
}

/// One managed file that is not as it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageFault {
    pub kind: ManagedStorageKind,
    /// Relative to the cache root.
    pub path: PathBuf,
    /// What it is, for a person: a library coordinate, an asset's logical name,
    /// or its file name.
    pub label: String,
    pub fault: StorageFaultKind,
    /// Whether anything needs it, which decides what can be done about it.
    pub standing: FaultStanding,
}

impl StorageFault {
    /// Whether an installed client's game version needs the file, so recovery
    /// can fetch it again.
    #[must_use]
    pub const fn is_required(&self) -> bool {
        matches!(self.standing, FaultStanding::Needed)
    }
}

/// What a damaged file is to the rest of the storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultStanding {
    /// An installed client's game version needs it. Recovery fetches it again.
    Needed,
    /// Nothing needs it, so a cleanup removes it.
    Unused,
    /// Nothing recorded needs it and a cleanup keeps it anyway, for the stated
    /// reason, so neither recovery nor a cleanup changes it.
    Kept(WithheldReason),
}

/// Something an integrity check could not cover, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncheckedStorage {
    /// Unpacked natives are derived from their jars, and nothing records what
    /// their files should contain.
    UnpackedNatives,
    /// A loader's libraries come from that loader's own documents, which a
    /// check does not read. Installer files check themselves.
    LoaderFiles,
    /// A client's game version has no metadata cached yet, so what it needs is
    /// not listed.
    VersionMetadataNotCached { version: String },
    /// Library files no installed version's metadata names, so there is no
    /// recorded digest to check them against.
    UnlistedLibraries { files: u64 },
}

/// What an integrity check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// How many files were read and compared.
    pub checked: u64,
    pub faults: Vec<StorageFault>,
    pub unchecked: Vec<UncheckedStorage>,
    /// Whether every directory of the stores could be read.
    pub complete: bool,
    pub verified_at: DateTime<Utc>,
}

impl IntegrityReport {
    /// Whether nothing checked is wrong.
    #[must_use]
    pub fn is_sound(&self) -> bool {
        self.faults.is_empty()
    }

    /// Faults an installed version needs, which recovery can fetch again.
    pub fn recoverable(&self) -> impl Iterator<Item = &StorageFault> {
        self.faults.iter().filter(|fault| fault.is_required())
    }
}

/// A version whose damaged files could not be fetched again.
#[derive(Debug, Clone)]
pub struct RecoveryFailure {
    pub version: String,
    /// Why the fetch stopped, as any other failure of the same cause is
    /// reported. A provider or network failure is named here, never hidden.
    pub error: std::sync::Arc<CoreError>,
}

/// What a recovery did.
#[derive(Debug, Clone)]
pub struct RecoveryReport {
    /// Faults the check found that are gone now.
    pub recovered: Vec<StorageFault>,
    /// Faults still present after the attempt. They are never presented as
    /// healthy: this is what a check finds afterwards.
    pub still_damaged: Vec<StorageFault>,
    /// Damaged files no installed version needs. Recovery leaves them, and a
    /// cleanup removes them.
    pub left_for_cleanup: Vec<StorageFault>,
    /// Damaged files nothing recorded needs and a cleanup keeps anyway, such as
    /// a loader's installer or a file also linked inside a host. Recovery and
    /// cleanup both leave them in place.
    pub left_in_place: Vec<StorageFault>,
    /// Versions whose files could not be fetched again, and why.
    pub failures: Vec<RecoveryFailure>,
    pub canceled: bool,
    /// The check made after the attempt.
    pub after: IntegrityReport,
}

// ----------------------------------------------------------------- references

/// What one installed client's game version needs from the stores.
struct VersionReferences {
    version: String,
    plan: InstallationPlan,
    index: IndexState,
    /// Whether some client of this version has been launched, which is what
    /// makes an absent artifact damage rather than something not fetched yet.
    launched: bool,
}

enum IndexState {
    /// No canonical index on disk: nothing was fetched for the version.
    NotOnDisk,
    Read(AssetIndex),
    /// On disk and not what its metadata states, or not an index.
    Damaged,
}

/// What every installed host needs, as far as that is recorded.
struct HostReferences {
    versions: Vec<VersionReferences>,
    /// Versions of installed clients with no metadata cached.
    uncached: Vec<String>,
    clients: usize,
    servers: usize,
    /// Hosts that run a loader.
    loader_hosts: usize,
    /// Hosts whose game version or asset index is present and unreadable.
    unreadable_hosts: usize,
}

struct References {
    files: HashSet<PathBuf>,
    directories: HashSet<PathBuf>,
}

impl HostReferences {
    fn references(&self, layout: &CacheLayout) -> References {
        let mut references = References {
            files: HashSet::new(),
            directories: HashSet::new(),
        };
        for version in &self.versions {
            let stored = version.plan.stored_paths();
            references.files.extend(stored.files);
            references.directories.extend(stored.directories);
            if let IndexState::Read(index) = &version.index {
                match index.object_paths(layout) {
                    Ok(paths) => references.files.extend(paths),
                    Err(error) => warn!(%error, "an asset index names an unusable object"),
                }
            }
        }
        references
    }
}

/// Whether nothing at all is at `path` now, not even a link.
fn is_gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// What a decision about one entry came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judgement {
    Required,
    Unused,
    Withheld(WithheldReason),
}

fn judge(entry: &StoreEntry, host: &HostReferences, references: &References) -> Judgement {
    let named = entry
        .paths
        .iter()
        .any(|path| references.files.contains(path) || references.directories.contains(path));
    if named {
        return Judgement::Required;
    }
    if entry.supplied_by_hand {
        return Judgement::Withheld(WithheldReason::SuppliedByHand);
    }
    if host.unreadable_hosts > 0 {
        return Judgement::Withheld(WithheldReason::HostReferencesUnknown);
    }
    let loader_files = matches!(
        ManagedStorageKind::of(entry),
        ManagedStorageKind::Libraries | ManagedStorageKind::InstallerFiles
    );
    if host.loader_hosts > 0 && loader_files {
        return Judgement::Withheld(WithheldReason::LoaderFilesNotRecorded);
    }
    if entry.linked_outside_store {
        return Judgement::Withheld(WithheldReason::LinkedIntoHost);
    }
    Judgement::Unused
}

/// Where a path is, relative to the cache root, for showing.
fn shown(cache_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(cache_root)
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

impl App {
    /// Reads what every installed host needs from the stores.
    ///
    /// Each host is read from its canonical metadata, not from its index row,
    /// which can lag behind a committed change. A host whose record cannot be
    /// read leaves what it needs unknown.
    async fn host_references(&self) -> CoreResult<HostReferences> {
        let mut unreadable_hosts = 0;
        let mut clients = Vec::new();
        for row in self.list_clients()? {
            match self.current_client(row.id) {
                Ok(client) => clients.push(client),
                Err(error) => {
                    warn!(%error, "a client's record could not be read");
                    unreadable_hosts += 1;
                }
            }
        }
        let mut loader_hosts = 0;
        let mut servers = 0;
        for row in self.list_servers()? {
            match self.current_server(row.id) {
                Ok(server) => {
                    servers += 1;
                    loader_hosts += usize::from(server.loader.kind != LoaderKind::Vanilla);
                }
                Err(error) => {
                    warn!(%error, "a server's record could not be read");
                    unreadable_hosts += 1;
                }
            }
        }
        let mut references = HostReferences {
            versions: Vec::new(),
            uncached: Vec::new(),
            clients: clients.len(),
            servers,
            loader_hosts,
            unreadable_hosts,
        };
        let layout = CacheLayout::new(self.paths().cache_dir());
        let context = LaunchFeatures::default().rule_context(Platform::host());

        // Each version is read once, however many clients run it. `launched`
        // is true when any of them has been.
        let mut versions: BTreeMap<String, (bool, usize)> = BTreeMap::new();
        for client in &clients {
            if client.loader.kind != LoaderKind::Vanilla {
                references.loader_hosts += 1;
            }
            let slot = versions
                .entry(client.minecraft_version.clone())
                .or_insert((false, 0));
            slot.0 |= client.last_launched_at.is_some();
            slot.1 += 1;
        }
        for (version, (launched, hosts)) in versions {
            let metadata = match self.minecraft()?.cached_metadata(&version).await {
                Ok(Some(metadata)) => metadata,
                Ok(None) => {
                    // A launched version fetched files, so that its metadata
                    // is not found now (a republished document, a file that
                    // cannot be opened) leaves what it needs unknown.
                    if launched {
                        references.unreadable_hosts += hosts;
                    }
                    references.uncached.push(version);
                    continue;
                }
                Err(error) => {
                    warn!(%error, %version, "a game version's metadata could not be read");
                    references.unreadable_hosts += hosts;
                    continue;
                }
            };
            let plan = match InstallationPlan::resolve(&metadata, &context, &layout) {
                Ok(plan) => plan,
                Err(error) => {
                    warn!(%error, %version, "a game version could not be planned");
                    references.unreadable_hosts += hosts;
                    continue;
                }
            };
            let index = read_asset_index(&plan).await;
            // An index that is gone for a launched version leaves its asset
            // objects unlisted, which is not the same as not needing any.
            if matches!(index, IndexState::Damaged)
                || (launched && matches!(index, IndexState::NotOnDisk))
            {
                references.unreadable_hosts += hosts;
            }
            references.versions.push(VersionReferences {
                version,
                plan,
                index,
                launched,
            });
        }
        Ok(references)
    }

    /// Lists the shared stores and judges every entry against what the
    /// installed hosts need.
    async fn survey_managed_storage(
        &self,
        cancel: &CancellationToken,
    ) -> CoreResult<(StoreInventory, HostReferences, Vec<Judgement>)> {
        let inventory = self.artifact_cache().inventory(cancel).await?;
        if inventory.canceled {
            return Err(PackError::Canceled.into());
        }
        let host = self.host_references().await?;
        let references = host.references(self.artifact_cache().layout());
        let judgements = inventory
            .entries
            .iter()
            .map(|entry| judge(entry, &host, &references))
            .collect();
        Ok((inventory, host, judgements))
    }

    /// Lists what the shared stores hold, by kind, and what no installed host
    /// needs.
    ///
    /// Reads the file system and the host records and changes nothing. It takes
    /// the shared side of the cache lock, so it never sees a store halfway
    /// through a collection.
    #[instrument(skip(self, cancel))]
    pub async fn inspect_managed_storage(
        &self,
        cancel: &CancellationToken,
    ) -> CoreResult<StorageInspection> {
        let (inventory, host, judgements) = self.survey_managed_storage(cancel).await?;
        let mut groups: Vec<ManagedStorageGroup> = ManagedStorageKind::ALL
            .into_iter()
            .map(|kind| ManagedStorageGroup {
                kind,
                required: Usage::default(),
                unused: Usage::default(),
                withheld: Vec::new(),
            })
            .collect();
        for (entry, judgement) in inventory.entries.iter().zip(&judgements) {
            let kind = ManagedStorageKind::of(entry);
            let Some(group) = groups.iter_mut().find(|group| group.kind == kind) else {
                continue;
            };
            match judgement {
                Judgement::Required => group.required.add(entry),
                Judgement::Unused => group.unused.add(entry),
                Judgement::Withheld(reason) => {
                    if let Some((_, usage)) =
                        group.withheld.iter_mut().find(|(have, _)| have == reason)
                    {
                        usage.add(entry);
                    } else {
                        let mut usage = Usage::default();
                        usage.add(entry);
                        group.withheld.push((*reason, usage));
                    }
                }
            }
        }
        for group in &mut groups {
            group.withheld.sort_by_key(|(reason, _)| *reason);
        }
        let inspection = StorageInspection {
            groups,
            clients: host.clients,
            servers: host.servers,
            complete: inventory.complete,
            inspected_at: Utc::now(),
        };
        info!(
            reclaimable = inspection.reclaimable_bytes(),
            complete = inspection.complete,
            "inspected the shared stores"
        );
        Ok(inspection)
    }

    /// Plans removing what no installed host needs, limited to `kinds`.
    ///
    /// Changes nothing. The plan is what to show before asking for
    /// confirmation, and the only thing [`Self::apply_storage_cleanup`] takes.
    #[instrument(skip(self, cancel))]
    pub async fn plan_storage_cleanup(
        &self,
        kinds: &[ManagedStorageKind],
        cancel: &CancellationToken,
    ) -> CoreResult<CleanupPlan> {
        let (inventory, _host, judgements) = self.survey_managed_storage(cancel).await?;
        let cache_root = self.paths().cache_dir().to_path_buf();
        let mut entries = Vec::new();
        let mut items: BTreeMap<ManagedStorageKind, (Usage, Vec<&StoreEntry>)> = BTreeMap::new();
        let mut kept: BTreeMap<(ManagedStorageKind, u8), KeptStorage> = BTreeMap::new();
        for (entry, judgement) in inventory.entries.iter().zip(&judgements) {
            let kind = ManagedStorageKind::of(entry);
            match judgement {
                Judgement::Unused if kinds.contains(&kind) => {
                    let (usage, members) = items.entry(kind).or_default();
                    usage.add(entry);
                    members.push(entry);
                    entries.push(entry.clone());
                }
                Judgement::Unused => {}
                Judgement::Required | Judgement::Withheld(_) => {
                    let reason = match judgement {
                        Judgement::Withheld(reason) => KeptReason::Withheld(*reason),
                        _ => KeptReason::Required,
                    };
                    let slot = match reason {
                        KeptReason::Required => 0,
                        KeptReason::Withheld(WithheldReason::LoaderFilesNotRecorded) => 1,
                        KeptReason::Withheld(WithheldReason::HostReferencesUnknown) => 2,
                        KeptReason::Withheld(WithheldReason::LinkedIntoHost) => 3,
                        KeptReason::Withheld(WithheldReason::SuppliedByHand) => 4,
                    };
                    kept.entry((kind, slot))
                        .or_insert_with(|| KeptStorage {
                            kind,
                            reason,
                            usage: Usage::default(),
                        })
                        .usage
                        .add(entry);
                }
            }
        }
        let items = items
            .into_iter()
            .map(|(kind, (usage, mut members))| {
                members.sort_by(|left, right| {
                    right
                        .bytes
                        .cmp(&left.bytes)
                        .then_with(|| left.paths.cmp(&right.paths))
                });
                CleanupItem {
                    kind,
                    usage,
                    largest: members
                        .iter()
                        .take(EXAMPLES_PER_ITEM)
                        .map(|entry| shown(&cache_root, &entry.paths[0]))
                        .collect(),
                }
            })
            .collect();
        Ok(CleanupPlan {
            items,
            kept: kept.into_values().collect(),
            complete: inventory.complete,
            planned_at: Utc::now(),
            entries,
            cache_root,
        })
    }

    /// Removes what a confirmed plan listed, as far as it is still unused.
    ///
    /// Refuses with the cache-busy error, and removes nothing, while an
    /// operation holds the cache: a launch, an installation, or a running game.
    /// Decides again from the references as they are now, and removes only
    /// entries that are in the plan and are still unused. A partial result says
    /// what was removed, what was not and why, and counts freed bytes only for
    /// entries that are gone.
    #[instrument(skip(self, plan, cancel))]
    pub async fn apply_storage_cleanup(
        &self,
        plan: &CleanupPlan,
        cancel: &CancellationToken,
    ) -> CoreResult<CleanupOutcome> {
        let mut outcome = CleanupOutcome::default();
        for entry in &plan.entries {
            outcome.planned.add(entry);
        }
        if plan.entries.is_empty() {
            return Ok(outcome);
        }

        let exclusive = self.artifact_cache().exclusive().await?;
        let inventory = exclusive.inventory(cancel).await;
        if inventory.canceled {
            return Err(PackError::Canceled.into());
        }
        let host = self.host_references().await?;
        let references = host.references(self.artifact_cache().layout());
        let unused: HashMap<&PathBuf, &StoreEntry> = inventory
            .entries
            .iter()
            .filter(|entry| judge(entry, &host, &references) == Judgement::Unused)
            .map(|entry| (&entry.paths[0], entry))
            .collect();

        // The plan's entries, kept only where an identical unused entry is
        // still there: same names, so a path that now holds something else is
        // not removed on the strength of an old plan. An entry whose names are
        // all gone was removed by someone else.
        let mut to_remove: Vec<StoreEntry> = Vec::new();
        for planned in &plan.entries {
            match unused.get(&planned.paths[0]) {
                Some(current) if current.paths == planned.paths => {
                    to_remove.push((*current).clone());
                }
                _ if planned.paths.iter().all(|path| is_gone(path)) => {
                    outcome.already_gone += 1;
                }
                _ => outcome.no_longer_unused += 1,
            }
        }

        let removals = exclusive.remove(to_remove.clone(), cancel).await;
        for (entry, removal) in to_remove.iter().zip(removals) {
            match removal {
                EntryRemoval::Removed => outcome.removed.add(entry),
                EntryRemoval::AlreadyGone => outcome.already_gone += 1,
                EntryRemoval::NotAttempted => outcome.not_attempted += 1,
                EntryRemoval::Failed(trouble) => {
                    outcome.failed_entries += 1;
                    outcome.failed_bytes = outcome.failed_bytes.saturating_add(entry.bytes);
                    if outcome.failures.len() < REPORTED_FAILURES {
                        outcome.failures.push(CleanupFailure {
                            kind: ManagedStorageKind::of(entry),
                            path: shown(&plan.cache_root, &trouble.path),
                            cause: trouble.cause,
                        });
                    }
                }
            }
        }
        outcome.canceled = cancel.is_cancelled() && outcome.not_attempted > 0;
        info!(
            planned = outcome.planned.bytes,
            freed = outcome.removed.bytes,
            failed = outcome.failed_entries,
            skipped = outcome.no_longer_unused,
            "applied a storage cleanup"
        );
        drop(exclusive);
        Ok(outcome)
    }

    /// Checks the managed entries that can be checked.
    ///
    /// Three kinds of evidence, and nothing else is claimed. Entries named by
    /// their digest (downloads, installers, asset objects) are read and compared
    /// with it. The files an installed client's game version lists (client jar,
    /// libraries, natives jars, logging configuration, asset index and objects)
    /// are compared with the digest that version's metadata states. A file that
    /// metadata says a launched version needs and that is absent is missing; one
    /// of a version never launched is not, because nothing has fetched it yet.
    ///
    /// Holds a shared lease for its whole run, so a cleanup cannot remove a
    /// file while it is being read. Reports progress, and stops when `cancel`
    /// fires.
    #[instrument(skip(self, reporter, cancel))]
    pub async fn verify_managed_storage(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<IntegrityReport> {
        let lease = self.artifact_cache_lease().await?;
        let host = self.host_references().await?;
        let report = self
            .check_managed_storage(&host, &CheckScope::everything(), reporter, cancel)
            .await;
        drop(lease);
        report
    }

    async fn check_managed_storage(
        &self,
        host: &HostReferences,
        scope: &CheckScope,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<IntegrityReport> {
        let layout = self.artifact_cache().layout().clone();
        let cache_root = self.paths().cache_dir().to_path_buf();
        let inventory = self.artifact_cache().inventory(cancel).await?;
        if inventory.canceled {
            return Err(PackError::Canceled.into());
        }
        let references = host.references(&layout);

        let mut work = checks_to_run(host, &inventory, &layout);
        if scope.only {
            work.retain(|item| scope.paths.contains(&item.path));
        }
        for item in &mut work {
            if scope.paths.contains(&item.path) {
                item.absent_is_fault = true;
            }
        }
        let judgements: HashMap<&PathBuf, Judgement> = inventory
            .entries
            .iter()
            .flat_map(|entry| {
                let judgement = judge(entry, host, &references);
                entry.paths.iter().map(move |path| (path, judgement))
            })
            .collect();

        let total = u64::try_from(work.len()).unwrap_or(u64::MAX);
        reporter.set_items(Progress {
            current: 0,
            total: Some(total),
        });
        let mut faults = Vec::new();
        let mut checked = 0u64;
        for (index, item) in work.into_iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(PackError::Canceled.into());
            }
            let (read, fault) = match check(&item.path, std::slice::from_ref(&item.expected)).await
            {
                Integrity::Intact => (true, None),
                Integrity::Absent if item.absent_is_fault => {
                    (false, Some(StorageFaultKind::Missing))
                }
                Integrity::Corrupt => (true, Some(StorageFaultKind::Corrupt)),
                Integrity::Unreadable => (true, Some(StorageFaultKind::Unreadable)),
                Integrity::Absent | Integrity::PutAside | Integrity::Unverifiable => (false, None),
            };
            checked += u64::from(read);
            if let Some(fault) = fault {
                faults.push(StorageFault {
                    kind: item.kind,
                    standing: if references.files.contains(&item.path) {
                        FaultStanding::Needed
                    } else {
                        match judgements.get(&item.path) {
                            Some(Judgement::Withheld(reason)) => FaultStanding::Kept(*reason),
                            Some(Judgement::Required) => FaultStanding::Needed,
                            _ => FaultStanding::Unused,
                        }
                    },
                    path: shown(&cache_root, &item.path),
                    label: item.label.clone(),
                    fault,
                });
            }
            reporter.set_items_at(
                Progress {
                    current: u64::try_from(index + 1).unwrap_or(u64::MAX),
                    total: Some(total),
                },
                Some(item.label),
            );
        }
        faults.sort_by(|left, right| left.path.cmp(&right.path));

        let unchecked = unchecked_notes(host, &inventory, &references);

        info!(
            checked,
            faults = faults.len(),
            "checked the shared stores against their digests"
        );
        Ok(IntegrityReport {
            checked,
            faults,
            unchecked,
            complete: inventory.complete && !scope.only,
            verified_at: Utc::now(),
        })
    }

    /// Fetches again the damaged files an installed game version needs.
    ///
    /// Checks first, then, for each version with damage, fetches only the
    /// damaged or missing files through the same download path an installation
    /// uses. That path sets aside whatever at a destination does not match its
    /// request and publishes the verified replacement atomically, so nothing
    /// else is removed, and a fetch that fails leaves the file absent rather
    /// than present and wrong. Checks again afterwards: what the report calls
    /// recovered is what that check no longer finds damaged, and a failed
    /// recovery stays listed as damaged. A provider or network failure is
    /// reported with its cause for the version it stopped.
    ///
    /// Holds a shared lease throughout, like any operation that uses the cache.
    #[instrument(skip(self, reporter, cancel))]
    pub async fn recover_managed_storage(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<RecoveryReport> {
        let lease = self.artifact_cache_lease().await?;
        let outcome = self.recover_under_lease(reporter, cancel).await;
        drop(lease);
        self.maintain_artifact_cache().await;
        outcome
    }

    async fn recover_under_lease(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<RecoveryReport> {
        let layout = self.artifact_cache().layout().clone();
        let cache_root = self.paths().cache_dir().to_path_buf();
        let host = self.host_references().await?;
        let before = self
            .check_managed_storage(&host, &CheckScope::everything(), reporter, cancel)
            .await?;

        let installer = VanillaInstaller::new(
            DownloadManager::new(self.download_config())
                .map_err(|source| LaunchError::download("the download client", source))?,
            layout.clone(),
        );
        let mut failures = Vec::new();
        let mut canceled = false;
        for version in &host.versions {
            let damaged = damaged_paths_of(version, &before, &cache_root, &layout);
            if damaged.is_empty() {
                continue;
            }
            if cancel.is_cancelled() {
                canceled = true;
                break;
            }
            match installer
                .reacquire(&version.plan, &damaged, reporter, cancel)
                .await
            {
                Ok(fetched) => {
                    info!(version = %version.version, fetched, "fetched damaged files again");
                }
                Err(LaunchError::Canceled) => {
                    canceled = true;
                    break;
                }
                Err(error) => {
                    let error = CoreError::from(error);
                    warn!(version = %version.version, error = %ErrorChain(&error), "recovery failed");
                    failures.push(RecoveryFailure {
                        version: version.version.clone(),
                        error: std::sync::Arc::new(error),
                    });
                }
            }
        }

        // Re-read what hosts need: a recovered asset index can list objects the
        // damaged one could not. A file that was damaged counts as recovered
        // only if it is intact now: one set aside and not fetched is missing.
        // After a stop, only those files are read again, so Stop does not turn
        // into hashing the whole store.
        let host = self.host_references().await?;
        let mut scope = CheckScope {
            only: canceled,
            paths: before
                .recoverable()
                .map(|fault| cache_root.join(&fault.path))
                .collect(),
        };
        let first = if canceled {
            self.check_managed_storage(&host, &scope, reporter, &CancellationToken::new())
                .await
        } else {
            self.check_managed_storage(&host, &scope, reporter, cancel)
                .await
        };
        let after = match first {
            Ok(after) => after,
            // A stop during the full re-check falls back to reading only the
            // files that were being fixed.
            Err(error) if error.is_cancellation() => {
                canceled = true;
                scope.only = true;
                self.check_managed_storage(&host, &scope, reporter, &CancellationToken::new())
                    .await?
            }
            Err(error) => return Err(error),
        };
        let still: BTreeSet<&PathBuf> = after.faults.iter().map(|fault| &fault.path).collect();
        let mut recovered = Vec::new();
        let mut still_damaged = Vec::new();
        let mut left_for_cleanup = Vec::new();
        let mut left_in_place = Vec::new();
        for fault in &before.faults {
            match fault.standing {
                FaultStanding::Unused => left_for_cleanup.push(fault.clone()),
                FaultStanding::Kept(_) => left_in_place.push(fault.clone()),
                FaultStanding::Needed if still.contains(&fault.path) => {
                    still_damaged.push(fault.clone());
                }
                FaultStanding::Needed => recovered.push(fault.clone()),
            }
        }
        Ok(RecoveryReport {
            recovered,
            still_damaged,
            left_for_cleanup,
            left_in_place,
            failures,
            canceled,
            after,
        })
    }
}

/// Every file there is evidence to check, each once.
fn checks_to_run(
    host: &HostReferences,
    inventory: &StoreInventory,
    layout: &CacheLayout,
) -> Vec<Check> {
    let mut work: Vec<Check> = Vec::new();
    let mut queued: HashSet<PathBuf> = HashSet::new();
    for version in &host.versions {
        for (artifact, kind) in planned_artifacts(&version.plan) {
            let expected = artifact.artifact.sha1.clone();
            for path in [Some(&artifact.path), launch_view(version, kind, artifact)]
                .into_iter()
                .flatten()
            {
                if queued.insert(path.clone()) {
                    work.push(Check {
                        kind,
                        path: path.clone(),
                        label: artifact.label.clone(),
                        expected: expected.clone(),
                        absent_is_fault: version.launched,
                    });
                }
            }
        }
        if let IndexState::Read(index) = &version.index {
            for (name, object) in index.distinct_objects() {
                let Ok(path) = layout.asset_object(&object.hash) else {
                    continue;
                };
                if queued.insert(path.clone()) {
                    work.push(Check {
                        kind: ManagedStorageKind::AssetObjects,
                        path,
                        label: name.to_owned(),
                        expected: object.hash.clone(),
                        absent_is_fault: version.launched,
                    });
                }
            }
        }
    }
    for entry in &inventory.entries {
        let expected = match &entry.address {
            EntryAddress::Content(address) => address.hash.clone(),
            EntryAddress::AssetObject(hash) => hash.clone(),
            _ => continue,
        };
        let path = entry.paths[0].clone();
        if queued.insert(path.clone()) {
            work.push(Check {
                kind: ManagedStorageKind::of(entry),
                label: path
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                path,
                expected,
                absent_is_fault: false,
            });
        }
    }
    work
}

/// What a check could not cover.
fn unchecked_notes(
    host: &HostReferences,
    inventory: &StoreInventory,
    references: &References,
) -> Vec<UncheckedStorage> {
    let mut unchecked = Vec::new();
    if inventory
        .entries
        .iter()
        .any(|entry| ManagedStorageKind::of(entry) == ManagedStorageKind::UnpackedNatives)
    {
        unchecked.push(UncheckedStorage::UnpackedNatives);
    }
    if host.loader_hosts > 0 {
        unchecked.push(UncheckedStorage::LoaderFiles);
    }
    for version in &host.uncached {
        unchecked.push(UncheckedStorage::VersionMetadataNotCached {
            version: version.clone(),
        });
    }
    let unlisted: u64 = inventory
        .entries
        .iter()
        .filter(|entry| {
            ManagedStorageKind::of(entry) == ManagedStorageKind::Libraries
                && !entry
                    .paths
                    .iter()
                    .any(|path| references.files.contains(path))
        })
        .map(|entry| entry.files)
        .sum();
    if unlisted > 0 {
        unchecked.push(UncheckedStorage::UnlistedLibraries { files: unlisted });
    }
    unchecked
}

/// What a check covers.
struct CheckScope {
    /// Only the files in `paths`, rather than everything there is evidence for.
    only: bool,
    /// Files, as absolute paths, that damaged an earlier check and so count as
    /// missing, not as not fetched yet, when they are absent now.
    paths: HashSet<PathBuf>,
}

impl CheckScope {
    fn everything() -> Self {
        Self {
            only: false,
            paths: HashSet::new(),
        }
    }
}

/// One file to compare with a digest.
struct Check {
    kind: ManagedStorageKind,
    path: PathBuf,
    label: String,
    expected: Hash,
    /// Whether not being there is damage rather than "not fetched yet".
    absent_is_fault: bool,
}

/// The artifacts a version's metadata states a digest for, with their kind.
fn planned_artifacts(plan: &InstallationPlan) -> Vec<(&PlannedArtifact, ManagedStorageKind)> {
    let mut artifacts = vec![
        (&plan.client_jar, ManagedStorageKind::DownloadedFiles),
        (&plan.asset_index, ManagedStorageKind::AssetIndexes),
    ];
    artifacts.extend(
        plan.libraries
            .iter()
            .map(|library| (library, ManagedStorageKind::Libraries)),
    );
    artifacts.extend(
        plan.natives
            .iter()
            .map(|natives| (&natives.artifact, ManagedStorageKind::DownloadedFiles)),
    );
    if let Some(logging) = &plan.logging {
        artifacts.push((&logging.artifact, ManagedStorageKind::DownloadedFiles));
    }
    artifacts
}

/// The launch view of the asset index, a copy of the canonical file, when
/// `artifact` is that index.
fn launch_view<'a>(
    version: &'a VersionReferences,
    kind: ManagedStorageKind,
    artifact: &PlannedArtifact,
) -> Option<&'a PathBuf> {
    (kind == ManagedStorageKind::AssetIndexes && artifact.path == version.plan.asset_index.path)
        .then_some(&version.plan.asset_index_path)
}

/// The damaged files of `before` that one version needs, as absolute paths.
fn damaged_paths_of(
    version: &VersionReferences,
    before: &IntegrityReport,
    cache_root: &Path,
    layout: &CacheLayout,
) -> BTreeSet<PathBuf> {
    let mut needed = version.plan.stored_paths().files;
    if let IndexState::Read(index) = &version.index
        && let Ok(objects) = index.object_paths(layout)
    {
        needed.extend(objects);
    }
    before
        .recoverable()
        .map(|fault| cache_root.join(&fault.path))
        .filter(|path| needed.contains(path))
        .collect()
}

/// Reads the canonical asset index a plan names.
async fn read_asset_index(plan: &InstallationPlan) -> IndexState {
    let json = match tokio::fs::read_to_string(&plan.asset_index.path).await {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return IndexState::NotOnDisk;
        }
        Err(error) => {
            warn!(%error, "an asset index could not be read");
            return IndexState::Damaged;
        }
    };
    // Trusted only when it is the document its metadata describes, so a
    // damaged index cannot name objects nobody asked for.
    if !matches!(
        check(
            &plan.asset_index.path,
            std::slice::from_ref(&plan.asset_index.artifact.sha1)
        )
        .await,
        Integrity::Intact
    ) {
        return IndexState::Damaged;
    }
    AssetIndex::parse(&plan.asset_index_id, &json).map_or(IndexState::Damaged, IndexState::Read)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use ephervex_download::{
        ContentStore, FileExtension, MetadataCache, MetadataKey, record_supplied_download,
    };
    use ephervex_minecraft::{VERSION_MANIFEST_CACHE_KEY, VERSION_MANIFEST_URL};
    use ephervex_model::{HashAlgorithm, LoaderSelection};
    use ephervex_storage::{NewClient, Paths};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn sha1_hex(bytes: &[u8]) -> String {
        use sha1::Digest as _;
        let mut hasher = sha1::Sha1::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    fn sha1_of(bytes: &[u8]) -> Hash {
        Hash::new(HashAlgorithm::Sha1, &sha1_hex(bytes)).expect("sha1")
    }

    /// The bytes one synthetic game version is made of, so every digest in its
    /// metadata is one the files can satisfy.
    struct Synthetic {
        id: &'static str,
        client: Vec<u8>,
        library: Vec<u8>,
        asset: Vec<u8>,
        index: Vec<u8>,
    }

    impl Synthetic {
        fn new(id: &'static str) -> Self {
            let asset = format!("an asset of {id}").into_bytes();
            Self {
                id,
                client: format!("client {id}").into_bytes(),
                library: format!("library of {id}").into_bytes(),
                index: format!(
                    r#"{{"objects":{{"minecraft/sounds/a.ogg":{{"hash":"{}","size":{}}}}}}}"#,
                    sha1_hex(&asset),
                    asset.len()
                )
                .into_bytes(),
                asset,
            }
        }

        fn artifact(server: &str, route: &str, body: &[u8]) -> String {
            format!(
                r#"{{"url":"{server}{route}","sha1":"{}","size":{}}}"#,
                sha1_hex(body),
                body.len()
            )
        }

        fn document(&self, server: &str) -> String {
            format!(
                r#"{{
                  "id": "{id}", "type": "release",
                  "time": "2024-08-08T12:24:45+00:00", "releaseTime": "2024-08-08T12:24:45+00:00",
                  "mainClass": "net.minecraft.client.main.Main",
                  "javaVersion": {{"component":"java-runtime-delta","majorVersion":21}},
                  "assets": "{id}",
                  "assetIndex": {{"id":"{id}","totalSize":1,"url":"{server}/index-{id}.json","sha1":"{index_sha}","size":{index_len}}},
                  "downloads": {{"client": {client}}},
                  "libraries": [
                    {{"name":"org.example:lib-{id}:1.0","downloads":{{"artifact":{library}}}}}
                  ],
                  "arguments": {{"game": ["--username","${{auth_player_name}}"], "jvm": ["-cp","${{classpath}}"]}}
                }}"#,
                id = self.id,
                index_sha = sha1_hex(&self.index),
                index_len = self.index.len(),
                client = Self::artifact(server, &format!("/client-{}.jar", self.id), &self.client),
                library = Self::artifact(server, &format!("/lib-{}.jar", self.id), &self.library),
            )
        }

        async fn serve(&self, server: &MockServer) {
            for (route, body) in [
                (format!("/client-{}.jar", self.id), &self.client),
                (format!("/lib-{}.jar", self.id), &self.library),
                (format!("/index-{}.json", self.id), &self.index),
            ] {
                Mock::given(method("GET"))
                    .and(path(route))
                    .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
                    .mount(server)
                    .await;
            }
        }
    }

    /// Stores Mojang's manifest and these versions' documents as already
    /// fetched, so nothing is looked up from anywhere.
    async fn seed(cache: &Path, server: &str, versions: &[&Synthetic]) {
        let layout = CacheLayout::new(cache);
        let json = FileExtension::new("json").unwrap();
        let mut records = Vec::new();
        for version in versions {
            let document = version.document(server);
            let sha1 = sha1_hex(document.as_bytes());
            records.push(format!(
                r#"{{"id":"{id}","type":"release","url":"{server}/{id}.json","time":"2024-08-08T12:24:45+00:00","releaseTime":"2024-08-08T12:24:45+00:00","sha1":"{sha1}","complianceLevel":1}}"#,
                id = version.id
            ));
            let path = layout.content(
                ContentStore::Metadata,
                &Hash::new(HashAlgorithm::Sha1, &sha1).unwrap(),
                Some(&json),
            );
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, document).unwrap();
        }
        let manifest = format!(
            r#"{{"latest":{{"release":"{first}","snapshot":"{first}"}},"versions":[{}]}}"#,
            records.join(","),
            first = versions[0].id
        );
        MetadataCache::new(layout)
            .store(
                &MetadataKey::new(VERSION_MANIFEST_CACHE_KEY).unwrap(),
                &VERSION_MANIFEST_URL.parse().unwrap(),
                serde_json::value::RawValue::from_string(manifest).unwrap(),
            )
            .await
            .unwrap();
    }

    struct World {
        base: tempfile::TempDir,
        app: App,
        server: MockServer,
        layout: CacheLayout,
    }

    impl World {
        async fn new() -> Self {
            let base = tempfile::tempdir().expect("temp dir");
            let mut app = App::open_at(Paths::rooted_at(base.path())).expect("app");
            app.loopback_http = true;
            let layout = CacheLayout::new(app.paths().cache_dir());
            Self {
                base,
                app,
                server: MockServer::start().await,
                layout,
            }
        }

        async fn with(versions: &[&Synthetic]) -> Self {
            let world = Self::new().await;
            seed(world.app.paths().cache_dir(), &world.server.uri(), versions).await;
            for version in versions {
                version.serve(&world.server).await;
            }
            world
        }

        fn client(&self, version: &str, loader: LoaderSelection) -> ephervex_storage::ClientRecord {
            self.app
                .create_client_for_test(NewClient::new("client", version, loader))
                .expect("client")
        }

        fn launched(&self, client: &ephervex_storage::ClientRecord) {
            self.app
                .commit_client_without_index_for_test(client.id, |record| {
                    record.last_launched_at = Some(Utc::now());
                })
                .expect("commit");
        }

        async fn plan_of(&self, version: &str) -> InstallationPlan {
            let metadata = self
                .app
                .minecraft()
                .unwrap()
                .cached_metadata(version)
                .await
                .unwrap()
                .expect("seeded");
            let context = LaunchFeatures::default().rule_context(Platform::host());
            InstallationPlan::resolve(&metadata, &context, &self.layout).unwrap()
        }

        /// Writes everything `version` needs, intact, and returns where.
        async fn install(&self, version: &Synthetic) -> Installed {
            let plan = self.plan_of(version.id).await;
            let asset = self.layout.asset_object(&sha1_of(&version.asset)).unwrap();
            for (path, body) in [
                (&plan.client_jar.path, &version.client),
                (&plan.libraries[0].path, &version.library),
                (&plan.asset_index.path, &version.index),
                (&plan.asset_index_path, &version.index),
                (&asset, &version.asset),
            ] {
                write(path, body);
            }
            write(&plan.natives_dir.join("liblwjgl.so"), b"natives");
            Installed {
                client: plan.client_jar.path.clone(),
                library: plan.libraries[0].path.clone(),
                index_view: plan.asset_index_path.clone(),
                asset,
                natives: plan.natives_dir,
            }
        }

        fn stray_download(&self, body: &[u8]) -> PathBuf {
            let path = self.layout.content(
                ContentStore::Downloads,
                &sha1_of(body),
                FileExtension::new("jar").ok().as_ref(),
            );
            write(&path, body);
            path
        }

        fn stray_installer(&self, body: &[u8]) -> PathBuf {
            let path = self.layout.content(
                ContentStore::InstallerCache,
                &sha1_of(body),
                FileExtension::new("jar").ok().as_ref(),
            );
            write(&path, body);
            path
        }

        fn stray_library(&self, name: &str, bytes: usize) -> PathBuf {
            let path = self
                .layout
                .store_root(ContentStore::Libraries)
                .join("org/stray")
                .join(name)
                .join("1.0")
                .join(format!("{name}-1.0.jar"));
            write(&path, &vec![b'x'; bytes]);
            path
        }

        fn stray_asset(&self, body: &[u8]) -> PathBuf {
            let path = self.layout.asset_object(&sha1_of(body)).unwrap();
            write(&path, body);
            path
        }

        fn stray_natives(&self) -> PathBuf {
            let path = self.layout.natives("1.19", "linux-x86_64").unwrap();
            write(&path.join("libold.so"), b"old natives");
            path
        }

        async fn inspect(&self) -> StorageInspection {
            self.app
                .inspect_managed_storage(&CancellationToken::new())
                .await
                .expect("inspect")
        }

        async fn plan(&self) -> CleanupPlan {
            self.app
                .plan_storage_cleanup(&ManagedStorageKind::ALL, &CancellationToken::new())
                .await
                .expect("plan")
        }

        async fn apply(&self, plan: &CleanupPlan) -> CoreResult<CleanupOutcome> {
            self.app
                .apply_storage_cleanup(plan, &CancellationToken::new())
                .await
        }

        async fn verify(&self) -> IntegrityReport {
            self.app
                .verify_managed_storage(
                    &OperationReporter::silent("test-verify"),
                    &CancellationToken::new(),
                )
                .await
                .expect("verify")
        }
    }

    struct Installed {
        client: PathBuf,
        library: PathBuf,
        index_view: PathBuf,
        asset: PathBuf,
        natives: PathBuf,
    }

    fn write(path: &Path, body: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn group(inspection: &StorageInspection, kind: ManagedStorageKind) -> &ManagedStorageGroup {
        inspection.group(kind).expect("every kind is listed")
    }

    // ------------------------------------------------------------ inspection

    #[tokio::test]
    async fn storage_no_installed_client_names_is_unused_and_what_one_names_is_required() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        world.install(&version).await;
        world.stray_download(b"a pack file");
        world.stray_asset(b"an old sound");
        world.stray_library("old", 40);
        world.stray_natives();
        write(
            &world.layout.asset_index("old").unwrap(),
            b"{\"objects\":{}}",
        );

        let inspection = world.inspect().await;

        assert!(inspection.complete);
        assert_eq!((inspection.clients, inspection.servers), (1, 0));
        let downloads = group(&inspection, ManagedStorageKind::DownloadedFiles);
        assert_eq!(
            (downloads.required.entries, downloads.unused.entries),
            (1, 1),
            "the client jar is needed and the pack file is not"
        );
        assert_eq!(downloads.unused.bytes, b"a pack file".len() as u64);
        let assets = group(&inspection, ManagedStorageKind::AssetObjects);
        assert_eq!((assets.required.entries, assets.unused.entries), (1, 1));
        let libraries = group(&inspection, ManagedStorageKind::Libraries);
        assert_eq!(
            (libraries.required.entries, libraries.unused.entries),
            (1, 1)
        );
        let natives = group(&inspection, ManagedStorageKind::UnpackedNatives);
        assert_eq!(
            (natives.required.entries, natives.unused.entries),
            (1, 1),
            "one version's natives, and another's that no client runs"
        );
        let indexes = group(&inspection, ManagedStorageKind::AssetIndexes);
        assert_eq!((indexes.required.entries, indexes.unused.entries), (1, 1));
        assert_eq!(
            inspection.reclaimable_bytes(),
            b"a pack file".len() as u64
                + b"an old sound".len() as u64
                + 40
                + b"old natives".len() as u64
                + b"{\"objects\":{}}".len() as u64
        );
    }

    #[tokio::test]
    async fn every_client_and_server_counts_not_only_one_selected_host() {
        let first = Synthetic::new("1.20.1");
        let second = Synthetic::new("1.21.1");
        let world = World::with(&[&first, &second]).await;
        world.client("1.20.1", LoaderSelection::vanilla());
        world.client("1.21.1", LoaderSelection::vanilla());
        let one = world.install(&first).await;
        let two = world.install(&second).await;

        let inspection = world.inspect().await;

        assert_eq!(inspection.reclaimable_bytes(), 0);
        for kept in [&one.client, &one.library, &two.client, &two.library] {
            assert!(kept.is_file());
        }
        let plan = world.plan().await;
        assert!(plan.is_empty(), "{:?}", plan.items());
    }

    #[tokio::test]
    async fn age_never_makes_an_entry_unused() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let installed = world.install(&version).await;
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_hours(24 * 900);
        for path in [&installed.client, &installed.library, &installed.asset] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }

        assert_eq!(world.inspect().await.reclaimable_bytes(), 0);
        assert!(world.plan().await.is_empty());
    }

    #[tokio::test]
    async fn a_host_that_runs_a_loader_keeps_libraries_and_installers_but_not_other_unused_files() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client(
            "1.21.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100"),
        );
        world.install(&version).await;
        let library = world.stray_library("loader", 30);
        let installer = world.stray_installer(b"an installer");
        let pack = world.stray_download(b"a pack file");

        let inspection = world.inspect().await;

        for kind in [
            ManagedStorageKind::Libraries,
            ManagedStorageKind::InstallerFiles,
        ] {
            let held = group(&inspection, kind);
            assert_eq!(held.unused, Usage::default(), "{kind:?}");
            assert_eq!(
                held.withheld.first().map(|(reason, _)| *reason),
                Some(WithheldReason::LoaderFilesNotRecorded)
            );
        }
        let plan = world.plan().await;
        let outcome = world.apply(&plan).await.expect("apply");
        assert_eq!(outcome.removed.entries, 1);
        assert!(library.is_file() && installer.is_file());
        assert!(!pack.exists(), "a file no loader needs is still removable");
    }

    #[tokio::test]
    async fn a_loader_server_keeps_libraries_and_installers_too() {
        let world = World::new().await;
        world
            .app
            .registry()
            .create_server(ephervex_storage::NewServer::new(
                "server",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::Forge, "52.0.0"),
            ))
            .expect("server");
        let library = world.stray_library("loader", 30);

        let inspection = world.inspect().await;

        assert_eq!(inspection.servers, 1);
        assert_eq!(
            group(&inspection, ManagedStorageKind::Libraries).unused,
            Usage::default()
        );
        assert!(library.is_file());
    }

    #[tokio::test]
    async fn a_game_version_whose_metadata_cannot_be_read_leaves_everything_undecided() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let plan = world.plan_of("1.21.1").await;
        let pack = world.stray_download(b"a pack file");
        let sha1 = sha1_hex(version.document(&world.server.uri()).as_bytes());
        let document = world.layout.content(
            ContentStore::Metadata,
            &Hash::new(HashAlgorithm::Sha1, &sha1).unwrap(),
            FileExtension::new("json").ok().as_ref(),
        );
        std::fs::write(document, b"damaged").unwrap();
        drop(plan);

        let inspection = world.inspect().await;

        let downloads = group(&inspection, ManagedStorageKind::DownloadedFiles);
        assert_eq!(downloads.unused, Usage::default());
        assert_eq!(
            downloads.withheld.first().map(|(reason, _)| *reason),
            Some(WithheldReason::HostReferencesUnknown)
        );
        let plan = world.plan().await;
        assert!(plan.is_empty());
        assert!(pack.is_file());
    }

    #[tokio::test]
    async fn a_client_with_a_damaged_asset_index_leaves_everything_undecided() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let installed = world.install(&version).await;
        let plan = world.plan_of("1.21.1").await;
        write(&plan.asset_index.path, b"not the index");
        world.stray_download(b"a pack file");

        assert!(world.plan().await.is_empty());
        assert!(installed.asset.is_file());
    }

    #[tokio::test]
    async fn a_client_whose_version_was_never_fetched_needs_nothing_recorded() {
        let world = World::new().await;
        world.client("1.21.1", LoaderSelection::vanilla());
        world.stray_download(b"a pack file");

        let plan = world.plan().await;

        assert_eq!(plan.entries(), 1);
    }

    #[tokio::test]
    async fn a_cached_file_also_linked_inside_a_host_is_kept() {
        let world = World::new().await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        let cached = world.stray_download(b"a mod jar");
        let mods = client.root.join("minecraft/mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::hard_link(&cached, mods.join("a.jar")).unwrap();

        let inspection = world.inspect().await;

        let downloads = group(&inspection, ManagedStorageKind::DownloadedFiles);
        assert_eq!(downloads.unused, Usage::default());
        assert_eq!(
            downloads.withheld.first().map(|(reason, _)| *reason),
            Some(WithheldReason::LinkedIntoHost)
        );
        let plan = world.plan().await;
        assert!(plan.is_empty());
        assert!(
            plan.kept()
                .iter()
                .any(|kept| kept.reason == KeptReason::Withheld(WithheldReason::LinkedIntoHost))
        );
    }

    #[tokio::test]
    async fn a_file_supplied_by_hand_is_kept_rather_than_judged_unused() {
        let world = World::new().await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let supplied = world.stray_download(b"a restricted mod jar");
        let ordinary = world.stray_download(b"an ordinary pack file");
        record_supplied_download(&world.layout, &sha1_of(b"a restricted mod jar"))
            .await
            .unwrap();

        let inspection = world.inspect().await;
        let downloads = group(&inspection, ManagedStorageKind::DownloadedFiles);
        assert_eq!(downloads.unused.entries, 1);
        assert_eq!(
            downloads
                .withheld
                .first()
                .map(|(reason, usage)| (*reason, usage.entries)),
            Some((WithheldReason::SuppliedByHand, 1))
        );

        let plan = world.plan().await;
        assert!(
            plan.kept()
                .iter()
                .any(|kept| kept.reason == KeptReason::Withheld(WithheldReason::SuppliedByHand))
        );
        world.apply(&plan).await.expect("apply");
        assert!(
            supplied.is_file(),
            "the hand-supplied file survives cleanup"
        );
        assert!(!ordinary.exists(), "an ordinary unused download is removed");
    }

    #[tokio::test]
    async fn a_plan_made_before_a_file_was_marked_as_supplied_by_hand_does_not_remove_it() {
        let world = World::new().await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let supplied = world.stray_download(b"a restricted mod jar");
        let plan = world.plan().await;
        assert_eq!(
            plan.entries(),
            1,
            "unmarked, the file is planned for removal"
        );

        record_supplied_download(&world.layout, &sha1_of(b"a restricted mod jar"))
            .await
            .unwrap();
        let outcome = world.apply(&plan).await.expect("apply");

        assert!(supplied.is_file());
        assert_eq!(outcome.no_longer_unused, 1);
        assert_eq!(outcome.removed, Usage::default());
    }

    // --------------------------------------------------------------- cleanup

    #[tokio::test]
    async fn a_cleanup_removes_what_the_plan_lists_and_nothing_a_client_needs_or_a_host_owns() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        let installed = world.install(&version).await;
        let pack = world.stray_download(b"a pack file");
        let sound = world.stray_asset(b"an old sound");
        let library = world.stray_library("old", 40);
        let natives = world.stray_natives();
        for owned in [
            "minecraft/saves/world/level.dat",
            "minecraft/mods/a.jar",
            "minecraft/options.txt",
            ".launcher/snapshots/s1/options.txt",
            ".launcher/staging/new.jar",
            ".launcher/retired/old.jar",
        ] {
            write(&client.root.join(owned), b"user data");
        }

        let plan = world.plan().await;
        assert_eq!(plan.entries(), 4);
        assert_eq!(
            plan.reclaimable_bytes(),
            b"a pack file".len() as u64
                + b"an old sound".len() as u64
                + 40
                + b"old natives".len() as u64
        );
        let outcome = world.apply(&plan).await.expect("apply");

        assert!(outcome.is_complete() && !outcome.can_retry());
        assert_eq!(outcome.planned, outcome.removed);
        assert_eq!(outcome.removed.bytes, plan.reclaimable_bytes());
        for gone in [&pack, &sound, &library, &natives] {
            assert!(!gone.exists(), "{}", gone.display());
        }
        for kept in [
            &installed.client,
            &installed.library,
            &installed.asset,
            &installed.index_view,
            &installed.natives.join("liblwjgl.so"),
        ] {
            assert!(kept.is_file(), "{}", kept.display());
        }
        for owned in [
            "minecraft/saves/world/level.dat",
            "minecraft/mods/a.jar",
            "minecraft/options.txt",
            ".launcher/snapshots/s1/options.txt",
            ".launcher/staging/new.jar",
            ".launcher/retired/old.jar",
        ] {
            assert!(client.root.join(owned).is_file(), "{owned}");
        }
        assert_eq!(world.inspect().await.reclaimable_bytes(), 0);
    }

    #[tokio::test]
    async fn a_plan_limited_to_some_kinds_leaves_the_others() {
        let world = World::new().await;
        let pack = world.stray_download(b"a pack file");
        let library = world.stray_library("old", 40);

        let plan = world
            .app
            .plan_storage_cleanup(&[ManagedStorageKind::Libraries], &CancellationToken::new())
            .await
            .expect("plan");
        world.apply(&plan).await.expect("apply");

        assert!(!library.exists());
        assert!(pack.is_file());
    }

    #[tokio::test]
    async fn a_plan_names_what_it_removes_by_kind_with_its_largest_entries() {
        let world = World::new().await;
        world.stray_library("small", 10);
        world.stray_library("large", 400);

        let plan = world.plan().await;

        let [item] = plan.items() else {
            panic!("one kind: {:?}", plan.items());
        };
        assert_eq!(item.kind, ManagedStorageKind::Libraries);
        assert_eq!(item.usage.entries, 2);
        assert_eq!(
            item.largest.first().unwrap(),
            Path::new("libraries/org/stray/large/1.0/large-1.0.jar"),
            "largest first, relative to the cache root"
        );
        assert!(plan.is_complete());
    }

    #[tokio::test]
    async fn applying_while_an_operation_holds_the_cache_removes_nothing() {
        let world = World::new().await;
        let pack = world.stray_download(b"a pack file");
        let plan = world.plan().await;
        let lease = world.app.artifact_cache_lease().await.expect("lease");

        let refused = world.apply(&plan).await;

        assert!(
            matches!(
                refused,
                Err(CoreError::ArtifactCache(
                    ephervex_download::ArtifactCacheError::Busy { .. }
                ))
            ),
            "{refused:?}"
        );
        assert!(pack.is_file());
        drop(lease);
        assert_eq!(world.apply(&plan).await.expect("free").removed.entries, 1);
    }

    #[tokio::test]
    async fn an_empty_plan_applies_without_needing_the_cache() {
        let world = World::new().await;
        let plan = world.plan().await;
        let _lease = world.app.artifact_cache_lease().await.expect("lease");

        let outcome = world.apply(&plan).await.expect("nothing to do");

        assert_eq!(outcome, CleanupOutcome::default());
    }

    #[tokio::test]
    async fn an_entry_a_client_came_to_need_after_planning_is_not_removed() {
        let version = Synthetic::new("1.21.1");
        let world = World::new().await;
        // The file a client of this version will need, planned as unused while
        // no client of it exists.
        let jar = world.stray_download(&version.client);
        let plan = world.plan().await;
        assert_eq!(plan.entries(), 1);
        seed(
            world.app.paths().cache_dir(),
            &world.server.uri(),
            &[&version],
        )
        .await;
        world.client("1.21.1", LoaderSelection::vanilla());

        let outcome = world.apply(&plan).await.expect("apply");

        assert!(
            jar.is_file(),
            "an old plan never removes what is now in use"
        );
        assert_eq!(outcome.no_longer_unused, 1);
        assert_eq!(outcome.removed, Usage::default());
        assert_eq!(outcome.planned.entries, 1);
    }

    #[tokio::test]
    async fn an_entry_that_was_replaced_since_planning_is_not_removed_on_the_old_plan() {
        let world = World::new().await;
        let library = world.stray_library("old", 40);
        let plan = world.plan().await;
        // Same path, now a link to something else entirely.
        std::fs::remove_file(&library).unwrap();
        let elsewhere = world.base.path().join("elsewhere.jar");
        write(&elsewhere, b"not a cache file");
        std::os::unix::fs::symlink(&elsewhere, &library).unwrap();

        let outcome = world.apply(&plan).await.expect("apply");

        assert_eq!(outcome.removed, Usage::default());
        assert_eq!(outcome.no_longer_unused, 1);
        assert!(elsewhere.is_file());
    }

    #[tokio::test]
    async fn a_removal_that_fails_is_reported_and_its_bytes_are_not_claimed() {
        use std::os::unix::fs::PermissionsExt as _;
        let world = World::new().await;
        let pack = world.stray_download(b"a pack file");
        let library = world.stray_library("old", 40);
        let plan = world.plan().await;
        let shard = pack.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o555)).unwrap();
        // A process that may write to it anyway, such as root, proves nothing. Removing a
        // missing name cannot tell, since lookup fails before permissions are checked.
        let privileged = std::fs::File::create(shard.join("probe")).is_ok();

        // Applying would succeed and prune the shard, leaving nothing to assert or restore.
        if privileged {
            return;
        }

        let outcome = world.apply(&plan).await.expect("apply");
        std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!outcome.is_complete() && outcome.can_retry());
        assert_eq!(outcome.failed_entries, 1);
        assert_eq!(
            outcome.failures[0].kind,
            ManagedStorageKind::DownloadedFiles
        );
        assert_eq!(
            outcome.failures[0].cause,
            std::io::ErrorKind::PermissionDenied
        );
        assert!(
            outcome.failures[0].path.starts_with("downloads"),
            "named relative to the cache root: {:?}",
            outcome.failures[0].path
        );
        assert_eq!(outcome.removed.entries, 1, "the other entry still went");
        assert_eq!(
            outcome.removed.bytes, 40,
            "freed bytes only for what is gone"
        );
        assert_eq!(outcome.failed_bytes, b"a pack file".len() as u64);
        assert!(pack.is_file() && !library.exists());

        let retry = world.apply(&world.plan().await).await.expect("retry");
        assert!(retry.is_complete());
        assert!(!pack.exists(), "a retry removes what is still unused");
    }

    #[tokio::test]
    async fn a_canceled_cleanup_reports_what_it_did_not_reach() {
        let world = World::new().await;
        let pack = world.stray_download(b"a pack file");
        let plan = world.plan().await;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = world
            .app
            .apply_storage_cleanup(&plan, &cancel)
            .await
            .expect_err("canceled before it starts");

        assert!(error.is_cancellation());
        assert!(pack.is_file());
    }

    // ------------------------------------------------------------- integrity

    #[tokio::test]
    async fn verification_names_damaged_files_from_their_digests_and_from_metadata() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        let installed = world.install(&version).await;
        let intact = world.stray_download(b"an intact download");
        let damaged_download = world.stray_download(b"a download that will be damaged");
        std::fs::write(&damaged_download, b"bit rot").unwrap();
        std::fs::write(&installed.library, b"bit rot").unwrap();
        std::fs::write(&installed.asset, b"bit rot").unwrap();
        std::fs::remove_file(&installed.client).unwrap();
        world.stray_library("unlisted", 5);

        let report = world.verify().await;

        let relative = |path: &Path| {
            path.strip_prefix(world.app.paths().cache_dir())
                .unwrap()
                .to_path_buf()
        };
        let fault_of = |path: &Path| {
            report
                .faults
                .iter()
                .find(|fault| fault.path == relative(path))
                .map(|fault| (fault.fault, fault.is_required(), fault.kind))
        };
        assert_eq!(
            fault_of(&installed.library),
            Some((
                StorageFaultKind::Corrupt,
                true,
                ManagedStorageKind::Libraries
            ))
        );
        assert_eq!(
            fault_of(&installed.asset),
            Some((
                StorageFaultKind::Corrupt,
                true,
                ManagedStorageKind::AssetObjects
            ))
        );
        assert_eq!(
            fault_of(&damaged_download),
            Some((
                StorageFaultKind::Corrupt,
                false,
                ManagedStorageKind::DownloadedFiles
            )),
            "a download names its own digest, so it needs no host to be checked"
        );
        assert_eq!(fault_of(&intact), None);
        assert_eq!(
            fault_of(&installed.client),
            None,
            "nothing has fetched it for a version that was never launched"
        );
        assert!(report.checked >= 4);
        assert!(
            report
                .unchecked
                .contains(&UncheckedStorage::UnpackedNatives)
        );
        assert!(
            report
                .unchecked
                .contains(&UncheckedStorage::UnlistedLibraries { files: 1 })
        );

        world.launched(&client);
        let report = world.verify().await;
        assert!(
            report.faults.iter().any(|fault| {
                fault.path == relative(&installed.client)
                    && fault.fault == StorageFaultKind::Missing
                    && fault.is_required()
            }),
            "a launched version that lost its jar is damaged: {:?}",
            report.faults
        );
    }

    #[tokio::test]
    async fn verification_says_what_it_could_not_cover_instead_of_guessing() {
        let world = World::new().await;
        world.client(
            "1.21.1",
            LoaderSelection::exact(LoaderKind::Forge, "52.0.0"),
        );

        let report = world.verify().await;

        assert!(report.is_sound());
        assert!(report.unchecked.contains(&UncheckedStorage::LoaderFiles));
        assert!(
            report
                .unchecked
                .contains(&UncheckedStorage::VersionMetadataNotCached {
                    version: "1.21.1".to_owned()
                })
        );
    }

    #[tokio::test]
    async fn a_canceled_verification_is_a_cancellation_and_not_a_clean_report() {
        let world = World::new().await;
        world.stray_download(b"an intact download");
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = world
            .app
            .verify_managed_storage(&OperationReporter::silent("test"), &cancel)
            .await
            .expect_err("canceled");

        assert!(error.is_cancellation());
    }

    // -------------------------------------------------------------- recovery

    #[tokio::test]
    async fn recovery_fetches_a_damaged_library_again_and_leaves_what_nothing_needs() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        world.launched(&client);
        let installed = world.install(&version).await;
        std::fs::write(&installed.library, b"bit rot").unwrap();
        let unneeded = world.stray_download(b"an unneeded download");
        std::fs::write(&unneeded, b"bit rot").unwrap();

        let report = world
            .app
            .recover_managed_storage(
                &OperationReporter::silent("test-recover"),
                &CancellationToken::new(),
            )
            .await
            .expect("recover");

        assert_eq!(report.recovered.len(), 1);
        assert_eq!(report.recovered[0].kind, ManagedStorageKind::Libraries);
        assert_eq!(std::fs::read(&installed.library).unwrap(), version.library);
        assert!(report.still_damaged.is_empty() && report.failures.is_empty());
        assert_eq!(report.left_for_cleanup.len(), 1);
        assert!(
            unneeded.is_file(),
            "recovery never removes what no installed version needs"
        );
        assert_eq!(
            report.after.faults.len(),
            1,
            "the unneeded one is still damaged"
        );
        assert!(!report.canceled);
    }

    #[tokio::test]
    async fn a_failed_recovery_names_the_provider_failure_and_never_reports_the_file_healthy() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        world.launched(&client);
        let installed = world.install(&version).await;
        std::fs::write(&installed.library, b"bit rot").unwrap();
        world.server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&world.server)
            .await;

        let report = world
            .app
            .recover_managed_storage(
                &OperationReporter::silent("test-recover"),
                &CancellationToken::new(),
            )
            .await
            .expect("recover reports a failure rather than erroring");

        assert!(report.recovered.is_empty());
        assert_eq!(report.still_damaged.len(), 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].version, "1.21.1");
        assert!(!report.failures[0].error.message_id().is_empty());
        assert!(
            report.failures[0].error.to_string().contains("lib-1.21.1")
                || report.failures[0].error.to_string().contains("library"),
            "the failure names what could not be fetched: {}",
            report.failures[0].error
        );
        assert!(
            !report.after.is_sound(),
            "the entry is not presented as healthy"
        );
        assert!(
            !installed.library.is_file()
                || std::fs::read(&installed.library).unwrap() == version.library,
            "damaged bytes do not stay at the path as if they were the artifact"
        );
    }

    #[tokio::test]
    async fn recovery_has_nothing_to_do_for_storage_that_is_sound() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        world.install(&version).await;
        world.server.reset().await;

        let report = world
            .app
            .recover_managed_storage(
                &OperationReporter::silent("test-recover"),
                &CancellationToken::new(),
            )
            .await
            .expect("recover");

        assert!(report.recovered.is_empty() && report.still_damaged.is_empty());
        assert!(report.after.is_sound());
        assert_eq!(
            world.server.received_requests().await.unwrap().len(),
            0,
            "nothing was fetched"
        );
    }

    #[tokio::test]
    async fn a_failed_recovery_for_a_version_never_launched_is_not_reported_recovered() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let installed = world.install(&version).await;
        std::fs::write(&installed.library, b"bit rot").unwrap();
        world.server.reset().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&world.server)
            .await;

        let report = world
            .app
            .recover_managed_storage(
                &OperationReporter::silent("test-recover"),
                &CancellationToken::new(),
            )
            .await
            .expect("recover");

        assert!(report.recovered.is_empty(), "{:?}", report.recovered);
        assert_eq!(report.still_damaged.len(), 1);
        assert!(
            !report.after.is_sound(),
            "the file set aside and not replaced is missing, not intact"
        );
        assert!(
            report
                .after
                .faults
                .iter()
                .any(|fault| fault.fault == StorageFaultKind::Missing)
        );
    }

    #[tokio::test]
    async fn rechecking_after_a_stop_reads_only_the_files_that_were_damaged() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client("1.21.1", LoaderSelection::vanilla());
        let installed = world.install(&version).await;
        world.stray_download(b"an intact download");
        std::fs::write(&installed.library, b"bit rot").unwrap();
        let host = world.app.host_references().await.expect("references");
        let scope = CheckScope {
            only: true,
            paths: [installed.library.clone()].into(),
        };

        let report = world
            .app
            .check_managed_storage(
                &host,
                &scope,
                &OperationReporter::silent("test"),
                &CancellationToken::new(),
            )
            .await
            .expect("check");

        assert_eq!(report.faults.len(), 1);
        assert_eq!(report.checked, 1, "only the damaged file was read");
        assert!(!report.complete, "a partial look says it is partial");
    }

    #[tokio::test]
    async fn a_launched_client_whose_metadata_is_not_found_leaves_everything_undecided() {
        let world = World::new().await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        world.launched(&client);
        world.stray_download(b"a pack file");

        let inspection = world.inspect().await;

        assert_eq!(inspection.reclaimable_bytes(), 0);
        assert!(world.plan().await.is_empty());
    }

    #[tokio::test]
    async fn a_launched_client_whose_asset_index_is_gone_keeps_every_asset_object() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        let client = world.client("1.21.1", LoaderSelection::vanilla());
        world.launched(&client);
        let installed = world.install(&version).await;
        let plan = world.plan_of("1.21.1").await;
        std::fs::remove_file(&plan.asset_index.path).unwrap();

        assert!(world.plan().await.is_empty());
        assert!(installed.asset.is_file());
    }

    #[tokio::test]
    async fn a_damaged_file_a_cleanup_keeps_is_not_described_as_one_it_removes() {
        let version = Synthetic::new("1.21.1");
        let world = World::with(&[&version]).await;
        world.client(
            "1.21.1",
            LoaderSelection::exact(LoaderKind::NeoForge, "21.1.100"),
        );
        world.install(&version).await;
        let installer = world.stray_installer(b"an installer");
        std::fs::write(&installer, b"bit rot").unwrap();
        let unused = world.stray_download(b"a pack file");
        std::fs::write(&unused, b"bit rot").unwrap();

        let report = world.verify().await;
        let standing = |path: &Path| {
            report
                .faults
                .iter()
                .find(|fault| {
                    fault
                        .path
                        .ends_with(path.strip_prefix(world.app.paths().cache_dir()).unwrap())
                })
                .map(|fault| fault.standing)
        };

        assert_eq!(
            standing(&installer),
            Some(FaultStanding::Kept(WithheldReason::LoaderFilesNotRecorded))
        );
        assert_eq!(standing(&unused), Some(FaultStanding::Unused));
        let recovery = world
            .app
            .recover_managed_storage(
                &OperationReporter::silent("test-recover"),
                &CancellationToken::new(),
            )
            .await
            .expect("recover");
        assert_eq!(recovery.left_in_place.len(), 1);
        assert_eq!(recovery.left_for_cleanup.len(), 1);
    }

    #[tokio::test]
    async fn an_entry_another_process_removed_after_planning_is_reported_as_gone() {
        let world = World::new().await;
        let pack = world.stray_download(b"a pack file");
        let plan = world.plan().await;
        std::fs::remove_file(&pack).unwrap();

        let outcome = world.apply(&plan).await.expect("apply");

        assert_eq!(outcome.already_gone, 1);
        assert_eq!(outcome.no_longer_unused, 0);
        assert_eq!(outcome.removed, Usage::default());
    }
}
