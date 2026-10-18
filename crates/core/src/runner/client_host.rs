//! The runner side of a hosted client: the process that owns the game for as
//! long as it runs, and the launch lock with it.

use ephervex_download::{ArtifactCache, CacheLayout};
use ephervex_minecraft::{
    GameProcess, LaunchCommand, LaunchError, LaunchEvent, LaunchObserver, OutputStream,
};
use ephervex_server::{
    CaptureStream, ControlAction, ControlRequest, ControlResponse, ControlSocket, OutputCapture,
    ServerExit,
};
use ephervex_storage::{LAUNCH_RUN_VARIABLE, LaunchLock, LockError, RunnerHost};
use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::protocol::{
    ClientRunnerMessage, ClientRunnerSpec, GameEnd, HostedLaunchFailure, RunnerRequest, WireRecord,
};
use super::relay::Relay;

/// What a hosted game is being asked to do, and what it has been told so far.
#[derive(Default)]
struct Ending {
    /// The game was sent a termination request, which it is trusted to honor.
    asked_to_stop: bool,
    /// Requests waiting for the game to exit.
    waiters: Vec<oneshot::Sender<ControlResponse>>,
}

/// Supervises one client's game for as long as it runs, reporting to `out`.
///
/// `requests` carries what the starter asks of the runner after the hand-off.
/// A closed channel means the starter is gone, which asks for nothing.
/// `terminations` carries the runner's own termination requests: the first
/// ends the game politely and a later one kills it.
pub(crate) async fn host_client<W>(
    spec: ClientRunnerSpec,
    out: W,
    requests: mpsc::UnboundedReceiver<RunnerRequest>,
    terminations: Option<mpsc::UnboundedReceiver<()>>,
) -> u8
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut relay = Relay::new(out);
    // Taken before the starter's own lease can end, and held until the game is
    // gone, so a collection never removes a file the game's classpath names.
    let lease = match ArtifactCache::new(CacheLayout::new(&spec.cache_directory))
        .lease()
        .await
    {
        Ok(lease) => lease,
        Err(error) => {
            let failure = HostedLaunchFailure::Other(crate::ErrorChain(&error).to_string());
            return finish_failed(relay, failure).await;
        }
    };
    let (control_requests, control) = mpsc::unbounded_channel();
    let socket = match ControlSocket::open(&spec.instance_root, spec.run, control_requests) {
        Ok(socket) => socket,
        Err(error) => return finish_failed(relay, HostedLaunchFailure::from_control(&error)).await,
    };
    let lock = match take_over_lock(&spec, &socket).await {
        Ok(lock) => lock,
        Err(failure) => return finish_failed(relay, failure).await,
    };

    let command = LaunchCommand::from_hand_off(spec.command.clone())
        .with_environment(LAUNCH_RUN_VARIABLE, spec.run.to_string());
    let (observer, events) = LaunchObserver::channel();
    let process = match GameProcess::start(&command, &observer) {
        Ok(process) => process,
        Err(error) => return finish_failed(relay, HostedLaunchFailure::from_launch(&error)).await,
    };
    let pid = process.pid();
    let lock = match record_game(lock, pid).await {
        Ok(lock) => lock,
        // The process is dropped here, which ends the game this runner no
        // longer holds the instance for.
        Err(failure) => return finish_failed(relay, failure).await,
    };
    relay.send(&ClientRunnerMessage::Running { pid }).await;

    let pump = tokio::spawn(pump_output(events, spec.capture_directory.clone(), relay));
    let mut ending = Ending::default();
    let mut channels = Channels {
        control,
        requests,
        terminations,
    };
    let outcome = supervise(process, &observer, &mut channels, &mut ending).await;
    drop(observer);
    let Ok(mut relay) = pump.await else {
        return 1;
    };
    let Channels { mut control, .. } = channels;

    let result = match outcome {
        Ok((exit, early_output)) => Ok(GameEnd {
            code: exit.code,
            signal: exit.signal,
            early_output,
        }),
        Err(error) => Err(HostedLaunchFailure::from_launch(&error)),
    };
    let succeeded = result.is_ok();
    let answer = match &result {
        Ok(end) => ControlResponse::Exited(ServerExit {
            code: end.code,
            signal: end.signal,
            finished_starting: false,
            stop_requested: true,
        }),
        Err(_) => ControlResponse::Refused("the game could not be supervised".to_owned()),
    };

    // The socket and the lock go before anything is answered, so an answered
    // stop means the instance is free again.
    drop(socket);
    let _ = crate::blocking::off_worker(move || drop(lock)).await;
    drop(lease);
    for waiter in ending.waiters {
        let _ = waiter.send(answer.clone());
    }
    control.close();
    while let Ok(request) = control.try_recv() {
        request.respond(ControlResponse::Refused("the game has ended".to_owned()));
    }
    relay.send(&ClientRunnerMessage::Finished { result }).await;
    tokio::time::sleep(super::CONTROL_ANSWER_GRACE).await;
    u8::from(!succeeded)
}

