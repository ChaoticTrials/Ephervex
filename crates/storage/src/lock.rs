//! The per-instance launch lock and the running state derived from it
//!
//! One instance directory may be launched once at a time. Two Minecraft
//! processes sharing a game directory corrupt each other's `saves/`,
//! `options.txt`, and `logs/latest.log`, and the damage is silent - which is
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
//! Acquiring, taking over, and force-stopping a lock happen under an exclusive
//! `flock` on `.launcher/launch.guard`, so two launchers can never both clear
//! the same dead lock and both believe they won. Releasing does too, unless the
//! guard cannot be had in time.
//!
//! Where `/proc` is not available the lock is treated as held. That is the
//! conservative direction - refusing a launch is recoverable, corrupting a
//! world is not - and Ephervex targets Linux (ADR 0003).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::metadata::metadata_dir;
use crate::paths::{PathError, create_dir_all};

/// The lock file's name inside an instance's `.launcher/` directory.
///
/// One instance can hold more than one launch at a time -- launching as a
/// second account is exactly that -- so this is the name of the *first* one.
/// A launch that is content to run alongside another names its own file
/// [`additional_lock_file`] instead, so the two cannot clear each other's.
pub const LAUNCH_LOCK_FILE: &str = "launch.lock";

/// The file whose `flock` serializes every decision about who holds an
/// instance's launch locks. It records nothing and is not itself a launch lock.
const OWNERSHIP_GUARD_FILE: &str = "launch.guard";

/// How long taking the ownership guard waits on another process before giving up.
const OWNERSHIP_GUARD_PATIENCE: std::time::Duration = std::time::Duration::from_secs(5);

/// The prefix every launch lock file's name starts with.
const LAUNCH_LOCK_PREFIX: &str = "launch.";

/// The lock file one additional launch holds.
///
/// `id` identifies the launch to whoever started it: it is the same handle a
/// frontend uses to find the launch again in a list and to stop it. Validated
/// as a single safe path segment, because it reaches a file name.
fn additional_lock_file(id: &str) -> Option<String> {
    let safe = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    safe.then(|| format!("{LAUNCH_LOCK_PREFIX}{id}.lock"))
}

