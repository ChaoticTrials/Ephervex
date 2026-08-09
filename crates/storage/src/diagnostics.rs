//! Discovery and bounded reading of the diagnostic files a launcher owns.
//!
//! Logs and crash reports are the developer feature this whole product exists
//! for, and every byte of them is untrusted: the game, its mods, and an
//! imported pack all write into these directories, and a bug being reproduced
//! is precisely the situation where the files are enormous, half-written,
//! rotated mid-read, or not valid UTF-8 at all.
//!
//! Three rules follow from that, and they are why this module exists rather
//! than a `read_to_string` at the call site.
//!
//! * **Nothing is read without a limit.** [`read_view`] takes a byte budget and
//!   never exceeds [`MAX_VIEW_BYTES`], so a 40 GiB `debug.log` produced by a
//!   crash loop costs a window, not the machine's memory.
//! * **Nothing outside the owned directory is opened.** An entry is resolved by
//!   *name* against an [`OwnedDirectory`], and a name that is not a single path
//!   component, an entry that is a symlink, or a directory that resolves outside
//!   the instance root is refused rather than followed.
//! * **Nothing is interpreted.** Text is returned as text. No line is executed,
//!   no path inside a log is opened, and no content is rewritten — a crash
//!   report that has been "cleaned up" is a crash report that lost the detail
//!   somebody needed.
//!
//! Summaries are best-effort by design. Minecraft's crash-report header is a
//! convention, not a specification, and a report whose shape is unfamiliar must
//! still be listed, opened, and searched — so [`summarize_crash_report`] returns
//! what it recognized and an excerpt of the file either way, and can fail to
//! recognize everything without failing.

use std::fmt;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};

/// The most bytes one read may return, whatever budget a caller asks for.
///
/// A ceiling rather than a suggestion: this is the value that decides whether
/// inspecting a runaway log file is a window or an out-of-memory kill.
pub const MAX_VIEW_BYTES: u64 = 4 * 1024 * 1024;

/// The default read budget, which comfortably holds the end of a normal session
/// log while staying small enough to hand to a UI on every refresh.
pub const DEFAULT_VIEW_BYTES: u64 = 512 * 1024;

/// How much of a crash report is read to summarize it.
///
/// The header Minecraft writes is at the top; the rest is the stack and the
/// system details, which a summary does not use.
pub const SUMMARY_SCAN_BYTES: u64 = 64 * 1024;

/// How many lines of a crash report the excerpt keeps.
pub const SUMMARY_EXCERPT_LINES: usize = 6;

/// How many characters one summary line keeps.
pub const SUMMARY_LINE_CHARS: usize = 240;

/// The most matches one search returns before reporting itself truncated.
pub const MAX_SEARCH_MATCHES: usize = 500;

/// How many bytes of a matching line are carried back with the match.
pub const MAX_MATCH_LINE_BYTES: usize = 2_000;

/// Failures inspecting a diagnostic file.
///
/// Structured rather than pre-formatted English: each variant maps to a
/// message id the frontend resolves through `ephervex-i18n`.
#[derive(Debug, thiserror::Error)]
pub enum DiagnosticError {
    /// The directory exists but could not be listed.
    #[error("the diagnostics directory {path} could not be read")]
    DirectoryUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file was there when it was listed and is not there now — a log that
    /// rotated, or a crash report a user deleted between refreshes.
    #[error("{path} is no longer there")]
    Missing { path: PathBuf },
    /// The file exists and could not be read.
    #[error("{path} could not be read")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The entry is not something this module will open.
    #[error("{path} is not a diagnostic file that can be opened: {reason}")]
    Refused { path: PathBuf, reason: Refusal },
}

impl DiagnosticError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::DirectoryUnreadable { .. } => "error-diagnostics-directory-unreadable",
            Self::Missing { .. } => "error-diagnostics-file-missing",
            Self::Unreadable { .. } => "error-diagnostics-file-unreadable",
            Self::Refused { .. } => "error-diagnostics-file-refused",
        }
    }

    /// The file or directory this failure is about.
    #[must_use]
    pub fn failing_path(&self) -> PathBuf {
        match self {
            Self::DirectoryUnreadable { path, .. }
            | Self::Missing { path }
            | Self::Unreadable { path, .. }
            | Self::Refused { path, .. } => path.clone(),
        }
    }
}

