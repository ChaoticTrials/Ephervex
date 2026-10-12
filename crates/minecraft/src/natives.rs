//! Unpacking the pre-1.19 natives jars.
//!
//! Up to 1.18.x a library carries a `natives` classifier map, and the named
//! classifier is a jar of shared objects that has to be on disk before the JVM
//! starts - `-Djava.library.path` cannot read inside an archive. From 1.19
//! onward there is nothing to unpack: natives are ordinary classpath jars and
//! LWJGL extracts what it needs itself. This module therefore only ever runs
//! for the older half of the supported range, and [`crate::library::Library`]
//! keeps the two forms distinguishable precisely so it cannot run for the
//! newer one.
//!
//! # Why this is not the shared extractor
//!
//! `ephervex-pack` grows the hardened extractor every *importer* uses in
//! Modrinth pack import. This is a deliberately much smaller thing: the archives are Mojang's
//! own, fetched over HTTPS and verified against a publisher-supplied SHA-1
//! before a byte of them is read, and the only entries kept are regular files.
//! It still enforces the security rules, because "the source is trusted" is exactly
//! the assumption that makes a traversal bug ship:
//!
//! * every entry's destination is resolved and required to stay inside the
//!   natives directory,
//! * absolute paths, `..`, and drive-letter-looking components are refused,
//!   rather than sanitized into something that merely looks safe,
//! * directory entries and symlinks are skipped - a natives jar contains
//!   neither, and honoring one would reintroduce the link-through-link problem
//!   `ephervex-java::archive` documents,
//! * directories below the natives directory must be real directories: a link
//!   already sitting in the shared store is refused rather than followed out of
//!   it, and a link at a file's own path is replaced, never written through,
//! * a file is written to a unique sibling and renamed over its destination, so
//!   an instance that has the previous file mapped keeps a complete file and a
//!   concurrent unpack never exposes a partial one (ADR 0011),
//! * a sibling is claimed with an `flock` owner marker while it is written, so
//!   the next unpack removes one an interrupted launcher left behind and never
//!   one another unpack is still writing,
//! * `extract.exclude` from the metadata is applied as a *filter* on top, never
//!   as a substitute for the checks above.
//!
//! This extraction path shares the same security requirements as pack and runtime
//! extraction. Two sets
//! of rules mean one of them gets forgotten, and it will be the one with the
//! vulnerability (see `docs/architecture.md`).

use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ephervex_download::scratch::{self, ScratchOwner};
use tracing::{debug, trace};

/// Largest a single unpacked native may be.
///
/// The biggest one Mojang ships is LWJGL's `libopenal.so` at roughly 1 MB; a
/// hundredfold ceiling stops a hostile or corrupt archive from filling a disk
/// while leaving every real file far inside it.
const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;

/// Largest total a single natives jar may unpack to.
const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

/// Most entries a single natives jar may contain.
const MAX_ENTRIES: usize = 4096;

/// Why a natives jar could not be unpacked.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativesError {
    /// The file is not a readable jar.
    #[error("`{path}` is not a readable natives archive")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// An entry names a destination outside the natives directory
    #[error("`{path}` contains an entry that would escape the natives directory: `{entry}`")]
    UnsafePath { path: PathBuf, entry: String },
    /// The archive exceeds an extraction limit.
    #[error("`{path}` exceeds the extraction limit for natives ({limit})")]
    TooLarge { path: PathBuf, limit: &'static str },
    /// Writing an unpacked file failed.
    #[error("could not write `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl NativesError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "error-launch-natives-unreadable",
            Self::UnsafePath { .. } => "error-launch-natives-unsafe-path",
            Self::TooLarge { .. } => "error-launch-natives-too-large",
            Self::Write { .. } => "error-launch-natives-write",
        }
    }

    /// The path this failure is about.
    #[must_use]
    pub fn failing_path(&self) -> PathBuf {
        match self {
            Self::Unreadable { path, .. }
            | Self::UnsafePath { path, .. }
            | Self::TooLarge { path, .. }
            | Self::Write { path, .. } => path.clone(),
        }
    }
}

/// What one jar contributed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativesReport {
    /// Files written, relative to the natives directory.
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    /// Entries dropped because `extract.exclude` named them.
    pub excluded: usize,
}

