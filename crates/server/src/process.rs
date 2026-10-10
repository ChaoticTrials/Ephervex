//! Foreground server supervision and the permission-restricted control channel.

use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use ephervex_model::{Operation, OperationId};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::capture::{CaptureStream, CapturedLine, OutputCapture, ServerOutputEvent};
use crate::runtime::{
    ProcessEvidence, RuntimeClassification, RuntimeEvidence, active_control_socket_path,
    capture_java_child, classify_runtime, clear_stale_runtime, create_runtime_lock,
    prepare_control_socket_path, read_runtime_evidence, remove_per_run_control_directory,
    runtime_lock_path, runtime_path, write_runtime_evidence,
};
use crate::{ServerError, ServerResult};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a restart may take to stop the old child and start the new one.
/// Longer than a plain stop: a large world saves before the stop completes, and
/// the answer only comes once the replacement has been spawned.
const RESTART_TIMEOUT: Duration = Duration::from_secs(300);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Pause after an accept failure that can persist, such as descriptor exhaustion.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
/// How long a child that has lost its executable link may take to finish exiting.
const EXITING_CHILD_GRACE: Duration = Duration::from_secs(5);
/// How long a write to the server's input may take before the input is treated as unavailable.
const STDIN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long output a descendant still holds open is read after the server exits.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// How long a failed stop write waits for the child to report that it was already exiting.
const STOP_WRITE_EXIT_GRACE: Duration = Duration::from_millis(500);
/// How long a child whose supervision failed may take to be killed and reaped.
const ABANDONED_CHILD_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a canceled run waits for its killed child to leave the process table.
const CANCELED_CHILD_EXIT_WAIT: Duration = Duration::from_secs(2);

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
    /// Whether the process announced that it finished starting before it
    /// ended. A server that fails during startup can still exit with code 0.
    #[serde(default)]
    pub finished_starting: bool,
    /// Whether a Stop or Kill through the control channel ended the process.
    /// A restart's stop is not one: the run goes on with a new process.
    #[serde(default)]
    pub stop_requested: bool,
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
    /// Progress of a download the preparation makes, such as a managed Java
    /// runtime. A snapshot of the operation named by `Preparing`.
    Downloading(Box<Operation>),
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
    /// The complete supervised run ended, after any restart. None means the
    /// run could not be prepared or supervised; Exited describes each child.
    Finished {
        exit: Option<ServerExit>,
    },
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
        let guard = RuntimeGuard::acquire(server_root)?;
        let mut evidence = RuntimeEvidence::for_current_supervisor()?;
        let (requests, mut request_receiver) = mpsc::unbounded_channel();
        let socket = ControlSocket::open(server_root, evidence.generation, requests)?;
        evidence.control_directory = socket.path.parent().map(Path::to_path_buf);
        let capture = Arc::new(Mutex::new(OutputCapture::new(capture_directory)));
        let mut outstanding = Outstanding::default();
        // Declared last so a canceled run withdraws its evidence before the lock is released.
        let mut cancellation = CanceledRunEvidence {
            server_root: server_root.to_path_buf(),
            generation: evidence.generation,
            live_child: Arc::clone(&outstanding.live_child),
            exit_wait: CANCELED_CHILD_EXIT_WAIT,
            armed: true,
        };

        let result = match write_runtime_evidence(server_root, &evidence) {
            Ok(()) => {
                supervise_commands(
                    command,
                    server_root,
                    observer,
                    &capture,
                    &mut evidence,
                    &mut request_receiver,
                    &mut outstanding,
                )
                .await
            }
            Err(error) => Err(error),
        };
        cancellation.armed = false;

        // Every path out of supervision releases what the run owns. The
        // evidence stays only when a child the run started may still be alive.
        drop(socket);
        request_receiver.close();
        if !outstanding.child_unconfirmed {
            let _ = std::fs::remove_file(runtime_path(server_root));
        }
        drop(guard);
        // Answered once the server is released, so an answered stop means nothing writes into it.
        let final_response = |request: Option<&ControlAction>| match (&result, request) {
            (
                Ok(exit),
                None | Some(ControlAction::Status | ControlAction::Stop | ControlAction::Kill),
            ) => ControlResponse::Exited(*exit),
            (Ok(_), Some(_)) => server_has_stopped(),
            (Err(error), _) => {
                ControlResponse::Refused(format!("the server could not be supervised: {error}"))
            }
        };
        for waiter in outstanding.waiters {
            let _ = waiter.send(final_response(None));
        }
        while let Ok(request) = request_receiver.try_recv() {
            let _ = request.response.send(final_response(Some(&request.action)));
        }
        result
    }
}

/// Marks that no child of the run is alive.
const NO_CHILD: u32 = 0;

/// Withdraws a run's runtime evidence when the run is dropped before it finishes.
///
/// The child is killed when the supervision future drops. The evidence stays
/// when that child is not gone after a short wait, because it may still be alive.
struct CanceledRunEvidence {
    server_root: PathBuf,
    generation: uuid::Uuid,
    live_child: Arc<AtomicU32>,
    exit_wait: Duration,
    armed: bool,
}

impl Drop for CanceledRunEvidence {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let pid = self.live_child.load(Ordering::Acquire);
        if pid != NO_CHILD && !wait_until_gone(pid, self.exit_wait) {
            return;
        }
        // Only this run's own evidence is withdrawn.
        if matches!(
            read_runtime_evidence(&self.server_root),
            Ok(Some(evidence)) if evidence.generation == self.generation
        ) {
            let _ = std::fs::remove_file(runtime_path(&self.server_root));
        }
    }
}