/// One launch an instance currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningLaunch {
    /// The launch's own handle: the id its starter gave it, or `None` for the
    /// first launch, which holds the unnamed lock.
    pub id: Option<String>,
    pub state: LaunchState,
}

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
    /// Another launcher was deciding who holds the instance at the same time,
    /// and kept at it for longer than this one waits.
    #[error("another launcher is claiming the instance at {path}")]
    Busy { path: PathBuf },
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
            Self::Busy { .. } => "error-launch-lock-busy",
            Self::Unusable { .. } => "error-launch-lock-unusable",
            Self::Path(error) => error.message_id(),
        }
    }

    /// The path this failure is about, where it names one.
    #[must_use]
    pub fn failing_path(&self) -> Option<PathBuf> {
        match self {
            Self::Unusable { path, .. } | Self::Busy { path } => Some(path.clone()),
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
    /// The launcher process that owns the launch - not the game.
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
    /// `/proc/<pid>/stat` field 22 for `game_pid`, which distinguishes the game
    /// from a later process that happened to be given the same number.
    ///
    /// Recorded for the same reason as `owner_start_ticks`, and needed for a
    /// stronger one: a pid alone is not safe to signal, so anything that ends a
    /// launch it is not itself supervising has to prove first that the pid is
    /// still the process the lock is describing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_start_ticks: Option<u64>,
    pub since: DateTime<Utc>,
}

impl LaunchState {
    /// Whether the game this lock describes is still the process it names.
    ///
    /// `false` when there is no game recorded yet, when it has exited, when the
    /// pid now belongs to something else, or when no start time was recorded to
    /// prove which process it was. Never a guess: a caller that is about to
    /// signal a pid, or to keep an instance locked for it, needs proof.
    #[must_use]
    pub fn game_is_alive(&self) -> bool {
        let (Some(pid), Some(recorded)) = (self.game_pid, self.game_start_ticks) else {
            return false;
        };
        proc_filesystem_available() && process_start_ticks(pid) == Some(recorded)
    }

    /// Whether this launch still holds its instance: its launcher run is
    /// alive, or the game it started still is.
    ///
    /// A launcher that died leaves its game running, and the game still writes
    /// to the instance's saves, options, and logs. Treating that instance as
    /// free is how a second game ends up sharing its directory.
    #[must_use]
    pub fn holds_instance(&self) -> bool {
        self.owner_is_alive() || self.game_is_alive()
    }

    /// The process that keeps this launch alive: the launcher run, or the
    /// game when the run is gone.
    #[must_use]
    pub fn holder_pid(&self) -> u32 {
        match self.game_pid {
            Some(game) if self.is_orphaned() => game,
            _ => self.owner_pid,
        }
    }

    /// Whether the game outlived the launcher run that started it.
    ///
    /// Such a game is running and holds its instance, but nothing supervises
    /// it: its output pipes and process handle went with that run. It can
    /// only be ended by signaling the proven process.
    #[must_use]
    pub fn is_orphaned(&self) -> bool {
        !self.owner_is_alive() && self.game_is_alive()
    }

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

/// What ending a launch from outside it actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ForcedStop {
    /// Whether the game process was signaled.
    pub signaled_game: bool,
    /// Whether the launcher run that owns the lock was signaled.
    pub signaled_owner: bool,
    /// Whether a lock file was removed.
    pub released: bool,
}

/// Ends a launch this process is not supervising, and releases its lock.
///
/// The ordinary way a launch stops is the run that owns it canceling and
/// waiting for the child, which releases the lock on the way out. This is for
/// when that is not available: the launch belongs to a run that has since gone,
/// or to one that is wedged, and the instance is left claiming to be busy.
///
/// # Why a pid alone is not enough
///
/// Process ids are reused. Signaling a number read out of a file could reach
/// something else entirely, so nothing here is signaled without first proving
/// the pid is still the process the lock describes, by the start time recorded
/// beside it. A pid that cannot be proven is skipped rather than signaled.
///
/// `SIGTERM` rather than `SIGKILL`: Minecraft saves on it. A game that was
/// signaled keeps its lock until it has exited, so an instance is never offered
/// for a second launch while its game is still writing to it; a launch with no
/// live game has its lock released at once.
pub fn force_release(instance_root: &Path, launch: Option<&str>) -> Result<ForcedStop, LockError> {
    let name = match launch {
        Some(id) => additional_lock_file(id).ok_or_else(|| {
            LockError::unusable(
                &metadata_dir(instance_root),
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "unusable launch id"),
            )
        })?,
        None => LAUNCH_LOCK_FILE.to_owned(),
    };
    let directory = metadata_dir(instance_root);
    let path = directory.join(name);
    // No lock is nothing to stop, and needs no guard to say so.
    if !path.exists() {
        return Ok(ForcedStop::default());
    }
    let _ownership = OwnershipGuard::take(&directory)?;
    let Some(state) = read_state(&path)? else {
        return Ok(ForcedStop::default());
    };

    let mut outcome = ForcedStop::default();
    // The game first, so it is not reparented and left running by the death of
    // the run that was supervising it.
    if state.game_is_alive()
        && let Some(pid) = state.game_pid
    {
        outcome.signaled_game = terminate(pid);
    }
    if state.owner_is_alive() && state.owner_pid != std::process::id() {
        outcome.signaled_owner = terminate(state.owner_pid);
    }

    // A signaled game has been asked to stop, not proven stopped, and while
    // it runs it still writes to the instance. Its lock stays until it has
    // exited, and is then cleared like any lock nobody holds.
    if outcome.signaled_game {
        debug!(path = %path.display(), ?outcome, "asked a launch to end; its lock stays until it exits");
        return Ok(outcome);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => outcome.released = true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(LockError::unusable(&path, error)),
    }
    debug!(path = %path.display(), ?outcome, "ended a launch from outside it");
    Ok(outcome)
}

