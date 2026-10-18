//! How much disk space Ephervex's own directories hold, and what for.
//!
//! # What a size means here
//!
//! Every figure is the apparent size of regular files: the length each file
//! reports, summed. That is the one number that is the same on every
//! filesystem and that a user can check with `du --apparent-size`. It is not a
//! promise about physical space:
//!
//! * a snapshot made with a copy-on-write clone shares its unchanged bytes
//!   with the host it was taken from, so it can occupy far less than its size;
//! * a sparse file occupies less than its length;
//! * compression and deduplication at the filesystem level are invisible here.
//!
//! Attributing shared extents to one owner would need filesystem-specific
//! extent queries, and the answer would still depend on which owner is asked
//! about first. This is an inspection view, so it reports what it can state
//! exactly and says what that leaves out.
//!
//! # Counting every byte once
//!
//! Each file belongs to exactly one category and one retention class, decided
//! by where it sits. A file reached through more than one hard link is counted
//! the first time it is seen, in a fixed walk order, and never again. Symbolic
//! links are never followed, so a link out of an instance cannot pull the
//! place it points at into Ephervex's total, and a directory that is itself
//! one of the measured roots is measured once, as that root.
//!
//! Nothing here deletes, moves, or changes anything.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ephervex_download::{CacheLayout, ContentStore, OperationReporter, is_supplied_download};
use ephervex_model::Progress;
use ephervex_pack::PackError;
use ephervex_storage::Paths;
use ephervex_storage::metadata::{
    CLIENT_GAME_DIR, PUT_ASIDE_DIR, RETIRED_DIR, SERVER_GAME_DIR, SNAPSHOTS_DIR, STAGING_DIR,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument};

use crate::app::App;
use crate::error::CoreResult;

/// What a stretch of storage is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageCategory {
    /// Client instances: game directories and their own records, apart from
    /// what the categories below take out of them.
    ClientInstances,
    /// Local servers, on the same terms.
    LocalServers,
    /// The `mods` directory of every client and server.
    Mods,
    /// Local safety snapshots kept inside each host.
    Snapshots,
    /// Game logs and crash reports inside hosts, Ephervex's own log, and the
    /// output it captured from servers.
    Logs,
    /// Verified downloads, loader installers, provider metadata, and artwork,
    /// and the provenance records of downloads the user supplied by hand. All
    /// but those downloads can be fetched again.
    DownloadCache,
    /// Minecraft's assets, libraries, and unpacked natives, shared by every
    /// host that launches the same release.
    SharedGameFiles,
    /// Java runtimes Ephervex downloaded.
    JavaRuntimes,
    /// Configuration, the index database, and icons.
    LauncherState,
    /// What an interrupted change left behind for recovery: files moved aside,
    /// files still arriving, and snapshot-restore journals.
    Recovery,
    /// Anything in Ephervex's roots that none of the above describes.
    Other,
}

impl StorageCategory {
    /// Every category, in the order a report lists them.
    pub const ALL: [Self; 11] = [
        Self::ClientInstances,
        Self::LocalServers,
        Self::Mods,
        Self::Snapshots,
        Self::Logs,
        Self::DownloadCache,
        Self::SharedGameFiles,
        Self::JavaRuntimes,
        Self::LauncherState,
        Self::Recovery,
        Self::Other,
    ];

    /// A stable identifier for frontends and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClientInstances => "client_instances",
            Self::LocalServers => "local_servers",
            Self::Mods => "mods",
            Self::Snapshots => "snapshots",
            Self::Logs => "logs",
            Self::DownloadCache => "download_cache",
            Self::SharedGameFiles => "shared_game_files",
            Self::JavaRuntimes => "java_runtimes",
            Self::LauncherState => "launcher_state",
            Self::Recovery => "recovery",
            Self::Other => "other",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::ClientInstances => 0,
            Self::LocalServers => 1,
            Self::Mods => 2,
            Self::Snapshots => 3,
            Self::Logs => 4,
            Self::DownloadCache => 5,
            Self::SharedGameFiles => 6,
            Self::JavaRuntimes => 7,
            Self::LauncherState => 8,
            Self::Recovery => 9,
            Self::Other => 10,
        }
    }
}

