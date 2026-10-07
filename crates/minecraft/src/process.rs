//! Starting Minecraft, streaming its output, and tracking its exit
//!
//! # One event stream, two frontends
//!
//! The GUI and CLI consume the same launch events. [`LaunchObserver`] is
//! therefore shaped exactly like `ephervex-auth`'s `AuthObserver` and
//! `ephervex-download`'s `OperationReporter`: a channel a frontend drains, plus
//! a `silent()` constructor for callers that only want the result. Nothing in
//! this module knows which frontend is listening, and nothing in it can block on
//! a window.
//!
//! Emitting never waits, because a slow consumer must never stall the game: a
//! terminal that is not being read would otherwise apply backpressure all the
//! way to the child's stdout pipe and freeze Minecraft. Output lines that the
//! consumer has not yet taken are capped instead. Past the cap a line is
//! dropped and the next event reports how many were, so memory stays bounded
//! while lifecycle events are always delivered.
//!
//! # The game outlives the call that started it
//!
//! [`GameProcess::start`] returns as soon as the JVM is running, so the pid is
//! available to be recorded in the instance's launch lock *before* anything
//! waits. [`GameProcess::wait`] then supervises it. Splitting the two is what
//! lets a launcher say "running as pid 12345" rather than only being able to
//! say so afterwards.
//!
//! # Canceling kills the game
//!
//! A canceled launch after the JVM has started can only mean "stop the game",
//! and the honest implementation is to kill it and report the signal rather
//! than to detach and pretend it exited. `kill_on_drop` covers the case where
//! the supervising task itself goes away.
//!
//! The game runs in its own process group so that cancel reaches everything it
//! started, not only the JVM.
//!
//! # Finishing does not wait for descendants
//!
//! A launch is over when the game process is. A descendant that inherited the
//! output pipes can outlive it indefinitely, so after the exit the remaining
//! output is drained for a short, fixed time and then reading stops.

use std::fmt::Write as _;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ephervex_model::{Operation, OperationId};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::error::{LaunchError, LaunchResult};
use crate::launch::LaunchCommand;
use crate::log::{Log4jReader, LogRecord};

const MAX_GAME_OUTPUT_LINE_BYTES: usize = 1024 * 1024;

/// How many output lines may wait for a consumer before further ones are dropped.
const MAX_QUEUED_OUTPUT_LINES: usize = 8192;

/// How many bytes of output may wait for a consumer before further lines are dropped.
const MAX_QUEUED_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// How long output keeps being read after the game process has ended.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Which pipe a line arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

impl OutputStream {
    /// A stable, untranslated identifier for logs and `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// One line of the game's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub stream: OutputStream,
    pub record: LogRecord,
}

impl OutputLine {
    /// The line as a terminal should show it.
    #[must_use]
    pub fn display(&self) -> String {
        self.record.display()
    }

    /// What the line costs while it waits in a queue.
    fn queued_bytes(&self) -> usize {
        let record = &self.record;
        let optional = |text: &Option<String>| text.as_ref().map_or(0, String::len);
        std::mem::size_of::<Self>()
            + record.message.len()
            + optional(&record.level)
            + optional(&record.logger)
            + optional(&record.thread)
    }
}

/// How the game finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameExit {
    /// The exit status, or `None` when a signal ended the process.
    pub code: Option<i32>,
    /// The signal that ended it, where one did.
    pub signal: Option<i32>,
}

impl GameExit {
    /// Whether the game exited cleanly.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.code, Some(0))
    }
}