/// Unpacks one natives jar into `destination`.
///
/// `exclude` is the metadata's `extract.exclude` - universally `["META-INF/"]`,
/// honored from the document rather than hardcoded. A prefix match, which is
/// what the trailing slash in Mojang's own value means.
pub fn extract(
    archive: &Path,
    destination: &Path,
    exclude: &[String],
) -> Result<NativesReport, NativesError> {
    let file = std::fs::File::open(archive).map_err(|source| NativesError::Unreadable {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|source| NativesError::Unreadable {
        path: archive.to_path_buf(),
        source: std::io::Error::other(source),
    })?;

    if zip.len() > MAX_ENTRIES {
        return Err(NativesError::TooLarge {
            path: archive.to_path_buf(),
            limit: "entry count",
        });
    }

    std::fs::create_dir_all(destination).map_err(|source| NativesError::Write {
        path: destination.to_path_buf(),
        source,
    })?;
    sweep_abandoned_temporaries(destination, 0);

    let mut report = NativesReport::default();
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|source| NativesError::Unreadable {
                path: archive.to_path_buf(),
                source: std::io::Error::other(source),
            })?;

        // `name` is attacker-controlled in the general case, so the raw string
        // is what gets checked - never a value derived from it.
        let name = entry.name().to_owned();
        if entry.is_dir() {
            continue;
        }
        if exclude
            .iter()
            .any(|prefix| name.starts_with(prefix.as_str()))
        {
            report.excluded += 1;
            continue;
        }

        let relative = safe_relative_path(&name).ok_or_else(|| NativesError::UnsafePath {
            path: archive.to_path_buf(),
            entry: name.clone(),
        })?;
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(NativesError::TooLarge {
                path: archive.to_path_buf(),
                limit: "single file",
            });
        }
        report.bytes = report.bytes.saturating_add(entry.size());
        if report.bytes > MAX_TOTAL_BYTES {
            return Err(NativesError::TooLarge {
                path: archive.to_path_buf(),
                limit: "total size",
            });
        }

        // Natives are flat in every jar Mojang publishes, but a nested entry is
        // written where it says rather than flattened - flattening would let
        // two entries collide silently.
        let target = destination.join(&relative);
        if let Some(parent) = relative.parent() {
            create_contained_directories(archive, destination, parent, &name)?;
        }

        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry
            .read_to_end(&mut bytes)
            .map_err(|source| NativesError::Unreadable {
                path: archive.to_path_buf(),
                source,
            })?;
        write_executable(&target, &bytes)?;

        trace!(entry = %name, path = %target.display(), "unpacked a native");
        report.files.push(relative);
    }

    debug!(
        archive = %archive.display(),
        destination = %destination.display(),
        files = report.files.len(),
        excluded = report.excluded,
        "unpacked a natives jar"
    );
    Ok(report)
}

static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Most unique temporary names tried before giving up on a directory.
const MAX_TEMPORARY_ATTEMPTS: usize = 16;

/// Most directory levels below the natives directory searched for leftovers.
const MAX_SWEEP_DEPTH: usize = 4;

/// How old a temporary with no owner marker must be before it is collected.
///
/// Only an unpack that predates owner markers writes one, and writing a
/// single native takes seconds.
const UNOWNED_TEMPORARY_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether `name` has the shape of a temporary [`create_temporary_sibling`]
/// makes: `.<native>.<pid>.<counter>.part`.
fn is_temporary_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".part"))
    else {
        return false;
    };
    let mut parts = rest.rsplitn(3, '.');
    let is_number = |part: Option<&str>| {
        part.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    is_number(parts.next())
        && is_number(parts.next())
        && parts.next().is_some_and(|native| !native.is_empty())
}

/// Removes temporaries an interrupted unpack left in `directory` and in the
/// real directories below it.
///
/// One is collected when its owner marker is unlocked, which a running unpack
/// in any process holds for as long as it writes, or when it has no marker and
/// has not been modified for [`UNOWNED_TEMPORARY_MIN_AGE`]. Links are never
/// followed.
fn sweep_abandoned_temporaries(directory: &Path, depth: usize) {
    let collected = scratch::sweep_abandoned(directory, is_temporary_name);
    if collected > 0 {
        debug!(directory = %directory.display(), collected, "removed abandoned native temporaries");
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth < MAX_SWEEP_DEPTH {
                sweep_abandoned_temporaries(&path, depth + 1);
            }
            continue;
        }
        if !kind.is_file() || !entry.file_name().to_str().is_some_and(is_temporary_name) {
            continue;
        }
        remove_unowned_temporary(&path);
    }
}

