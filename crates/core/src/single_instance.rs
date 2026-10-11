//! One launcher per user, and a way to hand it a link.
//!
//! # Why this exists
//!
//! A desktop scheme handler starts a *new process* for every link. Without
//! somewhere for that process to say "here, you take this", clicking a second
//! link opens a second launcher, and two launchers sharing one set of instances
//! is a worse problem than a wasted window.
//!
//! # Why a Unix socket
//!
//! The only place two Ephervex processes already meet is the artifact cache
//! lock, which contends gracefully but cannot carry a message. A socket can,
//! and it answers both questions at once: binding succeeds exactly when nobody
//! else holds it, so acquiring the name and finding the running instance are
//! the same operation and cannot disagree.
//!
//! A refused connection to a socket file means the last owner died without
//! cleaning up. That is a stale name to remove, not an error to report: it is
//! the normal state after a crash or a reboot that left `/tmp` alone. Any other
//! failure to connect, or a path that is not a socket of ours, proves nothing
//! about the owner and is reported instead of removed.
//!
//! # Taking over a name
//!
//! Removing a stale name and binding it again is two steps, so two launchers
//! doing it at once could each remove the other's live socket and both believe
//! they are the launcher. Every bind, takeover, and release therefore happens
//! under an exclusive `flock` on a lock file beside the socket. The kernel
//! drops that lock with the process, and the lock file itself is never removed,
//! because removing a lock file reopens the race it exists to close.
//!
//! # The 108-byte limit
//!
//! `sun_path` is 108 bytes, so the socket cannot live wherever the application
//! keeps everything else. `$XDG_RUNTIME_DIR` is the right home and is normally
//! `/run/user/<uid>`, which is short.
//!
//! # Without a usable runtime directory
//!
//! The socket moves to a `run-<hostname>` directory inside this account's state
//! directory, which no other account can create entries in. It is named for the
//! machine because a home shared between machines would otherwise show one
//! machine's socket to the others, which would take it for stale and remove it.
//! A state directory on a filesystem that cannot hold Unix sockets is not
//! detected: the launcher reports that it could not bind and runs without
//! handover.
//!
//! Failing that, the socket uses `/tmp/evx-<uid>`, the short directory
//! `ephervex-server`'s control sockets use. That name is predictable in a
//! directory every account can write to, so another account can occupy it, and
//! can free it again at any moment. It is therefore only a last resort: if it
//! were tried before the state directory, an account that occupied and then
//! freed the name could make later launchers choose a different directory than
//! the running one.
//!
//! The server's per-run random directory cannot be used here, because a second
//! launcher has to find the running one without being told where it is. Each
//! candidate is only examined when every earlier one is unusable.

use std::io::{BufRead, BufReader, Read as _, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, Mode, OFlags, flock};
use tracing::{debug, info, warn};

use crate::error::{CoreError, CoreResult};
use crate::pack_url::MAX_LAUNCHER_URL_BYTES;

/// The longest a `sun_path` may be, including its terminator.
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 107;

/// How many times a claim re-reads the name before giving up on it settling.
const MAX_CLAIM_ATTEMPTS: u32 = 4;

/// What starting up found.
#[derive(Debug)]
pub enum Startup {
    /// Nobody else was running. This process owns the name until the returned
    /// value is dropped, or until [`ListeningInstance::release`] gives it up.
    Primary(Box<PrimaryInstance>),
    /// A launcher was already running and has been handed the message. This
    /// process has nothing left to do and should exit successfully.
    HandedOff,
}

/// What a later launch asked the running launcher to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handover {
    /// The launch named nothing to act on. The running launcher should bring its
    /// window forward.
    Raise,
    /// The launch carried a launcher link. The running launcher should bring its
    /// window forward and act on the link, which has not been validated.
    Link(String),
}

/// Which file a socket name pointed at when this process bound it.
///
/// Giving a name up removes only the file that was bound, so a name another
/// launcher has since taken is never removed out from under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