/// What a launch reports as it happens.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LaunchEvent {
    /// Resolving metadata, choosing a runtime, refreshing the session.
    ///
    /// Carries the id every launcher log line for this launch is tagged with,
    /// so a frontend can show it before anything has had a chance to fail.
    Preparing { operation: OperationId },
    /// A snapshot of the download operation. The workspace's one progress
    /// abstraction rather than a second shape invented here.
    Downloading(Box<Operation>),
    /// The command line is built and the JVM is about to start.
    ///
    /// Carries the command so a frontend can show it. Its `Debug` and
    /// `Display` are redacted; `redacted_arguments` is the only form that may
    /// be printed.
    Starting(Box<LaunchCommand>),
    /// The game process is running and recorded as the instance's game, with
    /// the start time that proves which process it is.
    Running { pid: u32 },
    /// One line of the game's output.
    OutputLine(OutputLine),
    /// Output lines were discarded because the consumer fell behind.
    ///
    /// Reported in place, before the next event, so a reader knows where the
    /// gap is.
    OutputDropped { lines: u64 },
    /// The game ended.
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// The launch failed before or during the game.
    ///
    /// Carries a message id rather than a sentence: the frontend resolves it
    /// through `ephervex-i18n`, exactly as it does for a returned error
    Failed {
        message_id: &'static str,
        /// Untranslated technical detail for an expandable interface section
        /// and for the log.
        detail: String,
    },
}

/// Output waiting for a consumer, shared by the observer and the receiver.
#[derive(Debug, Default)]
struct OutputQueue {
    lines: usize,
    bytes: usize,
    dropped: u64,
}

impl OutputQueue {
    /// Whether a line of this size may be queued. A queue that is empty always
    /// accepts one, so an oversized record is delivered rather than lost.
    const fn has_room_for(&self, bytes: usize) -> bool {
        self.lines == 0
            || (self.lines < MAX_QUEUED_OUTPUT_LINES
                && self.bytes.saturating_add(bytes) <= MAX_QUEUED_OUTPUT_BYTES)
    }
}

type SharedQueue = Arc<Mutex<OutputQueue>>;

fn lock(queue: &SharedQueue) -> std::sync::MutexGuard<'_, OutputQueue> {
    // The counters stay meaningful if a holder panicked.
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone)]
struct Link {
    sender: mpsc::UnboundedSender<LaunchEvent>,
    queue: SharedQueue,
}

/// Receives [`LaunchEvent`]s.
#[derive(Debug, Clone)]
pub struct LaunchObserver {
    link: Option<Link>,
}

/// The receiving end of a [`LaunchObserver`].
#[derive(Debug)]
pub struct LaunchEvents {
    receiver: mpsc::UnboundedReceiver<LaunchEvent>,
    queue: SharedQueue,
}

impl LaunchEvents {
    /// Waits for the next event, or `None` once every observer is gone and the
    /// queue is empty.
    pub async fn recv(&mut self) -> Option<LaunchEvent> {
        let event = self.receiver.recv().await?;
        self.release(&event);
        Some(event)
    }

    /// Takes the next event if one is already waiting.
    ///
    /// # Errors
    ///
    /// Returns [`mpsc::error::TryRecvError`] when nothing is waiting or every
    /// observer is gone.
    pub fn try_recv(&mut self) -> Result<LaunchEvent, mpsc::error::TryRecvError> {
        let event = self.receiver.try_recv()?;
        self.release(&event);
        Ok(event)
    }

    fn release(&self, event: &LaunchEvent) {
        if let LaunchEvent::OutputLine(line) = event {
            let mut queue = lock(&self.queue);
            queue.lines = queue.lines.saturating_sub(1);
            queue.bytes = queue.bytes.saturating_sub(line.queued_bytes());
        }
    }
}

impl LaunchObserver {
    /// Creates an observer and the receiver a frontend should drain.
    #[must_use]
    pub fn channel() -> (Self, LaunchEvents) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let queue = SharedQueue::default();
        (
            Self {
                link: Some(Link {
                    sender,
                    queue: Arc::clone(&queue),
                }),
            },
            LaunchEvents { receiver, queue },
        )
    }

    /// An observer that discards everything.
    #[must_use]
    pub const fn silent() -> Self {
        Self { link: None }
    }

    /// Emits an event, ignoring a receiver that has gone away.
    ///
    /// Never waits. Output lines beyond the queue cap are counted and dropped;
    /// every other event is always queued.
    pub fn emit(&self, event: LaunchEvent) {
        let Some(link) = &self.link else {
            return;
        };
        let mut queue = lock(&link.queue);
        if let LaunchEvent::OutputLine(line) = &event {
            let bytes = line.queued_bytes();
            if !queue.has_room_for(bytes) {
                queue.dropped = queue.dropped.saturating_add(1);
                return;
            }
            queue.lines += 1;
            queue.bytes = queue.bytes.saturating_add(bytes);
        }
        if queue.dropped > 0 {
            let lines = std::mem::take(&mut queue.dropped);
            let _ = link.sender.send(LaunchEvent::OutputDropped { lines });
        }
        let _ = link.sender.send(event);
    }

    /// Emits a failure, taking the message id from the error itself.
    pub fn failed(&self, error: &LaunchError) {
        self.emit(LaunchEvent::Failed {
            message_id: error.message_id(),
            detail: error.to_string(),
        });
    }
}

