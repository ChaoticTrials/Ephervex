//! Unpacking a JDK tarball without letting it out of its directory
//!
//! # Where the rules live
//!
//! This module reads one archive shape - a gzipped tar from Adoptium - but the
//! rules it applies to an entry name are not its own. [`safe_relative_path`]
//! and [`safe_link_target`] come from `ephervex-pack`'s `path` module, which
//! every importer and extractor in the workspace shares. Keeping one lexical
//! implementation prevents JDK and pack extraction from drifting into
//! different security policies.
//!
//! What stays here is what is genuinely tar-specific: which entry *types* are
//! acceptable, the extraction limits a JDK needs, and the permission bits.
//!
//! # What is rejected
//!
//! Absolute paths, any `..` component, Windows drive prefixes, NUL bytes, and
//! anything whose resolved destination falls outside the extraction root.
//! Hard links are refused outright, as are device nodes, FIFOs, and anything
//! else that is not a file, a directory, or a symlink.
//!
//! **Symlinks are resolved, not banned.** The Temurin 16 archive contains 208
//! symlinks - `legal/jdk.naming.rmi/LICENSE` and its siblings point at
//! `../java.base/LICENSE` - so a blanket ban rejects valid upstream archives.
//! [`safe_link_target`] enforces the narrower security rule:
//! resolve the target lexically from the link's own directory and refuse it
//! only if it climbs out of the extraction root.
//!
//! Lexical validation alone cannot see several links composing into an escape.
//! This module therefore collects the complete tar link graph, rejects link
//! destinations nested beneath another link, resolves targets through graph
//! links, and creates links only after the entire graph is safe.
//!
//! # Limits
//!
//! Entry count and total uncompressed size are bounded. The defaults are
//! generous - a JDK is roughly 25 000 files and 350 MB unpacked - because this
//! is a developer tool, but "generous" is not "absent": without them a gzip
//! bomb is a full disk.
//!
//! # The executable bit
//!
//! Preserved from the tar header, masked to the permission bits. Losing it
//! makes the whole install useless, which is a failure mode worth stating out
//! loud because it would otherwise only surface at launch.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use ephervex_pack::path::{safe_link_target, safe_relative_path};
use flate2::read::GzDecoder;
use tar::{Archive, EntryType};
use tracing::{debug, instrument};

/// Bounds on what an archive may expand into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    /// Guards against one entry claiming the whole budget, which is what a
    /// bomb built from a single stream looks like.
    pub max_entry_bytes: u64,
}

impl Default for ExtractionLimits {
    /// Sized for a JDK with room to spare: a Temurin 21 JDK unpacks to about
    /// 24 000 files and 330 MB.
    fn default() -> Self {
        Self {
            max_entries: 200_000,
            max_total_bytes: 4 * 1024 * 1024 * 1024,
            max_entry_bytes: 1024 * 1024 * 1024,
        }
    }
}

/// Why an archive was refused.
///
/// Carries the offending entry name so a rejection can be investigated, and
/// carries it as a `String` because the whole point is that it was not a usable
/// path.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArchiveError {
    /// The archive could not be opened or read.
    #[error("the archive {path} could not be read")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// An entry names a path that would land outside the extraction root.
    #[error("the archive entry `{entry}` escapes the extraction directory")]
    UnsafePath { entry: String },

    /// An entry is a symlink, a hardlink, or a device node.
    #[error("the archive entry `{entry}` is a {kind}, which is never extracted")]
    UnsafeEntryType { entry: String, kind: &'static str },

    /// The archive exceeds one of the [`ExtractionLimits`].
    #[error("the archive exceeds the {limit} extraction limit")]
    LimitExceeded {
        limit: &'static str,
        allowed: u64,
        reached: u64,
    },

    /// A file could not be written during extraction.
    #[error("could not write {path} while extracting")]
    Unwritable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// Every entry was stripped away by `strip_components`, so nothing was
    /// extracted. Almost certainly a wrong assumption about the archive's
    /// shape rather than an empty archive.
    #[error("the archive produced no files")]
    Empty,
}

impl ArchiveError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Unreadable { .. } => "error-java-archive-unreadable",
            Self::UnsafePath { .. } | Self::UnsafeEntryType { .. } => "error-java-archive-unsafe",
            Self::LimitExceeded { .. } => "error-java-archive-too-large",
            Self::Unwritable { .. } => "error-java-archive-unwritable",
            Self::Empty => "error-java-archive-empty",
        }
    }
}

/// What one extraction produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionReport {
    pub files: u64,
    pub directories: u64,
    /// Links created after their targets were shown to stay inside the root.
    /// A real JDK has hundreds; see [`safe_link_target`].
    pub symlinks: u64,
    pub bytes: u64,
}

