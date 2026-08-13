//! Path rules every importer and extractor in the workspace shares
//!
//! # Why these live here rather than beside each archive reader
//!
//! An archive entry name is attacker-controlled text. The rules for turning one
//! into a path — refuse absolute paths, refuse `..`, refuse Windows drive
//! prefixes, refuse NUL, refuse anything that escapes the extraction root — are
//! the same whether the archive is a Modrinth `.mrpack`, a CurseForge ZIP,
//! another provider's archive, or the JDK tarball `ephervex-java` unpacks.
//! Written once per reader they would be four sets of rules to keep in step,
//! and the one that gets forgotten is the one with the vulnerability.
//! `ephervex-java`'s tar reader therefore calls into this module rather than
//! carrying its own copy.
//!
//! Everything here is **lexical**: it describes where an archive *says* an entry
//! goes. `fs::canonicalize` is deliberately not used, because it follows the
//! links that are themselves the untrusted input. [`walks_through_a_link`]
//! closes the remaining gap for archives that can carry links at all.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// The launcher-owned metadata directory, which pack contents may never write
/// into (ADR 0002).
///
/// Duplicated from `ephervex-storage`'s `LAUNCHER_METADATA_DIR` rather than
/// imported: this crate does not depend on the storage crate, and an extractor
/// that had to be *told* which names are reserved would be one caller away from
/// being told nothing. `ephervex-core` owns a test that the two agree.
pub const RESERVED_METADATA_DIR: &str = ".launcher";

/// Names an extracted entry may never start with.
pub const RESERVED_TOP_LEVEL_NAMES: &[&str] = &[RESERVED_METADATA_DIR];

/// Validates an archive entry name and turns it into a relative path.
///
/// Refuses, in order: NUL bytes, non-UTF-8, absolute paths, Windows drive
/// prefixes, backslash separators, and any `..` component. `.` components are
/// dropped as noise. `strip_components` removes that many leading components,
/// which is how a wrapper directory (`jdk-21.0.12+8/`, `overrides/`) is peeled
/// off; an entry consumed entirely by the strip returns an empty path, which
/// callers treat as "nothing to create" rather than as an error.
///
/// Returns `None` for anything unsafe. Callers turn that into a refusal of the
/// whole archive: an archive containing a traversal attempt is one to reject,
/// not one to partially trust.
#[must_use]
pub fn safe_relative_path(raw: &[u8], strip_components: usize) -> Option<PathBuf> {
    if raw.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    // A backslash is a separator on the platform an archive may have been built
    // for, and `C:\x` must not become a single filename here.
    if text.starts_with('/') || text.contains('\\') || text.contains(':') {
        return None;
    }

    let mut safe = PathBuf::new();
    let mut depth = 0usize;
    for component in Path::new(text).components() {
        match component {
            Component::Normal(part) => {
                depth += 1;
                if depth > strip_components {
                    safe.push(part);
                }
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(safe)
}

/// Validates a provider-supplied file name as a single, safe path component.
///
/// Stricter than [`safe_relative_path`] on purpose. That function validates an
/// archive *path*, which legitimately has directories in it; this validates a
/// name a remote catalog published for one file, which never should. A
/// `filename` of `nested/mod.jar` is not something to helpfully create a
/// directory for — it is a provider saying something this launcher does not
/// expect, and the safe response is to refuse rather than to guess at an
/// intention.
///
/// Returns `None` for anything with a separator, a traversal component, a
/// reserved name, or nothing in it at all.
#[must_use]
pub fn safe_file_name(raw: &str) -> Option<PathBuf> {
    let candidate = safe_relative_path(raw.as_bytes(), 0)?;
    let mut components = candidate.components();
    let only = match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) => PathBuf::from(part),
        // Zero components (empty, or `.`), or more than one (a directory).
        _ => return None,
    };
    (!is_reserved(&only)).then_some(only)
}

/// Whether a validated relative path would write into launcher-owned metadata.
///
/// Checked on the *validated* path rather than on the raw name so that
/// `foo/../.launcher/instance.json` cannot slip past a string comparison — by
/// the time a path reaches here it has no `..` left to hide behind.
#[must_use]
pub fn is_reserved(relative: &Path) -> bool {
    relative
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .is_some_and(|name| {
            RESERVED_TOP_LEVEL_NAMES
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
        })
}

/// Validates a symlink target, resolving it against the link's own directory.
///
/// Rejecting escaping symlinks requires a *resolution*
/// rather than a ban: a real Temurin JDK archive contains a couple of hundred
/// links that climb out of their own directory with `..` and land back inside
/// the runtime. The target is therefore walked lexically from the link's
/// directory — `..` pops a component, and popping past the root is the escape.
///
/// Returns the target to create, spelled exactly as the archive spelled it, so
/// the extracted tree stays relocatable.
#[must_use]
pub fn safe_link_target(raw: &[u8], link_path: &Path) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    if text.starts_with('/') || text.contains('\\') || text.contains(':') {
        return None;
    }

    let mut depth = link_path
        .parent()
        .map_or(0, |parent| parent.components().count());

    for component in Path::new(text).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                // Popping past the root is the escape, and it is the only thing
                // that distinguishes a legitimate relative link from a hostile
                // one.
                depth = depth.checked_sub(1)?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(PathBuf::from(text))
}

