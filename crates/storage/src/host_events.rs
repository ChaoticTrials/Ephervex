//! A short history of what happened to one instance or server: when it was
//! created, started, stopped or crashed, and what its log complained about.
//!
//! One file per host, one JSON object per line, under the state root and not
//! inside the host, so the history survives the host's files being reinstalled
//! and is removed with the host. Lines are only ever appended; the file is cut
//! back to the newest entries once it grows past a bound. Appending and
//! cutting back both run under an exclusive `flock` on a lock file beside the
//! history, so two launcher processes never lose each other's events.
//! Everything read back is treated as untrusted text, read as bytes: a line
//! that does not parse, or is not UTF-8, costs that line.

use std::fs::OpenOptions;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;

use chrono::{DateTime, Utc};
use ephervex_model::{InstanceId, ServerId};
use serde::{Deserialize, Serialize};

use crate::atomic::{AtomicWriteOptions, write_atomic};
use crate::paths::Paths;

/// The most events one host keeps.
pub const MAX_HOST_EVENTS: usize = 400;
/// The longest subject, in characters.
pub const MAX_EVENT_SUBJECT_CHARS: usize = 200;
/// The longest detail, in characters. A log excerpt fits.
pub const MAX_EVENT_DETAIL_CHARS: usize = 1200;
/// A file past this many bytes is cut back to its newest events. Well under
/// what [`MAX_HOST_EVENTS`] events of full length take, so a history that is
/// merely long is cut back once, not on every append.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// What an event says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostEventKind {
    /// The host was created. The subject names its storage.
    Created,
    /// A client was launched. The subject is the account it played as.
    Launched,
    /// A client exited. The subject is how: an exit code or a signal.
    Exited,
    /// A server started. The subject is its address.
    ServerStarted,
    /// A server stopped. The subject is how.
    ServerStopped,
    /// An error line in a log. The subject is the line and the detail the
    /// lines around it.
    LogError,
    /// The bug was seen again. The subject is the new count.
    Reproduced,
    /// A kind this build does not know, written by a newer one.
    #[serde(other)]
    Other,
}

/// One thing that happened to a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEvent {
    /// When it happened.
    pub at: DateTime<Utc>,
    pub kind: HostEventKind,
    /// The short fact the kind names.
    #[serde(default)]
    pub subject: String,
    /// More text for a reader who wants it, such as a log excerpt.
    #[serde(default)]
    pub detail: String,
}

impl HostEvent {
    /// An event happening now. The texts are cleaned of control characters
    /// (a detail keeps its line breaks) and cut to their bounds.
    #[must_use]
    pub fn now(kind: HostEventKind, subject: &str, detail: &str) -> Self {
        Self::at(Utc::now(), kind, subject, detail)
    }

    /// An event that happened at `at`.
    #[must_use]
    pub fn at(at: DateTime<Utc>, kind: HostEventKind, subject: &str, detail: &str) -> Self {
        Self {
            at,
            kind,
            subject: clean(subject, MAX_EVENT_SUBJECT_CHARS, false),
            detail: clean(detail, MAX_EVENT_DETAIL_CHARS, true),
        }
    }
}

