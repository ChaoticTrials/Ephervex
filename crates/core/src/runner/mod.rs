//! Runners: small detached processes that own a client or a server for as long
//! as it runs, so the Ephervex process that started it can exit and later find
//! it again.
//!
//! # Shape
//!
//! A frontend that is asked to keep a host running past its own exit starts the
//! same executable again with [`RUNNER_ARGUMENT`]. The starting process
//! prepares everything that needs the application, such as accounts, Java, and
//! installed files, and hands the finished command to the runner on standard
//! input. The runner puts itself in a session of its own and hosts the process
//! with the same supervisor an attached run uses. Ending a terminal, the
//! starting process, or its session therefore does not reach it.
//!
//! The runner reports what a supervisor emits as one JSON message per line on
//! its standard output, and the starting process turns them back into the
//! events an attached run would have produced. That pipe is a convenience: when
//! the starter is gone the runner stops writing to it and carries on, and a
//! later Ephervex process finds the host through the control channel, the
//! runtime evidence, and the output capture on disk, as it does for any host.

mod client_host;
mod hosted_game;
mod protocol;
mod relay;
mod server_host;

use std::ffi::OsString;
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ephervex_server::{ServerError, ServerExit, ServerObserver, ServerResult};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;

use crate::TerminationRequests;

#[cfg(test)]
pub(crate) use client_host::host_client;
pub(crate) use hosted_game::HostedGame;
pub use protocol::RUNNER_ARGUMENT;
pub(crate) use protocol::{ClientRunnerSpec, RunnerSpec, ServerRunnerMessage, ServerRunnerSpec};

use protocol::MAX_MESSAGE_BYTES;

/// How long a runner stays up after answering its last control requests, so the
/// answers reach the sockets they were written to before the process ends.
pub(super) const CONTROL_ANSWER_GRACE: Duration = Duration::from_millis(250);

/// Where a host runs relative to the process that started it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Hosting {
    /// The starting process supervises the host. Ending that process ends the
    /// host or leaves it unsupervised.
    #[default]
    Attached,
    /// A runner of its own supervises the host. It keeps running when the
    /// starting process exits, and any later Ephervex process can control it.
    Detached,
}

/// Runs a runner if this process was started as one.
///
/// A frontend calls this first thing in `main`, before reading its own
/// arguments. `None` means this is an ordinary start. `Some` is the exit status
/// of a runner that has finished.
pub fn run_if_requested(arguments: impl IntoIterator<Item = OsString>) -> Option<u8> {
    let mut arguments = arguments.into_iter().skip(1);
    let requested = arguments.next()? == RUNNER_ARGUMENT;
    if !requested || arguments.next().is_some() {
        return None;
    }
    Some(run_runner())
}

fn run_runner() -> u8 {
    // A session of its own, so a closing terminal or a signal sent to the
    // starter's process group does not reach it. Failing only means the process
    // already leads a group, which keeps it out of the terminal's reach anyway.
    if let Err(error) = rustix::process::setsid() {
        tracing::debug!(%error, "the runner is already a session leader");
    }
    let mut input = std::io::stdin().lock();
    let spec = match read_spec(&mut input) {
        Ok(spec) => spec,
        Err(error) => {
            eprintln!("ephervex runner: {error}");
            return 2;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("ephervex runner: {error}");
            return 1;
        }
    };
    match spec {
        RunnerSpec::Server(spec) => {
            drop(input);
            runtime.block_on(server_host::host_server(spec, tokio::io::stdout()))
        }
        RunnerSpec::Client(spec) => {
            // The reading thread takes the lock itself; the input already
            // buffered stays in the shared standard input.
            drop(input);
            let requests = request_channel();
            runtime.block_on(async {
                // Watched before the game starts, so a request that arrives
                // while it starts is not lost to the default disposition.
                let terminations = match TerminationRequests::watch() {
                    Ok(requests) => Some(termination_channel(requests)),
                    Err(error) => {
                        tracing::warn!(%error, "termination requests are not watched by the runner");
                        None
                    }
                };
                client_host::host_client(*spec, tokio::io::stdout(), requests, terminations).await
            })
        }
    }
}