/// A running Minecraft process.
#[derive(Debug)]
pub struct GameProcess {
    child: Child,
    pid: u32,
}

impl GameProcess {
    /// Starts the JVM and returns as soon as it is running.
    ///
    /// Both pipes are captured. Inheriting them would put the game's output on
    /// the launcher's own stdout, which would corrupt `--json` and give the GUI
    /// nothing at all.
    #[instrument(skip_all, fields(java = %command.java.display()))]
    pub fn start(command: &LaunchCommand, observer: &LaunchObserver) -> LaunchResult<Self> {
        std::fs::create_dir_all(&command.working_directory).map_err(|source| {
            LaunchError::GameDirectory {
                path: command.working_directory.clone(),
                source,
            }
        })?;

        observer.emit(LaunchEvent::Starting(Box::new(command.clone())));
        let mut process = Command::new(&command.java);
        process
            .args(command.arguments())
            .current_dir(&command.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // If the supervising task is dropped the game must not be left
            // orphaned with a pipe nobody reads.
            .kill_on_drop(true);
        // A group of its own, so canceling can stop everything the game started.
        #[cfg(unix)]
        process.process_group(0);
        let child = process
            .spawn()
            .map_err(|source| LaunchError::ProcessStart {
                executable: command.java.clone(),
                source,
            })?;

        // A child that has just been spawned always has an id; the `None` case
        // is a child that has already been reaped, which cannot have happened
        // between these two statements.
        let pid = child.id().unwrap_or_default();
        info!(pid, working_directory = %command.working_directory.display(), "minecraft started");
        Ok(Self { child, pid })
    }

    /// The game's process id.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Streams both pipes and waits for the game to end.
    ///
    /// Canceling kills the game and everything it started, and reports the
    /// signal, rather than detaching and reporting an exit that did not happen.
    /// Once the game has ended, output a descendant still holds open is read
    /// only for a short while, so the call returns even if one lingers.
    #[instrument(skip_all, fields(pid = self.pid))]
    pub async fn wait(
        mut self,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
    ) -> LaunchResult<GameExit> {
        let stdout = self.child.stdout.take();
        let stderr = self.child.stderr.take();
        let stop_reading = CancellationToken::new();
        let readers = async {
            tokio::join!(
                read_output(stdout, OutputStream::Stdout, observer, &stop_reading),
                read_output(stderr, OutputStream::Stderr, observer, &stop_reading),
            );
        };
        tokio::pin!(readers);
        let supervised = self.supervise(cancel);
        tokio::pin!(supervised);

        let mut readers_done = false;
        let supervision = loop {
            tokio::select! {
                () = &mut readers, if !readers_done => readers_done = true,
                supervision = &mut supervised => break supervision,
            }
        };

        if !readers_done {
            // A game stopped by cancel has had its whole group killed, so its
            // pipes close on their own and the grace period only has to cover
            // reading what is already buffered.
            let watch_cancel = !matches!(&supervision, Ok(game) if game.killed_by_cancel);
            let grace = tokio::time::sleep(OUTPUT_DRAIN_GRACE);
            tokio::pin!(grace);
            tokio::select! {
                // Readers first, so buffered output wins a tie with the timer.
                biased;
                () = &mut readers => readers_done = true,
                () = &mut grace => {}
                () = cancel.cancelled(), if watch_cancel => {}
            }
            if !readers_done {
                debug!("stopped reading output a descendant still holds open");
                stop_reading.cancel();
                readers.await;
            }
        }

        let exit = supervision?.exit;
        observer.emit(LaunchEvent::Exited {
            code: exit.code,
            signal: exit.signal,
        });
        info!(?exit, "minecraft exited");
        Ok(exit)
    }

    /// Waits for the child, killing it if the launch is canceled.
    async fn supervise(&mut self, cancel: &CancellationToken) -> LaunchResult<Supervised> {
        let mut killed_by_cancel = false;
        let status = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!(pid = self.pid, "canceled; stopping minecraft");
                killed_by_cancel = true;
                self.stop().await;
                self.child.wait().await
            }
            status = self.child.wait() => status,
        };