/// Extracts a gzipped tar into `destination`.
///
/// `strip_components` drops that many leading path components from every entry,
/// which is how a Temurin archive's single `jdk-21.0.12+8/` wrapper directory
/// is removed so `bin/java` lands directly under the destination.
///
/// Synchronous: `flate2` and `tar` are blocking, and wrapping them in
/// `spawn_blocking` is the caller's decision because only the caller knows
/// whether it is on a runtime.
#[instrument(skip(limits), fields(archive = %archive.display(), destination = %destination.display()))]
pub fn extract_tar_gz(
    archive: &Path,
    destination: &Path,
    strip_components: usize,
    limits: &ExtractionLimits,
) -> Result<ExtractionReport, ArchiveError> {
    let file = File::open(archive).map_err(|source| ArchiveError::Unreadable {
        path: archive.to_path_buf(),
        source,
    })?;

    // The root has to exist and be canonical before any entry is resolved
    // against it, or the containment check compares against a path that could
    // itself contain a symlink.
    std::fs::create_dir_all(destination).map_err(|source| ArchiveError::Unwritable {
        path: destination.to_path_buf(),
        source,
    })?;
    let root = std::fs::canonicalize(destination).map_err(|source| ArchiveError::Unwritable {
        path: destination.to_path_buf(),
        source,
    })?;

    let mut tar = Archive::new(GzDecoder::new(file));
    let entries = tar.entries().map_err(|source| ArchiveError::Unreadable {
        path: archive.to_path_buf(),
        source,
    })?;

    let mut report = ExtractionReport {
        files: 0,
        directories: 0,
        symlinks: 0,
        bytes: 0,
    };
    let mut seen: u64 = 0;
    // Links are created only after the complete graph is known. Checking one
    // target at a time is insufficient because several individually relative
    // links can compose into an escape, including through a forward reference.
    let mut pending_links = Vec::new();

    for entry in entries {
        let mut entry = entry.map_err(|source| ArchiveError::Unreadable {
            path: archive.to_path_buf(),
            source,
        })?;

        seen += 1;
        if seen > limits.max_entries {
            return Err(ArchiveError::LimitExceeded {
                limit: "entry-count",
                allowed: limits.max_entries,
                reached: seen,
            });
        }

        let Some(resolved) = resolve_entry(&entry, &root, strip_components)? else {
            // Entirely consumed by `strip_components` - the wrapper directory
            // itself. Nothing to create.
            continue;
        };

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&resolved.target).map_err(|source| {
                ArchiveError::Unwritable {
                    path: resolved.target.clone(),
                    source,
                }
            })?;
            report.directories += 1;
            continue;
        }

        if entry.header().entry_type().is_symlink() {
            let target = validated_link_target(&entry, &resolved)?;
            pending_links.push(PendingLink { resolved, target });
            continue;
        }

        report.bytes = extract_file(&mut entry, &resolved.target, report.bytes, limits)?;
        report.files += 1;
    }

    validate_link_graph(&pending_links)?;
    for link in pending_links {
        create_symlink(&link.resolved, &link.target)?;
        report.symlinks += 1;
    }

    if report.files == 0 {
        return Err(ArchiveError::Empty);
    }
    debug!(
        files = report.files,
        directories = report.directories,
        symlinks = report.symlinks,
        bytes = report.bytes,
        "extracted an archive"
    );
    Ok(report)
}

struct PendingLink {
    resolved: ResolvedEntry,
    target: PathBuf,
}

/// One entry's validated destination.
struct ResolvedEntry {
    /// The absolute path to create, inside the extraction root.
    target: PathBuf,
    /// The path relative to the root, kept because a symlink's target is
    /// resolved against this entry's own directory.
    relative: PathBuf,
    /// The entry name as the archive spells it, for error messages.
    name: String,
}

