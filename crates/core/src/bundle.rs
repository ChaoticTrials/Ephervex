//! Collecting diagnostics into one file somebody can attach to a bug report.
//!
//! # What this is not
//!
//! It is not an upload. Nothing here contacts a service, and no paste site is
//! integrated: the archive is written to a path the user chose and goes no
//! further than they take it.
//!
//! It is also not a cleaner. A diagnostic file goes in exactly as it is on
//! disk, because a crash report that has been tidied is a crash report that
//! lost the detail somebody needed (ADR 0016). What Ephervex guarantees is
//! narrower and checkable: it contributes no secret of its own. Its own log is
//! written under the same redaction rules as its stderr, launch commands are
//! redacted before they reach any log, and `config.toml` is never offered and
//! never included - it is the one file whose whole job is to hold settings, and
//! the walk that strips credential-shaped keys from it exists because that is
//! the file they collect in.
//!
//! # Why the contents are chosen rather than fixed
//!
//! Two reports are rarely about the same thing. Somebody chasing a crash wants
//! the crash report and the game log; somebody reporting that an import went
//! wrong wants the launcher log and the recipe. A fixed set would be too much
//! for one and too little for the other, so the caller picks, and the archive
//! carries a manifest saying what was picked and what was left out - a bundle
//! that silently omitted something would be worse than one that says it did.

use std::io::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use tracing::{info, instrument, warn};
use zip::write::SimpleFileOptions;

use crate::app::App;
use crate::diagnostics::DiagnosticScope;
use crate::error::{CoreError, CoreResult};
use ephervex_storage::diagnostics::DiagnosticKind;

/// How much of a part is read and written at a time.
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// One thing a bundle could carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePart {
    pub kind: BundlePartKind,
    /// What to call it, and the handle to pass back in a selection.
    pub name: String,
    /// Where it is now. Never joined by a frontend.
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// What kind of thing one part is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BundlePartKind {
    /// A log the game wrote.
    GameLog,
    /// A crash report the game wrote.
    CrashReport,
    /// A log Ephervex wrote.
    LauncherLog,
    /// The instance's retained recipe: what it is supposed to contain.
    Recipe,
    /// The instance's own record: which Minecraft, which loader, which Java.
    Record,
}

impl BundlePartKind {
    /// A stable identifier for `--json` output and for the manifest.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GameLog => "game_log",
            Self::CrashReport => "crash_report",
            Self::LauncherLog => "launcher_log",
            Self::Recipe => "recipe",
            Self::Record => "record",
        }
    }

    /// The directory a part of this kind is written into.
    const fn directory(self) -> &'static str {
        match self {
            Self::GameLog => "logs",
            Self::CrashReport => "crash-reports",
            Self::LauncherLog => "launcher",
            Self::Recipe | Self::Record => "instance",
        }
    }
}

/// Everything one scope could contribute, with what each one costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleContents {
    pub parts: Vec<BundlePart>,
}

impl BundleContents {
    /// Every part, as a selection.
    #[must_use]
    pub fn everything(&self) -> BundleSelection {
        BundleSelection {
            names: self.parts.iter().map(|part| part.name.clone()).collect(),
        }
    }

    /// What taking everything would cost.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.parts.iter().map(|part| part.size_bytes).sum()
    }
}

/// Which parts to take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BundleSelection {
    pub names: Vec<String>,
}

impl BundleSelection {
    /// Whether this selection asked for one part.
    #[must_use]
    pub fn includes(&self, name: &str) -> bool {
        self.names.iter().any(|chosen| chosen == name)
    }
}

/// What one export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleReport {
    pub path: PathBuf,
    pub included: Vec<String>,
    /// Parts that were offered and not taken. Named so the manifest and the
    /// report agree about what the archive is missing.
    pub omitted: Vec<String>,
    /// Parts that were taken and could not be read. A file that rotated away
    /// between the listing and the write is the ordinary cause.
    pub unreadable: Vec<String>,
    pub bytes: u64,
}

