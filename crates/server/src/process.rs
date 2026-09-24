//! Foreground server supervision and the permission-restricted control channel.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use ephervex_model::OperationId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::capture::{CaptureStream, CapturedLine, OutputCapture, ServerOutputEvent};
use crate::runtime::{
    RUNTIME_LOCK_FILE, RuntimeClassification, RuntimeEvidence, active_control_socket_path,
    capture_java_child, classify_runtime, clear_stale_runtime, prepare_control_socket_path,
    runtime_path, write_runtime_evidence,
};
use crate::{ServerError, ServerResult};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a restart may take to stop the old child and start the new one.
/// Longer than a plain stop: a large world saves before the stop completes, and
/// the answer only comes once the replacement has been spawned.
const RESTART_TIMEOUT: Duration = Duration::from_secs(300);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A redaction-safe command for a server JVM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
    pub java: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
}

/// How a server process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ServerEvent {
    /// Validating the record, the EULA, the network settings, and Java.
    ///
    /// Carries the id every launcher log line for this start is tagged with, so
    /// a frontend can show it before the process exists.
    Preparing {
        operation: OperationId,
    },
    Started {
        pid: u32,
    },
    Output(CapturedLine),
    Warning(ServerWarning),
    CapturePruned(crate::CaptureRetention),
    CaptureUnavailable {
        detail: String,
    },
    Exited(ServerExit),
    /// The child exited for a restart, and its replacement is being started.
    Restarting,
}

/// Advisory conditions that do not make a server start unsafe or impossible.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerWarning {
    OfflineModeExposed,
    PortUnavailable {
        address: String,
        port: u16,
        detail: String,
    },
}

#[derive(Debug, Clone)]
pub struct ServerObserver {
    sender: Option<mpsc::UnboundedSender<ServerEvent>>,
}

impl ServerObserver {
    #[must_use]
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<ServerEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: Some(sender),
            },
            receiver,
        )
    }

    #[must_use]
    pub const fn silent() -> Self {
        Self { sender: None }
    }

    pub fn emit(&self, event: ServerEvent) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "value")]
pub enum ControlAction {
    Status,
    Stop,
    Restart,
    Kill,
    Console(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "detail")]
pub enum ControlResponse {
    Running,
    Accepted,
    Exited(ServerExit),
    Restarted {
        pid: u32,
    },
    /// The old child stopped for a restart and the replacement could not be
    /// started. The server is left stopped.
    StartFailed(String),
    Refused(String),
}

#[derive(Debug)]
struct ControlRequest {
    action: ControlAction,
    response: oneshot::Sender<ControlResponse>,
}

/// Owns the Java child, its pipes, the runtime lock, and control socket.
#[derive(Debug)]
pub struct ServerSupervisor;

impl ServerSupervisor {
    pub async fn run(
        command: &ServerCommand,
        server_root: &Path,
        capture_directory: &Path,
        observer: &ServerObserver,
    ) -> ServerResult<ServerExit> {
        let _guard = RuntimeGuard::acquire(server_root)?;
        let mut evidence = RuntimeEvidence::for_current_supervisor()?;
        let socket_path = prepare_control_socket_path(server_root, evidence.generation)?;
        if let Err(error) = std::fs::remove_file(&socket_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(ServerError::ControlUnavailable {
                path: socket_path,
                source: error,
            });
        }
        let listener =
            UnixListener::bind(&socket_path).map_err(|source| ServerError::ControlUnavailable {
                path: socket_path.clone(),
                source,
            })?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| ServerError::ControlUnavailable {
                path: socket_path.clone(),
                source,
            },
        )?;

        let (requests, mut request_receiver) = mpsc::unbounded_channel();
        let control_task = tokio::spawn(serve_control(listener, requests));
        let capture = Arc::new(Mutex::new(OutputCapture::new(capture_directory)));
        write_runtime_evidence(server_root, &evidence)?;

        let result = supervise_commands(
            command,
            server_root,
            observer,
            &capture,
            &mut evidence,
            &mut request_receiver,
        )
        .await;

        control_task.abort();
        let _ = std::fs::remove_file(&socket_path);
        if result.is_ok() {
            let _ = std::fs::remove_file(runtime_path(server_root));
        }
        result
    }
}