/// Polls for `pid` to leave the process table. Bounded, for use where awaiting is impossible.
fn wait_until_gone(pid: u32, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while process_is_running(pid) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// What a run still owes answers for or must report once supervision ends.
#[derive(Debug, Default)]
struct Outstanding {
    /// Requests waiting for a stop, a kill, or a restart to settle.
    waiters: Vec<oneshot::Sender<ControlResponse>>,
    /// A child the run started could not be confirmed gone after supervision failed.
    child_unconfirmed: bool,
    /// The pid of the child the run has started and not yet reaped, or `NO_CHILD`.
    live_child: Arc<AtomicU32>,
}

/// The control socket and the task accepting on it, removed together on every path.
#[derive(Debug)]
struct ControlSocket {
    path: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ControlSocket {
    fn open(
        server_root: &Path,
        generation: uuid::Uuid,
        requests: mpsc::UnboundedSender<ControlRequest>,
    ) -> ServerResult<Self> {
        // Owns the path from here so a failure below also releases its directory.
        let mut socket = Self {
            path: prepare_control_socket_path(server_root, generation)?,
            task: None,
        };
        if let Err(error) = std::fs::remove_file(&socket.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(ServerError::ControlUnavailable {
                path: socket.path.clone(),
                source: error,
            });
        }
        let listener =
            UnixListener::bind(&socket.path).map_err(|source| ServerError::ControlUnavailable {
                path: socket.path.clone(),
                source,
            })?;
        std::fs::set_permissions(&socket.path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| ServerError::ControlUnavailable {
                path: socket.path.clone(),
                source,
            },
        )?;
        socket.task = Some(tokio::spawn(serve_control(listener, requests)));
        Ok(socket)
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.path);
        if let Some(directory) = self.path.parent() {
            remove_per_run_control_directory(directory);
        }
    }
}

async fn supervise_commands(
    command: &ServerCommand,
    server_root: &Path,
    observer: &ServerObserver,
    capture: &Arc<Mutex<OutputCapture>>,
    evidence: &mut RuntimeEvidence,
    requests: &mut mpsc::UnboundedReceiver<ControlRequest>,
    outstanding: &mut Outstanding,
) -> ServerResult<ServerExit> {
    let mut last_exit = None;
    loop {
        let finished_starting = Arc::new(AtomicBool::new(false));
        let spawned = spawn_child(command, observer, capture, &finished_starting);
        let (mut child, stdin, readers, pid) = match spawned {
            Ok(spawned) => spawned,
            // The old child is gone, which is what the runtime evidence has to
            // say: the server is stopped, and whoever asked for the restart is
            // told that its second half failed rather than left waiting.
            Err(error) if !outstanding.waiters.is_empty() => {
                let detail = error.to_string();
                for waiter in outstanding.waiters.drain(..) {
                    let _ = waiter.send(ControlResponse::StartFailed(detail.clone()));
                }
                return Ok(last_exit.unwrap_or(ServerExit {
                    code: None,
                    signal: None,
                    finished_starting: false,
                    stop_requested: false,
                }));
            }
            Err(error) => return Err(error),
        };
        // Declared after the child, so a dropped run kills the group before the child is reaped.
        let mut group = ServerGroup::new(pid);
        outstanding.live_child.store(pid, Ordering::Release);
        if let Err(error) = record_spawned_child(&mut child, pid, server_root, evidence).await {
            abandon_child(&mut child, &mut group, readers, outstanding).await;
            return Err(error);
        }
        observer.emit(ServerEvent::Started { pid });
        for waiter in outstanding.waiters.drain(..) {
            let _ = waiter.send(ControlResponse::Restarted { pid });
        }

        let (mut exit, restart) =
            match supervise_child(&mut child, stdin, requests, &mut outstanding.waiters).await {
                Ok(supervised) => supervised,
                Err(error) => {
                    abandon_child(&mut child, &mut group, readers, outstanding).await;
                    return Err(error);
                }
            };
        group.disarm();
        outstanding.live_child.store(NO_CHILD, Ordering::Release);
        drain_output(readers).await;
        exit.finished_starting = finished_starting.load(Ordering::Relaxed);
        observer.emit(ServerEvent::Exited(exit));
        last_exit = Some(exit);
        if restart {
            observer.emit(ServerEvent::Restarting);
            continue;
        }
        return Ok(exit);
    }
}

async fn record_spawned_child(
    child: &mut Child,
    pid: u32,
    server_root: &Path,
    evidence: &mut RuntimeEvidence,
) -> ServerResult<()> {
    evidence.child = capture_spawned_child(child, pid).await?;
    write_runtime_evidence(server_root, evidence)
}

/// Reads what the exited server left buffered, without waiting for a descendant that holds the pipes open.
async fn drain_output(readers: Vec<ReaderTask>) {
    let deadline = tokio::time::Instant::now() + OUTPUT_DRAIN_GRACE;
    for mut reader in readers {
        if tokio::time::timeout_at(deadline, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
            let _ = reader.await;
        }
    }
}

/// The process group the server leads, killed with it while the server is still unreaped.
struct ServerGroup {
    pid: u32,
    armed: bool,
}

impl ServerGroup {
    const fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    /// Kills every process in the group. Valid only until the server is reaped.
    fn kill(&mut self) -> bool {
        self.armed = false;
        kill_server_group(self.pid)
    }

    /// Leaves the group alone once the server is reaped, when its id may be reused.
    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ServerGroup {
    fn drop(&mut self) {
        if self.armed {
            self.kill();
        }
    }
}

/// Kills the process group `pid` leads. False when there is no such group to signal.
fn kill_server_group(pid: u32) -> bool {
    use rustix::process::{Pid, Signal, kill_process_group};

    let Some(group) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    match kill_process_group(group, Signal::KILL) {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(pid, %error, "could not signal the server's process group");
            false
        }
    }
}

/// Ends a child whose supervision failed, so the evidence withdrawn afterwards names nothing alive.
async fn abandon_child(
    child: &mut Child,
    group: &mut ServerGroup,
    readers: Vec<ReaderTask>,
    outstanding: &mut Outstanding,
) {
    let pid = child.id();
    group.kill();
    let _ = tokio::time::timeout(ABANDONED_CHILD_REAP_TIMEOUT, child.kill()).await;
    // A child reaped or signaled by someone else can no longer be waited on,
    // so its absence from the process table is what shows it is gone.
    let gone =
        matches!(child.try_wait(), Ok(Some(_))) || pid.is_none_or(|pid| !process_is_running(pid));
    outstanding.child_unconfirmed |= !gone;
    for reader in readers {
        reader.abort();
    }
}

/// Whether `pid` names a process that has not yet exited. A zombie has.
fn process_is_running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or_else(
        |error| error.kind() != std::io::ErrorKind::NotFound,
        |stat| {
            stat.rsplit_once(") ")
                .is_none_or(|(_, rest)| !rest.starts_with(['Z', 'X']))
        },
    )
}

type ReaderTask = tokio::task::JoinHandle<()>;

/// The spawned child's identity, or `None` when it has already exited.
///
/// A child that exits at once loses its executable link before it becomes
/// waitable, and its exit is reported like any other rather than as a
/// runtime-state failure.
async fn capture_spawned_child(
    child: &mut Child,
    pid: u32,
) -> ServerResult<Option<ProcessEvidence>> {
    let error = match capture_java_child(pid) {
        Ok(evidence) => return Ok(Some(evidence)),
        Err(error) => error,
    };
    let exiting = matches!(
        &error,
        ServerError::RuntimeState { path, source }
            if source.kind() == std::io::ErrorKind::NotFound
                && *path == Path::new(&format!("/proc/{pid}/exe"))
    );
    if !exiting {
        return Err(error);
    }
    match tokio::time::timeout(EXITING_CHILD_GRACE, child.wait()).await {
        Ok(Ok(_)) => Ok(None),
        Ok(Err(wait_error)) => Err(ServerError::ProcessIo(wait_error)),
        Err(_) => Err(error),
    }
}

