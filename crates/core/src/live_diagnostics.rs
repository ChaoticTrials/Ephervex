//! Shared presentation logic for live Minecraft logs and server output.
//!
//! Sources keep their own storage and capture behavior. This module owns the
//! bounded, normalized view a frontend can filter, search, and render.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
pub use ephervex_server::CaptureStream;
use ephervex_server::CapturedLine;

/// Default number of records retained by a live viewer.
pub const DEFAULT_RETAINED_RECORDS: usize = 4_000;
/// Default text budget retained by a live viewer.
pub const DEFAULT_RETAINED_BYTES: usize = 4 * 1024 * 1024;
/// Default amount read from the end of a log when tailing begins.
pub const DEFAULT_INITIAL_TAIL_BYTES: usize = 512 * 1024;
/// Default maximum amount read during one incremental poll.
pub const DEFAULT_TAIL_POLL_BYTES: usize = 256 * 1024;
/// Default maximum retained prefix of one unterminated physical line.
pub const DEFAULT_PARTIAL_LINE_BYTES: usize = 256 * 1024;
const TAIL_BOUNDARY_BYTES: usize = 64;

/// A reliably recognized Minecraft or Log4j severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    /// Output without a recognized logger level.
    Other,
}

impl LogLevel {
    pub const ALL: [Self; 7] = [
        Self::Trace,
        Self::Debug,
        Self::Info,
        Self::Warn,
        Self::Error,
        Self::Fatal,
        Self::Other,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Fatal => "fatal",
            Self::Other => "other",
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::Trace => 1 << 0,
            Self::Debug => 1 << 1,
            Self::Info => 1 << 2,
            Self::Warn => 1 << 3,
            Self::Error => 1 << 4,
            Self::Fatal => 1 << 5,
            Self::Other => 1 << 6,
        }
    }
}

/// Session-local severity selection for a live viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogFilter {
    enabled: u8,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self::all()
    }
}

impl LogFilter {
    const ALL_BITS: u8 = (1 << LogLevel::ALL.len()) - 1;

    #[must_use]
    pub const fn all() -> Self {
        Self {
            enabled: Self::ALL_BITS,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self { enabled: 0 }
    }

    #[must_use]
    pub const fn is_all(self) -> bool {
        self.enabled == Self::ALL_BITS
    }

    #[must_use]
    pub const fn is_enabled(self, level: LogLevel) -> bool {
        self.enabled & level.bit() != 0
    }

    pub const fn set(&mut self, level: LogLevel, enabled: bool) {
        if enabled {
            self.enabled |= level.bit();
        } else {
            self.enabled &= !level.bit();
        }
    }

    pub const fn set_all(&mut self, enabled: bool) {
        self.enabled = if enabled { Self::ALL_BITS } else { 0 };
    }

    pub const fn toggle(&mut self, level: LogLevel) {
        self.enabled ^= level.bit();
    }

    #[must_use]
    pub const fn includes(self, record: &LogRecord) -> bool {
        self.is_enabled(record.level)
    }
}

/// Process-capture details kept separate from Minecraft's own message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportMetadata {
    pub timestamp: DateTime<Utc>,
    pub stream: CaptureStream,
}

/// Details about content omitted before it reached the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncationMetadata {
    pub omitted_bytes: usize,
}

/// One filterable log event, including recognized continuation lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Normalized display content without launcher transport decoration.
    pub text: String,
    pub level: LogLevel,
    pub transport: Option<TransportMetadata>,
    pub truncation: Option<TruncationMetadata>,
}

impl LogRecord {
    #[must_use]
    pub fn from_line(text: impl Into<String>) -> Self {
        let text = normalize_physical_line(text.into());
        Self {
            level: parse_log_level(&text),
            text,
            transport: None,
            truncation: None,
        }
    }

    #[must_use]
    pub fn from_captured(line: CapturedLine) -> Self {
        let text = normalize_physical_line(line.text);
        Self {
            level: parse_log_level(&text),
            text,
            transport: Some(TransportMetadata {
                timestamp: line.timestamp,
                stream: line.stream,
            }),
            truncation: (line.truncated || line.omitted_bytes > 0).then_some(TruncationMetadata {
                omitted_bytes: line.omitted_bytes,
            }),
        }
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.text.len()
    }
}