/// Reads the specification, which is the first line of the runner's input.
fn read_spec(input: &mut impl std::io::BufRead) -> Result<RunnerSpec, String> {
    let mut body = Vec::new();
    // One byte past the limit tells a specification that is too large from one
    // that is exactly that size.
    input
        .by_ref()
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', &mut body)
        .map_err(|error| format!("the specification could not be read: {error}"))?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err("the specification is too large".to_owned());
    }
    serde_json::from_slice(&body).map_err(|error| format!("the specification is invalid: {error}"))
}

/// Turns what the starter writes after the specification into messages on a
/// channel. The channel closes when the starter's end of the pipe does.
fn request_channel() -> tokio::sync::mpsc::UnboundedReceiver<protocol::RunnerRequest> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let spawned = std::thread::Builder::new()
        .name("runner-requests".to_owned())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut line = Vec::new();
            loop {
                line.clear();
                let read = input
                    .by_ref()
                    .take(MAX_MESSAGE_BYTES as u64 + 1)
                    .read_until(b'\n', &mut line);
                if !matches!(read, Ok(count) if count > 0 && line.len() <= MAX_MESSAGE_BYTES) {
                    return;
                }
                // A request this version does not understand is ignored.
                if let Ok(request) = serde_json::from_slice(&line)
                    && sender.send(request).is_err()
                {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "the runner cannot be asked to stop its game");
    }
    receiver
}

/// Turns the process's termination requests into messages on a channel.
fn termination_channel(
    mut requests: TerminationRequests,
) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            requests.next().await;
            if sender.send(()).is_err() {
                return;
            }
        }
    });
    receiver
}

/// A game that is a shell script: a runner does not care what the process is,
/// only that it is started, supervised, and ended.
#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn shell_game(
    script: &str,
    working_directory: &Path,
) -> ephervex_minecraft::CommandHandOff {
    serde_json::from_value(serde_json::json!({
        "java": "/bin/sh",
        "working_directory": working_directory,
        "main_class": "",
        "arguments": [
            {"value": "-c", "sensitive": false},
            {"value": script, "sensitive": false},
        ],
        "jvm_argument_count": 2,
        "environment": [],
    }))
    .expect("a command")
}

/// What starts a runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunnerExecutable {
    pub path: PathBuf,
    /// Whether the runner must begin in a process group of its own, so a
    /// terminal's Ctrl-C aimed at the starter's group does not reach it. A
    /// packaged image's runtime keeps the mount alive under the runner, and a
    /// signal that ended it would pull the runner's program away.
    pub own_group: bool,
}

/// The running program's own image, which stays runnable after the file it came
/// from has been replaced by an upgrade or a rebuild.
const OWN_IMAGE: &str = "/proc/self/exe";

/// What runs a runner: the image of this very process, so the runner is the
/// build that started it, or the packaged image it was started from.
///
/// An AppImage's files exist only while the image's runtime process is alive,
/// and that process ends with the application. A runner started from the files
/// themselves would lose its program when the application exits. Starting the
/// image again gives the runner a runtime process and a mount of its own, which
/// last as long as it does.
pub(crate) fn runner_executable() -> std::io::Result<RunnerExecutable> {
    let current = std::env::current_exe()?;
    Ok(executable_for(
        &current,
        std::env::var_os("APPDIR").as_deref(),
        std::env::var_os("APPIMAGE").as_deref(),
    ))
}

fn executable_for(
    current: &Path,
    mount: Option<&std::ffi::OsStr>,
    image: Option<&std::ffi::OsStr>,
) -> RunnerExecutable {
    match packaged_image(current, mount, image) {
        Some(path) => RunnerExecutable {
            path,
            own_group: true,
        },
        None => RunnerExecutable {
            path: PathBuf::from(OWN_IMAGE),
            own_group: false,
        },
    }
}