impl App {
    /// Everything one scope could put in a bundle, and what each part costs.
    ///
    /// Reads only what the diagnostics surface already lists, so it opens no
    /// file and follows no path a frontend supplied.
    #[instrument(skip(self))]
    pub fn plan_diagnostic_bundle(&self, scope: DiagnosticScope) -> CoreResult<BundleContents> {
        let mut parts = Vec::new();
        for kind in scope.kinds() {
            let part_kind = match (scope, kind) {
                (DiagnosticScope::Launcher, DiagnosticKind::Log) => BundlePartKind::LauncherLog,
                (_, DiagnosticKind::Log) => BundlePartKind::GameLog,
                (_, DiagnosticKind::CrashReport) => BundlePartKind::CrashReport,
            };
            for entry in self.diagnostic_entries(scope, *kind)? {
                parts.push(BundlePart {
                    kind: part_kind,
                    name: format!("{}/{}", part_kind.directory(), entry.name),
                    path: entry.path,
                    size_bytes: entry.size_bytes,
                });
            }
        }

        // An instance's own state describes what everything else happened in.
        if let DiagnosticScope::Client(instance) = scope {
            let root = self.client(instance)?.root;
            for (part_kind, path) in [
                (
                    BundlePartKind::Recipe,
                    ephervex_storage::metadata::source_dir(&root).join("recipe.json"),
                ),
                (
                    BundlePartKind::Record,
                    ephervex_storage::metadata::metadata_dir(&root).join("instance.json"),
                ),
            ] {
                let Ok(metadata) = std::fs::metadata(&path) else {
                    continue;
                };
                parts.push(BundlePart {
                    kind: part_kind,
                    name: format!(
                        "{}/{}",
                        part_kind.directory(),
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                    path,
                    size_bytes: metadata.len(),
                });
            }
        }

        Ok(BundleContents { parts })
    }

    /// Writes the chosen parts to one archive at `destination`.
    ///
    /// Every file goes in byte for byte. A part that cannot be read is reported
    /// and the rest of the archive is still written, because a report missing
    /// one rotated log is worth more than no report.
    ///
    /// The archive is published only once it is complete, so a failed write
    /// never leaves a truncated zip at `destination`. A destination that is one
    /// of the files on offer is refused: replacing it would destroy the very
    /// diagnostics the bundle exists to carry.
    #[instrument(skip(self, selection), fields(destination = %destination.display()))]
    pub fn export_diagnostic_bundle(
        &self,
        scope: DiagnosticScope,
        selection: &BundleSelection,
        destination: &Path,
    ) -> CoreResult<BundleReport> {
        let contents = self.plan_diagnostic_bundle(scope)?;
        if names_an_offered_part(destination, &contents.parts) {
            return Err(CoreError::DiagnosticBundleOverwritesSource {
                path: destination.to_path_buf(),
            });
        }
        let (chosen, omitted): (Vec<&BundlePart>, Vec<&BundlePart>) = contents
            .parts
            .iter()
            .partition(|part| selection.includes(&part.name));

        let mut included = Vec::new();
        // Something asked for that no longer exists. A log rotates away
        // between choosing and writing, and saying nothing about it would let
        // the archive look complete while missing what somebody picked.
        let mut unreadable: Vec<String> = selection
            .names
            .iter()
            .filter(|name| !contents.parts.iter().any(|part| &&part.name == name))
            .cloned()
            .collect();
        ephervex_storage::write_atomic(
            destination,
            ephervex_storage::AtomicWriteOptions::default(),
            |file| {
                let mut archive = zip::ZipWriter::new(file);
                let options = SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                for part in &chosen {
                    let source = match std::fs::File::open(&part.path) {
                        Ok(source) => source,
                        Err(error) => {
                            warn!(%error, part = %part.name, "a diagnostic file could not be read");
                            unreadable.push(part.name.clone());
                            continue;
                        }
                    };
                    // A log can still be growing while it is copied, so its
                    // size when it was listed does not bound the entry.
                    let part_options = options.large_file(true);
                    match copy_entry(&mut archive, &part.name, source, part_options)? {
                        EntryCopy::Complete => included.push(part.name.clone()),
                        EntryCopy::Unreadable(error) => {
                            warn!(%error, part = %part.name, "a diagnostic file could not be read");
                            unreadable.push(part.name.clone());
                        }
                    }
                }

                let manifest = manifest(scope, &included, &omitted, &unreadable);
                write_entry(&mut archive, "bundle.txt", manifest.as_bytes(), options)?;
                archive.finish().map(drop).map_err(std::io::Error::other)
            },
        )
        .map_err(|source| CoreError::DiagnosticBundleNotWritten {
            path: destination.to_path_buf(),
            detail: source.to_string(),
        })?;

        let bytes = std::fs::metadata(destination).map_or(0, |data| data.len());
        info!(
            included = included.len(),
            omitted = omitted.len(),
            unreadable = unreadable.len(),
            bytes,
            "wrote a diagnostic bundle"
        );
        Ok(BundleReport {
            path: destination.to_path_buf(),
            included,
            omitted: omitted.iter().map(|part| part.name.clone()).collect(),
            unreadable,
            bytes,
        })
    }
}

/// Whether `destination` is the same file as any part on offer, whether it is
/// named directly, through a link, or by a second name for the same inode.
fn names_an_offered_part(destination: &Path, parts: &[BundlePart]) -> bool {
    let Ok(target) = std::fs::metadata(destination) else {
        return false;
    };
    parts.iter().any(|part| {
        std::fs::metadata(&part.path)
            .is_ok_and(|source| source.dev() == target.dev() && source.ino() == target.ino())
    })
}

fn write_entry<W: std::io::Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    name: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> std::io::Result<()> {
    archive
        .start_file(name, options)
        .map_err(std::io::Error::other)?;
    archive.write_all(bytes)
}

/// How one streamed entry ended.
enum EntryCopy {
    Complete,
    /// The source failed after the entry started. The partial entry has been
    /// removed from the archive.
    Unreadable(std::io::Error),
}

/// Streams `source` into a new entry without holding the whole file in memory.
///
/// A failure to write the archive is an error. A failure to read the source
/// drops the partial entry, so a truncated copy is never listed as included.
fn copy_entry<R: std::io::Read, W: std::io::Write + std::io::Seek>(
    archive: &mut zip::ZipWriter<W>,
    name: &str,
    mut source: R,
    options: SimpleFileOptions,
) -> std::io::Result<EntryCopy> {
    archive
        .start_file(name, options)
        .map_err(std::io::Error::other)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        match source.read(&mut buffer) {
            Ok(0) => return Ok(EntryCopy::Complete),
            Ok(read) => archive.write_all(&buffer[..read])?,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                archive.abort_file().map_err(std::io::Error::other)?;
                return Ok(EntryCopy::Unreadable(error));
            }
        }
    }
}