/// Validates one entry and resolves it to a path inside the root.
///
/// `Ok(None)` means the entry vanished into `strip_components`; anything unsafe
/// is an error rather than a skip, because an archive containing a traversal
/// attempt is an archive to refuse, not one to partially trust.
fn resolve_entry<R: Read>(
    entry: &tar::Entry<'_, R>,
    root: &Path,
    strip_components: usize,
) -> Result<Option<ResolvedEntry>, ArchiveError> {
    // The raw bytes, not `entry.path()`: that helper already sanitizes some of
    // what has to be *rejected* here, and a check that runs after the library
    // has quietly fixed the input is not a check.
    let raw = entry.path_bytes();
    let name = String::from_utf8_lossy(&raw).into_owned();

    if let Some(kind) = forbidden_entry_type(entry.header().entry_type()) {
        return Err(ArchiveError::UnsafeEntryType { entry: name, kind });
    }

    let Some(relative) = safe_relative_path(&raw, strip_components) else {
        return Err(ArchiveError::UnsafePath { entry: name });
    };
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }

    let target = root.join(&relative);
    // Belt and braces: `safe_relative_path` already refused `..` and absolute
    // paths, so this can only fail if that logic is wrong. It is cheap, and it
    // is the check that enforces the extraction boundary.
    if target.starts_with(root) {
        Ok(Some(ResolvedEntry {
            target,
            relative,
            name,
        }))
    } else {
        Err(ArchiveError::UnsafePath { entry: name })
    }
}

/// Creates a symlink whose target has been shown to stay inside the root.
///
/// The link is written **relative**, exactly as the archive spells it, so the
/// installed runtime can be moved without every link in `legal/` breaking.
fn validated_link_target<R: Read>(
    entry: &tar::Entry<'_, R>,
    resolved: &ResolvedEntry,
) -> Result<PathBuf, ArchiveError> {
    let raw = entry
        .link_name_bytes()
        .ok_or_else(|| ArchiveError::UnsafeEntryType {
            entry: resolved.name.clone(),
            kind: "symlink with no target",
        })?;

    let Some(link) = safe_link_target(&raw, &resolved.relative) else {
        return Err(ArchiveError::UnsafePath {
            entry: format!("{} -> {}", resolved.name, String::from_utf8_lossy(&raw)),
        });
    };

    Ok(link)
}

fn validate_link_graph(links: &[PendingLink]) -> Result<(), ArchiveError> {
    let mut graph = BTreeMap::new();
    for link in links {
        if graph
            .insert(link.resolved.relative.clone(), link.target.clone())
            .is_some()
        {
            return Err(ArchiveError::UnsafePath {
                entry: link.resolved.name.clone(),
            });
        }
    }

    for link in links {
        // Creating a link below another archive link follows that ancestor in
        // the destination path before the new link is created. Reject the
        // layout outright so the lexical parent used by target resolution is
        // also the parent where the filesystem operation will land. The graph
        // is complete here, so this covers forward references as well as links
        // that appeared earlier in the tar stream.
        if link
            .resolved
            .relative
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .any(|ancestor| graph.contains_key(ancestor))
        {
            return Err(ArchiveError::UnsafePath {
                entry: link.resolved.name.clone(),
            });
        }

        if !link_resolves_inside(&link.resolved.relative, &link.target, &graph) {
            return Err(ArchiveError::UnsafePath {
                entry: link.resolved.name.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Debug)]
enum LinkComponent {
    Normal(OsString),
    Parent,
}

fn link_resolves_inside(
    link_path: &Path,
    target: &Path,
    graph: &BTreeMap<PathBuf, PathBuf>,
) -> bool {
    let mut resolved: Vec<OsString> = link_path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .collect();
    let mut remaining = link_components(target);
    let mut visited = BTreeSet::new();

    while let Some(component) = remaining.pop_front() {
        match component {
            LinkComponent::Parent => {
                if resolved.pop().is_none() {
                    return false;
                }
            }
            LinkComponent::Normal(value) => {
                let mut candidate: PathBuf = resolved.iter().collect();
                candidate.push(&value);
                if let Some(next_target) = graph.get(&candidate) {
                    if !visited.insert(candidate) {
                        return false;
                    }
                    let expansion = link_components(next_target);
                    for nested in expansion.into_iter().rev() {
                        remaining.push_front(nested);
                    }
                } else {
                    resolved.push(value);
                }
            }
        }
    }
    true
}

fn link_components(path: &Path) -> VecDeque<LinkComponent> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => {
                Some(LinkComponent::Normal(value.to_os_string()))
            }
            std::path::Component::ParentDir => Some(LinkComponent::Parent),
            std::path::Component::CurDir => None,
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                Some(LinkComponent::Parent)
            }
        })
        .collect()
}

fn create_symlink(resolved: &ResolvedEntry, link: &Path) -> Result<(), ArchiveError> {
    if let Some(parent) = resolved.target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ArchiveError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    #[cfg(unix)]
    {
        // A previous entry may have created this path; a symlink over an
        // existing file fails rather than replacing it, which is the safer
        // default for an archive that names the same path twice.
        std::os::unix::fs::symlink(link, &resolved.target).map_err(|source| {
            ArchiveError::Unwritable {
                path: resolved.target.clone(),
                source,
            }
        })
    }
    #[cfg(not(unix))]
    {
        let _ = link;
        Err(ArchiveError::UnsafeEntryType {
            entry: resolved.name.clone(),
            kind: "symlink",
        })
    }
}

