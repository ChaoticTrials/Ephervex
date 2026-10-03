//! Conservative Linux process identity and orphan classification.

use std::io::Write as _;
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
}

impl RuntimeEvidence {
    pub fn for_current_supervisor() -> ServerResult<Self> {
        Ok(Self {
            generation: Uuid::new_v4(),
            supervisor: capture_process(std::process::id(), ProcessRole::Supervisor)?,
            child: None,
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
    let Some(evidence) = read_runtime_evidence(server_root)? else {
        return classify_lock_holder(server_root);
    };
    let supervisor = verify_process(&evidence.supervisor);
    let child = evidence.child.as_ref().map(verify_process);

    match (supervisor, child) {
        (IdentityState::VerifiedAlive, _) if control_socket_responds(server_root, &evidence) => {
            Ok(RuntimeClassification::Controlled {
                supervisor: evidence.supervisor,
                child: evidence.child,
            })
        }
        (IdentityState::VerifiedAlive | IdentityState::Unverifiable, _)
        | (IdentityState::ProvenDead, Some(IdentityState::Unverifiable)) => {
            Ok(RuntimeClassification::UnknownRunning {
                supervisor: evidence.supervisor,
                child: evidence.child,
            })
        }
        (IdentityState::ProvenDead, Some(IdentityState::VerifiedAlive)) => {
            let child = evidence.child.ok_or(ServerError::RuntimeMayBeAlive)?;
            Ok(RuntimeClassification::Orphaned { child })
        }
        (IdentityState::ProvenDead, None | Some(IdentityState::ProvenDead)) => {
            Ok(RuntimeClassification::ProvenStale { evidence })
        }
    }
}

/// A supervisor holds its lock before it writes evidence and after it clears it.
fn classify_lock_holder(server_root: &Path) -> ServerResult<RuntimeClassification> {
    let path = runtime_lock_path(server_root);
    let body = match std::fs::read(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RuntimeClassification::Stopped);
        }
        Err(source) => return Err(ServerError::RuntimeState { path, source }),
    };
    // A lock written before holders were recorded names nobody to check.
    let Ok(holder) = serde_json::from_slice::<ProcessEvidence>(&body) else {
        return Ok(RuntimeClassification::Stopped);
    };
    Ok(match verify_process(&holder) {
        IdentityState::ProvenDead => RuntimeClassification::ProvenStale {
            evidence: RuntimeEvidence {
                generation: Uuid::nil(),
                supervisor: holder,
                child: None,
            },
        },
        IdentityState::VerifiedAlive | IdentityState::Unverifiable => {
            RuntimeClassification::UnknownRunning {
                supervisor: holder,
                child: None,
            }
        }
    })
}

/// The prefix of the file a lock is written in before it is published.
const RUNTIME_LOCK_STAGING_PREFIX: &str = "server-runtime.lock.tmp.";
/// How old a staged lock must be to count as left behind by a dead supervisor.
const ABANDONED_STAGING_AGE: Duration = Duration::from_secs(60);

/// Creates the runtime lock with its holder already in it, so no reader finds
/// it empty. Fails with `AlreadyExists` when another lock is there.
pub(crate) fn create_runtime_lock(server_root: &Path) -> ServerResult<std::fs::File> {
    let path = runtime_lock_path(server_root);
    if let Some(directory) = path.parent() {
        remove_abandoned_staging(directory);
    }
    let holder = capture_process(std::process::id(), ProcessRole::Supervisor)?;
    let body = serde_json::to_vec(&holder).map_err(|error| ServerError::RuntimeState {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    })?;
    let temporary = path.with_file_name(format!("{RUNTIME_LOCK_STAGING_PREFIX}{}", Uuid::new_v4()));
    let staged = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;
        Ok::<_, std::io::Error>(file)
    })();
    let result = staged.and_then(|file| match std::fs::hard_link(&temporary, &path) {
        Ok(()) => Ok(file),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotFound
            ) =>
        {
            Err(error)
        }
        // A filesystem without working hard links gets the lock created in place instead.
        Err(_) => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(&body)?;
            file.sync_all()?;
            Ok(file)
        }
    });
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|source| ServerError::RuntimeState { path, source })
}

