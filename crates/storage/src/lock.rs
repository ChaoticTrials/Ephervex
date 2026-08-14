//! The per-instance launch lock and the running state derived from it
//!
//! One instance directory may be launched once at a time. Two Minecraft
//! processes sharing a game directory corrupt each other's `saves/`,
//! `options.txt`, and `logs/latest.log`, and the damage is silent — which is
//! precisely the failure a bug-reproduction launcher must not produce.
//!
//! # Why a file rather than an in-memory set
//!
//! The GUI, the CLI, and a second terminal are different *processes*. An
//! in-memory guard would only stop one of them from launching twice, which is
//! the case that happens least. The lock therefore lives next to the instance,
//! in `.launcher/launch.lock`, where anything that can see the instance can see
//! the lock.
//!
//! # Why it is safe to take over a lock nobody owns
//!
//! A launcher that is `SIGKILL`ed leaves its lock file behind, and an instance
//! that could never be launched again would be a worse bug than the one the
//! lock prevents. So a lock records **both** the owning pid and that process's
//! kernel start time, read from `/proc/<pid>/stat`. A pid alone is not enough:
//! pids are recycled, and a recycled one would make a dead launcher look alive
//! forever. The pair is unique for as long as the machine is up, which is
//! exactly as long as a lock file needs to mean anything.
//!
//! Where `/proc` is not available the lock is treated as held. That is the
//! conservative direction — refusing a launch is recoverable, corrupting a
//! world is not — and Ephervex targets Linux (ADR 0003).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::metadata::metadata_dir;
use crate::paths::{PathError, create_dir_all};

/// The lock file's name inside an instance's `.launcher/` directory.
pub const LAUNCH_LOCK_FILE: &str = "launch.lock";

/// How many times acquisition retries after clearing a lock whose owner is
/// gone.
///
/// One retry is enough: two launchers racing to clear the same dead lock both
/// try to create the file afterwards, exactly one wins, and the loser then sees
/// a *live* owner and reports it. A longer loop would only add ways to spin.
const ACQUIRE_ATTEMPTS: u32 = 3;

/// Why an instance could not be locked for launching.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LockError {
    /// Something already holds the lock and its owner is still running.
    #[error("the instance is already being launched by process {pid}")]
    AlreadyRunning {
        pid: u32,
        /// When that launch started, for a message the user can act on.
        since: DateTime<Utc>,
    },
    /// The lock file could not be created, read, or removed.
    #[error("the launch lock at {path} could not be used")]
    Unusable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Path(#[from] PathError),
}

