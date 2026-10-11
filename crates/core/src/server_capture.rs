//! Following a server's capture while it is written.
//!
//! The capture directory is the one record every supervisor leaves, so a reader
//! that follows it sees the same lines whichever process runs the server.

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use ephervex_server::{CaptureRetention, CapturedLine, segment_continues_run};

use crate::CoreResult;
use crate::server::{capture_segment_paths, read_capture_retention};

/// Marks a segment whose lines were left out of the first read.
const NOT_SHOWN: u64 = u64::MAX;

/// What one poll of a server's capture found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureUpdate {
    pub lines: Vec<CapturedLine>,
    /// `lines` replace everything shown so far: this is the first poll, or a
    /// new run has begun.
    pub reset: bool,
    /// Output of the run is missing from what was handed out, because it was
    /// bounded or retention removed it.
    pub truncated: bool,
}

impl CaptureUpdate {
    /// Whether the poll found nothing to report.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.reset && !self.truncated && self.lines.is_empty()
    }
}

/// Incremental state for following one server's capture.
#[derive(Debug)]
pub struct ServerCaptureTail {
    directory: PathBuf,
    initial_lines: usize,
    position: Option<RunPosition>,
}

/// Where a follower stands in the newest run.
#[derive(Debug)]
struct RunPosition {
    /// The segment that began the run, or `None` when retention removed it.
    start: Option<String>,
    /// How many bytes of each segment of the run were handed out.
    consumed: HashMap<String, u64>,
}

impl ServerCaptureTail {
    pub(crate) const fn new(directory: PathBuf, initial_lines: usize) -> Self {
        Self {
            directory,
            initial_lines,
            position: None,
        }
    }

    /// Reads what the capture gained since the previous poll.
    ///
    /// The first poll, and the first poll after a new run began, replace the
    /// follower's view with the newest `initial_lines` lines of the run.
    pub fn poll(&mut self) -> CoreResult<CaptureUpdate> {
        let mut files = capture_segment_paths(&self.directory)?;
        files.sort();
        let start = files.iter().rposition(|path| !continues_run(path));
        let start_name = start.and_then(|index| segment_name(&files[index]));
        let run = &files[start.unwrap_or(0)..];
        let same_run = self.position.as_ref().is_some_and(|position| {
            start_name.is_none() || position.start.as_deref() == start_name.as_deref()
        });
        if same_run {
            self.read_new_lines(run)
        } else {
            self.read_run(run, start_name)
        }
    }

    fn read_run(&mut self, run: &[PathBuf], start: Option<String>) -> CoreResult<CaptureUpdate> {
        let limit = self.initial_lines;
        let mut newest_first = Vec::new();
        let mut consumed = HashMap::new();
        let mut pruned_while_reading = false;
        for path in run.iter().rev() {
            let Some(name) = segment_name(path) else {
                continue;
            };
            if newest_first.len() > limit {
                consumed.insert(name, NOT_SHOWN);
                continue;
            }
            let Some(read) = read_new_bytes(path, 0)? else {
                pruned_while_reading = true;
                continue;
            };
            consumed.insert(name, read.consumed);
            for line in read.complete.rsplit(|byte| *byte == b'\n') {
                if let Ok(captured) = serde_json::from_slice::<CapturedLine>(line) {
                    newest_first.push(captured);
                    if newest_first.len() > limit {
                        break;
                    }
                }
            }
        }
        let bounded = newest_first.len() > limit;
        newest_first.truncate(limit);
        newest_first.reverse();
        // A run that kept its first segment lost nothing, whatever earlier runs lost.
        let retention = if start.is_some() && !pruned_while_reading {
            CaptureRetention::default()
        } else {
            read_capture_retention(&self.directory)
        };
        self.position = Some(RunPosition { start, consumed });
        Ok(CaptureUpdate {
            lines: newest_first,
            reset: true,
            truncated: bounded || pruned_while_reading || retention.removed_lines > 0,
        })
    }

