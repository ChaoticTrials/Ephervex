//! Tracing setup shared by both frontends.
//!
//! Logs carry operation ids, instance ids, and process exit codes. They must
//! never carry refresh tokens, access tokens, the CurseForge API key, or
//! authorization headers — redaction is the responsibility of the crate
//! producing the event, since only it knows which fields are secret.
//!
//! # Why the launcher writes its own log file
//!
//! Everything here also goes to stderr, which is enough while a developer is
//! watching a terminal and useless afterwards: a GUI has no terminal, and the
//! question a bug report actually asks — "what did the launcher do during the
//! import that failed an hour ago" — cannot be answered by output nobody kept.
//! So a second layer appends the same events to a file under
//! [`ephervex_storage::Paths::launcher_logs_dir`], where
//! [`crate::diagnostics`] can list it and a user can open it without knowing
//! where it is.
//!
//! The file is not a competing capture of anything: a *server's* process output
//! belongs to `ephervex-server`'s capture and stays there. This records the
//! launcher's own events, and the operation id in each of them is what ties one
//! of those events to a line in the other.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use chrono::Utc;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

static INITIALIZED: OnceLock<()> = OnceLock::new();

/// Environment variable that overrides the log filter, e.g.
/// `EPHERVEX_LOG=ephervex_download=debug`.
pub const LOG_ENV_VAR: &str = "EPHERVEX_LOG";

/// What both frontends log by default: Ephervex's own events, and nothing at
/// `INFO` from libraries that narrate their own startup.
///
/// `i18n_embed` logs "Selecting translations for domain" at `INFO` every time a
/// language is selected, which is once per CLI invocation. It says nothing a
/// user or a bug report needs, and burying `created client instance` under it
/// makes the logs that *do* matter harder to read. Still reachable with
/// `EPHERVEX_LOG=info,i18n_embed=info` when the localization loader itself is
/// the thing being debugged.
pub const DEFAULT_LOG_DIRECTIVES: &str = "info,i18n_embed=warn";

/// The name every launcher log file starts with.
pub const LAUNCHER_LOG_PREFIX: &str = "launcher-";

/// The extension every launcher log file ends with, matching what the
/// diagnostics browser looks for.
pub const LAUNCHER_LOG_EXTENSION: &str = "log";

/// How large one launcher log file grows before the next one is started.
pub const LAUNCHER_LOG_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

/// How many launcher log files are kept.
///
/// Bounded because this directory is written to on every run and nothing else
/// prunes it. With one file per day, ten of them cover a fortnight of ordinary
/// use — long enough to still hold the run a bug report is about — while a
/// machine that crash-loops overnight is capped by the segment size instead.
pub const RETAINED_LAUNCHER_LOGS: usize = 10;

/// Initializes the global tracing subscriber.
///
/// `launcher_logs` is the directory the launcher's own log file is written
/// into; passing `None` keeps everything on stderr, which is what a process
/// that could not resolve its state directory should do rather than failing to
/// start.
///
/// Safe to call more than once: repeated calls are ignored rather than
/// panicking, so a CLI entry point and an embedded test harness can both call
/// it without coordinating.
pub fn init_tracing(default_directives: &str, launcher_logs: Option<&Path>) {
    INITIALIZED.get_or_init(|| {
        let filter = EnvFilter::try_from_env(LOG_ENV_VAR)
            .unwrap_or_else(|_| EnvFilter::new(default_directives));

        // Writing to stderr keeps stdout clean for `--json` CLI output.
        let console = fmt::layer().with_writer(std::io::stderr).with_target(true);

        // The file layer is built only when a directory was given *and* the
        // first segment opens. A launcher that cannot write its log is still a
        // launcher; it says so once and keeps running.
        let file = launcher_logs.and_then(|directory| match LauncherLog::open(directory) {
            Ok(log) => {
                let handle = LauncherLogHandle(Arc::new(log));
                Some(
                    fmt::layer()
                        // No escape codes: this file is read by a text viewer
                        // and pasted into bug reports.
                        .with_ansi(false)
                        .fmt_fields(PlainFields::default())
                        .with_target(true)
                        .with_writer(move || handle.clone()),
                )
            }
            Err(error) => {
                eprintln!(
                    "could not open a launcher log file in {}: {error}",
                    directory.display()
                );
                None
            }
        });

        // A failed install means another subscriber already owns the global
        // slot, which is not worth aborting a launch over.
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(console)
            .with(file)
            .try_init();
    });
}