/// The image `current` runs out of, when it is a file inside a mounted image.
fn packaged_image(
    current: &Path,
    mount: Option<&std::ffi::OsStr>,
    image: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let mount = Path::new(mount?);
    let image = PathBuf::from(image?);
    (mount.is_absolute() && current.starts_with(mount) && image.is_absolute() && image.is_file())
        .then_some(image)
}

/// Starts a runner for `spec`, and returns when the server it supervises has
/// ended, translating the runner's messages into `observer`'s events.
///
/// `held` is released once the runner owns the server's runtime lock, which is
/// the first thing that keeps other changes out. Dropping this future leaves
/// the runner and its server running.
pub(crate) async fn run_server<H: Send>(
    executable: &RunnerExecutable,
    spec: &ServerRunnerSpec,
    held: H,
    observer: &ServerObserver,
) -> ServerResult<ServerExit> {
    let hosted = |detail: String| ServerError::HostedRun { detail };
    let mut held = Some(held);
    let mut command = Command::new(&executable.path);
    command
        .arg(RUNNER_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // The point of a runner: it must outlive this future and this process.
        .kill_on_drop(false);
    if executable.own_group {
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| hosted(format!("the runner could not be started: {error}")))?;

    let mut specification = serde_json::to_vec(&RunnerSpec::Server(spec.clone()))
        .map_err(|error| hosted(error.to_string()))?;
    specification.push(b'\n');
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(&specification)
            .await
            .map_err(|error| hosted(format!("the runner did not take its command: {error}")))?;
        // Closed so the runner sees the end of its specification.
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| hosted("the runner's output was not captured".to_owned()))?;

    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut finished = None;
    loop {
        match next_message(&mut reader, &mut line).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                return Err(hosted(format!(
                    "the runner's report was unreadable: {error}"
                )));
            }
        }
        let Ok(message) = serde_json::from_slice::<ServerRunnerMessage>(&line) else {
            tracing::warn!("ignored a runner message this version does not understand");
            continue;
        };
        match message {
            ServerRunnerMessage::Settled => drop(held.take()),
            ServerRunnerMessage::Finished { result } => {
                finished = Some(result);
                break;
            }
            other => {
                if let Some(event) = other.into_event() {
                    observer.emit(event);
                }
            }
        }
    }
    drop(held);
    // Reaped so it does not linger as a zombie; it is on its way out.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
    match finished {
        Some(result) => result.map_err(ServerError::from),
        None => Err(hosted(
            "the runner ended without reporting how the server ended".to_owned(),
        )),
    }
}