/// Asks one process to stop. `false` when it was already gone.
fn terminate(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    rustix::process::Pid::from_raw(raw).is_some_and(|pid| {
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).is_ok()
    })
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
    /// Takes a lock that does not exclude the launch already running.
    ///
    /// For a launch the user asked for knowing one is already in progress:
    /// a second client, under a second account, sharing the instance's game
    /// directory. Nothing here makes that safe -- two clients opening one
    /// world still fight over it, and Minecraft's own world lock is what
    /// decides that -- it only stops the launcher from refusing on its own.
    ///
    /// The file is named after `id`, so it can never collide with another
    /// launch's and neither can clear the other's on the way out.
    pub fn acquire_additional(instance_root: &Path, id: &str) -> Result<Self, LockError> {
        let directory = metadata_dir(instance_root);
        create_dir_all(&directory)?;
        let name = additional_lock_file(id).ok_or_else(|| {
            LockError::unusable(
                &directory.join(id),
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "unusable launch id"),
            )
        })?;
        let ownership = OwnershipGuard::take(&directory)?;
        Self::take_at(&ownership, directory.join(name))
    }

    /// Takes the lock at `path` unless a live launch holds it.
    ///
    /// A lock that cannot be parsed is replaced like a dead one: it is not
    /// evidence of a launch, and refusing on it would strand the instance.
    fn take_at(ownership: &OwnershipGuard, path: PathBuf) -> Result<Self, LockError> {
        // One read decides both liveness and whether a file is there to replace.
        let existing = match std::fs::read(&path) {
            Ok(body) => Some(serde_json::from_slice::<LaunchState>(&body).ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(LockError::unusable(&path, error)),
        };
        if let Some(Some(held)) = &existing
            && held.holds_instance()
        {
            return Err(already_running(held));
        }
        let state = Self::current_state();
        let body =
            serde_json::to_vec(&state).map_err(|error| LockError::unusable(&path, error.into()))?;
        if let Some(previous) = existing {
            replace_file(&path, &body).map_err(|error| LockError::unusable(&path, error))?;
            warn!(
                path = %path.display(),
                pid = previous.map(|previous| previous.owner_pid),
                "replaced a launch lock no live launch holds"
            );
        } else {
            // Exclusive even where the guard could not be taken across processes.
            match create_new(&path, &body) {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(match read_state(&path)? {
                        Some(held) if held.holds_instance() => already_running(&held),
                        _ => LockError::Busy {
                            path: ownership.path().to_path_buf(),
                        },
                    });
                }
                result => result.map_err(|error| LockError::unusable(&path, error))?,
            }
        }
        debug!(path = %path.display(), pid = state.owner_pid, "took a launch lock");
        Ok(Self { path, state })
    }

    fn current_state() -> LaunchState {
        LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: process_start_ticks(std::process::id()),
            game_pid: None,
            game_start_ticks: None,
            since: Utc::now(),
        }
    }

    /// Takes the lock for the current process, or reports who holds it.
    ///
    /// A lock whose owner and game are both gone is replaced.
    pub fn acquire(instance_root: &Path) -> Result<Self, LockError> {
        let directory = metadata_dir(instance_root);
        create_dir_all(&directory)?;
        let ownership = OwnershipGuard::take(&directory)?;
        Self::take_at(&ownership, directory.join(LAUNCH_LOCK_FILE))
    }

    /// Records the game's process id, so a frontend can show it and a stuck
    /// instance can be diagnosed without guessing.
    pub fn record_game_pid(&mut self, pid: u32) -> Result<(), LockError> {
        self.state.game_pid = Some(pid);
        self.state.game_start_ticks = process_start_ticks(pid);
        let body = serde_json::to_vec(&self.state)
            .map_err(|error| LockError::unusable(&self.path, error.into()))?;
        replace_file(&self.path, &body).map_err(|error| LockError::unusable(&self.path, error))
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
        let Some(directory) = self.path.parent().filter(|directory| directory.is_dir()) else {
            return;
        };
        // Releasing unguarded beats leaving this live process's lock behind.
        let _ownership = OwnershipGuard::take(directory)
            .inspect_err(|error| warn!(%error, "releasing the launch lock without its guard"))
            .ok();
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
/// A lock whose owner and game are both gone reports `None`: it describes a
/// launch that is over, and reporting it as running is how a UI ends up showing
/// a permanently "running" instance. A game that outlived its owner is still
/// running, and is reported.
#[must_use]
pub fn running_state(instance_root: &Path) -> Option<LaunchState> {
    running_launches(instance_root)
        .into_iter()
        .next()
        .map(|launch| launch.state)
}

/// Every launch this instance currently holds, first one first.
///
/// More than one is an ordinary state: launching as a second account while the
/// first client is still up is what produces it. The unnamed lock sorts first
/// and the rest by their own ids, so two readings of the same directory agree.
///
/// A lock whose owner is gone and whose game is gone too is skipped rather than
/// reported, for the same reason a single one is: a permanently "running"
/// instance is worse than none. One whose game outlived its owner is reported,
/// as orphaned, because it is running.
#[must_use]
pub fn running_launches(instance_root: &Path) -> Vec<RunningLaunch> {
    let directory = metadata_dir(instance_root);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };

    let mut launches: Vec<RunningLaunch> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = launch_id_of(&name)?.handle();
            let state = read_state(&entry.path()).ok().flatten()?;
            state
                .holds_instance()
                .then_some(RunningLaunch { id, state })
        })
        .collect();
    launches.sort_by(|left, right| left.id.cmp(&right.id));
    launches
}