        let status = status.map_err(|source| LaunchError::ProcessLost { source })?;
        Ok(Supervised {
            exit: GameExit {
                code: status.code(),
                signal: signal_of(status),
            },
            killed_by_cancel,
        })
    }

    /// Kills the game and what it started.
    ///
    /// The group is signaled before the game is reaped, while its id still
    /// cannot belong to anything else.
    async fn stop(&mut self) {
        #[cfg(unix)]
        if kill_group(self.pid) {
            return;
        }
        if let Err(error) = self.child.kill().await {
            warn!(pid = self.pid, %error, "could not stop minecraft");
        }
    }
}

/// How supervision ended.
struct Supervised {
    exit: GameExit,
    killed_by_cancel: bool,
}

/// Kills every process in the game's group, which the game leads.
#[cfg(unix)]
fn kill_group(pid: u32) -> bool {
    use rustix::process::{Pid, Signal, kill_process_group};

    let Some(group) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    match kill_process_group(group, Signal::KILL) {
        Ok(()) => true,
        Err(error) => {
            warn!(pid, %error, "could not stop minecraft's process group");
            false
        }
    }
}

/// The signal that ended a process, where one did.
#[cfg(unix)]
fn signal_of(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
const fn signal_of(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

/// Reads one pipe to the end or until `stop` is canceled, emitting a record per
/// line.
///
/// Reads bytes rather than `str` because a mod, a crash dump, or a resource
/// pack name can put invalid UTF-8 on the pipe, and a launcher that stopped
/// streaming at the first bad byte would hide the crash it was started to
/// capture.
async fn read_output<R>(
    pipe: Option<R>,
    stream: OutputStream,
    observer: &LaunchObserver,
    stop: &CancellationToken,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(pipe) = pipe else {
        return;
    };
    let mut reader = BufReader::new(pipe);
    let mut parser = Log4jReader::new();
    let mut line = Vec::new();
    let emit = |records: Vec<LogRecord>| {
        for record in records {
            observer.emit(LaunchEvent::OutputLine(OutputLine { stream, record }));
        }
    };

    loop {
        let read = tokio::select! {
            biased;
            () = stop.cancelled() => {
                // Whatever arrived without a newline is still output.
                if !line.is_empty() {
                    emit(parser.push(&String::from_utf8_lossy(&line)));
                }
                break;
            }
            read = read_limited_line(&mut reader, &mut line) => read,
        };
        match read {
            Ok(None) => break,
            Ok(Some(omitted_bytes)) => {
                let mut text = String::from_utf8_lossy(&line).into_owned();
                line.clear();
                if omitted_bytes > 0 {
                    let _ = write!(text, " [truncated, {omitted_bytes} bytes omitted]");
                }
                emit(parser.push(&text));
            }
            Err(error) => {
                warn!(stream = stream.as_str(), %error, "stopped reading the game's output");
                break;
            }
        }
    }

    emit(parser.flush());
}

async fn read_limited_line<R>(
    reader: &mut BufReader<R>,
    output: &mut Vec<u8>,
) -> std::io::Result<Option<usize>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    output.clear();
    let mut omitted = 0usize;
    let mut saw_bytes = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if !saw_bytes {
                return Ok(None);
            }
            if omitted == 0 && output.ends_with(b"\r") {
                output.pop();
            }
            return Ok(Some(omitted));
        }
        saw_bytes = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        let room = MAX_GAME_OUTPUT_LINE_BYTES.saturating_sub(output.len());
        let retain = take.min(room);
        output.extend_from_slice(&available[..retain]);
        omitted = omitted.saturating_add(take - retain);
        reader.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            if omitted == 0 && output.ends_with(b"\r") {
                output.pop();
            }
            return Ok(Some(omitted));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::PathBuf;

    use super::*;

    /// A command that runs a shell instead of a JVM.
    ///
    /// The point of the process layer is spawning, streaming, and exit
    /// tracking, none of which care what is on the other end. This keeps process
    /// forbids a test that depends on Minecraft actually starting.
    fn shell(script: &str, working_directory: &std::path::Path) -> LaunchCommand {
        crate::launch::test_command(
            PathBuf::from("/bin/sh"),
            vec!["-c".to_owned(), script.to_owned()],
            working_directory.to_path_buf(),
        )
    }

    #[tokio::test]
    async fn a_process_is_streamed_line_by_line_and_its_exit_code_is_reported() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (observer, mut events) = LaunchObserver::channel();
        let command = shell("echo first; echo second 1>&2; exit 3", directory.path());

        let process = GameProcess::start(&command, &observer).expect("starts");
        assert!(process.pid() > 0, "the pid is available before waiting");
        let exit = process
            .wait(&observer, &CancellationToken::new())
            .await
            .expect("waits");

        assert_eq!(exit.code, Some(3));
        assert!(!exit.is_success());

        drop(observer);
        let mut lines = Vec::new();
        let mut exited = None;
        let mut started = false;
        while let Some(event) = events.recv().await {
            match event {
                LaunchEvent::Starting(_) => started = true,
                LaunchEvent::OutputLine(line) => lines.push((line.stream, line.display())),
                LaunchEvent::Exited { code, .. } => exited = Some(code),
                _ => {}
            }
        }

        assert!(started);
        assert_eq!(exited, Some(Some(3)));
        assert!(lines.contains(&(OutputStream::Stdout, "first".to_owned())));
        assert!(lines.contains(&(OutputStream::Stderr, "second".to_owned())));
    }

    #[tokio::test]
    async fn the_games_log4j_records_are_reassembled_into_readable_lines() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (observer, mut events) = LaunchObserver::channel();
        let script = concat!(
            r#"printf '%s\n' '<log4j:Event logger="a" timestamp="1" level="INFO" thread="main">'; "#,
            r#"printf '%s\n' '<log4j:Message><![CDATA[Setting user: Notch]]></log4j:Message>'; "#,
            r#"printf '%s\n' '</log4j:Event>'"#
        );

        let process =
            GameProcess::start(&shell(script, directory.path()), &observer).expect("starts");
        process
            .wait(&observer, &CancellationToken::new())
            .await
            .expect("waits");

        drop(observer);
        let mut lines = Vec::new();
        while let Some(event) = events.recv().await {
            if let LaunchEvent::OutputLine(line) = event {
                lines.push(line.display());
            }
        }
        assert_eq!(lines, ["[main/INFO] Setting user: Notch"]);
    }

    #[tokio::test]
    async fn canceling_stops_the_game_rather_than_detaching_from_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        let observer = LaunchObserver::silent();
        let cancel = CancellationToken::new();

        let process =
            GameProcess::start(&shell("sleep 60", directory.path()), &observer).expect("starts");
        let pid = process.pid();
        let waiting = tokio::spawn(async move {
            let cancel = cancel;
            cancel.cancel();
            process.wait(&LaunchObserver::silent(), &cancel).await
        });

        let exit = waiting.await.expect("task").expect("waits");
        assert_eq!(exit.code, None, "a signaled process has no exit code");
        assert!(exit.signal.is_some(), "and the signal is reported instead");
        assert!(!std::path::Path::new(&format!("/proc/{pid}/stat")).exists());
    }

    #[tokio::test]
    async fn a_java_executable_that_does_not_exist_names_itself_in_the_failure() {
        let directory = tempfile::tempdir().expect("temp dir");
        let command = crate::launch::test_command(
            PathBuf::from("/nonexistent/bin/java"),
            Vec::new(),
            directory.path().to_path_buf(),
        );

        let error = GameProcess::start(&command, &LaunchObserver::silent())
            .expect_err("there is nothing to start");
        assert!(matches!(error, LaunchError::ProcessStart { .. }));
        assert_eq!(error.message_id(), "error-launch-process-start");
        assert_eq!(
            error.failing_path(),
            Some(PathBuf::from("/nonexistent/bin/java"))
        );
    }

    #[tokio::test]
    async fn output_that_is_not_utf8_does_not_stop_the_stream() {
        // A crash dump or a resource-pack name can put arbitrary bytes on the
        // pipe, and that is exactly when the output matters most.
        let directory = tempfile::tempdir().expect("temp dir");
        let (observer, mut events) = LaunchObserver::channel();
        let process = GameProcess::start(
            &shell(r"printf 'a\xffb\nafter\n'", directory.path()),
            &observer,
        )
        .expect("starts");
        process
            .wait(&observer, &CancellationToken::new())
            .await
            .expect("waits");

        drop(observer);
        let mut lines = Vec::new();
        while let Some(event) = events.recv().await {
            if let LaunchEvent::OutputLine(line) = event {
                lines.push(line.display());
            }
        }
        assert!(lines.iter().any(|line| line.contains("after")));
    }

    #[tokio::test]
    async fn one_unterminated_output_line_has_a_memory_bound() {
        let oversized = vec![b'x'; MAX_GAME_OUTPUT_LINE_BYTES + 4096];
        let mut reader = BufReader::new(oversized.as_slice());
        let mut retained = Vec::new();

        let omitted = read_limited_line(&mut reader, &mut retained)
            .await
            .expect("read")
            .expect("line");
        assert_eq!(retained.len(), MAX_GAME_OUTPUT_LINE_BYTES);
        assert_eq!(omitted, 4096);
        assert!(
            read_limited_line(&mut reader, &mut retained)
                .await
                .expect("eof")
                .is_none()
        );
    }

    /// A shell that leaves a descendant holding both output pipes for `seconds`.
    fn leaves_descendant(seconds: u32) -> String {
        format!("(sleep {seconds}) & echo before; exit 0")
    }

    #[tokio::test]
    async fn a_descendant_holding_the_pipes_does_not_keep_the_launch_open() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (observer, mut events) = LaunchObserver::channel();
        let process =
            GameProcess::start(&shell(&leaves_descendant(5), directory.path()), &observer)
                .expect("starts");

        let started = std::time::Instant::now();
        let exit = process
            .wait(&observer, &CancellationToken::new())
            .await
            .expect("waits");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(exit.code, Some(0));
        drop(observer);
        let mut lines = Vec::new();
        while let Some(event) = events.recv().await {
            if let LaunchEvent::OutputLine(line) = event {
                lines.push(line.display());
            }
        }
        assert_eq!(
            lines,
            ["before"],
            "output before the exit is still delivered"
        );
    }

    #[tokio::test]
    async fn canceling_while_a_descendant_holds_the_pipes_returns_promptly() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (observer, _events) = LaunchObserver::channel();
        let cancel = CancellationToken::new();
        let process =
            GameProcess::start(&shell(&leaves_descendant(5), directory.path()), &observer)
                .expect("starts");

        let canceling = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceling.cancel();
        });
        let started = std::time::Instant::now();
        process.wait(&observer, &cancel).await.expect("waits");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn canceling_stops_what_the_game_started() {
        let directory = tempfile::tempdir().expect("temp dir");
        let marker = directory.path().join("descendant.pid");
        let script = format!(
            "sleep 60 & echo $! > '{}'; wait",
            marker.to_str().expect("utf-8 path")
        );
        let cancel = CancellationToken::new();
        let process =
            GameProcess::start(&shell(&script, directory.path()), &LaunchObserver::silent())
                .expect("starts");

        let canceling = cancel.clone();
        let waiting =
            tokio::spawn(async move { process.wait(&LaunchObserver::silent(), &canceling).await });
        let descendant = loop {
            if let Ok(text) = std::fs::read_to_string(&marker)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        cancel.cancel();
        waiting.await.expect("task").expect("waits");

        // A killed process stays visible as a zombie until its parent reaps it,
        // and init does that asynchronously.
        let gone = || {
            std::fs::read_to_string(format!("/proc/{descendant}/stat"))
                .map_or(true, |stat| stat.contains(") Z "))
        };
        for _ in 0..200 {
            if gone() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the descendant outlived the canceled game");
    }

    #[tokio::test]
    async fn output_nobody_reads_is_bounded_and_the_exit_is_still_delivered() {
        let directory = tempfile::tempdir().expect("temp dir");
        let (observer, mut events) = LaunchObserver::channel();
        let script = "i=0; while [ $i -lt 10000 ]; do printf '%01024d\\n' $i; i=$((i+1)); done";
        let process =
            GameProcess::start(&shell(script, directory.path()), &observer).expect("starts");

        process
            .wait(&observer, &CancellationToken::new())
            .await
            .expect("waits");

        drop(observer);
        let mut lines = 0usize;
        let mut dropped = 0u64;
        let mut exited_after_notice = false;
        while let Some(event) = events.recv().await {
            match event {
                LaunchEvent::OutputLine(_) => lines += 1,
                LaunchEvent::OutputDropped { lines } => dropped += lines,
                LaunchEvent::Exited { .. } => exited_after_notice = true,
                _ => {}
            }
        }
        assert!(
            lines <= MAX_QUEUED_OUTPUT_LINES,
            "{lines} lines were queued"
        );
        assert!(dropped > 0, "the overflow is reported");
        assert_eq!(
            lines as u64 + dropped,
            10_000,
            "every line is delivered or counted"
        );
        assert!(exited_after_notice);
    }

    #[tokio::test]
    async fn a_consumer_that_keeps_up_loses_nothing() {
        let (observer, mut events) = LaunchObserver::channel();
        let line = |text: &str| {
            LaunchEvent::OutputLine(OutputLine {
                stream: OutputStream::Stdout,
                record: LogRecord::plain(text),
            })
        };
        for round in 0..(MAX_QUEUED_OUTPUT_LINES * 3) {
            observer.emit(line(&round.to_string()));
            assert!(matches!(
                events.recv().await,
                Some(LaunchEvent::OutputLine(_))
            ));
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn dropped_output_is_reported_where_the_gap_is() {
        let (observer, mut events) = LaunchObserver::channel();
        let big = "x".repeat(MAX_QUEUED_OUTPUT_BYTES);
        let line = |text: &str| {
            LaunchEvent::OutputLine(OutputLine {
                stream: OutputStream::Stderr,
                record: LogRecord::plain(text),
            })
        };
        observer.emit(line(&big));
        observer.emit(line("lost"));
        observer.emit(line("lost too"));
        assert!(matches!(
            events.recv().await,
            Some(LaunchEvent::OutputLine(_))
        ));
        observer.emit(line("kept"));
        observer.emit(LaunchEvent::Exited {
            code: Some(0),
            signal: None,
        });

        assert!(matches!(
            events.recv().await,
            Some(LaunchEvent::OutputDropped { lines: 2 })
        ));
        assert!(matches!(
            events.recv().await,
            Some(LaunchEvent::OutputLine(_))
        ));
        assert!(matches!(
            events.recv().await,
            Some(LaunchEvent::Exited { .. })
        ));
    }

    #[tokio::test]
    async fn an_oversized_record_is_delivered_when_nothing_else_is_waiting() {
        let (observer, mut events) = LaunchObserver::channel();
        observer.emit(LaunchEvent::OutputLine(OutputLine {
            stream: OutputStream::Stdout,
            record: LogRecord::plain("y".repeat(MAX_QUEUED_OUTPUT_BYTES * 2)),
        }));
        assert!(matches!(
            events.recv().await,
            Some(LaunchEvent::OutputLine(_))
        ));
    }
}
