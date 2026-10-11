//! Conservative Linux process identity and orphan classification.

use std::io::{Read as _, Seek as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ServerError, ServerResult};

pub const RUNTIME_FILE: &str = "server-runtime.json";
pub const RUNTIME_LOCK_FILE: &str = "server-runtime.lock";
const LEGACY_CONTROL_SOCKET_FILE: &str = "server-control.sock";
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;
/// Names the run in the environment of every server child it starts, so a
/// child started just before its supervisor died can still be found.
pub(crate) const SERVER_RUN_VARIABLE: &str = "EPHERVEX_SERVER_RUN";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessRole {
    Supervisor,
    JavaChild,
}

/// Evidence that identifies one process rather than merely one recyclable PID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessEvidence {
    pub role: ProcessRole,
    pub pid: u32,
    pub boot_id: String,
    pub start_ticks: u64,
    pub executable: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_device: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_inode: Option<u64>,
    pub started_at: DateTime<Utc>,
}

/// Ephemeral runtime evidence. Never copied into entity metadata or SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeEvidence {
    pub generation: Uuid,
    pub supervisor: ProcessEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<ProcessEvidence>,
    /// The directory holding the control socket. Absent in evidence written
    /// when sockets always lived in the shared per-user `/tmp` directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_directory: Option<PathBuf>,
    /// A child is being started and `child` does not name it yet. While set,
    /// a dead supervisor is not proof that nothing it started is alive, until
    /// no process carrying the run's [`SERVER_RUN_VARIABLE`] is left.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub child_pending: bool,
    /// The supervisor's session, which every child it starts inherits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_session: Option<u32>,
}

impl RuntimeEvidence {
    pub fn for_current_supervisor() -> ServerResult<Self> {
        Ok(Self {
            generation: Uuid::new_v4(),
            supervisor: capture_process(std::process::id(), ProcessRole::Supervisor)?,
            child: None,
            control_directory: None,
            child_pending: false,
            supervisor_session: read_stat(std::process::id()).ok().map(|stat| stat.session),
        })
    }

    pub fn record_child(&mut self, pid: u32) -> ServerResult<()> {
        self.child = Some(capture_process(pid, ProcessRole::JavaChild)?);
        Ok(())
    }
}

/// Runtime states deliberately distinguish ownership from mere liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeClassification {
    Stopped,
    Controlled {
        supervisor: ProcessEvidence,
        child: Option<ProcessEvidence>,
    },
    UnknownRunning {
        supervisor: ProcessEvidence,
        child: Option<ProcessEvidence>,
    },
    Orphaned {
        child: ProcessEvidence,
    },
    ProvenStale {
        evidence: RuntimeEvidence,
    },
}

impl RuntimeClassification {
    #[must_use]
    pub const fn may_be_running(&self) -> bool {
        matches!(
            self,
            Self::Controlled { .. } | Self::UnknownRunning { .. } | Self::Orphaned { .. }
        )
    }

