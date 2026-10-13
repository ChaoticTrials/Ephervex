//! The order in which stored credentials change (ADR 0004).
//!
//! Signing in, resuming, removing an account, and finishing a pending cleanup
//! each decide something about a credential and then change it. They must not
//! interleave, or the keyring ends up holding a credential that no account
//! names. This crate cannot know where the account records live, so whoever
//! owns them supplies the order and the provider takes it around every change
//! it makes after a wait on the network.

use std::any::Any;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::error::{AuthError, AuthResult};

/// Proof that credential changes are ordered for as long as it is alive.
///
/// Dropping it lets the next change proceed. Holding one while waiting on the
/// network stalls every other sign-in, so callers keep it to the decision and
/// the change it covers.
///
/// `Send`, so an order can be waited for on a blocking thread and handed to the
/// task that needs it (see [`hold_off_worker`]).
pub struct CredentialChangeGuard {
    _held: Box<dyn Any + Send>,
}

impl CredentialChangeGuard {
    /// Wraps whatever releases the order when dropped.
    #[must_use]
    pub fn new(held: impl Any + Send) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

impl fmt::Debug for CredentialChangeGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialChangeGuard")
    }
}

/// Serializes credential changes against everything that decides whether a
/// credential should exist.
///
/// Not reentrant: taking it again while a guard from the same order is alive
/// waits for that guard.
pub trait CredentialChangeOrder: fmt::Debug + Send + Sync {
    /// Waits until no other change is in progress, then claims the order.
    fn hold(&self) -> AuthResult<CredentialChangeGuard>;
}

/// Waits for `order` on the blocking pool, so a contended order does not hold
/// an async worker, and every task scheduled on it, for the whole wait.
///
/// The guard comes back to the caller once the wait is over. It is meant for a
/// change with no `await` in it: a guard carried across an await would make a
/// synchronous waiter on the same worker wait for a holder that cannot run.
pub async fn hold_off_worker(
    order: &Arc<dyn CredentialChangeOrder>,
) -> AuthResult<CredentialChangeGuard> {
    let order = Arc::clone(order);
    match tokio::task::spawn_blocking(move || order.hold()).await {
        Ok(held) => held,
        Err(error) => match error.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            // The runtime shut down before the wait could start.
            Err(_) => Err(AuthError::CredentialChangeUnavailable {
                path: PathBuf::new(),
            }),
        },
    }
}

/// Orders changes within this process only.
///
/// Process-wide rather than per provider: a launch builds its own provider,
/// and the account removal that races it runs in another `App`. A caller that
/// must also order against other processes supplies its own order.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessChangeOrder;

/// Whether a change is in progress in this process.
///
/// A flag and a condition variable rather than a mutex guard, which must be
/// released by the thread that took it: the guard here moves between threads.
static PROCESS_CHANGES: Mutex<bool> = Mutex::new(false);
static PROCESS_CHANGE_ENDED: Condvar = Condvar::new();

fn process_changes() -> MutexGuard<'static, bool> {
    // The flag protects ordering, not data, so a panic elsewhere leaves
    // nothing to distrust.
    PROCESS_CHANGES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The claim on [`PROCESS_CHANGES`], released on drop.
struct ProcessChange;

impl Drop for ProcessChange {
    fn drop(&mut self) {
        *process_changes() = false;
        PROCESS_CHANGE_ENDED.notify_one();
    }
}

fn claim_process_changes() -> ProcessChange {
    let mut in_progress = process_changes();
    while *in_progress {
        in_progress = PROCESS_CHANGE_ENDED
            .wait(in_progress)
            .unwrap_or_else(PoisonError::into_inner);
    }
    *in_progress = true;
    ProcessChange
}

impl CredentialChangeOrder for ProcessChangeOrder {
    fn hold(&self) -> AuthResult<CredentialChangeGuard> {
        Ok(CredentialChangeGuard::new(claim_process_changes()))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_process_order_is_released_by_dropping_its_guard_on_another_thread() {
        let guard = ProcessChangeOrder.hold().expect("order");
        let (taken, taken_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let second = ProcessChangeOrder.hold();
            taken.send(()).expect("receiver");
            drop(second);
        });

        assert!(
            taken_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "a second change must wait for the first"
        );
        std::thread::spawn(move || drop(guard))
            .join()
            .expect("releasing thread");

        taken_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the waiting change proceeds once the guard is dropped");
        waiter.join().expect("waiting thread");
    }

    #[derive(Debug)]
    struct ReleasedBy(Mutex<mpsc::Receiver<()>>);

    impl CredentialChangeOrder for ReleasedBy {
        fn hold(&self) -> AuthResult<CredentialChangeGuard> {
            self.0
                .lock()
                .expect("receiver")
                .recv_timeout(Duration::from_secs(10))
                .expect("released in time");
            Ok(CredentialChangeGuard::new(()))
        }
    }

    #[tokio::test]
    async fn a_contended_order_is_waited_for_without_holding_the_only_worker() {
        let (release, released) = mpsc::channel();
        let order: Arc<dyn CredentialChangeOrder> = Arc::new(ReleasedBy(Mutex::new(released)));

        let waiting = hold_off_worker(&order);
        tokio::pin!(waiting);
        let mut ticks = 0;
        while ticks < 5 {
            tokio::select! {
                biased;
                outcome = &mut waiting => panic!("finished before its release: {outcome:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => ticks += 1,
            }
        }

        release.send(()).expect("receiver");
        waiting.await.expect("the order is held once released");
    }
}
