//! Progress reporting on top of `ephervex_model::Operation`.
//!
//! There is exactly one progress abstraction in the workspace, and downloads
//! are not allowed a private one. The manager mutates an `Operation` and pushes
//! whole snapshots to a [`ProgressSink`]; the GUI renders the snapshot and the
//! CLI prints it, and neither has to know that a download produced it. That is
//! what lets the same import run headless or windowed without a second code
//! path.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ephervex_model::{Operation, OperationId, OperationStatus, Progress};
use tokio::sync::mpsc;

/// Receives a full snapshot of an operation whenever it changes.
///
/// Snapshots rather than deltas: a frontend that misses one update must still
/// end up displaying the truth, and a GUI that repaints from the latest value
/// is far harder to get wrong than one replaying an event log.
pub trait ProgressSink: fmt::Debug + Send + Sync {
    fn operation_changed(&self, operation: &Operation);
}

/// A [`ProgressSink`] that forwards snapshots down an unbounded channel.
///
/// Unbounded because a slow consumer must never be able to stall a download,
/// and the reporter's own throttling already bounds the message rate.
#[derive(Debug)]
pub struct ChannelSink {
    sender: mpsc::UnboundedSender<Operation>,
}

impl ChannelSink {
    /// Creates the sink and the receiver a frontend should drain.
    #[must_use]
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<Operation>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Arc::new(Self { sender }), receiver)
    }
}

impl ProgressSink for ChannelSink {
    fn operation_changed(&self, operation: &Operation) {
        // A dropped receiver means the frontend went away; the download itself
        // is still worth finishing, so the send failure is ignored.
        let _ = self.sender.send(operation.clone());
    }
}

#[derive(Debug)]
struct ReporterState {
    operation: Operation,
    last_emit: Option<Instant>,
}

#[derive(Debug)]
struct ReporterInner {
    state: Mutex<ReporterState>,
    sink: Option<Arc<dyn ProgressSink>>,
    min_interval: Duration,
}

/// Owns one [`Operation`] and publishes it as it changes.
///
/// Cloning is cheap and shares the same operation, so concurrent downloads in a
/// batch can all report into one progress bar.
#[derive(Debug, Clone)]
pub struct OperationReporter {
    inner: Arc<ReporterInner>,
}

impl OperationReporter {
    /// Shortest gap between two byte-progress emissions.
    ///
    /// A 200 MB download delivers thousands of chunks; forwarding every one of
    /// them would spend more time waking the UI thread than writing to disk.
    /// Terminal transitions bypass the throttle.
    const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(100);

    /// Creates a reporter for a new operation.
    ///
    /// `title` is a message id resolved by the frontend through
    /// `ephervex-i18n`, not English text.
    #[must_use]
    pub fn new(title: impl Into<String>, cancellable: bool, sink: Arc<dyn ProgressSink>) -> Self {
        Self::build(title, cancellable, Some(sink))
    }

    /// A reporter with no sink, for callers that only want the operation id for
    /// log correlation.
    #[must_use]
    pub fn silent(title: impl Into<String>) -> Self {
        Self::build(title, true, None)
    }

    fn build(
        title: impl Into<String>,
        cancellable: bool,
        sink: Option<Arc<dyn ProgressSink>>,
    ) -> Self {
        Self {
            inner: Arc::new(ReporterInner {
                state: Mutex::new(ReporterState {
                    operation: Operation::new(title, cancellable),
                    last_emit: None,
                }),
                sink,
                min_interval: Self::DEFAULT_MIN_INTERVAL,
            }),
        }
    }

    /// The id every log line for this download should carry.
    #[must_use]
    pub fn id(&self) -> OperationId {
        self.with_state(|state| state.operation.id)
    }

    /// The current operation, for a caller that wants to poll rather than
    /// subscribe.
    #[must_use]
    pub fn snapshot(&self) -> Operation {
        self.with_state(|state| state.operation.clone())
    }

