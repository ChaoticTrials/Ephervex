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
//! links, and creates links only after the entire graph is safe. Every link is
//! resolved once, so validation time grows linearly with the entry names.
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

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Components, Path, PathBuf};

use ephervex_pack::path::{safe_link_target, safe_relative_path};
use flate2::read::GzDecoder;
use tar::{Archive, EntryType};
use tokio_util::sync::CancellationToken;
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

    /// The extraction was canceled before it finished.
    #[error("the extraction was canceled")]
    Canceled,
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
            Self::Canceled => "error-java-managed-canceled",
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
pub fn extract_tar_gz(
    archive: &Path,
    destination: &Path,
    strip_components: usize,
    limits: &ExtractionLimits,
) -> Result<ExtractionReport, ArchiveError> {
    extract_tar_gz_until_canceled(
        archive,
        destination,
        strip_components,
        limits,
        &CancellationToken::new(),
    )
}

/// [`extract_tar_gz`] that stops with [`ArchiveError::Canceled`] once `cancel`
/// fires, between entries and between the reads of a large one.
///
/// What was written before the stop stays in `destination`; the caller owns
/// that directory and removes it.
#[instrument(skip(limits, cancel), fields(archive = %archive.display(), destination = %destination.display()))]
pub(crate) fn extract_tar_gz_until_canceled(
    archive: &Path,
    destination: &Path,
    strip_components: usize,
    limits: &ExtractionLimits,
    cancel: &CancellationToken,
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
        if cancel.is_cancelled() {
            return Err(ArchiveError::Canceled);
        }
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

        // A pax global header describes the whole archive and `tar` hands it
        // back as an ordinary entry; its payload is metadata, never a file.
        if entry.header().entry_type().is_pax_global_extensions() {
            continue;
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

        report.bytes = extract_file(
            archive,
            &mut entry,
            &resolved.target,
            report.bytes,
            limits,
            cancel,
        )?;
        report.files += 1;
    }

    validate_link_graph(&pending_links)?;
    for link in pending_links {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Canceled);
        }
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
    LinkResolver::new(links)?.validate()
}

const ROOT: usize = 0;

#[derive(Clone, Copy)]
enum Resolution {
    Unresolved,
    InProgress,
    /// The directory node the link's target finally resolves to.
    Done(usize),
}

/// A link being resolved, suspended while a link it passes through resolves.
struct Frame<'a> {
    link: usize,
    components: Components<'a>,
    at: usize,
}

/// Resolves every archive link through the complete link graph.
///
/// Paths are interned as nodes of one tree, so a position is a single index
/// and stepping into or out of a directory is constant time. Each link is
/// resolved once and its destination reused by every path that crosses it,
/// which keeps the work linear in the total size of the entry names.
struct LinkResolver<'a> {
    links: &'a [PendingLink],
    children: HashMap<(usize, &'a OsStr), usize>,
    parents: Vec<usize>,
    link_at: Vec<Option<usize>>,
    link_node: Vec<usize>,
    state: Vec<Resolution>,
    #[cfg(test)]
    steps: u64,
}

impl<'a> LinkResolver<'a> {
    fn new(links: &'a [PendingLink]) -> Result<Self, ArchiveError> {
        let mut resolver = Self {
            links,
            children: HashMap::new(),
            parents: vec![ROOT],
            link_at: vec![None],
            link_node: Vec::with_capacity(links.len()),
            state: vec![Resolution::Unresolved; links.len()],
            #[cfg(test)]
            steps: 0,
        };
        for (index, link) in links.iter().enumerate() {
            let mut node = ROOT;
            for component in link.resolved.relative.components() {
                if let Component::Normal(name) = component {
                    node = resolver.child(node, name);
                }
            }
            if resolver.link_at[node].replace(index).is_some() {
                return Err(ArchiveError::UnsafePath {
                    entry: link.resolved.name.clone(),
                });
            }
            resolver.link_node.push(node);
        }
        Ok(resolver)
    }

