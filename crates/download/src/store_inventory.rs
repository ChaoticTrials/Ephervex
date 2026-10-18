//! What the shared stores hold entry by entry, and the removal of chosen
//! entries.
//!
//! [`crate::artifact_cache`] answers "how much is cached, and which entries does
//! a byte budget evict". This module answers the question reference-aware
//! maintenance needs instead: which units exist in each shared store, how big
//! each is, and how to remove exactly the ones a caller decided on. It does not
//! decide anything itself. Whether an entry is still wanted depends on what is
//! installed, which only the application layer knows, so this module reports
//! and removes and never chooses.
//!
//! # What an entry is
//!
//! * `downloads/` and `installer-cache/`: a finalized file at a content address.
//! * `assets/objects/`: a finalized asset object at Mojang's own address, and
//!   `assets/indexes/`: a launch view of an asset index.
//! * `libraries/`: any regular file under the Maven-layout tree.
//! * `natives/`: one `<version>/<platform>` directory, as a single unit,
//!   because its files only mean something together.
//!
//! Anything this build does not recognize, a link, and a transfer in flight is
//! left out. Unrecognized names are never guessed at: guessing that an unknown
//! file is disposable is how a cleanup deletes something that mattered.
//!
//! # Removal is bound to the stores
//!
//! [`ArtifactCacheExclusive::remove`] takes the maintenance lock exclusively
//! for its caller and refuses any path that is not inside the store root its
//! entry names, so a malformed entry cannot point it anywhere else. Every name
//! is inspected without following links immediately before it is removed.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::path::{Component, Path, PathBuf};

use ephervex_model::{Hash, HashAlgorithm};
use tokio_util::sync::CancellationToken;
use tracing::{trace, warn};

use crate::artifact_cache::{
    ArtifactCache, ArtifactCacheError, ArtifactCacheMaintenance, Exclusivity, MaintenanceStage,
    MaintenanceTrouble, blocking, directory_name, file_identity, lock, lock_exclusively,
    note_entry_failure, note_listing_failure, read_directory,
};
use crate::cache::{CacheLayout, ContentAddress, ContentStore, PART_SUFFIX};
use crate::supplied::is_supplied_download;

/// How many levels above a removed file an emptied directory is pruned for a
/// store whose layout is `<root>/<shard>/<file>`-shaped.
const SHARD_LEVELS: usize = 1;

/// What a [`StoreEntry`] is the address of.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EntryAddress {
    /// A file whose name is its digest, in `downloads/` or `installer-cache/`.
    Content(ContentAddress),
    /// A Mojang asset object, named by its SHA-1.
    AssetObject(Hash),
    /// The launch view of one asset index, `assets/indexes/<id>.json`.
    AssetIndex { id: String },
    /// A file in the Maven-layout libraries store. Its name is a coordinate,
    /// not a digest, so nothing here can say what its bytes should be.
    Library,
    /// One version's unpacked natives for one platform.
    Natives { version: String, platform: String },
}

/// One removable unit in a shared store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEntry {
    pub store: ContentStore,
    pub address: EntryAddress,
    /// Every name the unit has inside its store, in path order and never empty.
    /// More than one only for a file reachable by hard links. A directory
    /// entry has exactly one.
    pub paths: Vec<PathBuf>,
    /// Apparent size: the length each file reports, summed over the unit.
    pub bytes: u64,
    /// How many regular files the unit is made of.
    pub files: u64,
    /// More hard links point at the file than the store has names for, so a
    /// host or another directory still holds its bytes after every name in the
    /// store is removed.
    pub linked_outside_store: bool,
    /// The user supplied this file by hand because its provider does not allow
    /// the launcher to download it, so removing it cannot be undone by fetching
    /// it again. See [`crate::record_supplied_download`].
    pub supplied_by_hand: bool,
}

/// The entries of the shared stores at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoreInventory {
    /// In store order, then path order.
    pub entries: Vec<StoreEntry>,
    /// Whether every directory could be listed and every entry inspected. A
    /// `false` makes everything derived from the inventory a lower bound.
    pub complete: bool,
    /// The walk stopped because it was canceled, so `entries` is partial.
    pub canceled: bool,
    /// A bounded sample of what could not be read.
    pub troubles: Vec<MaintenanceTrouble>,
    /// How many entries were troublesome in total, sampled or not.
    pub troubled_entries: u64,
}