/// Why an entry was refused.
///
/// One user-facing message covers all of them — "this is not a file Ephervex
/// will open" is the whole story a user needs — while the specific reason stays
/// available as untranslated technical detail for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The name was not a single path component: a separator, `..`, an absolute
    /// path, or nothing at all.
    NotOneComponent,
    /// The entry is a symbolic link. Following one would let a file inside the
    /// instance decide which file outside it gets read.
    Symlink,
    /// The entry is a directory, a socket, or a device rather than a file.
    NotARegularFile,
    /// The directory itself resolves outside the root that owns it.
    OutsideOwnedRoot,
    /// This kind of diagnostic file is not one the asked-for scope keeps.
    NotOfferedHere,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotOneComponent => "not a single path component",
            Self::Symlink => "a symbolic link",
            Self::NotARegularFile => "not a regular file",
            Self::OutsideOwnedRoot => "outside the owning root",
            Self::NotOfferedHere => "not a kind of file this location keeps",
        })
    }
}

/// What kind of diagnostic file a directory holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticKind {
    /// `latest.log`, `debug.log`, and the rest of a `logs/` directory.
    Log,
    /// A `crash-reports/` entry.
    CrashReport,
}

impl DiagnosticKind {
    /// Both kinds in stable display order.
    pub const ALL: &'static [Self] = &[Self::Log, Self::CrashReport];

    /// The stable name used on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::CrashReport => "crash-report",
        }
    }

    /// Parses a command-line name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == name.trim())
    }

    /// The file extension this kind is written with.
    ///
    /// Minecraft writes logs as `.log` and crash reports as `.txt`. Rotated
    /// logs are gzipped and keep a `.gz` extension, which is deliberately not
    /// matched: a compressed file is not inspectable as text, and decompressing
    /// untrusted archive data to browse a log is a security surface this
    /// feature has no reason to open.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::CrashReport => "txt",
        }
    }

    /// Which end of the file is worth showing first.
    ///
    /// A log is appended to, so the interesting part is the end; a crash report
    /// is written once and leads with what failed.
    #[must_use]
    pub const fn natural_anchor(self) -> TextAnchor {
        match self {
            Self::Log => TextAnchor::Tail,
            Self::CrashReport => TextAnchor::Head,
        }
    }
}

impl fmt::Display for DiagnosticKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One discovered diagnostic file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    /// The file name, which is also the handle a caller passes back to open,
    /// refresh, or search it. Never a path: a frontend must not be able to name
    /// a file the launcher does not own.
    pub name: String,
    /// The full path, for display and for "copy path".
    pub path: PathBuf,
    pub kind: DiagnosticKind,
    pub size_bytes: u64,
    /// When the file was last written, where the filesystem reports it.
    pub modified: Option<DateTime<Utc>>,
    /// What the first lines of a crash report say, best-effort. `None` for logs
    /// and for a report that could not be read at listing time.
    pub summary: Option<CrashSummary>,
}

/// What a crash report's opening lines said.
///
/// Every field is optional because the format is a convention: a report written
/// by an old version, a fork, or a mod's own handler must still be listed and
/// opened. [`CrashSummary::excerpt`] is what makes that true — it is filled from
/// the first useful lines whether or not anything else was recognized.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CrashSummary {
    /// The `// ...` line Minecraft writes, kept verbatim.
    pub comment: Option<String>,
    /// The `Time:` header, exactly as written. Not parsed into a timestamp: the
    /// format has changed between versions and the file's own text is what a
    /// bug report should quote.
    pub reported_at: Option<String>,
    /// The `Description:` header.
    pub description: Option<String>,
    /// The first line that looks like the failure itself, typically the
    /// exception and its message.
    pub failure: Option<String>,
    /// The first useful lines of the file, bounded.
    pub excerpt: Vec<String>,
}

impl CrashSummary {
    /// Whether nothing at all was recognized and the file had no usable text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.comment.is_none()
            && self.reported_at.is_none()
            && self.description.is_none()
            && self.failure.is_none()
            && self.excerpt.is_empty()
    }

    /// The one line worth showing beside the file name in a list.
    #[must_use]
    pub fn headline(&self) -> Option<&str> {
        self.description
            .as_deref()
            .or(self.failure.as_deref())
            .or_else(|| self.excerpt.first().map(String::as_str))
    }
}

