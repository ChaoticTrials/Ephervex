//! Copy-on-write cloning for local safety snapshots.
//!
//! A reflink is attempted for every regular file. It is a kernel-guaranteed
//! copy-on-write clone, unlike a hard link, so later Minecraft writes cannot
//! change the captured bytes. Filesystems without reflink support fall back to
//! an ordinary byte copy.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::Path;

/// The least space-efficient method used by a tree clone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloneMethod {
    Reflink,
    Copy,
}

/// What cloning a directory tree produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneReport {
    pub method: CloneMethod,
    pub apparent_bytes: u64,
    pub files: usize,
    pub directories: usize,
    pub symlinks: usize,
}

impl Default for CloneReport {
    fn default() -> Self {
        Self {
            method: CloneMethod::Reflink,
            apparent_bytes: 0,
            files: 0,
            directories: 0,
            symlinks: 0,
        }
    }
}

/// Clones one regular file, preferring a reflink and falling back to a copy.
pub fn clone_file(source: &Path, destination: &Path) -> std::io::Result<CloneMethod> {
    clone_file_checked(source, destination, &mut || Ok(()))
}

fn clone_file_checked(
    source: &Path,
    destination: &Path,
    check: &mut impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<CloneMethod> {
    check()?;
    if !std::fs::symlink_metadata(source)?.is_file() {
        return Err(std::io::Error::other(
            "snapshot source is not a regular file",
        ));
    }
    let input = std::fs::File::open(source)?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;

    if rustix::fs::ioctl_ficlone(&output, &input).is_ok() {
        output.sync_all()?;
        std::fs::set_permissions(destination, input.metadata()?.permissions())?;
        Ok(CloneMethod::Reflink)
    } else {
        copy_checked(&input, &output, check)?;
        std::fs::set_permissions(destination, input.metadata()?.permissions())?;
        output.sync_all()?;
        Ok(CloneMethod::Copy)
    }
}

fn copy_checked(
    mut input: &std::fs::File,
    mut output: &std::fs::File,
    check: &mut impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        check()?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        output.write_all(&buffer[..count])?;
    }
}

/// Clones a tree while excluding entries with one of the supplied names.
///
/// Symlinks are recreated with their original target text and are never
/// followed, including broken links and links to directories outside the tree.
pub fn clone_tree(
    source: &Path,
    destination: &Path,
    excluded: &[&str],
) -> std::io::Result<CloneReport> {
    clone_tree_with_check(source, destination, excluded, &mut || Ok(()))
}

/// Clones a tree, checking for cancellation before each entry and copy chunk.
pub fn clone_tree_with_check(
    source: &Path,
    destination: &Path,
    excluded: &[&str],
    check: &mut impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<CloneReport> {
    let mut report = CloneReport::default();
    clone_into(source, destination, excluded, &mut report, check)?;
    Ok(report)
}

fn clone_into(
    source: &Path,
    destination: &Path,
    excluded: &[&str],
    report: &mut CloneReport,
    check: &mut impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    check()?;
    let source_metadata = std::fs::symlink_metadata(source)?;
    if !source_metadata.is_dir() {
        return Err(std::io::Error::other("snapshot source is not a directory"));
    }
    std::fs::create_dir(destination)?;
    report.directories += 1;

    for entry in std::fs::read_dir(source)? {
        check()?;
        let entry = entry?;
        let name = entry.file_name();
        if excluded
            .iter()
            .any(|excluded| std::ffi::OsStr::new(excluded) == name)
        {
            continue;
        }

        let from = entry.path();
        let to = destination.join(&name);
        let metadata = std::fs::symlink_metadata(&from)?;
        let kind = metadata.file_type();
        if kind.is_symlink() {
            symlink(std::fs::read_link(&from)?, &to)?;
            report.symlinks += 1;
        } else if kind.is_dir() {
            clone_into(&from, &to, excluded, report, check)?;
        } else if kind.is_file() {
            if clone_file_checked(&from, &to, check)? == CloneMethod::Copy {
                report.method = CloneMethod::Copy;
            }
            report.apparent_bytes = report.apparent_bytes.saturating_add(metadata.len());
            report.files += 1;
        } else {
            return Err(std::io::Error::other("snapshot contains a special file"));
        }
    }
    std::fs::set_permissions(destination, source_metadata.permissions())?;
    std::fs::File::open(destination)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn write(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        std::fs::write(path, contents).expect("write file");
    }

    #[test]
    fn fallback_copy_checks_cancellation_between_chunks() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("source");
        write(&source, &vec![7; 3 * 1024 * 1024]);
        let input = std::fs::File::open(source).expect("input");
        let output = std::fs::File::create(temporary.path().join("copy")).expect("output");
        let mut checks = 0;
        let result = copy_checked(&input, &output, &mut || {
            checks += 1;
            if checks == 2 {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        });
        assert_eq!(
            result.expect_err("canceled").kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(output.metadata().expect("metadata").len(), 1024 * 1024);
    }

    #[test]
    fn a_cloned_file_never_shares_its_inode_with_the_source() {
        use std::os::unix::fs::MetadataExt;
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("copy");
        write(&source, b"original");
        clone_file(&source, &destination).expect("clone");
        assert_ne!(
            std::fs::metadata(&source).expect("source").ino(),
            std::fs::metadata(&destination).expect("copy").ino()
        );
        std::fs::write(source, b"modified").expect("modify");
        assert_eq!(std::fs::read(destination).expect("read"), b"original");
    }

    #[test]
    fn a_cloned_tree_is_byte_identical_whichever_method_the_filesystem_supports() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        let contents = b"snapshot bytes\0with binary data";
        write(&source.join("nested/file.bin"), contents);

        let report = clone_tree(&source, &destination, &[]).expect("clone tree");

        assert_eq!(
            std::fs::read(destination.join("nested/file.bin")).expect("read clone"),
            contents
        );
        assert_eq!(report.apparent_bytes, contents.len() as u64);
        assert_eq!(report.files, 1);
    }

    #[test]
    fn excluded_names_are_skipped_at_every_depth() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        write(&source.join("keep.txt"), b"keep");
        write(&source.join("nested/snapshots/history"), b"skip");
        write(&source.join("snapshots/history"), b"skip too");

        clone_tree(&source, &destination, &["snapshots"]).expect("clone tree");

        assert!(destination.join("keep.txt").is_file());
        assert!(!destination.join("snapshots").exists());
        assert!(!destination.join("nested/snapshots").exists());
    }

    #[test]
    fn symlinks_and_broken_symlinks_are_recreated_without_being_followed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        std::fs::create_dir(&source).expect("create source");
        symlink("../shared/libraries", source.join("libraries")).expect("library link");
        symlink("missing.jar", source.join("broken.jar")).expect("broken link");

        let report = clone_tree(&source, &destination, &[]).expect("clone tree");

        assert_eq!(report.symlinks, 2);
        assert_eq!(
            std::fs::read_link(destination.join("libraries")).expect("read library link"),
            std::path::PathBuf::from("../shared/libraries")
        );
        assert_eq!(
            std::fs::read_link(destination.join("broken.jar")).expect("read broken link"),
            std::path::PathBuf::from("missing.jar")
        );
    }
}
