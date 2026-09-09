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
//!   text like everything else in the file.
//!
//! Extraction is synchronous. `zip` is a blocking API, and wrapping it in
//! `spawn_blocking` is the caller's decision because only the caller knows
//! whether it is on a runtime.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use tracing::{debug, instrument};
use zip::ZipArchive;

use crate::detect::{PackFormat, identify};
use crate::error::{PackError, PackResult};
use crate::path::{contained_join, is_reserved, safe_relative_path};

/// Bounds on what an archive may expand into.
///
/// "For local developer use, limits can be generous, but they must exist." The
/// defaults are sized for a large real-world modpack — a few thousand files and
/// a couple of gigabytes once resource packs are counted — and are configurable
/// so a caller with a bigger archive can raise them deliberately rather than
/// discovering there was never a ceiling at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    /// Guards against one entry claiming the whole budget, which is what a bomb
    /// built from a single stream looks like.
    pub max_entry_bytes: u64,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_entry_bytes: 2 * 1024 * 1024 * 1024,
        }
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
    zip: ZipArchive<File>,
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
        let zip = ZipArchive::new(file).map_err(|source| PackError::ArchiveInvalid {
            path: path.to_path_buf(),
            source,
        })?;

        let entries = zip.len() as u64;
        if entries > limits.max_entries {
            return Err(PackError::LimitExceeded {
                limit: "entry-count",
                allowed: limits.max_entries,
                reached: entries,
            });
        }

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
    /// Directory entries are optional in a ZIP — plenty of writers emit only
    /// file entries — so this asks about *contents* rather than about the
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

    /// Reads one entry into memory, bounded by the per-entry limit.
    ///
    /// Manifests are the only thing read this way; everything else is streamed
    /// to disk.
    pub fn read_entry(&mut self, name: &'static str) -> PackResult<Vec<u8>> {
        let index = self
            .zip
            .index_for_name(name)
            .ok_or(PackError::ManifestMissing { entry: name })?;

        let path = self.path.clone();
        let limit = self.limits.max_entry_bytes;
        let mut entry = self
            .zip
            .by_index(index)
            .map_err(|source| PackError::ArchiveInvalid {
                path: path.clone(),
                source,
            })?;

        let mut bytes = Vec::new();
        // Bounded by the limit rather than by the declared size: the header's
        // figure is attacker-controlled, and a manifest is small enough that
        // anything approaching the cap is already an attack.
        let read = (&mut entry)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| PackError::Unreadable { path, source })? as u64;
        drop(entry);
        self.charge(read, 1)?;
        Ok(bytes)
    }

    /// Extracts everything beneath `prefix` into `destination`.
    ///
    /// The prefix is stripped, so `overrides/config/a.toml` lands at
    /// `<destination>/config/a.toml`. Extracting a prefix that does not exist is
    /// not an error — a pack with no `server-overrides/` is perfectly normal —
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

            if is_dir {
                std::fs::create_dir_all(&target).map_err(|source| PackError::Unwritable {
                    path: target.clone(),
                    source,
                })?;
                report.directories += 1;
                continue;
            }

            // Charged before the read so a bomb is refused on its declared size
            // where it lies about it, and on its real size where it does not.
            self.charge(declared, 1)?;
            let written = self.write_entry(index, &target)?;
            self.charge(written.saturating_sub(declared), 0)?;
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

    /// Streams one entry to disk through a bounded reader.
    fn write_entry(&mut self, index: usize, target: &Path) -> PackResult<u64> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| PackError::Unwritable {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let limit = self.limits.max_entry_bytes;
        let archive_path = self.path.clone();
        let mut entry = self
            .zip
            .by_index(index)
            .map_err(|source| PackError::ArchiveInvalid {
                path: archive_path,
                source,
            })?;

        let unwritable = |source: io::Error| PackError::Unwritable {
            path: target.to_path_buf(),
            source,
        };
        let mut file = File::create(target).map_err(unwritable)?;
        // `limit + 1` so an entry that fills the allowance exactly is
        // distinguishable from one that overruns it.
        let written = io::copy(&mut (&mut entry).take(limit.saturating_add(1)), &mut file)
            .map_err(unwritable)?;
        if written > limit {
            // The partial file is removed rather than left looking complete.
            let _ = std::fs::remove_file(target);
            return Err(PackError::LimitExceeded {
                limit: "entry-size",
                allowed: limit,
                reached: written,
            });
        }

        // Permission bits are deliberately not carried over from the archive.
        // Nothing in a modpack override needs to be executable, and honoring a
        // mode from untrusted input is how an archive gets to choose what runs
        Ok(written)
    }

    /// Charges the shared extraction budget, refusing when a limit is reached.
    fn charge(&mut self, bytes: u64, entries: u64) -> PackResult<()> {
        if bytes > self.limits.max_entry_bytes {
            return Err(PackError::LimitExceeded {
                limit: "entry-size",
                allowed: self.limits.max_entry_bytes,
                reached: bytes,
            });
        }
        self.spent_entries = self.spent_entries.saturating_add(entries);
        if self.spent_entries > self.limits.max_entries {
            return Err(PackError::LimitExceeded {
                limit: "entry-count",
                allowed: self.limits.max_entries,
                reached: self.spent_entries,
            });
        }
        self.spent_bytes = self.spent_bytes.saturating_add(bytes);
        if self.spent_bytes > self.limits.max_total_bytes {
            return Err(PackError::LimitExceeded {
                limit: "total-size",
                allowed: self.limits.max_total_bytes,
                reached: self.spent_bytes,
            });
        }
        Ok(())
    }
}

/// Validates one entry name and returns its path relative to `prefix`, or
/// `None` when the entry is not under the prefix.
///
/// A free function rather than a method so it can run while an entry borrowed
/// from the archive is still alive — and because it is pure: an entry name and
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
    use std::io::Write as _;

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

    fn open(path: &Path) -> PackArchive {
        PackArchive::open(path, ExtractionLimits::default()).expect("open")
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
            pack.read_entry("modrinth.index.json").expect("read"),
            br#"{"name":"x"}"#
        );

        let error = pack.read_entry("manifest.json").expect_err("not present");
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