/// Whether a stretch of storage could be recreated, and by what.
///
/// Deliberately not "safe to delete". Nothing in a report is a cleanup
/// suggestion: a download may be the only copy of a file its author no longer
/// distributes, and recovery state is what makes an interrupted change
/// reversible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageRetention {
    /// Kept until the user removes it: persistent hosts, their snapshots and
    /// logs, Ephervex's own records, and downloads the user supplied by hand
    /// because their provider does not let the launcher fetch them.
    Persistent,
    /// Temporary hosts, which live under the operating system's temporary
    /// directory and go when it is cleared.
    Temporary,
    /// Caches Ephervex can fetch or derive again.
    Regenerable,
    /// Evidence an interrupted change left behind. Needed to recover or review
    /// that change, and never regenerable.
    Recovery,
}

impl StorageRetention {
    /// Every class, in the order a report lists them.
    pub const ALL: [Self; 4] = [
        Self::Persistent,
        Self::Temporary,
        Self::Regenerable,
        Self::Recovery,
    ];

    /// A stable identifier for frontends and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Persistent => "persistent",
            Self::Temporary => "temporary",
            Self::Regenerable => "regenerable",
            Self::Recovery => "recovery",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Persistent => 0,
            Self::Temporary => 1,
            Self::Regenerable => 2,
            Self::Recovery => 3,
        }
    }
}

/// What one category holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryUsage {
    pub category: StorageCategory,
    pub bytes: u64,
    pub files: u64,
    /// Directories this category is made of, where it has its own. Empty for a
    /// category spread across every host, such as Mods.
    pub locations: Vec<PathBuf>,
}

/// What one retention class holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionUsage {
    pub retention: StorageRetention,
    pub bytes: u64,
}

/// One measurement of Ephervex's storage.
///
/// `categories` and `retention` each list every value once, and each sums to
/// `total_bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskUsageReport {
    pub total_bytes: u64,
    pub files: u64,
    pub categories: Vec<CategoryUsage>,
    pub retention: Vec<RetentionUsage>,
    /// Directories and files that exist and could not be read. Their size is
    /// missing from every figure.
    pub unreadable: u64,
    /// The first few of them, for saying where.
    pub unreadable_examples: Vec<PathBuf>,
    /// Symbolic links met and not followed.
    pub symlinks_skipped: u64,
    /// Extra hard links to a file already counted.
    pub hard_links_deduplicated: u64,
    pub measured_at: DateTime<Utc>,
}

impl DiskUsageReport {
    /// Whether some of the storage could not be measured.
    #[must_use]
    pub const fn is_incomplete(&self) -> bool {
        self.unreadable > 0
    }

    /// One category's usage.
    #[must_use]
    pub fn category(&self, category: StorageCategory) -> Option<&CategoryUsage> {
        self.categories
            .iter()
            .find(|usage| usage.category == category)
    }

    /// One retention class's usage.
    #[must_use]
    pub fn retention_bytes(&self, retention: StorageRetention) -> u64 {
        self.retention
            .iter()
            .find(|usage| usage.retention == retention)
            .map_or(0, |usage| usage.bytes)
    }
}

/// How the files under one measured root are classified.
#[derive(Debug, Clone, Copy)]
enum RootRule {
    /// Each child directory is one client instance.
    Clients(StorageRetention),
    /// Each child directory is one server.
    Servers(StorageRetention),
    Cache,
    Data,
    State,
    Config,
    Temp,
}

#[derive(Debug, Clone)]
struct ScanRoot {
    path: PathBuf,
    rule: RootRule,
}

/// How many unreadable paths a report names.
const UNREADABLE_EXAMPLES: usize = 20;
/// How often, in files, a scan reports how far it has got.
const PROGRESS_EVERY: u64 = 500;