impl SocketIdentity {
    fn of(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// An exclusive lock on the launcher name, held while it is claimed, taken
/// over, or given up.
#[derive(Debug)]
struct NameLock {
    _file: OwnedFd,
}

impl NameLock {
    /// Waits for the lock beside `socket`. Holders keep it for a few system
    /// calls, so the wait is short.
    fn acquire(socket: &Path) -> CoreResult<Self> {
        let path = socket.with_extension("lock");
        let unavailable =
            |action: &str, error: &dyn std::fmt::Display| CoreError::SingleInstanceUnavailable {
                detail: format!("{} could not be {action}: {error}", path.display()),
            };
        let file = rustix::fs::open(
            &path,
            OFlags::CREATE | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| unavailable("opened", &error))?;
        flock(&file, FlockOperation::LockExclusive)
            .map_err(|error| unavailable("locked", &error))?;
        Ok(Self { _file: file })
    }
}

/// Removes `path` if it is still the socket file `identity` names.
fn remove_if_owned(path: &Path, identity: SocketIdentity) -> std::io::Result<()> {
    let _lock =
        NameLock::acquire(path).map_err(|error| std::io::Error::other(error.to_string()))?;
    match SocketIdentity::of(path) {
        Ok(current) if current == identity => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The socket this process owns, and the file behind it.
///
/// Dropping it removes the file if it is still ours. That is a courtesy rather
/// than a guarantee: a process that dies without unwinding leaves the name
/// behind, which is exactly the stale case [`start`] already handles.
#[derive(Debug)]
pub struct PrimaryInstance {
    listener: UnixListener,
    path: PathBuf,
    identity: SocketIdentity,
}

/// The launcher name a listening instance still holds.
#[derive(Debug)]
pub struct ListeningInstance {
    path: PathBuf,
    identity: SocketIdentity,
}

impl ListeningInstance {
    /// Gives the name up, so a launch from now on starts a launcher of its own
    /// instead of handing over to this one. For a launcher that is quitting but
    /// has no window to show for the time that takes.
    pub fn release(self) {
        if let Err(error) = remove_if_owned(&self.path, self.identity) {
            warn!(path = %self.path.display(), %error, "could not release the launcher socket");
        }
    }
}

impl PrimaryInstance {
    fn new(listener: UnixListener, path: PathBuf) -> CoreResult<Self> {
        let identity =
            SocketIdentity::of(&path).map_err(|error| CoreError::SingleInstanceUnavailable {
                detail: format!(
                    "{} could not be read after binding: {error}",
                    path.display()
                ),
            })?;
        Ok(Self {
            listener,
            path,
            identity,
        })
    }

    /// Where the socket is, for logs and tests.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Answers later arrivals, calling `deliver` for each handover.
    ///
    /// Runs on a thread of its own that keeps answering until the process ends,
    /// because a launcher that stopped listening would silently start opening
    /// second windows again. The callback is invoked off the UI thread, so a
    /// frontend hands the message to its event loop rather than touching
    /// widgets here. A launch with no link is delivered as [`Handover::Raise`],
    /// so a frontend can bring its window forward for it.
    ///
    /// Returns the means of giving the name up, or `None` when the thread could
    /// not start: the name is then already released, and is no longer ours to
    /// give up.
    pub fn listen(self, deliver: impl Fn(Handover) + Send + 'static) -> Option<ListeningInstance> {
        let listening = ListeningInstance {
            path: self.path.clone(),
            identity: self.identity,
        };
        std::thread::Builder::new()
            .name("ephervex-single-instance".to_owned())
            .spawn(move || {
                for connection in self.listener.incoming() {
                    match connection {
                        Ok(stream) => {
                            if let Some(handover) = read_handover(stream) {
                                debug!(?handover, "a second launcher handed over");
                                deliver(handover);
                            }
                        }
                        // One refused connection is not a reason to stop
                        // answering the rest.
                        Err(error) => warn!(%error, "could not accept a handover"),
                    }
                }
            })
            .map_err(|error| warn!(%error, "could not answer later launches"))
            .ok()
            .map(|_handle| listening)
    }
}

impl Drop for PrimaryInstance {
    fn drop(&mut self) {
        if let Err(error) = remove_if_owned(&self.path, self.identity) {
            warn!(path = %self.path.display(), %error, "could not remove the launcher socket");
        }
    }
}

/// Claims the launcher name, or hands `message` to whoever already holds it.
///
/// `message` is what a running instance should act on, normally a launcher
/// link. `None` still hands over and reaches the running instance as
/// [`Handover::Raise`], so a second launch with no link raises the existing
/// window rather than opening a new one.
pub fn start(message: Option<&str>) -> CoreResult<Startup> {
    start_at(&socket_path()?, message)
}

fn start_at(path: &Path, message: Option<&str>) -> CoreResult<Startup> {
    let lock = NameLock::acquire(path)?;
    match claim_name(path)? {
        Claim::Bound(listener) => {
            info!(path = %path.display(), "this process is the launcher");
            Ok(Startup::Primary(Box::new(PrimaryInstance::new(
                listener,
                path.to_owned(),
            )?)))
        }
        Claim::Running(stream) => {
            drop(lock);
            hand_over(stream, message.unwrap_or_default(), path)?;
            Ok(Startup::HandedOff)
        }
    }
}

enum Claim {
    Bound(UnixListener),
    Running(UnixStream),
}

/// Binds the name, or connects to whoever holds it. Called with the name lock
/// held.
fn claim_name(path: &Path) -> CoreResult<Claim> {
    let unavailable =
        |action: &str, error: &dyn std::fmt::Display| CoreError::SingleInstanceUnavailable {
            detail: format!("{} could not be {action}: {error}", path.display()),
        };

    for _ in 0..MAX_CLAIM_ATTEMPTS {
        match UnixListener::bind(path) {
            Ok(listener) => return Ok(Claim::Bound(listener)),
            Err(error) if error.kind() != std::io::ErrorKind::AddrInUse => {
                return Err(unavailable("bound", &error));
            }
            Err(_) => {}
        }

        // The name is taken. Either somebody is listening, or the last owner
        // died without cleaning up.
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(Claim::Running(stream)),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                debug!(
                    path = %path.display(),
                    "the launcher socket is stale; taking it over"
                );
                remove_stale(path).map_err(|error| unavailable("cleared", &error))?;
            }
            // Gone between the bind and the connect; bind again.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(unavailable("reached", &error)),
        }
    }

    Err(unavailable("claimed", &"it kept changing hands"))
}

/// Removes a name whose connection was refused, if it is a socket of ours.
///
/// Connecting to a file that is not a socket is also refused, so a refusal
/// alone does not make a path safe to delete.
fn remove_stale(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != uid_of_self()? {
        return Err(std::io::Error::other("it is not a socket of ours"));
    }
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn hand_over(mut stream: UnixStream, message: &str, path: &Path) -> CoreResult<()> {
    // One line, because one message is all a launch has to say. Newline
    // terminated so the reader knows where it ends without a length prefix.
    let payload = format!("{}\n", message.replace('\n', " "));
    stream
        .write_all(payload.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| CoreError::SingleInstanceUnavailable {
            detail: format!("{} did not take the handover: {error}", path.display()),
        })
}

/// Reads one bounded line as a handover, or nothing when the connection could
/// not be read.
///
/// A blank line is a launch with nothing to say, which still asks for the
/// window to be raised.
///
/// Bounded because the message originates from a web page: an unbounded read
/// from a socket anyone on this account can connect to is a memory sink.
fn read_handover(stream: UnixStream) -> Option<Handover> {
    let mut reader = BufReader::new(stream).take(MAX_LAUNCHER_URL_BYTES as u64 + 1);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let trimmed = line.trim();
    Some(if trimmed.is_empty() {
        Handover::Raise
    } else {
        Handover::Link(trimmed.to_owned())
    })
}

/// Where the launcher socket lives.
fn socket_path() -> CoreResult<PathBuf> {
    let name = format!("ephervex-{}.sock", uid()?);
    SocketDirectories::from_environment().socket(&name)
}

/// Makes a fallback socket directory usable, or says why it is not.
type PrepareDirectory = fn(&Path) -> std::io::Result<()>;

/// The places the launcher socket may live, best first.
///
/// `$XDG_RUNTIME_DIR` is private to the account by definition, and so is the
/// `run` directory in the state directory, because only this account can create
/// anything in its home. The `evx-<uid>` directory in the temporary directory
/// has a predictable name in a directory everybody can write to, so another
/// account can occupy or free it at any time; it is tried last. A candidate is
/// looked at only when every earlier one is unusable.
///
/// Nothing another account does changes which candidate wins, so every launcher
/// of an account in the same environment agrees on where the socket is.
#[derive(Debug, Clone)]
struct SocketDirectories {
    runtime: Option<PathBuf>,
    temporary: PathBuf,
    state: Option<PathBuf>,
}

impl SocketDirectories {
    fn from_environment() -> Self {
        Self {
            runtime: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            temporary: PathBuf::from("/tmp"),
            state: host_local_state_directory(),
        }
    }

    fn socket(&self, name: &str) -> CoreResult<PathBuf> {
        let uid = uid()?;
        let mut skipped = Vec::new();

        if let Some(runtime) = &self.runtime {
            if runtime.is_absolute() && is_private_directory(runtime) {
                match socket_in(runtime, name) {
                    Ok(path) => return Ok(path),
                    Err(reason) => skipped.push(reason),
                }
            } else {
                debug!(
                    path = %runtime.display(),
                    "$XDG_RUNTIME_DIR is not a private directory of ours; using the fallback"
                );
            }
        }

        let mut fallbacks: Vec<(PathBuf, PrepareDirectory)> = Vec::new();
        if let Some(state) = &self.state {
            fallbacks.push((state.clone(), prepare_state_directory));
        }
        fallbacks.push((
            self.temporary.join(format!("evx-{uid}")),
            prepare_private_directory,
        ));
        for (directory, prepare) in fallbacks {
            match prepare(&directory).map_err(|error| error.to_string()) {
                Ok(()) => match socket_in(&directory, name) {
                    Ok(path) => return Ok(path),
                    Err(reason) => skipped.push(reason),
                },
                Err(error) => {
                    debug!(path = %directory.display(), %error, "this socket directory is not usable");
                    skipped.push(format!("{} is not usable: {error}", directory.display()));
                }
            }
        }

        Err(CoreError::SingleInstanceUnavailable {
            detail: if skipped.is_empty() {
                "no usable directory for the launcher socket".to_owned()
            } else {
                skipped.join("; ")
            },
        })
    }
}

/// The socket's path in `directory`, or why it cannot be one.
fn socket_in(directory: &Path, name: &str) -> Result<PathBuf, String> {
    let path = directory.join(name);
    if path.as_os_str().as_encoded_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES {
        Ok(path)
    } else {
        Err(format!(
            "{} is longer than a Unix socket path may be",
            path.display()
        ))
    }
}

/// The socket directory inside the state directory, named for this machine.
///
/// The state directory can be on a home shared between machines, where a socket
/// file is visible to all of them but only listens on the one that bound it. A
/// launcher on another machine would then take the name for stale and remove
/// it. Naming the directory for the machine gives each its own socket.
fn host_local_state_directory() -> Option<PathBuf> {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    let paths = ephervex_storage::Paths::from_environment().ok()?;
    Some(paths.state_dir().join(host_directory_name(&hostname)?))
}

/// `run-<hostname>`, or nothing when the host name leaves no usable name.
///
/// Anything but ASCII letters, digits, `.`, `-` and `_` becomes `_`, so a host
/// name cannot add a path component or an unreadable name.
fn host_directory_name(hostname: &str) -> Option<String> {
    let hostname = hostname.trim();
    if hostname.is_empty() {
        return None;
    }
    let safe: String = hostname
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    Some(format!("run-{safe}"))
}

/// Makes `directory` a mode-0700 directory of ours, creating it when missing.
///
/// Anything else at that name, including a symlink, is refused rather than
/// followed or reused.
fn prepare_private_directory(directory: &Path) -> std::io::Result<()> {
    // A launcher of this account, or the server, may create the directory
    // between the look and the creation. That is not a reason to go elsewhere,
    // because the other launchers would then disagree about where the socket is:
    // look again and judge what is there.
    for _ in 0..2 {
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) if metadata.file_type().is_dir() && metadata.uid() == uid_of_self()? => {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
                break;
            }
            Ok(_) => return Err(std::io::Error::other("it is not a directory of ours")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::DirBuilder::new().mode(0o700).create(directory) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
    if is_private_directory(directory) {
        Ok(())
    } else {
        Err(std::io::Error::other("it is not private to this account"))
    }
}

/// [`prepare_private_directory`] for a directory inside the account's own
/// state directory, creating the parents it needs.
fn prepare_state_directory(directory: &Path) -> std::io::Result<()> {
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent)?;
    }
    prepare_private_directory(directory)
}

/// Whether a directory is ours and closed to everybody else.
///
/// A socket in a world-writable directory is one anybody on the machine can
/// replace, which would let them receive links meant for this account.
fn is_private_directory(path: &std::path::Path) -> bool {
    let (Ok(metadata), Ok(uid)) = (std::fs::symlink_metadata(path), uid()) else {
        return false;
    };
    let others_have_access = metadata.mode() & 0o077 != 0;
    metadata.file_type().is_dir() && metadata.uid() == uid && !others_have_access
}

fn uid_of_self() -> std::io::Result<u32> {
    std::fs::metadata("/proc/self").map(|metadata| metadata.uid())
}

fn uid() -> CoreResult<u32> {
    uid_of_self().map_err(|error| CoreError::SingleInstanceUnavailable {
        detail: format!("this process cannot read its own identity: {error}"),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Two launches in one test, against a socket of this test's own.
    ///
    /// `start` reads the environment for the directory, so a test that used it
    /// directly would fight every other test for one name. These exercise the
    /// same bind/connect/stale sequence against an explicit path.
    fn claim(path: &std::path::Path) -> std::io::Result<UnixListener> {
        UnixListener::bind(path)
    }

    #[test]
    fn the_first_launch_owns_the_name_and_the_second_finds_it_taken() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");

        let _first = claim(&path).expect("the first launch binds");
        let second = claim(&path).expect_err("the second cannot");

        assert_eq!(
            second.kind(),
            std::io::ErrorKind::AddrInUse,
            "a taken name is how a running launcher is found"
        );
        assert!(
            UnixStream::connect(&path).is_ok(),
            "and the owner answers, which is what makes a handover possible"
        );
    }

    /// The normal state after a crash or a reboot that left the file alone.
    #[test]
    fn a_socket_nobody_answers_is_taken_over_rather_than_reported() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");

        let listener = claim(&path).expect("bind");
        drop(listener);
        // The file may survive the listener; what matters is that connecting
        // to it fails, which is the signal to clear it.
        if path.exists() {
            assert!(
                UnixStream::connect(&path).is_err(),
                "a socket with no listener must not appear to answer"
            );
            std::fs::remove_file(&path).expect("clear the stale name");
        }

        assert!(claim(&path).is_ok(), "the name is free again");
    }

    #[test]
    fn a_handed_over_message_arrives_whole() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let primary =
            PrimaryInstance::new(claim(&path).expect("bind"), path.clone()).expect("identity");

        let (sender, receiver) = mpsc::channel();
        primary.listen(move |message| {
            let _ = sender.send(message);
        });

        let link = "ephervex://import?url=https%3A%2F%2Fexample.com%2Fpack.mrpack";
        hand_over(UnixStream::connect(&path).expect("connect"), link, &path).expect("hand over");

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("the running launcher is handed the link"),
            Handover::Link(link.to_owned())
        );
    }

