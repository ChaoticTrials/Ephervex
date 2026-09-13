//! Moving an instance or server directory to a different storage root.
//!
//! # Why this is not the tree copy
//!
//! [`crate::copy`] deliberately does not follow a symlinked directory, and
//! reports it instead, because a user who links `mods/` into a shared folder
//! means "use these mods" rather than "give me a second copy of them". That is
//! right for a duplicate and wrong for a move: the instance is not gaining a
//! second existence, it is changing address, and a link it had before the move
//! is a link it still has afterwards.
//!
//! A dedicated server makes the difference concrete. Its `libraries` is a link
//! into the shared library store, so moving one with the copy's rules would
//! produce a server whose libraries are gone.
//!
//! Links are therefore recreated verbatim. Their targets lie outside the tree
//! being moved, so the same target still means the same thing at the new
//! address.
//!
//! # Why a rename is not enough on its own
//!
//! The temporary root comes from `std::env::temp_dir()`, which on a normal
//! Linux desktop is tmpfs, while the data root is on a disk. `rename` between
//! them fails with `CrossesDevices`, so copying and then removing the source is
//! the ordinary path rather than the exceptional one, and the rename is the
//! fast path for the case where both roots happen to share a filesystem.

use std::path::Path;

/// What a relocation moved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RelocationReport {
    pub files: usize,
    pub directories: usize,
    /// Symlinks recreated at the destination, pointing where they pointed.
    pub links: usize,
    /// Whether the whole tree moved as one rename, which is atomic and leaves
    /// no window in which the instance exists twice.
    pub renamed: bool,
}