async fn supervise_commands(
    command: &ServerCommand,
    server_root: &Path,
    observer: &ServerObserver,
    capture: &Arc<Mutex<OutputCapture>>,
    evidence: &mut RuntimeEvidence,
    requests: &mut mpsc::UnboundedReceiver<ControlRequest>,
) -> ServerResult<ServerExit> {
    let mut restart_waiters: Vec<oneshot::Sender<ControlResponse>> = Vec::new();
    let mut last_exit = None;
    loop {
        let (mut child, mut stdin, readers, pid) = match spawn_child(command, observer, capture) {
            Ok(spawned) => spawned,
            // The old child is gone, which is what the runtime evidence has to
            // say: the server is stopped, and whoever asked for the restart is
            // told that its second half failed rather than left waiting.
            Err(error) if !restart_waiters.is_empty() => {
                let detail = error.to_string();
                for waiter in restart_waiters.drain(..) {
                    let _ = waiter.send(ControlResponse::StartFailed(detail.clone()));
                }
                return Ok(last_exit.unwrap_or(ServerExit {
                    code: None,
                    signal: None,
                }));
            }
            Err(error) => return Err(error),
        };
        evidence.child = Some(capture_java_child(pid)?);
        write_runtime_evidence(server_root, evidence)?;
        observer.emit(ServerEvent::Started { pid });
        for waiter in restart_waiters.drain(..) {
            let _ = waiter.send(ControlResponse::Restarted { pid });
        }

        let (exit, restart, waiters) = supervise_child(&mut child, &mut stdin, requests).await?;
        for reader in readers {
            let _ = reader.await;
        }
        observer.emit(ServerEvent::Exited(exit));
        last_exit = Some(exit);
        if restart {
            observer.emit(ServerEvent::Restarting);
            restart_waiters = waiters;
            continue;
        }
        for waiter in waiters {
            let _ = waiter.send(ControlResponse::Exited(exit));
        }
        return Ok(exit);
    }
}

type ReaderTask = tokio::task::JoinHandle<()>;

fn spawn_child(
    command: &ServerCommand,
    observer: &ServerObserver,
    capture: &Arc<Mutex<OutputCapture>>,
) -> ServerResult<(Child, ChildStdin, Vec<ReaderTask>, u32)> {
    std::fs::create_dir_all(&command.working_directory).map_err(|source| ServerError::Io {
        path: command.working_directory.clone(),
        source,
    })?;
    let mut child = Command::new(&command.java)
        .args(&command.arguments)
        .current_dir(&command.working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ServerError::ProcessStart {
            executable: command.java.clone(),
            source,
        })?;
    let pid = child.id().ok_or_else(|| ServerError::ProcessStart {
        executable: command.java.clone(),
        source: std::io::Error::other("spawned child has no process id"),
    })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        ServerError::ProcessIo(std::io::Error::other("server stdin was not captured"))
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let readers = vec![
        tokio::spawn(read_pipe(
            stdout,
            CaptureStream::Stdout,
            Arc::clone(capture),
            observer.clone(),
        )),
        tokio::spawn(read_pipe(
            stderr,
            CaptureStream::Stderr,
            Arc::clone(capture),
            observer.clone(),
        )),
    ];
    Ok((child, stdin, readers, pid))
}