    /// A launcher that is quitting gives the name up, so the next launch opens a
    /// window instead of handing over to a process that has none to raise.
    #[test]
    fn a_released_name_is_taken_by_the_next_launch() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let primary =
            PrimaryInstance::new(claim(&path).expect("bind"), path.clone()).expect("identity");

        let listening = primary.listen(|_| {}).expect("the listener starts");
        assert_eq!(
            claim(&path).expect_err("taken while listening").kind(),
            std::io::ErrorKind::AddrInUse
        );
        listening.release();

        assert!(claim(&path).is_ok(), "the name is free for the next launch");
    }

    /// A second launch with no link still hands over, so the running launcher
    /// is told to raise its window rather than a second one opening.
    #[test]
    fn a_launch_with_nothing_to_say_asks_the_running_launcher_to_raise() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let primary =
            PrimaryInstance::new(claim(&path).expect("bind"), path.clone()).expect("identity");

        let (sender, receiver) = mpsc::channel();
        primary.listen(move |handover| {
            let _ = sender.send(handover);
        });

        hand_over(UnixStream::connect(&path).expect("connect"), "", &path).expect("hand over");
        hand_over(
            UnixStream::connect(&path).expect("connect"),
            "second",
            &path,
        )
        .expect("hand over");

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("the empty handover is delivered"),
            Handover::Raise,
            "an empty handover raises the window"
        );
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("the link that followed"),
            Handover::Link("second".to_owned()),
            "and does not swallow the handover after it"
        );
    }

    #[test]
    fn a_handover_of_only_whitespace_asks_the_running_launcher_to_raise() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let listener = claim(&path).expect("bind");

        hand_over(UnixStream::connect(&path).expect("connect"), "  \t", &path).expect("hand over");
        let (stream, _) = listener.accept().expect("accept");

        assert_eq!(read_handover(stream), Some(Handover::Raise));
    }

    /// The message comes from a web page, so the read is bounded.
    #[test]
    fn an_oversized_handover_cannot_grow_without_bound() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let listener = claim(&path).expect("bind");

        let sender = std::thread::spawn({
            let path = path.clone();
            move || {
                if let Ok(mut stream) = UnixStream::connect(&path) {
                    let _ = stream.write_all(&vec![b'a'; MAX_LAUNCHER_URL_BYTES * 4]);
                }
            }
        });

        let (stream, _) = listener.accept().expect("accept");
        let message = match read_handover(stream) {
            Some(Handover::Link(message)) => message,
            _ => String::new(),
        };
        let _ = sender.join();

        assert!(
            message.len() <= MAX_LAUNCHER_URL_BYTES + 1,
            "read {} bytes from a socket anyone on this account can write to",
            message.len()
        );
    }

    #[test]
    fn dropping_the_owner_releases_the_name() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let primary =
            PrimaryInstance::new(claim(&path).expect("bind"), path.clone()).expect("identity");
        assert!(path.exists());

        drop(primary);

        assert!(!path.exists(), "a clean exit leaves no name behind");
    }

    const SOCKET_NAME: &str = "ephervex-1000.sock";

    fn private_directory(path: &Path) -> PathBuf {
        std::fs::create_dir_all(path).expect("mkdir");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        path.to_owned()
    }

    fn shared_name() -> String {
        format!("evx-{}", uid().expect("uid"))
    }

    /// A runtime directory long enough to push the socket past `sun_path` costs
    /// the handover entirely unless something else is tried, and a second
    /// window is a worse answer than a socket somewhere equally private.
    #[test]
    fn a_runtime_directory_too_long_for_a_socket_falls_back_rather_than_giving_up() {
        let base = tempfile::tempdir().expect("temp dir");
        let long = private_directory(&base.path().join("d".repeat(MAX_UNIX_SOCKET_PATH_BYTES)));
        let directories = SocketDirectories {
            runtime: Some(long),
            temporary: base.path().to_owned(),
            state: None,
        };

        let path = directories.socket(SOCKET_NAME).expect("a fallback is used");

        assert_eq!(
            path,
            base.path().join(shared_name()).join(SOCKET_NAME),
            "the short shared directory is next"
        );
    }

    /// Another account can create the predictable name first. A launcher that
    /// has a private runtime directory must not even look at it.
    #[test]
    fn an_occupied_shared_name_does_not_affect_a_usable_runtime_directory() {
        let base = tempfile::tempdir().expect("temp dir");
        let runtime = private_directory(&base.path().join("runtime"));
        let temporary = base.path().join("tmp");
        std::fs::create_dir(&temporary).expect("mkdir");
        let attacker = tempfile::tempdir().expect("attacker dir");
        std::os::unix::fs::symlink(attacker.path(), temporary.join(shared_name()))
            .expect("symlink");
        let directories = SocketDirectories {
            runtime: Some(runtime.clone()),
            temporary,
            state: None,
        };

        assert_eq!(
            directories.socket(SOCKET_NAME).expect("socket path"),
            runtime.join(SOCKET_NAME)
        );
    }

    /// Another account can occupy the shared name and free it again at will,
    /// so nothing about it may change where a running launcher is looked for.
    #[test]
    fn occupying_and_freeing_the_shared_name_never_changes_where_the_socket_is() {
        let base = tempfile::tempdir().expect("temp dir");
        let temporary = base.path().join("tmp");
        std::fs::create_dir(&temporary).expect("mkdir");
        let attacker = tempfile::tempdir().expect("attacker dir");
        std::fs::set_permissions(attacker.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let occupied = temporary.join(shared_name());
        let state = base.path().join("state").join("run");
        let directories = SocketDirectories {
            runtime: None,
            temporary,
            state: Some(state.clone()),
        };

        let before = directories.socket(SOCKET_NAME).expect("socket path");
        std::os::unix::fs::symlink(attacker.path(), &occupied).expect("symlink");
        let occupied_path = directories.socket(SOCKET_NAME).expect("socket path");
        std::fs::remove_file(&occupied).expect("free the name");
        let freed = directories.socket(SOCKET_NAME).expect("socket path");

        assert_eq!(before, state.join(SOCKET_NAME));
        assert_eq!(occupied_path, before, "launchers of one account must agree");
        assert_eq!(freed, before, "also once the name is free again");
        assert!(is_private_directory(&state));
        assert!(
            std::fs::symlink_metadata(&occupied).is_err(),
            "the shared name is never created while a better place exists"
        );
        assert_eq!(
            attacker.path().metadata().expect("metadata").mode() & 0o777,
            0o755,
            "the directory the symlink points at is left alone"
        );
    }

    /// When the state directory cannot be used, the shared name is next, and an
    /// occupied one must neither be followed nor stop the launcher.
    #[test]
    fn an_unusable_state_directory_falls_back_to_the_shared_name() {
        let base = tempfile::tempdir().expect("temp dir");
        let elsewhere = private_directory(&base.path().join("elsewhere"));
        let state = base.path().join("state");
        std::fs::create_dir(&state).expect("mkdir");
        std::os::unix::fs::symlink(&elsewhere, state.join("run")).expect("symlink");
        let directories = SocketDirectories {
            runtime: None,
            temporary: base.path().to_owned(),
            state: Some(state.join("run")),
        };

        assert_eq!(
            directories.socket(SOCKET_NAME).expect("socket path"),
            base.path().join(shared_name()).join(SOCKET_NAME)
        );
    }

    /// Two launches, or a launch and the server, can create the same directory
    /// at once. The loser must use it rather than go somewhere the winner does
    /// not look.
    #[test]
    fn launches_creating_the_same_directory_at_once_all_use_it() {
        const LAUNCHES: usize = 8;
        let base = tempfile::tempdir().expect("temp dir");
        let directory = base.path().join(shared_name());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(LAUNCHES));
        let launches: Vec<_> = (0..LAUNCHES)
            .map(|_| {
                let (directory, barrier) = (directory.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    prepare_private_directory(&directory)
                })
            })
            .collect();

        for launch in launches {
            launch
                .join()
                .expect("launch thread")
                .expect("a directory of ours that appeared meanwhile is used");
        }
        assert!(is_private_directory(&directory));
    }

    #[test]
    fn an_occupied_shared_name_with_nowhere_else_reports_why() {
        let base = tempfile::tempdir().expect("temp dir");
        let temporary = base.path().join("tmp");
        std::fs::create_dir(&temporary).expect("mkdir");
        std::fs::write(temporary.join(shared_name()), b"").expect("occupy");
        let directories = SocketDirectories {
            runtime: None,
            temporary,
            state: None,
        };

        let Err(CoreError::SingleInstanceUnavailable { detail }) = directories.socket(SOCKET_NAME)
        else {
            panic!("an occupied name is not a socket directory");
        };
        assert!(detail.contains(&shared_name()), "{detail}");
    }

    #[test]
    fn a_host_name_becomes_one_safe_directory_name() {
        assert_eq!(
            host_directory_name("work-pc.example_1\n").as_deref(),
            Some("run-work-pc.example_1")
        );
        assert_eq!(
            host_directory_name("a/../b c").as_deref(),
            Some("run-a_.._b_c")
        );
        assert_eq!(host_directory_name("  \n"), None);
    }

    #[test]
    fn a_state_socket_directory_that_is_a_symlink_is_not_used() {
        let base = tempfile::tempdir().expect("temp dir");
        let temporary = base.path().join("tmp");
        std::fs::create_dir(&temporary).expect("mkdir");
        std::fs::write(temporary.join(shared_name()), b"").expect("occupy");
        let elsewhere = private_directory(&base.path().join("elsewhere"));
        let state = base.path().join("state");
        std::fs::create_dir(&state).expect("mkdir");
        std::os::unix::fs::symlink(&elsewhere, state.join("run")).expect("symlink");
        let directories = SocketDirectories {
            runtime: None,
            temporary,
            state: Some(state.join("run")),
        };

        assert!(matches!(
            directories.socket(SOCKET_NAME),
            Err(CoreError::SingleInstanceUnavailable { .. })
        ));
    }

    /// An existing shared directory of ours that others can enter is tightened
    /// rather than trusted.
    #[test]
    fn an_existing_shared_directory_of_ours_is_made_private() {
        let base = tempfile::tempdir().expect("temp dir");
        let shared = base.path().join(shared_name());
        std::fs::create_dir(&shared).expect("mkdir");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let directories = SocketDirectories {
            runtime: None,
            temporary: base.path().to_owned(),
            state: None,
        };

        directories.socket(SOCKET_NAME).expect("socket path");

        assert!(is_private_directory(&shared));
    }

    /// A socket in a directory others can write to is one they can replace, so
    /// links meant for this account would go to them.
    #[test]
    fn a_directory_others_can_write_to_is_not_used() {
        let base = tempfile::tempdir().expect("temp dir");
        let shared = base.path().join("shared");
        std::fs::create_dir(&shared).expect("mkdir");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).expect("chmod");

        assert!(!is_private_directory(&shared));

        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert!(is_private_directory(&shared));
    }

    fn leave_stale_socket(path: &Path) {
        // Dropping a std listener leaves its file behind, as a crash does.
        drop(claim(path).expect("bind"));
        assert_eq!(
            UnixStream::connect(path)
                .expect_err("nobody answers")
                .kind(),
            std::io::ErrorKind::ConnectionRefused
        );
    }

    #[test]
    fn concurrent_takeovers_of_a_stale_name_leave_exactly_one_launcher() {
        const LAUNCHES: usize = 8;
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        leave_stale_socket(&path);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(LAUNCHES));
        let launches: Vec<_> = (0..LAUNCHES)
            .map(|_| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    start_at(&path, Some("link")).expect("every launch settles")
                })
            })
            .collect();
        let outcomes: Vec<_> = launches
            .into_iter()
            .map(|launch| launch.join().expect("launch thread"))
            .collect();

        let primaries = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Startup::Primary(_)))
            .count();
        assert_eq!(primaries, 1, "two launchers must never both own the name");
        assert!(
            UnixStream::connect(&path).is_ok(),
            "and the one that does answers"
        );
    }

    #[test]
    fn a_takeover_waits_while_another_launch_holds_the_name_lock() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        leave_stale_socket(&path);

        let lock = NameLock::acquire(&path).expect("lock");
        let (sender, receiver) = mpsc::channel();
        let launch = std::thread::spawn({
            let path = path.clone();
            move || {
                let outcome = start_at(&path, None).expect("start");
                let _ = sender.send(());
                outcome
            }
        });

        assert!(
            receiver.recv_timeout(Duration::from_millis(300)).is_err(),
            "a launch must not touch the name while another holds the lock"
        );
        drop(lock);
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the launch proceeds once the lock is free");
        assert!(matches!(
            launch.join().expect("launch thread"),
            Startup::Primary(_)
        ));
    }

    /// Connecting to a file that is not a socket is refused too.
    #[test]
    fn a_path_that_is_not_a_socket_is_reported_and_left_alone() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        std::fs::write(&path, "not a socket").expect("write");

        assert!(matches!(
            start_at(&path, None),
            Err(CoreError::SingleInstanceUnavailable { .. })
        ));
        assert_eq!(std::fs::read(&path).expect("kept"), b"not a socket");
    }

    /// Only a refused connection proves the owner is gone.
    #[test]
    fn a_connection_failure_other_than_a_refusal_does_not_clear_the_name() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let _listener = claim(&path).expect("bind");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let Err(error) = UnixStream::connect(&path) else {
            // Privileged users connect regardless of the mode.
            return;
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

        assert!(matches!(
            start_at(&path, None),
            Err(CoreError::SingleInstanceUnavailable { .. })
        ));
        assert!(path.exists(), "the socket of a live owner is not removed");
    }

    #[test]
    fn dropping_an_owner_leaves_a_name_another_launcher_has_since_taken() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let first = PrimaryInstance::new(claim(&path).expect("bind"), path.clone()).expect("id");
        std::fs::remove_file(&path).expect("the name is taken away");
        let _second = claim(&path).expect("another launcher binds the name");

        drop(first);

        assert!(
            UnixStream::connect(&path).is_ok(),
            "the other launcher still answers"
        );
    }

    #[test]
    fn releasing_leaves_a_name_another_launcher_has_since_taken() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let first = PrimaryInstance::new(claim(&path).expect("bind"), path.clone()).expect("id");
        let listening = first.listen(|_| {}).expect("the listener starts");
        std::fs::remove_file(&path).expect("the name is taken away");
        let _second = claim(&path).expect("another launcher binds the name");

        listening.release();

        assert!(
            UnixStream::connect(&path).is_ok(),
            "the other launcher still answers"
        );
    }

    #[test]
    fn a_running_launcher_is_handed_the_message_by_a_later_start() {
        let base = tempfile::tempdir().expect("temp dir");
        let path = base.path().join("ephervex.sock");
        let Startup::Primary(primary) = start_at(&path, None).expect("first") else {
            panic!("the first launch owns the name");
        };
        let (sender, receiver) = mpsc::channel();
        let _listening = primary.listen(move |handover| {
            let _ = sender.send(handover);
        });

        assert!(matches!(
            start_at(&path, Some("link")).expect("second"),
            Startup::HandedOff
        ));
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("handed over"),
            Handover::Link("link".to_owned())
        );
    }
}