/// Moves `source` to `destination`, keeping symlinks as symlinks.
///
/// `destination` must not already exist: this is a move onto a free path, not a
/// merge into an occupied one.
pub(crate) fn relocate_tree(
    source: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(usize),
) -> std::io::Result<RelocationReport> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match std::fs::rename(source, destination) {
        Ok(()) => {
            return Ok(RelocationReport {
                renamed: true,
                ..RelocationReport::default()
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {}
        Err(error) => return Err(error),
    }

    let report = copy_across_filesystems(source, destination, progress)?;
    // `remove_dir_all` unlinks a symlink rather than following it, so removing
    // the source cannot reach whatever its links pointed at.
    std::fs::remove_dir_all(source)?;
    Ok(report)
}

/// The fallback for a destination on another filesystem.
///
/// Separate from [`relocate_tree`] so it can be tested directly. Every test
/// harness puts the temporary and persistent roots under one directory, where
/// the rename above always succeeds, so this is the path users take and no
/// integration test reaches.
pub(crate) fn copy_across_filesystems(
    source: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(usize),
) -> std::io::Result<RelocationReport> {
    let mut report = RelocationReport::default();
    copy_into(source, destination, progress, &mut report)?;
    Ok(report)
}

fn copy_into(
    source: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(usize),
    report: &mut RelocationReport,
) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    report.directories += 1;

    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        // `symlink_metadata` describes the entry itself; a directory entry's
        // own `file_type` can be `Unknown` on some filesystems.
        let kind = std::fs::symlink_metadata(&from)?.file_type();

        if kind.is_symlink() {
            // The target is copied as text, without resolving it. A link whose
            // target is already gone is still the link the instance had.
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
            report.links += 1;
        } else if kind.is_dir() {
            copy_into(&from, &to, progress, report)?;
        } else if kind.is_file() {
            std::fs::copy(&from, &to)?;
            report.files += 1;
            progress(report.files);
        }
        // Anything else -- a socket, a fifo, a device node -- has no business
        // in an instance directory and is not something to reproduce.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    fn ignore_progress() -> impl FnMut(usize) {
        |_| {}
    }

    #[test]
    fn a_relocated_tree_keeps_its_shape_and_leaves_nothing_behind() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");

        write(&source.join("minecraft/options.txt"), "guiScale:2\n");
        write(&source.join("minecraft/mods/example.jar"), "not a jar");
        write(&source.join(".launcher/instance.json"), "{}");
        std::fs::create_dir_all(source.join("minecraft/saves")).expect("mkdir");

        let report =
            relocate_tree(&source, &destination, &mut ignore_progress()).expect("relocate");

        assert!(report.renamed, "one filesystem is one rename");
        assert!(!source.exists(), "the instance is not in two places");
        assert_eq!(
            std::fs::read_to_string(destination.join("minecraft/options.txt")).expect("read"),
            "guiScale:2\n"
        );
        assert!(destination.join("minecraft/mods/example.jar").is_file());
        assert!(destination.join("minecraft/saves").is_dir());
        assert!(destination.join(".launcher/instance.json").is_file());
    }

    /// The case the tree copy gets deliberately wrong. A server's `libraries`
    /// is a link into the shared store, so a move that dropped it would produce
    /// a server that cannot start.
    #[test]
    fn a_symlinked_directory_survives_a_relocation_as_a_link() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        let shared = base.path().join("shared/libraries");
        write(&shared.join("net/example/library.jar"), "a shared library");

        std::fs::create_dir_all(source.join("server")).expect("mkdir");
        std::os::unix::fs::symlink(&shared, source.join("server/libraries")).expect("link");
        write(
            &source.join("server/server.properties"),
            "online-mode=false\n",
        );

        let report =
            copy_across_filesystems(&source, &destination, &mut ignore_progress()).expect("copy");

        assert_eq!(report.links, 1);
        assert_eq!(
            report.files, 1,
            "the link's contents are not copied by value"
        );
        let moved = destination.join("server/libraries");
        assert!(
            std::fs::symlink_metadata(&moved)
                .expect("the link is there")
                .file_type()
                .is_symlink(),
            "the link has to arrive as a link, not as a copy of what it points at"
        );
        assert_eq!(std::fs::read_link(&moved).expect("target"), shared);
        assert!(
            moved.join("net/example/library.jar").is_file(),
            "and it still resolves to the shared store"
        );
    }

    #[test]
    fn a_symlinked_file_arrives_as_a_link_rather_than_as_its_contents() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        let outside = base.path().join("outside/debug.jar");
        write(&outside, "a local build under test");

        std::fs::create_dir_all(source.join("minecraft/mods")).expect("mkdir");
        std::os::unix::fs::symlink(&outside, source.join("minecraft/mods/debug.jar"))
            .expect("link");

        copy_across_filesystems(&source, &destination, &mut ignore_progress()).expect("copy");

        let moved = destination.join("minecraft/mods/debug.jar");
        assert!(
            std::fs::symlink_metadata(&moved)
                .expect("the link is there")
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&moved).expect("target"), outside);
    }

    /// A link into the instance's own tree keeps working, because the shape
    /// around it is the same at the new address.
    #[test]
    fn a_relative_link_inside_the_tree_still_resolves_after_the_move() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");

        write(&source.join("minecraft/config/example.toml"), "value = 1\n");
        std::os::unix::fs::symlink("config/example.toml", source.join("minecraft/current.toml"))
            .expect("link");

        copy_across_filesystems(&source, &destination, &mut ignore_progress()).expect("copy");

        assert_eq!(
            std::fs::read_to_string(destination.join("minecraft/current.toml")).expect("read"),
            "value = 1\n"
        );
    }

    #[test]
    fn a_broken_link_is_carried_over_rather_than_failing_the_move() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");

        std::fs::create_dir_all(&source).expect("mkdir");
        std::os::unix::fs::symlink(base.path().join("nothing-here"), source.join("dangling"))
            .expect("link");
        write(&source.join("real.txt"), "still copied");

        let report =
            copy_across_filesystems(&source, &destination, &mut ignore_progress()).expect("copy");

        assert_eq!(report.links, 1);
        assert_eq!(report.files, 1);
        assert!(
            std::fs::symlink_metadata(destination.join("dangling"))
                .expect("the link is there")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_failed_relocation_leaves_the_source_where_it_was() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        // A file, so creating the destination directory under it cannot work.
        let destination = base.path().join("occupied/destination");
        write(&source.join("minecraft/options.txt"), "guiScale:2\n");
        write(&base.path().join("occupied"), "not a directory");

        let error = relocate_tree(&source, &destination, &mut ignore_progress())
            .expect_err("a destination under a file cannot be created");

        assert!(source.join("minecraft/options.txt").is_file(), "{error}");
    }

    #[test]
    fn every_copied_file_is_reported_as_it_lands() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        write(&source.join("minecraft/mods/one.jar"), "one");
        write(&source.join("minecraft/mods/two.jar"), "two");
        write(&source.join("minecraft/options.txt"), "three");

        let mut seen = Vec::new();
        copy_across_filesystems(&source, &destination, &mut |count| seen.push(count))
            .expect("copy");

        assert_eq!(
            seen,
            vec![1, 2, 3],
            "a multi-gigabyte move has to be able to say how far along it is"
        );
    }
}
