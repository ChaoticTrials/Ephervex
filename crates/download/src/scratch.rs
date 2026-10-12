//! Ownership of scratch directories and files, so one a dead process left
//! behind can be collected and one a live process is using never is.
//!
//! A scratch entry is claimed by holding an exclusive `flock` on a marker
//! file next to it, named after it with an `.owner` suffix. The kernel drops the
//! lock when the holder's last descriptor closes, including when the holder is
//! killed or the machine loses power, so liveness is never inferred from a pid.
//! A pid says nothing about a process in another pid namespace or on another
//! host that shares the directory; the lock is visible to all of them.
//!
//! NFS gives a process one lock per file shared by all its descriptors, and
//! closing any of them drops it. A lock alone therefore cannot tell a process
//! its own live scratch directory from an abandoned one, so every marker this
//! process holds or is sweeping is also registered in the process.
//!
//! The marker is created and locked before the entry exists and is removed
//! only after the entry is gone, so a marker without a lock always means the
//! entry (if any) is abandoned. An entry with no marker has no provable owner
//! and is left alone.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing::{debug, warn};

/// Markers this process currently holds or is sweeping, by canonical path.
static HELD_IN_PROCESS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The right to use one marker within this process. Released on drop.
#[derive(Debug)]
struct InProcessClaim {
    key: PathBuf,
}

impl InProcessClaim {
    fn take(key: PathBuf) -> Option<Self> {
        let mut held = HELD_IN_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.contains(&key) {
            return None;
        }
        held.push(key.clone());
        Some(Self { key })
    }
}

impl Drop for InProcessClaim {
    fn drop(&mut self) {
        HELD_IN_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|held| held != &self.key);
    }
}

/// Identifies `marker` independently of how its directory was spelled.
fn claim_key(marker: &Path) -> io::Result<PathBuf> {
    let name = marker
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let parent = marker
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(std::fs::canonicalize(parent)?.join(name))
}

/// Appended to a scratch entry's name to name its marker.
const MARKER_SUFFIX: &str = ".owner";

/// How long a claim waits out a marker locked by a sweep before concluding a
/// running process holds the name.
const CONTENTION_RETRIES: u32 = 10;
const CONTENTION_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);

/// The claim on one scratch entry, held for as long as this value lives.
///
/// Dropping it without [`Self::release`] gives up the lock but leaves the
/// marker, which is how a sweep finds an entry whose removal was cut short.
#[derive(Debug)]
pub struct ScratchOwner {
    lock: Option<File>,
    marker: PathBuf,
    // Declared last so the lock is closed before the in-process claim ends.
    _claim: InProcessClaim,
}

impl Drop for ScratchOwner {
    fn drop(&mut self) {
        self.lock = None;
    }
}

impl ScratchOwner {
    /// Claims `scratch` before it is created.
    ///
    /// Fails with [`io::ErrorKind::WouldBlock`] when a live owner already holds
    /// that name, which can happen when processes in different pid namespaces
    /// share a directory; the caller picks another name rather than clearing
    /// the directory out from under it.
    pub fn claim(scratch: &Path) -> io::Result<Self> {
        let marker = marker_path(scratch)?;
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key = claim_key(&marker)?;
        let mut contended = 0;
        loop {
            let contention = |contended: &mut u32| {
                if *contended < CONTENTION_RETRIES {
                    // A sweep can hold a marker it just found unlocked for a
                    // moment, and removes it if no directory is behind it.
                    *contended += 1;
                    std::thread::sleep(CONTENTION_PAUSE);
                    Ok(())
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "scratch directory name is in use by a running process",
                    ))
                }
            };
            let Some(claim) = InProcessClaim::take(key.clone()) else {
                contention(&mut contended)?;
                continue;
            };
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&marker)?;
            if !try_lock(&file)? {
                drop(file);
                drop(claim);
                contention(&mut contended)?;
                continue;
            }
            // A sweep may have removed the marker between the open and the lock;
            // a lock on an unlinked file protects nothing.
            if names_same_file(&file, &marker) {
                return Ok(Self {
                    lock: Some(file),
                    marker,
                    _claim: claim,
                });
            }
        }
    }

    /// Gives up the claim once the entry has been removed or published.
    pub fn release(self) {
        match std::fs::remove_file(&self.marker) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                debug!(path = %self.marker.display(), %error, "could not remove scratch marker");
            }
        }
    }
}

