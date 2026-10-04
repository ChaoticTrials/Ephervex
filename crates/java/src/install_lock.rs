//! Serializes installs of one managed release, across processes and within one.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rustix::fs::{FlockOperation, Mode, OFlags, flock};
use rustix::io::Errno;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// How often a waiting install checks whether the lock is free.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The lock files a task of this process currently holds.
static HELD_IN_PROCESS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The right to install one release. Released on drop.
///
/// A `flock`, which the kernel drops if the holder dies, plus a claim within
/// this process, because NFS gives all of a process's descriptors one lock.
#[derive(Debug)]
pub(crate) struct InstallLock {
    // Declared first so the flock is released before the in-process claim.
    file: Option<std::fs::File>,
    _claim: InProcessClaim,
}

#[derive(Debug)]
struct InProcessClaim(PathBuf);

impl InProcessClaim {
    fn try_take(path: &Path) -> Option<Self> {
        let mut held = HELD_IN_PROCESS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if held.iter().any(|claimed| claimed == path) {
            return None;
        }
        held.push(path.to_path_buf());
        Some(Self(path.to_path_buf()))
    }
}

impl Drop for InProcessClaim {
    fn drop(&mut self) {
        HELD_IN_PROCESS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|claimed| *claimed != self.0);
    }
}

impl InstallLock {
    /// Whether another process installing the same release is kept out too.
    pub(crate) const fn excludes_other_processes(&self) -> bool {
        self.file.is_some()
    }

    /// Waits until no other install holds the lock file at `path`.
    ///
    /// `None` when `cancel` fired first. There is no deadline: the holder is
    /// downloading and unpacking a JDK, which takes as long as it takes.
    pub(crate) async fn acquire(
        path: &Path,
        cancel: &CancellationToken,
    ) -> std::io::Result<Option<Self>> {
        let claim = loop {
            if let Some(claim) = InProcessClaim::try_take(path) {
                break claim;
            }
            if canceled_while_waiting(cancel).await {
                return Ok(None);
            }
        };
        let file = {
            let path = path.to_path_buf();
            // Off the runtime: opening on a stalled network filesystem blocks.
            tokio::task::spawn_blocking(move || {
                rustix::fs::open(
                    &path,
                    OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )
            })
            .await
            .map_err(std::io::Error::other)?
            .map(std::fs::File::from)?
        };
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {
                    return Ok(Some(Self {
                        file: Some(file),
                        _claim: claim,
                    }));
                }
                Err(Errno::INTR) => {}
                Err(Errno::WOULDBLOCK) => {
                    if canceled_while_waiting(cancel).await {
                        return Ok(None);
                    }
                }
                // Without locking, installs within this process stay serialized.
                Err(errno @ (Errno::NOLCK | Errno::OPNOTSUPP | Errno::NOSYS)) => {
                    warn!(path = %path.display(), %errno, "managed java installs are not locked across processes here");
                    return Ok(Some(Self {
                        file: None,
                        _claim: claim,
                    }));
                }
                Err(errno) => return Err(errno.into()),
            }
        }
    }
}

/// Waits one poll interval. True when `cancel` fired instead.
async fn canceled_while_waiting(cancel: &CancellationToken) -> bool {
    tokio::select! {
        () = cancel.cancelled() => true,
        () = tokio::time::sleep(POLL_INTERVAL) => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[tokio::test]
    async fn a_second_install_waits_until_the_first_releases_the_lock() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".release.lock");
        let first = InstallLock::acquire(&path, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();

        let waiting = {
            let path = path.clone();
            tokio::spawn(async move {
                InstallLock::acquire(&path, &CancellationToken::new())
                    .await
                    .unwrap()
                    .is_some()
            })
        };
        tokio::time::sleep(POLL_INTERVAL * 3).await;
        assert!(!waiting.is_finished(), "the lock is still held");

        drop(first);
        assert!(waiting.await.unwrap());
    }

    #[tokio::test]
    async fn an_install_waits_for_the_lock_another_process_holds() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".release.lock");
        // A descriptor of its own and no in-process claim: how another process holds it.
        let other = std::fs::File::create(&path).unwrap();
        flock(&other, FlockOperation::NonBlockingLockExclusive).unwrap();

        let waiting = {
            let path = path.clone();
            tokio::spawn(async move {
                InstallLock::acquire(&path, &CancellationToken::new())
                    .await
                    .unwrap()
                    .is_some_and(|lock| lock.excludes_other_processes())
            })
        };
        tokio::time::sleep(POLL_INTERVAL * 3).await;
        assert!(!waiting.is_finished(), "the other process still holds it");

        drop(other);
        assert!(waiting.await.unwrap());
    }

    #[tokio::test]
    async fn waiting_for_the_lock_ends_when_the_install_is_canceled() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".release.lock");
        let _held = InstallLock::acquire(&path, &CancellationToken::new())
            .await
            .unwrap()
            .unwrap();

        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            InstallLock::acquire(&path, &cancel)
                .await
                .unwrap()
                .is_none()
        );
    }
}
