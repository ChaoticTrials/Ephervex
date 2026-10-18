//! Watching configured folders for a manually downloaded copy of a file a
//! provider refused to serve automatically.
//!
//! Scoped to run only while an import's blocked-file list is visible and
//! non-empty, never as a persistent background service: a caller asks for a
//! [`DownloadWatcher`] over the exact files one import is still missing,
//! rescans the configured folders for a match, and drops the watcher once
//! its targets run out or the caller stops asking.
//!
//! A candidate is recognized purely by hash, the same proof an ordinary
//! completed download leaves behind. A match is adopted into the shared
//! cache so materialization needs no download address for it at all - it is
//! installed from the cache exactly the way an already-downloaded artifact
//! always is.

use std::path::{Path, PathBuf};

use ephervex_download::{
    CacheLayout, ContentStore, FileExtension, file_matches_hashes, publish_staged,
    record_supplied_download, staging_path, stored_file_matches,
};
use ephervex_model::Hash;
use ephervex_pack::{BlockedProviderFile, ProviderFileIdentity};
use tracing::{debug, warn};

use crate::app::App;

/// One still-blocked file a watcher is looking for.
#[derive(Debug, Clone)]
pub struct WatchTarget {
    pub identity: ProviderFileIdentity,
    /// The provider's own file name, read only for its cosmetic extension -
    /// never trusted to name a destination path. See
    /// [`ephervex_pack::safe_file_name`] for the actual destination that is
    /// built once a match is resolved back into a recipe entry.
    pub file_name: Option<String>,
    pub hashes: Vec<Hash>,
}

impl WatchTarget {
    /// The watch target a blocked file's own facts describe.
    #[must_use]
    pub fn from_blocked(blocked: &BlockedProviderFile) -> Self {
        Self {
            identity: blocked.identity.clone(),
            file_name: blocked.file_name.clone(),
            hashes: blocked.hashes.clone(),
        }
    }
}

impl App {
    /// A watcher over the configured folders for exactly the given targets.
    ///
    /// Reads the folder list and the cache location once, at the moment the
    /// caller starts watching; neither changes for the lifetime of one
    /// import attempt.
    #[must_use]
    pub fn download_watcher(&self, targets: Vec<WatchTarget>) -> DownloadWatcher {
        DownloadWatcher {
            folders: self.watched_download_folders(),
            targets,
            cache: self.artifact_cache().layout().clone(),
        }
    }
}

/// How long a caller should wait between calls to
/// [`DownloadWatcher::scan_once`].
///
/// Not a latency guarantee this launcher makes any promise about: a user who
/// just finished a manual download will not notice a few seconds either way,
/// and a fixed interval avoids depending on a platform's own filesystem
/// notification API for behavior this simple.
pub const RESCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Watches a folder list for files matching a set of targets, by hash.
///
/// Rescans on demand rather than subscribing to filesystem notifications: a
/// user who just finished placing a file by hand does not notice a
/// rescan interval either way, and a plain directory listing needs no
/// platform-specific notification API to behave correctly.
#[derive(Debug)]
pub struct DownloadWatcher {
    folders: Vec<PathBuf>,
    targets: Vec<WatchTarget>,
    cache: CacheLayout,
}