/// Which launch a lock file's name names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LockName {
    /// The first launch, which holds the unnamed lock.
    First,
    /// An additional launch, under the handle its starter gave it.
    Additional(String),
}

impl LockName {
    fn handle(self) -> Option<String> {
        match self {
            Self::First => None,
            Self::Additional(id) => Some(id),
        }
    }
}

/// The launch a lock file's name names, if it names one at all.
///
/// `None` means the file is not a launch lock, which every other thing under
/// `.launcher/` is.
fn launch_id_of(name: &str) -> Option<LockName> {
    if name == LAUNCH_LOCK_FILE {
        return Some(LockName::First);
    }
    let id = name
        .strip_prefix(LAUNCH_LOCK_PREFIX)?
        .strip_suffix(".lock")?;
    (!id.is_empty()).then(|| LockName::Additional(id.to_owned()))
}

/// Holds the right to decide who holds an instance's launch locks.
///
/// A `flock` rather than a lock file recording a pid: the kernel drops it when
/// the holder dies. Taken on a file opened for writing, because NFS refuses an
/// exclusive lock on a read-only descriptor. NFS also gives a process one lock
/// per file shared by all its threads, so threads are kept apart in-process too.
struct OwnershipGuard {
    // Declared first so the flock is released before the in-process claim.
    _file: Option<std::fs::File>,
    claim: InProcessClaim,
}

/// This process's claim on one guard file.
struct InProcessClaim(PathBuf);

/// The guard files a thread of this process currently holds.
static HELD_IN_PROCESS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
static RELEASED_IN_PROCESS: std::sync::Condvar = std::sync::Condvar::new();

impl OwnershipGuard {
    /// The guard file.
    fn path(&self) -> &Path {
        &self.claim.0
    }