/// Writes one file entry, enforcing both size limits. Returns the new running
/// byte total.
fn extract_file<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    target: &Path,
    written_so_far: u64,
    limits: &ExtractionLimits,
) -> Result<u64, ArchiveError> {
    let declared = entry.header().size().unwrap_or(0);
    check_size(declared, written_so_far.saturating_add(declared), limits)?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ArchiveError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mode = entry.header().mode().unwrap_or(0o644);
    // Read through a bounded reader rather than trusting the header's size: a
    // header may claim less than the stream delivers, and the declared figure
    // is attacker-controlled.
    let mut limited = entry.take(limits.max_entry_bytes.saturating_add(1));
    let written = write_file(target, &mut limited, mode)?;

    let total = written_so_far.saturating_add(written);
    check_size(written, total, limits)?;
    Ok(total)
}

/// The two size limits, checked together so neither can be forgotten.
const fn check_size(entry: u64, total: u64, limits: &ExtractionLimits) -> Result<(), ArchiveError> {
    if entry > limits.max_entry_bytes {
        return Err(ArchiveError::LimitExceeded {
            limit: "entry-size",
            allowed: limits.max_entry_bytes,
            reached: entry,
        });
    }
    if total > limits.max_total_bytes {
        return Err(ArchiveError::LimitExceeded {
            limit: "total-size",
            allowed: limits.max_total_bytes,
            reached: total,
        });
    }
    Ok(())
}

/// Writes one entry, preserving only its permission bits.
fn write_file(target: &Path, source: &mut impl Read, mode: u32) -> Result<u64, ArchiveError> {
    let unwritable = |source: io::Error| ArchiveError::Unwritable {
        path: target.to_path_buf(),
        source,
    };
    let mut file = File::create(target).map_err(unwritable)?;
    let written = io::copy(source, &mut file).map_err(unwritable)?;

    // Without this, `bin/java` arrives at 0644 and the entire install is inert.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Only the permission bits: setuid, setgid, and the sticky bit are
        // never honored from an archive.
        let permissions = std::fs::Permissions::from_mode(mode & 0o777);
        std::fs::set_permissions(target, permissions).map_err(unwritable)?;
    }
    #[cfg(not(unix))]
    let _ = mode;

    Ok(written)
}