/// Reads the next newline-terminated message into `line`.
///
/// `Ok(false)` is the end of the stream. A message longer than the limit is an
/// error rather than something to buffer.
pub(super) async fn next_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        line.extend_from_slice(&available[..take]);
        reader.consume(take + usize::from(newline.is_some()));
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a message exceeded the size limit",
            ));
        }
        if newline.is_some() {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::fmt::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use ephervex_server::{
        CaptureStream, CapturedLine, ControlAction, ControlResponse, ServerCommand, ServerEvent,
        classify_runtime, send_control,
    };

    use super::*;

    fn spec_running(root: &Path, script: &str) -> ServerRunnerSpec {
        std::fs::create_dir_all(root.join("server")).expect("server directory");
        ServerRunnerSpec {
            command: ServerCommand {
                java: PathBuf::from("/bin/sh"),
                arguments: vec!["-c".to_owned(), script.to_owned()],
                working_directory: root.join("server"),
            },
            server_root: root.to_path_buf(),
            capture_directory: root.join("capture"),
        }
    }

    /// An executable that stands in for a runner by printing `messages`, one
    /// per line, and ignoring everything it is given.
    fn fake_runner(directory: &Path, messages: &[ServerRunnerMessage]) -> PathBuf {
        let mut script = String::from("#!/bin/sh\ncat >/dev/null\n");
        for message in messages {
            let line = serde_json::to_string(message).expect("serialize");
            writeln!(script, "printf '%s\\n' '{line}'").expect("write to a string");
        }
        install_script(&directory.join("runner.sh"), &script)
    }

    /// Writes an executable script, and returns once it can be executed.
    ///
    /// A process forked by a concurrent test can briefly hold a copy of the
    /// descriptor the script was written through, and executing a file that is
    /// open for writing fails with "text file busy".
    fn install_script(path: &Path, body: &str) -> PathBuf {
        std::fs::write(path, body).expect("write the stand-in");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        for _ in 0..200 {
            match std::process::Command::new(path)
                .stdin(Stdio::null())
                .output()
            {
                Err(error) if error.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                _ => break,
            }
        }
        path.to_path_buf()
    }

    fn executable(path: &Path) -> RunnerExecutable {
        RunnerExecutable {
            path: path.to_path_buf(),
            own_group: false,
        }
    }

    fn line(text: &str) -> CapturedLine {
        CapturedLine::from_bytes(CaptureStream::Stdout, text.as_bytes())
    }

    fn drain(events: &mut ephervex_server::ServerEvents) -> Vec<ServerEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event);
        }
        seen
    }

    #[test]
    fn a_runner_is_requested_only_by_its_exact_argument() {
        let args = |rest: &[&str]| {
            std::iter::once("ephervex")
                .chain(rest.iter().copied())
                .map(OsString::from)
                .collect::<Vec<_>>()
        };
        assert!(run_if_requested(args(&[])).is_none());
        assert!(run_if_requested(args(&["server", "start"])).is_none());
        assert!(run_if_requested(args(&[RUNNER_ARGUMENT, "extra"])).is_none());
        assert!(run_if_requested(args(&["extra", RUNNER_ARGUMENT])).is_none());
    }

    #[test]
    fn a_runner_starts_from_the_image_only_when_the_program_lives_inside_its_mount() {
        let directory = tempfile::tempdir().expect("directory");
        let image = directory.path().join("Ephervex.AppImage");
        std::fs::write(&image, b"image").expect("write the image");
        let mount = Path::new("/tmp/.mount_Ephervex");
        let inside = mount.join("usr/bin/ephervex-gui");
        let as_os = |path: &Path| Some(path.as_os_str().to_owned());

        assert_eq!(
            packaged_image(&inside, as_os(mount).as_deref(), as_os(&image).as_deref()),
            Some(image.clone())
        );
        // A program outside the mount, such as a development build run from a
        // shell that inherited the variables, starts from itself.
        assert_eq!(
            packaged_image(
                Path::new("/usr/bin/ephervex"),
                as_os(mount).as_deref(),
                as_os(&image).as_deref()
            ),
            None
        );
        assert_eq!(
            packaged_image(&inside, None, as_os(&image).as_deref()),
            None
        );
        assert_eq!(packaged_image(&inside, as_os(mount).as_deref(), None), None);
        assert_eq!(
            packaged_image(
                &inside,
                as_os(mount).as_deref(),
                as_os(&directory.path().join("gone.AppImage")).as_deref()
            ),
            None
        );
    }

    #[test]
    fn a_runner_outside_a_packaged_image_starts_from_the_running_image_not_its_path() {
        // The path a replaced binary leaves behind ends in " (deleted)", and
        // starting that fails; the running image itself still starts.
        let replaced = Path::new("/usr/bin/ephervex-gui (deleted)");

        let executable = executable_for(replaced, None, None);

        assert_eq!(executable.path, Path::new("/proc/self/exe"));
        assert!(!executable.own_group);
    }

    #[test]
    fn a_specification_over_the_size_limit_is_refused_without_being_parsed() {
        let oversized = vec![b' '; MAX_MESSAGE_BYTES + 1];
        let error = read_spec(&mut oversized.as_slice()).expect_err("too large");
        assert!(error.contains("too large"), "{error}");
        assert!(read_spec(&mut &b"not json"[..]).is_err());
    }

    #[test]
    fn a_failure_keeps_the_kind_the_starter_tells_apart() {
        for error in [
            ServerError::AlreadyRunning,
            ServerError::RuntimeMayBeAlive,
            ServerError::RestartReplacementFailed {
                source: Box::new(ServerError::NotInstalled),
            },
            ServerError::ProcessStart {
                executable: PathBuf::from("/opt/java/bin/java"),
                source: std::io::Error::other("permission denied"),
            },
            ServerError::RuntimeState {
                path: PathBuf::from("/srv/.launcher/server-runtime.json"),
                source: std::io::Error::other("read-only file system"),
            },
            ServerError::Io {
                path: PathBuf::from("/srv/server"),
                source: std::io::Error::other("no space left"),
            },
            ServerError::ControlUnavailable {
                path: PathBuf::from("/run/evx/control.sock"),
                source: std::io::Error::other("refused"),
            },
            ServerError::ProcessIo(std::io::Error::other("broken pipe")),
        ] {
            let carried = ServerError::from(protocol::HostedFailure::from(&error));
            assert_eq!(
                std::mem::discriminant(&carried),
                std::mem::discriminant(&error),
                "{error:?} became {carried:?}"
            );
        }
    }

    #[test]
    fn a_failure_with_its_own_message_arrives_with_that_message_and_its_path() {
        let error = ServerError::ProcessStart {
            executable: PathBuf::from("/opt/java/bin/java"),
            source: std::io::Error::other("permission denied"),
        };

        let carried = ServerError::from(protocol::HostedFailure::from(&error));

        assert_eq!(carried.message_id(), error.message_id());
        assert_eq!(carried.failing_path(), error.failing_path());
    }

    #[test]
    fn a_failed_restart_keeps_the_inner_failure_its_path_and_its_reason() {
        let error = ServerError::RestartReplacementFailed {
            source: Box::new(ServerError::ProcessStart {
                executable: PathBuf::from("/opt/java/bin/java"),
                source: std::io::Error::other("permission denied"),
            }),
        };

        let carried = ServerError::from(protocol::HostedFailure::from(&error));

        assert_eq!(carried.failing_path(), error.failing_path());
        let ServerError::RestartReplacementFailed { source } = &carried else {
            panic!("expected a failed restart, got {carried:?}");
        };
        let ServerError::ProcessStart { source, .. } = source.as_ref() else {
            panic!("expected the inner start failure, got {source:?}");
        };
        assert!(source.to_string().contains("permission denied"));
    }

    #[test]
    fn any_other_failure_arrives_as_text_the_user_can_read() {
        let carried = ServerError::from(protocol::HostedFailure::from(&ServerError::NotInstalled));

        let ServerError::HostedRun { detail } = carried else {
            panic!("expected a hosted-run failure, got {carried:?}");
        };
        assert!(detail.contains("not installed"), "{detail}");
    }

    #[tokio::test]
    async fn the_starter_replays_a_runners_messages_as_the_events_an_attached_run_emits() {
        let directory = tempfile::tempdir().expect("directory");
        let exit = ServerExit {
            code: Some(0),
            signal: None,
            finished_starting: true,
            stop_requested: true,
        };
        let runner = fake_runner(
            directory.path(),
            &[
                ServerRunnerMessage::Settled,
                ServerRunnerMessage::Started { pid: 4242 },
                ServerRunnerMessage::Output {
                    line: line("Done (1s)!"),
                },
                ServerRunnerMessage::Exited { exit },
                ServerRunnerMessage::Finished { result: Ok(exit) },
            ],
        );
        let (observer, mut events) = ServerObserver::channel();
        let spec = spec_running(directory.path(), "true");

        let result = run_server(&executable(&runner), &spec, (), &observer).await;

        assert_eq!(result.expect("the run ended"), exit);
        let seen = drain(&mut events);
        assert!(
            matches!(seen[0], ServerEvent::Started { pid: 4242 }),
            "{seen:?}"
        );
        assert!(matches!(&seen[1], ServerEvent::Output(output) if output.text == "Done (1s)!"));
        assert!(matches!(seen[2], ServerEvent::Exited(reported) if reported == exit));
        assert_eq!(
            seen.len(),
            3,
            "settling and finishing are not events: {seen:?}"
        );
    }

    #[tokio::test]
    async fn the_starters_hold_on_the_server_is_released_when_the_runner_settles_not_when_it_ends()
    {
        let directory = tempfile::tempdir().expect("directory");
        let settled = serde_json::to_string(&ServerRunnerMessage::Settled).expect("serialize");
        let finished = serde_json::to_string(&ServerRunnerMessage::Finished {
            result: Err(protocol::HostedFailure::AlreadyRunning),
        })
        .expect("serialize");
        let runner = install_script(
            &directory.path().join("slow-runner.sh"),
            &format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{settled}'\nsleep 3\nprintf '%s\\n' '{finished}'\n"
            ),
        );
        let (released, was_released) = tokio::sync::oneshot::channel::<()>();
        let spec = spec_running(directory.path(), "true");

        let run = tokio::spawn(async move {
            run_server(
                &executable(&runner),
                &spec,
                released,
                &ServerObserver::silent(),
            )
            .await
        });

        let hold = tokio::time::timeout(std::time::Duration::from_secs(2), was_released)
            .await
            .expect("the hold is released while the runner is still going");
        assert!(hold.is_err(), "the sender was dropped, not used");
        assert!(
            !run.is_finished(),
            "the run is still waiting for the runner"
        );
        let result = run.await.expect("run task");
        assert!(
            matches!(result, Err(ServerError::AlreadyRunning)),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_runner_that_ends_without_reporting_an_end_is_an_error_not_a_clean_exit() {
        let directory = tempfile::tempdir().expect("directory");
        let runner = fake_runner(directory.path(), &[ServerRunnerMessage::Started { pid: 1 }]);
        let spec = spec_running(directory.path(), "true");

        let result = run_server(&executable(&runner), &spec, (), &ServerObserver::silent()).await;

        assert!(
            matches!(result, Err(ServerError::HostedRun { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_runner_that_cannot_be_started_is_reported_with_its_reason() {
        let directory = tempfile::tempdir().expect("directory");
        let spec = spec_running(directory.path(), "true");

        let result = run_server(
            &executable(&directory.path().join("missing-runner")),
            &spec,
            (),
            &ServerObserver::silent(),
        )
        .await;

        let Err(ServerError::HostedRun { detail }) = result else {
            panic!("expected a hosted-run failure, got {result:?}");
        };
        assert!(detail.contains("could not be started"), "{detail}");
    }

    #[tokio::test]
    async fn a_hosted_server_reports_its_whole_run_and_answers_its_control_channel() {
        let directory = tempfile::tempdir().expect("directory");
        let spec = spec_running(
            directory.path(),
            "echo 'Done (1s)! For help, type \"help\"'; while read -r line; do [ \"$line\" = stop ] && exit 0; done",
        );
        let root = spec.server_root.clone();
        let (reader, writer) = tokio::io::duplex(64 * 1024);

        let hosting = tokio::spawn(server_host::host_server(spec, writer));
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        let mut messages = Vec::new();
        while messages
            .iter()
            .all(|message| !matches!(message, ServerRunnerMessage::Started { .. }))
        {
            assert!(next_message(&mut reader, &mut buffer).await.expect("read"));
            messages.push(serde_json::from_slice::<ServerRunnerMessage>(&buffer).expect("message"));
        }
        assert_eq!(messages[0], ServerRunnerMessage::Settled);
        assert!(matches!(
            classify_runtime(&root),
            Ok(ephervex_server::RuntimeClassification::Controlled { .. })
        ));

        let response = send_control(&root, &ControlAction::Stop)
            .await
            .expect("stop");
        assert!(
            matches!(response, ControlResponse::Exited(_)),
            "{response:?}"
        );

        let mut last = None;
        while next_message(&mut reader, &mut buffer).await.expect("read") {
            last = Some(serde_json::from_slice::<ServerRunnerMessage>(&buffer).expect("message"));
        }
        let Some(ServerRunnerMessage::Finished { result: Ok(exit) }) = last else {
            panic!("the last message ends the run, got {last:?}");
        };
        assert!(exit.stop_requested && exit.finished_starting, "{exit:?}");
        assert_eq!(hosting.await.expect("host task"), 0);
    }

    #[tokio::test]
    async fn a_hosted_server_keeps_running_after_the_reader_of_its_messages_is_gone() {
        let directory = tempfile::tempdir().expect("directory");
        let spec = spec_running(
            directory.path(),
            "while read -r line; do case \"$line\" in stop) exit 0;; *) echo \"echo $line\";; esac; done",
        );
        let root = spec.server_root.clone();
        let capture = spec.capture_directory.clone();
        let (reader, writer) = tokio::io::duplex(1024);

        let hosting = tokio::spawn(server_host::host_server(spec, writer));
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        while next_message(&mut reader, &mut buffer).await.expect("read") {
            if matches!(
                serde_json::from_slice(&buffer),
                Ok(ServerRunnerMessage::Started { .. })
            ) {
                break;
            }
        }
        drop(reader);

        // Output produced after the starter left still reaches the capture.
        let response = send_control(&root, &ControlAction::Console("after".to_owned()))
            .await
            .expect("console");
        assert_eq!(response, ControlResponse::Accepted);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut tail = crate::server_capture::ServerCaptureTail::new(capture, 100);
            loop {
                let update = tail.poll().expect("capture poll");
                if update.lines.iter().any(|line| line.text == "echo after") {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("output captured after the reader left");
        assert!(matches!(
            classify_runtime(&root),
            Ok(ephervex_server::RuntimeClassification::Controlled { .. })
        ));

        send_control(&root, &ControlAction::Stop)
            .await
            .expect("stop");
        assert_eq!(hosting.await.expect("host task"), 0);
    }

    /// Runs a hosted server until it reports how it ended, returning that and
    /// the channel termination requests are sent on.
    async fn host_until_finished(
        script: &str,
        root: &Path,
        receiver: tokio::sync::mpsc::UnboundedReceiver<()>,
        after_start: impl AsyncFnOnce(),
    ) -> ServerExit {
        let spec = spec_running(root, script);
        let (reader, writer) = tokio::io::duplex(64 * 1024);
        let hosting = tokio::spawn(server_host::host_server_until(spec, writer, Some(receiver)));
        let mut reader = BufReader::new(reader);
        let mut buffer = Vec::new();
        let mut after_start = Some(after_start);
        let mut finished = None;
        while next_message(&mut reader, &mut buffer).await.expect("read") {
            match serde_json::from_slice(&buffer).expect("message") {
                ServerRunnerMessage::Started { .. } => {
                    if let Some(after_start) = after_start.take() {
                        after_start().await;
                    }
                }
                ServerRunnerMessage::Finished { result } => finished = Some(result),
                _ => {}
            }
            if finished.is_some() {
                break;
            }
        }
        hosting.await.expect("host task");
        finished.expect("finished").expect("the run ended")
    }

    #[tokio::test]
    async fn a_termination_request_stops_the_hosted_server_gracefully() {
        let directory = tempfile::tempdir().expect("directory");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        let exit = host_until_finished(
            "while read -r line; do [ \"$line\" = stop ] && exit 0; done",
            directory.path(),
            receiver,
            async move || {
                sender.send(()).expect("request");
            },
        )
        .await;

        assert!(exit.stop_requested && exit.code == Some(0), "{exit:?}");
    }

    #[tokio::test]
    async fn a_second_termination_request_kills_a_server_that_does_not_stop() {
        let directory = tempfile::tempdir().expect("directory");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        let exit = host_until_finished(
            "trap '' TERM; while true; do sleep 0.1; done",
            directory.path(),
            receiver,
            async move || {
                sender.send(()).expect("first request");
                // The stop is written and ignored; only the second request ends it.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                sender.send(()).expect("second request");
            },
        )
        .await;

        assert!(exit.stop_requested, "{exit:?}");
        assert_eq!(exit.signal, Some(9), "{exit:?}");
    }
}