type Classified = (StorageCategory, StorageRetention);

/// `cache` is the layout of the cache root, which is what tells a download the
/// user supplied by hand apart from one that can be fetched again.
fn classify(rule: RootRule, cache: &CacheLayout, components: &[&OsStr]) -> Classified {
    use StorageCategory as C;
    use StorageRetention as R;
    let first = components.first().and_then(|name| name.to_str());
    match rule {
        RootRule::Clients(retention) => {
            classify_host(C::ClientInstances, CLIENT_GAME_DIR, retention, components)
        }
        RootRule::Servers(retention) => {
            classify_host(C::LocalServers, SERVER_GAME_DIR, retention, components)
        }
        RootRule::Cache => match first {
            Some("assets" | "libraries" | "natives") => (C::SharedGameFiles, R::Regenerable),
            Some(name) if name == ContentStore::Downloads.directory() => {
                if is_supplied_file(cache, components) {
                    (C::DownloadCache, R::Persistent)
                } else {
                    (C::DownloadCache, R::Regenerable)
                }
            }
            Some("installer-cache" | "metadata" | "artwork") => (C::DownloadCache, R::Regenerable),
            Some(name) if name == CacheLayout::SUPPLIED_DIRECTORY => {
                (C::DownloadCache, R::Persistent)
            }
            _ => (C::Other, R::Regenerable),
        },
        RootRule::Data => match first {
            Some("java") => (C::JavaRuntimes, R::Regenerable),
            Some("icons") => (C::LauncherState, R::Persistent),
            Some("shared") => (C::SharedGameFiles, R::Persistent),
            _ => (C::Other, R::Persistent),
        },
        RootRule::State => match first {
            Some("logs") => (C::Logs, R::Persistent),
            Some(name) if name.starts_with(ephervex_storage::paths::DATABASE_FILE_NAME) => {
                (C::LauncherState, R::Persistent)
            }
            _ => (C::Other, R::Persistent),
        },
        RootRule::Config => (C::LauncherState, R::Persistent),
        RootRule::Temp => (C::Other, R::Temporary),
    }
}

/// Whether `components`, relative to the cache root, name a download store
/// entry whose digest is recorded as supplied by hand.
fn is_supplied_file(cache: &CacheLayout, components: &[&OsStr]) -> bool {
    let [_, algorithm, shard, file_name] = components else {
        return false;
    };
    let (Some(algorithm), Some(shard), Some(file_name)) =
        (algorithm.to_str(), shard.to_str(), file_name.to_str())
    else {
        return false;
    };
    CacheLayout::recognize_content(algorithm, shard, file_name)
        .is_some_and(|address| is_supplied_download(cache, &address.hash))
}

/// Where a file inside one host belongs. `components` starts with the host's
/// own directory.
fn classify_host(
    host: StorageCategory,
    game_dir: &str,
    retention: StorageRetention,
    components: &[&OsStr],
) -> Classified {
    let within: Vec<&str> = components
        .iter()
        .skip(1)
        .map(|name| name.to_str().unwrap_or_default())
        .collect();
    match within.as_slice() {
        [".launcher", SNAPSHOTS_DIR, ..] => (StorageCategory::Snapshots, retention),
        [".launcher", RETIRED_DIR | STAGING_DIR, ..] => {
            (StorageCategory::Recovery, StorageRetention::Recovery)
        }
        [".launcher", name, ..] if name.starts_with("restore-") => {
            (StorageCategory::Recovery, StorageRetention::Recovery)
        }
        [".launcher", PUT_ASIDE_DIR, ..] => (StorageCategory::Mods, retention),
        [dir, "mods", ..] if *dir == game_dir => (StorageCategory::Mods, retention),
        [dir, "logs" | "crash-reports", ..] if *dir == game_dir => {
            (StorageCategory::Logs, retention)
        }
        _ => (host, retention),
    }
}