    /// Marks the operation running and names what it is doing.
    pub fn begin(&self, step: Option<String>) {
        self.update(true, |state| {
            state.operation.status = OperationStatus::Running;
            state.operation.current_step = step;
        });
    }

    /// Updates the human-readable step without touching progress.
    pub fn set_step(&self, step: Option<String>) {
        self.update(true, |state| state.operation.current_step = step);
    }

    /// Publishes byte or file progress. Throttled.
    pub fn set_progress(&self, progress: Progress) {
        self.update(false, |state| state.operation.progress = progress);
    }

    /// Names the phase now running, as a message id the frontend resolves.
    ///
    /// Emitted immediately: a stage change is the thing a waiting user is
    /// watching for, and throttling it would be throttling the answer.
    pub fn set_stage(&self, stage: Option<String>) {
        self.update(true, |state| {
            state.operation.stage = stage;
            // The counts belonged to the phase that just ended.
            state.operation.items = Progress::indeterminate();
            state.operation.current_step = None;
        });
    }

    /// Publishes how many of the current stage's units are done. Throttled.
    pub fn set_items(&self, items: Progress) {
        self.update(false, |state| state.operation.items = items);
    }

    /// Publishes a completed unit and what it was, as one throttled update.
    ///
    /// One call rather than [`Self::set_items`] followed by [`Self::set_step`]
    /// because the two describe the same event: a batch that published them
    /// separately would emit the unthrottled step for every file it finished,
    /// which for a batch of already-cached artifacts is thousands of
    /// notifications in a tight loop. Stated together, the count and the name
    /// can never disagree either.
    pub fn set_items_at(&self, items: Progress, step: Option<String>) {
        self.update(false, |state| {
            state.operation.items = items;
            state.operation.current_step = step;
        });
    }

    /// Moves the operation to a terminal state and emits immediately.
    pub fn finish(&self, status: OperationStatus) {
        self.update(true, |state| {
            if status == OperationStatus::Completed {
                // A completed operation whose bar stopped at 97% because the
                // last chunk was throttled looks like a hang.
                if let Some(total) = state.operation.progress.total {
                    state.operation.progress.current = total;
                }
            }
            state.operation.status = status;
        });
    }

    fn with_state<R>(&self, read: impl FnOnce(&ReporterState) -> R) -> R {
        // A poisoned lock here means some other thread panicked mid-update. The
        // operation is a progress snapshot, not an invariant-bearing structure,
        // so recovering is strictly better than propagating the panic into a
        // download that is otherwise fine.
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        read(&state)
    }

    fn update(&self, force: bool, mutate: impl FnOnce(&mut ReporterState)) {
        let Some(sink) = self.inner.sink.as_ref() else {
            // Still apply the mutation so `snapshot` stays accurate.
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            mutate(&mut state);
            return;
        };

        let snapshot = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            mutate(&mut state);
            let now = Instant::now();
            let due = force
                || state
                    .last_emit
                    .is_none_or(|last| now.duration_since(last) >= self.inner.min_interval);
            if !due {
                return;
            }
            state.last_emit = Some(now);
            state.operation.clone()
        };
        // Emitted outside the lock: a sink that blocks must not block the
        // download's write loop while holding the state mutex.
        sink.operation_changed(&snapshot);
    }
}

/// Shared byte counter for a batch of downloads.
///
/// Files in a batch complete out of order, so per-file percentages cannot be
/// composed into a batch percentage. Summing bytes can, provided every file
/// contributes exactly once - hence [`ByteTally::adjust`], which takes a file's
/// new absolute contribution rather than a delta, so a restarted attempt
/// correctly *removes* the bytes it had previously claimed.
#[derive(Debug, Default)]
pub struct ByteTally {
    downloaded: AtomicU64,
    /// `None` when at least one file's size is unknown: an honest
    /// indeterminate bar beats a percentage that jumps backwards
    /// (`Progress::total` is optional for exactly this reason).
    total: Option<u64>,
}

