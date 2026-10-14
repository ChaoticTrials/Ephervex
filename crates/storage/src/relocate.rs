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
//!
//! # Why a copy is staged
//!
//! A copy takes as long as the tree is large, and an interruption must never
//! leave part of a host at the address the host is moving to: reconciliation
//! would take it for the whole host once the source is gone. So a copy is made
//! into a hidden directory beside the destination, flushed, and renamed into
//! place in one step (ADR 0025).

use std::path::{Path, PathBuf};

use crate::removal::remove_tree_if_present;

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

/// What a staging directory's name ends with.
const STAGE_SUFFIX: &str = ".moving";

/// Moves `source` to `destination`, keeping symlinks as symlinks.
///
/// `destination` must not already exist: this is a move onto a free path, not a
/// merge into an occupied one. The tree arrives there in one rename either way.
/// A copy leaves `source` in place for the caller to remove once the move is
/// recorded. The caller holds the move's claim, so a staging directory an
/// interrupted copy left is this move's to replace.
pub(crate) fn relocate_tree(
    source: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(usize),
) -> std::io::Result<RelocationReport> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    discard_stage(destination)?;

    if rename_is_allowed() {
        match std::fs::rename(source, destination) {
            Ok(()) => {
                // Durable before the caller records the move, like a copy.
                crate::atomic::sync_parent(destination)?;
                crate::atomic::sync_parent(source)?;
                return Ok(RelocationReport {
                    renamed: true,
                    ..RelocationReport::default()
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {}
            Err(error) => return Err(error),
        }
    }

    copy_through_stage(source, destination, progress)
}

/// The fallback for a destination on another filesystem.
fn copy_through_stage(
    source: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(usize),
) -> std::io::Result<RelocationReport> {
    let stage = stage_for(destination)?;
    let mut report = RelocationReport::default();
    let copied = copy_into(source, &stage, progress, &mut report)
        .and_then(|()| std::fs::rename(&stage, destination));
    if let Err(error) = copied {
        // Best effort: the next move of this host discards it too.
        let _ = discard_stage(destination);
        return Err(error);
    }
    // The rename has to be durable before the caller records the move, or a
    // crash could leave a record naming a directory that is not there.
    if let Err(error) = crate::atomic::sync_parent(destination) {
        let _ = discard_tree(destination);
        return Err(error);
    }
    Ok(report)
}

/// Deletes the tree at `path`.
///
/// Renamed aside first, so a deletion that fails partway leaves nothing at
/// `path` that could pass for the host.
pub(crate) fn discard_tree(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_DISCARD_OF.with_borrow(|refused| refused.as_deref() == Some(path)) {
        return Err(std::io::Error::other("discarding is refused"));
    }
    discard_stage(path)?;
    match std::fs::rename(path, stage_for(path)?) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(()) => {}
    }
    discard_stage(path)
}

/// Where a copy into `destination` is made before it is renamed into place:
/// hidden, and beside it so that rename stays on one filesystem.
fn stage_for(destination: &Path) -> std::io::Result<PathBuf> {
    let name = destination
        .file_name()
        .ok_or_else(|| std::io::Error::other("a move's destination has no name"))?;
    let mut hidden = std::ffi::OsString::from(".");
    hidden.push(name);
    hidden.push(STAGE_SUFFIX);
    Ok(destination.with_file_name(hidden))
}

/// The destination name of a copy that was never renamed into place, if
/// `directory` is one.
pub(crate) fn staged_name(directory: &Path) -> Option<&str> {
    directory
        .file_name()?
        .to_str()?
        .strip_prefix('.')?
        .strip_suffix(STAGE_SUFFIX)
}

fn discard_stage(destination: &Path) -> std::io::Result<()> {
    remove_tree_if_present(&stage_for(destination)?)
}

/// Removes a staging directory that holds nothing but directories, which is
/// all a copy killed before its first file landed has made. Returns whether it
/// was removed.
///
/// A file, link, or other entry anywhere in the tree is copied content, and the
/// whole tree is then left alone. The caller holds the host's claim, so no move
/// is filling the stage while it is checked and removed.
pub(crate) fn discard_stage_without_content(stage: &Path) -> std::io::Result<bool> {
    if !holds_only_directories(stage)? {
        return Ok(false);
    }
    remove_directories(stage)?;
    Ok(true)
}

fn holds_only_directories(path: &Path) -> std::io::Result<bool> {
    if !std::fs::symlink_metadata(path)?.file_type().is_dir() {
        return Ok(false);
    }
    for entry in std::fs::read_dir(path)? {
        if !holds_only_directories(&entry?.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Removes directories deepest first with `remove_dir`, which refuses a
/// directory that is not empty, so an entry that appeared since the check
/// survives. Anything that is no longer a real directory is an error, so a
/// link swapped in since the check is never entered.
fn remove_directories(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(std::io::Error::other(
            "a staging entry stopped being a directory",
        ));
    }
    // A copied directory can carry a mode that forbids removing its entries.
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700))?;
    }
    for entry in std::fs::read_dir(path)? {
        remove_directories(&entry?.path())?;
    }
    std::fs::remove_dir(path)
}