/// What removing one entry did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryRemoval {
    /// Every name of the entry was removed by this call.
    Removed,
    /// Nothing was left to remove: another process got there first.
    AlreadyGone,
    /// At least one name is still on disk.
    Failed(MaintenanceTrouble),
    /// The call was canceled before this entry was reached.
    NotAttempted,
}

/// The maintenance lock, held exclusively.
///
/// While one exists, in this process or any other, no operation holds a shared
/// lease and none can take one, so nothing is about to open a path in the
/// stores. Released on drop.
#[derive(Debug)]
pub struct ArtifactCacheExclusive {
    /// `None` only where the platform has no advisory locking, in which case
    /// taking the lock fails before one of these exists.
    _held: Option<std::fs::File>,
    layout: CacheLayout,
}

impl ArtifactCache {
    /// Lists the shared stores' entries, holding the shared side of the
    /// maintenance lock so a collection cannot be half done underneath it.
    ///
    /// Reads the file system and nothing else. Cancellation is checked between
    /// directories and reported on the result rather than as an error.
    pub async fn inventory(
        &self,
        cancel: &CancellationToken,
    ) -> Result<StoreInventory, ArtifactCacheError> {
        let layout = self.layout().clone();
        let path = self.lock_path();
        let cancel = cancel.clone();
        blocking(move || {
            let _lease = lock(&path, Exclusivity::Shared)?;
            Ok(inventory_of(&layout, &cancel))
        })
        .await
    }

    /// Takes the maintenance lock exclusively.
    ///
    /// Refused with [`ArtifactCacheError::Busy`] at once while an operation of
    /// this process holds a lease, and after the same short bounded wait the
    /// collector uses when another process holds the cache. Holding the result
    /// across asynchronous work is fine; holding it across a download is not,
    /// because no launch or installation can start until it is dropped.
    pub async fn exclusive(&self) -> Result<ArtifactCacheExclusive, ArtifactCacheError> {
        let layout = self.layout().clone();
        let path = self.lock_path();
        blocking(move || {
            let held = lock_exclusively(&path)?;
            Ok(ArtifactCacheExclusive {
                _held: held,
                layout,
            })
        })
        .await
    }
}

impl ArtifactCacheExclusive {
    /// Lists the entries under the lock this value holds.
    pub async fn inventory(&self, cancel: &CancellationToken) -> StoreInventory {
        let layout = self.layout.clone();
        let cancel = cancel.clone();
        blocking_inventory(move || inventory_of(&layout, &cancel)).await
    }

    /// Removes `entries`, one outcome per entry in the same order.
    ///
    /// Stops between entries when `cancel` fires, and reports the rest as
    /// [`EntryRemoval::NotAttempted`]. An entry that fails does not stop the
    /// others: one name the user may not delete must not keep the rest of a
    /// cleanup from happening, and it is not retried in a loop either.
    pub async fn remove(
        &self,
        entries: Vec<StoreEntry>,
        cancel: &CancellationToken,
    ) -> Vec<EntryRemoval> {
        let layout = self.layout.clone();
        let cancel = cancel.clone();
        blocking_inventory(move || {
            entries
                .iter()
                .map(|entry| {
                    if cancel.is_cancelled() {
                        EntryRemoval::NotAttempted
                    } else {
                        remove_entry(&layout, entry)
                    }
                })
                .collect()
        })
        .await
    }
}

/// Runs a blocking closure whose own result cannot fail.
///
/// A join error means the closure panicked, which would be a bug here; the
/// default is returned rather than propagating it, because every caller treats
/// "nothing found" and "nothing removed" as the conservative answer.
async fn blocking_inventory<T, F>(work: F) -> T
where
    T: Default + Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.unwrap_or_default()
}

/// The running state of one walk.
struct Walk<'a> {
    cancel: &'a CancellationToken,
    report: ArtifactCacheMaintenance,
    /// Entries in discovery order, each with the link count its file reported.
    found: Vec<(StoreEntry, u64)>,
    /// Which entry a (device, inode) pair already belongs to.
    seen: BTreeMap<(u64, u64), usize>,
    canceled: bool,
}

