//! The starting process's side of a hosted client: a runner that owns the game,
//! seen as the events an attached launch would have produced.

use std::process::Stdio;
use std::time::Duration;

use ephervex_minecraft::{
    GameExit, LaunchCommand, LaunchError, LaunchEvent, LaunchObserver, LogRecord, OutputLine,
};
use ephervex_storage::LockError;
use tokio::io::{AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::sync::CancellationToken;

use super::protocol::{
    ClientRunnerMessage, ClientRunnerSpec, HostedLaunchFailure, RUNNER_ARGUMENT, RunnerRequest,
    RunnerSpec,
};
use super::{RunnerExecutable, next_message};
use crate::error::CoreResult;

/// How long a runner that has reported its end may take to exit.
const RUNNER_EXIT_GRACE: Duration = Duration::from_secs(5);

/// A game running under a runner, as its starter follows it.
///
/// Dropping this leaves the runner and the game running; it only closes the
/// starter's end of the pipes.
#[derive(Debug)]
pub(crate) struct HostedGame {
    child: Child,
    requests: Option<ChildStdin>,
    reports: BufReader<ChildStdout>,
    line: Vec<u8>,
    pid: u32,
    kill_requested: bool,
}

impl HostedGame {
    /// Starts a runner for `spec` and returns once its game is running,
    /// passing the events until then to `observer`.
    ///
    /// A cancellation before the game starts is passed on as a kill request,
    /// which the runner acts on as soon as the game exists.
    pub(crate) async fn start(
        executable: &RunnerExecutable,
        spec: &ClientRunnerSpec,
        command: &LaunchCommand,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<Self> {
        let hosted = |detail: String| LaunchError::HostedRun { detail };
        let mut process = Command::new(&executable.path);
        process
            .arg(RUNNER_ARGUMENT)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // The point of a runner: it must outlive this process.
            .kill_on_drop(false);
        if executable.own_group {
            process.process_group(0);
        }
        let mut child = process
            .spawn()
            .map_err(|error| hosted(format!("the runner could not be started: {error}")))?;
        let mut specification = serde_json::to_vec(&RunnerSpec::Client(Box::new(spec.clone())))
            .map_err(|error| hosted(error.to_string()))?;
        specification.push(b'\n');
        let mut requests = child
            .stdin
            .take()
            .ok_or_else(|| hosted("the runner's input was not captured".to_owned()))?;
        requests
            .write_all(&specification)
            .await
            .map_err(|error| hosted(format!("the runner did not take its command: {error}")))?;
        let reports = child
            .stdout
            .take()
            .ok_or_else(|| hosted("the runner's output was not captured".to_owned()))?;
        // What an attached launch says as it starts the process; the runner's own
        // copy of this event stays with the runner.
        observer.emit(LaunchEvent::Starting(Box::new(command.clone())));
        let mut game = Self {
            child,
            requests: Some(requests),
            reports: BufReader::new(reports),
            line: Vec::new(),
            pid: 0,
            kill_requested: false,
        };

        loop {
            let message = tokio::select! {
                message = game.next_report() => message?,
                () = cancel.cancelled(), if !game.kill_requested => {
                    game.request_kill().await;
                    continue;
                }
            };
            match message {
                Some(ClientRunnerMessage::Running { pid }) => {
                    game.pid = pid;
                    return Ok(game);
                }
                Some(ClientRunnerMessage::Finished {
                    result: Err(failure),
                }) => {
                    game.reap().await;
                    return Err(failure_error(failure));
                }
                Some(other) => forward(other, observer),
                None => return Err(lost_runner()),
            }
        }
    }

    /// The game's process id.
    pub(crate) const fn pid(&self) -> u32 {
        self.pid
    }

    /// Follows the game until it ends, killing it when `cancel` fires.
    ///
    /// Returns how it ended, with the first lines each stream printed.
    pub(crate) async fn wait(
        mut self,
        observer: &LaunchObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<(GameExit, Vec<String>)> {
        loop {
            let message = tokio::select! {
                message = self.next_report() => message?,
                () = cancel.cancelled(), if !self.kill_requested => {
                    self.request_kill().await;
                    continue;
                }
            };
            match message {
                Some(ClientRunnerMessage::Finished { result: Ok(end) }) => {
                    self.reap().await;
                    observer.emit(LaunchEvent::Exited {
                        code: end.code,
                        signal: end.signal,
                    });
                    let exit = GameExit {
                        code: end.code,
                        signal: end.signal,
                    };
                    return Ok((exit, end.early_output));
                }
                Some(ClientRunnerMessage::Finished {
                    result: Err(failure),
                }) => {
                    self.reap().await;
                    return Err(failure_error(failure));
                }
                Some(other) => forward(other, observer),
                None => return Err(lost_runner()),
            }
        }
    }

    async fn next_report(&mut self) -> CoreResult<Option<ClientRunnerMessage>> {
        loop {
            match next_message(&mut self.reports, &mut self.line).await {
                Ok(true) => {}
                Ok(false) => return Ok(None),
                Err(error) => {
                    return Err(LaunchError::HostedRun {
                        detail: format!("the runner's report was unreadable: {error}"),
                    }
                    .into());
                }
            }
            if let Ok(message) = serde_json::from_slice(&self.line) {
                return Ok(Some(message));
            }
            tracing::warn!("ignored a runner message this version does not understand");
        }
    }

    async fn request_kill(&mut self) {
        self.kill_requested = true;
        let Some(requests) = self.requests.as_mut() else {
            return;
        };
        let Ok(mut line) = serde_json::to_vec(&RunnerRequest::Kill) else {
            return;
        };
        line.push(b'\n');
        if requests.write_all(&line).await.is_err() || requests.flush().await.is_err() {
            self.requests = None;
        }
    }

    /// Reaps the runner, which is on its way out, so it does not linger as a zombie.
    async fn reap(&mut self) {
        self.requests = None;
        let _ = tokio::time::timeout(RUNNER_EXIT_GRACE, self.child.wait()).await;
    }
}

/// Passes an event of the running game on as the event an attached launch emits.
fn forward(message: ClientRunnerMessage, observer: &LaunchObserver) {
    match message {
        ClientRunnerMessage::Output { stream, record } => {
            observer.emit(LaunchEvent::OutputLine(OutputLine {
                stream: stream.into(),
                record: LogRecord::from(record),
            }));
        }
        ClientRunnerMessage::OutputDropped { lines } => {
            observer.emit(LaunchEvent::OutputDropped { lines });
        }
        ClientRunnerMessage::Running { .. } | ClientRunnerMessage::Finished { .. } => {}
    }
}

fn lost_runner() -> crate::CoreError {
    LaunchError::ProcessLost {
        source: std::io::Error::other("the runner ended without reporting how the game ended"),
    }
    .into()
}

fn failure_error(failure: HostedLaunchFailure) -> crate::CoreError {
    match failure {
        HostedLaunchFailure::LockLost { path } => LockError::Lost { path }.into(),
        HostedLaunchFailure::ProcessStart { executable, cause } => LaunchError::ProcessStart {
            executable,
            source: std::io::Error::other(cause),
        }
        .into(),
        HostedLaunchFailure::GameDirectory { path, cause } => LaunchError::GameDirectory {
            path,
            source: std::io::Error::other(cause),
        }
        .into(),
        HostedLaunchFailure::Other(detail) => LaunchError::HostedRun { detail }.into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::runner::shell_game;

    fn spec(root: &Path) -> ClientRunnerSpec {
        ClientRunnerSpec {
            command: shell_game("true", root),
            instance_root: root.to_path_buf(),
            additional: None,
            previous: ephervex_storage::LaunchState {
                owner_pid: 1,
                owner_start_ticks: None,
                game_pid: None,
                game_start_ticks: None,
                since: chrono::Utc::now(),
                runner: None,
            },
            run: uuid::Uuid::new_v4(),
            capture_directory: root.join("capture"),
            cache_directory: root.join("cache"),
        }
    }

    fn command(root: &Path) -> LaunchCommand {
        LaunchCommand::from_hand_off(spec(root).command)
    }

    /// A runner stand-in: a script that reads the specification line, then
    /// follows `body`, which prints messages and may read further requests.
    fn runner(directory: &Path, body: &str) -> RunnerExecutable {
        let path: PathBuf = directory.join("runner.sh");
        std::fs::write(&path, format!("#!/bin/sh\nread -r specification\n{body}\n"))
            .expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        for _ in 0..200 {
            let probe = std::process::Command::new(&path)
                .stdin(Stdio::null())
                .output();
            if !matches!(&probe, Err(error) if error.raw_os_error() == Some(26)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        RunnerExecutable {
            path,
            own_group: false,
        }
    }

    fn line(message: &ClientRunnerMessage) -> String {
        format!(
            "printf '%s\\n' '{}'\n",
            serde_json::to_string(message).expect("serialize")
        )
    }

    fn finished(code: i32) -> ClientRunnerMessage {
        ClientRunnerMessage::Finished {
            result: Ok(super::super::protocol::GameEnd {
                code: Some(code),
                signal: None,
                early_output: vec!["early".to_owned()],
            }),
        }
    }

    #[tokio::test]
    async fn a_hosted_game_is_followed_to_its_end_as_an_attached_launch_would_be() {
        let directory = tempfile::tempdir().expect("directory");
        let body = [
            line(&ClientRunnerMessage::Running { pid: 4242 }),
            line(&ClientRunnerMessage::Output {
                stream: super::super::protocol::WireStream::Stdout,
                record: super::super::protocol::WireRecord {
                    level: Some("INFO".to_owned()),
                    logger: None,
                    thread: Some("main".to_owned()),
                    message: "Setting user".to_owned(),
                },
            }),
            line(&finished(3)),
        ]
        .concat();
        let (observer, mut events) = LaunchObserver::channel();
        let cancel = CancellationToken::new();

        let game = HostedGame::start(
            &runner(directory.path(), &body),
            &spec(directory.path()),
            &command(directory.path()),
            &observer,
            &cancel,
        )
        .await
        .expect("the game runs");
        assert_eq!(game.pid(), 4242);
        assert!(
            matches!(events.recv().await, Some(LaunchEvent::Starting(_))),
            "a launch announces that it is starting before anything else"
        );
        let (exit, early) = game.wait(&observer, &cancel).await.expect("the game ends");

        assert_eq!(exit.code, Some(3));
        assert_eq!(early, ["early"]);
        let Some(LaunchEvent::OutputLine(output)) = events.recv().await else {
            panic!("the game's line is replayed");
        };
        assert_eq!(output.record.message, "Setting user");
        assert_eq!(output.record.thread.as_deref(), Some("main"));
        assert!(matches!(
            events.recv().await,
            Some(LaunchEvent::Exited { code: Some(3), .. })
        ));
    }

    #[tokio::test]
    async fn canceling_asks_the_runner_to_kill_the_game_and_waits_for_its_report() {
        let directory = tempfile::tempdir().expect("directory");
        let body = format!(
            "{}read -r request\ncase \"$request\" in *kill*) {} ;; esac",
            line(&ClientRunnerMessage::Running { pid: 7 }),
            line(&ClientRunnerMessage::Finished {
                result: Ok(super::super::protocol::GameEnd {
                    code: None,
                    signal: Some(9),
                    early_output: Vec::new(),
                }),
            })
            .trim_end(),
        );
        let observer = LaunchObserver::silent();
        let cancel = CancellationToken::new();
        let game = HostedGame::start(
            &runner(directory.path(), &body),
            &spec(directory.path()),
            &command(directory.path()),
            &observer,
            &cancel,
        )
        .await
        .expect("the game runs");

        cancel.cancel();
        let (exit, _) = game.wait(&observer, &cancel).await.expect("the game ends");

        assert_eq!(exit.signal, Some(9), "the runner was asked to kill it");
    }

    #[tokio::test]
    async fn a_runner_that_cannot_take_the_lock_fails_the_launch_as_a_lost_lock() {
        let directory = tempfile::tempdir().expect("directory");
        let body = line(&ClientRunnerMessage::Finished {
            result: Err(HostedLaunchFailure::LockLost {
                path: PathBuf::from("/instance/.launcher/launch.lock"),
            }),
        });

        let error = HostedGame::start(
            &runner(directory.path(), &body),
            &spec(directory.path()),
            &command(directory.path()),
            &LaunchObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("no game");

        assert_eq!(error.message_id(), "error-launch-lock-lost", "{error}");
    }

    #[tokio::test]
    async fn a_runner_that_vanishes_is_reported_as_losing_track_of_the_game() {
        let directory = tempfile::tempdir().expect("directory");
        let body = line(&ClientRunnerMessage::Running { pid: 7 });
        let game = HostedGame::start(
            &runner(directory.path(), &body),
            &spec(directory.path()),
            &command(directory.path()),
            &LaunchObserver::silent(),
            &CancellationToken::new(),
        )
        .await
        .expect("the game runs");

        let error = game
            .wait(&LaunchObserver::silent(), &CancellationToken::new())
            .await
            .expect_err("no end was reported");

        assert_eq!(error.message_id(), "error-launch-process-lost", "{error}");
    }

    #[tokio::test]
    async fn the_runner_receives_the_launch_on_its_standard_input() {
        let directory = tempfile::tempdir().expect("directory");
        let seen = directory.path().join("specification");
        let body = format!(
            "printf '%s' \"$specification\" > {}\n{}",
            seen.display(),
            line(&finished(0)),
        );
        let observer = LaunchObserver::silent();
        let cancel = CancellationToken::new();
        // The runner reports the end without a game, which `start` treats as an
        // end before the game ran; the specification was read either way.
        let _ = HostedGame::start(
            &runner(directory.path(), &body),
            &spec(directory.path()),
            &command(directory.path()),
            &observer,
            &cancel,
        )
        .await;

        let text = std::fs::read_to_string(&seen).expect("the runner wrote what it read");
        assert!(text.contains("\"host\":\"client\""), "{text}");
        assert!(text.contains("\"instance_root\""), "{text}");
    }
}