/// Parses severity only from bracketed logger fields.
#[must_use]
pub fn parse_log_level(text: &str) -> LogLevel {
    let mut remainder = text.trim_start();
    let mut groups = 0;

    while let Some(after_open) = remainder.strip_prefix('[') {
        let Some(close) = after_open.find(']') else {
            break;
        };
        let field = &after_open[..close];
        groups += 1;

        if let Some(level) = level_from_logger_field(field, groups) {
            return level;
        }

        remainder = after_open[close + 1..].trim_start();
    }

    LogLevel::Other
}

fn level_from_logger_field(field: &str, group: usize) -> Option<LogLevel> {
    let candidate = field.rsplit('/').next().unwrap_or(field).trim();
    let has_thread = candidate.len() != field.trim().len();

    if !has_thread && group == 1 && field.contains(':') {
        return None;
    }

    if candidate.eq_ignore_ascii_case("TRACE") {
        Some(LogLevel::Trace)
    } else if candidate.eq_ignore_ascii_case("DEBUG") {
        Some(LogLevel::Debug)
    } else if candidate.eq_ignore_ascii_case("INFO") {
        Some(LogLevel::Info)
    } else if candidate.eq_ignore_ascii_case("WARN") {
        Some(LogLevel::Warn)
    } else if candidate.eq_ignore_ascii_case("ERROR") {
        Some(LogLevel::Error)
    } else if candidate.eq_ignore_ascii_case("FATAL") {
        Some(LogLevel::Fatal)
    } else {
        None
    }
}

/// A bounded set of parsed records shared by file and process sources.
#[derive(Debug, Clone)]
pub struct RetainedLog {
    records: VecDeque<LogRecord>,
    retained_bytes: usize,
    max_records: usize,
    max_bytes: usize,
    dropped_records: u64,
}

impl RetainedLog {
    #[must_use]
    pub fn new(max_records: usize, max_bytes: usize) -> Self {
        Self {
            records: VecDeque::new(),
            retained_bytes: 0,
            max_records,
            max_bytes,
            dropped_records: 0,
        }
    }

    #[must_use]
    pub fn with_default_limits() -> Self {
        Self::new(DEFAULT_RETAINED_RECORDS, DEFAULT_RETAINED_BYTES)
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.retained_bytes = 0;
        self.dropped_records = 0;
    }

    pub fn append_line(&mut self, text: impl Into<String>) {
        self.append_record(LogRecord::from_line(text));
    }

    pub fn append_tailed_line(&mut self, line: TailedLine) {
        let mut record = LogRecord::from_line(line.text);
        record.truncation = line.truncation;
        self.append_record(record);
    }

    pub fn append_captured(&mut self, line: CapturedLine) {
        self.append_record(LogRecord::from_captured(line));
    }

    pub fn append_record(&mut self, record: LogRecord) {
        if self.max_records == 0 || self.max_bytes == 0 {
            self.dropped_records = self.dropped_records.saturating_add(1);
            return;
        }

        if record.level == LogLevel::Other
            && self
                .records
                .back()
                .is_some_and(|previous| is_continuation(&record.text, previous))
        {
            self.append_continuation(record);
        } else {
            self.retained_bytes = self.retained_bytes.saturating_add(record.memory_bytes());
            self.records.push_back(record);
        }
        self.enforce_limits();
    }

    #[must_use]
    pub fn records(&self) -> impl ExactSizeIterator<Item = &LogRecord> {
        self.records.iter()
    }