/// What the archive says about itself.
///
/// Read by a person, so it is plain text rather than JSON, and it names the
/// omissions as plainly as the inclusions.
fn manifest(
    scope: DiagnosticScope,
    included: &[String],
    omitted: &[&BundlePart],
    unreadable: &[String],
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = write!(
        out,
        "Ephervex diagnostic bundle\n\nscope: {scope:?}\nversion: {}\n\n\
         Files are included exactly as they are on disk. Ephervex adds no secret of its own,\n\
         and the configuration file is never included.\n\nincluded:\n",
        ephervex_model::APP_VERSION
    );
    for name in included {
        let _ = writeln!(out, "  {name}");
    }
    if !omitted.is_empty() {
        out.push_str("\nleft out, by choice:\n");
        for part in omitted {
            let _ = writeln!(out, "  {}", part.name);
        }
    }
    if !unreadable.is_empty() {
        out.push_str("\nasked for and could not be read:\n");
        for name in unreadable {
            let _ = writeln!(out, "  {name}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::io::Read as _;

    use ephervex_model::{LoaderKind, LoaderSelection};
    use ephervex_storage::{NewClient, Paths};

    use super::*;

    /// An instance with a game log, a crash report and its own recorded state.
    fn instance(base: &Path) -> (App, crate::diagnostics::DiagnosticScope) {
        let app = App::open_at(Paths::rooted_at(base)).expect("app opens");
        let record = app
            .create_client_for_test(NewClient::new(
                "Bundle Example",
                "1.21.1",
                LoaderSelection::exact(LoaderKind::NeoForge, "21.1.77"),
            ))
            .expect("create");

        let game = ephervex_storage::metadata::client_game_dir(&record.root);
        std::fs::create_dir_all(game.join("logs")).expect("logs");
        std::fs::create_dir_all(game.join("crash-reports")).expect("crash reports");
        std::fs::write(game.join("logs").join("latest.log"), b"a game log").expect("log");
        std::fs::write(
            game.join("crash-reports").join("crash-2026-01-01.txt"),
            b"a crash report",
        )
        .expect("report");

        let source = ephervex_storage::metadata::source_dir(&record.root);
        std::fs::create_dir_all(&source).expect("source");
        std::fs::write(source.join("recipe.json"), b"{}").expect("recipe");

        (app, crate::diagnostics::DiagnosticScope::Client(record.id))
    }

    fn entries(archive: &Path) -> Vec<String> {
        let file = std::fs::File::open(archive).expect("open");
        let mut zip = zip::ZipArchive::new(file).expect("read archive");
        (0..zip.len())
            .map(|index| {
                zip.by_index(index)
                    .expect("entry")
                    .name()
                    .expect("entry name")
                    .into_owned()
            })
            .collect()
    }

    fn read_entry(archive: &Path, name: &str) -> String {
        let file = std::fs::File::open(archive).expect("open");
        let mut zip = zip::ZipArchive::new(file).expect("read archive");
        let mut entry = zip.by_name(name).expect("entry");
        let mut text = String::new();
        entry.read_to_string(&mut text).expect("read");
        text
    }

    #[test]
    fn the_configuration_file_is_never_offered_and_never_included() {
        // The one file whose whole job is to hold settings, and the one
        // credential-shaped keys collect in. It is not a choice a user can get
        // wrong, because it is not a choice.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());

        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        assert!(
            !contents
                .parts
                .iter()
                .any(|part| part.name.contains("config")),
            "nothing named config is on offer: {:?}",
            contents.parts
        );

        let archive = base.path().join("bundle.zip");
        app.export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");
        assert!(
            !entries(&archive).iter().any(|name| name.contains("config")),
            "and nothing named config is in the archive"
        );
    }

    #[test]
    fn only_diagnostic_files_and_the_hosts_own_record_are_ever_offered() {
        // A game directory and the launcher's own directories hold settings,
        // saves, and account state beside the logs. None of that is a choice
        // a bundle offers, whatever it is called.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let crate::diagnostics::DiagnosticScope::Client(id) = scope else {
            panic!("a client scope");
        };
        let game =
            ephervex_storage::metadata::client_game_dir(&app.client(id).expect("client").root);
        for name in [
            "options.txt",
            "servers.dat",
            "config.toml",
            "credentials.json",
        ] {
            std::fs::write(game.join(name), b"not diagnostics").expect("write");
        }
        std::fs::write(game.join("logs").join("launcher_accounts.json"), b"{}").expect("write");
        std::fs::create_dir_all(app.paths().config_dir()).expect("config dir");
        std::fs::write(app.paths().config_file(), b"[auth]").expect("config");
        std::fs::create_dir_all(app.paths().logs_dir()).expect("launcher logs");
        std::fs::write(
            app.paths().logs_dir().join("ephervex.log"),
            b"a launcher log",
        )
        .expect("log");

        for scope in [scope, crate::diagnostics::DiagnosticScope::Launcher] {
            for part in app.plan_diagnostic_bundle(scope).expect("plan").parts {
                let allowed = match part.kind {
                    BundlePartKind::GameLog | BundlePartKind::LauncherLog => {
                        std::path::Path::new(&part.name)
                            .extension()
                            .is_some_and(|extension| extension == "log" || extension == "gz")
                    }
                    BundlePartKind::CrashReport => part.name.starts_with("crash-reports/"),
                    BundlePartKind::Recipe => part.name == "instance/recipe.json",
                    BundlePartKind::Record => part.name == "instance/instance.json",
                };
                assert!(allowed, "{scope:?} offered {}", part.name);
                assert!(
                    !part.path.starts_with(app.paths().config_dir()),
                    "{}",
                    part.name
                );
            }
        }
    }

    #[test]
    fn only_what_was_chosen_goes_in_and_the_rest_is_named() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let contents = app.plan_diagnostic_bundle(scope).expect("plan");

        let only_the_crash = BundleSelection {
            names: contents
                .parts
                .iter()
                .filter(|part| part.kind == BundlePartKind::CrashReport)
                .map(|part| part.name.clone())
                .collect(),
        };
        assert_eq!(only_the_crash.names.len(), 1);

        let archive = base.path().join("bundle.zip");
        let report = app
            .export_diagnostic_bundle(scope, &only_the_crash, &archive)
            .expect("export");

        assert_eq!(report.included.len(), 1);
        assert!(
            !report.omitted.is_empty(),
            "the rest was offered and declined"
        );
        let manifest = read_entry(&archive, "bundle.txt");
        assert!(manifest.contains("left out, by choice"));
        for name in &report.omitted {
            assert!(
                manifest.contains(name.as_str()),
                "the archive says what it is missing: {name}"
            );
        }
    }

    #[test]
    fn a_file_goes_in_byte_for_byte() {
        // Tidying a crash report is how the line somebody needed disappears.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let archive = base.path().join("bundle.zip");
        app.export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");

        assert_eq!(
            read_entry(&archive, "crash-reports/crash-2026-01-01.txt"),
            "a crash report"
        );
        assert_eq!(read_entry(&archive, "logs/latest.log"), "a game log");
    }

    #[test]
    fn a_part_that_vanished_between_the_listing_and_the_write_is_reported() {
        // Logs rotate. One that went away is worth saying so about, and is not
        // a reason to abandon the rest of the report.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let vanishing = contents
            .parts
            .iter()
            .find(|part| part.kind == BundlePartKind::GameLog)
            .expect("a log");
        std::fs::remove_file(&vanishing.path).expect("rotate it away");

        let archive = base.path().join("bundle.zip");
        let report = app
            .export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("an unreadable part does not fail the export");

        assert_eq!(report.unreadable, [vanishing.name.as_str()]);
        assert!(!report.included.is_empty(), "the rest was still written");
        assert!(read_entry(&archive, "bundle.txt").contains("could not be read"));
    }

    /// Yields `good` bytes and then fails, as a disk error part way through would.
    struct FailsAfter {
        good: usize,
    }

    impl std::io::Read for FailsAfter {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.good == 0 {
                return Err(std::io::Error::other("a read failed"));
            }
            let count = buffer.len().min(self.good);
            buffer[..count].fill(b'x');
            self.good -= count;
            Ok(count)
        }
    }

    #[test]
    fn a_part_larger_than_the_copy_buffer_goes_in_byte_for_byte() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let log = part_path(&app, scope, "logs/latest.log");
        let content: Vec<u8> = (0..COPY_BUFFER_BYTES * 3 + 17)
            .map(|index| u8::try_from(index % 251).expect("fits"))
            .collect();
        std::fs::write(&log, &content).expect("grow the log");

        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let archive = base.path().join("bundle.zip");
        app.export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).expect("open"))
            .expect("read archive");
        let mut copied = Vec::new();
        zip.by_name("logs/latest.log")
            .expect("entry")
            .read_to_end(&mut copied)
            .expect("read");
        assert_eq!(copied, content);
    }

    #[test]
    fn a_read_failure_part_way_through_drops_the_partial_entry() {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        write_entry(&mut archive, "kept.txt", b"kept", options).expect("first entry");

        let outcome = copy_entry(
            &mut archive,
            "logs/broken.log",
            FailsAfter {
                good: COPY_BUFFER_BYTES + 5,
            },
            options,
        )
        .expect("a read failure is not a write failure");
        assert!(matches!(outcome, EntryCopy::Unreadable(_)));
        write_entry(&mut archive, "after.txt", b"after", options).expect("later entry");

        let mut zip = zip::ZipArchive::new(archive.finish().expect("finish")).expect("read");
        let names: Vec<String> = zip
            .file_names()
            .map(|name| name.expect("entry name").into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.iter().all(|name| name != "logs/broken.log"));
        let mut text = String::new();
        zip.by_name("after.txt")
            .expect("entry")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "after", "the entry after the dropped one is intact");
    }

    #[test]
    fn a_part_that_cannot_be_opened_is_unreadable_and_leaves_no_entry() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let log = part_path(&app, scope, "logs/latest.log");
        std::fs::set_permissions(&log, std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .expect("revoke read access");
        if std::fs::File::open(&log).is_ok() {
            // Running as a user that ignores file modes.
            return;
        }

        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let archive = base.path().join("bundle.zip");
        let report = app
            .export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");

        assert_eq!(report.unreadable, ["logs/latest.log"]);
        assert!(!entries(&archive).contains(&"logs/latest.log".to_owned()));
    }

    #[test]
    fn every_copied_part_is_declared_large_enough_for_a_growing_log() {
        // The sizes in an entry's local header are the 0xFFFFFFFF marker
        // exactly when the entry was started as a zip64 entry.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let archive = base.path().join("bundle.zip");
        app.export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");

        let bytes = std::fs::read(&archive).expect("read");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("read archive");
        let mut start = |name: &str| {
            usize::try_from(zip.by_name(name).expect("entry").header_start()).expect("offset")
        };
        let log = start("logs/latest.log");
        assert_eq!(bytes[log + 18..log + 26], [0xFF; 8], "a part is zip64");
        let manifest = start("bundle.txt");
        assert_ne!(bytes[manifest + 18..manifest + 26], [0xFF; 8]);
    }

    /// Writes a part past 4 GiB, which a plain entry cannot hold. Slow, so it
    /// runs only when asked for.
    #[test]
    #[ignore = "writes and compresses more than 4 GiB"]
    fn a_part_larger_than_four_gibibytes_is_exported() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let log = part_path(&app, scope, "logs/latest.log");
        let size = u64::from(u32::MAX) + 4096;
        std::fs::File::create(&log)
            .and_then(|file| file.set_len(size))
            .expect("sparse log");

        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let archive = base.path().join("bundle.zip");
        let report = app
            .export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");

        assert!(report.unreadable.is_empty());
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).expect("open"))
            .expect("read archive");
        assert_eq!(zip.by_name("logs/latest.log").expect("entry").size(), size);
    }

    fn part_path(app: &App, scope: DiagnosticScope, name: &str) -> PathBuf {
        app.plan_diagnostic_bundle(scope)
            .expect("plan")
            .parts
            .into_iter()
            .find(|part| part.name == name)
            .expect("the part is offered")
            .path
    }

    fn assert_refused_and_untouched(
        app: &App,
        scope: DiagnosticScope,
        destination: &Path,
        source: &Path,
    ) {
        let before = std::fs::read(source).expect("read source");
        let directory = destination.parent().expect("parent");
        let listing = |path: &Path| {
            let mut names: Vec<_> = std::fs::read_dir(path)
                .expect("list")
                .map(|entry| entry.expect("entry").file_name())
                .collect();
            names.sort();
            names
        };
        let siblings = listing(directory);

        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let error = app
            .export_diagnostic_bundle(scope, &contents.everything(), destination)
            .expect_err("a source file is never the destination");

        assert!(
            matches!(error, CoreError::DiagnosticBundleOverwritesSource { .. }),
            "{error:?}"
        );
        assert_eq!(std::fs::read(source).expect("read source"), before);
        assert_eq!(
            listing(directory),
            siblings,
            "no staging file is left behind"
        );
    }

    #[test]
    fn a_bundle_is_refused_when_its_destination_is_a_chosen_source() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let log = part_path(&app, scope, "logs/latest.log");

        assert_refused_and_untouched(&app, scope, &log, &log);
        assert_eq!(std::fs::read(&log).expect("read"), b"a game log");
    }

    #[test]
    fn a_bundle_is_refused_when_its_destination_reaches_a_source_through_a_link() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let log = part_path(&app, scope, "logs/latest.log");
        let symlink = base.path().join("link.zip");
        std::os::unix::fs::symlink(&log, &symlink).expect("symlink");
        let hard_link = base.path().join("hard.zip");
        std::fs::hard_link(&log, &hard_link).expect("hard link");

        assert_refused_and_untouched(&app, scope, &symlink, &log);
        assert_refused_and_untouched(&app, scope, &hard_link, &log);
    }

    #[test]
    fn a_bundle_is_refused_over_an_offered_file_that_was_not_chosen() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let log = part_path(&app, scope, "logs/latest.log");
        let only_the_crash = BundleSelection {
            names: vec!["crash-reports/crash-2026-01-01.txt".to_owned()],
        };

        let error = app
            .export_diagnostic_bundle(scope, &only_the_crash, &log)
            .expect_err("an offered file is never the destination");

        assert!(matches!(
            error,
            CoreError::DiagnosticBundleOverwritesSource { .. }
        ));
        assert_eq!(std::fs::read(&log).expect("read"), b"a game log");
    }

    #[test]
    fn a_bundle_replaces_an_unrelated_existing_file() {
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let archive = base.path().join("bundle.zip");
        std::fs::write(&archive, b"an earlier bundle").expect("seed");

        app.export_diagnostic_bundle(scope, &contents.everything(), &archive)
            .expect("export");

        assert_eq!(read_entry(&archive, "logs/latest.log"), "a game log");
    }

    #[test]
    fn a_failed_publication_leaves_neither_a_partial_archive_nor_a_staging_file() {
        // A directory cannot be replaced by a file, so the archive is fully
        // written to its staging name and publication is what fails.
        let base = tempfile::tempdir().expect("temp dir");
        let (app, scope) = instance(base.path());
        let contents = app.plan_diagnostic_bundle(scope).expect("plan");
        let blocked = base.path().join("out");
        std::fs::create_dir(&blocked).expect("directory");

        let error = app
            .export_diagnostic_bundle(scope, &contents.everything(), &blocked)
            .expect_err("the destination cannot be replaced");

        assert!(matches!(
            error,
            CoreError::DiagnosticBundleNotWritten { .. }
        ));
        assert!(blocked.is_dir());
        assert_eq!(
            std::fs::read_dir(base.path())
                .expect("list")
                .filter(|entry| {
                    entry
                        .as_ref()
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .contains(".tmp.")
                })
                .count(),
            0,
            "the staging file is removed"
        );
    }
}
