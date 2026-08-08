//! Unpacking the pre-1.19 natives jars.
//!
//! Up to 1.18.x a library carries a `natives` classifier map, and the named
//! classifier is a jar of shared objects that has to be on disk before the JVM
//! starts — `-Djava.library.path` cannot read inside an archive. From 1.19
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
//! * directory entries and symlinks are skipped — a natives jar contains
//!   neither, and honoring one would reintroduce the link-through-link problem
//!   `ephervex-java::archive` documents,
//! * `extract.exclude` from the metadata is applied as a *filter* on top, never
//!   as a substitute for the checks above.
//!
//! This extraction path shares the same security requirements as pack and runtime
//! extraction. Two sets
//! of rules mean one of them gets forgotten, and it will be the one with the
//! vulnerability (see `docs/architecture.md`).

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

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
/// `exclude` is the metadata's `extract.exclude` — universally `["META-INF/"]`,
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

    let mut report = NativesReport::default();
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|source| NativesError::Unreadable {
                path: archive.to_path_buf(),
                source: std::io::Error::other(source),
            })?;

        // `name` is attacker-controlled in the general case, so the raw string
        // is what gets checked — never a value derived from it.
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
        // written where it says rather than flattened — flattening would let
        // two entries collide silently.
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| NativesError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
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

/// Writes a file and makes it readable and executable by its owner.
///
/// A shared object does not strictly need the executable bit on Linux to be
/// `dlopen`ed, but some of what Mojang ships in these jars is a helper binary,
/// and Java runtime support already learned that dropping the bit produces an install that
/// is inert with no obvious cause.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), NativesError> {
    std::fs::write(path, bytes).map_err(|source| NativesError::Write {
        path: path.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(
            |source| NativesError::Write {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
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
    use std::io::Write as _;

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
}