async fn supervise_child(
    child: &mut Child,
    stdin: &mut ChildStdin,
    requests: &mut mpsc::UnboundedReceiver<ControlRequest>,
) -> ServerResult<(ServerExit, bool, Vec<oneshot::Sender<ControlResponse>>)> {
    let mut interval = tokio::time::interval(EXIT_POLL_INTERVAL);
    let mut restart = false;
    // A stop already asked for. A restart cannot turn it back into a start.
    let mut stopping = false;
    let mut completion_waiters = Vec::new();
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Some(status) = child.try_wait().map_err(ServerError::ProcessIo)? {
                    return Ok((exit_from_status(status), restart, completion_waiters));
                }
            }
            request = requests.recv() => {
                let Some(request) = request else {
                    continue;
                };
                match request.action {
                    ControlAction::Status => {
                        let _ = request.response.send(ControlResponse::Running);
                    }
                    ControlAction::Console(command) => {
                        if command.contains(['\0', '\n', '\r']) {
                            let _ = request.response.send(ControlResponse::Refused(
                                "console command must be one line".to_owned(),
                            ));
                            continue;
                        }
                        stdin.write_all(command.as_bytes()).await.map_err(ServerError::ProcessIo)?;
                        stdin.write_all(b"\n").await.map_err(ServerError::ProcessIo)?;
                        stdin.flush().await.map_err(ServerError::ProcessIo)?;
                        let _ = request.response.send(ControlResponse::Accepted);
                    }
                    // An explicit stop or kill is the last word: a restart
                    // still waiting for the old child ends with it stopped.
                    ControlAction::Stop => {
                        restart = false;
                        if !stopping {
                            stopping = true;
                            stdin.write_all(b"stop\n").await.map_err(ServerError::ProcessIo)?;
                            stdin.flush().await.map_err(ServerError::ProcessIo)?;
                        }
                        completion_waiters.push(request.response);
                    }
                    ControlAction::Restart if stopping && !restart => {
                        let _ = request.response.send(ControlResponse::Refused(
                            "the server is already stopping".to_owned(),
                        ));
                    }
                    // A second restart joins the first, so one stop is followed
                    // by exactly one start.
                    ControlAction::Restart => {
                        if !stopping {
                            stopping = true;
                            restart = true;
                            stdin.write_all(b"stop\n").await.map_err(ServerError::ProcessIo)?;
                            stdin.flush().await.map_err(ServerError::ProcessIo)?;
                        }
                        completion_waiters.push(request.response);
                    }
                    ControlAction::Kill => {
                        restart = false;
                        stopping = true;
                        child.start_kill().map_err(ServerError::ProcessIo)?;
                        completion_waiters.push(request.response);
                    }
                }
            }
        }
    }
}

async fn read_pipe<R>(
    pipe: Option<R>,
    stream: CaptureStream,
    capture: Arc<Mutex<OutputCapture>>,
    observer: ServerObserver,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(pipe) = pipe else {
        return;
    };
    let mut reader = BufReader::new(pipe);
    let mut bytes = Vec::new();
    loop {
        match read_limited_line(&mut reader, &mut bytes).await {
            Ok(None) => break,
            Ok(Some(omitted_bytes)) => {
                let mut capture = capture.lock().await;
                let line = capture.push_limited(stream, &bytes, omitted_bytes);
                observer.emit(ServerEvent::Output(line));
                for event in capture.drain_events() {
                    match event {
                        ServerOutputEvent::CapturePruned(retention) => {
                            observer.emit(ServerEvent::CapturePruned(retention));
                        }
                        ServerOutputEvent::CaptureUnavailable { detail } => {
                            observer.emit(ServerEvent::CaptureUnavailable { detail });
                        }
                        ServerOutputEvent::Line(_) => {}
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "server output pipe failed");
                break;
            }
        }
    }
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
        let room = crate::MAX_CAPTURE_LINE_BYTES.saturating_sub(output.len());
        let retain = take.min(room);
        output.extend_from_slice(&available[..retain]);
        omitted = omitted.saturating_add(take - retain);
        let consumed = take + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            if omitted == 0 && output.ends_with(b"\r") {
                output.pop();
            }
            return Ok(Some(omitted));
        }
    }
}

async fn serve_control(listener: UnixListener, requests: mpsc::UnboundedSender<ControlRequest>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let requests = requests.clone();
        tokio::spawn(async move {
            let _ = handle_connection(stream, requests).await;
        });
    }
}

async fn handle_connection(
    stream: UnixStream,
    requests: mpsc::UnboundedSender<ControlRequest>,
) -> ServerResult<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut body = Vec::new();
    reader
        .read_until(b'\n', &mut body)
        .await
        .map_err(ServerError::ProcessIo)?;
    let action: ControlAction =
        serde_json::from_slice(&body).map_err(|_| ServerError::InvalidControlResponse)?;
    let (sender, receiver) = oneshot::channel();
    requests
        .send(ControlRequest {
            action,
            response: sender,
        })
        .map_err(|_| ServerError::InvalidControlResponse)?;
    let response = receiver
        .await
        .map_err(|_| ServerError::InvalidControlResponse)?;
    let mut response =
        serde_json::to_vec(&response).map_err(|_| ServerError::InvalidControlResponse)?;
    response.push(b'\n');
    writer
        .write_all(&response)
        .await
        .map_err(ServerError::ProcessIo)
}