    #[must_use]
    pub const fn is_controlled(&self) -> bool {
        matches!(self, Self::Controlled { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityState {
    VerifiedAlive,
    ProvenDead,
    Unverifiable,
}

/// Classifies runtime evidence without signaling or adopting any process.
pub fn classify_runtime(server_root: &Path) -> ServerResult<RuntimeClassification> {
    match read_runtime_evidence(server_root)? {
        Some(evidence) => Ok(classify_evidence(server_root, evidence)),
        None => classify_lock_holder(server_root),
    }
}

fn classify_evidence(server_root: &Path, evidence: RuntimeEvidence) -> RuntimeClassification {
    let supervisor = verify_process(&evidence.supervisor);
    let child = evidence.child.as_ref().map(verify_process);

    match (supervisor, child) {
        (IdentityState::VerifiedAlive, _) if control_socket_responds(server_root, &evidence) => {
            RuntimeClassification::Controlled {
                supervisor: evidence.supervisor,
                child: evidence.child,
            }
        }
        (IdentityState::VerifiedAlive | IdentityState::Unverifiable, _)
        | (IdentityState::ProvenDead, Some(IdentityState::Unverifiable)) => {
            RuntimeClassification::UnknownRunning {
                supervisor: evidence.supervisor,
                child: evidence.child,
            }
        }
        (IdentityState::ProvenDead, Some(IdentityState::VerifiedAlive)) => match evidence.child {
            Some(child) => RuntimeClassification::Orphaned { child },
            None => RuntimeClassification::UnknownRunning {
                supervisor: evidence.supervisor,
                child: None,
            },
        },
        // A child started just before the supervisor died has no evidence to
        // check, so the processes carrying the run's tag stand in for it.
        (IdentityState::ProvenDead, None | Some(IdentityState::ProvenDead))
            if evidence.child_pending && recorded_in_current_boot(&evidence.supervisor) =>
        {
            match find_pending_child(&evidence) {
                PendingChild::Gone => RuntimeClassification::ProvenStale { evidence },
                PendingChild::Alive(child) => RuntimeClassification::Orphaned { child },
                PendingChild::Unverifiable => RuntimeClassification::UnknownRunning {
                    supervisor: evidence.supervisor,
                    child: None,
                },
            }
        }
        (IdentityState::ProvenDead, None | Some(IdentityState::ProvenDead)) => {
            RuntimeClassification::ProvenStale { evidence }
        }
    }
}

/// What is left of a child that was being started when its supervisor died.
enum PendingChild {
    Gone,
    Alive(ProcessEvidence),
    Unverifiable,
}

/// How long a scan that found nothing waits before it looks once more.
const PENDING_CHILD_SETTLE: Duration = Duration::from_millis(100);

/// Finds the oldest live process the run tagged with [`SERVER_RUN_VARIABLE`].
///
/// A child that is still being exec'd shows an environment without the tag, so
/// finding nothing is only believed after a second look.
fn find_pending_child(evidence: &RuntimeEvidence) -> PendingChild {
    match scan_for_pending_child(evidence) {
        PendingChild::Gone => {
            std::thread::sleep(PENDING_CHILD_SETTLE);
            scan_for_pending_child(evidence)
        }
        found => found,
    }
}

/// A process that started before the supervisor cannot be its child. One whose
/// environment this user may not read is skipped unless it could be the child:
/// a child leads its own process group in the supervisor's session. That skip
/// also needs this process to read its own environment, so a blanket refusal
/// fails closed.
fn scan_for_pending_child(evidence: &RuntimeEvidence) -> PendingChild {
    let Ok(uid) = current_uid() else {
        return PendingChild::Unverifiable;
    };
    if std::fs::read("/proc/self/environ").is_err() {
        return PendingChild::Unverifiable;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return PendingChild::Unverifiable;
    };
    let mut oldest: Option<(u64, u32)> = None;
    for entry in entries {
        let Ok(entry) = entry else {
            return PendingChild::Unverifiable;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        match tagged_with_run(pid, uid, evidence) {
            Ok(Some(ticks)) if oldest.is_none_or(|(oldest_ticks, _)| ticks < oldest_ticks) => {
                oldest = Some((ticks, pid));
            }
            Ok(_) => {}
            Err(_) => return PendingChild::Unverifiable,
        }
    }
    let Some((_, pid)) = oldest else {
        return PendingChild::Gone;
    };
    capture_process(pid, ProcessRole::JavaChild)
        .map_or(PendingChild::Unverifiable, PendingChild::Alive)
}

/// The start ticks of `pid` when it is a live process carrying the run's tag,
/// or `None` when it is not. Fails when that cannot be told.
fn tagged_with_run(pid: u32, uid: u32, evidence: &RuntimeEvidence) -> std::io::Result<Option<u64>> {
    /// A kernel thread has no environment of its own.
    const KERNEL_THREAD: u64 = 0x0020_0000;

    let stat = match read_stat(pid) {
        Ok(stat) => stat,
        Err(error) if process_vanished(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if matches!(stat.state, 'Z' | 'X')
        || stat.flags & KERNEL_THREAD != 0
        || stat.start_ticks < evidence.supervisor.start_ticks
    {
        return Ok(None);
    }
    match std::fs::read(format!("/proc/{pid}/environ")) {
        Ok(environment) => {
            Ok(environment_carries_run(&environment, evidence.generation)
                .then_some(stat.start_ticks))
        }
        Err(error) if process_vanished(&error) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            let ours = std::fs::metadata(format!("/proc/{pid}")).is_ok_and(|m| m.uid() == uid);
            let could_be_child = ours
                && stat.process_group == pid
                && evidence
                    .supervisor_session
                    .is_none_or(|session| session == stat.session);
            if could_be_child { Err(error) } else { Ok(None) }
        }
        Err(error) => Err(error),
    }
}

/// The fields of `/proc/<pid>/stat` that process lookups use.
struct ProcessStat {
    state: char,
    process_group: u32,
    session: u32,
    flags: u64,
    start_ticks: u64,
}

fn read_stat(pid: u32) -> std::io::Result<ProcessStat> {
    let malformed = || std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed stat");
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, tail) = stat.rsplit_once(") ").ok_or_else(malformed)?;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let field = |index: usize| fields.get(index).copied().ok_or_else(malformed);
    Ok(ProcessStat {
        state: field(0)?.chars().next().ok_or_else(malformed)?,
        process_group: field(2)?.parse().map_err(|_| malformed())?,
        session: field(3)?.parse().map_err(|_| malformed())?,
        flags: field(6)?.parse().map_err(|_| malformed())?,
        start_ticks: field(19)?.parse().map_err(|_| malformed())?,
    })
}

fn environment_carries_run(environment: &[u8], generation: Uuid) -> bool {
    let tag = format!("{SERVER_RUN_VARIABLE}={generation}");
    environment
        .split(|byte| *byte == 0)
        .any(|entry| entry == tag.as_bytes())
}

/// Whether reading a `/proc/<pid>` file failed because the process is gone,
/// which the kernel reports as `ESRCH` for one that exits mid-read.
fn process_vanished(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}

/// How long a reader waits for a lock that is held but not yet written.
const LOCK_HOLDER_PATIENCE: Duration = Duration::from_millis(50);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
/// Times a start follows a lock file its former owner removed while it was being opened.
const LOCK_REOPEN_ATTEMPTS: usize = 16;

/// A supervisor holds its lock before it writes evidence and after it clears it.
///
/// Whether the lock is held is the kernel's answer, so a lock no live process
/// holds never counts as running, whatever it contains.
fn classify_lock_holder(server_root: &Path) -> ServerResult<RuntimeClassification> {
    use rustix::fs::{FlockOperation, flock};
    use rustix::io::Errno;

    let path = runtime_lock_path(server_root);
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuntimeClassification::Stopped);
        }
        Err(source) => return Err(ServerError::RuntimeState { path, source }),
    };
    let held = match flock(&file, FlockOperation::NonBlockingLockShared) {
        Ok(()) => false,
        Err(Errno::WOULDBLOCK) => true,
        Err(errno) => {
            return Err(ServerError::RuntimeState {
                path,
                source: errno.into(),
            });
        }
    };
    let deadline = std::time::Instant::now() + LOCK_HOLDER_PATIENCE;
    let holder = loop {
        let holder = read_lock_holder(&mut file, &path)?;
        if holder.is_some() || !held || std::time::Instant::now() >= deadline {
            break holder;
        }
        std::thread::sleep(LOCK_RETRY_INTERVAL);
    };
    match (held, holder) {
        (true, Some(supervisor)) => Ok(RuntimeClassification::UnknownRunning {
            supervisor,
            child: None,
        }),
        (true, None) => Err(ServerError::RuntimeMayBeAlive),
        (false, None) => Ok(RuntimeClassification::Stopped),
        (false, Some(holder)) => Ok(match verify_process(&holder) {
            IdentityState::ProvenDead => RuntimeClassification::ProvenStale {
                evidence: RuntimeEvidence {
                    generation: Uuid::nil(),
                    supervisor: holder,
                    child: None,
                    control_directory: None,
                    child_pending: false,
                    supervisor_session: None,
                },
            },
            // Locks written before they were flocked name a supervisor that is still alive.
            IdentityState::VerifiedAlive | IdentityState::Unverifiable => {
                RuntimeClassification::UnknownRunning {
                    supervisor: holder,
                    child: None,
                }
            }
        }),
    }
}

/// The supervisor a lock file names, or `None` when it is empty or unreadable as one.
fn read_lock_holder(
    file: &mut std::fs::File,
    path: &Path,
) -> ServerResult<Option<ProcessEvidence>> {
    let mut body = Vec::new();
    file.rewind()
        .and_then(|()| file.read_to_end(&mut body))
        .map_err(|source| ServerError::RuntimeState {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(serde_json::from_slice(&body).ok())
}

/// The exclusive `flock` on a server root's runtime lock file.
///
/// The kernel drops the lock with its holder, and only one process can take
/// it, so taking it is also how a leftover lock is claimed. The file is
/// removed while it is held, and a start that opened it meanwhile checks that
/// it still holds the file at the path.
#[derive(Debug)]
pub(crate) struct RuntimeLock {
    server_root: PathBuf,
    path: PathBuf,
    file: std::fs::File,
    /// Whether dropping the lock removes the file. A lock that found a run
    /// which may be alive leaves the file for that run's own cleanup.
    remove_on_drop: bool,
}

impl RuntimeLock {
    /// Takes the lock for the current supervisor and records it as the holder.
    /// Fails with `AlreadyRunning` when a run may still own the server root.
    pub(crate) fn acquire(server_root: &Path) -> ServerResult<Self> {
        let mut lock = Self::take(server_root)?;
        match lock.take_over() {
            Ok(()) => {}
            Err(ServerError::RuntimeMayBeAlive) => return Err(ServerError::AlreadyRunning),
            Err(error) => return Err(error),
        }
        let holder = capture_process(std::process::id(), ProcessRole::Supervisor)?;
        let body = serde_json::to_vec(&holder).map_err(|error| lock.state_error(error.into()))?;
        lock.file
            .set_len(0)
            .and_then(|()| lock.file.rewind())
            .and_then(|()| lock.file.write_all(&body))
            .and_then(|()| lock.file.sync_all())
            .map_err(|source| lock.state_error(source))?;
        lock.remove_on_drop = true;
        Ok(lock)
    }

    fn state_error(&self, source: std::io::Error) -> ServerError {
        ServerError::RuntimeState {
            path: self.path.clone(),
            source,
        }
    }

    /// Opens or creates the lock file and takes its `flock`.
    fn take(server_root: &Path) -> ServerResult<Self> {
        use rustix::fs::{FlockOperation, flock};
        use rustix::io::Errno;

        let path = runtime_lock_path(server_root);
        for _ in 0..LOCK_REOPEN_ATTEMPTS {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|source| ServerError::RuntimeState {
                    path: path.clone(),
                    source,
                })?;
            // A reader probing the lock holds it for an instant, so a refusal is retried briefly.
            let deadline = std::time::Instant::now() + LOCK_HOLDER_PATIENCE;
            loop {
                match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                    Ok(()) => break,
                    Err(Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(LOCK_RETRY_INTERVAL);
                    }
                    Err(Errno::WOULDBLOCK) => return Err(ServerError::AlreadyRunning),
                    Err(errno) => {
                        return Err(ServerError::RuntimeState {
                            path,
                            source: errno.into(),
                        });
                    }
                }
            }
            if names_same_file(&file, &path) {
                return Ok(Self {
                    server_root: server_root.to_path_buf(),
                    path,
                    file,
                    remove_on_drop: false,
                });
            }
        }
        Err(ServerError::AlreadyRunning)
    }

    /// Checks that nothing a former run left behind may still be alive, and
    /// clears what is left of it. Fails with `RuntimeMayBeAlive` otherwise.
    fn take_over(&mut self) -> ServerResult<()> {
        let server_root = self.server_root.clone();
        let holder = read_lock_holder(&mut self.file, &self.path)?;
        if holder
            .as_ref()
            .is_some_and(|holder| verify_process(holder) != IdentityState::ProvenDead)
        {
            return Err(ServerError::RuntimeMayBeAlive);
        }
        match read_runtime_evidence(&server_root)? {
            None => {}
            Some(evidence) => match classify_evidence(&server_root, evidence) {
                RuntimeClassification::ProvenStale { evidence } => {
                    remove_runtime_artifacts(&server_root, Some(&evidence))?;
                }
                _ => return Err(ServerError::RuntimeMayBeAlive),
            },
        }
        Ok(())
    }
}

impl Drop for RuntimeLock {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn names_same_file(file: &std::fs::File, path: &Path) -> bool {
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
        _ => false,
    }
}

/// Clears runtime artifacts only when every recorded process is proven gone.
///
/// Holds the runtime lock throughout, so a start cannot publish its own state
/// between the check and the removal.
pub fn clear_stale_runtime(server_root: &Path) -> ServerResult<()> {
    let mut lock = match RuntimeLock::take(server_root) {
        Ok(lock) => lock,
        Err(ServerError::AlreadyRunning) => return Err(ServerError::RuntimeMayBeAlive),
        Err(ServerError::RuntimeState { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    lock.take_over()?;
    lock.remove_on_drop = true;
    Ok(())
}

/// Removes what a stopped run leaves besides its lock: the evidence and the control socket.
fn remove_runtime_artifacts(
    server_root: &Path,
    evidence: Option<&RuntimeEvidence>,
) -> ServerResult<()> {
    let mut paths = vec![
        runtime_path(server_root),
        server_root
            .join(".launcher")
            .join(LEGACY_CONTROL_SOCKET_FILE),
    ];
    if let Some(evidence) = evidence
        && let Ok(socket) = control_socket_path(server_root, evidence)
    {
        paths.push(socket);
    }
    for path in paths {
        if let Err(source) = std::fs::remove_file(&path)
            && source.kind() != std::io::ErrorKind::NotFound
        {
            return Err(ServerError::RuntimeState { path, source });
        }
    }
    if let Some(directory) = evidence.and_then(|evidence| evidence.control_directory.as_ref()) {
        remove_per_run_control_directory(directory);
    }
    Ok(())
}

pub fn write_runtime_evidence(server_root: &Path, evidence: &RuntimeEvidence) -> ServerResult<()> {
    let path = runtime_path(server_root);
    let parent = path.parent().unwrap_or(server_root);
    std::fs::create_dir_all(parent).map_err(|source| ServerError::RuntimeState {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = path.with_extension("json.tmp");
    let body = serde_json::to_vec(evidence).map_err(|error| ServerError::RuntimeState {
        path: temporary.clone(),
        source: std::io::Error::other(error),
    })?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| ServerError::RuntimeState {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(&body)
        .and_then(|()| file.sync_all())
        .map_err(|source| ServerError::RuntimeState {
            path: temporary.clone(),
            source,
        })?;
    std::fs::rename(&temporary, &path).map_err(|source| ServerError::RuntimeState {
        path: path.clone(),
        source,
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ServerError::RuntimeState {
            path: parent.to_path_buf(),
            source,
        })
}

pub fn read_runtime_evidence(server_root: &Path) -> ServerResult<Option<RuntimeEvidence>> {
    let path = runtime_path(server_root);
    let body = match std::fs::read(&path) {
        Ok(body) => body,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ServerError::RuntimeState { path, source }),
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|error| ServerError::RuntimeState {
            path,
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })
}

#[must_use]
pub(crate) fn runtime_lock_path(server_root: &Path) -> PathBuf {
    server_root.join(".launcher").join(RUNTIME_LOCK_FILE)
}

#[must_use]
pub fn runtime_path(server_root: &Path) -> PathBuf {
    server_root.join(".launcher").join(RUNTIME_FILE)
}

/// The control socket a run recorded in `evidence`, in a directory that is
/// still private to this account.
///
/// Evidence without a recorded directory predates per-run directory selection
/// and names the shared per-user `/tmp` directory.
pub fn control_socket_path(
    server_root: &Path,
    evidence: &RuntimeEvidence,
) -> ServerResult<PathBuf> {
    let directory = recorded_control_directory(evidence)?;
    validate_control_directory(&directory)?;
    control_socket_in(&directory, server_root, evidence.generation)
}

fn recorded_control_directory(evidence: &RuntimeEvidence) -> ServerResult<PathBuf> {
    match &evidence.control_directory {
        Some(directory) => Ok(directory.clone()),
        None => Ok(shared_control_directory(Path::new("/tmp"), current_uid()?)),
    }
}

/// Picks and prepares the directory for a new run's control socket.
pub fn prepare_control_socket_path(server_root: &Path, generation: Uuid) -> ServerResult<PathBuf> {
    ControlDirectories::from_environment().prepare(server_root, generation)
}

pub fn active_control_socket_path(server_root: &Path) -> ServerResult<Option<PathBuf>> {
    let Some(evidence) = read_runtime_evidence(server_root)? else {
        return Ok(None);
    };
    control_socket_path(server_root, &evidence).map(Some)
}

/// The file name of a run's control socket, which is keyed by the server
/// root's filesystem identity and the run's generation.
pub(crate) fn control_socket_file_name(
    server_root: &Path,
    generation: Uuid,
) -> ServerResult<String> {
    let root = std::fs::metadata(server_root).map_err(|source| ServerError::RuntimeState {
        path: server_root.to_path_buf(),
        source,
    })?;
    Ok(format!(
        "s-{:x}-{:x}-{}.sock",
        root.dev(),
        root.ino(),
        generation.simple()
    ))
}

fn control_socket_in(
    directory: &Path,
    server_root: &Path,
    generation: Uuid,
) -> ServerResult<PathBuf> {
    let path = directory.join(control_socket_file_name(server_root, generation)?);
    if path.as_os_str().as_bytes().len() > MAX_UNIX_SOCKET_PATH_BYTES {
        return Err(ServerError::RuntimeState {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "derived server control socket exceeds the Unix path limit",
            ),
        });
    }
    Ok(path)
}

/// The places a run's control socket may live, best first.
///
/// `$XDG_RUNTIME_DIR` is private to the account by definition. The shared
/// `evx-<uid>` directory in the temporary directory has a predictable name in a
/// directory everybody can write to, so another account can occupy it. A run
/// that finds it occupied uses a directory of its own with an unpredictable
/// name instead, so that occupying the name cannot stop any server from starting.
#[derive(Debug, Clone)]
pub(crate) struct ControlDirectories {
    runtime: Option<PathBuf>,
    temporary: PathBuf,
}

impl ControlDirectories {
    pub(crate) fn from_environment() -> Self {
        Self {
            runtime: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            temporary: PathBuf::from("/tmp"),
        }
    }

    pub(crate) fn prepare(&self, server_root: &Path, generation: Uuid) -> ServerResult<PathBuf> {
        let uid = current_uid()?;
        let fits = |directory: &Path| control_socket_in(directory, server_root, generation).is_ok();

        if let Some(runtime) = &self.runtime
            && runtime.is_absolute()
            && is_private_directory(runtime, uid)
            && fits(runtime)
        {
            return control_socket_in(runtime, server_root, generation);
        }
        let shared = shared_control_directory(&self.temporary, uid);
        if secure_shared_control_directory(&shared, uid) && fits(&shared) {
            return control_socket_in(&shared, server_root, generation);
        }
        let own = self
            .temporary
            .join(format!("evx-{uid}-{:08x}", generation.as_fields().0));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&own)
            .map_err(|source| ServerError::RuntimeState {
                path: own.clone(),
                source,
            })?;
        validate_control_directory(&own)?;
        control_socket_in(&own, server_root, generation)
    }
}

fn shared_control_directory(temporary: &Path, uid: u32) -> PathBuf {
    temporary.join(format!("evx-{uid}"))
}

/// Creates the shared directory, or tightens it when it is already ours.
/// Anything else occupying the name, or losing a creation race, is refused
/// rather than followed or reused.
fn secure_shared_control_directory(directory: &Path, uid: u32) -> bool {
    let secured = match std::fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_dir() && metadata.uid() == uid => {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).is_ok()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .is_ok(),
        Ok(_) | Err(_) => false,
    };
    secured && is_private_directory(directory, uid)
}

/// Removes the directory a single run created for itself once it is empty.
/// The shared per-user directory and a runtime directory are left alone.
pub(crate) fn remove_per_run_control_directory(directory: &Path) {
    let Ok(uid) = current_uid() else {
        return;
    };
    let own = directory
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&format!("evx-{uid}-")));
    if own {
        let _ = std::fs::remove_dir(directory);
    }
}

fn current_uid() -> ServerResult<u32> {
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(|source| ServerError::RuntimeState {
            path: PathBuf::from("/proc/self"),
            source,
        })
}

/// Whether `directory` is a real directory of ours that nobody else can enter.
fn is_private_directory(directory: &Path, uid: u32) -> bool {
    std::fs::symlink_metadata(directory).is_ok_and(|metadata| {
        let others_have_access = metadata.mode() & 0o077 != 0;
        metadata.file_type().is_dir() && metadata.uid() == uid && !others_have_access
    })
}

fn validate_control_directory(directory: &Path) -> ServerResult<()> {
    if directory.is_absolute() && is_private_directory(directory, current_uid()?) {
        return Ok(());
    }
    Err(ServerError::RuntimeState {
        path: directory.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "control socket directory must be an absolute directory owned by the current user with mode 0700",
        ),
    })
}

fn capture_process(pid: u32, role: ProcessRole) -> ServerResult<ProcessEvidence> {
    let state_path = PathBuf::from(format!("/proc/{pid}"));
    let boot_id = read_boot_id().map_err(|source| ServerError::RuntimeState {
        path: PathBuf::from("/proc/sys/kernel/random/boot_id"),
        source,
    })?;
    let start_ticks = read_start_ticks(pid).map_err(|source| ServerError::RuntimeState {
        path: state_path.join("stat"),
        source,
    })?;
    let executable =
        std::fs::read_link(state_path.join("exe")).map_err(|source| ServerError::RuntimeState {
            path: state_path.join("exe"),
            source,
        })?;
    // The process image, which still resolves after the file is replaced.
    let metadata = std::fs::metadata(state_path.join("exe")).ok();
    Ok(ProcessEvidence {
        role,
        pid,
        boot_id,
        start_ticks,
        executable,
        executable_device: metadata.as_ref().map(std::fs::Metadata::dev),
        executable_inode: metadata.as_ref().map(std::fs::Metadata::ino),
        started_at: Utc::now(),
    })
}

/// Captures a just-spawned Java child's full identity.
pub fn capture_java_child(pid: u32) -> ServerResult<ProcessEvidence> {
    capture_process(pid, ProcessRole::JavaChild)
}

/// Boot, PID, and start ticks identify the process. The recorded executable
/// is not compared: an upgrade that replaces the file, or an `exec` after the
/// evidence was captured, leaves the same process alive.
fn verify_process(evidence: &ProcessEvidence) -> IdentityState {
    match verify_process_instance(evidence) {
        IdentityState::VerifiedAlive => {}
        other => return other,
    }
    // An exited process that has not been reaped yet has no executable.
    match std::fs::read_link(format!("/proc/{}/exe", evidence.pid)) {
        Ok(_) => IdentityState::VerifiedAlive,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => IdentityState::ProvenDead,
        Err(_) => IdentityState::Unverifiable,
    }
}

/// Whether the process was recorded since the machine last started. An
/// unreadable boot ID counts as the current boot.
fn recorded_in_current_boot(evidence: &ProcessEvidence) -> bool {
    read_boot_id().map_or(true, |current| current == evidence.boot_id)
}

/// Whether the pid still names the process that was recorded, by boot and
/// start time, without looking at what it executes.
fn verify_process_instance(evidence: &ProcessEvidence) -> IdentityState {
    let Ok(current_boot) = read_boot_id() else {
        return IdentityState::Unverifiable;
    };
    if current_boot != evidence.boot_id {
        return IdentityState::ProvenDead;
    }
    match read_start_ticks(evidence.pid) {
        Ok(ticks) if ticks == evidence.start_ticks => IdentityState::VerifiedAlive,
        Ok(_) => IdentityState::ProvenDead,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => IdentityState::ProvenDead,
        Err(_) => IdentityState::Unverifiable,
    }
}

/// Whether the recorded Java child itself holds every listening TCP socket
/// that a connection to `target` can reach, since an accepted connection does
/// not say who accepted it.
///
/// `Ok(false)` means not yet: no child is recorded or provably alive, it has
/// not bound, or another program also listens where the connection would go.
/// Exit is left to the caller's runtime monitoring.
pub(crate) fn runtime_child_listens_on(
    server_root: &Path,
    target: SocketAddr,
) -> ServerResult<bool> {
    let Some(RuntimeEvidence {
        child: Some(child), ..
    }) = read_runtime_evidence(server_root)?
    else {
        return Ok(false);
    };
    // The executable is not compared: a Java binary an upgrade replaced on
    // disk still runs as the same server.
    let is_child = || {
        matches!(
            verify_process_instance(&child),
            IdentityState::VerifiedAlive
        )
    };
    if !is_child() {
        return Ok(false);
    }
    // The launcher's own tables: a socket the child holds in another network
    // namespace is not the one a local connection reaches. Unreadable tables
    // fail closed rather than falling back to an unattributed connection.
    let mut reachable = std::collections::HashSet::new();
    for table in ["/proc/self/net/tcp", "/proc/self/net/tcp6"] {
        let rows = match std::fs::read_to_string(table) {
            Ok(rows) => rows,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(ServerError::RuntimeState {
                    path: PathBuf::from(table),
                    source,
                });
            }
        };
        reachable.extend(
            rows.lines()
                .skip(1)
                .filter_map(|row| listening_inode(row, target)),
        );
    }
    if reachable.is_empty() {
        return Ok(false);
    }
    let descriptors = PathBuf::from(format!("/proc/{}/fd", child.pid));
    let entries = match std::fs::read_dir(&descriptors) {
        Ok(entries) => entries,
        // A child that went away is reported by exit monitoring, not here.
        Err(_) if !is_child() => return Ok(false),
        // A live child whose sockets cannot be attributed would only ever time out.
        Err(source) => {
            return Err(ServerError::RuntimeState {
                path: descriptors,
                source,
            });
        }
    };
    for entry in entries {
        // A descriptor can close between listing and reading it.
        let Ok(link) = entry.and_then(|entry| std::fs::read_link(entry.path())) else {
            continue;
        };
        if let Some(inode) = socket_inode(&link) {
            reachable.remove(&inode);
        }
    }
    // A co-listener, such as a separate IPv4 socket beside an IPv6-only wildcard
    // or a shared reuse port, could take the connection instead. Checked again
    // so a pid recycled during the scan cannot lend its descriptors.
    Ok(reachable.is_empty() && is_child())
}

/// The socket inode of a `/proc/net/tcp` or `tcp6` row that listens where a
/// connection to `target` arrives.
fn listening_inode(row: &str, target: SocketAddr) -> Option<u64> {
    const LISTEN: &str = "0A";
    let mut fields = row.split_whitespace();
    let (address, port) = fields.nth(1)?.rsplit_once(':')?;
    let state = fields.nth(1)?;
    let inode = fields.nth(5)?;
    if state != LISTEN || u16::from_str_radix(port, 16).ok()? != target.port() {
        return None;
    }
    if !accepts_for(table_address(address)?, target.ip()) {
        return None;
    }
    inode.parse().ok()
}

/// A `/proc/net` address: 32-bit words in host byte order, as the kernel prints them.
fn table_address(hex: &str) -> Option<IpAddr> {
    let mut octets = [0_u8; 16];
    let words = hex.len() / 8;
    if !matches!(words, 1 | 4) || hex.len() % 8 != 0 {
        return None;
    }
    for word in 0..words {
        let value = u32::from_str_radix(hex.get(word * 8..word * 8 + 8)?, 16).ok()?;
        octets[word * 4..word * 4 + 4].copy_from_slice(&value.to_ne_bytes());
    }
    Some(if words == 1 {
        IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]))
    } else {
        IpAddr::V6(Ipv6Addr::from(octets))
    })
}