impl DownloadWatcher {
    /// Whether every target has been matched (or the watcher was started
    /// with none). A caller stops asking once this is true.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.targets.is_empty()
    }

    /// Lists every configured folder once, hashing each file found against
    /// every remaining target, and adopts the first match into the shared
    /// cache.
    ///
    /// Returns the matched identity and removes it from the remaining
    /// targets. `None` means nothing matched this pass; the caller decides
    /// how long to wait before asking again. An unreadable folder or an
    /// unhashable candidate is logged and skipped rather than treated as a
    /// reason to stop watching the others.
    pub async fn scan_once(&mut self) -> Option<ProviderFileIdentity> {
        for folder in self.folders.clone() {
            let entries = match std::fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(error) => {
                    debug!(folder = %folder.display(), %error, "could not list a watched folder");
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                if let Some(identity) = self.check_candidate(&path).await {
                    return Some(identity);
                }
            }
        }
        None
    }

    /// Checks one candidate file against every remaining target, adopting
    /// and removing the first one it verifies against.
    async fn check_candidate(&mut self, path: &Path) -> Option<ProviderFileIdentity> {
        let mut matched = None;
        for (index, target) in self.targets.iter().enumerate() {
            match file_matches_hashes(path, &target.hashes).await {
                Ok(true) => {
                    matched = Some(index);
                    break;
                }
                Ok(false) => {}
                Err(error) => {
                    debug!(path = %path.display(), %error, "could not hash a candidate file");
                }
            }
        }
        let index = matched?;
        let target = self.targets[index].clone();
        if let Err(error) = adopt_into_cache(&self.cache, path, &target).await {
            warn!(
                path = %path.display(),
                %error,
                "found a matching file but could not adopt it into the shared cache"
            );
            return None;
        }
        self.targets.remove(index);
        Some(target.identity)
    }
}

/// Copies a verified candidate into the shared cache at its content-addressed
/// path, publishing it the way a completed download is: staged beside the
/// destination, made durable, then renamed into place under the directory's
/// publication lock, so a reader never observes a partial file and a discard
/// of the same path cannot interleave with the rename.
///
/// An existing destination is kept only when it verifies against the target's
/// hashes. A truncated, empty or otherwise mismatching file there counts as
/// absent and is replaced, so a bad entry cannot outlive the correct copy the
/// user supplied.
///
/// The adopted file is recorded as supplied by hand: the provider would not
/// serve it, so neither byte-budget collection nor storage cleanup may treat it
/// as something that can be downloaded again.
///
/// The candidate itself is left exactly where the user put it - this adopts
/// a copy, never a move, so a folder the user is still watching in a file
/// manager does not lose the file it just found.
async fn adopt_into_cache(
    cache: &CacheLayout,
    source: &Path,
    target: &WatchTarget,
) -> std::io::Result<()> {
    let Some(hash) = target.hashes.first() else {
        // Nothing to verify against, so nothing to adopt: `scan_once` never
        // reaches here for a target with no hash, but this stays a clean
        // no-op rather than an assumption if that ever changes.
        return Ok(());
    };
    let extension = target
        .file_name
        .as_deref()
        .and_then(|name| Path::new(name).extension())
        .and_then(|extension| extension.to_str())
        .and_then(|extension| FileExtension::new(extension).ok());
    let destination = cache.content(ContentStore::Downloads, hash, extension.as_ref());
    // Recorded before the file is published, and also when an identical copy is
    // already there, so a walk of the store that sees the file sees its
    // provenance too.
    record_supplied_download(cache, hash).await?;
    if stored_file_matches(&destination, None, &target.hashes)
        .await
        .unwrap_or(false)
    {
        // Another import, or an earlier pass over the same folder, already
        // adopted an identical copy.
        return Ok(());
    }
    let Some(parent) = destination.parent() else {
        return Ok(());
    };
    tokio::fs::create_dir_all(parent).await?;
    let staged = staging_path(&destination);
    let published = stage_and_publish(source, &staged, &destination, &target.hashes).await;
    if published.is_err() {
        let _ = tokio::fs::remove_file(&staged).await;
    }
    published
}