    fn read_new_lines(&mut self, run: &[PathBuf]) -> CoreResult<CaptureUpdate> {
        let Some(position) = self.position.as_mut() else {
            return Ok(CaptureUpdate::default());
        };
        let mut update = CaptureUpdate::default();
        let listed: Vec<String> = run.iter().filter_map(|path| segment_name(path)).collect();
        // Retention removed a segment of the run this follower has been reading.
        let before = position.consumed.len();
        position.consumed.retain(|name, _| listed.contains(name));
        update.truncated |= position.consumed.len() != before;
        // Retention removed the segment that began the run.
        if position
            .start
            .as_ref()
            .is_some_and(|name| !listed.contains(name))
        {
            position.start = None;
            update.truncated = true;
        }
        for path in run {
            let Some(name) = segment_name(path) else {
                continue;
            };
            let offset = position.consumed.get(&name).copied().unwrap_or(0);
            if offset == NOT_SHOWN {
                continue;
            }
            let Some(read) = read_new_bytes(path, offset)? else {
                position.consumed.remove(&name);
                update.truncated = true;
                continue;
            };
            position.consumed.insert(name, offset + read.consumed);
            update.lines.extend(
                read.complete
                    .split(|byte| *byte == b'\n')
                    .filter_map(|line| serde_json::from_slice::<CapturedLine>(line).ok()),
            );
        }
        Ok(update)
    }
}

struct NewBytes {
    /// The whole lines found after the offset.
    complete: Vec<u8>,
    /// How many bytes of the segment `complete` accounts for.
    consumed: u64,
}

/// The whole lines a segment gained after `offset`, or `None` when retention
/// removed the segment.
fn read_new_bytes(path: &Path, offset: u64) -> CoreResult<Option<NewBytes>> {
    let io_error = |source| ephervex_server::ServerError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(source).into()),
    };
    let length = file.metadata().map_err(io_error)?.len();
    let mut body = Vec::new();
    if length > offset {
        file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        file.read_to_end(&mut body).map_err(io_error)?;
    }
    // A line the writer has not finished stays for the next poll.
    let complete_len = body
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    body.truncate(complete_len);
    Ok(Some(NewBytes {
        consumed: u64::try_from(complete_len).unwrap_or(0),
        complete: body,
    }))
}

fn segment_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

