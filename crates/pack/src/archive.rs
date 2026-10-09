//! The hardened ZIP reader every importer shares.
//!
//! A pack archive is untrusted input taken straight out of a bug report, so
//! this reader assumes the archive is hostile and is written to be boring:
//!
//! * **Every entry name is validated before it becomes a path.** The rules live
//!   in [`crate::path`] and are shared with `ephervex-java`'s tar reader, so
//!   there is one definition of "safe relative path" in the workspace rather
//!   than one per archive format.
//! * **Launcher metadata is untouchable.** A pack entry named
//!   `.launcher/instance.json` is refused rather than sanitized, even though
//!   overrides are extracted into the *game* directory where it could do no
//!   harm today. The rule belongs to the extractor, not to the caller that
//!   happens to have chosen a safe destination (ADR 0002).
//! * **Symlinks are refused outright.** `ephervex-java` resolves them because a
//!   real JDK tarball is full of them; a modpack override tree is not, and the
//!   safest handling of a feature no legitimate input uses is to refuse it and
//!   say so.
//! * **Limits are enforced against a shared budget.** Entry count, total bytes,
//!   and per-entry bytes are counted across every read and extraction made
//!   through one [`PackArchive`], so a bomb cannot be split across the manifest,
//!   `overrides/`, and `client-overrides/` to stay under three separate caps.
//! * **Declared sizes are never trusted.** Each entry is read through a bounded
//!   reader, because the central directory's size field is attacker-controlled
//!   text like everything else in the file. The bound is the tighter of the
//!   entry's own ceiling and what is left of the shared total, so no entry is
//!   accepted past either.
//! * **Manifests have their own, much smaller ceiling.** A manifest is held in
//!   memory to be parsed, so the disk-sized entry ceiling is the wrong bound.
//!
//! Extraction is synchronous. `zip` is a blocking API, and wrapping it in
//! `spawn_blocking` is the caller's decision because only the caller knows
//! whether it is on a runtime.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tracing::{debug, instrument};
use zip::ZipArchive;

use crate::detect::{PackFormat, identify};
use crate::error::{PackError, PackResult};
use crate::path::{DestinationLinks, contained_join, is_reserved, safe_relative_path};

/// Bounds on what an archive may expand into.
///
/// "For local developer use, limits can be generous, but they must exist." The
/// defaults are sized for a large real-world modpack - a few thousand files and
/// a couple of gigabytes once resource packs are counted - and are configurable
/// so a caller with a bigger archive can raise them deliberately rather than
/// discovering there was never a ceiling at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    /// Guards against one entry claiming the whole budget, which is what a bomb
    /// built from a single stream looks like.
    pub max_entry_bytes: u64,
    /// The largest manifest read into memory. A large real-world index is a
    /// few megabytes.
    pub max_manifest_bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_entry_bytes: 2 * 1024 * 1024 * 1024,
            max_manifest_bytes: 64 * 1024 * 1024,
        }
    }
}

/// How many more bytes one entry may produce, and which limit says so.
#[derive(Debug, Clone, Copy)]
struct Allowance {
    limit: &'static str,
    allowed: u64,
    /// Already counted against `allowed` before this entry.
    spent: u64,
}

impl Allowance {
    const fn bytes(self) -> u64 {
        self.allowed.saturating_sub(self.spent)
    }

    /// Refuses `produced` bytes when they overrun the allowance.
    const fn check(self, produced: u64) -> PackResult<()> {
        if produced > self.bytes() {
            return Err(PackError::LimitExceeded {
                limit: self.limit,
                allowed: self.allowed,
                reached: self.spent.saturating_add(produced),
            });
        }
        Ok(())
    }
}

/// A ZIP central directory file header: the signature that starts it, and the
/// length of its fixed part, which `zip` reads in one call per entry.
const CENTRAL_HEADER_SIGNATURE: [u8; 4] = *b"PK\x01\x02";
const CENTRAL_HEADER_FIXED_LEN: usize = 46;

/// The ZIP64 end-of-central-directory record: its signature, the length of the
/// fixed part `zip` reads in one call, and where the total entry count sits in
/// it (a little-endian `u64`).
const END_RECORD64_SIGNATURE: [u8; 4] = *b"PK\x06\x06";
const END_RECORD64_FIXED_LEN: usize = 56;
const END_RECORD64_ENTRY_COUNT: std::ops::Range<usize> = 32..40;

/// Counts central directory headers as `zip` parses them and refuses further
/// reads once the entry limit is crossed.
///
/// `zip` allocates a record for every header it parses, and when the first
/// end-of-central-directory candidate fails it retries earlier ones, so a
/// count checked after `ZipArchive::new` comes too late. The budget is shared
/// across every candidate and is disarmed once the archive is open, so entry
/// contents are never counted. Only a read of exactly the fixed header length
/// counts: `zip` also scans for the first header with larger buffered reads.
///
/// A ZIP64 end record is checked the same way, before any header is parsed:
/// `zip` reserves room for the entry count it declares, and only the archive's
/// own size bounds that number. A ZIP32 count is a `u16`, so the room it
/// reserves is already small.
#[derive(Debug)]
struct HeaderBudget {
    allowed: u64,
    /// Headers parsed so far, or the entry count a ZIP64 end record declared
    /// when that is what crossed the limit.
    seen: AtomicU64,
    armed: AtomicBool,
    exceeded: AtomicBool,
}

