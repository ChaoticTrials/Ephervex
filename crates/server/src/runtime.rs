//! Conservative Linux process identity and orphan classification.

use std::io::Write as _;
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
        write_runtime_evidence(root.path(), &evidence).expect("write");
        let socket = prepare_control_socket_path(root.path(), evidence.generation).expect("path");
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