/// Running totals, filled in one file at a time.
struct Tally {
    categories: [(u64, u64); StorageCategory::ALL.len()],
    retention: [u64; StorageRetention::ALL.len()],
    files: u64,
    unreadable: u64,
    unreadable_examples: Vec<PathBuf>,
    symlinks_skipped: u64,
    hard_links_deduplicated: u64,
    /// Device and inode of every multiply linked file already counted.
    linked: HashSet<(u64, u64)>,
}

impl Tally {
    fn new() -> Self {
        Self {
            categories: [(0, 0); StorageCategory::ALL.len()],
            retention: [0; StorageRetention::ALL.len()],
            files: 0,
            unreadable: 0,
            unreadable_examples: Vec::new(),
            symlinks_skipped: 0,
            hard_links_deduplicated: 0,
            linked: HashSet::new(),
        }
    }

    fn unreadable(&mut self, path: &Path, error: &std::io::Error) {
        // Gone between being listed and being read is an ordinary race, and
        // there is nothing left to have missed.
        if error.kind() == std::io::ErrorKind::NotFound {
            return;
        }
        self.unreadable += 1;
        if self.unreadable_examples.len() < UNREADABLE_EXAMPLES {
            self.unreadable_examples.push(path.to_path_buf());
        }
    }

    fn file(&mut self, metadata: &std::fs::Metadata, (category, retention): Classified) {
        if metadata.nlink() > 1 && !self.linked.insert((metadata.dev(), metadata.ino())) {
            self.hard_links_deduplicated += 1;
            return;
        }
        let bytes = metadata.len();
        let slot = &mut self.categories[category.index()];
        slot.0 = slot.0.saturating_add(bytes);
        slot.1 += 1;
        let class = &mut self.retention[retention.index()];
        *class = class.saturating_add(bytes);
        self.files += 1;
    }
}

/// Walks `roots` in order, without following links, and totals what it finds.
///
/// A directory that is itself one of `roots` is skipped wherever else it
/// appears, so it is measured once, as its own root.
fn measure(
    roots: &[ScanRoot],
    reporter: &OperationReporter,
    cancel: &CancellationToken,
) -> CoreResult<Tally> {
    let root_paths: HashSet<&Path> = roots.iter().map(|root| root.path.as_path()).collect();
    let mut tally = Tally::new();
    let mut measured: HashSet<&Path> = HashSet::new();
    for root in roots {
        if !measured.insert(root.path.as_path()) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&root.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                tally.unreadable(&root.path, &error);
                continue;
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        let cache = CacheLayout::new(&root.path);
        let mut pending = vec![root.path.clone()];
        while let Some(directory) = pending.pop() {
            if cancel.is_cancelled() {
                return Err(PackError::Canceled.into());
            }
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => {
                    tally.unreadable(&directory, &error);
                    continue;
                }
            };
            let mut children: Vec<PathBuf> = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) => children.push(entry.path()),
                    Err(error) => tally.unreadable(&directory, &error),
                }
            }
            // Sorted, so which link to a shared inode counts is the same on
            // every run.
            children.sort();
            for path in children.into_iter().rev() {
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        tally.unreadable(&path, &error);
                        continue;
                    }
                };
                let kind = metadata.file_type();
                if kind.is_symlink() {
                    tally.symlinks_skipped += 1;
                } else if kind.is_dir() {
                    if !root_paths.contains(path.as_path()) {
                        pending.push(path);
                    }
                } else if kind.is_file() {
                    let components: Vec<&OsStr> = path
                        .strip_prefix(&root.path)
                        .map(|relative| relative.iter().collect())
                        .unwrap_or_default();
                    tally.file(&metadata, classify(root.rule, &cache, &components));
                    if tally.files % PROGRESS_EVERY == 0 {
                        reporter.set_items_at(
                            Progress {
                                current: tally.files,
                                total: None,
                            },
                            Some(root.path.display().to_string()),
                        );
                    }
                }
            }
        }
    }
    Ok(tally)
}