impl HeaderBudget {
    fn new(allowed: u64) -> Arc<Self> {
        Arc::new(Self {
            allowed,
            seen: AtomicU64::new(0),
            armed: AtomicBool::new(true),
            exceeded: AtomicBool::new(false),
        })
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::Relaxed);
    }

    fn exceeded(&self) -> Option<u64> {
        self.exceeded
            .load(Ordering::Relaxed)
            .then(|| self.seen.load(Ordering::Relaxed))
    }

    fn refusal() -> io::Error {
        io::Error::other("the archive declares more entries than the entry-count limit allows")
    }

    /// Looks at one read `zip` made, where `requested` is the buffer it offered.
    fn observe(&self, read: &[u8], requested: usize) {
        if !self.armed.load(Ordering::Relaxed) || read.len() != requested {
            return;
        }
        if requested == CENTRAL_HEADER_FIXED_LEN && read.starts_with(&CENTRAL_HEADER_SIGNATURE) {
            if self.seen.fetch_add(1, Ordering::Relaxed) >= self.allowed {
                self.exceeded.store(true, Ordering::Relaxed);
            }
        } else if requested == END_RECORD64_FIXED_LEN && read.starts_with(&END_RECORD64_SIGNATURE) {
            let declared = read
                .get(END_RECORD64_ENTRY_COUNT)
                .and_then(|field| <[u8; 8]>::try_from(field).ok())
                .map_or(u64::MAX, u64::from_le_bytes);
            if declared > self.allowed {
                self.seen.store(declared, Ordering::Relaxed);
                self.exceeded.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// The archive file as `zip` sees it, with [`HeaderBudget`] applied.
#[derive(Debug)]
struct BudgetedFile {
    file: File,
    budget: Arc<HeaderBudget>,
}

impl Read for BudgetedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.budget.exceeded.load(Ordering::Relaxed) {
            return Err(HeaderBudget::refusal());
        }
        let read = self.file.read(buf)?;
        self.budget.observe(&buf[..read], buf.len());
        if self.budget.exceeded.load(Ordering::Relaxed) {
            return Err(HeaderBudget::refusal());
        }
        Ok(read)
    }
}

impl Seek for BudgetedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if self.budget.exceeded.load(Ordering::Relaxed) {
            return Err(HeaderBudget::refusal());
        }
        self.file.seek(position)
    }
}

/// What one extraction produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractionReport {
    pub files: u64,
    pub directories: u64,
    /// Entries the caller's own policy declined. Always zero for an extraction
    /// that asked no questions.
    pub skipped: u64,
    pub bytes: u64,
}

/// What an extraction should do with one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryDecision {
    Extract,
    /// Leave the entry out and count it. For a destination the launcher owns,
    /// where the archive's copy is simply not wanted.
    Skip,
    /// Refuse the whole extraction. For an entry that should never have been
    /// offered at all.
    Refuse,
}

impl ExtractionReport {
    /// Whether anything was written at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.files == 0 && self.directories == 0
    }
}

/// An opened pack archive.
///
/// Holds the file open for the life of the import: the manifest is read first,
/// the override layers are extracted afterwards, and re-opening between the two
/// would let the file be swapped underneath the launcher.
#[derive(Debug)]
pub struct PackArchive {
    path: PathBuf,
    zip: ZipArchive<BudgetedFile>,
    limits: ExtractionLimits,
    /// Bytes and entries consumed so far, across every read of this archive.
    spent_bytes: u64,
    spent_entries: u64,
}