/// Removes every scratch directory or file in `parent` whose name `is_scratch`
/// accepts and whose owner is gone, with its marker. Returns how many entries
/// or stray markers were collected.
///
/// An entry with no marker, or whose marker is locked, is never touched.
/// Marker names come from the filesystem, so only a name that is a single
/// ordinary path component is ever turned into a path to remove.
pub fn sweep_abandoned(parent: &Path, is_scratch: impl Fn(&str) -> bool) -> usize {
    if !cfg!(unix) {
        // Without advisory locking an owner cannot be proven gone.
        return 0;
    }
    let (Ok(entries), Ok(parent_key)) = (std::fs::read_dir(parent), std::fs::canonicalize(parent))
    else {
        return 0;
    };
    let mut collected = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(scratch_name) = name
            .to_str()
            .and_then(|name| name.strip_suffix(MARKER_SUFFIX))
            .filter(|scratch_name| is_single_component(scratch_name) && is_scratch(scratch_name))
        else {
            continue;
        };
        // One this process holds, or is already sweeping, is live whatever the
        // lock says.
        let Some(claim) = InProcessClaim::take(parent_key.join(&name)) else {
            continue;
        };
        if reclaim(&entry.path(), &parent.join(scratch_name)) {
            collected += 1;
        }
        drop(claim);
    }
    collected
}

fn is_single_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

fn reclaim(marker: &Path, scratch: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(marker) else {
        return false;
    };
    if !matches!(try_lock(&file), Ok(true)) || !names_same_file(&file, marker) {
        return false;
    }
    match remove_entry(scratch) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(
                path = %scratch.display(),
                %error,
                "could not remove an abandoned scratch entry"
            );
            return false;
        }
    }
    // Still holding the lock, so a claim that opened this marker sees it
    // locked or sees it replaced, never both removed and held.
    let removed = std::fs::remove_file(marker).is_ok();
    if removed {
        debug!(path = %scratch.display(), "collected an abandoned scratch entry");
    }
    removed
}

/// Removes a scratch directory with its contents, or a scratch file or link.
fn remove_entry(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// The marker that claims `scratch`, which exists exactly while a claim on it
/// is held or its collection is pending.
pub fn marker_path(scratch: &Path) -> io::Result<PathBuf> {
    let name = scratch
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut marker = name.to_os_string();
    marker.push(MARKER_SUFFIX);
    Ok(scratch.with_file_name(marker))
}

/// `Ok(false)` when another descriptor holds the lock.
#[cfg(unix)]
fn try_lock(file: &File) -> io::Result<bool> {
    use rustix::fs::{FlockOperation, flock};

    loop {
        match flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(true),
            Err(rustix::io::Errno::INTR) => {}
            Err(errno)
                if errno == rustix::io::Errno::WOULDBLOCK || errno == rustix::io::Errno::AGAIN =>
            {
                return Ok(false);
            }
            Err(errno) => return Err(errno.into()),
        }
    }
}

/// Without advisory locking a claim succeeds without excluding anyone, and
/// [`sweep_abandoned`] collects nothing.
#[cfg(not(unix))]
fn try_lock(_file: &File) -> io::Result<bool> {
    Ok(true)
}

