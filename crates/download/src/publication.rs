//! Changes to a cache entry that must not interleave across processes.
//!
//! Publishing renames a verified file over a destination. Discarding removes a
//! file that failed revalidation. If a discard acts on a stale verdict after
//! another process has published, it deletes an artifact that was just
//! verified and returned to its caller, and content-addressed outputs that no
//! source can serve again are lost for good.
//!
//! Both steps therefore run under one exclusive advisory lock on the
//! destination's directory, and a discard only removes the exact file that was
//! inspected. The lock is held for a single filesystem call, never while
//! hashing or downloading.

use std::fs::Metadata;
use std::io;
use std::path::Path;

use tracing::debug;

/// Device and inode of a file, which tell one publication from the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    /// `None` where the platform exposes no identity, which a caller treats as
    /// "cannot prove it is the same file".
    #[cfg(unix)]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the platform without an identity to report is the other branch"
    )]
    pub(crate) fn of(metadata: &Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        Some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn of(_metadata: &Metadata) -> Option<Self> {
        None
    }
}

/// What a discard found at the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Discard {
    /// The inspected file is gone, removed now or by someone else.
    Removed,
    /// A different file now sits at the path and was left alone.
    Replaced,
}

/// Holds the directory's exclusive lock until dropped.
struct DirectoryLock {
    _file: Option<std::fs::File>,
}

/// Blocks until the lock is held.
///
/// A filesystem that cannot lock a directory leaves the caller with the
/// identity check alone, which narrows the window without closing it.
#[cfg(unix)]
fn lock_directory(directory: &Path) -> DirectoryLock {
    use rustix::fs::{FlockOperation, flock};

    let Ok(file) = std::fs::File::open(directory) else {
        return DirectoryLock { _file: None };
    };
    loop {
        match flock(&file, FlockOperation::LockExclusive) {
            Ok(()) => return DirectoryLock { _file: Some(file) },
            Err(rustix::io::Errno::INTR) => {}
            Err(errno) => {
                debug!(path = %directory.display(), %errno, "directory lock unavailable");
                return DirectoryLock { _file: None };
            }
        }
    }
}

#[cfg(not(unix))]
fn lock_directory(_directory: &Path) -> DirectoryLock {
    DirectoryLock { _file: None }
}

fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(io::Error::other(error)),
    }
}

/// Renames `from` over `to`, serialized with every discard in `to`'s directory.
pub(crate) async fn rename_into_place(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (from.to_path_buf(), to.to_path_buf());
    blocking(move || {
        let _lock = lock_directory(parent_of(&to));
        std::fs::rename(&from, &to)
    })
    .await
}

/// Removes `path` only while it is still the file with `inspected` identity.
///
/// The caller keeps the inspected file open until this returns, so its inode
/// number cannot have been reused by a later publication.
pub(crate) async fn discard_unless_replaced(
    path: &Path,
    inspected: FileIdentity,
) -> io::Result<Discard> {
    let path = path.to_path_buf();
    blocking(move || {
        let _lock = lock_directory(parent_of(&path));
        match std::fs::symlink_metadata(&path) {
            Ok(current) if FileIdentity::of(&current) == Some(inspected) => {
                std::fs::remove_file(&path)?;
                Ok(Discard::Removed)
            }
            Ok(_) => Ok(Discard::Replaced),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Discard::Removed),
            Err(error) => Err(error),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::time::Duration;

    use super::*;

    fn identity(path: &Path) -> FileIdentity {
        FileIdentity::of(&std::fs::symlink_metadata(path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn discard_removes_the_inspected_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, b"corrupt").unwrap();

        let outcome = discard_unless_replaced(&path, identity(&path))
            .await
            .unwrap();

        assert_eq!(outcome, Discard::Removed);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn discard_leaves_a_file_published_after_the_inspection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, b"corrupt").unwrap();
        let inspected = identity(&path);
        let _pinned = std::fs::File::open(&path).unwrap();

        let staged = dir.path().join("staged");
        std::fs::write(&staged, b"verified").unwrap();
        rename_into_place(&staged, &path).await.unwrap();

        let outcome = discard_unless_replaced(&path, inspected).await.unwrap();

        assert_eq!(outcome, Discard::Replaced);
        assert_eq!(std::fs::read(&path).unwrap(), b"verified");
    }

    #[tokio::test]
    async fn discard_of_a_file_someone_else_removed_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, b"corrupt").unwrap();
        let inspected = identity(&path);
        let _pinned = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        let outcome = discard_unless_replaced(&path, inspected).await.unwrap();

        assert_eq!(outcome, Discard::Removed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_waits_while_another_holder_has_the_directory_locked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        let staged = dir.path().join("staged");
        std::fs::write(&staged, b"verified").unwrap();

        let holder = lock_directory(dir.path());
        let publishing = tokio::spawn({
            let (staged, path) = (staged.clone(), path.clone());
            async move { rename_into_place(&staged, &path).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!path.exists(), "published while the directory was locked");

        drop(holder);
        publishing.await.unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"verified");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discarding_waits_while_another_holder_has_the_directory_locked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, b"corrupt").unwrap();
        let inspected = identity(&path);

        let holder = lock_directory(dir.path());
        let discarding = tokio::spawn({
            let path = path.clone();
            async move { discard_unless_replaced(&path, inspected).await }
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(path.exists(), "removed while the directory was locked");

        drop(holder);
        assert_eq!(discarding.await.unwrap().unwrap(), Discard::Removed);
        assert!(!path.exists());
    }
}
