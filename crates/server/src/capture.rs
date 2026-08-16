//! Bounded in-memory output and rotated JSON Lines diagnostic capture.

use std::collections::VecDeque;
use std::io::Write as _;
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

#[derive(Debug)]
struct CaptureWriter {
    directory: PathBuf,
    current_path: PathBuf,
    current: std::fs::File,
    current_bytes: u64,
    limits: CaptureLimits,
    retention: CaptureRetention,
}

impl CaptureWriter {
    fn open(directory: &Path, limits: CaptureLimits) -> std::io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let retention = read_retention(directory).unwrap_or_default();
        let (current_path, current) = create_segment(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            current_path,
            current,
            current_bytes: 0,
            limits,
            retention,
        })
    }

    fn write(&mut self, line: &CapturedLine) -> std::io::Result<Option<CaptureRetention>> {
        let mut body = serde_json::to_vec(line).map_err(std::io::Error::other)?;
        body.push(b'\n');
        if self.current_bytes > 0
            && self.current_bytes + u64::try_from(body.len()).unwrap_or(u64::MAX)
                > self.limits.segment_bytes
        {
            self.rotate()?;
        }
        self.current.write_all(&body)?;
        self.current.flush()?;
        self.current_bytes += u64::try_from(body.len()).unwrap_or(u64::MAX);
        self.enforce_budget()
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.current.sync_all()?;
        let (path, file) = create_segment(&self.directory)?;
        self.current_path = path;
        self.current = file;
        self.current_bytes = 0;
        Ok(())
    }

    fn enforce_budget(&mut self) -> std::io::Result<Option<CaptureRetention>> {
        let mut segments = segment_files(&self.directory)?;
        let mut total: u64 = segments.iter().map(|segment| segment.bytes).sum();
        let mut changed = false;
        while total > self.limits.budget_bytes {
            let Some(index) = segments
                .iter()
                .position(|segment| segment.path != self.current_path)
            else {
                break;
            };
            let segment = segments.remove(index);
            std::fs::remove_file(&segment.path)?;
            total = total.saturating_sub(segment.bytes);
            self.retention.removed_segments += 1;
            self.retention.removed_bytes += segment.bytes;
            self.retention.removed_lines += segment.lines;
            changed = true;
        }
        if !changed {
            return Ok(None);
        }
        self.retention.earliest_retained_at =
            segments.first().and_then(|segment| segment.started_at);
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
        self.current_bytes += u64::try_from(body.len()).unwrap_or(0);
        Ok(Some(self.retention.clone()))
    }
}

#[derive(Debug)]
struct SegmentInfo {
    path: PathBuf,
    bytes: u64,
    lines: u64,
    started_at: Option<DateTime<Utc>>,
}

fn create_segment(directory: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    let name = format!(
        "capture-{}-{}.jsonl",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        Uuid::new_v4()
    );
    let path = directory.join(name);
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&path)?;
    Ok((path, file))
}

fn segment_files(directory: &Path) -> std::io::Result<Vec<SegmentInfo>> {
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
        let bytes = entry.metadata()?.len();
        let body = std::fs::read(&path)?;
        let lines = u64::try_from(body.split(|byte| *byte == b'\n').count().saturating_sub(1))
            .unwrap_or(u64::MAX);
        let started_at = body
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .and_then(|line| serde_json::from_slice::<CapturedLine>(line).ok())
            .map(|line| line.timestamp);
        segments.push(SegmentInfo {
            path,
            bytes,
            lines,
            started_at,
        });
    }
    segments.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(segments)
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
