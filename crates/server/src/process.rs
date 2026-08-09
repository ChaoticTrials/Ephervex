//! Foreground server supervision and the permission-restricted control channel.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

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
    Started { pid: u32 },
    Output(CapturedLine),
    Warning(ServerWarning),
    CapturePruned(crate::CaptureRetention),
    CaptureUnavailable { detail: String },
    Exited(ServerExit),
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
    Restarted { pid: u32 },
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
    loop {
        let (mut child, mut stdin, readers, pid) = spawn_child(command, observer, capture)?;
        evidence.child = Some(capture_java_child(pid)?);
        write_runtime_evidence(server_root, evidence)?;
        for waiter in restart_waiters.drain(..) {
            let _ = waiter.send(ControlResponse::Restarted { pid });
        }

        let (exit, restart, waiters) = supervise_child(&mut child, &mut stdin, requests).await?;
        for reader in readers {
            let _ = reader.await;
        }
        observer.emit(ServerEvent::Exited(exit));
        if restart {
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
    observer.emit(ServerEvent::Started { pid });
    Ok((child, stdin, readers, pid))
}

async fn supervise_child(
    child: &mut Child,
    stdin: &mut ChildStdin,
    requests: &mut mpsc::UnboundedReceiver<ControlRequest>,
) -> ServerResult<(ServerExit, bool, Vec<oneshot::Sender<ControlResponse>>)> {
    let mut interval = tokio::time::interval(EXIT_POLL_INTERVAL);
    let mut restart = false;
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
                    ControlAction::Stop => {
                        stdin.write_all(b"stop\n").await.map_err(ServerError::ProcessIo)?;
                        stdin.flush().await.map_err(ServerError::ProcessIo)?;
                        completion_waiters.push(request.response);
                    }
                    ControlAction::Restart => {
                        restart = true;
                        stdin.write_all(b"stop\n").await.map_err(ServerError::ProcessIo)?;
                        stdin.flush().await.map_err(ServerError::ProcessIo)?;
                        completion_waiters.push(request.response);
                    }
                    ControlAction::Kill => {
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
        ControlAction::Stop | ControlAction::Restart | ControlAction::Kill => {
            tokio::time::timeout(CONTROL_TIMEOUT, read)
                .await
                .map_err(|_| ServerError::ControlUnavailable {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "server did not exit within 30 seconds",
                    ),
                })?
        }
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
