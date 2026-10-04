//! Copying an instance directory.
//!
//! # Correctness before cleverness
//!
//! Reflinks and copy-on-write remain optional optimizations; this module
//! says "correctness comes first". This is the plain implementation: read the
//! bytes, write the bytes. A duplicated instance that shares extents with its
//! original would be a fine optimization and a terrible bug if the sharing
//! turned out not to be copy-on-write after all.
//!
//! # What is deliberately not copied
//!
//! **Symlinked directories.** A user who symlinks `mods/` into a shared
//! development folder means "use these mods", not "give me a second copy of
//! them", and following the link would also let a duplication walk into a loop
//! or out of the instance entirely. They are skipped and reported, so the
//! caller can tell the user rather than leaving them to discover an
//! almost-complete copy. Symlinked *files* are copied by content, which is what
//! a duplicate of a single linked jar should be.
//!
//! Anything else is the caller's to skip: a duplicate leaves out launch state
//! (`lock::is_launch_state`), which describes processes the copy is not.

use std::path::{Path, PathBuf};

/// What a copy left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CopyReport {
    pub files: usize,
    pub directories: usize,
    /// Symlinked directories that were not followed, relative to the source
    /// root. Reported rather than silently skipped.
    pub skipped_links: Vec<PathBuf>,
}

/// Copies `source` into `destination`, skipping every entry `skip` accepts.
///
/// `skip` sees each entry's path relative to `source`.
///
/// `destination` is created if it does not exist. Existing files are
/// overwritten, which only matters to callers that copy into a non-empty
/// directory - the registry does not.
pub(crate) fn copy_tree(
    source: &Path,
    destination: &Path,
    skip: &dyn Fn(&Path) -> bool,
) -> std::io::Result<CopyReport> {
    let mut report = CopyReport::default();
    copy_into(source, destination, source, skip, &mut report)?;
    Ok(report)
}

fn copy_into(
    source: &Path,
    destination: &Path,
    root: &Path,
    skip: &dyn Fn(&Path) -> bool,
    report: &mut CopyReport,
) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    report.directories += 1;

    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let from = entry.path();
        if skip(from.strip_prefix(root).unwrap_or(&from)) {
            continue;
        }

        let to = destination.join(&name);
        // `symlink_metadata` describes the entry itself; `file_type` from the
        // directory entry can be `Unknown` on some filesystems.
        let kind = std::fs::symlink_metadata(&from)?.file_type();

        if kind.is_symlink() {
            // Resolving the target decides only whether this is a file to copy
            // or a directory to skip; the copy itself still reads through the
            // link, so a broken one is skipped rather than failing the copy.
            match std::fs::metadata(&from) {
                Ok(target) if target.is_file() => {
                    std::fs::copy(&from, &to)?;
                    report.files += 1;
                }
                Ok(_) | Err(_) => {
                    report
                        .skipped_links
                        .push(from.strip_prefix(root).unwrap_or(&from).to_path_buf());
                }
            }
            continue;
        }

        if kind.is_dir() {
            copy_into(&from, &to, root, skip, report)?;
        } else if kind.is_file() {
            std::fs::copy(&from, &to)?;
            report.files += 1;
        }
        // Anything else - a socket, a fifo, a device node - has no business in
        // an instance directory and is not something to reproduce.
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

    #[test]
    fn a_tree_is_copied_with_its_contents_and_its_shape() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");

        write(&source.join("minecraft/options.txt"), "guiScale:2\n");
        write(
            &source.join("minecraft/mods/example.jar"),
            "not really a jar",
        );
        std::fs::create_dir_all(source.join("minecraft/saves")).expect("mkdir");

        let report = copy_tree(&source, &destination, &|_| false).expect("copy");
        assert_eq!(report.files, 2);
        assert!(report.skipped_links.is_empty());

        assert_eq!(
            std::fs::read_to_string(destination.join("minecraft/options.txt")).expect("read"),
            "guiScale:2\n"
        );
        assert!(destination.join("minecraft/mods/example.jar").is_file());
        assert!(
            destination.join("minecraft/saves").is_dir(),
            "an empty directory is part of the shape"
        );
    }

    #[test]
    fn a_skipped_path_is_judged_relative_to_the_source() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        let destination = base.path().join("destination");

        write(&source.join(".launcher/launch.lock"), "pid");
        write(&source.join(".launcher/instance.json"), "{}");
        write(&source.join("minecraft/launch.lock"), "a game file");

        copy_tree(&source, &destination, &|relative| {
            relative == Path::new(".launcher/launch.lock")
        })
        .expect("copy");
        assert!(!destination.join(".launcher/launch.lock").exists());
        assert!(destination.join("minecraft/launch.lock").is_file());
        assert!(destination.join(".launcher/instance.json").is_file());
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_file_is_copied_by_content_and_a_symlinked_directory_is_not_followed() {
        // Following a directory link could walk out of the instance, or into a
        // loop; copying one file through a link is what a duplicate wants.
        let base = tempfile::tempdir().expect("temp dir");
        let outside = base.path().join("outside");
        write(&outside.join("shared.jar"), "shared");
        write(&outside.join("linked-dir/inside.txt"), "inside");

        let source = base.path().join("source");
        std::fs::create_dir_all(source.join("minecraft")).expect("mkdir");
        std::os::unix::fs::symlink(
            outside.join("shared.jar"),
            source.join("minecraft/link.jar"),
        )
        .expect("symlink");
        std::os::unix::fs::symlink(outside.join("linked-dir"), source.join("minecraft/mods"))
            .expect("symlink");

        let destination = base.path().join("destination");
        let report = copy_tree(&source, &destination, &|_| false).expect("copy");

        assert_eq!(
            std::fs::read_to_string(destination.join("minecraft/link.jar")).expect("read"),
            "shared"
        );
        assert_eq!(
            report.skipped_links,
            vec![PathBuf::from("minecraft/mods")],
            "a skipped link has to be reportable, not silent"
        );
        assert!(!destination.join("minecraft/mods").exists());
        assert!(
            outside.join("linked-dir/inside.txt").is_file(),
            "the copy must not have touched what the link pointed at"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_broken_symlink_is_skipped_rather_than_failing_the_copy() {
        let base = tempfile::tempdir().expect("temp dir");
        let source = base.path().join("source");
        std::fs::create_dir_all(&source).expect("mkdir");
        std::os::unix::fs::symlink(base.path().join("nothing-here"), source.join("dangling"))
            .expect("symlink");

        let destination = base.path().join("destination");
        let report = copy_tree(&source, &destination, &|_| false).expect("copy");
        assert_eq!(report.skipped_links, vec![PathBuf::from("dangling")]);
        assert_eq!(report.files, 0);
    }
}