/// Which end of a file a read window is taken from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAnchor {
    /// The end of the file — what a log is normally inspected for.
    #[default]
    Tail,
    /// The beginning of the file.
    Head,
}

impl TextAnchor {
    /// The stable name used on the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tail => "tail",
            Self::Head => "head",
        }
    }
}

impl fmt::Display for TextAnchor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A bounded window of one diagnostic file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextView {
    pub path: PathBuf,
    pub anchor: TextAnchor,
    /// The window's text. Invalid bytes are replaced rather than rejected, so a
    /// log containing one bad byte stays inspectable.
    pub text: String,
    /// The file's size when it was opened.
    pub file_bytes: u64,
    /// How many bytes this window covers.
    pub returned_bytes: u64,
    /// Whether the file is larger than the window.
    pub truncated: bool,
    /// Whether bytes had to be replaced to make text of it.
    pub lossy: bool,
    /// When the window was taken, so a frontend can show what a refresh is
    /// relative to.
    pub read_at: DateTime<Utc>,
}

/// How a search over a [`TextView`] behaves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    /// How many matches to return before reporting the result truncated.
    pub limit: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            limit: MAX_SEARCH_MATCHES,
        }
    }
}

/// One match inside a searched window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// 1-based, counted within the window rather than within the file: a tail
    /// window does not know which line of the file it starts at, and claiming
    /// otherwise would be worse than not saying.
    pub line_number: usize,
    /// The matching line, truncated to [`MAX_MATCH_LINE_BYTES`].
    pub line: String,
    /// Whether [`SearchMatch::line`] is shorter than the line in the file.
    pub line_truncated: bool,
    /// Byte offsets of the match within the *original* line.
    pub start: usize,
    pub end: usize,
}

/// What a search found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchOutcome {
    pub matches: Vec<SearchMatch>,
    /// Whether the limit stopped the search before the window ended.
    pub truncated: bool,
    pub lines_searched: usize,
}

/// A directory of diagnostic files, and the root that is allowed to own it.
///
/// The root is carried because the directory alone cannot answer the question
/// that matters: `<instance>/minecraft/logs` is only trustworthy while it still
/// resolves inside `<instance>`. Replacing it with a link to `/var/log` must
/// make every operation on it fail rather than quietly succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDirectory {
    root: PathBuf,
    directory: PathBuf,
    kind: DiagnosticKind,
}