    pub fn filtered_records<'a>(
        &'a self,
        filter: &'a LogFilter,
    ) -> impl Iterator<Item = &'a LogRecord> + 'a {
        self.records
            .iter()
            .filter(move |record| filter.includes(record))
    }

    #[must_use]
    pub fn text(&self) -> String {
        join_record_text(self.records.iter())
    }

    #[must_use]
    pub fn filtered_text(&self, filter: &LogFilter) -> String {
        join_record_text(self.filtered_records(filter))
    }

    /// Searches every retained record, independent of the active level filter.
    #[must_use]
    pub fn search<'a>(&'a self, query: &str) -> Vec<&'a LogRecord> {
        if query.is_empty() {
            return Vec::new();
        }
        self.records
            .iter()
            .filter(|record| record.text.contains(query))
            .collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    #[must_use]
    pub const fn dropped_records(&self) -> u64 {
        self.dropped_records
    }

    #[must_use]
    pub fn history_truncated(&self) -> bool {
        self.dropped_records > 0
            || self
                .records
                .iter()
                .any(|record| record.truncation.is_some())
    }

    fn append_continuation(&mut self, continuation: LogRecord) {
        let Some(previous) = self.records.back_mut() else {
            return;
        };
        let LogRecord {
            text, truncation, ..
        } = continuation;
        previous.text.push('\n');
        previous.text.push_str(&text);
        self.retained_bytes = self.retained_bytes.saturating_add(1 + text.len());
        merge_truncation(&mut previous.truncation, truncation);
    }

    fn enforce_limits(&mut self) {
        while self.records.len() > self.max_records
            || (self.retained_bytes > self.max_bytes && self.records.len() > 1)
        {
            if let Some(removed) = self.records.pop_front() {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.memory_bytes());
                self.dropped_records = self.dropped_records.saturating_add(1);
            }
        }

        let Some(record) = self.records.front_mut() else {
            return;
        };
        if record.text.len() <= self.max_bytes {
            return;
        }

        let original_len = record.text.len();
        let mut kept = self.max_bytes.min(original_len);
        while kept > 0 && !record.text.is_char_boundary(kept) {
            kept -= 1;
        }
        record.text.truncate(kept);
        self.retained_bytes = kept;
        merge_truncation(
            &mut record.truncation,
            Some(TruncationMetadata {
                omitted_bytes: original_len - kept,
            }),
        );
    }
}

impl Default for RetainedLog {
    fn default() -> Self {
        Self::with_default_limits()
    }
}

fn join_record_text<'a>(records: impl Iterator<Item = &'a LogRecord>) -> String {
    let mut output = String::new();
    for record in records {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&record.text);
    }
    output
}

fn merge_truncation(
    target: &mut Option<TruncationMetadata>,
    additional: Option<TruncationMetadata>,
) {
    if let Some(additional) = additional {
        if let Some(existing) = target {
            existing.omitted_bytes = existing
                .omitted_bytes
                .saturating_add(additional.omitted_bytes);
        } else {
            *target = Some(additional);
        }
    }
}

fn normalize_physical_line(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

fn is_continuation(text: &str, previous: &LogRecord) -> bool {
    let trimmed = text.trim_start();
    let indented = trimmed.len() != text.len();

    (indented
        && (trimmed.starts_with("at ")
            || trimmed.starts_with("... ")
            || trimmed.starts_with("Caused by:")
            || trimmed.starts_with("Suppressed:")
            || trimmed.starts_with("- locked")
            || trimmed.starts_with("- waiting")
            || trimmed.starts_with("- parking")))
        || trimmed.starts_with("Caused by:")
        || trimmed.starts_with("Suppressed:")
        || (matches!(previous.level, LogLevel::Error | LogLevel::Fatal)
            && looks_like_throwable_header(trimmed))
        || (trimmed.is_empty()
            && matches!(previous.level, LogLevel::Error | LogLevel::Fatal)
            && previous.text.contains('\n'))
}

fn looks_like_throwable_header(text: &str) -> bool {
    if text.starts_with("Exception in thread \"") {
        return true;
    }
    let class = text.split_once(':').map_or(text, |(class, _)| class);
    !class.contains(char::is_whitespace)
        && class.contains('.')
        && class.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '$' | '_')
        })
        && (class.ends_with("Exception")
            || class.ends_with("Error")
            || class.ends_with("Throwable"))
}

/// Limits for the incremental file tailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailLimits {
    pub initial_bytes: usize,
    pub poll_bytes: usize,
    pub partial_line_bytes: usize,
}

impl Default for TailLimits {
    fn default() -> Self {
        Self {
            initial_bytes: DEFAULT_INITIAL_TAIL_BYTES,
            poll_bytes: DEFAULT_TAIL_POLL_BYTES,
            partial_line_bytes: DEFAULT_PARTIAL_LINE_BYTES,
        }
    }
}