/// The span-field formatter the file layer uses.
///
/// A distinct *type* from the console layer's, deliberately: a span's formatted
/// fields are cached per formatter type, so two layers sharing one type also
/// share one cached string — and the file would inherit the terminal's color
/// escapes, in a file whose whole purpose is to be pasted into a bug report.
#[derive(Debug, Default)]
struct PlainFields(fmt::format::DefaultFields);

impl<'writer> tracing_subscriber::fmt::FormatFields<'writer> for PlainFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: fmt::format::Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        // The writer arrives with escapes already disabled by the layer, so the
        // default formatter produces plain text through it.
        self.0.format_fields(writer, fields)
    }
}

/// The launcher's own rotating log file.
#[derive(Debug)]
struct LauncherLog {
    directory: PathBuf,
    segment: Mutex<Option<Segment>>,
    /// How large one file grows before the next one starts. A field rather than
    /// the constant directly so rotation can be exercised without writing
    /// eight megabytes.
    segment_bytes: u64,
}

#[derive(Debug)]
struct Segment {
    file: std::fs::File,
    bytes: u64,
}

impl LauncherLog {
    fn open(directory: &Path) -> std::io::Result<Self> {
        Self::open_with_limit(directory, LAUNCHER_LOG_SEGMENT_BYTES)
    }

    fn open_with_limit(directory: &Path, segment_bytes: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let segment = Segment::create(directory, segment_bytes)?;
        prune(directory, RETAINED_LAUNCHER_LOGS);
        Ok(Self {
            directory: directory.to_path_buf(),
            segment: Mutex::new(Some(segment)),
            segment_bytes,
        })
    }

    /// Appends one formatted event.
    ///
    /// Failures give up on the file rather than propagating: a subscriber that
    /// returns errors would turn a full disk into failing launches, and the
    /// events are still on stderr.
    fn append(&self, buffer: &[u8]) {
        let mut held = self.segment.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(segment) = held.as_mut() else {
            return;
        };
        if segment.bytes >= self.segment_bytes {
            let Ok(next) = Segment::create(&self.directory, self.segment_bytes) else {
                *held = None;
                return;
            };
            *segment = next;
            prune(&self.directory, RETAINED_LAUNCHER_LOGS);
        }
        if segment.write_all(buffer).is_err() {
            *held = None;
        }
    }
}

impl Segment {
    /// Opens the file this run should append to.
    ///
    /// One file per day rather than one per process: a user asking "what
    /// happened when I imported that pack" is asking about a run that has
    /// already ended, and a launcher that started a new file on every
    /// invocation would push that run out of a bounded directory within
    /// minutes of ordinary CLI use. Appending is safe across processes because
    /// the file is opened in append mode, so a GUI and a terminal running at
    /// once interleave whole lines rather than overwriting each other.
    ///
    /// A day whose file has already reached the size limit continues into a
    /// timestamped overflow segment, which sorts after the day's own file.
    fn create(directory: &Path, segment_bytes: u64) -> std::io::Result<Self> {
        let now = Utc::now();
        let daily = directory.join(format!(
            "{LAUNCHER_LOG_PREFIX}{}.{LAUNCHER_LOG_EXTENSION}",
            now.format("%Y%m%d")
        ));
        let full = std::fs::metadata(&daily).is_ok_and(|metadata| metadata.len() >= segment_bytes);
        let path = if full {
            directory.join(format!(
                "{LAUNCHER_LOG_PREFIX}{}.{LAUNCHER_LOG_EXTENSION}",
                now.format("%Y%m%dT%H%M%S%.3fZ")
            ))
        } else {
            daily
        };

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let bytes = file.metadata().map(|metadata| metadata.len())?;
        Ok(Self { file, bytes })
    }

