//! Bounded in-memory output and rotated JSON Lines diagnostic capture.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_RING_LINES: usize = 2_000;
pub const MAX_RING_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CAPTURE_LINE_BYTES: usize = 64 * 1024;
pub const CAPTURE_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
pub const CAPTURE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
pub const CAPTURE_INDEX_FILE: &str = "capture-index.json";

/// Ends the name of a segment that carries on the run of the segment before
/// it. Every other segment starts a run, which is how a reader finds where the
/// newest run begins without any record inside a segment naming a run.
const CONTINUED_SEGMENT_SUFFIX: &str = ".continued.jsonl";

/// Whether a capture segment file continues the run of an older segment
/// instead of starting one. A segment without the mark, as older captures
/// have, reads as the start of a run.
#[must_use]
pub fn segment_continues_run(file_name: &str) -> bool {
    file_name.ends_with(CONTINUED_SEGMENT_SUFFIX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStream {
    Stdout,
    Stderr,
    Launcher,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedLine {
    pub timestamp: DateTime<Utc>,
    pub stream: CaptureStream,
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub omitted_bytes: usize,
}

impl CapturedLine {
    #[must_use]
    pub fn from_bytes(stream: CaptureStream, bytes: &[u8]) -> Self {
        let (kept, omitted_bytes) = truncate_bytes(bytes, MAX_CAPTURE_LINE_BYTES);
        Self::from_limited_bytes(stream, kept, omitted_bytes)
    }

    #[must_use]
    pub fn from_limited_bytes(
        stream: CaptureStream,
        bytes: &[u8],
        mut omitted_bytes: usize,
    ) -> Self {
        let bytes = match std::str::from_utf8(bytes) {
            Err(error) if error.error_len().is_none() => {
                omitted_bytes += bytes.len() - error.valid_up_to();
                &bytes[..error.valid_up_to()]
            }
            _ => bytes,
        };
        Self {
            timestamp: Utc::now(),
            stream,
            text: String::from_utf8_lossy(bytes).into_owned(),
            truncated: omitted_bytes > 0,
            omitted_bytes,
        }
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.text.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CaptureRetention {
    pub removed_segments: u64,
    pub removed_bytes: u64,
    pub removed_lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub earliest_retained_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureHealth {
    Available,
    Unavailable { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerOutputEvent {
    Line(CapturedLine),
    CapturePruned(CaptureRetention),
    CaptureUnavailable { detail: String },
}

/// Output state kept even when durable storage becomes unavailable.
#[derive(Debug)]
pub struct OutputCapture {
    ring: VecDeque<CapturedLine>,
    ring_bytes: usize,
    writer: Option<CaptureWriter>,
    health: CaptureHealth,
    events: VecDeque<ServerOutputEvent>,
}

impl OutputCapture {
    #[must_use]
    pub fn new(directory: &Path) -> Self {
        match CaptureWriter::open(directory, CaptureLimits::default()) {
            Ok(writer) => Self {
                ring: VecDeque::new(),
                ring_bytes: 0,
                writer: Some(writer),
                health: CaptureHealth::Available,
                events: VecDeque::new(),
            },
            Err(error) => Self {
                ring: VecDeque::new(),
                ring_bytes: 0,
                writer: None,
                health: CaptureHealth::Unavailable {
                    detail: error.to_string(),
                },
                events: VecDeque::from([ServerOutputEvent::CaptureUnavailable {
                    detail: error.to_string(),
                }]),
            },
        }
    }

    /// Records a line and always retains it in memory, even after disk failure.
    pub fn push(&mut self, stream: CaptureStream, bytes: &[u8]) -> CapturedLine {
        let line = CapturedLine::from_bytes(stream, bytes);
        self.push_line(line)
    }

    /// Records a line already bounded while it was drained from a pipe.
    pub fn push_limited(
        &mut self,
        stream: CaptureStream,
        bytes: &[u8],
        omitted_bytes: usize,
    ) -> CapturedLine {
        self.push_line(CapturedLine::from_limited_bytes(
            stream,
            bytes,
            omitted_bytes,
        ))
    }

    fn push_line(&mut self, line: CapturedLine) -> CapturedLine {
        self.ring_bytes += line.memory_bytes();
        self.ring.push_back(line.clone());
        while self.ring.len() > MAX_RING_LINES || self.ring_bytes > MAX_RING_BYTES {
            if let Some(removed) = self.ring.pop_front() {
                self.ring_bytes = self.ring_bytes.saturating_sub(removed.memory_bytes());
            }
        }
        self.events.push_back(ServerOutputEvent::Line(line.clone()));

        if let Some(writer) = &mut self.writer {
            match writer.write(&line) {
                Ok(Some(retention)) => self
                    .events
                    .push_back(ServerOutputEvent::CapturePruned(retention)),
                Ok(None) => {}
                Err(error) => {
                    let detail = error.to_string();
                    tracing::error!(%error, "server capture became unavailable; continuing pipe drain");
                    self.writer = None;
                    self.health = CaptureHealth::Unavailable {
                        detail: detail.clone(),
                    };
                    self.events
                        .push_back(ServerOutputEvent::CaptureUnavailable { detail });
                }
            }
        }
        line
    }

    /// Starts a new run in the capture: the lines pushed from here on belong to
    /// it, and a reader that wants only the newest run begins at its segment.
    pub fn begin_run(&mut self) {
        let Some(writer) = &mut self.writer else {
            return;
        };
        if let Err(error) = writer.rotate(SegmentStart::Run) {
            let detail = error.to_string();
            tracing::error!(%error, "server capture became unavailable; continuing pipe drain");
            self.writer = None;
            self.health = CaptureHealth::Unavailable {
                detail: detail.clone(),
            };
            self.events
                .push_back(ServerOutputEvent::CaptureUnavailable { detail });
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<CapturedLine> {
        self.ring.iter().cloned().collect()
    }

    #[must_use]
    pub const fn health(&self) -> &CaptureHealth {
        &self.health
    }

    pub fn drain_events(&mut self) -> Vec<ServerOutputEvent> {
        self.events.drain(..).collect()
    }

    #[must_use]
    pub fn retention(&self) -> CaptureRetention {
        self.writer
            .as_ref()
            .map_or_else(CaptureRetention::default, |writer| writer.retention.clone())
    }
}

#[derive(Debug, Clone, Copy)]
struct CaptureLimits {
    segment_bytes: u64,
    budget_bytes: u64,
}

impl Default for CaptureLimits {
    fn default() -> Self {
        Self {
            segment_bytes: CAPTURE_SEGMENT_BYTES,
            budget_bytes: CAPTURE_BUDGET_BYTES,
        }
    }
}

/// A capture segment as the writer accounts for it. `lines` is unknown for
/// segments left by earlier runs until retention needs to report them.
#[derive(Debug)]
struct TrackedSegment {
    path: PathBuf,
    bytes: u64,
    lines: Option<u64>,
    started_at: Option<DateTime<Utc>>,
}

impl TrackedSegment {
    fn empty(path: PathBuf) -> Self {
        Self {
            path,
            bytes: 0,
            lines: Some(0),
            started_at: None,
        }
    }
}

/// Whether a new segment begins a run or carries on the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentStart {
    Run,
    Continuation,
}

/// Upper bound on bytes read to find a segment's first line.
const SEGMENT_START_PROBE_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
struct CaptureWriter {
    directory: PathBuf,
    current: std::fs::File,
    current_segment: TrackedSegment,
    /// The instant in the current segment's name. A later segment is named
    /// after it, so names sort in creation order even within one millisecond.
    current_segment_at: DateTime<Utc>,
    /// Older segments, oldest first.
    retained: VecDeque<TrackedSegment>,
    /// Bytes across `retained` and `current_segment`, kept up to date so a write never rescans the directory.
    total_bytes: u64,
    limits: CaptureLimits,
    retention: CaptureRetention,
}

impl CaptureWriter {
    fn open(directory: &Path, limits: CaptureLimits) -> std::io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let retention = read_retention(directory).unwrap_or_default();
        let retained: VecDeque<TrackedSegment> = existing_segments(directory)?.into();
        let segment_at = Utc::now();
        let (path, current) = create_segment(directory, SegmentStart::Run, segment_at)?;
        let total_bytes = retained.iter().map(|segment| segment.bytes).sum();
        Ok(Self {
            directory: directory.to_path_buf(),
            current,
            current_segment: TrackedSegment::empty(path),
            current_segment_at: segment_at,
            retained,
            total_bytes,
            limits,
            retention,
        })
    }

    fn write(&mut self, line: &CapturedLine) -> std::io::Result<Option<CaptureRetention>> {
        let mut body = serde_json::to_vec(line).map_err(std::io::Error::other)?;
        body.push(b'\n');
        let length = u64::try_from(body.len()).unwrap_or(u64::MAX);
        if self.current_segment.bytes > 0
            && self.current_segment.bytes.saturating_add(length) > self.limits.segment_bytes
        {
            self.rotate(SegmentStart::Continuation)?;
        }
        self.current.write_all(&body)?;
        self.current.flush()?;
        self.record_current(length, Some(line.timestamp));
        self.enforce_budget()
    }

    fn record_current(&mut self, length: u64, timestamp: Option<DateTime<Utc>>) {
        let segment = &mut self.current_segment;
        segment.bytes = segment.bytes.saturating_add(length);
        segment.lines = segment.lines.map(|lines| lines.saturating_add(1));
        if segment.started_at.is_none() {
            segment.started_at = timestamp;
        }
        self.total_bytes = self.total_bytes.saturating_add(length);
    }

    fn rotate(&mut self, start: SegmentStart) -> std::io::Result<()> {
        self.current.sync_all()?;
        let segment_at =
            Utc::now().max(self.current_segment_at + chrono::Duration::milliseconds(1));
        let (path, file) = create_segment(&self.directory, start, segment_at)?;
        self.current = file;
        self.current_segment_at = segment_at;
        let finished = std::mem::replace(&mut self.current_segment, TrackedSegment::empty(path));
        self.retained.push_back(finished);
        Ok(())
    }

    fn enforce_budget(&mut self) -> std::io::Result<Option<CaptureRetention>> {
        let mut changed = false;
        while self.total_bytes > self.limits.budget_bytes {
            let Some(segment) = self.retained.pop_front() else {
                break;
            };
            let lines = match segment.lines {
                Some(lines) => lines,
                None => match count_lines(&segment.path) {
                    Ok(lines) => lines,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(error) => return Err(error),
                },
            };
            match std::fs::remove_file(&segment.path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            self.total_bytes = self.total_bytes.saturating_sub(segment.bytes);
            self.retention.removed_segments += 1;
            self.retention.removed_bytes += segment.bytes;
            self.retention.removed_lines += lines;
            changed = true;
        }
        if !changed {
            return Ok(None);
        }
        self.retention.earliest_retained_at = self
            .retained
            .front()
            .unwrap_or(&self.current_segment)
            .started_at;
        write_retention(&self.directory, &self.retention)?;
        let marker = CapturedLine {
            timestamp: Utc::now(),
            stream: CaptureStream::Launcher,
            text: format!(
                "capture retention pruned {} segments ({} bytes, {} lines)",
                self.retention.removed_segments,
                self.retention.removed_bytes,
                self.retention.removed_lines
            ),
            truncated: false,
            omitted_bytes: 0,
        };
        let mut body = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;
        body.push(b'\n');
        self.current.write_all(&body)?;
        self.current.flush()?;
        self.record_current(u64::try_from(body.len()).unwrap_or(0), None);
        Ok(Some(self.retention.clone()))
    }
}

fn create_segment(
    directory: &Path,
    start: SegmentStart,
    at: DateTime<Utc>,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    let name = format!(
        "capture-{}-{}{}",
        at.format("%Y%m%dT%H%M%S%.3fZ"),
        Uuid::new_v4(),
        match start {
            SegmentStart::Run => ".jsonl",
            SegmentStart::Continuation => CONTINUED_SEGMENT_SUFFIX,
        }
    );
    let path = directory.join(name);
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&path)?;
    Ok((path, file))
}

/// Lists segments left by earlier runs, oldest first. Only sizes and each
/// segment's first timestamp are read, so opening does not scan retained output.
fn existing_segments(directory: &Path) -> std::io::Result<Vec<TrackedSegment>> {
    let mut segments = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !name.starts_with("capture-")
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let started_at = first_timestamp(&path);
        segments.push(TrackedSegment {
            path,
            bytes: metadata.len(),
            lines: None,
            started_at,
        });
    }
    segments.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(segments)
}

fn first_timestamp(path: &Path) -> Option<DateTime<Utc>> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(SEGMENT_START_PROBE_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        if line.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return serde_json::from_slice::<CapturedLine>(&line)
                .ok()
                .map(|line| line.timestamp);
        }
    }
}

/// Counts newline-terminated lines, matching how segments are written.
fn count_lines(path: &Path) -> std::io::Result<u64> {
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut line = Vec::new();
    let mut lines = 0_u64;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(lines);
        }
        if line.last() == Some(&b'\n') {
            lines += 1;
        }
    }
}

fn truncate_bytes(bytes: &[u8], limit: usize) -> (&[u8], usize) {
    if bytes.len() <= limit {
        (bytes, 0)
    } else {
        let mut kept = limit;
        if let Ok(text) = std::str::from_utf8(bytes) {
            while kept > 0 && !text.is_char_boundary(kept) {
                kept -= 1;
            }
        }
        (&bytes[..kept], bytes.len() - kept)
    }
}

fn read_retention(directory: &Path) -> std::io::Result<CaptureRetention> {
    let body = std::fs::read(directory.join(CAPTURE_INDEX_FILE))?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}

fn write_retention(directory: &Path, retention: &CaptureRetention) -> std::io::Result<()> {
    let path = directory.join(CAPTURE_INDEX_FILE);
    let temporary = path.with_extension("json.tmp");
    let body = serde_json::to_vec(retention).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&body)?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    std::fs::File::open(directory)?.sync_all()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn long_and_invalid_utf8_lines_are_safely_truncated() {
        let bytes = vec![0xff; MAX_CAPTURE_LINE_BYTES + 17];
        let line = CapturedLine::from_bytes(CaptureStream::Stderr, &bytes);
        assert!(line.truncated);
        assert_eq!(line.omitted_bytes, 17);
        assert!(!line.text.is_empty());
    }

    #[test]
    fn ring_limits_evict_oldest_lines_first() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut capture = OutputCapture::new(directory.path());
        for index in 0..=MAX_RING_LINES {
            capture.push(CaptureStream::Stdout, index.to_string().as_bytes());
        }
        let snapshot = capture.snapshot();
        assert_eq!(snapshot.len(), MAX_RING_LINES);
        assert_eq!(snapshot[0].text, "1");
    }

    #[test]
    fn rotation_and_retention_are_observable() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut writer = CaptureWriter::open(
            directory.path(),
            CaptureLimits {
                segment_bytes: 180,
                budget_bytes: 500,
            },
        )
        .expect("writer");
        let mut pruned = None;
        for index in 0..30 {
            let line = CapturedLine::from_bytes(
                CaptureStream::Stdout,
                format!("line-{index}-xxxxxxxxxxxxxxxxxxxxxxxx").as_bytes(),
            );
            if let Some(retention) = writer.write(&line).expect("write") {
                pruned = Some(retention);
            }
        }
        let retention = pruned.expect("old segments should be pruned");
        assert!(retention.removed_segments > 0);
        assert!(directory.path().join(CAPTURE_INDEX_FILE).is_file());
    }

    fn segment_names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("capture-") && name != CAPTURE_INDEX_FILE)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_size_rotation_marks_a_segment_as_continuing_its_run() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut writer = CaptureWriter::open(
            directory.path(),
            CaptureLimits {
                segment_bytes: 180,
                budget_bytes: CAPTURE_BUDGET_BYTES,
            },
        )
        .expect("writer");
        for index in 0..6 {
            let line = CapturedLine::from_bytes(
                CaptureStream::Stdout,
                format!("line-{index}-xxxxxxxxxxxxxxxxxxxxxxxx").as_bytes(),
            );
            writer.write(&line).expect("write");
        }

        let names = segment_names(directory.path());
        assert!(names.len() > 1, "the run should have rotated: {names:?}");
        let starts: Vec<_> = names
            .iter()
            .filter(|name| !segment_continues_run(name))
            .collect();
        assert_eq!(starts.len(), 1, "one run starts one segment: {names:?}");
    }

    #[test]
    fn segments_sort_in_creation_order_even_when_created_in_one_millisecond() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut capture = OutputCapture::new(directory.path());
        for _ in 0..50 {
            capture.begin_run();
        }

        let names = segment_names(directory.path());
        let created = capture
            .writer
            .as_ref()
            .map(|writer| {
                writer
                    .retained
                    .iter()
                    .map(|segment| segment.path.clone())
                    .chain([writer.current_segment.path.clone()])
                    .collect::<Vec<_>>()
            })
            .expect("writer");
        let mut sorted = created.clone();
        sorted.sort();
        assert_eq!(names.len(), 51);
        assert_eq!(created, sorted, "the newest segment must sort last");
    }

    #[test]
    fn beginning_a_run_starts_a_segment_even_before_it_writes_a_line() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut capture = OutputCapture::new(directory.path());
        capture.push(CaptureStream::Stdout, b"first run");

        capture.begin_run();

        let names = segment_names(directory.path());
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|name| !segment_continues_run(name)));
    }

    fn timestamp(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    fn write_prior_segment(directory: &Path, name: &str, start: &str, lines: usize) -> u64 {
        let start = timestamp(start);
        let mut body = Vec::new();
        for index in 0..lines {
            let line = CapturedLine {
                timestamp: start + chrono::Duration::seconds(i64::try_from(index).unwrap()),
                stream: CaptureStream::Stdout,
                text: format!("prior-{index}-{}", "x".repeat(200)),
                truncated: false,
                omitted_bytes: 0,
            };
            body.extend(serde_json::to_vec(&line).unwrap());
            body.push(b'\n');
        }
        std::fs::write(directory.join(name), &body).unwrap();
        u64::try_from(body.len()).unwrap()
    }

    #[test]
    fn writes_within_budget_never_read_retained_segments() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_prior_segment(
            directory.path(),
            "capture-20200101T000000.000Z-a.jsonl",
            "2020-01-01T00:00:00Z",
            3,
        );
        let mut writer =
            CaptureWriter::open(directory.path(), CaptureLimits::default()).expect("writer");

        // A directory cannot be read as a file, so any per-write rescan would fail.
        let retained = directory
            .path()
            .join("capture-20200101T000000.000Z-a.jsonl");
        std::fs::remove_file(&retained).unwrap();
        std::fs::create_dir(&retained).unwrap();

        for index in 0..50 {
            let line =
                CapturedLine::from_bytes(CaptureStream::Stdout, format!("{index}").as_bytes());
            assert!(writer.write(&line).expect("write").is_none());
        }
    }

    #[test]
    fn pruning_reports_segments_from_earlier_runs_and_survives_reopen() {
        let directory = tempfile::tempdir().expect("temp dir");
        let oldest = write_prior_segment(
            directory.path(),
            "capture-20200101T000000.000Z-a.jsonl",
            "2020-01-01T00:00:00Z",
            3,
        );
        let newer = write_prior_segment(
            directory.path(),
            "capture-20200102T000000.000Z-b.jsonl",
            "2020-01-02T00:00:00Z",
            2,
        );
        let line = CapturedLine::from_bytes(CaptureStream::Stdout, b"fresh");
        let line_bytes = u64::try_from(serde_json::to_vec(&line).unwrap().len() + 1).unwrap();
        let limits = CaptureLimits {
            segment_bytes: CAPTURE_SEGMENT_BYTES,
            budget_bytes: oldest + newer + line_bytes - 1,
        };

        let mut writer = CaptureWriter::open(directory.path(), limits).expect("writer");
        let retention = writer.write(&line).expect("write").expect("pruned");

        assert_eq!(retention.removed_segments, 1);
        assert_eq!(retention.removed_bytes, oldest);
        assert_eq!(retention.removed_lines, 3);
        assert_eq!(
            retention.earliest_retained_at,
            Some(timestamp("2020-01-02T00:00:00Z"))
        );
        assert!(
            !directory
                .path()
                .join("capture-20200101T000000.000Z-a.jsonl")
                .exists()
        );
        assert!(
            directory
                .path()
                .join("capture-20200102T000000.000Z-b.jsonl")
                .exists()
        );
        assert_eq!(read_retention(directory.path()).expect("index"), retention);

        let reopened = CaptureWriter::open(directory.path(), limits).expect("reopen");
        assert_eq!(reopened.retention, retention);
    }

    #[test]
    fn the_capture_index_is_stored_compactly_and_a_pretty_one_still_loads() {
        let directory = tempfile::tempdir().expect("temp dir");
        let retention = CaptureRetention {
            removed_segments: 2,
            removed_bytes: 1024,
            removed_lines: 17,
            earliest_retained_at: Some(Utc::now()),
        };
        write_retention(directory.path(), &retention).expect("write");

        let path = directory.path().join(CAPTURE_INDEX_FILE);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            !text.contains('\n'),
            "the capture index carries pretty-printing whitespace: {text}"
        );
        assert_eq!(read_retention(directory.path()).expect("read"), retention);

        let stored: serde_json::Value = serde_json::from_str(&text).expect("parse");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&stored).expect("indent"),
        )
        .expect("rewrite as an older build would have");
        assert_eq!(read_retention(directory.path()).expect("read"), retention);
    }
}