/// Removes `path` when no owner marker accounts for it and it is old enough.
///
/// The decision is made while holding the claim on the name, as a writer must
/// before it creates a temporary there, so a file found under the claim cannot
/// belong to a running unpack.
fn remove_unowned_temporary(path: &Path) {
    if scratch::marker_path(path).is_ok_and(|marker| std::fs::symlink_metadata(marker).is_ok()) {
        return;
    }
    let Ok(owner) = ScratchOwner::claim(path) else {
        return;
    };
    let stale = std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= UNOWNED_TEMPORARY_MIN_AGE);
    if stale && std::fs::remove_file(path).is_ok() {
        debug!(path = %path.display(), "removed an unowned native temporary");
    }
    owner.release();
}

/// Creates the directories between `destination` and `destination/relative`,
/// refusing any that is already a link or a non-directory.
///
/// `create_dir_all` and `fs::write` follow links that already exist, and the
/// natives directory is shared state that outlives any one launch, so each
/// component is inspected without following it.
fn create_contained_directories(
    archive: &Path,
    destination: &Path,
    relative: &Path,
    entry: &str,
) -> Result<(), NativesError> {
    let unsafe_path = || NativesError::UnsafePath {
        path: archive.to_path_buf(),
        entry: entry.to_owned(),
    };
    let mut current = destination.to_path_buf();
    for component in relative.components() {
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(source) => {
                        return Err(NativesError::Write {
                            path: current,
                            source,
                        });
                    }
                }
                // A concurrent unpack may have created the name first, so the
                // result is checked again rather than assumed.
                std::fs::symlink_metadata(&current).map_err(|source| NativesError::Write {
                    path: current.clone(),
                    source,
                })?
            }
            Err(source) => {
                return Err(NativesError::Write {
                    path: current,
                    source,
                });
            }
        };
        if !metadata.is_dir() {
            return Err(unsafe_path());
        }
    }
    Ok(())
}