/// Whether a listener bound to `bound` receives connections made to `target`.
fn accepts_for(bound: IpAddr, target: IpAddr) -> bool {
    // An IPv4-mapped address reaches the same listeners as the IPv4 address.
    match (bound.to_canonical(), target.to_canonical()) {
        // A dual-stack wildcard also takes IPv4 connections.
        (IpAddr::V6(bound), _) if bound.is_unspecified() => true,
        (IpAddr::V4(bound), IpAddr::V4(target)) => bound.is_unspecified() || bound == target,
        (IpAddr::V6(bound), IpAddr::V6(target)) => bound == target,
        _ => false,
    }
}

fn socket_inode(link: &Path) -> Option<u64> {
    link.to_str()?
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

fn read_boot_id() -> std::io::Result<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|value| value.trim().to_owned())
}

fn read_start_ticks(pid: u32) -> std::io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed stat"))?;
    tail.split_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing start time"))?
        .parse()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn control_socket_responds(server_root: &Path, evidence: &RuntimeEvidence) -> bool {
    let Ok(path) = control_socket_path(server_root, evidence) else {
        return false;
    };
    UnixStream::connect(path).is_ok_and(|stream| {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
        true
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::os::unix::net::UnixListener;

    use super::*;

    fn private_directory(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("directory");
        path
    }

    fn directories(runtime: Option<PathBuf>, temporary: &Path) -> ControlDirectories {
        ControlDirectories {
            runtime,
            temporary: temporary.to_path_buf(),
        }
    }

    fn evidence_in(directory: Option<PathBuf>) -> RuntimeEvidence {
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("evidence");
        evidence.control_directory = directory;
        evidence
    }

    #[test]
    fn a_private_runtime_directory_holds_the_control_socket() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let runtime = private_directory(base.path(), "runtime");
        let temporary = private_directory(base.path(), "tmp");

        let path = directories(Some(runtime.clone()), &temporary)
            .prepare(root.path(), Uuid::new_v4())
            .expect("path");

        assert_eq!(path.parent(), Some(runtime.as_path()));
        assert!(std::fs::read_dir(&temporary).expect("tmp").next().is_none());
    }

    #[test]
    fn a_runtime_directory_others_can_enter_is_not_used() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let runtime = base.path().join("runtime");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&runtime)
            .expect("runtime");
        let temporary = private_directory(base.path(), "tmp");

        let path = directories(Some(runtime), &temporary)
            .prepare(root.path(), Uuid::new_v4())
            .expect("path");

        assert_eq!(
            path.parent().and_then(Path::parent),
            Some(temporary.as_path())
        );
    }

    #[test]
    fn a_relative_runtime_directory_is_not_used() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let temporary = private_directory(base.path(), "tmp");

        let path = directories(Some(PathBuf::from("runtime")), &temporary)
            .prepare(root.path(), Uuid::new_v4())
            .expect("path");

        assert!(path.starts_with(&temporary));
    }

    #[test]
    fn the_shared_directory_is_created_private_and_reused_when_it_is_ours() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let uid = current_uid().expect("uid");
        let shared = base.path().join(format!("evx-{uid}"));

        let first = directories(None, base.path())
            .prepare(root.path(), Uuid::new_v4())
            .expect("first");
        assert_eq!(first.parent(), Some(shared.as_path()));
        assert_eq!(
            std::fs::metadata(&shared).expect("shared").mode() & 0o777,
            0o700
        );

        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).expect("loosen");
        let second = directories(None, base.path())
            .prepare(root.path(), Uuid::new_v4())
            .expect("second");
        assert_eq!(second.parent(), Some(shared.as_path()));
        assert_eq!(
            std::fs::metadata(&shared).expect("shared").mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn a_symlink_in_the_shared_directory_name_is_never_followed() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let attacker = tempfile::tempdir().expect("attacker");
        let uid = current_uid().expect("uid");
        std::os::unix::fs::symlink(attacker.path(), base.path().join(format!("evx-{uid}")))
            .expect("symlink");

        let generation = Uuid::new_v4();
        let path = directories(None, base.path())
            .prepare(root.path(), generation)
            .expect("a claimed name does not stop the run");

        let directory = path.parent().expect("directory");
        assert_eq!(
            directory,
            base.path()
                .join(format!("evx-{uid}-{:08x}", generation.as_fields().0))
        );
        assert!(is_private_directory(directory, uid));
        assert!(
            std::fs::read_dir(attacker.path())
                .expect("attacker")
                .next()
                .is_none()
        );
    }

    #[test]
    fn a_file_in_the_shared_directory_name_does_not_stop_the_run() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let uid = current_uid().expect("uid");
        std::fs::write(base.path().join(format!("evx-{uid}")), b"").expect("file");

        let path = directories(None, base.path())
            .prepare(root.path(), Uuid::new_v4())
            .expect("path");

        assert!(is_private_directory(path.parent().expect("directory"), uid));
    }

    #[test]
    fn a_runtime_directory_too_long_for_a_socket_falls_back() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let long = private_directory(base.path(), &"r".repeat(120));
        let temporary = private_directory(base.path(), "tmp");

        let path = directories(Some(long), &temporary)
            .prepare(root.path(), Uuid::new_v4())
            .expect("path");

        assert!(path.starts_with(&temporary));
        assert!(path.as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES);
    }

    #[test]
    fn a_later_process_finds_the_socket_in_the_directory_the_run_recorded() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let uid = current_uid().expect("uid");
        std::fs::write(base.path().join(format!("evx-{uid}")), b"").expect("claimed");
        let mut evidence = evidence_in(None);
        let prepared = directories(None, base.path())
            .prepare(root.path(), evidence.generation)
            .expect("path");
        evidence.control_directory = prepared.parent().map(Path::to_path_buf);

        // The consumer's environment is not consulted.
        let found = control_socket_path(root.path(), &evidence).expect("found");

        assert_eq!(found, prepared);
    }

    #[test]
    fn a_recorded_directory_that_is_not_private_is_refused() {
        let root = tempfile::tempdir().expect("root");
        let base = tempfile::tempdir().expect("base");
        let open = base.path().join("open");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&open)
            .expect("open");
        let target = private_directory(base.path(), "target");
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        for directory in [open, link, PathBuf::from("relative")] {
            assert!(
                control_socket_path(root.path(), &evidence_in(Some(directory.clone()))).is_err(),
                "{} must not be used",
                directory.display()
            );
        }
    }

    #[test]
    fn evidence_without_a_recorded_directory_names_the_shared_directory() {
        let uid = current_uid().expect("uid");
        let mut body = serde_json::to_value(evidence_in(None)).expect("json");
        assert!(body.get("control_directory").is_none());
        body["generation"] = serde_json::json!(Uuid::nil());
        let legacy: RuntimeEvidence = serde_json::from_value(body).expect("legacy evidence");
        assert_eq!(legacy.control_directory, None);

        let directory = recorded_control_directory(&legacy).expect("directory");
        assert_eq!(directory, PathBuf::from(format!("/tmp/evx-{uid}")));
    }

    #[test]
    fn only_a_directory_a_run_made_for_itself_is_removed_after_the_run() {
        let base = tempfile::tempdir().expect("base");
        let uid = current_uid().expect("uid");
        let own = private_directory(base.path(), &format!("evx-{uid}-0badcafe"));
        let shared = private_directory(base.path(), &format!("evx-{uid}"));
        let runtime = private_directory(base.path(), "runtime");

        for directory in [&own, &shared, &runtime] {
            remove_per_run_control_directory(directory);
        }

        assert!(!own.exists());
        assert!(shared.exists());
        assert!(runtime.exists());
    }

    #[test]
    fn listening_rows_are_matched_by_address_port_and_state() {
        let target: SocketAddr = "127.0.0.1:25565".parse().expect("target");
        let row = |address: &str, state: &str| {
            format!(
                "   0: {address}:63DD 00000000:0000 {state} 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0"
            )
        };
        let loopback = hex_v4(Ipv4Addr::LOCALHOST);
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";
        assert_eq!(listening_inode(&row(&loopback, "0A"), target), Some(4242));
        assert_eq!(
            listening_inode(&row(&hex_v4(Ipv4Addr::UNSPECIFIED), "0A"), target),
            Some(4242)
        );
        assert_eq!(
            listening_inode(&row(&hex_v6(Ipv6Addr::UNSPECIFIED), "0A"), target),
            Some(4242)
        );
        assert_eq!(
            listening_inode(
                &row(&hex_v6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()), "0A"),
                target
            ),
            Some(4242)
        );
        assert_eq!(listening_inode(&row(&loopback, "01"), target), None);
        assert_eq!(
            listening_inode(&row(&hex_v4(Ipv4Addr::new(127, 0, 0, 2)), "0A"), target),
            None
        );
        assert_eq!(
            listening_inode(&row(&hex_v6(Ipv6Addr::LOCALHOST), "0A"), target),
            None
        );
        assert_eq!(
            listening_inode(
                &row(&loopback, "0A"),
                "127.0.0.1:25566".parse().expect("target")
            ),
            None
        );
        assert_eq!(listening_inode(header, target), None);
        assert_eq!(
            listening_inode(
                &row(&hex_v6(Ipv4Addr::UNSPECIFIED.to_ipv6_mapped()), "0A"),
                target
            ),
            Some(4242)
        );
        assert_eq!(
            listening_inode(
                &row(&loopback, "0A"),
                SocketAddr::new(IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()), 25_565)
            ),
            Some(4242)
        );
        assert_eq!(socket_inode(Path::new("socket:[4242]")), Some(4242));
        assert_eq!(socket_inode(Path::new("pipe:[4242]")), None);
    }

    fn hex_v4(address: Ipv4Addr) -> String {
        format!("{:08X}", u32::from_ne_bytes(address.octets()))
    }

    fn hex_v6(address: Ipv6Addr) -> String {
        use std::fmt::Write as _;
        address
            .octets()
            .chunks(4)
            .fold(String::new(), |mut hex, word| {
                let word = u32::from_ne_bytes(word.try_into().expect("word"));
                write!(hex, "{word:08X}").expect("write to string");
                hex
            })
    }

    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher dir");
        root
    }

    /// A supervisor holds its lock before it writes evidence and after it
    /// clears it, and the lock names it.
    #[test]
    fn a_held_runtime_lock_is_running_until_its_holder_releases_it() {
        let root = root();
        let lock = RuntimeLock::acquire(root.path()).expect("lock");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { child: None, .. }
        ));
        assert!(matches!(
            RuntimeLock::acquire(root.path()),
            Err(ServerError::AlreadyRunning)
        ));
        assert!(matches!(
            clear_stale_runtime(root.path()),
            Err(ServerError::RuntimeMayBeAlive)
        ));
        assert!(
            runtime_lock_path(root.path()).exists(),
            "a refused clear leaves the holder's lock"
        );

        drop(lock);
        assert!(!runtime_lock_path(root.path()).exists());
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::Stopped
        ));
    }

    #[test]
    fn a_leftover_runtime_lock_no_process_holds_does_not_block_a_start() {
        for leftover in [&b""[..], b"not a holder", b"{\"pid\":1}"] {
            let root = root();
            let path = runtime_lock_path(root.path());
            std::fs::write(&path, leftover).expect("leftover");
            assert!(matches!(
                classify_runtime(root.path()).expect("classify"),
                RuntimeClassification::Stopped
            ));

            let lock = RuntimeLock::acquire(root.path()).expect("the leftover is taken over");
            let body = std::fs::read(&path).expect("read");
            assert!(
                serde_json::from_slice::<ProcessEvidence>(&body)
                    .is_ok_and(|holder| holder.pid == std::process::id()),
                "the new holder is recorded"
            );
            drop(lock);

            std::fs::write(&path, leftover).expect("leftover");
            clear_stale_runtime(root.path()).expect("recovery clears it");
            assert!(!path.exists());
        }
    }

    #[test]
    fn a_runtime_lock_naming_a_dead_holder_is_stale_and_cleared() {
        let root = root();
        let lock = runtime_lock_path(root.path());
        let mut holder = capture_process(std::process::id(), ProcessRole::Supervisor).expect("me");
        holder.start_ticks = 0;
        std::fs::write(&lock, serde_json::to_vec(&holder).expect("holder")).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::ProvenStale { .. }
        ));
        clear_stale_runtime(root.path()).expect("clear");
        assert!(!lock.exists());
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::Stopped
        ));
    }

    /// A lock written before locks were flocked holds nothing, so only its
    /// holder says whether a supervisor still owns it.
    #[test]
    fn a_runtime_lock_naming_a_live_holder_is_never_taken_over() {
        let root = root();
        let lock = runtime_lock_path(root.path());
        let holder = capture_process(std::process::id(), ProcessRole::Supervisor).expect("me");
        std::fs::write(&lock, serde_json::to_vec(&holder).expect("holder")).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { .. }
        ));
        assert!(matches!(
            RuntimeLock::acquire(root.path()),
            Err(ServerError::AlreadyRunning)
        ));
        assert!(matches!(
            clear_stale_runtime(root.path()),
            Err(ServerError::RuntimeMayBeAlive)
        ));
        assert!(lock.exists());
    }

    #[test]
    fn starts_racing_for_a_leftover_runtime_lock_yield_one_owner() {
        const STARTS: usize = 8;
        let root = root();
        let mut holder = capture_process(std::process::id(), ProcessRole::Supervisor).expect("me");
        holder.start_ticks = 0;
        std::fs::write(
            runtime_lock_path(root.path()),
            serde_json::to_vec(&holder).expect("holder"),
        )
        .expect("leftover");
        let barrier = std::sync::Barrier::new(STARTS);

        let outcomes: Vec<_> = std::thread::scope(|scope| {
            let starts: Vec<_> = (0..STARTS)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        RuntimeLock::acquire(root.path())
                    })
                })
                .collect();
            starts
                .into_iter()
                .map(|start| start.join().expect("join"))
                .collect()
        });

        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert!(
            outcomes
                .iter()
                .filter_map(|outcome| outcome.as_ref().err())
                .all(|error| matches!(error, ServerError::AlreadyRunning))
        );
    }

    /// Above the kernel's pid limit, so no process leads a session with this id.
    const NO_SUCH_SESSION: u32 = u32::MAX;

    /// A pending start whose supervisor is already gone and whose session holds no process, so a
    /// newly exec'd process elsewhere on the machine cannot make the scan unverifiable.
    fn write_dead_pending_start(root: &Path) -> RuntimeEvidence {
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        evidence.supervisor.start_ticks = 0;
        evidence.supervisor_session = Some(NO_SUCH_SESSION);
        evidence.child_pending = true;
        write_runtime_evidence(root, &evidence).expect("write");
        evidence
    }

    /// Spawns a tagged process and returns once its environment shows the tag, which can lag the
    /// spawn while the new program image is still being installed.
    fn spawn_tagged(run: Uuid) -> KillOnDrop {
        let child = KillOnDrop(
            std::process::Command::new("/bin/sleep")
                .arg("60")
                .env(SERVER_RUN_VARIABLE, run.to_string())
                .spawn()
                .expect("spawn"),
        );
        let environment = format!("/proc/{}/environ", child.0.id());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !std::fs::read(&environment).is_ok_and(|bytes| environment_carries_run(&bytes, run)) {
            assert!(
                std::time::Instant::now() < deadline,
                "the run tag never became visible in the child's environment"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        child
    }

    #[test]
    fn a_child_started_before_its_supervisor_died_is_found_by_the_run_tag() {
        let root = root();
        let evidence = write_dead_pending_start(root.path());
        let decoy = spawn_tagged(Uuid::new_v4());
        let mut child = spawn_tagged(evidence.generation);

        let RuntimeClassification::Orphaned { child: found } =
            classify_runtime(root.path()).expect("classify")
        else {
            panic!("a tagged child is an orphan");
        };
        assert_eq!(found.pid, child.0.id());
        assert!(matches!(
            RuntimeLock::acquire(root.path()),
            Err(ServerError::AlreadyRunning)
        ));
        assert!(matches!(
            clear_stale_runtime(root.path()),
            Err(ServerError::RuntimeMayBeAlive)
        ));
        assert!(runtime_path(root.path()).exists());

        child.0.kill().expect("kill");
        child.0.wait().expect("reap");
        assert!(
            matches!(
                classify_runtime(root.path()).expect("classify"),
                RuntimeClassification::ProvenStale { .. }
            ),
            "another run's child does not keep this run alive"
        );
        drop(decoy);
        drop(RuntimeLock::acquire(root.path()).expect("a proven stale run is taken over"));
    }

    #[test]
    fn a_pending_start_that_left_no_process_behind_can_be_cleared_without_editing_files() {
        let root = root();
        write_dead_pending_start(root.path());
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::ProvenStale { .. }
        ));
        clear_stale_runtime(root.path()).expect("recovery clears it");
        assert!(!runtime_path(root.path()).exists());
    }

    #[test]
    fn a_process_that_started_before_the_supervisor_is_not_its_pending_child() {
        let root = root();
        let mut evidence = write_dead_pending_start(root.path());
        let early = spawn_tagged(evidence.generation);
        evidence.supervisor.start_ticks = u64::MAX;
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::ProvenStale { .. }
        ));
        drop(early);
    }

    /// A tagged process that leads a session of its own and whose environment this user cannot read.
    fn spawn_unreadable_leader(directory: &Path, run: Uuid) -> KillOnDrop {
        let program = directory.join("sleep");
        std::fs::copy("/bin/sleep", &program).expect("copy sleep");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o111))
            .expect("execute only");
        // A concurrent fork can briefly hold the copy's write descriptor, which blocks an exec.
        for _ in 0..100 {
            match std::process::Command::new(&program).arg("0").status() {
                Ok(_) => break,
                Err(error) if error.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("probe: {error}"),
            }
        }
        // Not a group leader, so setsid takes over the process itself rather than forking.
        KillOnDrop(
            std::process::Command::new("setsid")
                .arg(&program)
                .arg("60")
                .env(SERVER_RUN_VARIABLE, run.to_string())
                .spawn()
                .expect("spawn"),
        )
    }

    #[test]
    fn a_group_leader_in_the_supervisors_session_with_an_unreadable_environment_keeps_the_start_unresolved()
     {
        let root = root();
        let bin = tempfile::tempdir().expect("bin dir");
        let mut evidence = write_dead_pending_start(root.path());
        let leader = spawn_unreadable_leader(bin.path(), evidence.generation);
        let session = leader.0.id();
        let environment = format!("/proc/{session}/environ");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(format!("/proc/{session}/stat"))
            .is_ok_and(|stat| stat.contains("(setsid)"))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        match std::fs::read(&environment) {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied),
            // A process that reads an execute-only program anyway, such as root, proves nothing here.
            Ok(_) => return,
        }

        evidence.supervisor_session = Some(session);
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { child: None, .. }
        ));
        assert!(matches!(
            clear_stale_runtime(root.path()),
            Err(ServerError::RuntimeMayBeAlive)
        ));

        evidence.supervisor_session = None;
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { .. }
        ));

        evidence.supervisor_session = Some(NO_SUCH_SESSION);
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(
            matches!(
                classify_runtime(root.path()).expect("classify"),
                RuntimeClassification::ProvenStale { .. }
            ),
            "a process in another session did not come from this supervisor"
        );
    }

    #[test]
    fn a_run_tag_is_matched_as_a_whole_environment_entry() {
        let run = Uuid::new_v4();
        let entry = format!("{SERVER_RUN_VARIABLE}={run}");
        let other = format!("{SERVER_RUN_VARIABLE}={}", Uuid::new_v4());
        assert!(environment_carries_run(
            format!("PATH=/bin\0{entry}\0HOME=/x\0").as_bytes(),
            run
        ));
        assert!(!environment_carries_run(
            format!("PATH=/bin\0{other}\0").as_bytes(),
            run
        ));
        assert!(!environment_carries_run(
            format!("X{entry}\0").as_bytes(),
            run
        ));
        assert!(!environment_carries_run(
            format!("{entry}x\0").as_bytes(),
            run
        ));
        assert!(!environment_carries_run(b"", run));
    }

    #[test]
    fn a_spawn_in_progress_before_a_restart_of_the_machine_is_proven_stale() {
        let root = root();
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        evidence.supervisor.boot_id = "an earlier boot".to_owned();
        evidence.child_pending = true;
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::ProvenStale { .. }
        ));
        clear_stale_runtime(root.path()).expect("recovery clears it");
        assert!(!runtime_path(root.path()).exists());
    }

    #[test]
    fn a_verified_supervisor_requires_a_reachable_control_socket_to_be_controlled() {
        let root = root();
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        let socket = prepare_control_socket_path(root.path(), evidence.generation).expect("path");
        evidence.control_directory = socket.parent().map(Path::to_path_buf);
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { .. }
        ));

        let listener = UnixListener::bind(&socket).expect("bind");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::Controlled { .. }
        ));
        drop(listener);
        std::fs::remove_file(socket).expect("remove socket");
    }

    #[test]
    fn pid_reuse_evidence_is_proven_stale_and_can_be_cleared() {
        let root = root();
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        evidence.supervisor.start_ticks = 0;
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::ProvenStale { .. }
        ));
        clear_stale_runtime(root.path()).expect("clear");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::Stopped
        ));
    }

    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Records `child` under a supervisor that is already gone.
    fn record_orphan(root: &Path, child: &KillOnDrop) {
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        evidence.supervisor.start_ticks = 0;
        evidence.child = Some(capture_java_child(child.0.id()).expect("child evidence"));
        write_runtime_evidence(root, &evidence).expect("write");
    }

    fn assert_orphaned_and_not_clearable(root: &Path) {
        assert!(matches!(
            classify_runtime(root).expect("classify"),
            RuntimeClassification::Orphaned { .. }
        ));
        assert!(matches!(
            clear_stale_runtime(root),
            Err(ServerError::RuntimeMayBeAlive)
        ));
        assert!(runtime_path(root).exists());
    }

    #[test]
    fn a_live_orphan_whose_executable_was_replaced_is_not_proven_dead() {
        let root = root();
        let bin = tempfile::tempdir().expect("bin dir");
        let java = bin.path().join("java");
        std::fs::copy("/bin/sleep", &java).expect("copy sleep");
        let child = KillOnDrop(
            std::process::Command::new(&java)
                .arg("60")
                .spawn()
                .expect("spawn"),
        );
        record_orphan(root.path(), &child);
        assert_orphaned_and_not_clearable(root.path());

        let replacement = bin.path().join("java.new");
        std::fs::copy("/bin/sleep", &replacement).expect("copy replacement");
        std::fs::rename(&replacement, &java).expect("replace by rename");
        assert_orphaned_and_not_clearable(root.path());
    }

    #[test]
    fn a_live_orphan_that_executed_another_program_is_not_proven_dead() {
        let root = root();
        let child = KillOnDrop(
            std::process::Command::new("sh")
                .args(["-c", "sleep 0.3; exec sleep 60"])
                .spawn()
                .expect("spawn"),
        );
        record_orphan(root.path(), &child);
        assert_orphaned_and_not_clearable(root.path());

        std::thread::sleep(Duration::from_millis(800));
        assert_orphaned_and_not_clearable(root.path());
    }

    #[test]
    fn a_supervisor_whose_executable_was_replaced_stays_controlled() {
        let root = root();
        let bin = tempfile::tempdir().expect("bin dir");
        let launcher = bin.path().join("launcher");
        std::fs::copy("/bin/sleep", &launcher).expect("copy sleep");
        let supervisor = KillOnDrop(
            std::process::Command::new(&launcher)
                .arg("60")
                .spawn()
                .expect("spawn"),
        );
        let mut evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        evidence.supervisor = capture_process(supervisor.0.id(), ProcessRole::Supervisor)
            .expect("supervisor evidence");
        let socket = prepare_control_socket_path(root.path(), evidence.generation).expect("path");
        evidence.control_directory = socket.parent().map(Path::to_path_buf);
        write_runtime_evidence(root.path(), &evidence).expect("write");
        let listener = UnixListener::bind(&socket).expect("bind");

        let replacement = bin.path().join("launcher.new");
        std::fs::copy("/bin/sleep", &replacement).expect("copy replacement");
        std::fs::rename(&replacement, &launcher).expect("replace by rename");

        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::Controlled { .. }
        ));
        drop(listener);
        std::fs::remove_file(socket).expect("remove socket");
    }

    #[test]
    fn runtime_evidence_is_stored_compactly_and_a_pretty_file_from_an_older_build_still_loads() {
        let root = root();
        let evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        write_runtime_evidence(root.path(), &evidence).expect("write");

        let path = runtime_path(root.path());
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            !text.contains('\n'),
            "runtime evidence carries pretty-printing whitespace: {text}"
        );
        assert_eq!(
            read_runtime_evidence(root.path()).expect("read"),
            Some(evidence.clone())
        );

        let stored: serde_json::Value = serde_json::from_str(&text).expect("parse");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&stored).expect("indent"),
        )
        .expect("rewrite as an older build would have");
        assert_eq!(
            read_runtime_evidence(root.path()).expect("read"),
            Some(evidence)
        );
    }

    #[test]
    fn a_live_recorded_process_refuses_recovery() {
        let root = root();
        let evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            clear_stale_runtime(root.path()),
            Err(ServerError::RuntimeMayBeAlive)
        ));
    }

    #[test]
    fn long_server_roots_use_a_short_control_socket_path() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().join("a".repeat(180));
        std::fs::create_dir_all(root.join(".launcher")).expect("long server root");
        let evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        let socket =
            prepare_control_socket_path(&root, evidence.generation).expect("control socket path");

        assert!(socket.as_os_str().as_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES);
        assert!(!socket.starts_with(&root));
        let listener = UnixListener::bind(&socket).expect("bind short control socket");
        drop(listener);
        std::fs::remove_file(socket).expect("remove socket");
    }
}
