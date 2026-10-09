//! Serializes changes to one host's recipe within this process.
//!
//! A change reads the recipe, works for a while, and writes the whole recipe
//! back. Two of them overlapping lose whichever wrote first, so every change
//! that rewrites a host's recipe from a copy it read earlier holds that host's
//! gate from the read to the write.
//!
//! The gate is process-wide rather than a field of [`crate::app::App`]: a
//! frontend opens a second application for its background work, and that one
//! has to wait for the first's mod changes too. It does not reach another
//! process.
//!
//! Waiting is possible from async and from synchronous code, because both
//! kinds of operation rewrite recipes. Waiting synchronously blocks the thread,
//! which is only acceptable for a short holder.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use ephervex_pack::PackError;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::error::CoreResult;
use crate::mods::InstalledHost;

/// One host's gate: whether it is held, and who to tell when it is not.
#[derive(Debug, Default)]
struct Slot {
    held: Mutex<bool>,
    freed: Condvar,
    wake: Notify,
}

impl Slot {
    fn try_take(&self) -> bool {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        !std::mem::replace(&mut *held, true)
    }
}

type Slots = Mutex<HashMap<InstalledHost, Arc<Slot>>>;

fn slot(host: InstalledHost) -> Arc<Slot> {
    static SLOTS: OnceLock<Slots> = OnceLock::new();
    let mut slots = SLOTS
        .get_or_init(Slots::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    Arc::clone(slots.entry(host).or_default())
}

/// Exclusive right to change one host's recipe, released on drop.
#[derive(Debug)]
pub(crate) struct HostRecipeGate {
    slot: Arc<Slot>,
}

impl Drop for HostRecipeGate {
    fn drop(&mut self) {
        *self
            .slot
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        self.slot.freed.notify_one();
        self.slot.wake.notify_waiters();
    }
}

/// Waits for the host's gate, giving up when `cancel` fires first.
///
/// `None` means canceled while waiting. The gate is not reentrant: never wait
/// for a host's gate while holding it.
pub(crate) async fn hold(
    host: InstalledHost,
    cancel: &CancellationToken,
) -> Option<HostRecipeGate> {
    let slot = slot(host);
    loop {
        // Registered before the check, so a release between the two is not
        // missed.
        let mut freed = std::pin::pin!(slot.wake.notified());
        freed.as_mut().enable();
        if slot.try_take() {
            return Some(HostRecipeGate {
                slot: Arc::clone(&slot),
            });
        }
        tokio::select! {
            biased;
            () = &mut freed => {}
            () = cancel.cancelled() => return None,
        }
    }
}

/// Like [`hold`], for an operation that fails when it is canceled.
pub(crate) async fn hold_for_change(
    host: InstalledHost,
    cancel: &CancellationToken,
) -> CoreResult<HostRecipeGate> {
    hold(host, cancel)
        .await
        .ok_or_else(|| PackError::Canceled.into())
}

/// Waits for the host's gate on the calling thread.
pub(crate) fn hold_blocking(host: InstalledHost) -> HostRecipeGate {
    let slot = slot(host);
    {
        let mut held = slot.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held {
            held = slot
                .freed
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *held = true;
    }
    HostRecipeGate { slot }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::time::Duration;

    use ephervex_model::InstanceId;

    use super::*;

    fn host() -> InstalledHost {
        InstalledHost::Instance(InstanceId::new())
    }

    #[tokio::test]
    async fn a_second_holder_waits_until_the_first_releases() {
        let host = host();
        let cancel = CancellationToken::new();
        let first = hold(host, &cancel).await.expect("first holder");

        let waiting = tokio::spawn({
            let cancel = cancel.clone();
            async move { hold(host, &cancel).await.is_some() }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the gate was shared while held");

        drop(first);
        assert!(waiting.await.expect("join"));
    }

    #[tokio::test]
    async fn different_hosts_do_not_wait_for_each_other() {
        let cancel = CancellationToken::new();
        let _first = hold(host(), &cancel).await.expect("first host");
        tokio::time::timeout(Duration::from_secs(5), hold(host(), &cancel))
            .await
            .expect("another host's gate is free")
            .expect("second host");
    }

    #[tokio::test]
    async fn cancellation_ends_the_wait_without_taking_the_gate() {
        let host = host();
        let held = hold(host, &CancellationToken::new())
            .await
            .expect("first holder");

        let cancel = CancellationToken::new();
        let waiting = tokio::spawn({
            let cancel = cancel.clone();
            async move { hold(host, &cancel).await.is_none() }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        assert!(waiting.await.expect("join"));

        drop(held);
        assert!(hold(host, &CancellationToken::new()).await.is_some());
    }

    #[tokio::test]
    async fn a_thread_waiting_synchronously_proceeds_once_the_gate_is_released() {
        let host = host();
        let held = hold(host, &CancellationToken::new())
            .await
            .expect("first holder");

        let waiting = std::thread::spawn(move || drop(hold_blocking(host)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the gate was shared while held");

        drop(held);
        waiting.join().expect("the waiting thread finishes");
        assert!(hold(host, &CancellationToken::new()).await.is_some());
    }

    #[tokio::test]
    async fn an_async_waiter_proceeds_once_a_synchronous_holder_releases() {
        let host = host();
        let held = hold_blocking(host);

        let waiting =
            tokio::spawn(async move { hold(host, &CancellationToken::new()).await.is_some() });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the gate was shared while held");

        drop(held);
        assert!(waiting.await.expect("join"));
    }
}