/// Copies `source` to `staged`, re-checks the copy against `hashes`, and
/// publishes it at `destination`. The candidate was hashed before this copy,
/// so the copy is verified again rather than trusting the earlier read.
async fn stage_and_publish(
    source: &Path,
    staged: &Path,
    destination: &Path,
    hashes: &[Hash],
) -> std::io::Result<()> {
    tokio::fs::copy(source, staged).await?;
    if !file_matches_hashes(staged, hashes)
        .await
        .map_err(std::io::Error::other)?
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the file changed while it was being copied into the cache",
        ));
    }
    publish_staged(staged, destination).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use ephervex_download::{ContentStore, is_supplied_download};
    use ephervex_model::{HashAlgorithm, ModProvider};
    use ephervex_pack::ProviderFileIdentity;
    use ephervex_storage::Paths;

    use super::*;

    fn app_rooted_at(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens against a temporary root")
    }

    fn identity(project: &str, file: &str) -> ProviderFileIdentity {
        ProviderFileIdentity {
            provider: Some(ModProvider::CurseForge),
            project_id: Some(project.to_owned()),
            version_id: Some(file.to_owned()),
            file_id: Some(file.to_owned()),
        }
    }

    #[tokio::test]
    async fn a_matching_file_is_adopted_and_its_identity_returned() {
        let base = tempfile::tempdir().expect("tempdir");
        let app = app_rooted_at(base.path());
        let folder = tempfile::tempdir().expect("watched folder");
        let body = b"a copy the user placed by hand";
        std::fs::write(folder.path().join("mod.jar"), body).expect("write fixture");

        let hash = Hash::new(HashAlgorithm::Sha1, &sha1_of(body)).expect("sha1");
        let target = WatchTarget {
            identity: identity("238222", "4593548"),
            file_name: Some("mod.jar".to_owned()),
            hashes: vec![hash.clone()],
        };
        let mut watcher = DownloadWatcher {
            folders: vec![folder.path().to_path_buf()],
            targets: vec![target],
            cache: app.artifact_cache().layout().clone(),
        };

        let matched = watcher.scan_once().await.expect("the file matches");
        assert_eq!(matched, identity("238222", "4593548"));
        assert!(watcher.is_finished());

        let extension = FileExtension::new("jar").expect("jar");
        let cache_path =
            app.artifact_cache()
                .layout()
                .content(ContentStore::Downloads, &hash, Some(&extension));
        assert_eq!(
            std::fs::read(&cache_path).expect("adopted copy"),
            body,
            "the cache holds a copy of the exact bytes that matched"
        );
        assert!(
            folder.path().join("mod.jar").exists(),
            "the user's own copy is left in place"
        );
        assert!(
            is_supplied_download(app.artifact_cache().layout(), &hash),
            "the adopted copy is recorded as supplied by hand"
        );
    }

    #[tokio::test]
    async fn a_non_matching_file_leaves_the_target_unresolved() {
        let base = tempfile::tempdir().expect("tempdir");
        let app = app_rooted_at(base.path());
        let folder = tempfile::tempdir().expect("watched folder");
        std::fs::write(folder.path().join("mod.jar"), b"not the expected content")
            .expect("write fixture");

        let hash = Hash::new(HashAlgorithm::Sha1, &sha1_of(b"expected content")).expect("sha1");
        let mut watcher = DownloadWatcher {
            folders: vec![folder.path().to_path_buf()],
            targets: vec![WatchTarget {
                identity: identity("238222", "4593548"),
                file_name: Some("mod.jar".to_owned()),
                hashes: vec![hash],
            }],
            cache: app.artifact_cache().layout().clone(),
        };

        assert!(watcher.scan_once().await.is_none());
        assert!(!watcher.is_finished());
    }

    #[tokio::test]
    async fn an_unreadable_folder_does_not_stop_the_others_from_being_checked() {
        let base = tempfile::tempdir().expect("tempdir");
        let app = app_rooted_at(base.path());
        let folder = tempfile::tempdir().expect("watched folder");
        let body = b"found in the second folder";
        std::fs::write(folder.path().join("mod.jar"), body).expect("write fixture");

        let hash = Hash::new(HashAlgorithm::Sha1, &sha1_of(body)).expect("sha1");
        let mut watcher = DownloadWatcher {
            folders: vec![
                base.path().join("does-not-exist"),
                folder.path().to_path_buf(),
            ],
            targets: vec![WatchTarget {
                identity: identity("238222", "4593548"),
                file_name: Some("mod.jar".to_owned()),
                hashes: vec![hash],
            }],
            cache: app.artifact_cache().layout().clone(),
        };

        assert!(watcher.scan_once().await.is_some());
    }

    fn adoption_fixture(
        body: &[u8],
    ) -> (
        tempfile::TempDir,
        App,
        tempfile::TempDir,
        DownloadWatcher,
        PathBuf,
    ) {
        let base = tempfile::tempdir().expect("tempdir");
        let app = app_rooted_at(base.path());
        let folder = tempfile::tempdir().expect("watched folder");
        std::fs::write(folder.path().join("mod.jar"), body).expect("write fixture");
        let hash = Hash::new(HashAlgorithm::Sha1, &sha1_of(body)).expect("sha1");
        let extension = FileExtension::new("jar").expect("jar");
        let cache_path =
            app.artifact_cache()
                .layout()
                .content(ContentStore::Downloads, &hash, Some(&extension));
        let watcher = DownloadWatcher {
            folders: vec![folder.path().to_path_buf()],
            targets: vec![WatchTarget {
                identity: identity("238222", "4593548"),
                file_name: Some("mod.jar".to_owned()),
                hashes: vec![hash],
            }],
            cache: app.artifact_cache().layout().clone(),
        };
        (base, app, folder, watcher, cache_path)
    }

    #[tokio::test]
    async fn a_corrupt_cache_entry_is_replaced_by_the_verified_copy() {
        let body = b"the correct bytes for the blocked file";
        for corrupt in [&b""[..], &b"the correct"[..], &b"entirely different"[..]] {
            let (_base, app, _folder, mut watcher, cache_path) = adoption_fixture(body);
            std::fs::create_dir_all(cache_path.parent().expect("parent")).expect("cache dir");
            std::fs::write(&cache_path, corrupt).expect("pre-place a bad entry");

            assert!(watcher.scan_once().await.is_some());

            assert_eq!(std::fs::read(&cache_path).expect("cache entry"), body);
            let hash = Hash::new(HashAlgorithm::Sha1, &sha1_of(body)).expect("sha1");
            assert!(
                app.cached_artifact_bytes(&[hash], Some("mod.jar"))
                    .await
                    .expect("probe")
                    .is_some(),
                "the retry sees the artifact as cached"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_verified_cache_entry_is_left_untouched_by_adoption() {
        let body = b"already adopted earlier";
        let (_base, _app, _folder, mut watcher, cache_path) = adoption_fixture(body);
        std::fs::create_dir_all(cache_path.parent().expect("parent")).expect("cache dir");
        std::fs::write(&cache_path, body).expect("pre-place a good entry");
        let before = std::fs::metadata(&cache_path).expect("metadata");

        assert!(watcher.scan_once().await.is_some());

        let after = std::fs::metadata(&cache_path).expect("metadata");
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&before),
            std::os::unix::fs::MetadataExt::ino(&after),
            "a verified entry is not republished"
        );
        assert_eq!(std::fs::read(&cache_path).expect("cache entry"), body);
    }

    #[tokio::test]
    async fn adoption_leaves_no_staging_file_behind() {
        let (_base, _app, _folder, mut watcher, cache_path) =
            adoption_fixture(b"a copy the user placed by hand");

        assert!(watcher.scan_once().await.is_some());

        let leftovers: Vec<_> = std::fs::read_dir(cache_path.parent().expect("parent"))
            .expect("cache dir")
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name != cache_path.file_name().expect("file name"))
            .collect();
        assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
    }

    fn sha1_of(bytes: &[u8]) -> String {
        use sha1::{Digest, Sha1};
        let mut hasher = Sha1::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }
}