impl Walk<'_> {
    fn stop(&mut self) -> bool {
        if self.cancel.is_cancelled() {
            self.canceled = true;
        }
        self.canceled
    }

    /// Lists a directory, recording a failure to. `None` means skip it.
    fn list(&mut self, directory: &Path) -> Option<Vec<PathBuf>> {
        match read_directory(directory) {
            Ok(Some(children)) => Some(children),
            Ok(None) => None,
            Err(source) => {
                note_listing_failure(directory, &source, &mut self.report);
                None
            }
        }
    }

    fn metadata(&mut self, path: &Path) -> Option<Metadata> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Some(metadata),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                note_entry_failure(
                    path,
                    MaintenanceStage::Inspecting,
                    &source,
                    &mut self.report,
                );
                None
            }
        }
    }

    /// Adds one regular file, or another name for one already added.
    fn file(&mut self, store: ContentStore, address: EntryAddress, path: &Path) {
        let Some(metadata) = self.metadata(path) else {
            return;
        };
        self.file_with(store, address, path, &metadata);
    }

    fn file_with(
        &mut self,
        store: ContentStore,
        address: EntryAddress,
        path: &Path,
        metadata: &Metadata,
    ) {
        if !metadata.is_file() {
            return;
        }
        let links = link_count(metadata);
        if let Some(identity) = file_identity(metadata) {
            if let Some(&index) = self.seen.get(&identity) {
                trace!(path = %path.display(), "another name for one stored file");
                let paths = &mut self.found[index].0.paths;
                paths.push(path.to_path_buf());
                paths.sort();
                return;
            }
            self.seen.insert(identity, self.found.len());
        }
        self.found.push((
            StoreEntry {
                store,
                address,
                paths: vec![path.to_path_buf()],
                bytes: metadata.len(),
                files: 1,
                linked_outside_store: false,
                supplied_by_hand: false,
            },
            links,
        ));
    }
}

#[cfg(unix)]
fn link_count(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.nlink()
}

#[cfg(not(unix))]
fn link_count(_metadata: &Metadata) -> u64 {
    1
}

fn inventory_of(layout: &CacheLayout, cancel: &CancellationToken) -> StoreInventory {
    let mut walk = Walk {
        cancel,
        report: ArtifactCacheMaintenance {
            complete: true,
            ..ArtifactCacheMaintenance::default()
        },
        found: Vec::new(),
        seen: BTreeMap::new(),
        canceled: false,
    };
    for store in [ContentStore::Downloads, ContentStore::InstallerCache] {
        walk_content_store(&mut walk, layout, store);
    }
    walk_assets(&mut walk, layout);
    walk_libraries(&mut walk, layout);
    walk_natives(&mut walk, layout);

    let mut entries: Vec<StoreEntry> = walk
        .found
        .into_iter()
        .map(|(mut entry, links)| {
            entry.linked_outside_store = links > entry.paths.len() as u64;
            entry.supplied_by_hand = matches!(
                &entry.address,
                EntryAddress::Content(address) if is_supplied_download(layout, &address.hash)
            );
            entry
        })
        .collect();
    entries.sort_by(|left, right| left.paths.cmp(&right.paths));
    StoreInventory {
        entries,
        complete: walk.report.complete && !walk.canceled,
        canceled: walk.canceled,
        troubles: walk.report.troubles,
        troubled_entries: walk.report.troubled_entries,
    }
}

/// `<store>/<algorithm>/<shard>/<digest>[.extension]`.
fn walk_content_store(walk: &mut Walk<'_>, layout: &CacheLayout, store: ContentStore) {
    let root = layout.store_root(store);
    let Some(algorithms) = walk.list(&root) else {
        return;
    };
    for algorithm in algorithms {
        if walk.stop() {
            return;
        }
        let Some(algorithm_name) = directory_name(&algorithm, &mut walk.report) else {
            continue;
        };
        let Some(shards) = walk.list(&algorithm) else {
            continue;
        };
        for shard in shards {
            let Some(shard_name) = directory_name(&shard, &mut walk.report) else {
                continue;
            };
            let Some(files) = walk.list(&shard) else {
                continue;
            };
            for file in files {
                let Some(file_name) = file.file_name().and_then(std::ffi::OsStr::to_str) else {
                    continue;
                };
                let Some(address) =
                    CacheLayout::recognize_content(&algorithm_name, &shard_name, file_name)
                else {
                    trace!(path = %file.display(), "leaving an unrecognized cache file alone");
                    continue;
                };
                walk.file(store, EntryAddress::Content(address), &file);
            }
        }
    }
}