    /// Waits a bounded time, so a stopped or wedged launcher cannot hang this one.
    fn take(directory: &Path) -> Result<Self, LockError> {
        use rustix::fs::{FlockOperation, Mode, OFlags, flock};
        use rustix::io::Errno;

        // Canonical, so two spellings of one instance claim the same guard.
        let path = std::fs::canonicalize(directory)
            .map_err(|error| LockError::unusable(directory, error))?
            .join(OWNERSHIP_GUARD_FILE);
        let deadline = std::time::Instant::now() + OWNERSHIP_GUARD_PATIENCE;
        let busy = || LockError::Busy { path: path.clone() };

        let mut held = HELD_IN_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while held.contains(&path) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(busy());
            }
            held = RELEASED_IN_PROCESS
                .wait_timeout(held, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        held.push(path.clone());
        drop(held);
        let claim = InProcessClaim(path.clone());

        let file = rustix::fs::open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map(std::fs::File::from)
        .map_err(|errno| LockError::unusable(&path, errno.into()))?;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(Errno::INTR) => {}
                Err(Errno::WOULDBLOCK) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(Errno::WOULDBLOCK) => return Err(busy()),
                // A filesystem with no locking at all keeps the in-process
                // exclusion, which is as much as the lock files alone gave.
                Err(errno @ (Errno::NOLCK | Errno::OPNOTSUPP | Errno::NOSYS)) => {
                    warn!(path = %path.display(), %errno, "launch locks are not guarded across processes here");
                    return Ok(Self { _file: None, claim });
                }
                Err(errno) => return Err(LockError::unusable(&path, errno.into())),
            }
        }
        Ok(Self {
            _file: Some(file),
            claim,
        })
    }
}

impl Drop for InProcessClaim {
    fn drop(&mut self) {
        let mut held = HELD_IN_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.retain(|path| *path != self.0);
        drop(held);
        RELEASED_IN_PROCESS.notify_all();
    }
}

fn already_running(held: &LaunchState) -> LockError {
    LockError::AlreadyRunning {
        pid: held.holder_pid(),
        since: held.since,
    }
}

/// Replaces `path` with `body` in one step, so a reader sees the old lock or
/// the new one and never neither.
///
/// The directory is not synced, unlike [`crate::atomic`]: a lock means nothing
/// after a reboot, and a failure there would come after the lock is in place.
fn replace_file(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    // Ends in a uuid, never `.lock`, so `launch_id_of` ignores it.
    let mut staging_name = path.as_os_str().to_os_string();
    staging_name.push(format!(".tmp.{}", uuid::Uuid::new_v4()));
    let staging = PathBuf::from(staging_name);
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .and_then(|mut file| file.write_all(body).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&staging, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result
}

/// Creates `path` with `body`, failing with `AlreadyExists` if it exists.
///
/// The body is staged and hard-linked into place, so a reader never sees an
/// empty lock and mistakes a starting launch for none.
fn create_new(path: &Path, body: &[u8]) -> std::io::Result<()> {
    // Ends in a uuid, never `.lock`, so `launch_id_of` ignores it.
    let mut staging_name = path.as_os_str().to_os_string();
    staging_name.push(format!(".tmp.{}", uuid::Uuid::new_v4()));
    let staging = PathBuf::from(staging_name);

    let published =
        create_with_body(&staging, body).and_then(|()| std::fs::hard_link(&staging, path));
    let _ = std::fs::remove_file(&staging);
    match published {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            create_with_body(path, body)
        }
        result => result,
    }
}