/// Where each measured root is and how it is read, in the fixed order hard
/// links are attributed in. Hosts first, so a file a host shares with a cache
/// by hard link is counted as the host's.
fn scan_roots(paths: &Paths) -> Vec<ScanRoot> {
    let resolve = |path: PathBuf| std::fs::canonicalize(&path).unwrap_or(path);
    [
        (
            paths.persistent_instances_dir(),
            RootRule::Clients(StorageRetention::Persistent),
        ),
        (
            paths.temporary_instances_dir(),
            RootRule::Clients(StorageRetention::Temporary),
        ),
        (
            paths.persistent_servers_dir(),
            RootRule::Servers(StorageRetention::Persistent),
        ),
        (
            paths.temporary_servers_dir(),
            RootRule::Servers(StorageRetention::Temporary),
        ),
        (paths.cache_dir().to_path_buf(), RootRule::Cache),
        (paths.data_dir().to_path_buf(), RootRule::Data),
        (paths.state_dir().to_path_buf(), RootRule::State),
        (paths.config_dir().to_path_buf(), RootRule::Config),
        (paths.temp_dir().to_path_buf(), RootRule::Temp),
    ]
    .into_iter()
    .map(|(path, rule)| ScanRoot {
        path: resolve(path),
        rule,
    })
    .collect()
}

/// The directories a category has of its own, existing or not.
fn category_locations(paths: &Paths, category: StorageCategory) -> Vec<PathBuf> {
    let cache = paths.cache_dir();
    match category {
        StorageCategory::ClientInstances => vec![
            paths.persistent_instances_dir(),
            paths.temporary_instances_dir(),
        ],
        StorageCategory::LocalServers => vec![
            paths.persistent_servers_dir(),
            paths.temporary_servers_dir(),
        ],
        StorageCategory::Logs => vec![paths.logs_dir()],
        StorageCategory::DownloadCache => vec![
            paths.downloads_cache_dir(),
            paths.installer_cache_dir(),
            paths.metadata_cache_dir(),
            cache.join("artwork"),
        ],
        StorageCategory::SharedGameFiles => vec![
            paths.assets_cache_dir(),
            paths.libraries_cache_dir(),
            paths.natives_cache_dir(),
            paths.shared_dir(),
        ],
        StorageCategory::JavaRuntimes => vec![paths.java_dir()],
        StorageCategory::LauncherState => {
            vec![
                paths.config_dir().to_path_buf(),
                paths.state_dir().to_path_buf(),
            ]
        }
        // Inside every host, or scattered: no one folder is this category.
        StorageCategory::Mods
        | StorageCategory::Snapshots
        | StorageCategory::Recovery
        | StorageCategory::Other => Vec::new(),
    }
}