/// Removes staged locks old enough that no live supervisor is still writing them.
fn remove_abandoned_staging(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let staged = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(RUNTIME_LOCK_STAGING_PREFIX));
        let abandoned = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                modified
                    .elapsed()
                    .is_ok_and(|age| age >= ABANDONED_STAGING_AGE)
            });
        if staged && abandoned {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Clears runtime artifacts only when every recorded process is proven gone.
pub fn clear_stale_runtime(server_root: &Path) -> ServerResult<()> {
    let evidence = match classify_runtime(server_root)? {
        RuntimeClassification::Stopped => return Ok(()),
        RuntimeClassification::ProvenStale { evidence } => evidence,
        _ => return Err(ServerError::RuntimeMayBeAlive),
    };
    let mut paths = vec![
        runtime_path(server_root),
        runtime_lock_path(server_root),
        server_root
            .join(".launcher")
            .join(LEGACY_CONTROL_SOCKET_FILE),
    ];
    if validate_control_socket_directory().is_ok() {
        paths.push(control_socket_path(server_root, evidence.generation)?);
    }
    for path in paths {
        if let Err(source) = std::fs::remove_file(&path)
            && source.kind() != std::io::ErrorKind::NotFound
        {
            return Err(ServerError::RuntimeState { path, source });
        }
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

pub fn control_socket_path(server_root: &Path, generation: Uuid) -> ServerResult<PathBuf> {
    let root = std::fs::metadata(server_root).map_err(|source| ServerError::RuntimeState {
        path: server_root.to_path_buf(),
        source,
    })?;
    let path = control_socket_directory()?.join(format!(
        "s-{:x}-{:x}-{}.sock",
        root.dev(),
        root.ino(),
        generation.simple()
    ));
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

pub fn prepare_control_socket_path(server_root: &Path, generation: Uuid) -> ServerResult<PathBuf> {
    let directory = control_socket_directory()?;
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) => {
            let uid = current_uid()?;
            if metadata.file_type().is_dir() && metadata.uid() == uid {
                std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                    .map_err(|source| ServerError::RuntimeState {
                        path: directory.clone(),
                        source,
                    })?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .map_err(|source| ServerError::RuntimeState {
                    path: directory.clone(),
                    source,
                })?;
        }
        Err(source) => {
            return Err(ServerError::RuntimeState {
                path: directory,
                source,
            });
        }
    }
    validate_control_socket_directory()?;
    control_socket_path(server_root, generation)
}

pub fn active_control_socket_path(server_root: &Path) -> ServerResult<Option<PathBuf>> {
    let Some(evidence) = read_runtime_evidence(server_root)? else {
        return Ok(None);
    };
    validate_control_socket_directory()?;
    control_socket_path(server_root, evidence.generation).map(Some)
}

fn control_socket_directory() -> ServerResult<PathBuf> {
    Ok(PathBuf::from(format!("/tmp/evx-{}", current_uid()?)))
}

fn current_uid() -> ServerResult<u32> {
    std::fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(|source| ServerError::RuntimeState {
            path: PathBuf::from("/proc/self"),
            source,
        })
}

fn validate_control_socket_directory() -> ServerResult<()> {
    let directory = control_socket_directory()?;
    let metadata =
        std::fs::symlink_metadata(&directory).map_err(|source| ServerError::RuntimeState {
            path: directory.clone(),
            source,
        })?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != current_uid()?
        || metadata.mode() & 0o077 != 0
    {
        return Err(ServerError::RuntimeState {
            path: directory,
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "control socket directory must be owned by the current user with mode 0700",
            ),
        });
    }
    Ok(())
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
    let metadata = std::fs::metadata(&executable).ok();
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

fn verify_process(evidence: &ProcessEvidence) -> IdentityState {
    let Ok(current_boot) = read_boot_id() else {
        return IdentityState::Unverifiable;
    };
    if current_boot != evidence.boot_id {
        return IdentityState::ProvenDead;
    }
    let ticks = match read_start_ticks(evidence.pid) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return IdentityState::ProvenDead;
        }
        Err(_) => return IdentityState::Unverifiable,
    };
    if ticks != evidence.start_ticks {
        return IdentityState::ProvenDead;
    }
    let executable = match std::fs::read_link(format!("/proc/{}/exe", evidence.pid)) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return IdentityState::ProvenDead;
        }
        Err(_) => return IdentityState::Unverifiable,
    };
    if executable != evidence.executable {
        return IdentityState::ProvenDead;
    }
    if evidence.executable_device.is_some() || evidence.executable_inode.is_some() {
        let Ok(metadata) = std::fs::metadata(&executable) else {
            return IdentityState::Unverifiable;
        };
        if evidence
            .executable_device
            .is_some_and(|value| value != metadata.dev())
            || evidence
                .executable_inode
                .is_some_and(|value| value != metadata.ino())
        {
            return IdentityState::ProvenDead;
        }
    }
    IdentityState::VerifiedAlive
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
    let Ok(path) = control_socket_path(server_root, evidence.generation) else {
        return false;
    };
    if validate_control_socket_directory().is_err() {
        return false;
    }
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

    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(root.path().join(".launcher")).expect("launcher dir");
        root
    }

    /// A supervisor holds its lock before it writes evidence and after it
    /// clears it, and the lock names it so a dead one is not waited on.
    #[test]
    fn a_held_runtime_lock_is_running_until_its_holder_is_proven_dead() {
        let root = root();
        create_runtime_lock(root.path()).expect("lock");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { child: None, .. }
        ));

        let lock = runtime_lock_path(root.path());
        let mut holder: ProcessEvidence =
            serde_json::from_slice(&std::fs::read(&lock).expect("read")).expect("holder");
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

    #[test]
    fn creating_a_runtime_lock_clears_only_abandoned_staged_locks() {
        let root = root();
        let launcher = root.path().join(".launcher");
        let abandoned = launcher.join(format!("{RUNTIME_LOCK_STAGING_PREFIX}abandoned"));
        let fresh = launcher.join(format!("{RUNTIME_LOCK_STAGING_PREFIX}fresh"));
        std::fs::write(&fresh, b"").expect("fresh");
        std::fs::File::create(&abandoned)
            .expect("abandoned")
            .set_modified(std::time::SystemTime::now() - ABANDONED_STAGING_AGE * 2)
            .expect("age");

        create_runtime_lock(root.path()).expect("lock");

        assert!(
            !abandoned.exists(),
            "a dead supervisor's staging is cleared"
        );
        assert!(fresh.exists(), "a live supervisor's staging is left alone");
    }

    #[test]
    fn a_runtime_lock_is_created_once_and_never_empty() {
        let root = root();
        create_runtime_lock(root.path()).expect("lock");
        let body = std::fs::read(runtime_lock_path(root.path())).expect("read");
        assert!(serde_json::from_slice::<ProcessEvidence>(&body).is_ok());
        assert!(matches!(
            create_runtime_lock(root.path()),
            Err(ServerError::RuntimeState { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists
        ));
    }

    #[test]
    fn a_verified_supervisor_requires_a_reachable_control_socket_to_be_controlled() {
        let root = root();
        let evidence = RuntimeEvidence::for_current_supervisor().expect("capture");
        write_runtime_evidence(root.path(), &evidence).expect("write");
        assert!(matches!(
            classify_runtime(root.path()).expect("classify"),
            RuntimeClassification::UnknownRunning { .. }
        ));

        let socket = prepare_control_socket_path(root.path(), evidence.generation).expect("path");
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