impl OwnedDirectory {
    /// Declares `directory` to be the `kind` directory owned by `root`.
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        directory: impl Into<PathBuf>,
        kind: DiagnosticKind,
    ) -> Self {
        Self {
            root: root.into(),
            directory: directory.into(),
            kind,
        }
    }

    /// The root this directory must stay inside.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the directory is, whether or not it exists yet.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub const fn kind(&self) -> DiagnosticKind {
        self.kind
    }

    /// Every diagnostic file in the directory, newest first.
    ///
    /// A directory that does not exist yields nothing rather than failing: an
    /// instance that has never been launched has no `logs/`, and that is a
    /// normal state to render, not an error to report.
    ///
    /// Entries that are not regular files, and entries that are symlinks, are
    /// skipped — see the module docs for why following one is refused.
    pub fn entries(&self) -> Result<Vec<DiagnosticEntry>, DiagnosticError> {
        let Some(directory) = self.resolved_directory()? else {
            return Ok(Vec::new());
        };

        let listing = std::fs::read_dir(&directory).map_err(|source| {
            DiagnosticError::DirectoryUnreadable {
                path: directory.clone(),
                source,
            }
        })?;

        let mut entries = Vec::new();
        for entry in listing {
            let entry = entry.map_err(|source| DiagnosticError::DirectoryUnreadable {
                path: directory.clone(),
                source,
            })?;
            // `file_type` on the directory entry does not follow links, so a
            // symlink is recognized as one rather than as whatever it points at.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = entry.path();
            if !has_extension(&path, self.kind.extension()) {
                continue;
            }

            let metadata = entry.metadata().ok();
            entries.push(DiagnosticEntry {
                name: name.to_owned(),
                summary: match self.kind {
                    DiagnosticKind::Log => None,
                    DiagnosticKind::CrashReport => Some(summarize_crash_file(&path)),
                },
                size_bytes: metadata.as_ref().map_or(0, std::fs::Metadata::len),
                modified: metadata
                    .as_ref()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(DateTime::<Utc>::from),
                kind: self.kind,
                path,
            });
        }

        entries.sort_by(newest_first);
        Ok(entries)
    }

    /// Turns one entry name back into a path that is safe to open.
    ///
    /// This is the only way in: a caller names a file, and this decides whether
    /// that name refers to something the launcher owns. Re-checked on every
    /// call rather than trusted from an earlier listing, because the file may
    /// have been replaced by a link in between.
    pub fn resolve(&self, name: &str) -> Result<PathBuf, DiagnosticError> {
        let Some(directory) = self.resolved_directory()? else {
            return Err(DiagnosticError::Missing {
                path: self.directory.join(name),
            });
        };
        if !is_single_component(name) {
            return Err(DiagnosticError::Refused {
                path: self.directory.join(name),
                reason: Refusal::NotOneComponent,
            });
        }

        let path = directory.join(name);
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                DiagnosticError::Missing { path: path.clone() }
            } else {
                DiagnosticError::Unreadable {
                    path: path.clone(),
                    source,
                }
            }
        })?;
        if metadata.is_symlink() {
            return Err(DiagnosticError::Refused {
                path,
                reason: Refusal::Symlink,
            });
        }
        if !metadata.is_file() {
            return Err(DiagnosticError::Refused {
                path,
                reason: Refusal::NotARegularFile,
            });
        }
        Ok(path)
    }

    /// The directory as it actually resolves, or `None` when it does not exist.
    ///
    /// Both sides are canonicalized before they are compared, so a root reached
    /// through a link is not mistaken for an escape, and an escape through a
    /// link is not mistaken for the root.
    fn resolved_directory(&self) -> Result<Option<PathBuf>, DiagnosticError> {
        let root = match std::fs::canonicalize(&self.root) {
            Ok(root) => root,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(DiagnosticError::Missing {
                    path: self.root.clone(),
                });
            }
            Err(source) => {
                return Err(DiagnosticError::DirectoryUnreadable {
                    path: self.root.clone(),
                    source,
                });
            }
        };
        let directory = match std::fs::canonicalize(&self.directory) {
            Ok(directory) => directory,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(DiagnosticError::DirectoryUnreadable {
                    path: self.directory.clone(),
                    source,
                });
            }
        };
        if !directory.starts_with(&root) {
            return Err(DiagnosticError::Refused {
                path: self.directory.clone(),
                reason: Refusal::OutsideOwnedRoot,
            });
        }
        if !directory.is_dir() {
            return Err(DiagnosticError::Refused {
                path: self.directory.clone(),
                reason: Refusal::NotARegularFile,
            });
        }
        Ok(Some(directory))
    }
}

