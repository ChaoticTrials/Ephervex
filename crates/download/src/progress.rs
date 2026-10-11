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
use std::thread;
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
    /// A change the throttle has not published yet.
    held_back: bool,
    /// Whether a thread is already waiting to publish a held-back change.
    trailing_scheduled: bool,
    /// Whether `operation.items` is a download batch's count rather than one
    /// its caller keeps, so a later single download knows it is stale.
    items_from_batch: bool,
}

#[derive(Debug)]
struct ReporterInner {
    state: Mutex<ReporterState>,
    /// Held while publishing, so snapshots reach the sink in the order the
    /// state changed and a stale one never follows a newer one.
    publishing: Mutex<()>,
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
        Self::build_with_interval(title, cancellable, sink, Self::DEFAULT_MIN_INTERVAL)
    }

    fn build_with_interval(
        title: impl Into<String>,
        cancellable: bool,
        sink: Option<Arc<dyn ProgressSink>>,
        min_interval: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(ReporterInner {
                state: Mutex::new(ReporterState {
                    operation: Operation::new(title, cancellable),
                    last_emit: None,
                    held_back: false,
                    trailing_scheduled: false,
                    items_from_batch: false,
                }),
                publishing: Mutex::new(()),
                sink,
                min_interval,
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
    ///
    /// A finished operation stays finished: only its owner ends it, and a late
    /// stage cannot reopen it.
    pub fn begin(&self, step: Option<String>) {
        self.update(true, |state| {
            state.operation.status = OperationStatus::Running;
            state.operation.current_step = step;
        });
    }

    /// Marks the operation running in a named phase, as one published change.
    pub fn begin_stage(&self, stage: String) {
        self.update(true, |state| {
            state.operation.status = OperationStatus::Running;
            enter_stage(state, Some(stage));
        });
    }

    /// Publishes an update the throttle held back right away.
    ///
    /// A held-back update is also published once the throttle interval has
    /// passed, so calling this is only needed to publish before then.
    pub fn flush(&self) {
        let held_back = {
            let mut state = self.lock_state();
            // A finished operation was published when it finished.
            !state.operation.status.is_terminal() && std::mem::take(&mut state.held_back)
        };
        if held_back {
            self.publish(true);
        }
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
        self.update(true, |state| enter_stage(state, stage));
    }

    /// Publishes how many of the current stage's units are done. Throttled.
    pub fn set_items(&self, items: Progress) {
        self.update(false, |state| {
            state.operation.items = items;
            state.items_from_batch = false;
        });
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
            state.items_from_batch = false;
        });
    }

    /// Publishes a download batch's count of finished files. Throttled.
    ///
    /// The count belongs to the batch: a single download that follows it
    /// starts without it, see [`Self::begin_single_download`].
    pub(crate) fn set_batch_items(&self, items: Progress) {
        self.update(false, |state| {
            state.operation.items = items;
            state.items_from_batch = true;
        });
    }

    /// [`Self::set_batch_items`] together with the file that just landed.
    pub(crate) fn set_batch_items_at(&self, items: Progress, step: Option<String>) {
        self.update(false, |state| {
            state.operation.items = items;
            state.operation.current_step = step;
            state.items_from_batch = true;
        });
    }

    /// Names the file a single download is fetching.
    ///
    /// A count the caller published itself stays, because it counts across
    /// single downloads. One left behind by a download batch is dropped: the
    /// single downloads that follow a batch are another stage, and showing them
    /// beside the batch's total would be wrong.
    pub(crate) fn begin_single_download(&self, step: Option<String>) {
        self.update(true, |state| {
            state.operation.current_step = step;
            if std::mem::take(&mut state.items_from_batch) {
                state.operation.items = Progress::indeterminate();
            }
        });
    }

    /// Moves the operation to a terminal state and emits immediately. Only
    /// the first call has an effect.
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
        read(&self.lock_state())
    }

    fn update(&self, force: bool, mutate: impl FnOnce(&mut ReporterState)) {
        let due = {
            let mut state = self.lock_state();
            // A finished operation stays finished: its owner ends it once, and
            // a late stage can neither reopen nor rewrite it.
            if state.operation.status.is_terminal() {
                return;
            }
            mutate(&mut state);
            let now = Instant::now();
            let due = force
                || state
                    .last_emit
                    .is_none_or(|last| now.duration_since(last) >= self.inner.min_interval);
            // Claimed here, so concurrent updates in the same window do not all publish.
            if due {
                state.last_emit = Some(now);
            }
            state.held_back = !due;
            if !due {
                self.ensure_trailing_publisher(&mut state);
            }
            due
        };
        if due {
            self.publish(force);
        }
    }

    /// Sends the state as it is now, outside the state lock.
    ///
    /// Publishing is serialized so a stale snapshot never follows a newer one.
    /// A forced change waits its turn; a throttled one never blocks a download's
    /// write loop and is held back for the next publish instead.
    fn publish(&self, force: bool) {
        let Some(sink) = self.inner.sink.as_ref() else {
            return;
        };
        let _order = if force {
            self.inner
                .publishing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        } else {
            match self.inner.publishing.try_lock() {
                Ok(order) => order,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let mut state = self.lock_state();
                    state.held_back = true;
                    self.ensure_trailing_publisher(&mut state);
                    return;
                }
            }
        };
        let snapshot = {
            let mut state = self.lock_state();
            // Whatever was held back is in this snapshot.
            state.held_back = false;
            state.operation.clone()
        };
        sink.operation_changed(&snapshot);
    }

    /// Makes sure a held-back change is published once the throttle interval
    /// has passed, so the last change before quiet work is never left unseen.
    ///
    /// A plain thread rather than a runtime timer: the reporter is driven from
    /// async tasks, blocking pools and plain threads alike. The thread lives
    /// only while something is held back.
    fn ensure_trailing_publisher(&self, state: &mut ReporterState) {
        if state.trailing_scheduled || self.inner.sink.is_none() {
            return;
        }
        state.trailing_scheduled = true;
        let reporter = self.clone();
        let spawned = thread::Builder::new()
            .name("progress-trailing".to_owned())
            .spawn(move || reporter.publish_held_back_updates());
        if spawned.is_err() {
            // Without a thread, the next update or flush publishes it as before.
            state.trailing_scheduled = false;
        }
    }

    fn publish_held_back_updates(&self) {
        loop {
            let wait = {
                let mut state = self.lock_state();
                if !state.held_back || state.operation.status.is_terminal() {
                    state.trailing_scheduled = false;
                    return;
                }
                state.last_emit.map_or(Duration::ZERO, |last| {
                    self.inner.min_interval.saturating_sub(last.elapsed())
                })
            };
            if !wait.is_zero() {
                thread::sleep(wait);
                continue;
            }
            let Some(sink) = self.inner.sink.as_ref() else {
                return;
            };
            let _order = self
                .inner
                .publishing
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let snapshot = {
                let mut state = self.lock_state();
                // A forced change may have published it while this waited.
                (state.held_back && !state.operation.status.is_terminal()).then(|| {
                    state.held_back = false;
                    state.last_emit = Some(Instant::now());
                    state.operation.clone()
                })
            };
            if let Some(snapshot) = snapshot {
                sink.operation_changed(&snapshot);
            }
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ReporterState> {
        // A poisoned lock means another thread panicked mid-update. The
        // operation is a progress snapshot, not an invariant-bearing structure,
        // so recovering beats propagating the panic into a working download.
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

/// Starts a phase: the counts, bytes and step belonged to the one that ended.
fn enter_stage(reporter_state: &mut ReporterState, stage: Option<String>) {
    let operation = &mut reporter_state.operation;
    operation.stage = stage;
    operation.items = Progress::indeterminate();
    operation.progress = Progress::indeterminate();
    operation.current_step = None;
    reporter_state.items_from_batch = false;
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
    fn a_finished_operation_cannot_be_reopened_or_rewritten() {
        let sink = Arc::new(RecordingSink::default());
        let reporter = OperationReporter::new("download", true, sink.clone());
        reporter.begin(Some("first".to_owned()));
        reporter.finish(OperationStatus::Failed("lost".to_owned()));
        let published = sink.snapshots().len();

        reporter.begin(Some("second".to_owned()));
        reporter.set_step(Some("third".to_owned()));
        reporter.set_items(Progress::new(1, 1));
        reporter.finish(OperationStatus::Completed);

        let snapshot = reporter.snapshot();
        assert_eq!(snapshot.status, OperationStatus::Failed("lost".to_owned()));
        assert_eq!(snapshot.current_step.as_deref(), Some("first"));
        assert_eq!(
            sink.snapshots().len(),
            published,
            "nothing more was published"
        );
    }

    #[test]
    fn flushing_publishes_only_an_update_the_throttle_held_back() {
        let sink = Arc::new(RecordingSink::default());
        let reporter = OperationReporter::build_with_interval(
            "download",
            true,
            Some(sink.clone()),
            Duration::from_secs(3600),
        );
        reporter.begin(None);
        reporter.flush();
        assert_eq!(sink.snapshots().len(), 1, "nothing was held back yet");

        reporter.set_items(Progress::new(7, 8));
        reporter.set_items(Progress::new(8, 8));
        assert_ne!(
            sink.snapshots().pop().expect("begin").items,
            Progress::new(8, 8)
        );
        reporter.flush();

        let snapshots = sink.snapshots();
        assert_eq!(snapshots.len(), 2);
        let last = snapshots.last().expect("flushed");
        assert_eq!(last.items, Progress::new(8, 8));
        assert_eq!(last.status, OperationStatus::Running);
    }

    fn wait_for(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        condition()
    }

    #[test]
    fn a_held_back_update_is_published_once_the_interval_passes_without_a_flush() {
        let sink = Arc::new(RecordingSink::default());
        let reporter = OperationReporter::build_with_interval(
            "verify",
            true,
            Some(sink.clone()),
            Duration::from_millis(30),
        );
        reporter.begin(None);
        reporter.set_items(Progress::new(7, 8));
        reporter.set_items(Progress::new(8, 8));

        assert!(
            wait_for(|| sink
                .snapshots()
                .last()
                .is_some_and(|last| last.items == Progress::new(8, 8))),
            "the last count reached the sink with no further call"
        );
    }

    #[test]
    fn a_trailing_publish_does_not_follow_a_finished_operation() {
        let sink = Arc::new(RecordingSink::default());
        let reporter = OperationReporter::build_with_interval(
            "verify",
            true,
            Some(sink.clone()),
            Duration::from_millis(30),
        );
        reporter.begin(None);
        reporter.set_items(Progress::new(1, 2));
        reporter.finish(OperationStatus::Completed);
        let published = sink.snapshots().len();

        thread::sleep(Duration::from_millis(150));
        let snapshots = sink.snapshots();
        assert_eq!(snapshots.len(), published, "nothing followed the finish");
        assert_eq!(
            snapshots.last().expect("finished").status,
            OperationStatus::Completed
        );
    }

    #[test]
    fn an_update_that_lost_the_publishing_race_is_still_published() {
        #[derive(Debug, Default)]
        struct GatedSink {
            gate_open: Mutex<bool>,
            opened: std::sync::Condvar,
            entered: Mutex<bool>,
            entered_signal: std::sync::Condvar,
            inner: RecordingSink,
        }

        impl ProgressSink for GatedSink {
            fn operation_changed(&self, operation: &Operation) {
                let first = {
                    let mut entered = self.entered.lock().unwrap();
                    !std::mem::replace(&mut *entered, true)
                };
                self.entered_signal.notify_all();
                if first {
                    let mut open = self.gate_open.lock().unwrap();
                    while !*open {
                        open = self.opened.wait(open).unwrap();
                    }
                }
                self.inner.operation_changed(operation);
            }
        }

        let sink = Arc::new(GatedSink::default());
        let reporter = OperationReporter::build_with_interval(
            "verify",
            true,
            Some(sink.clone()),
            Duration::from_millis(20),
        );
        let publisher = {
            let reporter = reporter.clone();
            thread::spawn(move || reporter.begin(None))
        };
        // The first publish now holds the publishing order inside the sink.
        {
            let mut entered = sink.entered.lock().unwrap();
            while !*entered {
                entered = sink.entered_signal.wait(entered).unwrap();
            }
        }
        thread::sleep(Duration::from_millis(40));
        reporter.set_items(Progress::new(5, 5));

        *sink.gate_open.lock().unwrap() = true;
        sink.opened.notify_all();
        publisher.join().unwrap();

        assert!(
            wait_for(|| sink
                .inner
                .snapshots()
                .last()
                .is_some_and(|last| last.items == Progress::new(5, 5))),
            "the update that could not publish was published afterwards"
        );
    }

    #[test]
    fn a_single_download_after_a_batch_does_not_show_the_batchs_count() {
        let reporter = OperationReporter::silent("download");
        reporter.begin(None);
        reporter.set_batch_items_at(Progress::new(37, 200), Some("a.jar".to_owned()));
        reporter.begin_single_download(Some("b.jar".to_owned()));

        let snapshot = reporter.snapshot();
        assert_eq!(snapshot.items, Progress::indeterminate());
        assert_eq!(snapshot.current_step.as_deref(), Some("b.jar"));
    }

    #[test]
    fn a_single_download_keeps_the_count_its_caller_publishes() {
        let reporter = OperationReporter::silent("verify");
        reporter.begin(None);
        reporter.set_items_at(Progress::new(3, 10), Some("a.jar".to_owned()));
        reporter.begin_single_download(Some("b.jar".to_owned()));

        assert_eq!(reporter.snapshot().items, Progress::new(3, 10));
    }

    #[test]
    fn a_count_the_caller_publishes_after_a_batch_is_not_cleared_by_a_single_download() {
        let reporter = OperationReporter::silent("download");
        reporter.begin(None);
        reporter.set_batch_items(Progress::new(5, 5));
        reporter.set_items(Progress::new(1, 4));
        reporter.begin_single_download(None);

        assert_eq!(reporter.snapshot().items, Progress::new(1, 4));
    }

    #[test]
    fn a_stage_change_forgets_that_the_count_came_from_a_batch() {
        let reporter = OperationReporter::silent("download");
        reporter.begin(None);
        reporter.set_batch_items(Progress::new(5, 5));
        reporter.set_stage(Some("stage".to_owned()));
        reporter.set_items(Progress::new(2, 9));
        reporter.begin_single_download(None);

        assert_eq!(reporter.snapshot().items, Progress::new(2, 9));
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