/// Creates `path` with `body`, failing if it already exists.
fn create_with_body(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(body)?;
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
/// and parentheses - splitting the whole line on whitespace is the classic way
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

    fn plant(path: &Path, state: &LaunchState) {
        replace_file(path, &serde_json::to_vec(state).expect("json")).expect("lock file");
    }

    /// A lock left by a launcher run that is gone, naming `game` as the game
    /// it started, with the start time `game_ticks`.
    fn orphaned_lock(root: &Path, game: u32, game_ticks: Option<u64>) {
        let own = process_start_ticks(std::process::id()).expect("own start time");
        let state = LaunchState {
            // This process's id with a start time it does not have: a run that
            // is provably gone.
            owner_pid: std::process::id(),
            owner_start_ticks: Some(own.wrapping_add(1)),
            game_pid: Some(game),
            game_start_ticks: game_ticks,
            since: Utc::now(),
        };
        plant(&metadata_dir(root).join(LAUNCH_LOCK_FILE), &state);
    }

    /// A process standing in for a game that outlived its launcher.
    fn live_game() -> (std::process::Child, u64) {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a child");
        let ticks = process_start_ticks(child.id()).expect("its start time");
        (child, ticks)
    }

    #[test]
    fn a_game_that_outlived_its_launcher_keeps_the_instance_locked() {
        let root = instance();
        let (mut game, ticks) = live_game();
        orphaned_lock(root.path(), game.id(), Some(ticks));

        let state = running_state(root.path()).expect("still running");
        assert!(state.is_orphaned());
        assert_eq!(state.holder_pid(), game.id());
        let error = LaunchLock::acquire(root.path()).expect_err("a second game is refused");
        assert!(matches!(error, LockError::AlreadyRunning { pid, .. } if pid == game.id()));

        game.kill().expect("end the game");
        game.wait().expect("reap");
        assert!(
            running_state(root.path()).is_none(),
            "once the game is gone the launch is over"
        );
        LaunchLock::acquire(root.path()).expect("and its stale lock is cleared by the next launch");
    }

    #[test]
    fn a_recycled_game_pid_is_not_taken_for_the_orphaned_game() {
        let root = instance();
        let (mut stranger, ticks) = live_game();
        // The recorded id now belongs to a process that started at another time.
        orphaned_lock(root.path(), stranger.id(), Some(ticks.wrapping_add(1)));
        assert!(running_state(root.path()).is_none());
        LaunchLock::acquire(root.path()).expect("the instance is free");

        let other = instance();
        // No start time recorded at all proves nothing either.
        orphaned_lock(other.path(), stranger.id(), None);
        assert!(running_state(other.path()).is_none());

        stranger.kill().expect("end");
        stranger.wait().expect("reap");
    }

    #[test]
    fn force_stopping_an_orphan_signals_only_the_proven_game_and_keeps_its_lock() {
        let root = instance();
        let (mut stranger, ticks) = live_game();
        orphaned_lock(root.path(), stranger.id(), Some(ticks.wrapping_add(1)));
        let outcome = force_release(root.path(), None).expect("released");
        assert!(
            !outcome.signaled_game,
            "a process that cannot be proven is left alone"
        );
        assert!(outcome.released);
        assert!(
            stranger.try_wait().expect("status").is_none(),
            "still running"
        );
        stranger.kill().expect("end");
        stranger.wait().expect("reap");

        let (mut game, ticks) = live_game();
        orphaned_lock(root.path(), game.id(), Some(ticks));
        let outcome = force_release(root.path(), None).expect("signaled");
        assert!(outcome.signaled_game);
        assert!(!outcome.signaled_owner, "the dead owner is not signaled");
        assert!(
            !outcome.released,
            "asked to stop is not stopped: the lock stays while the game may still write"
        );
        game.wait().expect("the game ends on the signal");
        assert!(running_state(root.path()).is_none());
        LaunchLock::acquire(root.path()).expect("free once the game has exited");
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
    fn owner_evidence_is_stored_compactly_and_a_pretty_file_from_an_older_build_still_loads() {
        let root = instance();
        let mut held = LaunchLock::acquire(root.path()).expect("lock");
        held.record_game_pid(4242).expect("record");

        let text = std::fs::read_to_string(held.path()).expect("lock body");
        assert!(
            !text.contains('\n'),
            "the launch lock carries pretty-printing whitespace: {text}"
        );

        let state: LaunchState = serde_json::from_str(&text).expect("parse");
        std::fs::write(
            held.path(),
            serde_json::to_string_pretty(&state).expect("indent"),
        )
        .expect("rewrite as an older build would have");
        assert_eq!(running_state(root.path()).expect("running"), state);
    }

    /// Launching as a second account while the first client is up is the whole
    /// point of the additional lock: it must not be refused, and it must not
    /// disturb the launch already holding the instance.
    #[test]
    fn a_second_launch_runs_alongside_the_first_without_clearing_it() {
        let root = instance();
        let first = LaunchLock::acquire(root.path()).expect("the first launch");
        let second =
            LaunchLock::acquire_additional(root.path(), "bob").expect("a second launch is allowed");

        let launches = running_launches(root.path());
        assert_eq!(launches.len(), 2);
        assert_eq!(
            launches[0].id, None,
            "the first launch holds the unnamed lock"
        );
        assert_eq!(launches[1].id.as_deref(), Some("bob"));
        assert!(
            running_state(root.path()).is_some(),
            "the instance is running while either of them is"
        );

        // Each releases only its own.
        drop(second);
        assert_eq!(running_launches(root.path()).len(), 1);
        assert!(
            LaunchLock::acquire(root.path()).is_err(),
            "the first launch still holds the instance"
        );
        drop(first);
        assert!(running_launches(root.path()).is_empty());
    }

    /// A plain launch is still exclusive. Only a caller that has said it is
    /// content to run alongside another gets to.
    #[test]
    fn an_ordinary_second_launch_is_still_refused() {
        let root = instance();
        let _held = LaunchLock::acquire(root.path()).expect("the first launch");
        assert!(matches!(
            LaunchLock::acquire(root.path()),
            Err(LockError::AlreadyRunning { .. })
        ));
    }

    /// The id reaches a file name, so it cannot be anything that would leave
    /// the directory or name something else in it.
    #[test]
    fn a_launch_id_that_could_name_another_file_is_refused() {
        let root = instance();
        for id in ["", "../escape", "a/b", "a.lock", "with space"] {
            assert!(
                LaunchLock::acquire_additional(root.path(), id).is_err(),
                "{id:?} must not become a lock file name"
            );
        }
        assert!(running_launches(root.path()).is_empty());
    }

    /// Only this launcher's own lock files are launches; everything else under
    /// `.launcher/` is somebody else's business.
    #[test]
    fn only_launch_locks_are_read_as_launches() {
        assert_eq!(launch_id_of(LAUNCH_LOCK_FILE), Some(LockName::First));
        assert_eq!(
            launch_id_of("launch.bob.lock"),
            Some(LockName::Additional("bob".to_owned()))
        );
        assert_eq!(launch_id_of("mod-display.json"), None);
        assert_eq!(launch_id_of("launch..lock"), None);
        assert_eq!(launch_id_of("server-runtime.json"), None);
        assert_eq!(launch_id_of(OWNERSHIP_GUARD_FILE), None);
    }

    #[test]
    fn a_published_lock_is_complete_and_leaves_no_staging_file_behind() {
        let root = instance();
        let held = LaunchLock::acquire(root.path()).expect("lock");
        let extra = LaunchLock::acquire_additional(root.path(), "bob").expect("additional");

        for lock in [&held, &extra] {
            let body = std::fs::read(lock.path()).expect("read lock");
            let state: LaunchState = serde_json::from_slice(&body).expect("parseable lock");
            assert_eq!(&state, lock.state());
        }
        let mut names: Vec<String> = std::fs::read_dir(metadata_dir(root.path()))
            .expect("metadata dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .into_string()
                    .expect("utf-8")
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["launch.bob.lock", OWNERSHIP_GUARD_FILE, LAUNCH_LOCK_FILE]
        );
        assert_eq!(running_launches(root.path()).len(), 2);
        assert_eq!(launch_id_of("launch.lock.tmp.0"), None);
        assert_eq!(launch_id_of("launch.bob.lock.tmp.0"), None);
    }

    #[test]
    fn dropping_an_old_guard_does_not_remove_replacement_owner_evidence() {
        let root = instance();
        let held = LaunchLock::acquire(root.path()).expect("lock");
        let replacement = LaunchState {
            owner_pid: std::process::id(),
            owner_start_ticks: process_start_ticks(std::process::id()),
            game_pid: Some(99),
            game_start_ticks: None,
            since: Utc::now(),
        };
        std::fs::write(
            held.path(),
            serde_json::to_vec(&replacement).expect("serialize"),
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
            game_start_ticks: None,
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

    /// Runs `contenders` acquisitions at once and returns the locks that
    /// succeeded, after checking every other one was refused for a live holder.
    fn race(
        contenders: usize,
        acquire: impl Fn() -> Result<LaunchLock, LockError> + Sync,
    ) -> Vec<LaunchLock> {
        let barrier = std::sync::Barrier::new(contenders);
        std::thread::scope(|scope| {
            let attempts: Vec<_> = (0..contenders)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        acquire()
                    })
                })
                .collect();
            attempts
                .into_iter()
                .filter_map(|attempt| match attempt.join().expect("no panic") {
                    Ok(held) => Some(held),
                    Err(LockError::AlreadyRunning { .. }) => None,
                    Err(error) => panic!("a loser must see the winner, not {error}"),
                })
                .collect()
        })
    }

    #[test]
    fn launchers_racing_to_take_over_a_dead_lock_leave_exactly_one_holder() {
        for _ in 0..5 {
            let root = instance();
            let dead = LaunchState {
                owner_pid: 0,
                owner_start_ticks: Some(1),
                game_pid: None,
                game_start_ticks: None,
                since: Utc::now(),
            };
            plant(&metadata_dir(root.path()).join(LAUNCH_LOCK_FILE), &dead);
            plant(&metadata_dir(root.path()).join("launch.bob.lock"), &dead);

            let held = race(16, || LaunchLock::acquire(root.path()));
            assert_eq!(held.len(), 1, "one ordinary launch holds the instance");

            let held = race(16, || LaunchLock::acquire_additional(root.path(), "bob"));
            assert_eq!(held.len(), 1, "one launch holds each additional lock");
        }
    }

    #[test]
    fn a_launch_waits_for_the_ownership_decision_another_launcher_is_making() {
        let root = instance();
        let directory = metadata_dir(root.path());
        // A descriptor of its own, as another process would have.
        let deciding = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(OWNERSHIP_GUARD_FILE))
            .expect("guard file");
        rustix::fs::flock(&deciding, rustix::fs::FlockOperation::LockExclusive).expect("flock");

        let waiting = std::thread::spawn({
            let root = root.path().to_path_buf();
            move || LaunchLock::acquire(&root).map(|held| held.state().clone())
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !waiting.is_finished(),
            "nothing is decided while the guard is held"
        );
        assert!(!directory.join(LAUNCH_LOCK_FILE).exists());

        drop(deciding);
        waiting
            .join()
            .expect("no panic")
            .expect("taken once the guard is free");
    }

    #[test]
    fn a_thread_waits_for_the_ownership_decision_another_thread_is_making() {
        let root = instance();
        let deciding = OwnershipGuard::take(&metadata_dir(root.path())).expect("guard");

        let waiting = std::thread::spawn({
            let root = root.path().to_path_buf();
            move || LaunchLock::acquire(&root).map(|held| held.state().clone())
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !waiting.is_finished(),
            "nothing is decided while the claim is held"
        );

        drop(deciding);
        waiting
            .join()
            .expect("no panic")
            .expect("taken once the claim is free");
    }

    #[test]
    fn a_force_stop_waits_for_the_ownership_decision_another_launcher_is_making() {
        let root = instance();
        let directory = metadata_dir(root.path());
        let (mut stranger, ticks) = live_game();
        orphaned_lock(root.path(), stranger.id(), Some(ticks.wrapping_add(1)));
        let deciding = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(OWNERSHIP_GUARD_FILE))
            .expect("guard file");
        rustix::fs::flock(&deciding, rustix::fs::FlockOperation::LockExclusive).expect("flock");

        let waiting = std::thread::spawn({
            let root = root.path().to_path_buf();
            move || force_release(&root, None).map(|outcome| outcome.released)
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !waiting.is_finished(),
            "nothing is removed while the guard is held"
        );
        assert!(directory.join(LAUNCH_LOCK_FILE).exists());

        drop(deciding);
        assert!(waiting.join().expect("no panic").expect("released"));
        stranger.kill().expect("end");
        stranger.wait().expect("reap");
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
            game_start_ticks: None,
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