/// Sends a lifecycle or console request through the live owner channel.
pub async fn send_control(
    server_root: &Path,
    action: &ControlAction,
) -> ServerResult<ControlResponse> {
    if let ControlAction::Console(command) = action
        && command.contains(['\0', '\n', '\r'])
    {
        return Err(ServerError::InvalidConsoleCommand);
    }
    let path = active_control_socket_path(server_root)?.ok_or_else(|| {
        ServerError::ControlUnavailable {
            path: runtime_path(server_root),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "server has no active control socket generation",
            ),
        }
    })?;
    let mut stream =
        UnixStream::connect(&path)
            .await
            .map_err(|source| ServerError::ControlUnavailable {
                path: path.clone(),
                source,
            })?;
    let mut request =
        serde_json::to_vec(action).map_err(|_| ServerError::InvalidControlResponse)?;
    request.push(b'\n');
    stream
        .write_all(&request)
        .await
        .map_err(ServerError::ProcessIo)?;
    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    let read = reader.read_until(b'\n', &mut response);
    let count = match action {
        ControlAction::Restart => {
            tokio::time::timeout(RESTART_TIMEOUT, read)
                .await
                .map_err(|_| ServerError::ControlUnavailable {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "server did not restart within 5 minutes",
                    ),
                })?
        }
        ControlAction::Stop | ControlAction::Kill => tokio::time::timeout(CONTROL_TIMEOUT, read)
            .await
            .map_err(|_| ServerError::ControlUnavailable {
                path,
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "server did not exit within 30 seconds",
                ),
            })?,
        ControlAction::Status | ControlAction::Console(_) => read.await,
    }
    .map_err(ServerError::ProcessIo)?;
    if count == 0 {
        return Err(ServerError::InvalidControlResponse);
    }
    serde_json::from_slice(&response).map_err(|_| ServerError::InvalidControlResponse)
}

fn exit_from_status(status: std::process::ExitStatus) -> ServerExit {
    use std::os::unix::process::ExitStatusExt as _;
    ServerExit {
        code: status.code(),
        signal: status.signal(),
    }
}

#[derive(Debug)]
struct RuntimeGuard {
    path: PathBuf,
    _file: std::fs::File,
}