/// `assets/objects/<first two digits>/<sha1>` and `assets/indexes/<id>.json`.
fn walk_assets(walk: &mut Walk<'_>, layout: &CacheLayout) {
    let root = layout.store_root(ContentStore::Assets);
    if let Some(shards) = walk.list(&root.join("objects")) {
        for shard in shards {
            if walk.stop() {
                return;
            }
            let Some(shard_name) = directory_name(&shard, &mut walk.report) else {
                continue;
            };
            let Some(files) = walk.list(&shard) else {
                continue;
            };
            for file in files {
                let Some(hash) = file
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .and_then(|name| asset_object_hash(&shard_name, name))
                else {
                    continue;
                };
                walk.file(ContentStore::Assets, EntryAddress::AssetObject(hash), &file);
            }
        }
    }
    if let Some(files) = walk.list(&root.join("indexes")) {
        for file in files {
            let Some(id) = file
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .and_then(|name| name.strip_suffix(".json"))
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let address = EntryAddress::AssetIndex { id: id.to_owned() };
            walk.file(ContentStore::Assets, address, &file);
        }
    }
}

/// The SHA-1 an asset object's path names, when the path is in Mojang's
/// layout: lowercase hex of the exact length, under its own two-digit shard.
fn asset_object_hash(shard: &str, name: &str) -> Option<Hash> {
    let canonical = name.len() == 40
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !canonical || name.get(..2) != Some(shard) {
        return None;
    }
    Hash::new(HashAlgorithm::Sha1, name)
}

/// Every regular file under the libraries store, without following links.
fn walk_libraries(walk: &mut Walk<'_>, layout: &CacheLayout) {
    let mut pending = vec![layout.store_root(ContentStore::Libraries)];
    while let Some(directory) = pending.pop() {
        if walk.stop() {
            return;
        }
        let Some(children) = walk.list(&directory) else {
            continue;
        };
        for child in children {
            let Some(metadata) = walk.metadata(&child) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(child);
            } else if metadata.is_file() && !is_transfer_in_flight(&child) {
                walk.file_with(
                    ContentStore::Libraries,
                    EntryAddress::Library,
                    &child,
                    &metadata,
                );
            }
        }
    }
}

/// Whether a file's last extension marks it as a transfer in flight.
fn is_transfer_in_flight(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.starts_with(PART_SUFFIX))
}

/// `natives/<version>/<platform>`, each as one unit.
fn walk_natives(walk: &mut Walk<'_>, layout: &CacheLayout) {
    let root = layout.store_root(ContentStore::Natives);
    let Some(versions) = walk.list(&root) else {
        return;
    };
    for version in versions {
        if walk.stop() {
            return;
        }
        let Some(version_name) = directory_name(&version, &mut walk.report) else {
            continue;
        };
        let Some(platforms) = walk.list(&version) else {
            continue;
        };
        for platform in platforms {
            let Some(platform_name) = directory_name(&platform, &mut walk.report) else {
                continue;
            };
            let (bytes, files) = measure_tree(walk, &platform);
            walk.found.push((
                StoreEntry {
                    store: ContentStore::Natives,
                    address: EntryAddress::Natives {
                        version: version_name.clone(),
                        platform: platform_name,
                    },
                    paths: vec![platform],
                    bytes,
                    files,
                    linked_outside_store: false,
                    supplied_by_hand: false,
                },
                1,
            ));
        }
    }
}