#[cfg(unix)]
fn names_same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(held), Ok(named)) => held.dev() == named.dev() && held.ino() == named.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn names_same_file(_file: &File, _path: &Path) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn scratch_in(root: &Path, name: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir(&path).expect("create scratch");
        std::fs::write(path.join("partial"), b"x").expect("write partial");
        path
    }

    #[test]
    fn a_directory_whose_owner_is_gone_is_collected_with_its_marker() {
        let root = tempfile::tempdir().expect("temp dir");
        let owner = ScratchOwner::claim(&root.path().join("client-7-0")).expect("claim");
        let scratch = scratch_in(root.path(), "client-7-0");
        drop(owner);

        assert_eq!(sweep_abandoned(root.path(), |_| true), 1);

        assert!(!scratch.exists());
        assert!(!root.path().join("client-7-0.owner").exists());
    }

    #[test]
    fn a_file_whose_owner_is_gone_is_collected_with_its_marker() {
        let root = tempfile::tempdir().expect("temp dir");
        let owner = ScratchOwner::claim(&root.path().join("client-7-0")).expect("claim");
        let scratch = root.path().join("client-7-0");
        std::fs::write(&scratch, b"partial").expect("write partial");
        drop(owner);

        assert_eq!(sweep_abandoned(root.path(), |_| true), 1);

        assert!(!scratch.exists());
        assert!(!root.path().join("client-7-0.owner").exists());
    }

    #[test]
    fn a_directory_a_live_owner_holds_is_never_collected() {
        let root = tempfile::tempdir().expect("temp dir");
        let owner = ScratchOwner::claim(&root.path().join("client-7-0")).expect("claim");
        let scratch = scratch_in(root.path(), "client-7-0");

        assert_eq!(sweep_abandoned(root.path(), |_| true), 0);

        assert!(scratch.join("partial").is_file());
        drop(owner);
    }

    #[test]
    fn a_name_a_live_owner_holds_cannot_be_claimed_again() {
        let root = tempfile::tempdir().expect("temp dir");
        let scratch = root.path().join("client-7-0");
        let owner = ScratchOwner::claim(&scratch).expect("claim");

        let error = ScratchOwner::claim(&scratch).expect_err("name is held");

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(owner);
        ScratchOwner::claim(&scratch).expect("claim after the owner is gone");
    }

    #[test]
    fn a_directory_without_a_marker_has_no_provable_owner_and_is_left_alone() {
        let root = tempfile::tempdir().expect("temp dir");
        let scratch = scratch_in(root.path(), "client-7-0");

        assert_eq!(sweep_abandoned(root.path(), |_| true), 0);

        assert!(scratch.join("partial").is_file());
    }

    #[test]
    fn a_directory_the_caller_does_not_recognize_is_left_alone() {
        let root = tempfile::tempdir().expect("temp dir");
        drop(ScratchOwner::claim(&root.path().join("other")).expect("claim"));
        let scratch = scratch_in(root.path(), "other");

        assert_eq!(
            sweep_abandoned(root.path(), |name| name.starts_with("client-")),
            0
        );

        assert!(scratch.join("partial").is_file());
    }

    #[test]
    fn a_marker_whose_directory_never_appeared_is_collected() {
        let root = tempfile::tempdir().expect("temp dir");
        drop(ScratchOwner::claim(&root.path().join("client-7-0")).expect("claim"));

        assert_eq!(sweep_abandoned(root.path(), |_| true), 1);

        assert!(!root.path().join("client-7-0.owner").exists());
    }

    #[test]
    fn a_released_claim_leaves_no_marker_behind() {
        let root = tempfile::tempdir().expect("temp dir");
        ScratchOwner::claim(&root.path().join("client-7-0"))
            .expect("claim")
            .release();

        assert!(!root.path().join("client-7-0.owner").exists());
    }

    #[test]
    fn a_marker_named_for_a_parent_directory_never_removes_anything() {
        let root = tempfile::tempdir().expect("temp dir");
        let work = root.path().join("work");
        std::fs::create_dir(&work).expect("work");
        let sibling = scratch_in(root.path(), "server");
        let live = scratch_in(&work, "client-1-0");
        for marker in ["..owner", "...owner"] {
            std::fs::write(work.join(marker), b"").expect("marker");
        }

        assert_eq!(sweep_abandoned(&work, |_| true), 0);

        assert!(live.join("partial").is_file());
        assert!(sibling.join("partial").is_file());
    }

    #[test]
    fn a_sweep_never_reclaims_a_marker_this_process_holds_even_when_its_lock_is_granted() {
        // NFS grants a process's second descriptor the lock its first holds, so
        // the process's own claims, not the lock, keep it off its live scratch.
        let root = tempfile::tempdir().expect("temp dir");
        let scratch = scratch_in(root.path(), "client-7-0");
        let marker = root.path().join("client-7-0.owner");
        std::fs::write(&marker, b"").expect("unlocked marker");
        let held = InProcessClaim::take(claim_key(&marker).expect("key")).expect("claim");

        assert_eq!(sweep_abandoned(root.path(), |_| true), 0);

        assert!(scratch.join("partial").is_file());
        drop(held);
        assert_eq!(sweep_abandoned(root.path(), |_| true), 1);
        assert!(!scratch.exists());
    }

    #[test]
    fn a_name_this_process_holds_through_another_spelling_cannot_be_claimed() {
        let root = tempfile::tempdir().expect("temp dir");
        let owner = ScratchOwner::claim(&root.path().join("client-7-0")).expect("claim");
        let respelled = root.path().join(".").join("client-7-0");

        let error = ScratchOwner::claim(&respelled).expect_err("name is held");

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(owner);
    }
}