impl PackArchive {
    /// Opens an archive and reads its central directory.
    pub fn open(path: &Path, limits: ExtractionLimits) -> PackResult<Self> {
        let file = File::open(path).map_err(|source| PackError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        let budget = HeaderBudget::new(limits.max_entries);
        let opened = ZipArchive::new(BudgetedFile {
            file,
            budget: Arc::clone(&budget),
        });
        budget.disarm();
        if let Some(reached) = budget.exceeded() {
            return Err(PackError::LimitExceeded {
                limit: "entry-count",
                allowed: limits.max_entries,
                reached,
            });
        }
        let zip = opened.map_err(|source| PackError::ArchiveInvalid {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(Self {
            path: path.to_path_buf(),
            zip,
            limits,
            spent_bytes: 0,
            spent_entries: 0,
        })
    }

    /// The archive's own path on disk.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The archive's file name, for display and for recording where an instance
    /// came from. Never used to build a path.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.path.file_name().and_then(|name| name.to_str())
    }

    /// Which pack format this archive is, if any (format detection).
    #[must_use]
    pub fn format(&self) -> Option<PackFormat> {
        identify(self.zip.file_names())
    }

    /// Whether the archive contains an entry with this exact name.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.zip.index_for_name(name).is_some()
    }

    /// Whether the archive contains anything beneath a directory prefix.
    ///
    /// Directory entries are optional in a ZIP - plenty of writers emit only
    /// file entries - so this asks about *contents* rather than about the
    /// directory entry itself.
    #[must_use]
    pub fn contains_prefix(&self, prefix: &str) -> bool {
        let prefix = normalize_prefix(prefix);
        self.zip
            .file_names()
            .any(|name| name.replace('\\', "/").starts_with(&prefix))
    }

    /// The one top-level directory every entry lives under, if there is one.
    ///
    /// Archives published for people to unzip themselves are usually wrapped in
    /// a single folder so that unpacking them in a downloads directory does not
    /// scatter files everywhere. Stripping that wrapper is destructive when it
    /// is guessed wrong -- everything lands one level from where it belongs --
    /// so this answers `None` unless the shape is unambiguous.
    ///
    /// Two conditions. Nothing may sit at the archive's root, and there may be
    /// only one top-level directory: either would mean the directory is content
    /// rather than a wrapper. And that directory must itself contain a
    /// directory, because an archive holding one folder of loose files is
    /// exactly as consistent with being a content folder as with being a
    /// wrapper, and there is no evidence in the archive to tell them apart.
    ///
    /// Computed from the central directory, before anything is written.
    #[must_use]
    pub fn single_root(&self) -> Option<String> {
        let mut root: Option<String> = None;
        let mut nested_directory = false;
        for name in self.zip.file_names() {
            let name = name.replace('\\', "/");
            let trimmed = name.trim_start_matches("./");
            let (first, rest) = trimmed.split_once('/')?;
            if first.is_empty() || first == "." || first == ".." {
                return None;
            }
            nested_directory |= rest.contains('/');
            match &root {
                Some(existing) if existing != first => return None,
                Some(_) => {}
                None => root = Some(first.to_owned()),
            }
        }
        nested_directory.then_some(root).flatten()
    }

    /// Reads a manifest into memory, bounded by the manifest ceiling.
    ///
    /// Manifests are the only thing read this way; everything else is streamed
    /// to disk.
    pub fn read_manifest(&mut self, name: &'static str) -> PackResult<Vec<u8>> {
        let index = self
            .zip
            .index_for_name(name)
            .ok_or(PackError::ManifestMissing { entry: name })?;

        let allowance = self.allowance(&[
            ("manifest-size", self.limits.max_manifest_bytes),
            ("entry-size", self.limits.max_entry_bytes),
        ]);
        self.charge_entry()?;

        let path = self.path.clone();
        let mut entry = self
            .zip
            .by_index(index)
            .map_err(|source| PackError::ArchiveInvalid {
                path: path.clone(),
                source,
            })?;
        // The declared size can lie, so the read below is bounded as well.
        allowance.check(entry.size())?;

        let ceiling = usize::try_from(allowance.bytes().saturating_add(1)).unwrap_or(usize::MAX);
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        let read = stream_entry(&mut entry, allowance, &path, &mut |chunk| {
            // Doubling growth, capped so an understated size cannot double the
            // ceiling either.
            let needed = bytes.len().saturating_add(chunk.len());
            if needed > bytes.capacity() {
                let grown = needed.max(bytes.len().saturating_mul(2)).min(ceiling);
                bytes.reserve_exact(grown.max(needed) - bytes.len());
            }
            bytes.extend_from_slice(chunk);
            Ok(())
        })?;
        drop(entry);
        self.spend_bytes(read);
        Ok(bytes)
    }

    /// Extracts everything beneath `prefix` into `destination`.
    ///
    /// The prefix is stripped, so `overrides/config/a.toml` lands at
    /// `<destination>/config/a.toml`. Extracting a prefix that does not exist is
    /// not an error - a pack with no `server-overrides/` is perfectly normal -
    /// and returns an empty report.
    ///
    /// Later layers overwrite earlier ones, which is why entries are visited in
    /// "later layer wins" asks for; the caller controls the order.
    #[instrument(skip(self), fields(archive = %self.path.display(), destination = %destination.display()))]
    pub fn extract_prefix(
        &mut self,
        prefix: &str,
        destination: &Path,
    ) -> PackResult<ExtractionReport> {
        self.extract_prefix_filtered(prefix, destination, &mut |_| EntryDecision::Extract)
    }

    /// The same extraction, with the caller deciding entry by entry.
    ///
    /// `decide` is asked about each entry's path relative to `destination`,
    /// before anything is written. It exists so a policy about which
    /// destinations belong to the archive can live with the caller that owns
    /// that destination, rather than in a reader that is shared by every
    /// importer.
    #[instrument(skip(self, decide), fields(archive = %self.path.display(), destination = %destination.display()))]
    pub fn extract_prefix_filtered(
        &mut self,
        prefix: &str,
        destination: &Path,
        decide: &mut dyn FnMut(&Path) -> EntryDecision,
    ) -> PackResult<ExtractionReport> {
        let prefix = normalize_prefix(prefix);
        if !self.contains_prefix(&prefix) {
            return Ok(ExtractionReport::default());
        }

        std::fs::create_dir_all(destination).map_err(|source| PackError::Unwritable {
            path: destination.to_path_buf(),
            source,
        })?;
        // The root has to exist and be canonical before any entry resolves
        // against it, or containment is measured against a path that could
        // itself contain a symlink.
        let root = std::fs::canonicalize(destination).map_err(|source| PackError::Unwritable {
            path: destination.to_path_buf(),
            source,
        })?;

        let archive_path = self.path.clone();
        let mut report = ExtractionReport::default();
        let mut links = DestinationLinks::new(&root);
        for index in 0..self.zip.len() {
            let (name, relative, is_dir, declared) = {
                let entry =
                    self.zip
                        .by_index(index)
                        .map_err(|source| PackError::ArchiveInvalid {
                            path: archive_path.clone(),
                            source,
                        })?;

                let name = String::from_utf8_lossy(entry.name_raw()).into_owned();
                let Some(relative) = entry_under(entry.name_raw(), &prefix)? else {
                    continue;
                };
                if entry.is_symlink() {
                    // A modpack override tree has no legitimate symlink, and
                    // the safest handling of a feature nothing needs is to
                    // refuse it rather than to resolve it.
                    return Err(PackError::EntryTypeUnsupported {
                        entry: name,
                        kind: "symlink",
                    });
                }
                (name, relative, entry.is_dir(), entry.size())
            };

            match decide(&relative) {
                EntryDecision::Extract => {}
                EntryDecision::Skip => {
                    report.skipped += 1;
                    continue;
                }
                EntryDecision::Refuse => {
                    return Err(PackError::EntryPathUnsafe { entry: name });
                }
            }

            let Some(target) = contained_join(&root, &relative) else {
                return Err(PackError::EntryPathUnsafe { entry: name });
            };
            // Containment above is lexical. A destination that is already a
            // link would carry the write somewhere else.
            if links.walks_a_link(&relative) {
                return Err(PackError::EntryPathUnsafe { entry: name });
            }

            if is_dir {
                std::fs::create_dir_all(&target).map_err(|source| PackError::Unwritable {
                    path: target.clone(),
                    source,
                })?;
                report.directories += 1;
                continue;
            }

            let allowance = self.allowance(&[("entry-size", self.limits.max_entry_bytes)]);
            self.charge_entry()?;
            allowance.check(declared)?;
            let written = self.write_entry(index, &target, allowance)?;
            self.spend_bytes(written);
            report.files += 1;
            report.bytes = report.bytes.saturating_add(written);
        }

        debug!(
            prefix = %prefix,
            files = report.files,
            directories = report.directories,
            skipped = report.skipped,
            bytes = report.bytes,
            "extracted a pack override layer"
        );
        Ok(report)
    }

    /// Streams one entry to disk through a reader bounded by `allowance`.
    fn write_entry(
        &mut self,
        index: usize,
        target: &Path,
        allowance: Allowance,
    ) -> PackResult<u64> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PackError::Unwritable {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let archive_path = self.path.clone();
        let mut entry = self
            .zip
            .by_index(index)
            .map_err(|source| PackError::ArchiveInvalid {
                path: archive_path.clone(),
                source,
            })?;

        let mut file = File::create(target).map_err(|source| PackError::Unwritable {
            path: target.to_path_buf(),
            source,
        })?;
        let copied = stream_entry(&mut entry, allowance, &archive_path, &mut |chunk| {
            file.write_all(chunk)
                .map_err(|source| PackError::Unwritable {
                    path: target.to_path_buf(),
                    source,
                })
        });
        if copied.is_err() {
            // A refused or broken entry is removed rather than left looking
            // complete.
            drop(file);
            let _ = std::fs::remove_file(target);
        }

        // Permission bits are deliberately not carried over from the archive.
        // Nothing in a modpack override needs to be executable, and honoring a
        // mode from untrusted input is how an archive gets to choose what runs
        copied
    }

    /// The tightest of an entry's own ceilings and what is left of the shared
    /// total. Ties go to the earliest ceiling, and any ceiling beats the total.
    fn allowance(&self, ceilings: &[(&'static str, u64)]) -> Allowance {
        let total = Allowance {
            limit: "total-size",
            allowed: self.limits.max_total_bytes,
            spent: self.spent_bytes,
        };
        ceilings
            .iter()
            .rev()
            .map(|&(limit, allowed)| Allowance {
                limit,
                allowed,
                spent: 0,
            })
            .fold(total, |tightest, ceiling| {
                if ceiling.bytes() <= tightest.bytes() {
                    ceiling
                } else {
                    tightest
                }
            })
    }

    /// Counts one more entry against the shared budget.
    fn charge_entry(&mut self) -> PackResult<()> {
        self.spent_entries = self.spent_entries.saturating_add(1);
        if self.spent_entries > self.limits.max_entries {
            return Err(PackError::LimitExceeded {
                limit: "entry-count",
                allowed: self.limits.max_entries,
                reached: self.spent_entries,
            });
        }
        Ok(())
    }

    /// Records bytes an entry produced within its allowance.
    const fn spend_bytes(&mut self, bytes: u64) {
        self.spent_bytes = self.spent_bytes.saturating_add(bytes);
    }
}

/// Streams an entry into `sink`, stopping one byte past the allowance so an
/// entry that fills it exactly is distinguishable from one that overruns it.
///
/// A failed read is the archive's fault, typically a corrupt entry failing its
/// checksum, so it is reported against the archive rather than the destination.
fn stream_entry(
    entry: &mut impl Read,
    allowance: Allowance,
    archive: &Path,
    sink: &mut dyn FnMut(&[u8]) -> PackResult<()>,
) -> PackResult<u64> {
    let mut reader = entry.take(allowance.bytes().saturating_add(1));
    let mut buffer = vec![0; 64 * 1024];
    let mut produced = 0_u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(PackError::ArchiveInvalid {
                    path: archive.to_path_buf(),
                    source: zip::result::ZipError::Io(source),
                });
            }
        };
        sink(&buffer[..read])?;
        produced = produced.saturating_add(read as u64);
    }
    allowance.check(produced)?;
    Ok(produced)
}