impl LockError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::AlreadyRunning { .. } => "error-launch-already-running",
            Self::Unusable { .. } => "error-launch-lock-unusable",
            Self::Path(error) => error.message_id(),
        }
    }

    /// The path this failure is about, where it names one.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Unusable { path, .. } => Some(path.clone()),
            Self::AlreadyRunning { .. } | Self::Path(_) => None,
        }
    }

    fn unusable(path: &Path, source: std::io::Error) -> Self {
        Self::Unusable {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// What a held lock records.
///
/// Serialized as JSON so a user looking at a stuck instance can read it, and so
/// the schema can gain fields without invalidating existing files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchState {
    /// The launcher process that owns the launch — not the game.
    ///
    /// The lock is taken before anything is downloaded, so at that moment there
    /// is no game process to name. What holds the instance is the launcher run
    /// that is preparing and supervising it.
    pub owner_pid: u32,
    /// `/proc/<pid>/stat` field 22 for `owner_pid`, which disambiguates a
    /// recycled pid from the original.
    #[serde(default)]
    pub owner_start_ticks: Option<u64>,
    /// The Minecraft process, once it has been spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_pid: Option<u32>,
    pub since: DateTime<Utc>,
}

impl LaunchState {
    /// Whether the process that took this lock is still running.
    #[must_use]
    pub fn owner_is_alive(&self) -> bool {
        if !proc_filesystem_available() {
            // Cannot prove the owner is gone, so it is treated as present.
            return true;
        }
        match process_start_ticks(self.owner_pid) {
            // A start time was recorded and still matches: the same process.
            Some(ticks) => self
                .owner_start_ticks
                .is_none_or(|recorded| recorded == ticks),
            None => false,
        }
    }
}

/// A held launch lock. Released on drop.
///
/// Deliberately not `Clone`: the whole value of the type is that exactly one
/// live handle corresponds to one lock file.
#[derive(Debug)]
pub struct LaunchLock {
    path: PathBuf,
    state: LaunchState,
}

impl LaunchLock {
    /// Takes the lock for the current process, or reports who holds it.
    ///
    /// Creation is `O_EXCL`, so two launchers that arrive at the same instant
    /// cannot both believe they succeeded. A lock whose owner is gone is
    /// cleared and the creation retried.
    pub fn acquire(instance_root: &Path) -> Result<Self, LockError> {
        let directory = metadata_dir(instance_root);
        create_dir_all(&directory)?;
        let path = directory.join(LAUNCH_LOCK_FILE);

        let state = LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: process_start_ticks(std::process::id()),
            game_pid: None,
            since: Utc::now(),
        };

        for _ in 0..ACQUIRE_ATTEMPTS {
            match write_new(&path, &state) {
                Ok(()) => {
                    debug!(path = %path.display(), pid = state.owner_pid, "took the launch lock");
                    return Ok(Self { path, state });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let held = read_state(&path)?;
                    match held {
                        // A lock file we cannot parse is not evidence of a live
                        // launch, and refusing forever on one would strand the
                        // instance. Treated exactly like a dead owner.
                        Some(held) if held.owner_is_alive() => {
                            return Err(LockError::AlreadyRunning {
                                pid: held.owner_pid,
                                since: held.since,
                            });
                        }
                        held => {
                            warn!(
                                path = %path.display(),
                                pid = held.as_ref().map(|state| state.owner_pid),
                                "clearing a launch lock whose owner is gone"
                            );
                            if let Err(error) = std::fs::remove_file(&path) {
                                if error.kind() != std::io::ErrorKind::NotFound {
                                    return Err(LockError::unusable(&path, error));
                                }
                            }
                        }
                    }
                }
                Err(error) => return Err(LockError::unusable(&path, error)),
            }
        }

        // Every attempt lost the race, which means somebody else is holding it.
        match read_state(&path)? {
            Some(held) => Err(LockError::AlreadyRunning {
                pid: held.owner_pid,
                since: held.since,
            }),
            None => Err(LockError::unusable(
                &path,
                std::io::Error::other("the launch lock could not be taken"),
            )),
        }
    }

    /// Records the game's process id, so a frontend can show it and a stuck
    /// instance can be diagnosed without guessing.
    pub fn record_game_pid(&mut self, pid: u32) -> Result<(), LockError> {
        self.state.game_pid = Some(pid);
        let body = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| LockError::unusable(&self.path, error.into()))?;
        let mut temporary_name = self.path.as_os_str().to_os_string();
        temporary_name.push(format!(".tmp.{}", uuid::Uuid::new_v4()));
        let temporary = PathBuf::from(temporary_name);
        let result = (|| {
            use std::io::Write as _;

            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&body)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|error| LockError::unusable(&self.path, error))
    }

    /// What this lock records.
    #[must_use]
    pub const fn state(&self) -> &LaunchState {
        &self.state
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LaunchLock {
    fn drop(&mut self) {
        let owns_path = read_state(&self.path).ok().flatten().is_some_and(|state| {
            state.owner_pid == self.state.owner_pid
                && state.owner_start_ticks == self.state.owner_start_ticks
                && state.since == self.state.since
        });
        if !owns_path {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.path.display(), %error, "could not release the launch lock");
            }
        }
    }
}

/// What is currently launching or running in an instance directory, if
/// anything.
///
/// A lock whose owner is gone reports `None`: it describes a launcher that is
/// no longer there, and reporting it as running is how a UI ends up showing a
/// permanently "running" instance.
#[must_use]
pub fn running_state(instance_root: &Path) -> Option<LaunchState> {
    let path = metadata_dir(instance_root).join(LAUNCH_LOCK_FILE);
    read_state(&path)
        .ok()
        .flatten()
        .filter(LaunchState::owner_is_alive)
}

/// Creates the lock file, failing if it already exists.
fn write_new(path: &Path, state: &LaunchState) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let body = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    file.write_all(&body)?;
    file.sync_all()
}

/// Reads a lock file. `Ok(None)` means absent or unreadable as a lock.
fn read_state(path: &Path) -> Result<Option<LaunchState>, LockError> {
    match std::fs::read(path) {
        Ok(body) => Ok(serde_json::from_slice(&body).ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LockError::unusable(path, error)),
    }
}

fn proc_filesystem_available() -> bool {
    Path::new("/proc/self/stat").exists()
}

