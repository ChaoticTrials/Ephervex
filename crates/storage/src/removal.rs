//! Removing directory trees that may hold read-only directories.
//!
//! Directory permissions are copied along with their contents (snapshots,
//! cross-filesystem moves), so a tree can contain a directory whose mode forbids
//! unlinking its entries. Every removal of a copied tree goes through here.

use std::path::Path;

/// Removes a directory tree; one that is already gone is success.
///
/// Directories with a read-only mode get owner access back first, without
/// following links.
pub fn remove_tree_if_present(path: &Path) -> std::io::Result<()> {
    let mut removed = std::fs::remove_dir_all(path);
    if matches!(&removed, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied) {
        restore_owner_access(path);
        removed = std::fs::remove_dir_all(path);
    }
    match removed {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Gives the owner full access to every directory under `path`, links excluded.
/// Best effort: whatever still cannot be removed is reported by the removal.
fn restore_owner_access(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.file_type().is_dir() {
        return;
    }
    let mode = metadata.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        restore_owner_access(&entry.path());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn chmod(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn a_missing_tree_is_removed_successfully() {
        let base = tempfile::tempdir().expect("temp dir");
        remove_tree_if_present(&base.path().join("absent")).expect("already gone");
    }

    #[test]
    fn a_tree_with_read_only_directories_is_removed() {
        let base = tempfile::tempdir().expect("temp dir");
        let tree = base.path().join("tree");
        std::fs::create_dir_all(tree.join("frozen/inner")).expect("tree");
        std::fs::write(tree.join("frozen/inner/file.txt"), b"kept").expect("file");
        chmod(&tree.join("frozen/inner"), 0o500);
        chmod(&tree.join("frozen"), 0o555);

        remove_tree_if_present(&tree).expect("a read-only tree can be removed");

        assert!(!tree.exists());
    }

    #[test]
    fn restoring_access_does_not_follow_a_link_out_of_the_tree() {
        let base = tempfile::tempdir().expect("temp dir");
        let outside = base.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        chmod(&outside, 0o555);
        let tree = base.path().join("tree");
        std::fs::create_dir_all(tree.join("frozen")).expect("tree");
        std::fs::write(tree.join("frozen/file.txt"), b"x").expect("file");
        std::os::unix::fs::symlink(&outside, tree.join("frozen/link")).expect("link");
        chmod(&tree.join("frozen"), 0o555);

        remove_tree_if_present(&tree).expect("remove");

        let mode = std::fs::metadata(&outside)
            .expect("outside survives")
            .permissions()
            .mode();
        chmod(&outside, 0o755);
        assert!(!tree.exists());
        assert_eq!(mode & 0o777, 0o555, "the link target keeps its mode");
    }
}