/// Validates one entry name and returns its path relative to `prefix`, or
/// `None` when the entry is not under the prefix.
///
/// A free function rather than a method so it can run while an entry borrowed
/// from the archive is still alive - and because it is pure: an entry name and
/// a prefix in, a decision out.
fn entry_under(raw: &[u8], prefix: &str) -> PackResult<Option<PathBuf>> {
    let name = String::from_utf8_lossy(raw).into_owned();
    let Some(full) = safe_relative_path(raw, 0) else {
        return Err(PackError::EntryPathUnsafe { entry: name });
    };
    if is_reserved(&full) {
        return Err(PackError::EntryReserved { entry: name });
    }

    let prefix_path = Path::new(prefix.trim_end_matches('/'));
    let Ok(relative) = full.strip_prefix(prefix_path) else {
        return Ok(None);
    };
    if relative.as_os_str().is_empty() {
        // The prefix directory entry itself.
        return Ok(None);
    }
    // Checked again after stripping: `overrides/.launcher/x` is a pack trying
    // to write launcher metadata into whatever the layer's destination is.
    if is_reserved(relative) {
        return Err(PackError::EntryReserved { entry: name });
    }
    Ok(Some(relative.to_path_buf()))
}

/// A directory prefix with exactly one trailing slash and no leading `./`.
///
/// The archive root is the empty string rather than `"/"`: every entry name is
/// relative, so a root prefix has to be a prefix every name already starts
/// with.
fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_owned();
    if trimmed.is_empty() {
        return trimmed;
    }
    format!("{trimmed}/")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// Writes a ZIP from `(name, contents)` pairs, using the raw name so a
    /// hostile entry can be built at all.
    fn zip_file(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(body).expect("write entry");
        }
        writer.finish().expect("finish");
    }

    /// Writes `stored` entries followed by one DEFLATE entry whose headers
    /// declare `declared` as its uncompressed size, whatever it inflates to.
    fn zip_with_understated_entry(
        path: &Path,
        stored: &[(&str, &[u8])],
        name: &str,
        body: &[u8],
        declared: u32,
    ) {
        let file = File::create(path).expect("create archive");
        let mut writer = zip::ZipWriter::new(file);
        let plain: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (stored_name, stored_body) in stored {
            writer.start_file(*stored_name, plain).expect("start entry");
            writer.write_all(stored_body).expect("write entry");
        }
        let deflated = plain.compression_method(zip::CompressionMethod::Deflated);
        writer.start_file(name, deflated).expect("start entry");
        writer.write_all(body).expect("write entry");
        writer.finish().expect("finish");

        let mut bytes = std::fs::read(path).expect("read archive");
        let last = |signature: &[u8]| {
            bytes
                .windows(4)
                .rposition(|window| window == signature)
                .expect("entry header")
        };
        let (local, central) = (last(b"PK\x03\x04"), last(b"PK\x01\x02"));
        let actual = u32::try_from(body.len()).expect("small body").to_le_bytes();
        // Little-endian size fields, checked so a misplaced patch fails loudly.
        for offset in [local + 22, central + 24] {
            assert_eq!(bytes[offset..offset + 4], actual, "uncompressed size field");
            bytes[offset..offset + 4].copy_from_slice(&declared.to_le_bytes());
        }
        std::fs::write(path, bytes).expect("rewrite archive");
    }

    fn open(path: &Path) -> PackArchive {
        PackArchive::open(path, ExtractionLimits::default()).expect("open")
    }

    #[test]
    fn a_manifest_over_its_ceiling_is_refused_on_its_declared_size() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        let body = vec![b' '; 64 * 1024];
        zip_with_understated_entry(&archive, &[], "modrinth.index.json", &body, 64 * 1024);

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_manifest_bytes: 1024,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        let error = pack
            .read_manifest("modrinth.index.json")
            .expect_err("a manifest over its ceiling is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "manifest-size",
                    allowed: 1024,
                    reached: 65536,
                }
            ),
            "refused on the declared size before inflating: {error:?}"
        );
    }

    #[test]
    fn a_manifest_understating_its_size_stops_inflating_at_the_ceiling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        let body = vec![b' '; 64 * 1024];
        zip_with_understated_entry(&archive, &[], "modrinth.index.json", &body, 1);

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_manifest_bytes: 1024,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        let error = pack
            .read_manifest("modrinth.index.json")
            .expect_err("a manifest over its ceiling is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "manifest-size",
                    reached: 1025,
                    ..
                }
            ),
            "reading stops one byte past the ceiling: {error:?}"
        );
    }

    #[test]
    fn a_manifest_read_is_bounded_by_the_remaining_total() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        zip_with_understated_entry(
            &archive,
            &[("overrides/a.bin", &[b'x'; 40])],
            "modrinth.index.json",
            &[b' '; 80],
            1,
        );

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_total_bytes: 100,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        pack.extract_prefix("overrides", &dir.path().join("out"))
            .expect("the layer fits");
        let error = pack
            .read_manifest("modrinth.index.json")
            .expect_err("a manifest over what is left of the total is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "total-size",
                    allowed: 100,
                    reached: 101,
                }
            ),
            "reading stops one byte past the remaining total: {error:?}"
        );
    }

    #[test]
    fn a_manifest_at_both_ceilings_is_refused_under_the_manifest_ceiling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        zip_with_understated_entry(&archive, &[], "modrinth.index.json", &[b' '; 4096], 4096);

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_manifest_bytes: 1024,
                max_entry_bytes: 1024,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        let error = pack
            .read_manifest("modrinth.index.json")
            .expect_err("a manifest over its ceiling is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "manifest-size",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn entries_that_fill_their_allowance_exactly_are_accepted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                ("modrinth.index.json", &[b' '; 64]),
                ("overrides/a.bin", &[b'x'; 36]),
            ],
        );

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_total_bytes: 100,
                max_manifest_bytes: 64,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        assert_eq!(
            pack.read_manifest("modrinth.index.json")
                .expect("a manifest at its ceiling is read")
                .len(),
            64
        );
        let report = pack
            .extract_prefix("overrides", &dir.path().join("out"))
            .expect("an entry filling the remaining total is extracted");
        assert_eq!(report.bytes, 36);
    }

    #[test]
    fn a_corrupt_entry_is_reported_against_the_archive_and_leaves_no_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("broken.mrpack");
        let body = b"0123456789abcdef".repeat(64);
        zip_file(&archive, &[("overrides/a.bin", &body)]);

        let mut bytes = std::fs::read(&archive).expect("read archive");
        let at = bytes
            .windows(body.len())
            .position(|window| window == body.as_slice())
            .expect("stored body");
        bytes[at] ^= 0xff;
        std::fs::write(&archive, bytes).expect("rewrite archive");

        let destination = dir.path().join("out");
        let error = open(&archive)
            .extract_prefix("overrides", &destination)
            .expect_err("a checksum mismatch is refused");
        assert!(
            matches!(&error, PackError::ArchiveInvalid { path, .. } if *path == archive),
            "unexpected error: {error:?}"
        );
        assert!(
            !destination.join("a.bin").exists(),
            "the broken entry is removed rather than left behind"
        );
    }

    #[test]
    fn an_entry_understating_its_size_stops_at_the_remaining_total_and_leaves_no_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        zip_with_understated_entry(
            &archive,
            &[("modrinth.index.json", &[b' '; 40])],
            "overrides/big.bin",
            &[b'x'; 80],
            1,
        );

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_total_bytes: 100,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        pack.read_manifest("modrinth.index.json")
            .expect("the manifest fits");
        let destination = dir.path().join("out");
        let error = pack
            .extract_prefix("overrides", &destination)
            .expect_err("an entry over what is left of the total is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "total-size",
                    allowed: 100,
                    reached: 101,
                }
            ),
            "writing stops one byte past the remaining total: {error:?}"
        );
        assert!(
            !destination.join("big.bin").exists(),
            "the partially written entry is removed rather than left behind"
        );
    }

    #[test]
    fn an_entry_ceiling_tied_with_the_remaining_total_is_the_limit_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        zip_with_understated_entry(&archive, &[], "overrides/big.bin", &[b'x'; 4096], 1);

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_total_bytes: 100,
                max_entry_bytes: 100,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");
        let error = pack
            .extract_prefix("overrides", &dir.path().join("out"))
            .expect_err("an oversize entry is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "entry-size",
                    allowed: 100,
                    reached: 101,
                }
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn an_override_layer_is_extracted_with_its_prefix_stripped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                ("modrinth.index.json", b"{}"),
                ("overrides/config/a.toml", b"a=1"),
                ("overrides/mods/local.jar", b"jar"),
            ],
        );

        let destination = dir.path().join("minecraft");
        let report = open(&archive)
            .extract_prefix("overrides", &destination)
            .expect("extract");

        assert_eq!(report.files, 2);
        assert_eq!(
            std::fs::read_to_string(destination.join("config/a.toml")).expect("read"),
            "a=1"
        );
        assert!(destination.join("mods/local.jar").is_file());
        assert!(!destination.join("overrides").exists());
    }

    #[test]
    fn a_missing_layer_is_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(&archive, &[("modrinth.index.json", b"{}")]);

        let report = open(&archive)
            .extract_prefix("server-overrides", &dir.path().join("out"))
            .expect("extract");
        assert!(report.is_empty(), "a pack without a layer is normal");
    }

    #[test]
    fn later_layers_overwrite_earlier_ones() {
        // overrides, then client-overrides, and the later layer wins.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(
            &archive,
            &[
                ("overrides/config/a.toml", b"common"),
                ("client-overrides/config/a.toml", b"client"),
            ],
        );

        let destination = dir.path().join("minecraft");
        let mut pack = open(&archive);
        pack.extract_prefix("overrides", &destination)
            .expect("common");
        pack.extract_prefix("client-overrides", &destination)
            .expect("client");

        assert_eq!(
            std::fs::read_to_string(destination.join("config/a.toml")).expect("read"),
            "client"
        );
    }

    #[test]
    fn a_traversing_entry_is_refused_rather_than_sanitized() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("evil.mrpack");
        zip_file(
            &archive,
            &[("overrides/../../../../etc/ephervex-owned", b"pwned")],
        );

        let error = open(&archive)
            .extract_prefix("overrides", &dir.path().join("out"))
            .expect_err("must refuse");
        assert!(matches!(error, PackError::EntryPathUnsafe { .. }));
        assert_eq!(error.message_id(), "error-pack-entry-unsafe");
    }

    #[test]
    fn launcher_metadata_can_never_be_written_by_a_pack() {
        // ADR 0002: pack contents must not be able to overwrite `.launcher/`.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("evil.mrpack");
        zip_file(
            &archive,
            &[("overrides/.launcher/instance.json", b"{\"id\":\"stolen\"}")],
        );

        let error = open(&archive)
            .extract_prefix("overrides", &dir.path().join("out"))
            .expect_err("must refuse");
        assert!(matches!(error, PackError::EntryReserved { .. }));
    }

    #[test]
    fn a_top_level_launcher_entry_is_refused_even_outside_the_extracted_layer() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("evil.mrpack");
        zip_file(
            &archive,
            &[
                (".launcher/instance.json", b"{}"),
                ("overrides/config/a.toml", b"a"),
            ],
        );

        let error = open(&archive)
            .extract_prefix("overrides", &dir.path().join("out"))
            .expect_err("must refuse");
        assert!(matches!(error, PackError::EntryReserved { .. }));
    }

    #[test]
    fn the_entry_count_limit_is_enforced_when_the_archive_is_opened() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        let entries: Vec<(String, Vec<u8>)> = (0..8)
            .map(|index| (format!("overrides/{index}.txt"), b"x".to_vec()))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_slice()))
            .collect();
        zip_file(&archive, &borrowed);

        let error = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 4,
                ..ExtractionLimits::default()
            },
        )
        .expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::LimitExceeded {
                limit: "entry-count",
                ..
            }
        ));
    }

    /// How many central directory headers `zip` parses before the budget stops it.
    fn headers_parsed_before_refusal(path: &Path, max_entries: u64) -> (bool, u64) {
        let budget = HeaderBudget::new(max_entries);
        let file = File::open(path).expect("open archive");
        let opened = ZipArchive::new(BudgetedFile {
            file,
            budget: Arc::clone(&budget),
        });
        (opened.is_ok(), budget.seen.load(Ordering::Relaxed))
    }

    fn numbered_entries(count: usize) -> Vec<(String, Vec<u8>)> {
        (0..count)
            .map(|index| (format!("overrides/{index}.txt"), b"x".to_vec()))
            .collect()
    }

    fn write_numbered_zip(path: &Path, count: usize) {
        let entries = numbered_entries(count);
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_slice()))
            .collect();
        zip_file(path, &borrowed);
    }

    #[test]
    fn central_directory_parsing_stops_once_the_entry_limit_is_crossed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("many.mrpack");
        write_numbered_zip(&archive, 500);

        let (opened, parsed) = headers_parsed_before_refusal(&archive, 2);
        assert!(!opened, "an over-limit directory must not open");
        assert_eq!(parsed, 3, "parsing must stop at the first excess header");

        let error = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 2,
                ..ExtractionLimits::default()
            },
        )
        .expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::LimitExceeded {
                limit: "entry-count",
                allowed: 2,
                reached: 3,
            }
        ));
    }

    #[test]
    fn an_earlier_end_record_cannot_be_used_to_parse_past_the_entry_limit() {
        // A trailing end record whose directory parses a few headers and then
        // breaks makes `zip` fall back to the previous end record, which
        // declares far more entries. Both candidates draw on one budget.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("fallback.mrpack");
        write_numbered_zip(&archive, 200);
        let mut bytes = std::fs::read(&archive).expect("read archive");

        let u16_at =
            |bytes: &[u8], at: usize| usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
        let end = bytes
            .windows(4)
            .rposition(|window| window == b"PK\x05\x06")
            .expect("end record");
        let mut cursor = usize::try_from(u32::from_le_bytes(
            bytes[end + 16..end + 20].try_into().expect("offset field"),
        ))
        .expect("offset");
        let mut copied = Vec::new();
        for _ in 0..5 {
            let length = 46
                + u16_at(&bytes, cursor + 28)
                + u16_at(&bytes, cursor + 30)
                + u16_at(&bytes, cursor + 32);
            copied.extend_from_slice(&bytes[cursor..cursor + length]);
            cursor += length;
        }
        // A sixth header whose name length runs past the end of the file.
        let mut broken = bytes[cursor..cursor + 46].to_vec();
        broken[28..30].copy_from_slice(&u16::MAX.to_le_bytes());
        copied.extend_from_slice(&broken);

        let directory_offset = u32::try_from(bytes.len()).expect("small archive");
        let directory_size = u32::try_from(copied.len()).expect("small directory");
        bytes.extend_from_slice(&copied);
        bytes.extend_from_slice(b"PK\x05\x06");
        bytes.extend_from_slice(&[0; 4]); // disk numbers
        bytes.extend_from_slice(&6u16.to_le_bytes()); // entries on this disk
        bytes.extend_from_slice(&6u16.to_le_bytes()); // entries in total
        bytes.extend_from_slice(&directory_size.to_le_bytes());
        bytes.extend_from_slice(&directory_offset.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes()); // comment length
        std::fs::write(&archive, bytes).expect("rewrite archive");

        // Five headers from the trailing record, then six from the earlier one.
        let (_, parsed) = headers_parsed_before_refusal(&archive, 10);
        assert_eq!(parsed, 11, "fallback candidates share one entry budget");
        let error = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 10,
                ..ExtractionLimits::default()
            },
        )
        .expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::LimitExceeded {
                limit: "entry-count",
                ..
            }
        ));
    }

    #[test]
    fn an_archive_with_exactly_the_entry_limit_opens() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("exact.mrpack");
        write_numbered_zip(&archive, 6);

        let pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 6,
                ..ExtractionLimits::default()
            },
        )
        .expect("the limit itself is allowed");
        assert!(pack.contains("overrides/5.txt"));
    }

    /// Rewrites a ZIP32 archive so a ZIP64 end record declares `declared`
    /// entries, over the same directory that really holds `real` of them.
    fn declare_zip64_entry_count(path: &Path, real: u64, declared: u64) {
        let mut bytes = std::fs::read(path).expect("read archive");
        let end = bytes
            .windows(4)
            .rposition(|window| window == b"PK\x05\x06")
            .expect("end record");
        let directory_size = u64::from(u32::from_le_bytes(
            bytes[end + 12..end + 16].try_into().expect("size field"),
        ));
        let directory_offset = u64::from(u32::from_le_bytes(
            bytes[end + 16..end + 20].try_into().expect("offset field"),
        ));
        bytes.truncate(end);

        let record_offset = bytes.len() as u64;
        bytes.extend_from_slice(b"PK\x06\x06");
        bytes.extend_from_slice(&44u64.to_le_bytes()); // record size
        bytes.extend_from_slice(&45u16.to_le_bytes()); // made by
        bytes.extend_from_slice(&45u16.to_le_bytes()); // needed to extract
        bytes.extend_from_slice(&[0; 8]); // disk numbers
        bytes.extend_from_slice(&real.min(declared).to_le_bytes()); // on this disk
        bytes.extend_from_slice(&declared.to_le_bytes()); // in total
        bytes.extend_from_slice(&directory_size.to_le_bytes());
        bytes.extend_from_slice(&directory_offset.to_le_bytes());
        bytes.extend_from_slice(b"PK\x06\x07");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // disk with the record
        bytes.extend_from_slice(&record_offset.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // disks
        bytes.extend_from_slice(b"PK\x05\x06");
        bytes.extend_from_slice(&[0; 4]); // disk numbers
        bytes.extend_from_slice(&u16::MAX.to_le_bytes());
        bytes.extend_from_slice(&u16::MAX.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes()); // comment length
        std::fs::write(path, bytes).expect("rewrite archive");
    }

    #[test]
    fn a_zip64_entry_count_over_the_limit_is_refused_before_any_header_is_parsed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("declared.mrpack");
        write_numbered_zip(&archive, 3);
        declare_zip64_entry_count(&archive, 3, 1 << 40);

        let (opened, declared) = headers_parsed_before_refusal(&archive, 10);
        assert!(!opened, "an over-limit count must not open");
        assert_eq!(declared, 1 << 40, "the declared count is what was refused");

        let error = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 10,
                ..ExtractionLimits::default()
            },
        )
        .expect_err("must refuse");
        assert!(matches!(
            error,
            PackError::LimitExceeded {
                limit: "entry-count",
                allowed: 10,
                reached,
            } if reached == 1 << 40
        ));
    }

    #[test]
    fn a_zip64_entry_count_at_the_limit_opens() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("zip64.mrpack");
        write_numbered_zip(&archive, 3);
        declare_zip64_entry_count(&archive, 3, 3);

        let pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 3,
                ..ExtractionLimits::default()
            },
        )
        .expect("the limit itself is allowed");
        assert!(pack.contains("overrides/2.txt"));
    }

    #[test]
    fn entry_contents_that_look_like_directory_headers_are_not_counted() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("lookalike.mrpack");
        let body = b"PK\x01\x02".repeat(64);
        zip_file(&archive, &[("manifest.json", &body)]);

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entries: 1,
                ..ExtractionLimits::default()
            },
        )
        .expect("one entry fits");
        assert_eq!(pack.read_manifest("manifest.json").expect("read"), body);
    }

    #[test]
    fn one_oversize_entry_is_refused_and_leaves_no_partial_file() {
        // The per-entry ceiling, distinct from the shared total: a single
        // entry that inflates past it must not be written even in part.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        zip_file(&archive, &[("overrides/huge.bin", &vec![b'x'; 8192])]);

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_entry_bytes: 1024,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");

        let destination = dir.path().join("out");
        let error = pack
            .extract_prefix("overrides", &destination)
            .expect_err("an entry over its own ceiling is refused");
        assert!(
            matches!(
                error,
                PackError::LimitExceeded {
                    limit: "entry-size",
                    allowed: 1024,
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
        assert!(
            !destination.join("huge.bin").exists(),
            "the partially written entry is removed rather than left behind"
        );
    }

    #[test]
    fn a_symlink_entry_is_refused_rather_than_resolved() {
        // A modpack override tree has no legitimate symlink, and resolving one
        // is how an archive reaches outside the directory it was given.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("linked.mrpack");
        {
            let file = File::create(&archive).expect("create archive");
            let mut writer = zip::ZipWriter::new(file);
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            writer
                .add_symlink("overrides/escape.txt", "/etc/passwd", options)
                .expect("write a symlink entry");
            writer.finish().expect("finish");
        }

        let destination = dir.path().join("out");
        let error = open(&archive)
            .extract_prefix("overrides", &destination)
            .expect_err("a symlink entry is refused");
        assert!(
            matches!(
                error,
                PackError::EntryTypeUnsupported {
                    kind: "symlink",
                    ..
                }
            ),
            "unexpected error: {error:?}"
        );
        assert!(!destination.join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_beneath_an_existing_directory_link_is_refused_rather_than_followed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(&archive, &[("overrides/config/x.toml", b"archive")]);
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&sibling).expect("sibling");
        std::fs::write(sibling.join("x.toml"), b"sibling").expect("sibling file");
        let destination = dir.path().join("out");
        std::fs::create_dir_all(&destination).expect("destination");
        std::os::unix::fs::symlink(&sibling, destination.join("config")).expect("link");

        let error = open(&archive)
            .extract_prefix("overrides", &destination)
            .expect_err("a linked ancestor is refused");

        assert!(
            matches!(error, PackError::EntryPathUnsafe { .. }),
            "unexpected error: {error:?}"
        );
        assert_eq!(
            std::fs::read(sibling.join("x.toml")).expect("read"),
            b"sibling"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_that_is_itself_an_existing_link_is_refused_rather_than_followed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(&archive, &[("overrides/config/x.toml", b"archive")]);
        let outside = dir.path().join("outside.toml");
        std::fs::write(&outside, b"outside").expect("outside file");
        let destination = dir.path().join("out");
        std::fs::create_dir_all(destination.join("config")).expect("config");
        std::os::unix::fs::symlink(&outside, destination.join("config/x.toml")).expect("link");

        let error = open(&archive)
            .extract_prefix("overrides", &destination)
            .expect_err("a linked destination file is refused");

        assert!(
            matches!(error, PackError::EntryPathUnsafe { .. }),
            "unexpected error: {error:?}"
        );
        assert_eq!(std::fs::read(&outside).expect("read"), b"outside");
    }

    #[test]
    fn the_size_budget_is_shared_across_layers_rather_than_per_layer() {
        // A bomb split across `overrides/` and `client-overrides/` must not
        // stay under two separate ceilings.
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("bomb.mrpack");
        let payload = vec![b'x'; 4096];
        zip_file(
            &archive,
            &[
                ("overrides/a.bin", &payload),
                ("client-overrides/b.bin", &payload),
            ],
        );

        let mut pack = PackArchive::open(
            &archive,
            ExtractionLimits {
                max_total_bytes: 6000,
                ..ExtractionLimits::default()
            },
        )
        .expect("open");

        let destination = dir.path().join("out");
        pack.extract_prefix("overrides", &destination)
            .expect("the first layer fits");
        let error = pack
            .extract_prefix("client-overrides", &destination)
            .expect_err("the second overruns the shared budget");
        assert!(matches!(
            error,
            PackError::LimitExceeded {
                limit: "total-size",
                ..
            }
        ));
    }

    #[test]
    fn a_manifest_is_read_by_name_and_a_missing_one_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let archive = dir.path().join("pack.mrpack");
        zip_file(&archive, &[("modrinth.index.json", br#"{"name":"x"}"#)]);

        let mut pack = open(&archive);
        assert_eq!(pack.format(), Some(PackFormat::ModrinthPack));
        assert!(pack.contains("modrinth.index.json"));
        assert_eq!(
            pack.read_manifest("modrinth.index.json").expect("read"),
            br#"{"name":"x"}"#
        );

        let error = pack
            .read_manifest("manifest.json")
            .expect_err("not present");
        assert!(matches!(
            error,
            PackError::ManifestMissing {
                entry: "manifest.json"
            }
        ));
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_refused_with_its_own_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("not-a-pack.mrpack");
        std::fs::write(&path, b"this is not a zip").expect("write");

        let error = PackArchive::open(&path, ExtractionLimits::default()).expect_err("must refuse");
        assert!(matches!(error, PackError::ArchiveInvalid { .. }));
        assert_eq!(error.failing_path(), Some(path));
    }
}