/// One completed physical line from an incremental tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailedLine {
    pub text: String,
    pub truncation: Option<TruncationMetadata>,
}

/// Changes observed during one tail poll.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TailUpdate {
    /// The file was truncated or replaced and the previous view must be reset.
    pub reset: bool,
    /// The file does not currently exist. This is expected around launches.
    pub missing: bool,
    /// Bytes omitted before the retained initial window or its first full line.
    pub skipped_bytes: u64,
    pub lines: Vec<TailedLine>,
}

/// Incrementally follows one path without repeatedly reading the whole file.
#[derive(Debug)]
pub struct IncrementalLogTail {
    path: PathBuf,
    limits: TailLimits,
    identity: Option<FileIdentity>,
    offset: u64,
    boundary_start: u64,
    boundary: Vec<u8>,
    pending: Vec<u8>,
    pending_omitted: usize,
    discard_initial_partial: bool,
}

impl IncrementalLogTail {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, limits: TailLimits) -> Self {
        Self {
            path: path.into(),
            limits,
            identity: None,
            offset: 0,
            boundary_start: 0,
            boundary: Vec::new(),
            pending: Vec::new(),
            pending_omitted: 0,
            discard_initial_partial: false,
        }
    }

    #[must_use]
    pub fn with_default_limits(path: impl Into<PathBuf>) -> Self {
        Self::new(path, TailLimits::default())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads only newly available bytes and emits completed physical lines.
    pub fn poll(&mut self) -> io::Result<TailUpdate> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TailUpdate {
                    missing: true,
                    ..TailUpdate::default()
                });
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        let identity = FileIdentity::from_metadata(&metadata);
        let length = metadata.len();

        let replaced = self.identity.is_some_and(|current| current != identity);
        let shortened = !replaced && self.identity.is_some() && length < self.offset;
        let rewritten = !replaced
            && !shortened
            && self.identity.is_some()
            && !self.boundary_matches(&mut file)?;
        let reset = replaced || shortened || rewritten;
        let initializing = self.identity.is_none() || reset;
        let mut update = TailUpdate {
            reset,
            ..TailUpdate::default()
        };

        let budget = if initializing {
            self.pending.clear();
            self.pending_omitted = 0;
            self.boundary.clear();
            self.boundary_start = 0;
            let start = length.saturating_sub(self.limits.initial_bytes as u64);
            self.offset = start;
            self.discard_initial_partial = start > 0 && !starts_after_newline(&mut file, start)?;
            update.skipped_bytes = start;
            self.limits.initial_bytes
        } else {
            self.limits.poll_bytes
        };
        self.identity = Some(identity);

        if budget == 0 || self.offset >= length {
            self.remember_boundary(&mut file)?;
            return Ok(update);
        }

        file.seek(SeekFrom::Start(self.offset))?;
        let available = length.saturating_sub(self.offset);
        let read_limit = available.min(budget as u64);
        let capacity = usize::try_from(read_limit).map_or(64 * 1024, |value| value.min(64 * 1024));
        let mut bytes = Vec::with_capacity(capacity);
        (&mut file).take(read_limit).read_to_end(&mut bytes)?;
        self.offset = self.offset.saturating_add(bytes.len() as u64);
        self.process_bytes(&bytes, &mut update);
        self.remember_boundary(&mut file)?;
        Ok(update)
    }

    /// Emits a final unterminated line when the source is known to be done.
    pub fn finish_partial_line(&mut self) -> Option<TailedLine> {
        if self.discard_initial_partial {
            self.pending.clear();
            self.pending_omitted = 0;
            return None;
        }
        if self.pending.is_empty() && self.pending_omitted == 0 {
            return None;
        }
        Some(self.take_pending_line())
    }

    fn process_bytes(&mut self, bytes: &[u8], update: &mut TailUpdate) {
        for &byte in bytes {
            if self.discard_initial_partial {
                update.skipped_bytes = update.skipped_bytes.saturating_add(1);
                if byte == b'\n' {
                    self.discard_initial_partial = false;
                }
                continue;
            }

            if byte == b'\n' {
                update.lines.push(self.take_pending_line());
            } else if self.pending.len() < self.limits.partial_line_bytes {
                self.pending.push(byte);
            } else {
                self.pending_omitted = self.pending_omitted.saturating_add(1);
            }
        }
    }

    fn take_pending_line(&mut self) -> TailedLine {
        if self.pending.last() == Some(&b'\r') {
            self.pending.pop();
        }
        let bytes = std::mem::take(&mut self.pending);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let omitted_bytes = std::mem::take(&mut self.pending_omitted);
        TailedLine {
            text,
            truncation: (omitted_bytes > 0).then_some(TruncationMetadata { omitted_bytes }),
        }
    }

    fn boundary_matches(&self, file: &mut File) -> io::Result<bool> {
        if self.offset == 0 {
            return Ok(true);
        }
        if self.boundary.is_empty() {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(self.boundary_start))?;
        let mut current = vec![0; self.boundary.len()];
        match file.read_exact(&mut current) {
            Ok(()) => Ok(current == self.boundary),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn remember_boundary(&mut self, file: &mut File) -> io::Result<()> {
        let length = usize::try_from(self.offset.min(TAIL_BOUNDARY_BYTES as u64))
            .unwrap_or(TAIL_BOUNDARY_BYTES);
        self.boundary_start = self.offset.saturating_sub(length as u64);
        self.boundary.resize(length, 0);
        file.seek(SeekFrom::Start(self.boundary_start))?;
        match file.read_exact(&mut self.boundary) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                self.boundary.clear();
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

fn starts_after_newline(file: &mut File, start: u64) -> io::Result<bool> {
    if start == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::Start(start - 1))?;
    let mut previous = [0_u8; 1];
    Ok(file.read(&mut previous)? == 1 && previous[0] == b'\n')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: Option<std::time::SystemTime>,
}

impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                created: metadata.created().ok(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::fs::{self, OpenOptions};
    use std::io::Write as _;

    use chrono::Utc;
    use ephervex_server::{CaptureStream, CapturedLine};

    use super::*;

    #[test]
    fn severity_parser_recognizes_vanilla_and_modded_logger_fields() {
        let cases = [
            ("[12:00:00] [main/TRACE]: trace", LogLevel::Trace),
            ("[12:00:00] [main/DEBUG]: debug", LogLevel::Debug),
            ("[12:00:00] [Render thread/INFO]: ready", LogLevel::Info),
            ("[12:00:00] [Server thread/WARN]: slow", LogLevel::Warn),
            ("[12:00:00] [main/ERROR]: failed", LogLevel::Error),
            ("[12:00:00] [main/FATAL]: stopped", LogLevel::Fatal),
            (
                "[13Aug2026 12:00:00.123] [modloading-worker-0/INFO] [net.neoforged.fml/LOADING]: Loaded",
                LogLevel::Info,
            ),
            (
                "[12:00:00] [Server thread/WARN] [solapplepie/]: Item is not food",
                LogLevel::Warn,
            ),
        ];

        for (text, expected) in cases {
            assert_eq!(parse_log_level(text), expected, "{text}");
        }
    }

    #[test]
    fn severity_parser_leaves_unstructured_output_unclassified() {
        assert_eq!(parse_log_level("server is ready"), LogLevel::Other);
        assert_eq!(
            parse_log_level("this message says ERROR but has no logger field"),
            LogLevel::Other
        );
        assert_eq!(
            parse_log_level("[12:00:00] ordinary output"),
            LogLevel::Other
        );
    }

    #[test]
    fn structured_server_capture_removes_transport_envelope_from_display() {
        let timestamp = "2026-08-13T13:27:22Z".parse().expect("timestamp");
        let captured = CapturedLine {
            timestamp,
            stream: CaptureStream::Stdout,
            text: "[15:27:22] [Server thread/WARN] [solapplepie/]: Item is not food".into(),
            truncated: false,
            omitted_bytes: 0,
        };

        let record = LogRecord::from_captured(captured);

        assert_eq!(
            record.text,
            "[15:27:22] [Server thread/WARN] [solapplepie/]: Item is not food"
        );
        assert_eq!(record.level, LogLevel::Warn);
        assert_eq!(
            record.transport.as_ref().map(|value| value.stream),
            Some(CaptureStream::Stdout)
        );
        assert_eq!(
            record.transport.map(|value| value.timestamp),
            Some(timestamp)
        );
    }

    #[test]
    fn stderr_transport_does_not_imply_error_severity() {
        let captured = CapturedLine {
            timestamp: Utc::now(),
            stream: CaptureStream::Stderr,
            text: "unclassified stderr output".into(),
            truncated: false,
            omitted_bytes: 0,
        };

        let record = LogRecord::from_captured(captured);

        assert_eq!(record.level, LogLevel::Other);
        assert_eq!(
            record.transport.map(|value| value.stream),
            Some(CaptureStream::Stderr)
        );
    }

    #[test]
    fn error_filter_keeps_multiline_stack_trace_as_one_record() {
        let mut retained = RetainedLog::new(20, 16 * 1024);
        retained.append_line("[12:00:00] [main/INFO]: starting");
        retained.append_line("[12:00:01] [main/ERROR]: launch failed");
        retained.append_line("java.lang.IllegalStateException: broken state");
        retained.append_line("\tat example.mod.Entry.start(Entry.java:42)");
        retained.append_line("Caused by: java.io.IOException: unavailable");
        retained.append_line("\tat example.mod.Loader.load(Loader.java:9)");
        retained.append_line("plain stdout");

        let mut errors = LogFilter::none();
        errors.set(LogLevel::Error, true);
        let text = retained.filtered_text(&errors);

        assert_eq!(retained.len(), 3);
        assert!(text.starts_with("[12:00:01] [main/ERROR]: launch failed"));
        assert!(text.contains("IllegalStateException"));
        assert!(text.contains("Caused by:"));
        assert!(text.contains("Loader.java:9"));
        assert!(!text.contains("starting"));
        assert!(!text.contains("plain stdout"));
    }

    #[test]
    fn filters_enable_levels_independently_and_default_to_all() {
        let mut retained = RetainedLog::new(20, 16 * 1024);
        retained.append_line("[12:00:00] [main/DEBUG]: detail");
        retained.append_line("[12:00:01] [main/INFO]: ready");
        retained.append_line("unclassified");

        let mut filter = LogFilter::default();
        assert!(filter.is_all());
        filter.set(LogLevel::Info, false);
        filter.set(LogLevel::Other, false);

        assert_eq!(
            retained.filtered_text(&filter),
            "[12:00:00] [main/DEBUG]: detail"
        );
        filter.toggle(LogLevel::Debug);
        assert_eq!(retained.filtered_text(&filter), "");
        filter.set_all(true);
        assert!(filter.is_all());
    }

    #[test]
    fn retained_history_obeys_record_and_byte_bounds() {
        let mut by_records = RetainedLog::new(2, 1_024);
        by_records.append_line("one");
        by_records.append_line("two");
        by_records.append_line("three");
        assert_eq!(by_records.text(), "two\nthree");
        assert_eq!(by_records.dropped_records(), 1);
        assert!(by_records.history_truncated());

        let mut by_bytes = RetainedLog::new(10, 5);
        by_bytes.append_line("older");
        by_bytes.append_line("newer");
        assert_eq!(by_bytes.text(), "newer");
        assert_eq!(by_bytes.retained_bytes(), 5);
        assert_eq!(by_bytes.dropped_records(), 1);

        by_bytes.append_line("multibyte-💥");
        assert!(by_bytes.retained_bytes() <= 5);
        assert!(std::str::from_utf8(by_bytes.text().as_bytes()).is_ok());
    }

    #[test]
    fn search_uses_all_retained_records_independent_of_filtering() {
        let mut retained = RetainedLog::new(10, 1_024);
        retained.append_line("[12:00:00] [main/INFO]: searchable value");
        retained.append_line("[12:00:01] [main/ERROR]: visible failure");
        let mut errors = LogFilter::none();
        errors.set(LogLevel::Error, true);

        assert!(!retained.filtered_text(&errors).contains("searchable"));
        assert_eq!(retained.search("searchable").len(), 1);
    }

    #[test]
    fn tail_reads_initial_window_and_only_new_appends_afterward() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        fs::write(&path, b"old line\nkept one\nkept two\n").expect("initial log");
        let mut tail = IncrementalLogTail::new(
            &path,
            TailLimits {
                initial_bytes: 20,
                poll_bytes: 64,
                partial_line_bytes: 64,
            },
        );

        let initial = tail.poll().expect("initial poll");
        assert!(initial.skipped_bytes > 0);
        assert_eq!(texts(&initial), vec!["kept one", "kept two"]);

        append(&path, b"new line\n");
        let appended = tail.poll().expect("append poll");
        assert_eq!(texts(&appended), vec!["new line"]);
    }

    #[test]
    fn missing_log_is_quiet_and_creation_is_followed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        let mut tail = IncrementalLogTail::with_default_limits(&path);

        assert!(tail.poll().expect("missing poll").missing);
        fs::write(&path, b"created\n").expect("created log");
        let created = tail.poll().expect("creation poll");
        assert!(!created.missing);
        assert!(!created.reset);
        assert_eq!(texts(&created), vec!["created"]);
    }

    #[test]
    fn truncation_resets_tail_and_reads_new_content() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        fs::write(&path, b"first line\nsecond line\n").expect("initial log");
        let mut tail = IncrementalLogTail::with_default_limits(&path);
        tail.poll().expect("initial poll");

        fs::write(&path, b"short\n").expect("truncated log");
        let update = tail.poll().expect("truncation poll");

        assert!(update.reset);
        assert_eq!(texts(&update), vec!["short"]);
    }

    #[test]
    fn truncation_that_regrows_between_polls_resets_tail() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        fs::write(&path, b"old first line\nold second line\n").expect("initial log");
        let mut tail = IncrementalLogTail::with_default_limits(&path);
        tail.poll().expect("initial poll");

        fs::write(
            &path,
            b"new first line after truncation\nnew second line is also longer\n",
        )
        .expect("regrown log");
        let update = tail.poll().expect("regrown poll");

        assert!(update.reset);
        assert_eq!(
            texts(&update),
            vec![
                "new first line after truncation",
                "new second line is also longer"
            ]
        );
    }

    #[test]
    fn replacement_resets_tail_even_when_new_file_is_longer() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        let replacement = directory.path().join("replacement.log");
        fs::write(&path, b"old\n").expect("initial log");
        let mut tail = IncrementalLogTail::with_default_limits(&path);
        tail.poll().expect("initial poll");

        fs::write(&replacement, b"replacement content\n").expect("replacement log");
        fs::rename(&replacement, &path).expect("replace log");
        let update = tail.poll().expect("replacement poll");

        assert!(update.reset);
        assert_eq!(texts(&update), vec!["replacement content"]);
    }

    #[test]
    fn partial_lines_and_utf8_boundaries_are_not_corrupted() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        fs::write(&path, b"").expect("empty log");
        let mut tail = IncrementalLogTail::new(
            &path,
            TailLimits {
                initial_bytes: 2,
                poll_bytes: 2,
                partial_line_bytes: 64,
            },
        );
        tail.poll().expect("initial poll");

        let content = "prefix 💥 suffix\n".as_bytes();
        for chunk in content.chunks(2) {
            append(&path, chunk);
            let update = tail.poll().expect("partial poll");
            if chunk.last() == Some(&b'\n') {
                assert_eq!(texts(&update), vec!["prefix 💥 suffix"]);
            } else {
                assert!(update.lines.is_empty());
            }
        }
    }

    #[test]
    fn oversized_unterminated_line_keeps_bounded_prefix_and_reports_omission() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("latest.log");
        fs::write(&path, b"").expect("empty log");
        let mut tail = IncrementalLogTail::new(
            &path,
            TailLimits {
                initial_bytes: 4,
                poll_bytes: 4,
                partial_line_bytes: 5,
            },
        );
        tail.poll().expect("initial poll");
        append(&path, b"abcdefghijkl\n");

        let mut complete = Vec::new();
        for _ in 0..4 {
            complete.extend(tail.poll().expect("bounded poll").lines);
        }

        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0].text, "abcde");
        assert_eq!(
            complete[0].truncation,
            Some(TruncationMetadata { omitted_bytes: 7 })
        );
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .expect("open log")
            .write_all(bytes)
            .expect("append log");
    }

    fn texts(update: &TailUpdate) -> Vec<&str> {
        update.lines.iter().map(|line| line.text.as_str()).collect()
    }
}