/// Field 22 of `/proc/<pid>/stat`: the process's start time in clock ticks
/// since boot.
///
/// The fields are counted from **after** the closing parenthesis of field 2,
/// because that field is the executable's name and may itself contain spaces
/// and parentheses — splitting the whole line on whitespace is the classic way
/// to read the wrong number.
fn process_start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(')')?;
    // Field 3 is the first token of `tail`; the start time is field 22, so it
    // is the twentieth token after the name.
    tail.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn instance() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp dir");
        create_dir_all(&metadata_dir(root.path())).expect("metadata dir");
        root
    }

    #[test]
    fn a_lock_is_held_until_it_is_dropped_and_the_second_launch_is_refused() {
        let root = instance();
        let held = LaunchLock::acquire(root.path()).expect("first launch takes the lock");
        assert!(held.path().exists());

        let error = LaunchLock::acquire(root.path())
            .expect_err("a second launch of the same instance must be refused");
        assert!(matches!(error, LockError::AlreadyRunning { pid,.. } if pid == std::process::id()));
        assert_eq!(error.message_id(), "error-launch-already-running");

        drop(held);
        LaunchLock::acquire(root.path()).expect("the lock is free again");
    }

    #[test]
    fn the_running_state_reports_the_owner_and_the_game_once_it_is_spawned() {
        let root = instance();
        let mut held = LaunchLock::acquire(root.path()).expect("lock");
        assert_eq!(
            running_state(root.path()).expect("running").game_pid,
            None,
            "nothing has been spawned yet"
        );

        held.record_game_pid(4242).expect("record");
        let state = running_state(root.path()).expect("running");
        assert_eq!(state.game_pid, Some(4242));
        assert_eq!(state.owner_pid, std::process::id());
    }

    #[test]
    fn recording_the_game_pid_publishes_complete_owner_evidence() {
        let root = instance();
        let mut held = LaunchLock::acquire(root.path()).expect("lock");
        held.record_game_pid(4242).expect("record");

        let body = std::fs::read(held.path()).expect("lock body");
        let state: LaunchState = serde_json::from_slice(&body).expect("complete JSON");
        assert_eq!(state, *held.state());
        assert!(
            std::fs::read_dir(held.path().parent().expect("parent"))
                .expect("metadata directory")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
        );
    }

    #[test]
    fn dropping_an_old_guard_does_not_remove_replacement_owner_evidence() {
        let root = instance();
        let held = LaunchLock::acquire(root.path()).expect("lock");
        let replacement = LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: process_start_ticks(std::process::id()),
            game_pid: Some(99),
            since: Utc::now(),
        };
        std::fs::write(
            held.path(),
            serde_json::to_vec_pretty(&replacement).expect("serialize"),
        )
        .expect("replace evidence");

        let path = held.path().to_path_buf();
        drop(held);
        assert!(path.exists());
    }

    #[test]
    fn an_instance_with_no_lock_is_not_running() {
        let root = instance();
        assert!(running_state(root.path()).is_none());
    }

    #[test]
    fn a_lock_left_by_a_dead_launcher_is_taken_over_rather_than_stranding_the_instance() {
        let root = instance();
        // Pid 0 is never a live process, so this stands in for a launcher that
        // was killed without releasing its lock.
        let dead = LaunchState {
            owner_pid: 0,
            owner_start_ticks: Some(1),
            game_pid: None,
            since: Utc::now(),
        };
        let path = metadata_dir(root.path()).join(LAUNCH_LOCK_FILE);
        std::fs::write(&path, serde_json::to_vec(&dead).expect("json")).expect("write");

        assert!(
            running_state(root.path()).is_none(),
            "a dead owner is not a running instance"
        );
        let held = LaunchLock::acquire(root.path()).expect("a dead owner's lock is taken over");
        assert_eq!(held.state().owner_pid, std::process::id());
    }

    #[test]
    fn a_recycled_pid_does_not_make_a_dead_launcher_look_alive() {
        let root = instance();
        // Our own pid, but a start time that cannot be ours: the pair is what
        // identifies a process, and this pair names one that no longer exists.
        let recycled = LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: Some(0),
            game_pid: None,
            since: Utc::now(),
        };
        let path = metadata_dir(root.path()).join(LAUNCH_LOCK_FILE);
        std::fs::write(&path, serde_json::to_vec(&recycled).expect("json")).expect("write");

        assert!(!recycled.owner_is_alive());
        LaunchLock::acquire(root.path()).expect("the pid was recycled; the lock is stale");
    }

    #[test]
    fn a_corrupt_lock_file_does_not_strand_the_instance_forever() {
        let root = instance();
        let path = metadata_dir(root.path()).join(LAUNCH_LOCK_FILE);
        std::fs::write(&path, b"not json at all").expect("write");

        assert!(running_state(root.path()).is_none());
        LaunchLock::acquire(root.path()).expect("an unreadable lock is not evidence of a launch");
    }

    #[test]
    fn this_processs_start_time_is_readable_and_stable() {
        // The parse has to survive an executable name containing spaces and
        // parentheses, which is why it counts fields from the last `)`.
        let first = process_start_ticks(std::process::id());
        if proc_filesystem_available() {
            let ticks = first.expect("/proc/self/stat should be readable");
            assert!(ticks > 0);
            assert_eq!(process_start_ticks(std::process::id()), Some(ticks));
        }
        assert_eq!(process_start_ticks(u32::MAX), None);
    }
}