fn continues_run(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(segment_continues_run)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::Write as _;

    use ephervex_server::CaptureStream;

    use super::*;

    const FIRST: &str = "capture-20260813T120000.000Z-a.jsonl";
    const FIRST_CONTINUED: &str = "capture-20260813T120500.000Z-b.continued.jsonl";
    const SECOND: &str = "capture-20260813T130000.000Z-c.jsonl";

    fn line(text: &str) -> Vec<u8> {
        let mut body = serde_json::to_vec(&CapturedLine {
            timestamp: chrono::Utc::now(),
            stream: CaptureStream::Stdout,
            text: text.to_owned(),
            truncated: false,
            omitted_bytes: 0,
        })
        .expect("line");
        body.push(b'\n');
        body
    }

    fn append(directory: &Path, name: &str, texts: &[&str]) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(name))
            .expect("segment");
        for text in texts {
            file.write_all(&line(text)).expect("write");
        }
    }

    fn texts(update: &CaptureUpdate) -> Vec<&str> {
        update.lines.iter().map(|line| line.text.as_str()).collect()
    }

    fn tail(directory: &Path, initial_lines: usize) -> ServerCaptureTail {
        ServerCaptureTail::new(directory.to_path_buf(), initial_lines)
    }

    #[test]
    fn the_first_poll_hands_out_the_latest_run_and_later_polls_only_what_was_appended() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["old run"]);
        append(directory.path(), SECOND, &["one", "two"]);
        let mut tail = tail(directory.path(), 100);

        let first = tail.poll().expect("poll");
        assert!(first.reset && !first.truncated);
        assert_eq!(texts(&first), ["one", "two"]);

        assert!(tail.poll().expect("poll").is_empty());

        append(directory.path(), SECOND, &["three"]);
        let next = tail.poll().expect("poll");
        assert!(!next.reset && !next.truncated);
        assert_eq!(texts(&next), ["three"]);
    }

    #[test]
    fn a_capture_that_does_not_exist_yet_reads_as_an_empty_run() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut tail = tail(&directory.path().join("missing"), 100);

        let first = tail.poll().expect("poll");
        assert!(first.reset && first.lines.is_empty());
        assert!(tail.poll().expect("poll").is_empty());
    }

    #[test]
    fn a_line_the_writer_has_not_finished_is_handed_out_once_it_is_whole() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["whole"]);
        let mut tail = tail(directory.path(), 100);
        tail.poll().expect("poll");

        let pending = line("later");
        let (head, rest) = pending.split_at(10);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(directory.path().join(FIRST))
            .expect("segment");
        file.write_all(head).expect("write");
        assert!(tail.poll().expect("poll").is_empty());

        file.write_all(rest).expect("write");
        assert_eq!(texts(&tail.poll().expect("poll")), ["later"]);
    }

    #[test]
    fn lines_in_a_segment_that_continues_the_run_follow_those_already_handed_out() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["one"]);
        let mut tail = tail(directory.path(), 100);
        tail.poll().expect("poll");

        append(directory.path(), FIRST, &["two"]);
        append(directory.path(), FIRST_CONTINUED, &["three"]);
        let next = tail.poll().expect("poll");
        assert!(!next.reset);
        assert_eq!(texts(&next), ["two", "three"]);

        append(directory.path(), FIRST_CONTINUED, &["four"]);
        assert_eq!(texts(&tail.poll().expect("poll")), ["four"]);
    }

    #[test]
    fn a_new_run_starts_the_view_over_with_only_that_run() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["old"]);
        let mut tail = tail(directory.path(), 100);
        tail.poll().expect("poll");

        append(directory.path(), SECOND, &["new"]);
        let next = tail.poll().expect("poll");
        assert!(next.reset && !next.truncated);
        assert_eq!(texts(&next), ["new"]);

        append(directory.path(), SECOND, &["newer"]);
        let after = tail.poll().expect("poll");
        assert!(!after.reset);
        assert_eq!(texts(&after), ["newer"]);
    }

    #[test]
    fn a_first_read_bounded_by_the_line_limit_keeps_the_newest_lines_and_says_so() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["a", "b"]);
        append(directory.path(), FIRST_CONTINUED, &["c", "d"]);
        let mut tail = tail(directory.path(), 3);

        let first = tail.poll().expect("poll");
        assert_eq!(texts(&first), ["b", "c", "d"]);
        assert!(first.reset && first.truncated);

        append(directory.path(), FIRST_CONTINUED, &["e"]);
        let next = tail.poll().expect("poll");
        assert_eq!(texts(&next), ["e"]);
        assert!(!next.truncated);
    }

    #[test]
    fn a_first_read_of_a_run_that_lost_its_first_segment_says_output_is_missing() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST_CONTINUED, &["rest"]);
        let retention = CaptureRetention {
            removed_segments: 1,
            removed_bytes: 10,
            removed_lines: 4,
            earliest_retained_at: None,
        };
        std::fs::write(
            directory.path().join(ephervex_server::CAPTURE_INDEX_FILE),
            serde_json::to_vec(&retention).expect("index"),
        )
        .expect("write index");
        let mut tail = tail(directory.path(), 100);

        let first = tail.poll().expect("poll");
        assert_eq!(texts(&first), ["rest"]);
        assert!(first.truncated);
    }

    #[test]
    fn retention_removing_the_runs_first_segment_is_reported_without_losing_what_was_shown() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["one"]);
        append(directory.path(), FIRST_CONTINUED, &["two"]);
        let mut tail = tail(directory.path(), 100);
        tail.poll().expect("poll");

        std::fs::remove_file(directory.path().join(FIRST)).expect("prune");
        append(directory.path(), FIRST_CONTINUED, &["three"]);
        let next = tail.poll().expect("poll");
        assert!(!next.reset && next.truncated);
        assert_eq!(texts(&next), ["three"]);

        // The same run carries on, and a segment that is gone is reported once.
        append(directory.path(), FIRST_CONTINUED, &["four"]);
        let after = tail.poll().expect("poll");
        assert!(!after.reset && !after.truncated);
        assert_eq!(texts(&after), ["four"]);
    }

    #[test]
    fn a_segment_removed_before_it_was_read_is_reported_as_missing_output() {
        let directory = tempfile::tempdir().expect("temp dir");
        append(directory.path(), FIRST, &["one"]);
        let mut tail = tail(directory.path(), 100);
        tail.poll().expect("poll");

        std::fs::remove_file(directory.path().join(FIRST)).expect("prune");
        let next = tail.poll().expect("poll");
        assert!(!next.reset && next.truncated);
        assert!(next.lines.is_empty());
    }
}