/// Publishes a file at `path`, readable and executable by its owner.
///
/// The bytes go to a unique sibling that is renamed over `path`, so `path` is
/// replaced by a different inode: a reader never observes a truncated file, a
/// process that has the old file mapped keeps its complete contents, and a link
/// already at `path` is replaced rather than written through.
///
/// A shared object does not strictly need the executable bit on Linux to be
/// `dlopen`ed, but some of what Mojang ships in these jars is a helper binary,
/// and Java runtime support already learned that dropping the bit produces an install that
/// is inert with no obvious cause.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), NativesError> {
    let write_error = |source: std::io::Error| NativesError::Write {
        path: path.to_path_buf(),
        source,
    };
    let parent = path
        .parent()
        .ok_or_else(|| write_error(std::io::Error::other("native has no parent directory")))?;

    let (mut file, temporary, owner) =
        create_temporary_sibling(path, parent).map_err(write_error)?;
    let published = (|| {
        file.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if published.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    // The marker goes last, after the temporary is renamed or removed.
    owner.release();
    published.map_err(write_error)
}

/// Exclusively creates a uniquely named file next to `path`, claimed by the
/// returned owner until the caller releases it.
///
/// `create_new` refuses an existing name, including a link, so the temporary
/// can never be a way to write through something already there.
fn create_temporary_sibling(
    path: &Path,
    parent: &Path,
) -> std::io::Result<(std::fs::File, PathBuf, ScratchOwner)> {
    let name = path.file_name().unwrap_or_default();
    for _ in 0..MAX_TEMPORARY_ATTEMPTS {
        let counter = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = std::ffi::OsString::from(".");
        temporary_name.push(name);
        temporary_name.push(format!(".{}.{counter}.part", std::process::id()));
        let temporary = parent.join(temporary_name);
        // Claimed before the file exists, so a sweep never sees it unowned.
        let owner = match ScratchOwner::claim(&temporary) {
            Ok(owner) => owner,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((file, temporary, owner)),
            Err(error) => {
                owner.release();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no unused temporary name",
    ))
}

/// Validates an archive entry name as a relative path that cannot escape.
///
/// Returns `None` for anything absolute, anything containing `..`, anything
/// with a root or prefix component, and anything that normalizes to nothing.
/// Deliberately *rejecting* rather than stripping: a sanitized hostile path is
/// still a path somebody chose, and the launcher has no reason to write it.
fn safe_relative_path(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return None;
    }
    // A Windows-style archive entry reaches Linux as one long name rather than
    // as components, so the separator is checked before `Path` sees it.
    if name.contains('\\') || name.contains('\0') {
        return None;
    }

    let candidate = Path::new(name);
    let mut relative = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// Builds a jar in memory with the given entries.
    fn jar(entries: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(file.path()).expect("create"));
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(body).expect("write entry");
        }
        writer.finish().expect("finish");
        file
    }

    #[test]
    fn a_natives_jar_unpacks_its_shared_objects_and_drops_the_excluded_prefix() {
        let archive = jar(&[
            ("liblwjgl.so", b"elf" as &[u8]),
            ("libopenal.so", b"elf"),
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0"),
        ]);
        let destination = tempfile::tempdir().expect("temp dir");

        let report = extract(
            archive.path(),
            destination.path(),
            &["META-INF/".to_owned()],
        )
        .expect("a natives jar unpacks");

        assert_eq!(report.files.len(), 2);
        assert_eq!(report.excluded, 1);
        assert!(destination.path().join("liblwjgl.so").is_file());
        assert!(
            !destination.path().join("META-INF").exists(),
            "extract.exclude is honored from the metadata, not hardcoded"
        );
    }

    #[test]
    fn an_unpacked_native_is_executable() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let archive = jar(&[("liblwjgl.so", b"elf")]);
            let destination = tempfile::tempdir().expect("temp dir");
            extract(archive.path(), destination.path(), &[]).expect("unpack");

            let mode = std::fs::metadata(destination.path().join("liblwjgl.so"))
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "an inert install has no obvious cause");
        }
    }

    #[test]
    fn an_entry_that_would_escape_the_natives_directory_is_refused() {
        for hostile in [
            "../../../etc/cron.d/evil",
            "/etc/passwd",
            "a/../../b",
            "..\\windows\\system32\\evil.dll",
        ] {
            let archive = jar(&[(hostile, b"payload")]);
            let destination = tempfile::tempdir().expect("temp dir");
            let outcome = extract(archive.path(), destination.path(), &[]);
            assert!(
                matches!(outcome, Err(NativesError::UnsafePath { .. })),
                "`{hostile}` should be refused, got {outcome:?}"
            );
        }
    }

    #[test]
    fn a_nested_entry_is_written_where_it_says_rather_than_flattened() {
        // Flattening would let `a/x.so` and `b/x.so` overwrite each other.
        let archive = jar(&[("linux/x64/libfoo.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let report = extract(archive.path(), destination.path(), &[]).expect("unpack");

        assert_eq!(report.files, [PathBuf::from("linux/x64/libfoo.so")]);
        assert!(destination.path().join("linux/x64/libfoo.so").is_file());
    }

    #[test]
    fn a_file_that_is_not_an_archive_fails_rather_than_producing_no_natives() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"not a zip").expect("write");
        let destination = tempfile::tempdir().expect("temp dir");

        let error = extract(file.path(), destination.path(), &[])
            .expect_err("a truncated download must not look like an empty natives set");
        assert!(matches!(error, NativesError::Unreadable { .. }));
        assert_eq!(error.message_id(), "error-launch-natives-unreadable");
    }

    #[test]
    fn a_relative_path_check_accepts_only_ordinary_names() {
        assert_eq!(
            safe_relative_path("liblwjgl.so"),
            Some(PathBuf::from("liblwjgl.so"))
        );
        assert_eq!(
            safe_relative_path("./liblwjgl.so"),
            Some(PathBuf::from("liblwjgl.so"))
        );
        for hostile in ["", "/abs", "../up", "a/../../b", "a\\b", "a\0b"] {
            assert!(
                safe_relative_path(hostile).is_none(),
                "`{hostile}` should be refused"
            );
        }
    }

    #[test]
    fn re_extracting_a_native_replaces_the_file_instead_of_rewriting_it() {
        use std::os::unix::fs::MetadataExt as _;

        let archive = jar(&[("liblwjgl.so", b"new contents")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let target = destination.path().join("liblwjgl.so");
        std::fs::write(&target, b"old contents").expect("seed");
        // A process that has the previous file open keeps seeing it whole.
        let mut held = std::fs::File::open(&target).expect("open previous file");
        let previous_inode = std::fs::metadata(&target).expect("metadata").ino();

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        let mut seen = String::new();
        held.read_to_string(&mut seen).expect("read held file");
        assert_eq!(seen, "old contents");
        assert_ne!(
            std::fs::metadata(&target).expect("metadata").ino(),
            previous_inode
        );
        assert_eq!(std::fs::read(&target).expect("read"), b"new contents");
    }

    #[test]
    fn extraction_leaves_no_temporary_files_behind() {
        let archive = jar(&[("liblwjgl.so", b"elf"), ("linux/libfoo.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        extract(archive.path(), destination.path(), &[]).expect("unpack");
        extract(archive.path(), destination.path(), &[]).expect("unpack again");

        for directory in [
            destination.path().to_path_buf(),
            destination.path().join("linux"),
        ] {
            for entry in std::fs::read_dir(directory).expect("read dir") {
                let name = entry.expect("entry").file_name();
                let name = name.to_string_lossy();
                assert!(
                    !name.ends_with(".part") && !name.ends_with(".owner"),
                    "{name}"
                );
            }
        }
    }

    fn leave_temporary(directory: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(directory).expect("directory");
        let path = directory.join(name);
        std::fs::write(&path, b"partial").expect("write temporary");
        path
    }

    fn age(path: &Path, by: Duration) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(std::time::SystemTime::now() - by)
            .expect("set mtime");
    }

    #[test]
    fn an_unpack_removes_temporaries_an_interrupted_unpack_left_behind() {
        let archive = jar(&[("liblwjgl.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let top = destination.path().join(".libold.so.4242.0.part");
        let nested = destination
            .path()
            .join("linux")
            .join(".libold.so.4242.1.part");
        for path in [&top, &nested] {
            let parent = path.parent().expect("parent");
            drop(ScratchOwner::claim(path).expect("claim"));
            leave_temporary(parent, path.file_name().unwrap().to_str().unwrap());
        }

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        for path in [&top, &nested] {
            assert!(!path.exists(), "{path:?}");
            assert!(!scratch::marker_path(path).expect("marker").exists());
        }
        assert!(destination.path().join("liblwjgl.so").is_file());
    }

    #[test]
    fn an_unpack_never_removes_a_temporary_another_unpack_is_writing() {
        let archive = jar(&[("liblwjgl.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let live = destination.path().join(".libbig.so.4242.0.part");
        let owner = ScratchOwner::claim(&live).expect("claim");
        leave_temporary(destination.path(), ".libbig.so.4242.0.part");
        // Even a long-unmodified file is live while its owner holds the marker.
        age(&live, Duration::from_secs(10 * 24 * 60 * 60));

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        assert_eq!(std::fs::read(&live).expect("read"), b"partial");
        owner.release();
    }

    #[test]
    fn an_old_temporary_without_an_owner_marker_is_removed_and_a_recent_one_is_kept() {
        let archive = jar(&[("liblwjgl.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let old = leave_temporary(destination.path(), ".libold.so.4242.0.part");
        age(&old, UNOWNED_TEMPORARY_MIN_AGE + Duration::from_secs(60));
        let recent = leave_temporary(destination.path(), ".librecent.so.4242.1.part");

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        assert!(!old.exists());
        assert!(!scratch::marker_path(&old).expect("marker").exists());
        assert!(recent.is_file());
        assert!(!scratch::marker_path(&recent).expect("marker").exists());
    }

    #[test]
    fn files_that_are_not_native_temporaries_are_never_removed() {
        let archive = jar(&[("liblwjgl.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let kept: Vec<_> = [
            ".notes.part",
            ".lib.so.x.1.part",
            "..1.2.part",
            "lib.so.4242.0.part",
        ]
        .into_iter()
        .map(|name| {
            let path = leave_temporary(destination.path(), name);
            age(&path, UNOWNED_TEMPORARY_MIN_AGE * 2);
            path
        })
        .collect();

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        for path in kept {
            assert!(path.is_file(), "{path:?}");
        }
    }

    #[test]
    fn a_link_named_like_a_temporary_is_never_followed_or_removed() {
        let archive = jar(&[("liblwjgl.so", b"elf")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let outside = tempfile::tempdir().expect("outside dir");
        let victim = leave_temporary(outside.path(), "victim");
        age(&victim, UNOWNED_TEMPORARY_MIN_AGE * 2);
        let link = destination.path().join(".libold.so.4242.0.part");
        std::os::unix::fs::symlink(&victim, &link).expect("link");

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        assert!(victim.is_file());
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link kept")
                .is_symlink()
        );
    }

    #[test]
    fn temporary_names_are_recognized_by_their_exact_shape() {
        assert!(is_temporary_name(".liblwjgl.so.4242.0.part"));
        assert!(is_temporary_name(".a.b.c.12.345.part"));
        for name in [
            "liblwjgl.so.4242.0.part",
            ".liblwjgl.so.4242.part",
            ".liblwjgl.so.x.0.part",
            "..4242.0.part",
            ".liblwjgl.so.4242.0",
            ".liblwjgl.so.4242.0.part.owner",
        ] {
            assert!(!is_temporary_name(name), "{name}");
        }
    }

    #[test]
    fn concurrent_unpacks_never_expose_a_partial_native() {
        let payload = vec![7_u8; 4 * 1024 * 1024];
        let archive = jar(&[("libbig.so", payload.as_slice())]);
        let destination = tempfile::tempdir().expect("temp dir");
        extract(archive.path(), destination.path(), &[]).expect("seed");
        let target = destination.path().join("libbig.so");
        let done = std::sync::atomic::AtomicBool::new(false);

        std::thread::scope(|scope| {
            let observer = scope.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    let length = std::fs::metadata(&target).expect("metadata").len();
                    assert_eq!(length, payload.len() as u64, "observed a partial native");
                }
            });
            let writers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..20 {
                            extract(archive.path(), destination.path(), &[]).expect("unpack");
                        }
                    })
                })
                .collect();
            // The observer is stopped before any failure is raised, or a
            // panicking writer would leave it looping and hang the scope.
            let writer_results: Vec<_> = writers
                .into_iter()
                .map(std::thread::ScopedJoinHandle::join)
                .collect();
            done.store(true, Ordering::Relaxed);
            let observer_result = observer.join();
            for result in writer_results {
                result.expect("writer");
            }
            observer_result.expect("observer");
        });
    }

    #[test]
    fn a_link_already_at_a_native_path_is_replaced_not_written_through() {
        let archive = jar(&[("lib.so", b"extracted")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let outside = tempfile::tempdir().expect("outside dir");
        let victim = outside.path().join("victim");
        std::fs::write(&victim, b"untouched").expect("seed victim");
        std::os::unix::fs::symlink(&victim, destination.path().join("lib.so")).expect("link");

        extract(archive.path(), destination.path(), &[]).expect("unpack");

        assert_eq!(std::fs::read(&victim).expect("read"), b"untouched");
        let published = destination.path().join("lib.so");
        assert!(
            !std::fs::symlink_metadata(&published)
                .expect("metadata")
                .is_symlink()
        );
        assert_eq!(std::fs::read(&published).expect("read"), b"extracted");
    }

    #[test]
    fn a_linked_directory_below_the_natives_directory_is_refused() {
        let archive = jar(&[("linux/lib.so", b"extracted")]);
        let destination = tempfile::tempdir().expect("temp dir");
        let outside = tempfile::tempdir().expect("outside dir");
        std::os::unix::fs::symlink(outside.path(), destination.path().join("linux")).expect("link");

        let outcome = extract(archive.path(), destination.path(), &[]);

        assert!(
            matches!(outcome, Err(NativesError::UnsafePath { .. })),
            "got {outcome:?}"
        );
        assert!(!outside.path().join("lib.so").exists());
    }

    #[test]
    fn a_file_where_a_native_directory_is_needed_is_refused() {
        let archive = jar(&[("linux/lib.so", b"extracted")]);
        let destination = tempfile::tempdir().expect("temp dir");
        std::fs::write(destination.path().join("linux"), b"file").expect("seed");

        let outcome = extract(archive.path(), destination.path(), &[]);

        assert!(matches!(outcome, Err(NativesError::UnsafePath { .. })));
    }
}