/// Characters that change how surrounding text is laid out or ordered, which a
/// line of untrusted text has no business doing.
const fn is_layout_control(character: char) -> bool {
    matches!(
        character,
        '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

fn clean(text: &str, limit: usize, keep_lines: bool) -> String {
    text.chars()
        .filter(|character| {
            !is_layout_control(*character)
                && (!character.is_control() || (keep_lines && *character == '\n'))
        })
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Whose history it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventHost {
    Client(InstanceId),
    Server(ServerId),
}

impl EventHost {
    fn file_stem(self) -> String {
        match self {
            Self::Client(id) => format!("client-{id}"),
            Self::Server(id) => format!("server-{id}"),
        }
    }
}

impl Paths {
    /// `$XDG_STATE_HOME/ephervex/host-events/`, one history per host.
    #[must_use]
    pub fn host_events_dir(&self) -> std::path::PathBuf {
        self.state_dir().join("host-events")
    }

    /// The history file of one host.
    #[must_use]
    pub fn host_events_file(&self, host: EventHost) -> std::path::PathBuf {
        self.host_events_dir()
            .join(format!("{}.jsonl", host.file_stem()))
    }

    /// The file whose lock serializes writers of one host's history.
    fn host_events_lock_file(&self, host: EventHost) -> std::path::PathBuf {
        self.host_events_dir()
            .join(format!("{}.lock", host.file_stem()))
    }
}

/// Holds the exclusive lock on one host's history for as long as it lives.
struct HistoryLock {
    _file: std::fs::File,
}

impl HistoryLock {
    fn take(paths: &Paths, host: EventHost) -> io::Result<Self> {
        std::fs::create_dir_all(paths.host_events_dir())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(paths.host_events_lock_file(host))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(io::Error::from)?;
        Ok(Self { _file: file })
    }
}

/// Adds `event` to the end of the host's history.
///
/// A line is written with a single append under the history's lock. A file that
/// has grown past its bound is cut back to the newest events while the lock is
/// still held.
pub fn append_host_event(paths: &Paths, host: EventHost, event: &HostEvent) -> io::Result<()> {
    let _lock = HistoryLock::take(paths, host)?;
    let path = paths.host_events_file(host);
    let mut line = serde_json::to_vec(event).map_err(io::Error::other)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&line)?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
        drop(file);
        cut_back(paths, host)?;
    }
    Ok(())
}

/// Rewrites the history with only its newest events. A history that cannot be
/// read is left as it is rather than replaced by an empty one.
fn cut_back(paths: &Paths, host: EventHost) -> io::Result<()> {
    let newest = read_events(paths, host)?;
    let skip = newest.len().saturating_sub(MAX_HOST_EVENTS);
    write_atomic(
        &paths.host_events_file(host),
        AtomicWriteOptions::private(),
        |file| {
            for event in &newest[skip..] {
                let mut line = serde_json::to_vec(event).map_err(io::Error::other)?;
                line.push(b'\n');
                file.write_all(&line)?;
            }
            Ok(())
        },
    )
}

/// The events the file holds, oldest first. A line that is not UTF-8 or does not
/// parse is skipped; a file that is not there holds none.
fn read_events(paths: &Paths, host: EventHost) -> io::Result<Vec<HostEvent>> {
    let file = match std::fs::File::open(paths.host_events_file(host)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if let Ok(event) = serde_json::from_slice::<HostEvent>(&line) {
            events.push(HostEvent::at(
                event.at,
                event.kind,
                &event.subject,
                &event.detail,
            ));
        }
    }
    Ok(events)
}

/// The host's history, oldest first, at most [`MAX_HOST_EVENTS`] long. A host
/// with no history, or one that cannot be read, has none.
#[must_use]
pub fn read_host_events(paths: &Paths, host: EventHost) -> Vec<HostEvent> {
    let mut events = read_events(paths, host).unwrap_or_default();
    let skip = events.len().saturating_sub(MAX_HOST_EVENTS);
    events.drain(..skip);
    events
}

/// Forgets the host's history. A history that is already gone is not an error.
pub fn remove_host_events(paths: &Paths, host: EventHost) -> io::Result<()> {
    for path in [
        paths.host_events_file(host),
        paths.host_events_lock_file(host),
    ] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let directory = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(directory.path());
        (directory, paths)
    }

    #[test]
    fn events_are_read_back_oldest_first() {
        let (_directory, paths) = paths();
        let host = EventHost::Client(InstanceId::new());
        append_host_event(
            &paths,
            host,
            &HostEvent::now(HostEventKind::Created, "temporary", ""),
        )
        .expect("append");
        append_host_event(
            &paths,
            host,
            &HostEvent::now(HostEventKind::Launched, "QuietBlock", ""),
        )
        .expect("append");

        let events = read_host_events(&paths, host);
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [HostEventKind::Created, HostEventKind::Launched]
        );
        assert_eq!(events[1].subject, "QuietBlock");
    }

    #[test]
    fn a_host_has_only_its_own_history() {
        let (_directory, paths) = paths();
        let first = EventHost::Client(InstanceId::new());
        let second = EventHost::Server(ServerId::new());
        append_host_event(
            &paths,
            first,
            &HostEvent::now(HostEventKind::Created, "", ""),
        )
        .expect("append");

        assert_eq!(read_host_events(&paths, first).len(), 1);
        assert!(read_host_events(&paths, second).is_empty());
    }

    #[test]
    fn text_is_cleaned_and_cut_to_its_bounds() {
        let event = HostEvent::now(
            HostEventKind::LogError,
            &format!("bad\u{7}\nline{}", "x".repeat(500)),
            &format!("first\nsecond\u{1b}{}", "y".repeat(5000)),
        );

        assert!(!event.subject.contains('\n'));
        assert!(!event.subject.contains('\u{7}'));
        assert_eq!(event.subject.chars().count(), MAX_EVENT_SUBJECT_CHARS);
        assert!(event.detail.starts_with("first\nsecond"));
        assert!(!event.detail.contains('\u{1b}'));
        assert_eq!(event.detail.chars().count(), MAX_EVENT_DETAIL_CHARS);
    }

    #[test]
    fn a_damaged_line_costs_only_itself_and_an_unknown_kind_is_kept() {
        let (_directory, paths) = paths();
        let host = EventHost::Server(ServerId::new());
        std::fs::create_dir_all(paths.host_events_dir()).expect("dir");
        std::fs::write(
            paths.host_events_file(host),
            concat!(
                "{\"at\":\"2026-10-25T10:00:00Z\",\"kind\":\"server_started\",\"subject\":\"localhost:25565\"}\n",
                "this is not json\n",
                "{\"at\":\"2026-10-25T10:01:00Z\",\"kind\":\"from_a_newer_build\"}\n",
            ),
        )
        .expect("write");

        let events = read_host_events(&paths, host);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, HostEventKind::ServerStarted);
        assert_eq!(events[1].kind, HostEventKind::Other);
    }

    #[test]
    fn a_line_that_is_not_utf8_costs_only_itself() {
        let (_directory, paths) = paths();
        let host = EventHost::Server(ServerId::new());
        std::fs::create_dir_all(paths.host_events_dir()).expect("dir");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            b"{\"at\":\"2026-10-25T10:00:00Z\",\"kind\":\"created\",\"subject\":\"first\"}\n",
        );
        bytes.extend_from_slice(b"{\"at\":\"2026-10-25T10:00:30Z\",\"subject\":\"\xff\xfe\"}\n");
        bytes.extend_from_slice(
            b"{\"at\":\"2026-10-25T10:01:00Z\",\"kind\":\"created\",\"subject\":\"last\"}\n",
        );
        std::fs::write(paths.host_events_file(host), bytes).expect("write");

        let subjects: Vec<String> = read_host_events(&paths, host)
            .into_iter()
            .map(|event| event.subject)
            .collect();

        assert_eq!(subjects, ["first", "last"]);
    }

    #[test]
    fn text_that_reorders_or_breaks_lines_is_dropped() {
        let event = HostEvent::now(
            HostEventKind::LogError,
            "a\u{202e}b\u{2028}c\u{2066}d",
            "x\u{202a}y",
        );

        assert_eq!(event.subject, "abcd");
        assert_eq!(event.detail, "xy");
    }

    #[test]
    fn a_history_that_cannot_be_read_is_not_replaced_when_cutting_back() {
        let (_directory, paths) = paths();
        let host = EventHost::Client(InstanceId::new());
        std::fs::create_dir_all(paths.host_events_dir()).expect("dir");
        // A directory where the file belongs cannot be opened for reading as a
        // file of lines, so a cut-back has nothing it may rewrite.
        std::fs::create_dir_all(paths.host_events_file(host)).expect("a directory");

        assert!(cut_back(&paths, host).is_err());
        assert!(paths.host_events_file(host).is_dir());
    }

    #[test]
    fn writers_in_several_threads_lose_no_event() {
        let (_directory, paths) = paths();
        let host = EventHost::Client(InstanceId::new());
        let paths = std::sync::Arc::new(paths);
        let threads: Vec<_> = (0..4)
            .map(|thread| {
                let paths = std::sync::Arc::clone(&paths);
                std::thread::spawn(move || {
                    for number in 0..25 {
                        append_host_event(
                            &paths,
                            host,
                            &HostEvent::now(
                                HostEventKind::Launched,
                                &format!("{thread}-{number}"),
                                "",
                            ),
                        )
                        .expect("append");
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("a writer");
        }

        assert_eq!(read_host_events(&paths, host).len(), 100);
    }

    #[test]
    fn a_long_history_is_cut_back_to_the_newest_events() {
        let (_directory, paths) = paths();
        let host = EventHost::Client(InstanceId::new());
        let detail = "z".repeat(MAX_EVENT_DETAIL_CHARS);
        let total = usize::try_from(MAX_FILE_BYTES).unwrap() / MAX_EVENT_DETAIL_CHARS + 50;
        for number in 0..total {
            append_host_event(
                &paths,
                host,
                &HostEvent::now(HostEventKind::LogError, &number.to_string(), &detail),
            )
            .expect("append");
        }

        let events = read_host_events(&paths, host);

        assert!(events.len() <= MAX_HOST_EVENTS);
        assert_eq!(
            events.last().map(|event| event.subject.clone()),
            Some((total - 1).to_string()),
            "the newest event is kept"
        );
        assert!(
            std::fs::metadata(paths.host_events_file(host))
                .unwrap()
                .len()
                <= MAX_FILE_BYTES * 2
        );
    }

    #[test]
    fn removing_a_history_is_idempotent() {
        let (_directory, paths) = paths();
        let host = EventHost::Client(InstanceId::new());
        append_host_event(
            &paths,
            host,
            &HostEvent::now(HostEventKind::Created, "", ""),
        )
        .expect("append");

        remove_host_events(&paths, host).expect("remove");
        remove_host_events(&paths, host).expect("remove again");

        assert!(read_host_events(&paths, host).is_empty());
    }
}