/// Reads a bounded window of a file's text.
///
/// `budget` is clamped to [`MAX_VIEW_BYTES`], so no caller — including a future
/// frontend that passes a number straight from a text field — can ask for a
/// file to be loaded whole.
///
/// A tail window starts at the first line boundary inside it, so the first line
/// shown is a whole line rather than the middle of one. A file that shrank,
/// rotated, or is still being written between the open and the read yields the
/// bytes that were actually there; it is not an error.
pub fn read_view(
    path: &Path,
    anchor: TextAnchor,
    budget: u64,
) -> Result<TextView, DiagnosticError> {
    let budget = budget.clamp(1, MAX_VIEW_BYTES);
    let mut file = std::fs::File::open(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            DiagnosticError::Missing {
                path: path.to_path_buf(),
            }
        } else {
            DiagnosticError::Unreadable {
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    // Taken from the open handle rather than from the path: a file replaced
    // between the two would otherwise be measured and read as two files.
    let file_bytes = file
        .metadata()
        .map_err(|source| DiagnosticError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?
        .len();

    let mut skipped_prefix = false;
    if anchor == TextAnchor::Tail && file_bytes > budget {
        file.seek(SeekFrom::Start(file_bytes - budget))
            .map_err(|source| DiagnosticError::Unreadable {
                path: path.to_path_buf(),
                source,
            })?;
        skipped_prefix = true;
    }

    let mut bytes = Vec::new();
    (&mut file)
        .take(budget)
        .read_to_end(&mut bytes)
        .map_err(|source| DiagnosticError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;

    let mut window: &[u8] = &bytes;
    if skipped_prefix {
        // A window that starts mid-line would also start mid-character. Cutting
        // at the first newline fixes both at once.
        if let Some(position) = window.iter().position(|byte| *byte == b'\n') {
            window = &window[position + 1..];
        }
    }
    // A tail window always runs to the end of the file; a head window does only
    // when the whole file fit inside the budget.
    let ends_at_file_end =
        skipped_prefix || u64::try_from(bytes.len()).unwrap_or(u64::MAX) >= file_bytes;
    if !ends_at_file_end {
        // The far end of a window can land inside a character. Dropping the
        // partial sequence keeps `lossy` meaning "the file has bad bytes"
        // rather than "the window ended".
        window = trim_partial_character(window);
    }

    let text = String::from_utf8_lossy(window);
    let lossy = matches!(text, std::borrow::Cow::Owned(_));
    let returned_bytes = u64::try_from(window.len()).unwrap_or(u64::MAX);

    Ok(TextView {
        path: path.to_path_buf(),
        anchor,
        text: text.into_owned(),
        file_bytes,
        returned_bytes,
        truncated: returned_bytes < file_bytes,
        lossy,
        read_at: Utc::now(),
    })
}

/// Finds `query` in already-read text.
///
/// Operates on a [`TextView`]'s text rather than on a path: a search must never
/// be a reason to read a file again, and it must never be able to write to one.
#[must_use]
pub fn search(text: &str, query: &str, options: &SearchOptions) -> SearchOutcome {
    let mut outcome = SearchOutcome::default();
    if query.is_empty() {
        outcome.lines_searched = text.lines().count();
        return outcome;
    }

    for (index, line) in text.lines().enumerate() {
        outcome.lines_searched = index + 1;
        let mut from = 0;
        while let Some((start, end)) = find_from(line, query, options.case_sensitive, from) {
            if outcome.matches.len() >= options.limit {
                outcome.truncated = true;
                return outcome;
            }
            let (kept, line_truncated) = truncate_chars_to_bytes(line, MAX_MATCH_LINE_BYTES);
            outcome.matches.push(SearchMatch {
                line_number: index + 1,
                line: kept.to_owned(),
                line_truncated,
                start,
                end,
            });
            // `end` is past the match and on a character boundary, so a query
            // that can overlap itself still advances and cannot loop.
            from = end;
            if from >= line.len() {
                break;
            }
        }
    }
    outcome
}

/// Reads the head of a crash report and summarizes it.
///
/// Never fails: a report that cannot be read summarizes as empty, and stays
/// listed and openable.
#[must_use]
pub fn summarize_crash_file(path: &Path) -> CrashSummary {
    read_view(path, TextAnchor::Head, SUMMARY_SCAN_BYTES)
        .map(|view| summarize_crash_report(&view.text))
        .unwrap_or_default()
}

/// Extracts what a crash report's opening lines say.
///
/// Recognizes the headers Minecraft writes — the `// ...` comment, `Time:`, and
/// `Description:` — and takes the failure line to be the first line that is not
/// a banner, a header, a comment, or a stack frame. Anything unrecognized is
/// left as `None`; the excerpt is filled regardless, which is what keeps an
/// unfamiliar report inspectable instead of blank.
#[must_use]
pub fn summarize_crash_report(text: &str) -> CrashSummary {
    let mut summary = CrashSummary::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(comment) = trimmed.strip_prefix("//") {
            summary.comment.get_or_insert_with(|| clip(comment.trim()));
        } else if let Some(value) = strip_header(trimmed, "Time:") {
            summary.reported_at.get_or_insert_with(|| clip(value));
        } else if let Some(value) = strip_header(trimmed, "Description:") {
            summary.description.get_or_insert_with(|| clip(value));
        } else if summary.failure.is_none() && is_failure_line(trimmed) {
            summary.failure = Some(clip(trimmed));
        }

        if summary.excerpt.len() < SUMMARY_EXCERPT_LINES && !is_banner(trimmed) {
            summary.excerpt.push(clip(trimmed));
        }
    }
    summary
}

/// Newest first, with the file name as a stable tie-break.
///
/// Modification times collide routinely — a crash and the log line that
/// describes it are written in the same second, and some filesystems report
/// whole seconds — so an ordering that stops there is not reproducible between
/// two listings of the same directory. Names descend because Minecraft's own
/// names lead with a timestamp, which makes the tie-break agree with the
/// primary key rather than fight it.
fn newest_first(left: &DiagnosticEntry, right: &DiagnosticEntry) -> std::cmp::Ordering {
    right
        .modified
        .cmp(&left.modified)
        .then_with(|| right.name.cmp(&left.name))
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// Whether a name refers to one entry of a directory and nothing else.
///
/// Rejects `..`, `.`, an empty name, an absolute path, and anything containing a
/// separator — the four shapes that turn "open this log" into "open any file".
fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    let Some(Component::Normal(single)) = components.next() else {
        return false;
    };
    components.next().is_none() && single == std::ffi::OsStr::new(name)
}

/// Drops an incomplete UTF-8 sequence from the end of a window.
fn trim_partial_character(bytes: &[u8]) -> &[u8] {
    // `error_len() == None` means the input ended mid-character rather than
    // containing an invalid one; only that case is the window's fault, and
    // everything else is returned untouched for the lossy conversion to report.
    match std::str::from_utf8(bytes) {
        Err(error) if error.error_len().is_none() => &bytes[..error.valid_up_to()],
        _ => bytes,
    }
}

fn strip_header<'a>(line: &'a str, header: &str) -> Option<&'a str> {
    let rest = line.get(..header.len())?;
    rest.eq_ignore_ascii_case(header)
        .then(|| line[header.len()..].trim())
}

/// Whether a line looks like the failure itself rather than the report's frame.
fn is_failure_line(line: &str) -> bool {
    !(is_banner(line)
        || line.starts_with("//")
        || line.starts_with("at ")
        || is_context_header(line))
}

/// Whether a line is one of the report's `Key: value` headers.
///
/// Recognized by the shape of the key rather than by a list of known headers,
/// so a Minecraft version that adds one does not turn its header into the
/// failure line. A qualified exception name fails the test because of its dots,
/// which is exactly the distinction wanted.
fn is_context_header(line: &str) -> bool {
    let Some((key, _)) = line.split_once(':') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphabetic() || character == ' ')
}