    fn write_all(&mut self, buffer: &[u8]) -> std::io::Result<()> {
        self.file.write_all(buffer)?;
        // Flushed per event: a launcher log is read after the process it
        // describes has crashed, and buffered lines would be exactly the ones
        // missing.
        self.file.flush()?;
        self.bytes += u64::try_from(buffer.len()).unwrap_or(0);
        Ok(())
    }
}

/// Keeps the newest `retained` launcher log files and removes the rest.
fn prune(directory: &Path, retained: usize) {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| is_launcher_log(path))
        .collect();
    if files.len() <= retained {
        return;
    }
    // By name, which is by creation time: the names lead with a timestamp.
    files.sort();
    let removable = files.len() - retained;
    for path in files.into_iter().take(removable) {
        let _ = std::fs::remove_file(path);
    }
}

fn is_launcher_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(LAUNCHER_LOG_PREFIX))
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(LAUNCHER_LOG_EXTENSION))
}

/// The writer handed to the formatting layer.
///
/// Cheap to clone because the layer asks for one per event; the file itself is
/// shared and serialized by the mutex inside.
#[derive(Debug, Clone)]
struct LauncherLogHandle(Arc<LauncherLog>);

impl std::io::Write for LauncherLogHandle {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.append(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn launcher_logs(directory: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_launcher_log(path))
            .collect();
        files.sort();
        files
    }

    #[test]
    fn repeated_initialization_is_harmless() {
        init_tracing("info", None);
        init_tracing("debug", None);
    }

    #[test]
    fn events_reach_a_file_in_the_launcher_log_directory() {
        let base = tempfile::tempdir().expect("temp dir");
        let log = LauncherLog::open(base.path()).expect("open");
        log.append(b"launcher started\n");

        let files = launcher_logs(base.path());
        assert_eq!(files.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&files[0]).expect("read"),
            "launcher started\n"
        );
    }

    #[test]
    fn a_second_run_on_the_same_day_continues_the_same_file() {
        // Otherwise a handful of CLI invocations would push the run a bug
        // report is about out of a bounded directory.
        let base = tempfile::tempdir().expect("temp dir");
        LauncherLog::open(base.path())
            .expect("open")
            .append(b"first run\n");
        LauncherLog::open(base.path())
            .expect("open")
            .append(b"second run\n");

        let files = launcher_logs(base.path());
        assert_eq!(files.len(), 1);
        let body = std::fs::read_to_string(&files[0]).expect("read");
        assert_eq!(body, "first run\nsecond run\n");
    }

    #[test]
    fn a_full_segment_is_continued_in_another_file_and_old_ones_are_pruned() {
        let base = tempfile::tempdir().expect("temp dir");
        let log = LauncherLog::open_with_limit(base.path(), 16).expect("open");

        for _ in 0..4 {
            // Overflow names are millisecond-resolution; two created inside one
            // millisecond would reuse the same file.
            std::thread::sleep(std::time::Duration::from_millis(2));
            log.append(b"a line long enough to fill this segment\n");
        }

        let files = launcher_logs(base.path());
        assert_eq!(files.len(), 4);
        assert!(
            files[0]
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.contains('T')),
            "the day's own file sorts before its overflow segments: {files:?}"
        );

        prune(base.path(), 2);
        assert_eq!(
            launcher_logs(base.path()).len(),
            2,
            "the newest files are the ones kept"
        );
    }

    #[test]
    fn a_directory_that_cannot_be_written_does_not_stop_the_launcher() {
        // `open` reports the failure; the caller keeps logging to stderr.
        let base = tempfile::tempdir().expect("temp dir");
        let occupied = base.path().join("logs");
        std::fs::write(&occupied, "not a directory").expect("write");

        assert!(LauncherLog::open(&occupied).is_err());
    }
}
