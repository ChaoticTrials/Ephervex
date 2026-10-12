//! Durable publication of launcher-owned files.
//!
//! A caller writes a complete sibling file, flushes its contents, and only
//! then replaces the destination. Syncing the containing directory makes the
//! rename durable across a power loss on Linux. The temporary name is unique
//! and created exclusively, so cleanup can never remove another operation's
//! work.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// Options that differ between launcher-owned files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtomicWriteOptions {
    /// Unix mode applied when the temporary file is created.
    pub mode: Option<u32>,
}

impl AtomicWriteOptions {
    /// A private file readable and writable only by its owner.
    #[must_use]
    pub const fn private() -> Self {
        Self { mode: Some(0o600) }
    }
}

/// How far a published file got toward durability.
#[derive(Debug)]
#[must_use]
pub(crate) enum AtomicPublication {
    /// The file was renamed into place and its directory entry was synced.
    Durable,
    /// The file was renamed into place and is visible to readers, but syncing
    /// its directory failed, so a power loss could still revert it.
    Unsynced(io::Error),
}

impl AtomicPublication {
    /// Treats a published-but-unsynced file as a failure.
    pub(crate) fn require_durable(self) -> io::Result<()> {
        match self {
            Self::Durable => Ok(()),
            Self::Unsynced(error) => Err(error),
        }
    }
}

/// Writes and durably publishes one complete file.
///
/// A directory sync failure after the rename is returned as an error even
/// though the new contents are already visible. Callers that must tell that
/// apart from an unpublished write use [`publish_bytes_atomic`].
pub fn write_atomic(
    path: &Path,
    options: AtomicWriteOptions,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    publish_atomic(path, options, write)?.require_durable()
}

/// Writes and publishes one complete file, distinguishing a failed write from
/// a published file whose directory sync failed.
///
/// An `Err` means the destination still holds its previous contents. After
/// `Ok` the new contents are visible whatever the [`AtomicPublication`] says.
fn publish_atomic(
    path: &Path,
    options: AtomicWriteOptions,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<AtomicPublication> {
    let temporary = temporary_path(path);
    let mut open = OpenOptions::new();
    open.create_new(true).write(true);
    #[cfg(unix)]
    if let Some(mode) = options.mode {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(mode);
    }

    let mut file = open.open(&temporary)?;
    let result = write(&mut file)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&temporary, path)
        });

    if result.is_err() {
        // This name was created with `create_new` by this call, so it is safe
        // to remove. After a successful rename it simply no longer exists.
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(match sync_parent(path) {
        Ok(()) => AtomicPublication::Durable,
        Err(error) => AtomicPublication::Unsynced(error),
    })
}

/// Writes bytes through [`write_atomic`].
pub fn write_bytes_atomic(
    path: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
) -> io::Result<()> {
    write_atomic(path, options, |file| file.write_all(bytes))
}

/// Writes bytes like [`write_bytes_atomic`], but reports a directory sync
/// failure after the rename as [`AtomicPublication::Unsynced`] instead of an
/// error.
pub(crate) fn publish_bytes_atomic(
    path: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
) -> io::Result<AtomicPublication> {
    publish_atomic(path, options, |file| file.write_all(bytes))
}

/// Syncs the directory entry containing `path`.
pub fn sync_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    // A bare file name has the empty path as its parent, which means the
    // current directory but cannot be opened.
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    File::open(parent)?.sync_all()
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".tmp.{}", uuid::Uuid::new_v4()));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_failed_write_keeps_the_previous_file_and_removes_its_own_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        std::fs::write(&path, b"old").expect("seed state");

        let error = write_atomic(&path, AtomicWriteOptions::default(), |file| {
            file.write_all(b"new")?;
            Err(io::Error::other("injected failure"))
        })
        .expect_err("the injected failure is returned");

        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(std::fs::read(&path).expect("read state"), b"old");
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("list directory")
                .count(),
            1,
            "only the original destination remains"
        );
    }

    #[test]
    fn a_bare_file_name_is_published_in_the_current_directory() {
        let name = PathBuf::from(format!("atomic-bare-name-{}.tmp", uuid::Uuid::new_v4()));

        let result = write_bytes_atomic(&name, b"new", AtomicWriteOptions::default());
        let written = std::fs::read(&name);
        let _ = std::fs::remove_file(&name);

        result.expect("a bare file name needs no directory part");
        assert_eq!(written.expect("read file"), b"new");
    }

    #[test]
    fn a_successful_write_replaces_the_file_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        std::fs::write(&path, b"old").expect("seed state");

        write_bytes_atomic(&path, b"new", AtomicWriteOptions::default()).expect("replace state");

        assert_eq!(std::fs::read(&path).expect("read state"), b"new");
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("list directory")
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_private_atomic_file_is_not_created_with_group_or_world_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("config.toml");
        write_bytes_atomic(&path, b"schema_version = 1", AtomicWriteOptions::private())
            .expect("write private file");

        let mode = std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Makes `directory` writable but not readable, so a rename into it works
    /// while opening it for a sync fails. Returns `None` when the process can
    /// open it anyway (for example as root).
    #[cfg(unix)]
    fn unsyncable_directory(directory: &Path) -> Option<impl Drop> {
        use std::os::unix::fs::PermissionsExt as _;

        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }

        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o300))
            .expect("restrict directory");
        let restore = Restore(directory.to_path_buf());
        File::open(directory).is_err().then_some(restore)
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_directory_sync_after_the_rename_is_reported_as_published_but_unsynced() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        std::fs::write(&path, b"old").expect("seed state");
        let Some(_restore) = unsyncable_directory(directory.path()) else {
            return;
        };

        let publication = publish_bytes_atomic(&path, b"new", AtomicWriteOptions::default())
            .expect("the rename succeeded");

        assert!(matches!(publication, AtomicPublication::Unsynced(_)));
        assert_eq!(std::fs::read(&path).expect("read state"), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn the_strict_writer_still_fails_when_the_directory_sync_fails() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        let Some(_restore) = unsyncable_directory(directory.path()) else {
            return;
        };

        write_bytes_atomic(&path, b"new", AtomicWriteOptions::default())
            .expect_err("an unsynced directory is an error");
        assert_eq!(std::fs::read(&path).expect("read state"), b"new");
    }

    #[test]
    fn a_failed_rename_is_an_error_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.json");
        std::fs::create_dir(&path).expect("occupy the destination with a directory");
        std::fs::write(path.join("child"), b"x").expect("make the directory non-empty");

        publish_bytes_atomic(&path, b"new", AtomicWriteOptions::default())
            .expect_err("a file cannot replace a non-empty directory");

        assert!(path.join("child").is_file(), "the destination is untouched");
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("list directory")
                .count(),
            1,
            "only the destination remains"
        );
    }
}