/// Whether a line is the `---- Minecraft Crash Report ----` style banner.
fn is_banner(line: &str) -> bool {
    line.starts_with("----") || line.chars().all(|character| character == '-')
}

/// Truncates to at most `limit` characters, on a character boundary.
fn clip(text: &str) -> String {
    text.chars().take(SUMMARY_LINE_CHARS).collect()
}

/// Truncates to at most `limit` bytes without splitting a character.
fn truncate_chars_to_bytes(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Finds `needle` in `haystack` at or after `from`.
///
/// Case-insensitive matching folds character by character rather than
/// lowercasing the whole line, because lowercasing can change a string's length
/// and the offsets returned here must index the line the caller has.
fn find_from(
    haystack: &str,
    needle: &str,
    case_sensitive: bool,
    from: usize,
) -> Option<(usize, usize)> {
    if case_sensitive {
        return haystack
            .get(from..)
            .and_then(|rest| rest.find(needle))
            .map(|offset| (from + offset, from + offset + needle.len()));
    }
    let rest = haystack.get(from..)?;
    for (offset, _) in rest.char_indices() {
        if let Some(length) = match_length(&rest[offset..], needle) {
            return Some((from + offset, from + offset + length));
        }
    }
    None
}

/// The byte length of a case-insensitive match of `needle` at the start of
/// `text`, or `None` if it does not match there.
fn match_length(text: &str, needle: &str) -> Option<usize> {
    let mut consumed = 0;
    let mut wanted = needle.chars();
    let mut found = text.chars();
    loop {
        let Some(expected) = wanted.next() else {
            return Some(consumed);
        };
        let actual = found.next()?;
        if !expected.to_lowercase().eq(actual.to_lowercase()) {
            return None;
        }
        consumed += actual.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn write(path: &Path, body: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn a_name_that_is_not_one_component_is_refused_before_anything_is_opened() {
        for name in [
            "..",
            ".",
            "",
            "../../etc/passwd",
            "logs/latest.log",
            "/etc/passwd",
        ] {
            assert!(!is_single_component(name), "{name} must be refused");
        }
        assert!(is_single_component("latest.log"));
        assert!(is_single_component("crash-2026-08-09_10.30.00-client.txt"));
    }

    #[test]
    fn a_window_never_exceeds_the_ceiling_however_large_a_budget_is_asked_for() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("huge.log");
        write(&path, &vec![b'x'; 4096]);

        let view = read_view(&path, TextAnchor::Head, u64::MAX).expect("read");
        assert_eq!(view.returned_bytes, 4096, "a small file reads whole");
        assert!(!view.truncated);
    }

    #[test]
    fn a_tail_window_starts_on_a_line_boundary_and_reports_itself_truncated() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("latest.log");
        let body: String = (0..500).fold(String::new(), |mut body, index| {
            use std::fmt::Write as _;
            let _ = writeln!(body, "line {index}");
            body
        });
        write(&path, body.as_bytes());

        let view = read_view(&path, TextAnchor::Tail, 64).expect("read");
        assert!(view.truncated);
        assert!(view.returned_bytes <= 64);
        assert!(
            view.text.starts_with("line "),
            "a window must not start mid-line: {:?}",
            view.text
        );
        assert!(view.text.ends_with("line 499\n"));
    }

    #[test]
    fn invalid_bytes_are_reported_and_do_not_prevent_inspection() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("broken.log");
        write(&path, b"before \xff\xfe after\n");

        let view = read_view(&path, TextAnchor::Head, DEFAULT_VIEW_BYTES).expect("read");
        assert!(view.lossy);
        assert!(view.text.contains("before"));
        assert!(view.text.contains("after"));
    }

    #[test]
    fn a_window_ending_inside_a_character_is_not_mistaken_for_invalid_bytes() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("unicode.log");
        write(&path, "ααααααα\n".as_bytes());

        let view = read_view(&path, TextAnchor::Head, 5).expect("read");
        assert!(!view.lossy, "the file's bytes are valid; the window ended");
        assert_eq!(view.text, "αα");
        assert!(view.truncated);
    }

    #[test]
    fn an_empty_file_reads_as_empty_rather_than_failing() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("empty.log");
        write(&path, b"");

        let view = read_view(&path, TextAnchor::Tail, DEFAULT_VIEW_BYTES).expect("read");
        assert_eq!(view.text, "");
        assert_eq!(view.file_bytes, 0);
        assert!(!view.truncated);
        assert!(!view.lossy);
    }

    #[test]
    fn a_file_that_disappeared_reports_that_rather_than_a_generic_failure() {
        let base = tempfile::tempdir().expect("temp dir");
        let error = read_view(
            &base.path().join("gone.log"),
            TextAnchor::Tail,
            DEFAULT_VIEW_BYTES,
        )
        .expect_err("nothing to read");
        assert!(matches!(error, DiagnosticError::Missing { .. }));
        assert_eq!(error.message_id(), "error-diagnostics-file-missing");
    }

    #[test]
    fn entries_are_newest_first_with_the_name_breaking_ties() {
        let older = DiagnosticEntry {
            name: "crash-a.txt".to_owned(),
            path: PathBuf::from("/crash-a.txt"),
            kind: DiagnosticKind::CrashReport,
            size_bytes: 0,
            modified: Some(DateTime::from_timestamp(1_000, 0).expect("timestamp")),
            summary: None,
        };
        let same_time_later_name = DiagnosticEntry {
            name: "crash-b.txt".to_owned(),
            ..older.clone()
        };
        let newer = DiagnosticEntry {
            name: "crash-c.txt".to_owned(),
            modified: Some(DateTime::from_timestamp(2_000, 0).expect("timestamp")),
            ..older.clone()
        };

        let mut entries = [older.clone(), newer.clone(), same_time_later_name.clone()];
        entries.sort_by(newest_first);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["crash-c.txt", "crash-b.txt", "crash-a.txt"]
        );

        // Deterministic: a different input order produces the same output.
        let mut shuffled = [same_time_later_name, newer, older];
        shuffled.sort_by(newest_first);
        assert_eq!(
            shuffled
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["crash-c.txt", "crash-b.txt", "crash-a.txt"]
        );
    }

    #[test]
    fn a_familiar_crash_report_yields_its_comment_time_description_and_failure() {
        let summary = summarize_crash_report(
            "---- Minecraft Crash Report ----\n\
             // Everything's going to plan. No, really, that was supposed to happen.\n\
             \n\
             Time: 2026-08-09 10:30:00\n\
             Description: Ticking entity\n\
             \n\
             java.lang.NullPointerException: Ticking entity\n\
             \tat net.minecraft.world.entity.Entity.tick(Entity.java:100)\n",
        );
        assert_eq!(
            summary.comment.as_deref(),
            Some("Everything's going to plan. No, really, that was supposed to happen.")
        );
        assert_eq!(summary.reported_at.as_deref(), Some("2026-08-09 10:30:00"));
        assert_eq!(summary.description.as_deref(), Some("Ticking entity"));
        assert_eq!(
            summary.failure.as_deref(),
            Some("java.lang.NullPointerException: Ticking entity")
        );
        assert_eq!(summary.headline(), Some("Ticking entity"));
    }

    #[test]
    fn an_unfamiliar_report_still_summarizes_to_something_useful() {
        let summary = summarize_crash_report("a mod wrote this\nand then this\n");
        assert!(summary.description.is_none());
        assert!(!summary.is_empty(), "an excerpt is always available");
        assert_eq!(summary.headline(), Some("a mod wrote this"));
        assert_eq!(summary.excerpt.len(), 2);
    }

    #[test]
    fn an_empty_report_summarizes_to_nothing_without_failing() {
        assert!(summarize_crash_report("").is_empty());
        assert!(summarize_crash_report("\n\n\n").is_empty());
    }

    #[test]
    fn search_reports_matches_with_offsets_into_the_line_it_returns() {
        let outcome = search(
            "first line\nsecond LINE\nthird\n",
            "line",
            &SearchOptions::default(),
        );
        assert_eq!(outcome.matches.len(), 2);
        assert_eq!(outcome.matches[0].line_number, 1);
        assert_eq!(outcome.matches[1].line_number, 2);
        let second = &outcome.matches[1];
        assert_eq!(&second.line[second.start..second.end], "LINE");
        assert!(!outcome.truncated);
    }

    #[test]
    fn a_case_sensitive_search_finds_only_what_was_typed() {
        let options = SearchOptions {
            case_sensitive: true,
            ..SearchOptions::default()
        };
        let outcome = search("Error\nerror\n", "error", &options);
        assert_eq!(outcome.matches.len(), 1);
        assert_eq!(outcome.matches[0].line_number, 2);
    }

    #[test]
    fn a_search_with_no_matches_says_so_rather_than_failing() {
        let outcome = search("nothing here\n", "exception", &SearchOptions::default());
        assert!(outcome.matches.is_empty());
        assert!(!outcome.truncated);
        assert_eq!(outcome.lines_searched, 1);
    }

    #[test]
    fn a_search_stops_at_its_limit_and_says_it_was_truncated() {
        let text = "match\n".repeat(10);
        let outcome = search(
            &text,
            "match",
            &SearchOptions {
                case_sensitive: false,
                limit: 3,
            },
        );
        assert_eq!(outcome.matches.len(), 3);
        assert!(outcome.truncated);
    }

    #[test]
    fn a_search_over_a_line_with_several_matches_advances_past_each_one() {
        let outcome = search("aaa\n", "a", &SearchOptions::default());
        assert_eq!(outcome.matches.len(), 3);
        assert_eq!(
            outcome
                .matches
                .iter()
                .map(|found| found.start)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        let outcome = search("some text\n", "", &SearchOptions::default());
        assert!(outcome.matches.is_empty());
    }

    #[test]
    fn a_case_insensitive_match_keeps_offsets_that_index_the_original_line() {
        // Turkish dotted capital I lowercases to two characters; folding the
        // whole line would move every offset after it.
        let line = "İstanbul error\n";
        let outcome = search(line, "ERROR", &SearchOptions::default());
        let found = outcome.matches.first().expect("a match");
        assert_eq!(&found.line[found.start..found.end], "error");
    }
}