fn spawn_child(
    command: &ServerCommand,
    observer: &ServerObserver,
    capture: &Arc<Mutex<OutputCapture>>,
    finished_starting: &Arc<AtomicBool>,
) -> ServerResult<(Child, ChildStdin, Vec<ReaderTask>, u32)> {
    std::fs::create_dir_all(&command.working_directory).map_err(|source| ServerError::Io {
        path: command.working_directory.clone(),
        source,
    })?;
    let mut process = Command::new(&command.java);
    process
        .args(&command.arguments)
        .current_dir(&command.working_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // A group of its own, so a Ctrl-C or hangup aimed at the supervisor's
    // terminal reaches the server only through the supervisor's graceful stop.
    #[cfg(unix)]
    process.process_group(0);
    let mut child = process
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
            Arc::clone(finished_starting),
        )),
        tokio::spawn(read_pipe(
            stderr,
            CaptureStream::Stderr,
            Arc::clone(capture),
            observer.clone(),
            Arc::clone(finished_starting),
        )),
    ];
    Ok((child, stdin, readers, pid))
}

/// A line being delivered to the server's input while supervision keeps polling the child and serving requests.
type PendingStdinWrite =
    std::pin::Pin<Box<dyn std::future::Future<Output = (ChildStdin, std::io::Result<()>)> + Send>>;

fn begin_write(mut stdin: ChildStdin, line: Vec<u8>) -> PendingStdinWrite {
    Box::pin(async move {
        let delivery = tokio::time::timeout(STDIN_WRITE_TIMEOUT, async {
            stdin.write_all(&line).await?;
            stdin.flush().await
        })
        .await;
        let result = delivery.unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the server did not read its input",
            ))
        });
        (stdin, result)
    })
}

/// Why the input is being written to.
enum WriteKind {
    Stop,
    /// A console line, answered once the write settles.
    Console(oneshot::Sender<ControlResponse>),
}

struct PendingWrite {
    delivery: PendingStdinWrite,
    kind: WriteKind,
}

fn is_one_console_line(command: &str) -> bool {
    !command.trim().is_empty() && !command.contains(['\0', '\n', '\r'])
}

fn refuse_unavailable_input(waiters: &mut Vec<oneshot::Sender<ControlResponse>>) {
    for waiter in waiters.drain(..) {
        let _ = waiter.send(ControlResponse::Refused(
            "server input is unavailable; use Kill to stop this server".to_owned(),
        ));
    }
}

/// The exit of a child whose input closed because it is already exiting.
async fn exit_of_closed_input(
    child: &mut Child,
    error: &std::io::Error,
) -> Option<std::process::ExitStatus> {
    if error.kind() != std::io::ErrorKind::BrokenPipe {
        return None;
    }
    tokio::time::timeout(STOP_WRITE_EXIT_GRACE, child.wait())
        .await
        .ok()?
        .ok()
}

/// How the child has been asked to end.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
    Running,
    /// A stop already asked for. A restart cannot turn it back into a start.
    Stopping,
    Killed,
}

/// What supervision has been asked to do with one child and what it has done so far.
struct ChildControl {
    restart: bool,
    ending: Ending,
    input_available: bool,
    /// Held by the pending write while one is in flight.
    stdin: Option<ChildStdin>,
    pending_write: Option<PendingWrite>,
    /// Requests that need the input while a console line is being written, kept in arrival order.
    deferred: VecDeque<ControlRequest>,
}

impl ChildControl {
    const fn new(stdin: ChildStdin) -> Self {
        Self {
            restart: false,
            ending: Ending::Running,
            input_available: true,
            stdin: Some(stdin),
            pending_write: None,
            deferred: VecDeque::new(),
        }
    }

    fn stopping(&self) -> bool {
        self.ending != Ending::Running
    }

    fn console_write_pending(&self) -> bool {
        matches!(
            self.pending_write,
            Some(PendingWrite {
                kind: WriteKind::Console(_),
                ..
            })
        )
    }

    /// The next request that waited behind a console write, once none is in flight.
    fn next_deferred(&mut self) -> Option<ControlRequest> {
        if self.console_write_pending() {
            return None;
        }
        self.deferred.pop_front()
    }

    fn exit(&self, status: std::process::ExitStatus) -> ServerExit {
        let mut exit = exit_from_status(status);
        exit.stop_requested = self.stopping() && !self.restart;
        exit
    }

    /// Answers what can no longer be served once the child has exited.
    fn conclude(
        &mut self,
        status: std::process::ExitStatus,
        completion_waiters: &mut Vec<oneshot::Sender<ControlResponse>>,
    ) -> (ServerExit, bool) {
        if let Some(PendingWrite {
            kind: WriteKind::Console(response),
            ..
        }) = self.pending_write.take()
        {
            let _ = response.send(ControlResponse::Refused(
                "the server exited before the command was delivered".to_owned(),
            ));
        }
        for request in self.deferred.drain(..) {
            if request.action == ControlAction::Stop {
                completion_waiters.push(request.response);
            } else {
                let _ = request.response.send(server_has_stopped());
            }
        }
        (self.exit(status), self.restart)
    }

    /// Settles a finished input write. Returns the exit when the write failed because the child was exiting.
    async fn write_finished(
        &mut self,
        child: &mut Child,
        completion_waiters: &mut Vec<oneshot::Sender<ControlResponse>>,
        stdin: ChildStdin,
        result: std::io::Result<()>,
    ) -> Option<std::process::ExitStatus> {
        let pending = self.pending_write.take()?;
        self.stdin = Some(stdin);
        match pending.kind {
            WriteKind::Console(response) => {
                // A closed input pipe does not transfer process ownership
                // or stop output capture. Keep supervising the child.
                self.input_available &= result.is_ok();
                let _ = response.send(if result.is_ok() {
                    ControlResponse::Accepted
                } else {
                    ControlResponse::Refused(
                        "server input is unavailable; command delivery was not confirmed"
                            .to_owned(),
                    )
                });
                None
            }
            WriteKind::Stop => {
                // After a kill the write failing is expected and the exit settles the waiters.
                let Err(error) = result else { return None };
                if self.ending == Ending::Killed {
                    return None;
                }
                if let Some(status) = exit_of_closed_input(child, &error).await {
                    return Some(status);
                }
                // The child is alive and cannot be asked to stop, which leaves it
                // running and supervised like a failed console write does.
                self.input_available = false;
                self.ending = Ending::Running;
                self.restart = false;
                refuse_unavailable_input(completion_waiters);
                None
            }
        }
    }