/// Entry types that are never extracted, with the name to report.
const fn forbidden_entry_type(kind: EntryType) -> Option<&'static str> {
    match kind {
        // Files and directories are the whole of a JDK tarball. GNU/PAX
        // metadata entries carry no content of their own - the `tar` crate
        // applies them to the entries that follow, which is what lets a long
        // filename work - so they pass through and never become files.
        EntryType::Regular
        | EntryType::Directory
        | EntryType::Continuous
        // Resolved rather than banned: a Temurin JDK genuinely contains a few
        // hundred of them. See `safe_link_target`.
        | EntryType::Symlink
        | EntryType::GNULongName
        | EntryType::GNULongLink
        | EntryType::GNUSparse
        | EntryType::XGlobalHeader
        | EntryType::XHeader => None,
        // Still refused. A tar hard link names an earlier entry by path, so
        // honoring one means resolving that path and calling `fs::hard_link`
        // - a second containment problem for a feature no JDK archive uses.
        EntryType::Link => Some("hard link"),
        EntryType::Char | EntryType::Block => Some("device node"),
        EntryType::Fifo => Some("fifo"),
        _ => Some("unsupported entry"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Builds a `.tar.gz` from `(name, mode, contents)` triples.
    fn tar_gz(path: &Path, entries: &[(&str, u32, &[u8])]) {
        let file = File::create(path).expect("create archive");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, mode, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *contents)
                .expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("gz");
    }

    /// Builds a `.tar.gz` whose entry names bypass `tar`'s own validation.
    ///
    /// `Builder::append_data` refuses a path containing `..`, which is helpful
    /// for writing archives and useless for testing a reader: the hostile
    /// archive this has to survive was not built with this crate. The name is
    /// therefore written straight into the header field.
    fn tar_gz_raw_names(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create archive");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, contents) in entries {
            let mut header = tar::Header::new_old();
            let bytes = name.as_bytes();
            assert!(
                bytes.len() < 100,
                "the old header's name field is 100 bytes"
            );
            header.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(EntryType::Regular);
            header.set_cksum();
            builder.append(&header, *contents).expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("gz");
    }

    /// A tarball holding one ordinary file plus one link entry.
    ///
    /// The file is there so that a refusal is provably a refusal rather than
    /// [`ArchiveError::Empty`] - an archive with nothing extractable in it
    /// would fail either way and prove nothing.
    fn tar_gz_with_link(path: &Path, name: &str, target: &str, kind: EntryType) {
        let file = File::create(path).expect("create archive");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);

        let mut regular = tar::Header::new_gnu();
        regular.set_size(8);
        regular.set_mode(0o644);
        regular.set_cksum();
        builder
            .append_data(&mut regular, "jdk/release", &b"JAVA=21\n"[..])
            .expect("append");

        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_entry_type(kind);
        header.set_link_name(target).expect("link name");
        header.set_path(name).expect("path");
        header.set_cksum();
        builder.append(&header, io::empty()).expect("append link");
        builder.into_inner().expect("finish").finish().expect("gz");
    }

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    /// Builds a tarball entry by entry, so a test can order links and the paths
    /// that walk through them.
    fn tar_gz_entries(path: &Path, entries: &[(&str, EntryType, &str, &[u8])]) {
        let file = File::create(path).expect("create archive");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, kind, link, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(0o755);
            if *kind == EntryType::Symlink {
                header.set_size(0);
                header.set_link_name(link).expect("link name");
            } else {
                header.set_size(contents.len() as u64);
            }
            header.set_path(name).expect("path");
            header.set_cksum();
            let body: &[u8] = if *kind == EntryType::Symlink {
                &[]
            } else {
                contents
            };
            builder.append(&header, body).expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("gz");
    }

    /// Judging a link by the path the *archive* gives it is not enough once an
    /// earlier link has moved where that path really is. `a/l1 ->..` lands on
    /// the root and is legitimate; `a/l1/l2` then really lives at the root, so
    /// its own `../..` climbs two levels above where the checker thinks it
    /// stands. Every entry underneath inherits the escape.
    #[test]
    fn a_link_reached_through_an_earlier_link_cannot_escape() {
        let base = temp();
        let archive = base.path().join("chain.tar.gz");
        let root = base.path().join("root");
        let outside = base.path().join("outside");
        // The directory the chain aims at already exists - a hostile archive
        // targets somewhere real, like a config or autostart directory. Without
        // it the escape only fails because a dangling link is unwritable, which
        // proves nothing about the containment check.
        std::fs::create_dir_all(&outside).expect("create the target directory");

        tar_gz_entries(
            &archive,
            &[
                ("jdk/release", EntryType::Regular, "", b"JAVA=21\n"),
                ("jdk/a/l1", EntryType::Symlink, "..", b""),
                // The checker measures this from `a/l1/`, two levels deep, and
                // reads it as `a/outside`. It is really written at the root,
                // one level up, so it lands beside the root instead.
                ("jdk/a/l1/l2", EntryType::Symlink, "../outside", b""),
                ("jdk/a/l1/l2/pwned", EntryType::Regular, "", b"owned\n"),
            ],
        );

        let result = extract_tar_gz(&archive, &root, 1, &ExtractionLimits::default());

        assert!(
            !outside.join("pwned").exists(),
            "an entry escaped the extraction root via a chain of links"
        );
        assert!(
            result.is_err(),
            "the escape attempt should be refused, not silently contained"
        );
    }

    #[test]
    fn composed_links_cannot_resolve_outside_in_either_archive_order() {
        for reverse in [false, true] {
            let base = temp();
            let archive = base.path().join("chain.tar.gz");
            let root = base.path().join("root");
            let mut links = vec![
                ("jdk/d/l", EntryType::Symlink, "..", &b""[..]),
                ("jdk/escape", EntryType::Symlink, "d/l/../outside", &b""[..]),
            ];
            if reverse {
                links.reverse();
            }
            let mut entries = vec![("jdk/release", EntryType::Regular, "", &b"JAVA=21\n"[..])];
            entries.extend(links);
            tar_gz_entries(&archive, &entries);

            let error = extract_tar_gz(&archive, &root, 1, &ExtractionLimits::default())
                .expect_err("a composed link escape must be refused");
            assert!(matches!(error, ArchiveError::UnsafePath { .. }));
            assert!(
                !root.join("escape").exists(),
                "link creation must wait until the complete graph is validated"
            );
        }
    }

    #[test]
    fn a_link_destination_below_another_link_is_refused_in_either_archive_order() {
        for reverse in [false, true] {
            let base = temp();
            let archive = base.path().join("nested-destination.tar.gz");
            let root = base.path().join("root");
            let outside = base.path().join("outside");
            let mut links = vec![
                ("jdk/a", EntryType::Symlink, ".", &b""[..]),
                ("jdk/a/b", EntryType::Symlink, "../outside", &b""[..]),
            ];
            if reverse {
                links.reverse();
            }
            let mut entries = vec![("jdk/release", EntryType::Regular, "", &b"JAVA=21\n"[..])];
            entries.extend(links);
            tar_gz_entries(&archive, &entries);

            let error = extract_tar_gz(&archive, &root, 1, &ExtractionLimits::default())
                .expect_err("a link created through another link must be refused");
            assert!(matches!(error, ArchiveError::UnsafePath { .. }));
            assert!(
                !outside.exists(),
                "the link target must not escape the root"
            );
            assert!(
                !root.join("a").exists(),
                "no archive link may be created before graph validation"
            );
            assert!(
                !root.join("b").exists(),
                "the nested link must not be created at its effective path"
            );
        }
    }

    #[test]
    fn cyclic_symlinks_are_refused_before_any_link_is_created() {
        let base = temp();
        let archive = base.path().join("cycle.tar.gz");
        let root = base.path().join("root");
        tar_gz_entries(
            &archive,
            &[
                ("jdk/release", EntryType::Regular, "", b"JAVA=21\n"),
                ("jdk/a", EntryType::Symlink, "b", b""),
                ("jdk/b", EntryType::Symlink, "a", b""),
            ],
        );

        let error = extract_tar_gz(&archive, &root, 1, &ExtractionLimits::default())
            .expect_err("a link cycle must be refused");
        assert!(matches!(error, ArchiveError::UnsafePath { .. }));
        assert!(!root.join("a").exists());
        assert!(!root.join("b").exists());
    }

    #[test]
    fn a_temurin_shaped_archive_unwraps_its_top_level_directory() {
        let dir = temp();
        let archive = dir.path().join("jdk.tar.gz");
        tar_gz(
            &archive,
            &[
                (
                    "jdk-21.0.12+8/release",
                    0o644,
                    b"JAVA_VERSION=\"21.0.12\"\n",
                ),
                ("jdk-21.0.12+8/bin/java", 0o755, b"#!/bin/sh\n"),
                ("jdk-21.0.12+8/lib/modules", 0o644, b"binary"),
            ],
        );

        let destination = dir.path().join("install");
        let report = extract_tar_gz(&archive, &destination, 1, &ExtractionLimits::default())
            .expect("extract");

        assert_eq!(report.files, 3);
        assert!(
            destination.join("bin/java").is_file(),
            "bin/java must land at the root"
        );
        assert!(destination.join("release").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_survives_extraction() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp();
        let archive = dir.path().join("jdk.tar.gz");
        tar_gz(
            &archive,
            &[
                ("jdk/bin/java", 0o755, b"#!/bin/sh\n"),
                ("jdk/release", 0o644, b"x"),
            ],
        );

        let destination = dir.path().join("install");
        extract_tar_gz(&archive, &destination, 1, &ExtractionLimits::default()).expect("extract");

        let mode = std::fs::metadata(destination.join("bin/java"))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "an inert bin/java is a useless install"
        );
        assert_eq!(
            std::fs::metadata(destination.join("release"))
                .expect("metadata")
                .permissions()
                .mode()
                & 0o111,
            0,
            "and a data file must not become executable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn setuid_is_stripped_even_when_the_archive_asks_for_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp();
        let archive = dir.path().join("jdk.tar.gz");
        tar_gz(&archive, &[("jdk/bin/java", 0o4755, b"#!/bin/sh\n")]);

        let destination = dir.path().join("install");
        extract_tar_gz(&archive, &destination, 1, &ExtractionLimits::default()).expect("extract");

        let mode = std::fs::metadata(destination.join("bin/java"))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7000, 0, "a downloaded archive does not get setuid");
    }

    #[test]
    fn an_entry_climbing_out_of_the_root_is_refused() {
        let dir = temp();
        let archive = dir.path().join("evil.tar.gz");
        tar_gz_raw_names(
            &archive,
            &[
                ("jdk/bin/java", b"#!/bin/sh\n"),
                ("jdk/../../../../etc/cron.d/pwn", b"* * * * * root sh\n"),
            ],
        );

        let destination = dir.path().join("install");
        let error = extract_tar_gz(&archive, &destination, 1, &ExtractionLimits::default())
            .expect_err("traversal must be refused");
        assert!(
            matches!(error, ArchiveError::UnsafePath { .. }),
            "{error:?}"
        );
        assert!(
            !dir.path().join("etc").exists(),
            "nothing may be written outside the destination"
        );
    }

    #[test]
    fn an_absolute_entry_is_refused() {
        let dir = temp();
        let archive = dir.path().join("evil.tar.gz");
        tar_gz_raw_names(&archive, &[("/etc/cron.d/pwn", b"* * * * * root sh\n")]);

        let error = extract_tar_gz(
            &archive,
            &dir.path().join("install"),
            0,
            &ExtractionLimits::default(),
        )
        .expect_err("an absolute entry must be refused");
        assert!(
            matches!(error, ArchiveError::UnsafePath { .. }),
            "{error:?}"
        );
        assert!(!Path::new("/etc/cron.d/pwn").exists());
    }

    #[test]
    fn the_path_check_refuses_every_shape_of_escape() {
        assert!(safe_relative_path(b"/etc/passwd", 0).is_none(), "absolute");
        assert!(
            safe_relative_path(b"C:\\windows\\system32", 0).is_none(),
            "windows drive path"
        );
        assert!(
            safe_relative_path(b"jdk/bin/../../etc/passwd", 0).is_none(),
            "traversal"
        );
        assert!(
            safe_relative_path(b"jdk/bin/ja\0va", 0).is_none(),
            "NUL byte"
        );
        assert!(
            safe_relative_path(&[b'j', 0xff, 0xfe, b'k'], 0).is_none(),
            "malformed UTF-8"
        );
    }

    #[test]
    fn a_symlink_that_escapes_the_root_is_refused() {
        let dir = temp();
        let archive = dir.path().join("link.tar.gz");
        tar_gz_with_link(
            &archive,
            "jdk/bin/java",
            "../../../../../../etc/shadow",
            EntryType::Symlink,
        );

        let error = extract_tar_gz(
            &archive,
            &dir.path().join("install"),
            1,
            &ExtractionLimits::default(),
        )
        .expect_err("an escaping symlink must be refused");
        assert!(
            matches!(error, ArchiveError::UnsafePath { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn an_absolute_symlink_is_refused() {
        let dir = temp();
        let archive = dir.path().join("link.tar.gz");
        tar_gz_with_link(&archive, "jdk/bin/java", "/etc/shadow", EntryType::Symlink);

        let error = extract_tar_gz(
            &archive,
            &dir.path().join("install"),
            1,
            &ExtractionLimits::default(),
        )
        .expect_err("an absolute symlink must be refused");
        assert!(
            matches!(error, ArchiveError::UnsafePath { .. }),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_relative_symlinks_a_real_jdk_contains_are_created() {
        // Exactly the shape Temurin ships 208 of. The resolved target remains
        // inside the archive root, so this valid upstream link is accepted.
        let dir = temp();
        let archive = dir.path().join("jdk.tar.gz");

        let file = File::create(&archive).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);

        let mut license = tar::Header::new_gnu();
        license.set_size(8);
        license.set_mode(0o644);
        license.set_cksum();
        builder
            .append_data(
                &mut license,
                "jdk/legal/java.base/LICENSE",
                &b"GPLv2+CE"[..],
            )
            .expect("append");

        let mut link = tar::Header::new_gnu();
        link.set_size(0);
        link.set_mode(0o777);
        link.set_entry_type(EntryType::Symlink);
        link.set_link_name("../java.base/LICENSE")
            .expect("link name");
        link.set_path("jdk/legal/jdk.naming.rmi/LICENSE")
            .expect("path");
        link.set_cksum();
        builder.append(&link, io::empty()).expect("append link");

        let mut java = tar::Header::new_gnu();
        java.set_size(10);
        java.set_mode(0o755);
        java.set_cksum();
        builder
            .append_data(&mut java, "jdk/bin/java", &b"#!/bin/sh\n"[..])
            .expect("append");
        builder.into_inner().expect("finish").finish().expect("gz");

        let destination = dir.path().join("install");
        let report = extract_tar_gz(&archive, &destination, 1, &ExtractionLimits::default())
            .expect("a JDK's own symlinks must not be refused");

        assert_eq!(report.symlinks, 1);
        let created = destination.join("legal/jdk.naming.rmi/LICENSE");
        assert!(
            created
                .symlink_metadata()
                .expect("symlink metadata")
                .is_symlink(),
            "the link must be a link, not a copy"
        );
        assert_eq!(
            std::fs::read_to_string(&created).expect("read through the link"),
            "GPLv2+CE",
            "and it must resolve to the file it names"
        );
        assert_eq!(
            std::fs::read_link(&created).expect("read link"),
            PathBuf::from("../java.base/LICENSE"),
            "kept relative so the install can be moved"
        );
    }

    #[test]
    fn link_targets_are_judged_by_where_they_land_not_by_whether_they_contain_dotdot() {
        // The distinction the whole rule turns on. Both of these climb out of
        // their own directory; only one of them leaves the root.
        let inside = Path::new("legal/jdk.naming.rmi/LICENSE");
        assert!(
            safe_link_target(b"../java.base/LICENSE", inside).is_some(),
            "a link that climbs one level and comes back is what a JDK is full of"
        );
        assert!(
            safe_link_target(b"../../../../etc/shadow", inside).is_none(),
            "a link that climbs past the root is the one to refuse"
        );

        // Depth is measured from the link's own directory, so the same target
        // is safe deep in the tree and unsafe near the top.
        assert!(safe_link_target(b"../x", Path::new("a/b/link")).is_some());
        assert!(safe_link_target(b"../../x", Path::new("a/link")).is_none());

        assert!(
            safe_link_target(b"/etc/shadow", inside).is_none(),
            "absolute"
        );
        assert!(safe_link_target(b"", inside).is_none(), "empty");
        assert!(
            safe_link_target(&[b'a', 0, b'b'], inside).is_none(),
            "NUL byte"
        );
        assert!(
            safe_link_target(b"C:\\windows", inside).is_none(),
            "windows drive path"
        );
    }

    #[test]
    fn a_hard_link_entry_is_refused_outright() {
        let dir = temp();
        let archive = dir.path().join("link.tar.gz");
        tar_gz_with_link(
            &archive,
            "jdk/bin/java",
            "../../etc/shadow",
            EntryType::Link,
        );

        let error = extract_tar_gz(
            &archive,
            &dir.path().join("install"),
            1,
            &ExtractionLimits::default(),
        )
        .expect_err("hard links are never extracted");
        assert!(
            matches!(
                error,
                ArchiveError::UnsafeEntryType {
                    kind: "hard link",
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn the_entry_count_limit_stops_an_archive_of_many_tiny_files() {
        let dir = temp();
        let archive = dir.path().join("many.tar.gz");
        let names: Vec<String> = (0..50).map(|index| format!("jdk/f{index}")).collect();
        let entries: Vec<(&str, u32, &[u8])> = names
            .iter()
            .map(|name| (name.as_str(), 0o644, &b"x"[..]))
            .collect();
        tar_gz(&archive, &entries);

        let limits = ExtractionLimits {
            max_entries: 10,
            ..ExtractionLimits::default()
        };
        let error = extract_tar_gz(&archive, &dir.path().join("install"), 1, &limits)
            .expect_err("the limit must bite");
        assert!(
            matches!(
                error,
                ArchiveError::LimitExceeded {
                    limit: "entry-count",
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn the_size_limit_stops_a_compression_bomb() {
        let dir = temp();
        let archive = dir.path().join("bomb.tar.gz");
        // Highly compressible: 2 MiB of zeroes packs into a few hundred bytes.
        let payload = vec![0u8; 2 * 1024 * 1024];
        tar_gz(&archive, &[("jdk/big", 0o644, &payload)]);

        let limits = ExtractionLimits {
            max_total_bytes: 1024,
            max_entry_bytes: 1024,
            ..ExtractionLimits::default()
        };
        let error = extract_tar_gz(&archive, &dir.path().join("install"), 1, &limits)
            .expect_err("the limit must bite");
        assert!(
            matches!(error, ArchiveError::LimitExceeded { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn an_archive_that_strips_down_to_nothing_is_an_error_rather_than_a_silent_success() {
        let dir = temp();
        let archive = dir.path().join("flat.tar.gz");
        tar_gz(&archive, &[("release", 0o644, b"x")]);

        let error = extract_tar_gz(
            &archive,
            &dir.path().join("install"),
            1,
            &ExtractionLimits::default(),
        )
        .expect_err("an empty result is a wrong assumption, not an outcome");
        assert!(matches!(error, ArchiveError::Empty), "{error:?}");
    }

    #[test]
    fn strip_components_removes_exactly_the_requested_depth() {
        assert_eq!(
            safe_relative_path(b"jdk-21/bin/java", 1).expect("safe"),
            PathBuf::from("bin/java")
        );
        assert_eq!(
            safe_relative_path(b"jdk-21/bin/java", 0).expect("safe"),
            PathBuf::from("jdk-21/bin/java")
        );
        assert_eq!(
            safe_relative_path(b"jdk-21", 1).expect("safe"),
            PathBuf::new(),
            "the wrapper directory itself becomes nothing"
        );
        assert_eq!(
            safe_relative_path(b"./jdk-21/bin/java", 1).expect("safe"),
            PathBuf::from("bin/java"),
            "a `.` component is noise, not depth"
        );
    }
}