/// What a hosted game is listening to while it runs.
struct Channels {
    control: mpsc::UnboundedReceiver<ControlRequest>,
    requests: mpsc::UnboundedReceiver<RunnerRequest>,
    terminations: Option<mpsc::UnboundedReceiver<()>>,
}

/// Waits for the game to end, acting on what the control socket, the starter,
/// and the runner's own termination requests ask in the meantime.
async fn supervise(
    process: GameProcess,
    observer: &LaunchObserver,
    channels: &mut Channels,
    ending: &mut Ending,
) -> Result<(ephervex_minecraft::GameExit, Vec<String>), LaunchError> {
    let pid = process.pid();
    let cancel = CancellationToken::new();
    let wait = process.wait_with_early_output(observer, &cancel);
    tokio::pin!(wait);
    let mut requests_open = true;
    let mut control_open = true;
    loop {
        tokio::select! {
            outcome = &mut wait => return outcome,
            request = channels.control.recv(), if control_open => match request {
                Some(request) => answer_control(request, pid, &cancel, ending),
                None => control_open = false,
            },
            request = channels.requests.recv(), if requests_open => match request {
                Some(RunnerRequest::Kill) => cancel.cancel(),
                None => requests_open = false,
            },
            Some(()) = async {
                match channels.terminations.as_mut() {
                    Some(terminations) => terminations.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if ending.asked_to_stop {
                    cancel.cancel();
                } else {
                    ending.asked_to_stop = true;
                    signal_group(pid, rustix::process::Signal::TERM);
                }
            }
        }
    }
}

/// Takes over the lock the starter holds, naming the socket this runner serves.
async fn take_over_lock(
    spec: &ClientRunnerSpec,
    socket: &ControlSocket,
) -> Result<LaunchLock, HostedLaunchFailure> {
    let root = spec.instance_root.clone();
    let additional = spec.additional.clone();
    let previous = spec.previous.clone();
    let runner = RunnerHost {
        run: spec.run,
        control_socket: socket.path().to_path_buf(),
    };
    crate::blocking::off_worker(move || {
        LaunchLock::take_over(&root, additional.as_deref(), &previous, runner)
    })
    .await
    .ok_or_else(|| HostedLaunchFailure::Other("the runner was stopped".to_owned()))?
    .map_err(|error| HostedLaunchFailure::from_lock(&error))
}

/// Records the game in the lock this runner owns.
async fn record_game(mut lock: LaunchLock, pid: u32) -> Result<LaunchLock, HostedLaunchFailure> {
    crate::blocking::off_worker(move || {
        let recorded = lock.record_game_pid(pid);
        (lock, recorded)
    })
    .await
    .ok_or_else(|| HostedLaunchFailure::Other("the runner was stopped".to_owned()))
    .and_then(|(lock, recorded)| {
        recorded
            .map(|()| lock)
            .map_err(|error| HostedLaunchFailure::from_lock(&error))
    })
}

fn answer_control(
    request: ControlRequest,
    pid: u32,
    cancel: &CancellationToken,
    ending: &mut Ending,
) {
    let (action, response) = request.into_parts();
    match action {
        ControlAction::Status => {
            let _ = response.send(ControlResponse::Running);
        }
        ControlAction::Stop => {
            if !ending.asked_to_stop {
                ending.asked_to_stop = true;
                signal_group(pid, rustix::process::Signal::TERM);
            }
            ending.waiters.push(response);
        }
        ControlAction::Kill => {
            cancel.cancel();
            ending.waiters.push(response);
        }
        ControlAction::Restart | ControlAction::Console(_) => {
            let _ = response.send(ControlResponse::Refused(
                "a client has no console and cannot be restarted in place".to_owned(),
            ));
        }
    }
}

/// Signals the process group the game leads.
fn signal_group(pid: u32, signal: rustix::process::Signal) {
    use rustix::process::{Pid, kill_process_group};

    if let Some(group) = i32::try_from(pid).ok().and_then(Pid::from_raw)
        && let Err(error) = kill_process_group(group, signal)
    {
        tracing::debug!(pid, %error, "could not signal the game's process group");
    }
}

/// Writes the game's output to the durable capture and reports it, until every
/// observer is gone.
async fn pump_output<W>(
    mut events: ephervex_minecraft::LaunchEvents,
    capture_directory: std::path::PathBuf,
    mut relay: Relay<W>,
) -> Relay<W>
where
    W: AsyncWrite + Unpin,
{
    let mut capture = OutputCapture::new(&capture_directory);
    while let Some(event) = events.recv().await {
        match event {
            LaunchEvent::OutputLine(line) => {
                let stream = match line.stream {
                    OutputStream::Stdout => CaptureStream::Stdout,
                    OutputStream::Stderr => CaptureStream::Stderr,
                };
                capture.push(stream, line.display().as_bytes());
                let _ = capture.drain_events();
                relay
                    .send(&ClientRunnerMessage::Output {
                        stream: line.stream.into(),
                        record: WireRecord::from(&line.record),
                    })
                    .await;
            }
            LaunchEvent::OutputDropped { lines } => {
                relay
                    .send(&ClientRunnerMessage::OutputDropped { lines })
                    .await;
            }
            _ => {}
        }
    }
    relay
}

async fn finish_failed<W>(mut relay: Relay<W>, failure: HostedLaunchFailure) -> u8
where
    W: AsyncWrite + Unpin,
{
    relay
        .send(&ClientRunnerMessage::Finished {
            result: Err(failure),
        })
        .await;
    1
}

impl HostedLaunchFailure {
    fn from_lock(error: &LockError) -> Self {
        match error {
            LockError::Lost { path } => Self::LockLost { path: path.clone() },
            other => Self::Other(crate::ErrorChain(other).to_string()),
        }
    }

    fn from_launch(error: &LaunchError) -> Self {
        use std::error::Error as _;
        let cause = || error.source().map(ToString::to_string).unwrap_or_default();
        match error {
            LaunchError::ProcessStart { executable, .. } => Self::ProcessStart {
                executable: executable.clone(),
                cause: cause(),
            },
            LaunchError::GameDirectory { path, .. } => Self::GameDirectory {
                path: path.clone(),
                cause: cause(),
            },
            other => Self::Other(crate::ErrorChain(other).to_string()),
        }
    }

    fn from_control(error: &ephervex_server::ServerError) -> Self {
        Self::Other(crate::ErrorChain(error).to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::PathBuf;

    use ephervex_server::send_control_to;
    use ephervex_storage::{LaunchState, running_state};
    use tokio::io::BufReader;

    use super::*;
    use crate::runner::{next_message, shell_game};

    struct Hosted {
        root: tempfile::TempDir,
        capture: PathBuf,
        requests: mpsc::UnboundedSender<RunnerRequest>,
        terminations: mpsc::UnboundedSender<()>,
        reports: BufReader<tokio::io::DuplexStream>,
        host: tokio::task::JoinHandle<u8>,
        /// The starter's lock, which the runner takes over.
        starter: Option<LaunchLock>,
        line: Vec<u8>,
    }

    impl Hosted {
        fn start(script: &str) -> Self {
            let root = tempfile::tempdir().expect("instance root");
            std::fs::create_dir_all(root.path().join(".launcher")).expect("metadata directory");
            let starter = LaunchLock::acquire(root.path()).expect("the starter's lock");
            let previous = starter.state().clone();
            Self::start_with(root, starter, previous, script)
        }

        fn start_with(
            root: tempfile::TempDir,
            starter: LaunchLock,
            previous: LaunchState,
            script: &str,
        ) -> Self {
            let game_directory = root.path().join("minecraft");
            std::fs::create_dir_all(&game_directory).expect("game directory");
            let capture = root.path().join("capture");
            let spec = ClientRunnerSpec {
                command: shell_game(script, &game_directory),
                instance_root: root.path().to_path_buf(),
                additional: None,
                previous,
                run: uuid::Uuid::new_v4(),
                capture_directory: capture.clone(),
                cache_directory: root.path().join("cache"),
            };
            let (reader, writer) = tokio::io::duplex(64 * 1024);
            let (requests, request_receiver) = mpsc::unbounded_channel();
            let (terminations, termination_receiver) = mpsc::unbounded_channel();
            let host = tokio::spawn(host_client(
                spec,
                writer,
                request_receiver,
                Some(termination_receiver),
            ));
            Self {
                root,
                capture,
                requests,
                terminations,
                reports: BufReader::new(reader),
                host,
                starter: Some(starter),
                line: Vec::new(),
            }
        }

        async fn next(&mut self) -> Option<ClientRunnerMessage> {
            if !next_message(&mut self.reports, &mut self.line)
                .await
                .expect("read")
            {
                return None;
            }
            Some(serde_json::from_slice(&self.line).expect("a message"))
        }

        /// Reads until the game is running and returns its process id.
        async fn running(&mut self) -> u32 {
            loop {
                match self.next().await.expect("the runner reports") {
                    ClientRunnerMessage::Running { pid } => return pid,
                    ClientRunnerMessage::Finished { result } => {
                        panic!("the game never ran: {result:?}")
                    }
                    _ => {}
                }
            }
        }

        /// Reads to the end and returns every message.
        async fn rest(&mut self) -> Vec<ClientRunnerMessage> {
            let mut messages = Vec::new();
            while let Some(message) = self.next().await {
                messages.push(message);
            }
            messages
        }

        fn lock_state(&self) -> Option<LaunchState> {
            running_state(self.root.path())
        }

        fn socket(&self) -> PathBuf {
            self.lock_state()
                .and_then(|state| state.runner)
                .expect("the runner's lock")
                .control_socket
        }
    }

    fn game_end(messages: &[ClientRunnerMessage]) -> GameEnd {
        let Some(ClientRunnerMessage::Finished { result }) = messages.last() else {
            panic!("the last message ends the run: {messages:?}");
        };
        result.clone().expect("the game ended")
    }

    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| !rest.starts_with(['Z', 'X']))
        })
    }

    #[tokio::test]
    async fn a_hosted_game_is_recorded_under_the_runner_and_its_whole_run_is_reported() {
        let mut hosted = Hosted::start("echo from-stdout; echo from-stderr >&2; sleep 0.3");

        let pid = hosted.running().await;
        let state = hosted.lock_state().expect("the launch holds the instance");
        assert_eq!(state.game_pid, Some(pid));
        assert!(state.runner.is_some(), "the lock names the runner");
        assert!(state.game_is_alive(), "the game carries its run id");
        let messages = hosted.rest().await;

        let end = game_end(&messages);
        assert_eq!(end.code, Some(0));
        let texts: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                ClientRunnerMessage::Output { record, .. } => Some(record.message.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"from-stdout") && texts.contains(&"from-stderr"));
        assert!(hosted.lock_state().is_none(), "the instance is free again");
        assert_eq!(hosted.host.await.expect("host task"), 0);
        let mut tail = crate::server_capture::ServerCaptureTail::new(hosted.capture.clone(), 50);
        let captured = tail.poll().expect("capture").lines;
        assert!(captured.iter().any(|line| line.text == "from-stdout"));
    }

    #[tokio::test]
    async fn a_kill_over_the_control_socket_ends_the_game_and_is_answered_once_the_instance_is_free()
     {
        let mut hosted = Hosted::start("sleep 30");
        let pid = hosted.running().await;
        let socket = hosted.socket();

        let response = send_control_to(&socket, &ControlAction::Kill, Some(std::process::id()))
            .await
            .expect("the runner answers");

        let ControlResponse::Exited(exit) = response else {
            panic!("expected the exit, got {response:?}");
        };
        assert_eq!(exit.signal, Some(9));
        assert!(hosted.lock_state().is_none(), "answered after the release");
        assert!(!alive(pid));
        let end = game_end(&hosted.rest().await);
        assert_eq!(end.signal, Some(9));
    }

    #[tokio::test]
    async fn a_graceful_stop_terminates_the_game_but_never_escalates() {
        let mut hosted = Hosted::start("trap '' TERM; while :; do sleep 0.1; done");
        let pid = hosted.running().await;
        let socket = hosted.socket();

        let stopping = tokio::spawn({
            let socket = socket.clone();
            async move { send_control_to(&socket, &ControlAction::Stop, Some(std::process::id())).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(alive(pid), "a game that ignores the request is left alone");
        assert!(
            !stopping.is_finished(),
            "and the request waits for its exit"
        );

        send_control_to(&socket, &ControlAction::Kill, Some(std::process::id()))
            .await
            .expect("kill");
        let answered = stopping.await.expect("stop task").expect("answered");
        assert!(
            matches!(answered, ControlResponse::Exited(_)),
            "{answered:?}"
        );
    }

    #[tokio::test]
    async fn a_graceful_stop_lets_a_game_that_honors_it_exit_by_itself() {
        let mut hosted = Hosted::start("trap 'exit 0' TERM; while :; do sleep 0.1; done");
        hosted.running().await;
        let socket = hosted.socket();

        let response = send_control_to(&socket, &ControlAction::Stop, Some(std::process::id()))
            .await
            .expect("stop");

        let ControlResponse::Exited(exit) = response else {
            panic!("expected the exit, got {response:?}");
        };
        assert_eq!(exit.code, Some(0));
        assert_eq!(game_end(&hosted.rest().await).code, Some(0));
    }

    #[tokio::test]
    async fn the_socket_refuses_a_console_line_and_answers_status() {
        let mut hosted = Hosted::start("sleep 30");
        hosted.running().await;
        let socket = hosted.socket();
        let me = Some(std::process::id());

        let status = send_control_to(&socket, &ControlAction::Status, me).await;
        let console =
            send_control_to(&socket, &ControlAction::Console("op me".to_owned()), me).await;

        assert_eq!(status.expect("status"), ControlResponse::Running);
        assert!(
            matches!(console, Ok(ControlResponse::Refused(_))),
            "{console:?}"
        );
        send_control_to(&socket, &ControlAction::Kill, me)
            .await
            .expect("kill");
    }

    #[tokio::test]
    async fn the_socket_is_refused_for_a_peer_that_is_not_the_recorded_process() {
        let mut hosted = Hosted::start("sleep 30");
        hosted.running().await;
        let socket = hosted.socket();

        let refused = send_control_to(&socket, &ControlAction::Status, Some(1)).await;

        assert!(refused.is_err(), "{refused:?}");
        send_control_to(&socket, &ControlAction::Kill, Some(std::process::id()))
            .await
            .expect("kill");
    }

    #[tokio::test]
    async fn a_starter_that_goes_away_leaves_the_game_running_and_controllable() {
        let mut hosted = Hosted::start("sleep 30");
        let pid = hosted.running().await;
        let socket = hosted.socket();
        // The starter is gone: nothing reads the reports and nothing asks.
        let Hosted {
            requests,
            reports,
            host,
            root,
            starter,
            ..
        } = hosted;
        drop((requests, reports));
        // In a real launch the starter is another process and its handle names
        // a lock the runner has replaced, so releasing it removes nothing. Here
        // both are this process, so the handle is left alone instead.
        std::mem::forget(starter);
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        assert!(alive(pid), "the game outlives its starter");
        assert!(running_state(root.path()).is_some_and(|state| state.runner_is_proven()));
        let response = send_control_to(&socket, &ControlAction::Kill, Some(std::process::id()))
            .await
            .expect("a later process controls it");
        assert!(matches!(response, ControlResponse::Exited(_)));
        assert_eq!(host.await.expect("host task"), 0);
    }

    #[tokio::test]
    async fn the_starter_can_ask_the_runner_to_kill_the_game() {
        let mut hosted = Hosted::start("sleep 30");
        hosted.running().await;

        hosted.requests.send(RunnerRequest::Kill).expect("request");

        assert_eq!(game_end(&hosted.rest().await).signal, Some(9));
    }

    #[tokio::test]
    async fn the_first_termination_request_stops_the_game_and_the_second_kills_it() {
        let mut hosted = Hosted::start("trap '' TERM; while :; do sleep 0.1; done");
        let pid = hosted.running().await;

        hosted.terminations.send(()).expect("first request");
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(alive(pid), "the first request only asks the game to stop");
        hosted.terminations.send(()).expect("second request");

        assert_eq!(game_end(&hosted.rest().await).signal, Some(9));
    }

    #[tokio::test]
    async fn a_runner_that_cannot_take_over_the_lock_starts_no_game() {
        let root = tempfile::tempdir().expect("instance root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("metadata directory");
        let starter = LaunchLock::acquire(root.path()).expect("the starter's lock");
        // The hand-off describes a different launch than the one holding the lock.
        let mut previous = starter.state().clone();
        previous.since += chrono::Duration::seconds(5);
        let marker = root.path().join("game-ran");
        let script = format!("touch {}", marker.display());
        let mut hosted = Hosted::start_with(root, starter, previous, &script);

        let messages = hosted.rest().await;

        let Some(ClientRunnerMessage::Finished {
            result: Err(HostedLaunchFailure::LockLost { .. }),
        }) = messages.last()
        else {
            panic!("expected a lost lock, got {messages:?}");
        };
        assert!(!marker.exists(), "no game was started");
        assert_eq!(hosted.host.await.expect("host task"), 1);
    }
}