    fn handle_request(
        &mut self,
        child: &mut Child,
        completion_waiters: &mut Vec<oneshot::Sender<ControlResponse>>,
        request: ControlRequest,
    ) -> ServerResult<()> {
        match request.action {
            ControlAction::Status => {
                let _ = request.response.send(ControlResponse::Running);
            }
            ControlAction::Console(_) if self.stopping() || !self.input_available => {
                let _ = request.response.send(ControlResponse::Refused(
                    if self.stopping() {
                        "the server is stopping or restarting"
                    } else {
                        "server input is unavailable"
                    }
                    .to_owned(),
                ));
            }
            ControlAction::Console(command) if !is_one_console_line(&command) => {
                let _ = request.response.send(ControlResponse::Refused(
                    "console command must be one line".to_owned(),
                ));
            }
            ControlAction::Console(_) | ControlAction::Stop | ControlAction::Restart
                if self.console_write_pending() =>
            {
                self.deferred.push_back(request);
            }
            ControlAction::Console(command) => {
                let Some(input) = self.stdin.take() else {
                    let _ = request.response.send(ControlResponse::Refused(
                        "server input is unavailable".to_owned(),
                    ));
                    self.input_available = false;
                    return Ok(());
                };
                let mut line = command.into_bytes();
                line.push(b'\n');
                self.pending_write = Some(PendingWrite {
                    delivery: begin_write(input, line),
                    kind: WriteKind::Console(request.response),
                });
            }
            ControlAction::Stop | ControlAction::Restart if !self.input_available => {
                refuse_unavailable_input(&mut vec![request.response]);
            }
            ControlAction::Restart if self.stopping() && !self.restart => {
                let _ = request.response.send(ControlResponse::Refused(
                    "the server is already stopping".to_owned(),
                ));
            }
            // An explicit stop is the last word: a restart still waiting
            // for the old child ends with it stopped. A second restart
            // joins the first, so one stop is followed by exactly one start.
            ControlAction::Stop | ControlAction::Restart => {
                if !self.stopping() {
                    let Some(input) = self.stdin.take() else {
                        refuse_unavailable_input(&mut vec![request.response]);
                        return Ok(());
                    };
                    self.ending = Ending::Stopping;
                    self.pending_write = Some(PendingWrite {
                        delivery: begin_write(input, b"stop\n".to_vec()),
                        kind: WriteKind::Stop,
                    });
                }
                self.restart = matches!(request.action, ControlAction::Restart);
                completion_waiters.push(request.response);
            }
            ControlAction::Kill => {
                self.restart = false;
                self.ending = Ending::Killed;
                completion_waiters.push(request.response);
                // The whole group, so a wrapper's own children go with it.
                if let Some(pid) = child.id() {
                    kill_server_group(pid);
                }
                child.start_kill().map_err(ServerError::ProcessIo)?;
            }
        }
        Ok(())
    }
}

async fn supervise_child(
    child: &mut Child,
    stdin: ChildStdin,
    requests: &mut mpsc::UnboundedReceiver<ControlRequest>,
    completion_waiters: &mut Vec<oneshot::Sender<ControlResponse>>,
) -> ServerResult<(ServerExit, bool)> {
    let mut interval = tokio::time::interval(EXIT_POLL_INTERVAL);
    let mut control = ChildControl::new(stdin);
    // Every sender is gone once the control service has ended, which a closed
    // channel reports on every receive. It is not polled again after that.
    let mut requests_open = true;
    loop {
        if let Some(request) = control.next_deferred() {
            control.handle_request(child, completion_waiters, request)?;
            continue;
        }
        tokio::select! {
            _ = interval.tick() => {
                if let Some(status) = child.try_wait().map_err(ServerError::ProcessIo)? {
                    return Ok(control.conclude(status, completion_waiters));
                }
            }
            (returned, result) = async {
                match control.pending_write.as_mut() {
                    Some(write) => (&mut write.delivery).await,
                    None => std::future::pending().await,
                }
            }, if control.pending_write.is_some() => {
                if let Some(status) = control
                    .write_finished(child, completion_waiters, returned, result)
                    .await
                {
                    return Ok(control.conclude(status, completion_waiters));
                }
            }
            request = requests.recv(), if requests_open => {
                let Some(request) = request else {
                    requests_open = false;
                    continue;
                };
                control.handle_request(child, completion_waiters, request)?;
            }
        }
    }
}

async fn read_pipe<R>(
    pipe: Option<R>,
    stream: CaptureStream,
    capture: Arc<Mutex<OutputCapture>>,
    observer: ServerObserver,
    finished_starting: Arc<AtomicBool>,
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
                if announces_startup_finished(&line.text) {
                    finished_starting.store(true, Ordering::Relaxed);
                }
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

/// The dedicated server's own announcement that it accepts players:
/// `Done (<seconds>)! For help, type "help"`, logged by vanilla's
/// `DedicatedServer` once startup completes. Loaders keep the line as is.
fn announces_startup_finished(text: &str) -> bool {
    text.split_once("Done (")
        .and_then(|(_, rest)| rest.split_once(')'))
        .is_some_and(|(_, rest)| rest.starts_with("! For help, type \"help\""))
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

/// How long to wait before accepting again after a failed accept, or `None`
/// when the failure belongs to one connection and the next accept is
/// unaffected.
fn accept_retry_delay(error: &std::io::Error) -> Option<Duration> {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::Interrupted => None,
        _ => Some(ACCEPT_RETRY_DELAY),
    }
}

/// Serves control connections until the task is aborted. A failed accept never
/// ends the service, because the supervised child would then be left running
/// with no way to stop it.
async fn serve_control(listener: UnixListener, requests: mpsc::UnboundedSender<ControlRequest>) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                if let Some(delay) = accept_retry_delay(&error) {
                    tokio::time::sleep(delay).await;
                }
                continue;
            }
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
    // A request that reaches a run which has already ended is refused rather
    // than left to a closed connection.
    let response = match requests.send(ControlRequest {
        action,
        response: sender,
    }) {
        Ok(()) => receiver.await.unwrap_or_else(|_| server_has_stopped()),
        Err(_) => server_has_stopped(),
    };
    let mut response =
        serde_json::to_vec(&response).map_err(|_| ServerError::InvalidControlResponse)?;
    response.push(b'\n');
    writer
        .write_all(&response)
        .await
        .map_err(ServerError::ProcessIo)
}

fn server_has_stopped() -> ControlResponse {
    ControlResponse::Refused("the server has stopped".to_owned())
}

