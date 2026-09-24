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
use std::path::{Path, PathBuf};

use tracing::{info, instrument, warn};
use zip::write::SimpleFileOptions;

use crate::app::App;
use crate::diagnostics::DiagnosticScope;
use crate::error::{CoreError, CoreResult};
use ephervex_storage::diagnostics::DiagnosticKind;

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
    #[instrument(skip(self, selection), fields(destination = %destination.display()))]
    pub fn export_diagnostic_bundle(
        &self,
        scope: DiagnosticScope,
        selection: &BundleSelection,
        destination: &Path,
    ) -> CoreResult<BundleReport> {
        let contents = self.plan_diagnostic_bundle(scope)?;
        let (chosen, omitted): (Vec<&BundlePart>, Vec<&BundlePart>) = contents
            .parts
            .iter()
            .partition(|part| selection.includes(&part.name));

        let file = std::fs::File::create(destination).map_err(|source| {
            CoreError::DiagnosticBundleNotWritten {
                path: destination.to_path_buf(),
                detail: source.to_string(),
            }
        })?;
        let mut archive = zip::ZipWriter::new(file);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

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
        for part in &chosen {
            match std::fs::read(&part.path) {
                Ok(bytes) => {
                    if write_entry(&mut archive, &part.name, &bytes, options).is_err() {
                        unreadable.push(part.name.clone());
                        continue;
                    }
                    included.push(part.name.clone());
                }
                Err(error) => {
                    warn!(%error, part = %part.name, "a diagnostic file could not be read");
                    unreadable.push(part.name.clone());
                }
            }
        }

        let manifest = manifest(scope, &included, &omitted, &unreadable);
        let _ = write_entry(&mut archive, "bundle.txt", manifest.as_bytes(), options);
        archive
            .finish()
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

fn write_entry(
    archive: &mut zip::ZipWriter<std::fs::File>,
    name: &str,
    bytes: &[u8],
    options: SimpleFileOptions,
) -> std::io::Result<()> {
    archive
        .start_file(name, options)
        .map_err(std::io::Error::other)?;
    archive.write_all(bytes)
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
            .map(|index| zip.by_index(index).expect("entry").name().to_owned())
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
}