fn existing_locations(paths: &Paths, category: StorageCategory) -> Vec<PathBuf> {
    category_locations(paths, category)
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

impl App {
    /// Measures what Ephervex's directories hold, by what each part is for.
    ///
    /// Reads the file system and nothing else: nothing is fetched, written, or
    /// removed. The walk reports how many files it has counted, and stops
    /// between directories when `cancel` fires.
    #[instrument(skip(self, reporter, cancel))]
    pub fn measure_disk_usage(
        &self,
        reporter: &OperationReporter,
        cancel: &CancellationToken,
    ) -> CoreResult<DiskUsageReport> {
        disk_usage_report(self.paths(), reporter, cancel)
    }

    /// Shows one of a category's own directories in the file manager.
    ///
    /// Addressed by category and position rather than by path, so a frontend
    /// can only open a directory this launcher itself names. Returns the path
    /// opened, or `None` when there is no such directory.
    pub fn open_storage_location(
        &self,
        category: StorageCategory,
        index: usize,
    ) -> Option<PathBuf> {
        let path = existing_locations(self.paths(), category)
            .into_iter()
            .nth(index)?;
        crate::instance::open_folder(&path);
        Some(path)
    }
}

fn disk_usage_report(
    paths: &Paths,
    reporter: &OperationReporter,
    cancel: &CancellationToken,
) -> CoreResult<DiskUsageReport> {
    let tally = measure(&scan_roots(paths), reporter, cancel)?;
    let categories = StorageCategory::ALL
        .into_iter()
        .map(|category| {
            let (bytes, files) = tally.categories[category.index()];
            CategoryUsage {
                category,
                bytes,
                files,
                locations: existing_locations(paths, category),
            }
        })
        .collect::<Vec<_>>();
    let retention = StorageRetention::ALL
        .into_iter()
        .map(|retention| RetentionUsage {
            retention,
            bytes: tally.retention[retention.index()],
        })
        .collect();
    let total_bytes = categories.iter().map(|usage| usage.bytes).sum();
    let report = DiskUsageReport {
        total_bytes,
        files: tally.files,
        categories,
        retention,
        unreadable: tally.unreadable,
        unreadable_examples: tally.unreadable_examples,
        symlinks_skipped: tally.symlinks_skipped,
        hard_links_deduplicated: tally.hard_links_deduplicated,
        measured_at: Utc::now(),
    };
    info!(
        total = report.total_bytes,
        files = report.files,
        unreadable = report.unreadable,
        "measured Ephervex's storage"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use ephervex_model::{Hash, HashAlgorithm};

    use super::*;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directories");
        std::fs::write(path, vec![b'x'; bytes]).expect("write");
    }

    fn report(paths: &Paths) -> DiskUsageReport {
        disk_usage_report(
            paths,
            &OperationReporter::silent("test-disk-usage"),
            &CancellationToken::new(),
        )
        .expect("measure")
    }

    fn bytes(report: &DiskUsageReport, category: StorageCategory) -> u64 {
        report.category(category).expect("listed").bytes
    }

    /// An installation with one of everything, each file a distinct size so a
    /// figure landing in the wrong place shows.
    fn installation(base: &Path) -> Paths {
        let paths = Paths::rooted_at(base);
        let client = paths.persistent_instances_dir().join("client-a");
        write(&client.join(".launcher/instance.json"), 1);
        write(&client.join(".launcher/source/recipe.json"), 2);
        write(&client.join("minecraft/options.txt"), 4);
        write(&client.join("minecraft/mods/a.jar"), 8);
        write(&client.join("minecraft/logs/latest.log"), 16);
        write(&client.join("minecraft/crash-reports/crash.txt"), 32);
        write(
            &client.join(".launcher/snapshots/s1/minecraft/options.txt"),
            64,
        );
        write(&client.join(".launcher/retired/old.jar"), 128);
        write(&client.join(".launcher/staging/new.jar"), 256);
        write(&client.join(".launcher/restore-1/restore.json"), 512);
        let server = paths.persistent_servers_dir().join("server-a");
        write(&server.join("server/mods/b.jar"), 1024);
        write(&server.join("server/world/level.dat"), 2048);
        let temporary = paths.temporary_instances_dir().join("client-t");
        write(&temporary.join("minecraft/options.txt"), 4096);
        write(&paths.downloads_cache_dir().join("ab/cd.jar"), 8192);
        write(&paths.assets_cache_dir().join("objects/ab/abcd"), 16384);
        write(&paths.libraries_cache_dir().join("org/x.jar"), 32768);
        write(&paths.java_dir().join("jdk/bin/java"), 65536);
        write(&paths.config_file(), 131_072);
        write(&paths.database_file(), 262_144);
        write(&paths.launcher_logs_dir().join("ephervex.log"), 524_288);
        paths
    }

    #[test]
    fn every_file_lands_in_exactly_one_category_and_the_totals_agree() {
        let base = tempfile::tempdir().expect("temp dir");
        let report = report(&installation(base.path()));

        assert_eq!(
            bytes(&report, StorageCategory::ClientInstances),
            1 + 2 + 4 + 4096
        );
        assert_eq!(bytes(&report, StorageCategory::LocalServers), 2048);
        assert_eq!(bytes(&report, StorageCategory::Mods), 8 + 1024);
        assert_eq!(bytes(&report, StorageCategory::Logs), 16 + 32 + 524_288);
        assert_eq!(bytes(&report, StorageCategory::Snapshots), 64);
        assert_eq!(bytes(&report, StorageCategory::Recovery), 128 + 256 + 512);
        assert_eq!(bytes(&report, StorageCategory::DownloadCache), 8192);
        assert_eq!(
            bytes(&report, StorageCategory::SharedGameFiles),
            16384 + 32768
        );
        assert_eq!(bytes(&report, StorageCategory::JavaRuntimes), 65536);
        assert_eq!(
            bytes(&report, StorageCategory::LauncherState),
            131_072 + 262_144
        );

        let categories: u64 = report.categories.iter().map(|usage| usage.bytes).sum();
        let retention: u64 = report.retention.iter().map(|usage| usage.bytes).sum();
        assert_eq!(categories, report.total_bytes);
        assert_eq!(retention, report.total_bytes);
        assert_eq!(report.total_bytes, (1u64 << 20) - 1, "every byte once");
        assert!(!report.is_incomplete());
    }

    #[test]
    fn recovery_state_is_kept_apart_from_what_can_be_regenerated() {
        let base = tempfile::tempdir().expect("temp dir");
        let report = report(&installation(base.path()));

        assert_eq!(
            report.retention_bytes(StorageRetention::Recovery),
            128 + 256 + 512
        );
        assert_eq!(
            report.retention_bytes(StorageRetention::Regenerable),
            8192 + 16384 + 32768 + 65536,
            "caches and runtimes, and nothing a host or a change depends on"
        );
        assert_eq!(report.retention_bytes(StorageRetention::Temporary), 4096);
    }

    fn supplied_download(paths: &Paths, digit: char, bytes: usize) -> (PathBuf, Hash) {
        let digest = digit.to_string().repeat(40);
        let hash = Hash::new(HashAlgorithm::Sha1, &digest).expect("valid sha1");
        let layout = CacheLayout::new(paths.cache_dir());
        let file = layout.content(ContentStore::Downloads, &hash, None);
        write(&file, bytes);
        (file, hash)
    }

    fn mark_supplied(paths: &Paths, hash: &Hash) {
        let marker = CacheLayout::new(paths.cache_dir()).supplied_marker(hash);
        write(&marker, 0);
    }

    #[test]
    fn a_download_supplied_by_hand_is_not_reported_as_regenerable() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let (_, supplied) = supplied_download(&paths, '1', 100);
        supplied_download(&paths, '2', 20);
        mark_supplied(&paths, &supplied);

        let report = report(&paths);

        assert_eq!(report.retention_bytes(StorageRetention::Persistent), 100);
        assert_eq!(report.retention_bytes(StorageRetention::Regenerable), 20);
        assert_eq!(
            bytes(&report, StorageCategory::DownloadCache),
            120,
            "both stay in the download cache"
        );
    }

    #[test]
    fn the_supplied_download_markers_are_classified_explicitly() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let (_, supplied) = supplied_download(&paths, '1', 100);
        mark_supplied(&paths, &supplied);
        // Markers are empty; a stray file in the directory shows where it lands.
        write(&paths.cache_dir().join("supplied-downloads/note.txt"), 7);

        let report = report(&paths);

        assert_eq!(bytes(&report, StorageCategory::Other), 0);
        assert_eq!(bytes(&report, StorageCategory::DownloadCache), 107);
        assert_eq!(report.retention_bytes(StorageRetention::Persistent), 107);
        assert_eq!(report.retention_bytes(StorageRetention::Regenerable), 0);
    }

    #[test]
    fn a_marker_for_another_digest_does_not_spare_a_download() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        supplied_download(&paths, '1', 100);
        let other = Hash::new(HashAlgorithm::Sha1, &"3".repeat(40)).expect("valid sha1");
        mark_supplied(&paths, &other);

        let report = report(&paths);

        assert_eq!(report.retention_bytes(StorageRetention::Persistent), 0);
        assert_eq!(report.retention_bytes(StorageRetention::Regenerable), 100);
    }

    #[test]
    fn a_link_out_of_an_instance_is_not_followed() {
        let base = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        write(&elsewhere.path().join("huge.bin"), 1 << 16);
        let paths = Paths::rooted_at(base.path());
        let client = paths.persistent_instances_dir().join("client-a");
        write(&client.join("minecraft/options.txt"), 10);
        std::os::unix::fs::symlink(elsewhere.path(), client.join("minecraft/linked"))
            .expect("link");

        let report = report(&paths);
        assert_eq!(report.total_bytes, 10);
        assert_eq!(report.symlinks_skipped, 1);
    }

    #[test]
    fn a_hard_linked_file_is_counted_once_as_the_first_owner_met() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let cached = paths.downloads_cache_dir().join("ab/mod.jar");
        write(&cached, 100);
        let installed = paths
            .persistent_instances_dir()
            .join("client-a/minecraft/mods/mod.jar");
        std::fs::create_dir_all(installed.parent().expect("mods")).expect("mods");
        std::fs::hard_link(&cached, &installed).expect("hard link");

        let report = report(&paths);
        assert_eq!(report.total_bytes, 100);
        assert_eq!(report.hard_links_deduplicated, 1);
        assert_eq!(
            bytes(&report, StorageCategory::Mods),
            100,
            "hosts are walked first, so the installed copy owns it"
        );
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_reported_without_losing_the_rest() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let locked = paths
            .persistent_instances_dir()
            .join("client-a/minecraft/saves");
        write(&locked.join("world/level.dat"), 50);
        write(&paths.downloads_cache_dir().join("ab/cd.jar"), 70);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("lock");
        // A process that can read it anyway, such as root, proves nothing here.
        let effective = std::fs::read_dir(&locked).is_err();

        let report = report(&paths);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("unlock");
        if !effective {
            return;
        }
        assert!(report.is_incomplete());
        assert_eq!(report.unreadable_examples, [locked]);
        assert_eq!(bytes(&report, StorageCategory::DownloadCache), 70);
        assert_eq!(report.total_bytes, 70);
    }

    #[test]
    fn a_file_gone_before_it_is_read_is_not_an_unreadable_one() {
        let mut tally = Tally::new();
        let gone = std::io::Error::from(std::io::ErrorKind::NotFound);
        tally.unreadable(Path::new("/vanished"), &gone);
        assert_eq!(tally.unreadable, 0);
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        tally.unreadable(Path::new("/locked"), &denied);
        assert_eq!(tally.unreadable, 1);
    }

    #[test]
    fn a_relocated_instance_root_inside_another_root_is_measured_once() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path())
            .with_persistent_instance_root(Some(base.path().join("cache/instances")));
        write(
            &paths
                .persistent_instances_dir()
                .join("client-a/minecraft/options.txt"),
            40,
        );

        let report = report(&paths);
        assert_eq!(report.total_bytes, 40);
        assert_eq!(bytes(&report, StorageCategory::ClientInstances), 40);
        assert_eq!(
            bytes(&report, StorageCategory::Other),
            0,
            "not counted again as unknown cache content"
        );
    }

    #[test]
    fn a_canceled_measurement_reports_nothing_partial() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = installation(base.path());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = disk_usage_report(&paths, &OperationReporter::silent("test"), &cancel)
            .expect_err("canceled");
        assert!(error.is_cancellation());
    }

    #[test]
    fn a_folder_is_offered_only_for_a_category_with_its_own() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = installation(base.path());
        let report = report(&paths);
        assert_eq!(
            report
                .category(StorageCategory::JavaRuntimes)
                .expect("listed")
                .locations,
            [paths.java_dir()]
        );
        assert!(
            report
                .category(StorageCategory::Mods)
                .expect("listed")
                .locations
                .is_empty(),
            "mods sit inside every host, so no one folder is them"
        );
    }
}