/// Sends a lifecycle or console request through the live owner channel.
pub async fn send_control(
    server_root: &Path,
    action: &ControlAction,
) -> ServerResult<ControlResponse> {
    if let ControlAction::Console(command) = action
        && (command.trim().is_empty() || command.contains(['\0', '\n', '\r']))
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
        ControlAction::Console(_) => {
            tokio::time::timeout(CONTROL_TIMEOUT, read)
                .await
                .map_err(|_| ServerError::ControlUnavailable {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "server did not confirm console input within 30 seconds",
                    ),
                })?
        }
        ControlAction::Status => read.await,
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
        finished_starting: false,
        stop_requested: false,
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
        let path = runtime_lock_path(server_root);
        let parent = path.parent().unwrap_or(server_root);
        std::fs::create_dir_all(parent).map_err(|source| ServerError::RuntimeState {
            path: parent.to_path_buf(),
            source,
        })?;
        match create_runtime_lock(server_root) {
            Ok(file) => Ok(Self { path, _file: file }),
            Err(ServerError::RuntimeState { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                // A stale lock is removable only when runtime evidence proves
                // the former owner and child are gone.
                if matches!(
                    classify_runtime(server_root)?,
                    RuntimeClassification::ProvenStale { .. }
                ) {
                    clear_stale_runtime(server_root)?;
                    let file = create_runtime_lock(server_root)?;
                    Ok(Self { path, _file: file })
                } else {
                    Err(ServerError::AlreadyRunning)
                }
            }
            Err(error) => Err(error),
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

    use std::os::unix::fs::MetadataExt as _;

    use super::*;
    use crate::runtime::control_socket_file_name;

    /// The process group a process belongs to, from `/proc/<pid>/stat`.
    fn process_group_of(pid: u32) -> String {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("stat");
        let (_, rest) = stat.rsplit_once(") ").expect("stat format");
        // Fields after the command name: state, parent, process group.
        rest.split(' ').nth(2).expect("process group").to_owned()
    }

    #[tokio::test]
    async fn the_server_child_runs_in_a_process_group_of_its_own() {
        let directory = tempfile::tempdir().expect("directory");
        let command = ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".to_owned(), "exec sleep 30".to_owned()],
            working_directory: directory.path().join("server"),
        };
        let capture = Arc::new(Mutex::new(OutputCapture::new(directory.path())));
        let (mut child, _stdin, readers, pid) = spawn_child(
            &command,
            &ServerObserver::silent(),
            &capture,
            &Arc::new(AtomicBool::new(false)),
        )
        .expect("spawn");

        let group = process_group_of(pid);
        let own_group = process_group_of(std::process::id());
        child.kill().await.expect("kill");
        for reader in readers {
            let _ = reader.await;
        }

        assert_eq!(
            group,
            pid.to_string(),
            "the child leads its own group, so a terminal signal for the supervisor does not reach it"
        );
        assert_ne!(group, own_group);
    }

    #[tokio::test]
    async fn a_child_that_exits_before_its_identity_is_read_is_reported_as_an_exit() {
        let mut child = Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn");
        let pid = child.id().expect("pid");
        // Wait for the unreaped zombie, which has no executable to read.
        let stat = format!("/proc/{pid}/stat");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !std::fs::read_to_string(&stat).is_ok_and(|text| {
            text.rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "the child never exited"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let evidence = capture_spawned_child(&mut child, pid)
            .await
            .expect("not a runtime-state failure");
        assert!(evidence.is_none());
        assert_eq!(child.wait().await.expect("status").code(), Some(7));
    }

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
        let (observer, mut events) = ServerObserver::channel();
        let task = tokio::spawn(async move {
            ServerSupervisor::run(&command, &server_root, &capture_path, &observer).await
        });
        for _ in 0..50 {
            if active_control_socket_path(root.path())
                .is_ok_and(|path| path.is_some_and(|path| path.exists()))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for invalid in ["", "   ", "list\nstop", "list\rstop", "list\0stop"] {
            assert!(matches!(
                send_control(root.path(), &ControlAction::Console(invalid.to_owned())).await,
                Err(ServerError::InvalidConsoleCommand)
            ));
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
        // The answer waits for the release, so nothing is left to wait on.
        assert!(!runtime_path(root.path()).exists());
        assert!(!runtime_lock_path(root.path()).exists());
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::Stopped
        ));
        assert_eq!(task.await.expect("join").expect("supervise").code, Some(0));
        let mut output = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let ServerEvent::Output(line) = event {
                output.push(line.text);
            }
        }
        assert_eq!(output, ["got:hello", "got:stop"]);
        assert!(
            send_control(root.path(), &ControlAction::Console("list".to_owned()))
                .await
                .is_err()
        );
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
        assert!(matches!(
            send_control(
                server.root.path(),
                &ControlAction::Console("list".to_owned())
            )
            .await
            .expect("console refusal"),
            ControlResponse::Refused(_)
        ));
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
        assert!(matches!(
            send_control(
                server.root.path(),
                &ControlAction::Console("list".to_owned())
            )
            .await
            .expect("console refusal"),
            ControlResponse::Refused(_)
        ));
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
    #[tokio::test]
    async fn closed_stdin_refuses_commands_without_abandoning_process_or_capture() {
        let root = tempfile::tempdir().expect("root");
        let capture = tempfile::tempdir().expect("capture");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let command = ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".to_owned(),
                "exec 0<&-; echo input-closed; while :; do sleep 0.1; done".to_owned(),
            ],
            working_directory: root.path().join("server"),
        };
        let (observer, mut events) = ServerObserver::channel();
        let server_root = root.path().to_path_buf();
        let capture_path = capture.path().to_path_buf();
        let task = tokio::spawn(async move {
            ServerSupervisor::run(&command, &server_root, &capture_path, &observer).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ServerEvent::Output(line)) = events.recv().await
                    && line.text == "input-closed"
                {
                    break;
                }
            }
        })
        .await
        .expect("child closed stdin");
        for _ in 0..2 {
            assert!(matches!(
                send_control(root.path(), &ControlAction::Console("list".to_owned()))
                    .await
                    .expect("answered"),
                ControlResponse::Refused(_)
            ));
        }
        for action in [ControlAction::Stop, ControlAction::Restart] {
            assert!(matches!(
                send_control(root.path(), &action).await.expect("refused"),
                ControlResponse::Refused(_)
            ));
        }
        assert!(
            classify_runtime(root.path())
                .expect("runtime")
                .is_controlled()
        );
        assert!(!task.is_finished());
        assert_eq!(
            send_control(root.path(), &ControlAction::Status)
                .await
                .expect("status"),
            ControlResponse::Running
        );
        assert!(matches!(
            send_control(root.path(), &ControlAction::Kill)
                .await
                .expect("kill"),
            ControlResponse::Exited(_)
        ));
        task.await.expect("join").expect("supervise");
    }

    /// Starts a supervised shell `script` and waits until it prints `ready`.
    async fn supervise_script(
        script: &str,
        ready: &str,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        mpsc::UnboundedReceiver<ServerEvent>,
        tokio::task::JoinHandle<ServerResult<ServerExit>>,
    ) {
        let root = tempfile::tempdir().expect("root");
        let capture = tempfile::tempdir().expect("capture");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let command = ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".to_owned(), script.to_owned()],
            working_directory: root.path().join("server"),
        };
        let (observer, mut events) = ServerObserver::channel();
        let server_root = root.path().to_path_buf();
        let capture_path = capture.path().to_path_buf();
        let task = tokio::spawn(async move {
            ServerSupervisor::run(&command, &server_root, &capture_path, &observer).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ServerEvent::Output(line)) = events.recv().await
                    && line.text == ready
                {
                    break;
                }
            }
        })
        .await
        .expect("child became ready");
        (root, capture, events, task)
    }

    #[tokio::test]
    async fn a_stop_or_restart_sent_first_to_a_closed_input_is_refused_and_leaves_the_child_running()
     {
        let (root, _capture, mut events, task) = supervise_script(
            "exec 0<&-; echo input-closed; while :; do echo alive; sleep 0.1; done",
            "input-closed",
        )
        .await;
        for action in [ControlAction::Stop, ControlAction::Restart] {
            assert!(matches!(
                send_control(root.path(), &action).await.expect("answered"),
                ControlResponse::Refused(_)
            ));
        }
        assert!(!task.is_finished());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ServerEvent::Output(line)) = events.recv().await
                    && line.text == "alive"
                {
                    break;
                }
            }
        })
        .await
        .expect("output capture continues");
        assert!(matches!(
            send_control(root.path(), &ControlAction::Kill)
                .await
                .expect("kill"),
            ControlResponse::Exited(_)
        ));
        task.await.expect("join").expect("supervise");
    }

    #[tokio::test]
    async fn a_stop_that_meets_a_child_already_exiting_reports_the_real_exit() {
        let (root, _capture, _events, task) = supervise_script(
            "exec 0<&-; echo input-closed; sleep 0.35; exit 3",
            "input-closed",
        )
        .await;
        let response = send_control(root.path(), &ControlAction::Stop)
            .await
            .expect("answered");
        let ControlResponse::Exited(exit) = response else {
            panic!("the stop settles with the exit: {response:?}");
        };
        assert_eq!(exit.code, Some(3));
        assert!(exit.stop_requested);
        assert_eq!(task.await.expect("join").expect("supervise"), exit);
    }

    #[tokio::test]
    async fn a_stop_that_cannot_be_delivered_does_not_block_a_kill() {
        let (root, _capture, _events, task) =
            supervise_script("echo ready; exec sleep 1000", "ready").await;
        // Leaves less room in the input pipe than a stop line needs.
        let filler = "a".repeat(65_533);
        assert_eq!(
            send_control(root.path(), &ControlAction::Console(filler))
                .await
                .expect("answered"),
            ControlResponse::Accepted
        );
        let stop = tokio::spawn({
            let root = root.path().to_path_buf();
            async move { send_control(&root, &ControlAction::Stop).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let kill = tokio::time::timeout(
            Duration::from_secs(3),
            send_control(root.path(), &ControlAction::Kill),
        )
        .await
        .expect("kill is answered while the stop is undelivered")
        .expect("kill");
        assert!(matches!(kill, ControlResponse::Exited(_)), "{kill:?}");
        assert!(matches!(
            stop.await.expect("join").expect("stop"),
            ControlResponse::Exited(_)
        ));
        task.await.expect("join").expect("supervise");
    }

    /// Runs a child that prints `line` and exits with code 0 once it reads a
    /// console line that is not a stop, so nothing asked it to stop. It waits
    /// for that line so its exit cannot outrun the supervisor recording it.
    async fn run_to_exit(line: &str) -> ServerExit {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let capture = tempfile::tempdir().expect("capture");
        let script = format!(
            "echo '{line}'; while IFS= read -r input; do [ \"$input\" = quit ] && exit 0; done"
        );
        let command = ServerCommand {
            java: PathBuf::from("/bin/sh"),
            arguments: vec!["-c".to_owned(), script],
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
        send_control(root.path(), &ControlAction::Console("quit".to_owned()))
            .await
            .expect("console");
        task.await.expect("join").expect("supervise")
    }

    #[tokio::test]
    async fn a_clean_exit_before_startup_finished_is_recorded_as_such() {
        let failed = run_to_exit(
            "[main/ERROR] [net.minecraft.server.Main/FATAL]: Failed to start the minecraft server",
        )
        .await;
        assert_eq!(failed.code, Some(0));
        assert!(!failed.finished_starting);
        assert!(!failed.stop_requested);

        let stopped = run_to_exit(
            r#"[Server thread/INFO] [minecraft/DedicatedServer]: Done (4.211s)! For help, type "help""#,
        )
        .await;
        assert_eq!(stopped.code, Some(0));
        assert!(stopped.finished_starting);
        assert!(!stopped.stop_requested);
    }

    /// The control sockets a run for `root` left in any directory a run may use.
    fn control_sockets_of(root: &Path) -> Vec<PathBuf> {
        let name = control_socket_file_name(root, uuid::Uuid::nil()).expect("name");
        let prefix = name.trim_end_matches("00000000000000000000000000000000.sock");
        let uid = std::fs::metadata("/proc/self").expect("self").uid();
        let mut directories = vec![PathBuf::from(format!("/tmp/evx-{uid}"))];
        directories.extend(std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from));
        directories.extend(
            std::fs::read_dir("/tmp")
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!("evx-{uid}-"))
                })
                .map(|entry| entry.path()),
        );
        directories
            .iter()
            .flat_map(|directory| std::fs::read_dir(directory).into_iter().flatten().flatten())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .map(|entry| entry.path())
            .collect()
    }

    fn assert_released(root: &Path) {
        assert!(
            !runtime_path(root).exists(),
            "runtime evidence is withdrawn"
        );
        assert!(!runtime_lock_path(root).exists(), "the lock is released");
        assert!(control_sockets_of(root).is_empty(), "the socket is removed");
        assert!(matches!(
            classify_runtime(root).expect("classify"),
            RuntimeClassification::Stopped
        ));
    }

    #[tokio::test]
    async fn a_start_that_fails_leaves_the_server_stopped_and_startable() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let capture = tempfile::tempdir().expect("capture");
        let mut command = ServerCommand {
            java: PathBuf::from("/nonexistent/java"),
            arguments: Vec::new(),
            working_directory: root.path().join("server"),
        };

        let failed = ServerSupervisor::run(
            &command,
            root.path(),
            capture.path(),
            &ServerObserver::silent(),
        )
        .await;

        assert!(matches!(failed, Err(ServerError::ProcessStart { .. })));
        assert_released(root.path());

        command.java = PathBuf::from("/bin/true");
        ServerSupervisor::run(
            &command,
            root.path(),
            capture.path(),
            &ServerObserver::silent(),
        )
        .await
        .expect("a later start in the same process succeeds");
    }

    #[tokio::test]
    async fn a_canceled_run_withdraws_its_evidence_and_leaves_the_server_startable() {
        let server = supervise_journaled(None).await;

        server.task.abort();
        assert!(server.task.await.expect_err("canceled").is_cancelled());

        assert_released(server.root.path());
        drop(RuntimeGuard::acquire(server.root.path()).expect("a later start is not refused"));
    }

    #[tokio::test]
    async fn a_canceled_run_kills_everything_the_server_started() {
        let (root, _capture, mut events, task) = supervise_script(
            "sleep 1000 & p=$!; echo ready; echo \"descendant:$p\"; wait",
            "ready",
        )
        .await;
        let descendant = announced_descendant(&mut events).await;

        task.abort();
        assert!(task.await.expect_err("canceled").is_cancelled());

        wait_until_not_running(descendant).await;
        assert_released(root.path());
    }

    #[test]
    fn a_canceled_run_keeps_its_evidence_while_its_child_is_still_alive() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let evidence = RuntimeEvidence::for_current_supervisor().expect("evidence");
        write_runtime_evidence(root.path(), &evidence).expect("write");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn");

        drop(CanceledRunEvidence {
            server_root: root.path().to_path_buf(),
            generation: evidence.generation,
            live_child: Arc::new(AtomicU32::new(child.id())),
            exit_wait: Duration::from_millis(50),
            armed: true,
        });

        assert!(runtime_path(root.path()).exists());
        child.kill().expect("kill");
        child.wait().expect("reap");
    }

    #[test]
    fn a_canceled_run_never_withdraws_evidence_written_by_another_run() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher");
        let other = RuntimeEvidence::for_current_supervisor().expect("evidence");
        write_runtime_evidence(root.path(), &other).expect("write");

        drop(CanceledRunEvidence {
            server_root: root.path().to_path_buf(),
            generation: uuid::Uuid::new_v4(),
            live_child: Arc::new(AtomicU32::new(NO_CHILD)),
            exit_wait: Duration::from_millis(50),
            armed: true,
        });

        assert!(runtime_path(root.path()).exists());
    }

    #[tokio::test]
    async fn a_run_whose_first_evidence_cannot_be_written_releases_the_control_socket() {
        let root = tempfile::tempdir().expect("root");
        let launcher = root.path().join(".launcher");
        std::fs::create_dir_all(launcher.join("server-runtime.json.tmp")).expect("blocker");
        let capture = tempfile::tempdir().expect("capture");
        let command = ServerCommand {
            java: PathBuf::from("/bin/true"),
            arguments: Vec::new(),
            working_directory: root.path().join("server"),
        };

        let failed = ServerSupervisor::run(
            &command,
            root.path(),
            capture.path(),
            &ServerObserver::silent(),
        )
        .await;

        assert!(matches!(failed, Err(ServerError::RuntimeState { .. })));
        std::fs::remove_dir(launcher.join("server-runtime.json.tmp")).expect("unblock");
        assert_released(root.path());
    }

    #[tokio::test]
    async fn a_replacement_whose_evidence_cannot_be_written_is_killed_and_answers_the_restart() {
        let server = supervise_journaled(None).await;
        let restart = tokio::spawn({
            let root = server.root.path().to_path_buf();
            async move { send_control(&root, &ControlAction::Restart).await }
        });
        // The old child is still saving its world, so the replacement's
        // evidence is the next thing written.
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::create_dir(server.root.path().join(".launcher/server-runtime.json.tmp"))
            .expect("blocker");

        let answer = restart.await.expect("join").expect("answered");
        let ControlResponse::Refused(detail) = answer else {
            panic!("the restart cannot report a start: {answer:?}");
        };
        assert!(detail.contains("could not be supervised"), "{detail}");
        assert!(matches!(
            server.task.await.expect("join"),
            Err(ServerError::RuntimeState { .. })
        ));
        std::fs::remove_dir(server.root.path().join(".launcher/server-runtime.json.tmp"))
            .expect("unblock");
        assert_released(server.root.path());
        assert_eq!(
            journal(&server.journal)[..2],
            ["start", "stop"],
            "the old child stopped first"
        );
    }

    #[tokio::test]
    async fn an_abandoned_child_is_gone_before_its_run_reports_it() {
        let mut child = Command::new("sleep")
            .arg("1000")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn");
        let pid = child.id().expect("pid");
        let mut outstanding = Outstanding::default();

        abandon_child(
            &mut child,
            &mut ServerGroup::new(pid),
            Vec::new(),
            &mut outstanding,
        )
        .await;

        assert!(!outstanding.child_unconfirmed);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[tokio::test]
    async fn a_reaped_process_is_not_running_and_a_live_one_is() {
        assert!(process_is_running(std::process::id()));
        let mut child = Command::new("true").spawn().expect("spawn");
        let pid = child.id().expect("pid");
        child.wait().await.expect("reaped");
        assert!(!process_is_running(pid));
    }

    /// The pid a script announced as `descendant:<pid>`.
    async fn announced_descendant(events: &mut mpsc::UnboundedReceiver<ServerEvent>) -> u32 {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ServerEvent::Output(line)) = events.recv().await
                    && let Some(pid) = line.text.strip_prefix("descendant:")
                {
                    return pid.parse().expect("pid");
                }
            }
        })
        .await
        .expect("the descendant was announced")
    }

    async fn wait_until_not_running(pid: u32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while process_is_running(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the process ended");
    }

    #[tokio::test]
    async fn a_descendant_holding_the_output_pipes_does_not_delay_the_exit() {
        let (root, _capture, mut events, task) = supervise_script(
            "sleep 3 & echo ready; while IFS= read -r line; do [ \"$line\" = stop ] && exit 0; done",
            "ready",
        )
        .await;
        let started = std::time::Instant::now();

        let stop = send_control(root.path(), &ControlAction::Stop)
            .await
            .expect("stop answered");

        let ControlResponse::Exited(exit) = stop else {
            panic!("the stop settles with the exit: {stop:?}");
        };
        assert!(exit.stop_requested);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the run waited for the descendant: {:?}",
            started.elapsed()
        );
        assert_eq!(task.await.expect("join").expect("supervise"), exit);
        let mut finished = false;
        while let Ok(event) = events.try_recv() {
            finished |= matches!(event, ServerEvent::Finished { .. } | ServerEvent::Exited(_));
        }
        assert!(finished, "the exit is reported");
    }

    #[tokio::test]
    async fn a_restart_is_not_held_behind_a_descendant_holding_the_output_pipes() {
        let (root, _capture, _events, task) = supervise_script(
            "(sleep 3 &); echo ready; while IFS= read -r line; do [ \"$line\" = stop ] && exit 0; done",
            "ready",
        )
        .await;

        let restart = tokio::time::timeout(
            Duration::from_secs(2),
            send_control(root.path(), &ControlAction::Restart),
        )
        .await
        .expect("the replacement started while the descendant lingered")
        .expect("restart answered");

        assert!(
            matches!(restart, ControlResponse::Restarted { .. }),
            "{restart:?}"
        );
        assert_eq!(
            send_control(root.path(), &ControlAction::Status)
                .await
                .expect("status"),
            ControlResponse::Running
        );
        send_control(root.path(), &ControlAction::Kill)
            .await
            .expect("kill");
        task.await.expect("join").expect("supervise");
    }

    #[tokio::test]
    async fn a_kill_ends_everything_the_server_started() {
        let (root, _capture, mut events, task) = supervise_script(
            "sleep 1000 & p=$!; echo ready; echo \"descendant:$p\"; wait",
            "ready",
        )
        .await;
        let descendant = announced_descendant(&mut events).await;
        assert!(process_is_running(descendant));

        let kill = send_control(root.path(), &ControlAction::Kill)
            .await
            .expect("kill answered");

        assert!(matches!(kill, ControlResponse::Exited(_)), "{kill:?}");
        wait_until_not_running(descendant).await;
        task.await.expect("join").expect("supervise");
    }

    #[tokio::test]
    async fn a_kill_is_answered_promptly_while_a_console_line_cannot_be_written() {
        let (root, _capture, _events, task) =
            supervise_script("echo ready; exec sleep 1000", "ready").await;
        // The child never reads its input, so this write cannot complete.
        let console = tokio::spawn({
            let root = root.path().to_path_buf();
            let line = "a".repeat(512 * 1024);
            async move { send_control(&root, &ControlAction::Console(line)).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;

        let kill = tokio::time::timeout(
            Duration::from_secs(2),
            send_control(root.path(), &ControlAction::Kill),
        )
        .await
        .expect("the kill is not queued behind the stuck write")
        .expect("kill answered");

        assert!(matches!(kill, ControlResponse::Exited(_)), "{kill:?}");
        assert!(matches!(
            console.await.expect("join").expect("console answered"),
            ControlResponse::Refused(_)
        ));
        task.await.expect("join").expect("supervise");
    }

    #[tokio::test]
    async fn a_child_that_exits_during_a_stuck_console_write_is_noticed_at_once() {
        let (root, _capture, _events, task) = supervise_script(
            // A descendant keeps the input pipe open, so the write does not fail on its own.
            "exec 3<&0; (sleep 4 <&3 3<&- &); echo ready; sleep 0.5; exit 4",
            "ready",
        )
        .await;
        let started = std::time::Instant::now();
        let console = tokio::spawn({
            let root = root.path().to_path_buf();
            let line = "a".repeat(512 * 1024);
            async move { send_control(&root, &ControlAction::Console(line)).await }
        });

        let exit = task.await.expect("join").expect("supervise");

        assert_eq!(exit.code, Some(4));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the exit waited for the write timeout: {:?}",
            started.elapsed()
        );
        assert!(matches!(
            console.await.expect("join").expect("console answered"),
            ControlResponse::Refused(_)
        ));
    }

    #[tokio::test]
    async fn a_stop_sent_during_a_console_write_is_carried_out_after_it() {
        // The shell reads nothing for a while, so the console line is still being written.
        let (root, _capture, _events, task) = supervise_script(
            "echo ready; sleep 0.6; while IFS= read -r line; do [ \"$line\" = stop ] && exit 0; done",
            "ready",
        )
        .await;
        let console = tokio::spawn({
            let root = root.path().to_path_buf();
            let line = "a".repeat(512 * 1024);
            async move { send_control(&root, &ControlAction::Console(line)).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;

        let stop = send_control(root.path(), &ControlAction::Stop)
            .await
            .expect("stop answered");

        let ControlResponse::Exited(exit) = stop else {
            panic!("the stop settles with the exit: {stop:?}");
        };
        assert_eq!(exit.code, Some(0));
        assert!(exit.stop_requested);
        assert_eq!(
            console.await.expect("join").expect("console answered"),
            ControlResponse::Accepted
        );
        task.await.expect("join").expect("supervise");
    }

    #[tokio::test]
    async fn a_request_that_reaches_a_finished_run_is_refused_not_dropped() {
        let (client, server) = UnixStream::pair().expect("pair");
        let (requests, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let connection = tokio::spawn(handle_connection(server, requests));

        let mut client = BufReader::new(client);
        let mut request = serde_json::to_vec(&ControlAction::Stop).expect("request");
        request.push(b'\n');
        client.get_mut().write_all(&request).await.expect("send");
        let mut response = Vec::new();
        client.read_until(b'\n', &mut response).await.expect("read");

        assert!(matches!(
            serde_json::from_slice(&response).expect("a response"),
            ControlResponse::Refused(_)
        ));
        connection.await.expect("join").expect("handled");
    }

    #[test]
    fn an_accept_failure_that_can_persist_is_retried_after_a_pause() {
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::ConnectionAborted, ErrorKind::ConnectionReset] {
            assert_eq!(accept_retry_delay(&Error::from(kind)), None, "{kind:?}");
        }
        // EMFILE and ENFILE on Linux must not turn the retry into a busy loop.
        for code in [24, 23] {
            assert_eq!(
                accept_retry_delay(&Error::from_raw_os_error(code)),
                Some(ACCEPT_RETRY_DELAY),
                "os error {code}"
            );
        }
    }

    #[tokio::test]
    async fn a_closed_request_channel_does_not_spin_supervision_and_the_exit_is_still_seen() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.6"])
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn");
        let stdin = child.stdin.take().expect("stdin");
        let (requests, mut receiver) = mpsc::unbounded_channel();
        drop(requests);
        let mut waiters = Vec::new();
        let mut polls = 0_u32;
        let mut supervision = std::pin::pin!(supervise_child(
            &mut child,
            stdin,
            &mut receiver,
            &mut waiters
        ));

        let (exit, restart) = std::future::poll_fn(|context| {
            polls += 1;
            supervision.as_mut().poll(context)
        })
        .await
        .expect("supervised");

        assert!(!restart);
        assert!(!exit.stop_requested);
        // Waking only for the exit poll takes a handful of polls. A spinning
        // loop takes many thousands in the same time.
        assert!(polls < 200, "supervision polled {polls} times");
    }

    #[test]
    fn only_the_dedicated_servers_ready_line_announces_startup_finished() {
        for ready in [
            r#"[28Sept.2026 17:49:01.433] [Server thread/INFO] [minecraft/DedicatedServer]: Done (12.345s)! For help, type "help""#,
            r#"[12:00:00] [Server thread/INFO]: Done (5.0s)! For help, type "help""#,
            r#"[12:00:00 INFO]: Done (3.101s)! For help, type "help" or "?""#,
        ] {
            assert!(announces_startup_finished(ready), "{ready}");
        }
        for other in [
            "[main/ERROR] [net.minecraft.server.Main/FATAL]: Failed to start the minecraft server",
            "[Worker-Main-1/INFO]: Done (preparing spawn area)",
            "[Server thread/INFO]: Preparing spawn area: 0%",
            r#"For help, type "help""#,
        ] {
            assert!(!announces_startup_finished(other), "{other}");
        }
    }
}