/// The apparent size and file count of a directory tree, links not followed.
fn measure_tree(walk: &mut Walk<'_>, root: &Path) -> (u64, u64) {
    let (mut bytes, mut files) = (0u64, 0u64);
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Some(children) = walk.list(&directory) else {
            continue;
        };
        for child in children {
            let Some(metadata) = walk.metadata(&child) else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(child);
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                files += 1;
            }
        }
    }
    (bytes, files)
}

fn remove_entry(layout: &CacheLayout, entry: &StoreEntry) -> EntryRemoval {
    let root = layout.store_root(entry.store);
    let refuse = |path: &Path, cause: std::io::ErrorKind| {
        EntryRemoval::Failed(MaintenanceTrouble {
            path: path.to_path_buf(),
            stage: MaintenanceStage::Removing,
            cause,
        })
    };
    if entry.paths.is_empty() {
        return refuse(&root, std::io::ErrorKind::InvalidInput);
    }
    for path in &entry.paths {
        if !is_inside(&root, path) {
            warn!(path = %path.display(), "refusing to remove a path outside its store");
            return refuse(path, std::io::ErrorKind::InvalidInput);
        }
    }

    let directory = matches!(entry.address, EntryAddress::Natives { .. });
    let levels = if matches!(entry.address, EntryAddress::Library) {
        usize::MAX
    } else {
        SHARD_LEVELS
    };
    let (mut removed, mut gone) = (0usize, 0usize);
    for path in &entry.paths {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                gone += 1;
                continue;
            }
            Err(error) => return refuse(path, error.kind()),
        };
        // Judged on the entry itself, so a name that has since become a link, or
        // a file where a directory was, is left exactly as it is.
        let expected = if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !expected {
            return refuse(path, std::io::ErrorKind::InvalidInput);
        }
        let outcome = if directory {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match outcome {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => gone += 1,
            Err(error) => {
                warn!(path = %path.display(), %error, "could not remove a stored entry");
                return refuse(path, error.kind());
            }
        }
        prune_empty_parents(path, &root, levels);
    }
    if removed > 0 || gone == 0 {
        EntryRemoval::Removed
    } else {
        EntryRemoval::AlreadyGone
    }
}

/// Whether `path` is below `root` and cannot climb out of it.
fn is_inside(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        && path != root
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir | Component::CurDir))
}