impl RuntimeGuard {
    fn acquire(server_root: &Path) -> ServerResult<Self> {
        match classify_runtime(server_root)? {
            RuntimeClassification::Stopped => {}
            RuntimeClassification::ProvenStale { .. } => clear_stale_runtime(server_root)?,
            _ => return Err(ServerError::AlreadyRunning),
        }
        let path = server_root.join(".launcher").join(RUNTIME_LOCK_FILE);
        let parent = path.parent().unwrap_or(server_root);
        std::fs::create_dir_all(parent).map_err(|source| ServerError::RuntimeState {
            path: parent.to_path_buf(),
            source,
        })?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => Ok(Self { path, _file: file }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // A stale lock is removable only when runtime evidence proves
                // the former owner and child are gone.
                if matches!(
                    classify_runtime(server_root)?,
                    RuntimeClassification::ProvenStale { .. }
                ) {
                    clear_stale_runtime(server_root)?;
                    std::fs::remove_file(&path).map_err(|source| ServerError::RuntimeState {
                        path: path.clone(),
                        source,
                    })?;
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(|source| ServerError::RuntimeState {
                            path: path.clone(),
                            source,
                        })?;
                    Ok(Self { path, _file: file })
                } else {
                    Err(ServerError::AlreadyRunning)
                }
            }
            Err(source) => Err(ServerError::RuntimeState {
                path: path.clone(),
                source,
            }),
        }
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[tokio::test]
    async fn synthetic_child_receives_console_and_graceful_stop() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let capture = tempfile::tempdir().expect("capture");
        let command = ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".to_owned(),
                "while IFS= read -r line; do echo \"got:$line\"; [ \"$line\" = stop ] && exit 0; done"
                    .to_owned(),
            ],
            working_directory: root.path().join("server"),
        };
        let server_root = root.path().to_path_buf();
        let capture_path = capture.path().to_path_buf();
        let task = tokio::spawn(async move {
            ServerSupervisor::run(
                &command,
                &server_root,
                &capture_path,
                &ServerObserver::silent(),
            )
            .await
        });
        for _ in 0..50 {
            if active_control_socket_path(root.path())
                .is_ok_and(|path| path.is_some_and(|path| path.exists()))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            send_control(root.path(), &ControlAction::Console("hello".to_owned()))
                .await
                .expect("console"),
            ControlResponse::Accepted
        );
        assert!(matches!(
            send_control(root.path(), &ControlAction::Stop)
                .await
                .expect("stop"),
            ControlResponse::Exited(ServerExit { code: Some(0), .. })
        ));
        assert_eq!(task.await.expect("join").expect("supervise").code, Some(0));
    }

    #[tokio::test]
    async fn started_event_is_emitted_after_child_runtime_evidence_is_published() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let capture = tempfile::tempdir().expect("capture");
        let command = ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".to_owned(),
                "while read -r line; do [ \"$line\" = stop ] && exit 0; done".to_owned(),
            ],
            working_directory: root.path().join("server"),
        };
        let (observer, mut events) = ServerObserver::channel();
        let server_root = root.path().to_path_buf();
        let capture_path = capture.path().to_path_buf();
        let task = tokio::spawn(async move {
            ServerSupervisor::run(&command, &server_root, &capture_path, &observer).await
        });

        while let Some(event) = events.recv().await {
            if matches!(event, ServerEvent::Started { .. }) {
                break;
            }
        }
        assert!(matches!(
            classify_runtime(root.path()).expect("runtime classification"),
            RuntimeClassification::Controlled { child: Some(_), .. }
        ));

        send_control(root.path(), &ControlAction::Stop)
            .await
            .expect("stop");
        assert_eq!(task.await.expect("join").expect("supervise").code, Some(0));
    }

    /// A synthetic server that records each start and each stop in a journal
    /// and exits a moment after it is asked to stop, the way a real one saves
    /// its world first.
    fn journaled_server(root: &Path, java: PathBuf) -> (ServerCommand, PathBuf) {
        let journal = root.join("journal");
        let script = format!(
            "echo start >> '{journal}'; while read -r line; do [ \"$line\" = stop ] && {{ sleep 0.3; echo stop >> '{journal}'; exit 0; }}; done",
            journal = journal.display()
        );
        (
            ServerCommand {
                java,
                arguments: vec!["-c".to_owned(), script],
                working_directory: root.join("server"),
            },
            journal,
        )
    }

    fn journal(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    }

    struct Supervised {
        root: tempfile::TempDir,
        _capture: tempfile::TempDir,
        journal: PathBuf,
        events: mpsc::UnboundedReceiver<ServerEvent>,
        task: tokio::task::JoinHandle<ServerResult<ServerExit>>,
    }

    /// Starts a journaled server and waits until its first child is running.
    async fn supervise_journaled(java: Option<PathBuf>) -> Supervised {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let capture = tempfile::tempdir().expect("capture");
        let (command, journal) = journaled_server(
            root.path(),
            java.unwrap_or_else(|| PathBuf::from("/bin/sh")),
        );
        let (observer, mut events) = ServerObserver::channel();
        let server_root = root.path().to_path_buf();
        let capture_path = capture.path().to_path_buf();
        let task = tokio::spawn(async move {
            ServerSupervisor::run(&command, &server_root, &capture_path, &observer).await
        });
        while let Some(event) = events.recv().await {
            if matches!(event, ServerEvent::Started { .. }) {
                break;
            }
        }
        Supervised {
            root,
            _capture: capture,
            journal,
            events,
            task,
        }
    }

    fn lifecycle(events: &mut mpsc::UnboundedReceiver<ServerEvent>) -> Vec<&'static str> {
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(match event {
                ServerEvent::Started { .. } => "started",
                ServerEvent::Exited(_) => "exited",
                ServerEvent::Restarting => "restarting",
                _ => continue,
            });
        }
        seen
    }

    #[tokio::test]
    async fn a_restart_stops_the_child_first_and_starts_exactly_one_replacement() {
        let mut server = supervise_journaled(None).await;
        let first = classify_runtime(server.root.path()).expect("runtime");
        let RuntimeClassification::Controlled {
            child: Some(old), ..
        } = first
        else {
            panic!("a running child: {first:?}");
        };

        let (a, b) = tokio::join!(
            send_control(server.root.path(), &ControlAction::Restart),
            send_control(server.root.path(), &ControlAction::Restart),
        );
        let (a, b) = (a.expect("restart"), b.expect("restart"));
        assert!(matches!(a, ControlResponse::Restarted { .. }), "{a:?}");
        assert_eq!(a, b, "a second restart joins the first");
        let ControlResponse::Restarted { pid } = a else {
            unreachable!()
        };
        assert_ne!(pid, old.pid, "a new process");
        assert_eq!(
            journal(&server.journal),
            ["start", "stop", "start"],
            "the old child had stopped before the replacement started, once"
        );
        assert_eq!(
            lifecycle(&mut server.events),
            ["exited", "restarting", "started"]
        );

        send_control(server.root.path(), &ControlAction::Stop)
            .await
            .expect("stop");
        server.task.await.expect("join").expect("supervise");
    }

    #[tokio::test]
    async fn a_stop_while_a_restart_waits_leaves_the_server_stopped() {
        let server = supervise_journaled(None).await;
        let restart = tokio::spawn({
            let root = server.root.path().to_path_buf();
            async move { send_control(&root, &ControlAction::Restart).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let stopped = send_control(server.root.path(), &ControlAction::Stop)
            .await
            .expect("stop");

        assert!(matches!(stopped, ControlResponse::Exited(_)));
        assert!(matches!(
            restart.await.expect("join").expect("restart"),
            ControlResponse::Exited(_)
        ));
        server.task.await.expect("join").expect("supervise");
        assert_eq!(
            journal(&server.journal),
            ["start", "stop"],
            "nothing started again"
        );
    }

    #[tokio::test]
    async fn a_restart_cannot_turn_a_requested_stop_into_a_start() {
        let server = supervise_journaled(None).await;
        let stop = tokio::spawn({
            let root = server.root.path().to_path_buf();
            async move { send_control(&root, &ControlAction::Stop).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let restart = send_control(server.root.path(), &ControlAction::Restart)
            .await
            .expect("answered");

        assert!(
            matches!(restart, ControlResponse::Refused(_)),
            "{restart:?}"
        );
        assert!(matches!(
            stop.await.expect("join").expect("stop"),
            ControlResponse::Exited(_)
        ));
        server.task.await.expect("join").expect("supervise");
        assert_eq!(journal(&server.journal), ["start", "stop"]);
    }

    #[tokio::test]
    async fn a_replacement_that_cannot_start_leaves_the_server_stopped_and_says_so() {
        let bin = tempfile::tempdir().expect("bin");
        let java = bin.path().join("java");
        std::fs::copy("/bin/sh", &java).expect("a java stand-in");
        let server = supervise_journaled(Some(java.clone())).await;
        // The executable goes away while the first child runs, so only the
        // replacement's start can fail.
        std::fs::remove_file(&java).expect("remove");

        let restart = send_control(server.root.path(), &ControlAction::Restart)
            .await
            .expect("answered");

        assert!(
            matches!(restart, ControlResponse::StartFailed(_)),
            "{restart:?}"
        );
        server
            .task
            .await
            .expect("join")
            .expect("the old child stopped cleanly");
        assert_eq!(journal(&server.journal), ["start", "stop"]);
        assert!(
            !runtime_path(server.root.path()).exists(),
            "stopped, not left looking like a server that needs recovery"
        );
    }

    #[tokio::test]
    async fn console_rejects_embedded_line_breaks() {
        let root = tempfile::tempdir().expect("root");
        assert!(matches!(
            send_control(
                root.path(),
                &ControlAction::Console("first\nstop".to_owned())
            )
            .await,
            Err(ServerError::InvalidConsoleCommand)
        ));
    }
}
