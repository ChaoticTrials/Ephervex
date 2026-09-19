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

/// Writes and durably publishes one complete file.
pub fn write_atomic(
    path: &Path,
    options: AtomicWriteOptions,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
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
        })
        .and_then(|()| sync_parent(path));

    if result.is_err() {
        // This name was created with `create_new` by this call, so it is safe
        // to remove. After a successful rename it simply no longer exists.
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Writes bytes through [`write_atomic`].
pub fn write_bytes_atomic(
    path: &Path,
    bytes: &[u8],
    options: AtomicWriteOptions,
) -> io::Result<()> {
    write_atomic(path, options, |file| file.write_all(bytes))
}

/// Syncs the directory entry containing `path`.
pub fn sync_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
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
}