/// Removes directories a removal emptied, upward, never the store root.
///
/// `remove_dir` refuses a directory with contents, which is what keeps this
/// from deleting anything that was not already empty.
fn prune_empty_parents(removed: &Path, root: &Path, levels: usize) {
    let mut current = removed.parent();
    for _ in 0..levels {
        let Some(directory) = current else {
            return;
        };
        if directory == root || !directory.starts_with(root) {
            return;
        }
        if let Err(error) = std::fs::remove_dir(directory) {
            trace!(path = %directory.display(), %error, "directory kept");
            return;
        }
        current = directory.parent();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use ephervex_model::Hash;

    use super::*;
    use crate::cache::FileExtension;

    struct Fixture {
        root: tempfile::TempDir,
        cache: ArtifactCache,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("temp dir");
            let cache = ArtifactCache::new(CacheLayout::new(root.path()));
            Self { root, cache }
        }

        fn layout(&self) -> &CacheLayout {
            self.cache.layout()
        }

        fn download(&self, seed: u8, bytes: usize) -> PathBuf {
            let hash = sha1(seed);
            let extension = FileExtension::new("jar").expect("jar");
            let path = self
                .layout()
                .content(ContentStore::Downloads, &hash, Some(&extension));
            write_file(&path, bytes);
            path
        }

        async fn inventory(&self) -> StoreInventory {
            self.cache
                .inventory(&CancellationToken::new())
                .await
                .expect("inventory")
        }
    }

    fn write_file(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
        std::fs::write(path, vec![b'x'; bytes]).expect("write");
    }

    fn sha1(seed: u8) -> Hash {
        Hash::new(HashAlgorithm::Sha1, &format!("{seed:02x}").repeat(20)).expect("sha1")
    }

    fn entry_at<'a>(inventory: &'a StoreInventory, path: &Path) -> &'a StoreEntry {
        inventory
            .entries
            .iter()
            .find(|entry| entry.paths.iter().any(|name| name == path))
            .unwrap_or_else(|| panic!("no entry at {}", path.display()))
    }

    #[tokio::test]
    async fn every_store_is_listed_by_what_its_layout_says_an_entry_is() {
        let fixture = Fixture::new();
        let download = fixture.download(0x11, 10);
        let object = fixture
            .layout()
            .asset_object(&sha1(0x22))
            .expect("asset object");
        write_file(&object, 20);
        let index = fixture.layout().asset_index("17").expect("index");
        write_file(&index, 30);
        let library = fixture
            .layout()
            .store_root(ContentStore::Libraries)
            .join("org/example/lib/1.0/lib-1.0.jar");
        write_file(&library, 40);
        let natives = fixture
            .layout()
            .natives("1.12.2", "linux-x86_64")
            .expect("natives");
        write_file(&natives.join("liblwjgl.so"), 50);
        write_file(&natives.join("sub/libopenal.so"), 60);

        let inventory = fixture.inventory().await;

        assert!(inventory.complete);
        assert_eq!(inventory.entries.len(), 5);
        assert!(matches!(
            entry_at(&inventory, &download).address,
            EntryAddress::Content(_)
        ));
        assert!(matches!(
            entry_at(&inventory, &object).address,
            EntryAddress::AssetObject(ref hash) if *hash == sha1(0x22)
        ));
        assert!(matches!(
            entry_at(&inventory, &index).address,
            EntryAddress::AssetIndex { ref id } if id == "17"
        ));
        assert_eq!(
            entry_at(&inventory, &library).address,
            EntryAddress::Library
        );
        let natives_entry = entry_at(&inventory, &natives);
        assert_eq!((natives_entry.bytes, natives_entry.files), (110, 2));
        assert_eq!(natives_entry.paths, [natives]);
    }

    #[tokio::test]
    async fn a_name_this_build_does_not_recognize_is_never_an_entry() {
        let fixture = Fixture::new();
        let downloads = fixture.layout().store_root(ContentStore::Downloads);
        write_file(&downloads.join("sha1/ab/not-a-digest.jar"), 5);
        write_file(
            &downloads.join("sha1/ab/ababababababababababababababababababababab"),
            5,
        );
        let part = fixture.download(0x33, 5).with_extension("jar.part-1234");
        write_file(&part, 5);
        let objects = fixture
            .layout()
            .store_root(ContentStore::Assets)
            .join("objects");
        write_file(
            &objects.join("zz/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
            5,
        );
        let wrong_shard = format!("{}{}", "ab", "cd".repeat(19));
        write_file(&objects.join("cd").join(wrong_shard), 5);
        let library_part = fixture
            .layout()
            .store_root(ContentStore::Libraries)
            .join("org/x/lib.jar.part-9");
        write_file(&library_part, 5);

        let inventory = fixture.inventory().await;

        let listed: Vec<&PathBuf> = inventory
            .entries
            .iter()
            .flat_map(|entry| &entry.paths)
            .collect();
        assert!(
            !listed.contains(&&part) && !listed.contains(&&library_part),
            "a transfer in flight is not a finished entry: {listed:?}"
        );
        assert!(
            inventory
                .entries
                .iter()
                .all(|entry| entry.store == ContentStore::Downloads
                    && matches!(entry.address, EntryAddress::Content(_))),
            "only the recognized download is listed: {:?}",
            inventory.entries
        );
        assert_eq!(inventory.entries.len(), 1);
    }

    #[tokio::test]
    async fn links_are_not_followed_and_a_linked_directory_adds_nothing() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().expect("outside");
        write_file(&outside.path().join("huge.jar"), 1 << 12);
        let libraries = fixture.layout().store_root(ContentStore::Libraries);
        std::fs::create_dir_all(&libraries).expect("libraries");
        std::os::unix::fs::symlink(outside.path(), libraries.join("linked")).expect("link");
        std::os::unix::fs::symlink(
            outside.path().join("huge.jar"),
            libraries.join("huge-link.jar"),
        )
        .expect("link");
        let natives = fixture.layout().store_root(ContentStore::Natives);
        std::fs::create_dir_all(natives.join("1.12.2")).expect("natives");
        std::os::unix::fs::symlink(outside.path(), natives.join("1.12.2/linux-x86_64"))
            .expect("link");

        assert!(fixture.inventory().await.entries.is_empty());
    }

    #[tokio::test]
    async fn a_file_linked_from_outside_the_store_is_marked_and_names_inside_are_one_entry() {
        let fixture = Fixture::new();
        let kept = fixture.download(0x44, 7);
        let host = fixture.root.path().join("instance/mods/a.jar");
        std::fs::create_dir_all(host.parent().unwrap()).expect("mods");
        std::fs::hard_link(&kept, &host).expect("link into a host");
        let twin = fixture
            .layout()
            .store_root(ContentStore::Downloads)
            .join("sha1/55")
            .join(format!("{}.jar", "55".repeat(20)));
        write_file(&twin, 9);
        let alias = fixture
            .layout()
            .store_root(ContentStore::Downloads)
            .join("sha1/66")
            .join(format!("{}.jar", "66".repeat(20)));
        std::fs::create_dir_all(alias.parent().unwrap()).expect("shard");
        std::fs::hard_link(&twin, &alias).expect("second name in the store");

        let inventory = fixture.inventory().await;

        assert!(entry_at(&inventory, &kept).linked_outside_store);
        let shared = entry_at(&inventory, &twin);
        assert_eq!(shared.paths.len(), 2, "one file, both of its names");
        assert!(!shared.linked_outside_store);
        assert_eq!(inventory.entries.len(), 2);
    }

    #[tokio::test]
    async fn removal_takes_every_name_and_prunes_only_what_it_emptied() {
        let fixture = Fixture::new();
        let library = fixture
            .layout()
            .store_root(ContentStore::Libraries)
            .join("org/example/lib/1.0/lib-1.0.jar");
        write_file(&library, 4);
        let sibling = fixture
            .layout()
            .store_root(ContentStore::Libraries)
            .join("org/example/other/2.0/other-2.0.jar");
        write_file(&sibling, 4);
        let download = fixture.download(0x77, 3);
        let neighbor = fixture.download(0x78, 3);
        let natives = fixture
            .layout()
            .natives("1.12.2", "linux-x86_64")
            .expect("natives");
        write_file(&natives.join("liblwjgl.so"), 3);

        let exclusive = fixture.cache.exclusive().await.expect("exclusive");
        let inventory = exclusive.inventory(&CancellationToken::new()).await;
        let chosen: Vec<StoreEntry> = inventory
            .entries
            .iter()
            .filter(|entry| {
                [&library, &download, &natives]
                    .iter()
                    .any(|path| entry.paths.contains(path))
            })
            .cloned()
            .collect();
        let outcomes = exclusive.remove(chosen, &CancellationToken::new()).await;

        assert_eq!(outcomes, vec![EntryRemoval::Removed; 3]);
        assert!(!library.exists() && !download.exists() && !natives.exists());
        assert!(
            !library.parent().unwrap().exists(),
            "the version directory it emptied goes"
        );
        assert!(sibling.is_file(), "a sibling library keeps its directories");
        assert!(neighbor.is_file(), "a neighboring download is untouched");
        assert!(
            fixture
                .layout()
                .store_root(ContentStore::Libraries)
                .is_dir(),
            "a store root is part of the layout, not of its contents"
        );
        assert!(
            !fixture
                .layout()
                .store_root(ContentStore::Natives)
                .join("1.12.2")
                .exists(),
            "an emptied version directory goes with its last platform"
        );
    }

    #[tokio::test]
    async fn removal_refuses_a_path_outside_its_store_and_one_that_is_no_longer_a_file() {
        let fixture = Fixture::new();
        let outside = fixture.root.path().join("instance/saves/level.dat");
        write_file(&outside, 3);
        let download = fixture.download(0x79, 3);
        let mut forged = entry_for(ContentStore::Downloads, &download);
        forged.paths = vec![outside.clone()];
        let traversal = fixture
            .layout()
            .store_root(ContentStore::Downloads)
            .join("../../instance/saves/level.dat");
        let mut climbing = entry_for(ContentStore::Downloads, &download);
        climbing.paths = vec![traversal];

        std::fs::remove_file(&download).expect("replace");
        std::os::unix::fs::symlink(&outside, &download).expect("link");
        let swapped = entry_for(ContentStore::Downloads, &download);

        let exclusive = fixture.cache.exclusive().await.expect("exclusive");
        let outcomes = exclusive
            .remove(vec![forged, climbing, swapped], &CancellationToken::new())
            .await;

        assert!(
            outcomes
                .iter()
                .all(|outcome| matches!(outcome, EntryRemoval::Failed(_))),
            "{outcomes:?}"
        );
        assert!(outside.is_file(), "nothing outside the store was touched");
        assert!(
            std::fs::symlink_metadata(&download)
                .expect("still there")
                .file_type()
                .is_symlink(),
            "a name that became a link is left as it is"
        );
    }

    #[tokio::test]
    async fn an_entry_supplied_by_hand_is_reported_as_such() {
        let fixture = Fixture::new();
        let supplied = fixture.download(0x31, 3);
        let ordinary = fixture.download(0x32, 3);
        crate::supplied::record_supplied_download(fixture.layout(), &sha1(0x31))
            .await
            .expect("record");

        let inventory = fixture.inventory().await;

        assert!(entry_at(&inventory, &supplied).supplied_by_hand);
        assert!(!entry_at(&inventory, &ordinary).supplied_by_hand);
    }

    fn entry_for(store: ContentStore, path: &Path) -> StoreEntry {
        StoreEntry {
            store,
            address: EntryAddress::Library,
            paths: vec![path.to_path_buf()],
            bytes: 3,
            files: 1,
            linked_outside_store: false,
            supplied_by_hand: false,
        }
    }

    #[tokio::test]
    async fn an_entry_another_process_already_removed_is_reported_as_gone() {
        let fixture = Fixture::new();
        let download = fixture.download(0x7a, 3);
        let inventory = fixture.inventory().await;
        std::fs::remove_file(&download).expect("removed elsewhere");

        let exclusive = fixture.cache.exclusive().await.expect("exclusive");
        let outcomes = exclusive
            .remove(inventory.entries, &CancellationToken::new())
            .await;

        assert_eq!(outcomes, [EntryRemoval::AlreadyGone]);
    }

    #[tokio::test]
    async fn a_canceled_removal_attempts_nothing_and_says_so() {
        let fixture = Fixture::new();
        let download = fixture.download(0x7b, 3);
        let inventory = fixture.inventory().await;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let exclusive = fixture.cache.exclusive().await.expect("exclusive");
        let outcomes = exclusive.remove(inventory.entries, &cancel).await;

        assert_eq!(outcomes, [EntryRemoval::NotAttempted]);
        assert!(download.is_file());
    }

    #[tokio::test]
    async fn a_canceled_inventory_is_marked_partial() {
        let fixture = Fixture::new();
        fixture.download(0x7c, 3);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let inventory = fixture.cache.inventory(&cancel).await.expect("inventory");

        assert!(inventory.canceled && !inventory.complete);
    }

    #[tokio::test]
    async fn the_exclusive_lock_is_refused_while_an_operation_holds_a_lease() {
        let fixture = Fixture::new();
        let lease = fixture.cache.lease().await.expect("lease");

        let refused = fixture.cache.exclusive().await;
        assert!(
            matches!(refused, Err(ArtifactCacheError::Busy { .. })),
            "{refused:?}"
        );

        drop(lease);
        fixture
            .cache
            .exclusive()
            .await
            .expect("free once the operation is done");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_directory_that_cannot_be_read_makes_the_inventory_incomplete_not_empty() {
        use std::os::unix::fs::PermissionsExt as _;
        let fixture = Fixture::new();
        let kept = fixture.download(0x7d, 3);
        let locked = fixture
            .layout()
            .store_root(ContentStore::Libraries)
            .join("org");
        write_file(&locked.join("x/lib.jar"), 3);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");
        let effective = std::fs::read_dir(&locked).is_err();

        let inventory = fixture.inventory().await;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        if !effective {
            return;
        }
        assert!(!inventory.complete);
        assert_eq!(inventory.troubled_entries, 1);
        assert_eq!(entry_at(&inventory, &kept).bytes, 3);
    }
}