fn copy_into(
    source: &Path,
    destination: &Path,
    progress: &mut dyn FnMut(usize),
    report: &mut RelocationReport,
) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let source_permissions =
        std::fs::Permissions::from_mode(std::fs::metadata(source)?.permissions().mode() & 0o7777);
    // Private while it fills; the source's mode is applied after its entries.
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
        .create(destination)?;
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
            copy_file(&from, &to)?;
            report.files += 1;
            progress(report.files);
        }
        // Anything else -- a socket, a fifo, a device node -- has no business
        // in an instance directory and is not something to reproduce.
    }
    // Opened before the mode narrows, so even an unreadable directory flushes.
    // Flushed before the rename that publishes it, like every file in it.
    let directory = std::fs::File::open(destination)?;
    directory.set_permissions(source_permissions)?;
    directory.sync_all()
}

fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut input = std::fs::File::open(from)?;
    let permissions = input.metadata()?.permissions();
    // Created with the source's mode, so a private file is never readable by
    // others while it is copied.
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(permissions.mode())
        .open(to)?;
    std::io::copy(&mut input, &mut output)?;
    // The mode above is narrowed by the umask; the source's is kept exactly.
    output.set_permissions(permissions)?;
    output.sync_all()
}

#[cfg(test)]
thread_local! {
    static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_DISCARD_OF: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Makes this thread's removal of the tree at `path` fail, so a move whose old
/// copy cannot be removed is reachable without depending on file permissions.
/// `None` restores removal.
#[cfg(test)]
pub(crate) fn fail_discarding(path: Option<PathBuf>) {
    FAIL_DISCARD_OF.set(path);
}

/// Makes this thread's moves copy even where a rename would work. Every test
/// environment keeps its roots on one filesystem, where the copy users take
/// from a tmpfs temporary root is otherwise never reached.
#[cfg(test)]
pub(crate) fn force_copy(on: bool) {
    FORCE_COPY.set(on);
}

fn rename_is_allowed() -> bool {
    #[cfg(test)]
    {
        !FORCE_COPY.get()
    }
    #[cfg(not(test))]
    {
        true
    }
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
            copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

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

        copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

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

        copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

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
            copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

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
    fn an_interrupted_copy_leaves_nothing_at_the_destination() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        for name in ["one.jar", "two.jar", "three.jar"] {
            write(&source.join("minecraft/mods").join(name), name);
        }

        crate::test_support::interrupt_copy_after(&source, &destination, 1);

        assert!(!destination.exists());
        assert_eq!(
            staged_name(&stage_for(&destination).expect("stage")),
            Some("destination")
        );
        assert!(source.join("minecraft/mods/one.jar").is_file());
    }

    #[test]
    fn a_move_replaces_what_an_interrupted_copy_left() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        for name in ["one.jar", "two.jar", "three.jar"] {
            write(&source.join("minecraft/mods").join(name), name);
        }
        crate::test_support::interrupt_copy_after(&source, &destination, 1);

        force_copy(true);
        let report = relocate_tree(&source, &destination, &mut ignore_progress()).expect("move");
        force_copy(false);

        assert!(!report.renamed);
        assert_eq!(report.files, 3);
        assert!(!stage_for(&destination).expect("stage").exists());
        assert!(
            source.is_dir(),
            "the caller removes the source once it is recorded"
        );
    }

    #[test]
    fn a_copied_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        write(&source.join("server/run.sh"), "#!/bin/sh\n");
        std::fs::set_permissions(
            source.join("server/run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");

        copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

        let mode = std::fs::metadata(destination.join("server/run.sh"))
            .expect("copied")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o7777
    }

    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn copied_directories_keep_their_permissions_whatever_the_umask() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        write(&source.join("private/world/level.dat"), "data");
        write(&source.join("open/readme.txt"), "text");
        chmod(&source.join("private/world"), 0o700);
        chmod(&source.join("private"), 0o750);
        chmod(&source.join("open"), 0o777);
        chmod(&source, 0o700);

        copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

        assert_eq!(mode_of(&destination), 0o700, "the tree root");
        assert_eq!(mode_of(&destination.join("private")), 0o750);
        assert_eq!(mode_of(&destination.join("private/world")), 0o700);
        assert_eq!(mode_of(&destination.join("open")), 0o777);
    }

    #[test]
    fn a_read_only_directory_is_copied_with_its_entries_and_its_mode() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        write(&source.join("frozen/inner/file.txt"), "kept");
        chmod(&source.join("frozen/inner"), 0o500);
        chmod(&source.join("frozen"), 0o555);

        let copied = copy_through_stage(&source, &destination, &mut ignore_progress());

        // Let the temporary directory clean up after itself.
        remove_tree_if_present(&source).expect("remove source");
        copied.expect("copy");
        assert_eq!(mode_of(&destination.join("frozen")), 0o555);
        assert_eq!(mode_of(&destination.join("frozen/inner")), 0o500);
        assert_eq!(
            std::fs::read_to_string(destination.join("frozen/inner/file.txt")).expect("read"),
            "kept"
        );
        discard_tree(&destination).expect("a read-only tree can be discarded");
        assert!(!destination.exists());
    }

    #[test]
    fn an_interrupted_copy_of_read_only_directories_is_replaced_by_the_next_move() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        write(&source.join("frozen/one.jar"), "one");
        write(&source.join("frozen/two.jar"), "two");
        chmod(&source.join("frozen"), 0o555);
        // A copy that finished but was never renamed into place.
        let stage = stage_for(&destination).expect("stage");
        let mut leftover = RelocationReport::default();
        copy_into(&source, &stage, &mut ignore_progress(), &mut leftover).expect("stage");
        assert_eq!(mode_of(&stage.join("frozen")), 0o555);

        force_copy(true);
        let moved = relocate_tree(&source, &destination, &mut ignore_progress());
        force_copy(false);

        remove_tree_if_present(&source).expect("remove source");
        let report = moved.expect("the leftover stage is discarded");
        assert_eq!(report.files, 2);
        assert_eq!(mode_of(&destination.join("frozen")), 0o555);
        assert!(!stage_for(&destination).expect("stage").exists());
        discard_tree(&destination).expect("discard");
    }

    #[test]
    fn removing_a_staging_skeleton_never_enters_a_link_swapped_in_for_a_directory() {
        let base = tempfile::tempdir().expect("temp dir");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(outside.join("inner")).expect("mkdir");
        chmod(&outside.join("inner"), 0o500);
        let stage = base.path().join("stage");
        std::fs::create_dir_all(&stage).expect("mkdir");
        std::os::unix::fs::symlink(&outside, stage.join("mods")).expect("link");

        remove_directories(&stage).expect_err("a link is not a directory to remove");

        assert!(outside.join("inner").is_dir());
        assert_eq!(mode_of(&outside.join("inner")), 0o500);
        chmod(&outside.join("inner"), 0o700);
    }

    #[test]
    fn a_staging_directory_is_kept_when_a_link_sits_anywhere_in_it() {
        let base = tempfile::tempdir().expect("temp dir");
        let stage = base.path().join("stage");
        std::fs::create_dir_all(stage.join("minecraft")).expect("mkdir");
        std::os::unix::fs::symlink(base.path(), stage.join("minecraft/linked")).expect("link");

        assert!(!discard_stage_without_content(&stage).expect("checked"));
        assert!(stage.join("minecraft").is_dir());
    }

    #[test]
    fn a_staging_skeleton_without_content_is_discarded() {
        let base = tempfile::tempdir().expect("temp dir");
        let stage = base.path().join("stage");
        std::fs::create_dir_all(stage.join("minecraft/mods")).expect("mkdir");

        assert!(discard_stage_without_content(&stage).expect("discard"));
        assert!(!stage.exists());
    }

    #[test]
    fn a_discarded_tree_leaves_nothing_behind() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");
        write(&source.join("minecraft/options.txt"), "guiScale:2\n");
        copy_through_stage(&source, &destination, &mut ignore_progress()).expect("copy");

        discard_tree(&destination).expect("discard");

        assert!(!destination.exists());
        assert!(!stage_for(&destination).expect("stage").exists());
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
        copy_through_stage(&source, &destination, &mut |count| seen.push(count)).expect("copy");

        assert_eq!(
            seen,
            vec![1, 2, 3],
            "a multi-gigabyte move has to be able to say how far along it is"
        );
    }
}