/// Whether `relative` is, or sits underneath, a path this archive has already
/// made a symlink.
///
/// Every check above is lexical, so it describes where the archive *says* an
/// entry goes. Once an earlier entry has made one of its ancestors a symlink,
/// that is no longer where it lands: `a/l1 ->..` is a link that passes every
/// test and silently relocates everything named `a/l1/…` one level up. Refusing
/// to write through a link the archive itself created keeps the lexical view
/// and the filesystem in step.
#[must_use]
pub fn walks_through_a_link(relative: &Path, links: &BTreeSet<PathBuf>) -> bool {
    relative
        .ancestors()
        .any(|ancestor| links.contains(ancestor))
}

/// Joins a validated relative path onto a root, restating the containment
/// invariant.
///
/// [`safe_relative_path`] has already refused everything that could escape, so
/// this can only fail if that logic is wrong. It is cheap, and it is the check
/// that actually names the property the caller depends on.
#[must_use]
pub fn contained_join(root: &Path, relative: &Path) -> Option<PathBuf> {
    let target = root.join(relative);
    target.starts_with(root).then_some(target)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn safe(name: &str) -> Option<PathBuf> {
        safe_relative_path(name.as_bytes(), 0)
    }

    #[test]
    fn ordinary_pack_paths_are_accepted() {
        assert_eq!(safe("mods/jei.jar"), Some(PathBuf::from("mods/jei.jar")));
        assert_eq!(
            safe("config/some mod/settings.toml"),
            Some(PathBuf::from("config/some mod/settings.toml"))
        );
    }

    #[test]
    fn traversal_absolute_and_windows_paths_are_refused() {
        assert_eq!(safe("../../etc/passwd"), None);
        assert_eq!(safe("mods/../../escape.jar"), None);
        assert_eq!(safe("/etc/passwd"), None);
        assert_eq!(safe("C:\\Windows\\system32"), None);
        assert_eq!(safe("mods\\jei.jar"), None);
    }

    #[test]
    fn nul_bytes_and_invalid_utf8_are_refused() {
        assert_eq!(safe_relative_path(b"mods/a\0b.jar", 0), None);
        assert_eq!(safe_relative_path(&[0xff, 0xfe, 0x2f, 0x61], 0), None);
    }

    #[test]
    fn stripping_components_peels_a_wrapper_directory() {
        assert_eq!(
            safe_relative_path(b"overrides/config/a.toml", 1),
            Some(PathBuf::from("config/a.toml"))
        );
        // The wrapper directory entry itself survives as an empty path, which
        // means "nothing to create" rather than "unsafe".
        assert_eq!(
            safe_relative_path(b"overrides/", 1),
            Some(PathBuf::new()),
            "the wrapper entry is not an error"
        );
    }

    #[test]
    fn launcher_owned_metadata_is_reserved_however_it_is_spelled() {
        assert!(is_reserved(Path::new(".launcher/instance.json")));
        assert!(is_reserved(Path::new(".LAUNCHER/instance.json")));
        assert!(is_reserved(Path::new(".launcher")));
        assert!(!is_reserved(Path::new("mods/.launcher-helper.jar")));
        assert!(
            !is_reserved(Path::new("config/.launcher/x")),
            "only the top level is launcher-owned; the game directory is not"
        );
    }

    #[test]
    fn a_relative_link_that_stays_inside_the_root_is_accepted() {
        assert_eq!(
            safe_link_target(b"../java.base/LICENSE", Path::new("legal/jdk.rmi/LICENSE")),
            Some(PathBuf::from("../java.base/LICENSE"))
        );
    }

    #[test]
    fn a_link_that_climbs_out_of_the_root_is_refused() {
        assert_eq!(
            safe_link_target(b"/etc/shadow", Path::new("bin/java")),
            None
        );
        assert_eq!(
            safe_link_target(b"../../outside", Path::new("bin/java")),
            None
        );
        assert_eq!(safe_link_target(b"", Path::new("bin/java")), None);
    }

    #[test]
    fn nothing_may_be_written_through_a_link_the_archive_created() {
        let mut links = BTreeSet::new();
        links.insert(PathBuf::from("a/l1"));
        assert!(walks_through_a_link(Path::new("a/l1/passwd"), &links));
        assert!(walks_through_a_link(Path::new("a/l1"), &links));
        assert!(!walks_through_a_link(Path::new("a/other"), &links));
    }

    #[test]
    fn joining_restates_containment() {
        let root = Path::new("/instances/x/minecraft");
        assert_eq!(
            contained_join(root, Path::new("mods/a.jar")),
            Some(PathBuf::from("/instances/x/minecraft/mods/a.jar"))
        );
    }
}
