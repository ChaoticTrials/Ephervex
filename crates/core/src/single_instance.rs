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
//! `/run/user/<uid>`, which is short; the fallback is the same short,
//! user-owned `/tmp` directory `ephervex-server`'s control sockets use, for the
//! same reason.

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
///
/// `$XDG_RUNTIME_DIR` first, and the short `/tmp` directory when that does not
/// fit. A runtime directory is normally `/run/user/<uid>` and leaves plenty of
/// room, but it is set from the environment and an unusual one would otherwise
/// cost the handover entirely: a second window is a worse answer than a socket
/// somewhere else that is just as private.
fn socket_path() -> CoreResult<PathBuf> {
    let name = format!("ephervex-{}.sock", uid()?);
    let mut refusal = None;

    for directory in socket_directories()? {
        let path = directory.join(&name);
        if path.as_os_str().as_encoded_bytes().len() <= MAX_UNIX_SOCKET_PATH_BYTES {
            return Ok(path);
        }
        debug!(
            path = %path.display(),
            "this socket path is longer than a Unix socket path may be"
        );
        refusal = Some(path);
    }

    Err(CoreError::SingleInstanceUnavailable {
        detail: refusal.map_or_else(
            || "no usable directory for the launcher socket".to_owned(),
            |path| {
                format!(
                    "{} is longer than a Unix socket path may be",
                    path.display()
                )
            },
        ),
    })
}

/// The directories a launcher socket may live in, best first.
fn socket_directories() -> CoreResult<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let directory = PathBuf::from(runtime);
        if directory.is_absolute() && is_private_directory(&directory) {
            candidates.push(directory);
        } else {
            debug!(
                path = %directory.display(),
                "$XDG_RUNTIME_DIR is not a private directory of ours; using the fallback"
            );
        }
    }
    candidates.push(private_tmp_directory()?);
    Ok(candidates)
}

/// The short, private `/tmp` directory a socket falls back to.
///
/// The same shape `ephervex-server` uses for its control sockets, and for the
/// same reasons: short enough for `sun_path`, owned by this account, and closed
/// to everybody else.
fn private_tmp_directory() -> CoreResult<PathBuf> {
    let fallback = PathBuf::from(format!("/tmp/evx-{}", uid()?));
    match std::fs::symlink_metadata(&fallback) {
        Ok(metadata) if metadata.file_type().is_dir() && metadata.uid() == uid()? => {
            std::fs::set_permissions(&fallback, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| CoreError::SingleInstanceUnavailable {
                    detail: format!("{} could not be secured: {error}", fallback.display()),
                },
            )?;
        }
        Ok(_) => {
            return Err(CoreError::SingleInstanceUnavailable {
                detail: format!("{} is not a directory of ours", fallback.display()),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&fallback)
                .map_err(|error| CoreError::SingleInstanceUnavailable {
                    detail: format!("{} could not be created: {error}", fallback.display()),
                })?;
        }
        Err(error) => {
            return Err(CoreError::SingleInstanceUnavailable {
                detail: format!("{} could not be read: {error}", fallback.display()),
            });
        }
    }
    Ok(fallback)
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

    /// A runtime directory long enough to push the socket past `sun_path` costs
    /// the handover entirely unless something else is tried, and a second
    /// window is a worse answer than a socket somewhere equally private.
    #[test]
    fn a_runtime_directory_too_long_for_a_socket_falls_back_rather_than_giving_up() {
        let long = "/tmp/".to_owned() + &"d".repeat(MAX_UNIX_SOCKET_PATH_BYTES);
        let name = "ephervex-1000.sock";

        assert!(
            std::path::Path::new(&long)
                .join(name)
                .as_os_str()
                .as_encoded_bytes()
                .len()
                > MAX_UNIX_SOCKET_PATH_BYTES,
            "this test's own premise"
        );
        assert!(
            std::path::Path::new("/tmp/evx-1000")
                .join(name)
                .as_os_str()
                .as_encoded_bytes()
                .len()
                <= MAX_UNIX_SOCKET_PATH_BYTES,
            "the fallback has to be short enough to be worth falling back to"
        );
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