impl ByteTally {
    /// Creates a tally whose total is the sum of the known sizes, or `None` if
    /// any size is unknown.
    #[must_use]
    pub fn new(sizes: &[Option<u64>]) -> Self {
        let total = sizes
            .iter()
            .try_fold(0_u64, |sum, size| Some(sum.saturating_add((*size)?)));
        Self {
            downloaded: AtomicU64::new(0),
            total,
        }
    }

    /// Replaces one file's contribution and returns the batch progress.
    pub fn adjust(&self, previous: u64, current: u64) -> Progress {
        let downloaded = if current >= previous {
            self.downloaded
                .fetch_add(current - previous, Ordering::Relaxed)
                + (current - previous)
        } else {
            self.downloaded
                .fetch_sub(previous - current, Ordering::Relaxed)
                - (previous - current)
        };
        Progress {
            current: downloaded,
            total: self.total,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[derive(Debug, Default)]
    struct RecordingSink {
        seen: Mutex<Vec<Operation>>,
    }

    impl RecordingSink {
        fn snapshots(&self) -> Vec<Operation> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl ProgressSink for RecordingSink {
        fn operation_changed(&self, operation: &Operation) {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(operation.clone());
        }
    }

    #[test]
    fn a_silent_reporter_still_tracks_state_for_polling() {
        let reporter = OperationReporter::silent("download");
        reporter.begin(Some("mod.jar".to_owned()));
        reporter.finish(OperationStatus::Completed);
        let snapshot = reporter.snapshot();
        assert_eq!(snapshot.status, OperationStatus::Completed);
        assert_eq!(snapshot.current_step.as_deref(), Some("mod.jar"));
    }

    #[test]
    fn terminal_transitions_are_always_published() {
        let sink = Arc::new(RecordingSink::default());
        let reporter = OperationReporter::new("download", true, sink.clone());
        reporter.begin(None);
        // These are throttled and may be coalesced away.
        reporter.set_progress(Progress::new(1, 10));
        reporter.set_progress(Progress::new(2, 10));
        reporter.finish(OperationStatus::Completed);

        let snapshots = sink.snapshots();
        let last = snapshots.last().expect("at least one snapshot");
        assert_eq!(last.status, OperationStatus::Completed);
    }

    #[test]
    fn a_completed_operation_reports_a_full_bar() {
        let reporter = OperationReporter::silent("download");
        reporter.set_progress(Progress::new(3, 10));
        reporter.finish(OperationStatus::Completed);
        assert_eq!(reporter.snapshot().progress, Progress::new(10, 10));
    }

    #[test]
    fn a_canceled_operation_does_not_pretend_to_be_complete() {
        let reporter = OperationReporter::silent("download");
        reporter.set_progress(Progress::new(3, 10));
        reporter.finish(OperationStatus::Canceled);
        assert_eq!(reporter.snapshot().progress, Progress::new(3, 10));
    }

    #[test]
    fn every_operation_gets_its_own_id_for_log_correlation() {
        let a = OperationReporter::silent("download");
        let b = OperationReporter::silent("download");
        assert_ne!(a.id(), b.id());
        assert_eq!(a.id(), a.clone().id());
    }

    #[test]
    fn a_batch_total_is_unknown_when_any_file_size_is_unknown() {
        assert_eq!(ByteTally::new(&[Some(1), Some(2)]).total, Some(3));
        assert_eq!(ByteTally::new(&[Some(1), None]).total, None);
    }

    #[test]
    fn a_restarted_file_removes_the_bytes_it_had_already_claimed() {
        let tally = ByteTally::new(&[Some(100), Some(100)]);
        assert_eq!(tally.adjust(0, 40).current, 40);
        assert_eq!(tally.adjust(0, 30).current, 70);
        // The first file's attempt collapsed and restarts from zero.
        assert_eq!(tally.adjust(40, 0).current, 30);
    }
}
