//! Starting Minecraft, streaming its output, and tracking its exit
//!
//! # One event stream, two frontends
//!
//! The GUI and CLI consume the same launch events. [`LaunchObserver`] is
//! therefore shaped exactly like `ephervex-auth`'s `AuthObserver` and
//! `ephervex-download`'s `OperationReporter`: an unbounded channel a frontend
//! drains, plus a `silent()` constructor for callers that only want the result.
//! Nothing in this module knows which frontend is listening, and nothing in it
//! can block on a window.
//!
//! Unbounded because a slow consumer must never stall the game: a terminal that
//! is not being read would otherwise apply backpressure all the way to the
//! child's stdout pipe and freeze Minecraft.
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

use std::process::Stdio;

use ephervex_model::{Operation, OperationId};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::error::{LaunchError, LaunchResult};
use crate::launch::LaunchCommand;
use crate::log::{Log4jReader, LogRecord};

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
    /// One line of the game's output.
    OutputLine(OutputLine),
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

/// Receives [`LaunchEvent`]s.
#[derive(Debug, Clone)]
pub struct LaunchObserver {
    sender: Option<mpsc::UnboundedSender<LaunchEvent>>,
}

impl LaunchObserver {
    /// Creates an observer and the receiver a frontend should drain.
    #[must_use]
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<LaunchEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: Some(sender),
            },
            receiver,
        )
    }

    /// An observer that discards everything.
    #[must_use]
    pub const fn silent() -> Self {
        Self { sender: None }
    }

    /// Emits an event, ignoring a receiver that has gone away.
    pub fn emit(&self, event: LaunchEvent) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
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
        let child = Command::new(&command.java)
            .args(command.arguments())
            .current_dir(&command.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // If the supervising task is dropped the game must not be left
            // orphaned with a pipe nobody reads.
            .kill_on_drop(true)
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
    /// Canceling kills the game and reports the signal, rather than detaching
    /// and reporting an exit that did not happen.
    #[instrument(skip_all, fields(pid = self.pid))]
    pub async fn wait(
        mut self,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
    ) -> LaunchResult<GameExit> {
        let stdout = self.child.stdout.take();
        let stderr = self.child.stderr.take();
        let streams = tokio::join!(
            spawn_reader(stdout, OutputStream::Stdout, observer.clone()),
            spawn_reader(stderr, OutputStream::Stderr, observer.clone()),
            self.supervise(cancel)
        );

        let exit = streams.2?;
        observer.emit(LaunchEvent::Exited {
            code: exit.code,
            signal: exit.signal,
        });
        info!(?exit, "minecraft exited");
        Ok(exit)
    }

    /// Waits for the child, killing it if the launch is canceled.
    async fn supervise(&mut self, cancel: &CancellationToken) -> LaunchResult<GameExit> {
        let status = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                debug!(pid = self.pid, "canceled; stopping minecraft");
                if let Err(error) = self.child.kill().await {
                    warn!(pid = self.pid, %error, "could not stop minecraft");
                }
                self.child.wait().await
            }
            status = self.child.wait() => status,
        };

        let status = status.map_err(|source| LaunchError::ProcessLost { source })?;
        Ok(GameExit {
            code: status.code(),
            signal: signal_of(status),
        })
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

/// Reads one pipe to the end, emitting a record per line.
///
/// Reads bytes rather than `str` because a mod, a crash dump, or a resource
/// pack name can put invalid UTF-8 on the pipe, and a launcher that stopped
/// streaming at the first bad byte would hide the crash it was started to
/// capture.
async fn spawn_reader<R>(pipe: Option<R>, stream: OutputStream, observer: LaunchObserver)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let Some(pipe) = pipe else {
        return;
    };
    let mut reader = BufReader::new(pipe);
    let mut parser = Log4jReader::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\n', '\r']);
                for record in parser.push(text) {
                    observer.emit(LaunchEvent::OutputLine(OutputLine { stream, record }));
                }
            }
            Err(error) => {
                warn!(stream = stream.as_str(), %error, "stopped reading the game's output");
                break;
            }
        }
    }

    for record in parser.flush() {
        observer.emit(LaunchEvent::OutputLine(OutputLine { stream, record }));
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
}