    fn child(&mut self, parent: usize, name: &'a OsStr) -> usize {
        match self.children.entry((parent, name)) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let node = self.parents.len();
                self.parents.push(parent);
                self.link_at.push(None);
                entry.insert(node);
                node
            }
        }
    }

    #[cfg(test)]
    fn tick(&mut self) {
        self.steps += 1;
    }

    fn validate(&mut self) -> Result<(), ArchiveError> {
        for index in 0..self.links.len() {
            // Creating a link below another archive link follows that ancestor
            // in the destination path before the new link is created. Reject
            // the layout outright so the lexical parent used by target
            // resolution is also the parent where the filesystem operation
            // will land. The graph is complete here, so this covers forward
            // references as well as links earlier in the tar stream.
            if self.has_link_ancestor(index) || !self.resolve(index) {
                return Err(ArchiveError::UnsafePath {
                    entry: self.links[index].resolved.name.clone(),
                });
            }
        }
        Ok(())
    }

    fn has_link_ancestor(&mut self, index: usize) -> bool {
        let mut node = self.parents[self.link_node[index]];
        while node != ROOT {
            #[cfg(test)]
            self.tick();
            if self.link_at[node].is_some() {
                return true;
            }
            node = self.parents[node];
        }
        false
    }

    /// Whether the target of `start` stays inside the root and is not part of a
    /// cycle. Iterative, because a hostile archive can chain links as deep as
    /// its entry limit allows.
    fn resolve(&mut self, start: usize) -> bool {
        if !matches!(self.state[start], Resolution::Unresolved) {
            return true;
        }
        self.state[start] = Resolution::InProgress;
        let mut frames = vec![Frame {
            link: start,
            components: self.links[start].target.components(),
            at: self.parents[self.link_node[start]],
        }];

        while let Some(frame) = frames.last_mut() {
            #[cfg(test)]
            self.tick();
            match frame.components.next() {
                None => {
                    let (link, at) = (frame.link, frame.at);
                    self.state[link] = Resolution::Done(at);
                    frames.pop();
                    if let Some(waiting) = frames.last_mut() {
                        waiting.at = at;
                    }
                }
                Some(Component::CurDir) => {}
                Some(Component::ParentDir | Component::RootDir | Component::Prefix(_)) => {
                    if frame.at == ROOT {
                        return false;
                    }
                    frame.at = self.parents[frame.at];
                }
                Some(Component::Normal(name)) => {
                    let candidate = self.child(frame.at, name);
                    let Some(next) = self.link_at[candidate] else {
                        frame.at = candidate;
                        continue;
                    };
                    match self.state[next] {
                        Resolution::Done(node) => frame.at = node,
                        Resolution::InProgress => return false,
                        Resolution::Unresolved => {
                            let at = frame.at;
                            self.state[next] = Resolution::InProgress;
                            frames.push(Frame {
                                link: next,
                                components: self.links[next].target.components(),
                                at,
                            });
                        }
                    }
                }
            }
        }
        true
    }
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
///
/// A file that was refused or that failed part-way is removed, so nothing left
/// in the destination looks like a complete entry.
fn extract_file<R: Read>(
    archive: &Path,
    entry: &mut tar::Entry<'_, R>,
    target: &Path,
    written_so_far: u64,
    limits: &ExtractionLimits,
    cancel: &CancellationToken,
) -> Result<u64, ArchiveError> {
    // `Entry::size` is the length the stream delivers: a PAX `size` record
    // overrides the ustar header field that `Header::size` reports.
    let declared = entry.size();
    check_size(declared, written_so_far.saturating_add(declared), limits)?;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ArchiveError::Unwritable {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mode = entry.header().mode().unwrap_or(0o644);
    // The declared size is attacker-controlled, so the copy is bounded by the
    // tighter of the two limits, with one byte of headroom so that an overrun is
    // observed rather than silently truncated.
    let remaining = limits.max_total_bytes.saturating_sub(written_so_far);
    let bound = limits.max_entry_bytes.min(remaining);
    let mut limited = entry.take(bound.saturating_add(1));

    let written = write_file(archive, target, &mut limited, mode, cancel).and_then(|written| {
        let total = written_so_far.saturating_add(written);
        check_size(written, total, limits)?;
        // An intact stream delivers exactly `declared` bytes. A shorter entry
        // means the tar ended mid-file without the gzip layer failing.
        if written < declared {
            return Err(ArchiveError::Unreadable {
                path: archive.to_path_buf(),
                source: io::Error::from(io::ErrorKind::UnexpectedEof),
            });
        }
        Ok(written)
    });
    match written {
        Ok(written) => Ok(written_so_far.saturating_add(written)),
        Err(error) => {
            // Best effort: the extraction is already failing, and the caller
            // discards the whole destination.
            let _ = std::fs::remove_file(target);
            Err(error)
        }
    }
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
///
/// Copies by hand rather than with [`io::copy`] so a failure reading the
/// archive (a corrupt gzip stream) is reported against `archive` and only a
/// failure writing `target` is reported against the destination. Cancellation
/// is checked before every read, so a large entry stops mid-copy.
fn write_file(
    archive: &Path,
    target: &Path,
    source: &mut impl Read,
    mode: u32,
    cancel: &CancellationToken,
) -> Result<u64, ArchiveError> {
    let unwritable = |source: io::Error| ArchiveError::Unwritable {
        path: target.to_path_buf(),
        source,
    };
    let mut file = File::create(target).map_err(unwritable)?;

    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut written: u64 = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Canceled);
        }
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(if cancel.is_cancelled() {
                    ArchiveError::Canceled
                } else {
                    ArchiveError::Unreadable {
                        path: archive.to_path_buf(),
                        source,
                    }
                });
            }
        };
        file.write_all(&buffer[..read]).map_err(unwritable)?;
        written = written.saturating_add(read as u64);
    }

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

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Entry types that are never extracted, with the name to report.
const fn forbidden_entry_type(kind: EntryType) -> Option<&'static str> {
    match kind {
        // Files and directories are the whole of a JDK tarball. A pax global
        // header is also accepted here, but the extraction loop skips it before
        // this check runs: `tar` returns it as an ordinary entry.
        EntryType::Regular
        | EntryType::Directory
        | EntryType::Continuous
        // Resolved rather than banned: a Temurin JDK genuinely contains a few
        // hundred of them. See `safe_link_target`.
        | EntryType::Symlink
        | EntryType::GNUSparse
        | EntryType::XGlobalHeader => None,
        // `tar` applies these to the entry that follows, so they only reach
        // the caller when their header is not ustar or GNU. The following
        // entry would then be extracted under the wrong name, so refuse.
        EntryType::GNULongName | EntryType::GNULongLink | EntryType::XHeader => {
            Some("unapplied extended header")
        }
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
    fn a_canceled_extraction_writes_nothing() {
        let base = temp();
        let archive = base.path().join("jdk.tar.gz");
        let root = base.path().join("root");
        tar_gz(&archive, &[("jdk/bin/java", 0o755, b"#!/bin/sh\n")]);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = extract_tar_gz_until_canceled(
            &archive,
            &root,
            1,
            &ExtractionLimits::default(),
            &cancel,
        )
        .expect_err("a canceled extraction must stop");

        assert!(matches!(error, ArchiveError::Canceled), "{error:?}");
        assert!(!root.join("bin/java").exists());
    }

    #[test]
    fn a_large_entry_stops_copying_when_cancellation_arrives_mid_entry() {
        struct CancelsAfterFirstRead<'a>(&'a CancellationToken);
        impl Read for CancelsAfterFirstRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.0.cancel();
                buffer.fill(0);
                Ok(buffer.len())
            }
        }

        let base = temp();
        let target = base.path().join("big");
        let cancel = CancellationToken::new();

        let error = write_file(
            Path::new("archive.tar.gz"),
            &target,
            &mut CancelsAfterFirstRead(&cancel),
            0o644,
            &cancel,
        )
        .expect_err("the copy must stop once canceled");

        assert!(matches!(error, ArchiveError::Canceled), "{error:?}");
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

    fn pending_link(path: &str, target: &str) -> PendingLink {
        PendingLink {
            resolved: ResolvedEntry {
                target: Path::new("/root").join(path),
                relative: PathBuf::from(path),
                name: path.to_owned(),
            },
            target: PathBuf::from(target),
        }
    }

    /// Validates `links` and returns the verdict with the work it took.
    fn validate_counting(links: &[PendingLink]) -> (Result<(), ArchiveError>, u64) {
        let mut resolver = LinkResolver::new(links).expect("distinct link paths");
        let verdict = resolver.validate();
        (verdict, resolver.steps)
    }

    /// `l{n}` -> `l{n-1}` -> ... -> `l0` -> `release`, listed in `order`.
    fn link_chain(length: usize, step: &str, reverse: bool) -> Vec<PendingLink> {
        let mut links = vec![pending_link("l0", "release")];
        for index in 1..length {
            links.push(pending_link(
                &format!("l{index}"),
                &format!("l{}{step}", index - 1),
            ));
        }
        if reverse {
            links.reverse();
        }
        links
    }

    #[test]
    fn link_validation_work_grows_linearly_with_chained_links() {
        for (step, reverse) in [("", false), ("", true), ("/sub", false), ("/sub", true)] {
            for length in [1_000_usize, 4_000, 16_000] {
                let (verdict, steps) = validate_counting(&link_chain(length, step, reverse));
                verdict.expect("a chain that stays inside the root is accepted");
                assert!(
                    steps <= 8 * length as u64,
                    "{length} chained links took {steps} steps (step {step:?}, reverse {reverse})"
                );
            }
        }
    }

    #[test]
    fn a_deep_link_chain_is_validated_without_recursion() {
        let (verdict, _) = validate_counting(&link_chain(200_000, "", true));
        verdict.expect("a deep chain is accepted");
    }

    #[test]
    fn a_long_link_cycle_is_refused_in_linear_work() {
        let length = 20_000;
        let mut links = link_chain(length, "", false);
        links[0] = pending_link("l0", &format!("l{}", length - 1));

        let (verdict, steps) = validate_counting(&links);

        assert!(matches!(verdict, Err(ArchiveError::UnsafePath { .. })));
        assert!(steps <= 8 * length as u64, "{steps}");
    }

    #[test]
    fn a_long_link_chain_that_ends_outside_the_root_is_refused() {
        let length = 5_000;
        let mut links = link_chain(length, "", false);
        links[0] = pending_link("l0", "../outside");

        for reverse in [false, true] {
            if reverse {
                links.reverse();
            }
            let (verdict, steps) = validate_counting(&links);
            assert!(matches!(verdict, Err(ArchiveError::UnsafePath { .. })));
            assert!(steps <= 8 * length as u64, "{steps}");
        }
    }

    #[test]
    fn links_nested_under_a_deep_directory_resolve_in_linear_work() {
        let depth = "d/".repeat(200);
        let length = 2_000_u64;
        let mut links = vec![pending_link(&format!("{depth}l0"), "../release")];
        for index in 1..length {
            links.push(pending_link(
                &format!("{depth}l{index}"),
                &format!("l{}", index - 1),
            ));
        }

        let (verdict, steps) = validate_counting(&links);

        verdict.expect("links inside the root are accepted");
        assert!(steps <= 8 * 200 * length, "{steps}");
        assert!(steps >= length);
    }

    #[test]
    fn a_link_below_a_link_is_refused_by_the_ancestor_check() {
        let links = vec![pending_link("a", "."), pending_link("a/b", "c")];

        let (verdict, _) = validate_counting(&links);

        assert!(matches!(verdict, Err(ArchiveError::UnsafePath { .. })));
    }

    #[test]
    fn a_path_that_crosses_one_link_twice_without_looping_is_accepted() {
        let links = vec![pending_link("d", "x"), pending_link("via", "d/../d/file")];

        let (verdict, _) = validate_counting(&links);

        verdict.expect("crossing a link twice is not a cycle");
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

    /// Builds a `.tar.gz` whose only entry has a ustar size field smaller than
    /// the data that follows, with the real length in a PAX `size` record.
    fn tar_gz_with_pax_size(path: &Path, name: &str, header_size: u64, contents: &[u8]) {
        let file = File::create(path).expect("create archive");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let real = contents.len().to_string();
        builder
            .append_pax_extensions([("size", real.as_bytes())])
            .expect("pax");
        let mut header = tar::Header::new_gnu();
        header.set_size(header_size);
        header.set_mode(0o644);
        header.set_path(name).expect("path");
        header.set_cksum();
        builder.append(&header, contents).expect("append");
        builder.into_inner().expect("finish").finish().expect("gz");
    }

    /// Builds a `.tar.gz` of one metadata entry of `kind`, followed by `file` when it is not empty.
    fn tar_gz_with_metadata_entry(path: &Path, kind: EntryType, header_name: &str, file: &str) {
        let encoder = flate2::write::GzEncoder::new(
            File::create(path).expect("create archive"),
            flate2::Compression::fast(),
        );
        let mut builder = tar::Builder::new(encoder);
        let payload = b"52 comment=0123456789abcdef0123456789abcdef01234567\n";
        let mut header = if kind == EntryType::XGlobalHeader {
            tar::Header::new_ustar()
        } else {
            tar::Header::new_old()
        };
        header.set_entry_type(kind);
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_path(header_name).expect("path");
        header.set_cksum();
        builder.append(&header, &payload[..]).expect("append");
        if !file.is_empty() {
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o644);
            header.set_path(file).expect("path");
            header.set_cksum();
            builder.append(&header, &b"ok"[..]).expect("append");
        }
        builder.into_inner().expect("finish").finish().expect("gz");
    }

    #[test]
    fn a_pax_global_header_is_skipped_and_never_written_as_a_file() {
        for (header_name, strip) in [("pax_global_header", 0), ("jdk/pax_global_header", 1)] {
            let dir = temp();
            let archive = dir.path().join("global.tar.gz");
            let root = dir.path().join("install");
            tar_gz_with_metadata_entry(&archive, EntryType::XGlobalHeader, header_name, "jdk/bin");

            let report = extract_tar_gz(&archive, &root, strip, &ExtractionLimits::default())
                .expect("a global header is not an error");

            assert_eq!(report.files, 1, "{header_name}");
            assert_eq!(report.bytes, 2, "{header_name}");
            assert!(!root.join("pax_global_header").exists(), "{header_name}");
            assert!(
                !root.join("jdk/pax_global_header").exists(),
                "{header_name}"
            );
        }
    }

    #[test]
    fn an_archive_of_only_a_pax_global_header_is_empty() {
        let dir = temp();
        let archive = dir.path().join("only-global.tar.gz");
        tar_gz_with_metadata_entry(&archive, EntryType::XGlobalHeader, "pax_global_header", "");

        let error = extract_tar_gz(
            &archive,
            &dir.path().join("install"),
            0,
            &ExtractionLimits::default(),
        )
        .expect_err("no files were extracted");

        assert!(matches!(error, ArchiveError::Empty), "{error:?}");
    }

    #[test]
    fn a_pax_global_header_counts_against_the_entry_limit() {
        let dir = temp();
        let archive = dir.path().join("global.tar.gz");
        tar_gz_with_metadata_entry(&archive, EntryType::XGlobalHeader, "pax_global_header", "f");

        let limits = ExtractionLimits {
            max_entries: 1,
            ..ExtractionLimits::default()
        };
        let error = extract_tar_gz(&archive, &dir.path().join("install"), 0, &limits)
            .expect_err("the second entry exceeds the limit");

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
    fn an_extended_header_the_tar_reader_did_not_apply_is_refused() {
        for kind in [
            EntryType::XHeader,
            EntryType::GNULongName,
            EntryType::GNULongLink,
        ] {
            let dir = temp();
            let archive = dir.path().join("unapplied.tar.gz");
            tar_gz_with_metadata_entry(&archive, kind, "meta", "jdk/bin");

            let error = extract_tar_gz(
                &archive,
                &dir.path().join("install"),
                1,
                &ExtractionLimits::default(),
            )
            .expect_err("an unapplied extended header must be refused");

            assert!(
                matches!(
                    error,
                    ArchiveError::UnsafeEntryType {
                        kind: "unapplied extended header",
                        ..
                    }
                ),
                "{kind:?}: {error:?}"
            );
        }
    }

    /// Incompressible filler, so truncating the compressed stream cuts an entry.
    fn noise(len: usize) -> Vec<u8> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[4]
            })
            .collect()
    }

    #[test]
    fn an_entry_whose_header_understates_its_size_is_refused_and_not_left_behind() {
        let dir = temp();
        let archive = dir.path().join("understated.tar.gz");
        let root = dir.path().join("install");
        tar_gz_with_pax_size(&archive, "jdk/big", 8, &vec![7u8; 64 * 1024]);

        let limits = ExtractionLimits {
            max_total_bytes: 16 * 1024,
            ..ExtractionLimits::default()
        };
        let error =
            extract_tar_gz(&archive, &root, 1, &limits).expect_err("the budget must be enforced");

        assert!(
            matches!(
                error,
                ArchiveError::LimitExceeded {
                    limit: "total-size",
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(!root.join("big").exists(), "a refused file must not remain");
    }

    #[test]
    fn a_file_that_pushes_the_running_total_over_budget_is_not_left_behind() {
        let dir = temp();
        let archive = dir.path().join("two.tar.gz");
        let root = dir.path().join("install");
        tar_gz(
            &archive,
            &[
                ("jdk/first", 0o644, &[1u8; 600]),
                ("jdk/second", 0o644, &[2u8; 600]),
            ],
        );

        let limits = ExtractionLimits {
            max_total_bytes: 1000,
            ..ExtractionLimits::default()
        };
        let error =
            extract_tar_gz(&archive, &root, 1, &limits).expect_err("the total must be enforced");

        assert!(
            matches!(
                error,
                ArchiveError::LimitExceeded {
                    limit: "total-size",
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(
            !root.join("second").exists(),
            "a refused file must not remain"
        );
    }

    #[test]
    fn an_entry_that_exactly_fills_the_remaining_budget_is_extracted() {
        let dir = temp();
        let archive = dir.path().join("exact.tar.gz");
        let root = dir.path().join("install");
        tar_gz(
            &archive,
            &[
                ("jdk/first", 0o644, &[1u8; 600]),
                ("jdk/second", 0o644, &[2u8; 400]),
            ],
        );

        let limits = ExtractionLimits {
            max_total_bytes: 1000,
            ..ExtractionLimits::default()
        };
        let report = extract_tar_gz(&archive, &root, 1, &limits).expect("within budget");

        assert_eq!(report.bytes, 1000);
        assert_eq!(std::fs::read(root.join("second")).expect("read").len(), 400);
    }

    #[test]
    fn a_corrupt_archive_stream_is_reported_against_the_archive_and_leaves_no_partial_file() {
        let dir = temp();
        let archive = dir.path().join("truncated.tar.gz");
        let root = dir.path().join("install");
        tar_gz(&archive, &[("jdk/big", 0o644, &noise(256 * 1024))]);
        let compressed = std::fs::read(&archive).expect("read archive");
        std::fs::write(&archive, &compressed[..compressed.len() * 6 / 10]).expect("truncate");

        let error = extract_tar_gz(&archive, &root, 1, &ExtractionLimits::default())
            .expect_err("a truncated stream must fail");

        match &error {
            ArchiveError::Unreadable { path, .. } => assert_eq!(path, &archive),
            other => panic!("expected Unreadable against the archive, got {other:?}"),
        }
        assert!(
            !root.join("big").exists(),
            "a partially written file must not remain"
        );
    }

    #[test]
    fn an_entry_cut_short_inside_an_intact_gzip_stream_is_refused_and_not_left_behind() {
        let dir = temp();
        let archive = dir.path().join("short.tar.gz");
        let root = dir.path().join("install");
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(8 * 1024);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "jdk/big", &[3u8; 8 * 1024][..])
                .expect("append");
            builder.finish().expect("finish");
        }
        tar_bytes.truncate(512 + 4096);
        let file = File::create(&archive).expect("create archive");
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        encoder.write_all(&tar_bytes).expect("write");
        encoder.finish().expect("gz");

        let error = extract_tar_gz(&archive, &root, 1, &ExtractionLimits::default())
            .expect_err("a short entry must fail");

        match &error {
            ArchiveError::Unreadable { path, .. } => assert_eq!(path, &archive),
            other => panic!("expected Unreadable against the archive, got {other:?}"),
        }
        assert!(!root.join("big").exists(), "a short file must not remain");
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
