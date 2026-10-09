//! Serializes changes to one host's recipe across threads and launcher
//! processes.
//!
//! A change reads the recipe, works for a while, and writes the whole recipe
//! back. Two of them overlapping lose whichever wrote first, so every change
//! that rewrites a host's recipe from a copy it read earlier holds that host's
//! gate from the read to the write.
//!
//! Within a process the gate is a process-wide slot rather than a field of
//! [`crate::app::App`]: a frontend opens a second application for its
//! background work, and that one has to wait for the first's mod changes too.
//! Across processes it is a file claim the storage layer keeps outside the
//! host's tree, so a command line and the desktop app changing one host take
//! turns.
//!
//! Waiting is possible from async and from synchronous code, because both
//! kinds of operation rewrite recipes. Waiting synchronously blocks the thread,
//! which is only acceptable for a short holder, or for a wait that `cancel`
//! can end. The wait for another process polls the claim, so it never holds an
//! async worker.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use ephervex_pack::PackError;
use ephervex_storage::{RecipeClaim, Registry, RegistryError};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::warn;

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

/// A taken slot, released on drop.
#[derive(Debug)]
struct SlotHold(Arc<Slot>);

impl Drop for SlotHold {
    fn drop(&mut self) {
        *self.0.held.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.0.freed.notify_one();
        self.0.wake.notify_waiters();
    }
}

/// Exclusive right to change one host's recipe, released on drop.
#[derive(Debug)]
pub(crate) struct HostRecipeGate {
    // Declared first, so the file claim is released before a waiter in this
    // process is woken to look for it.
    _claim: RecipeClaim,
    _slot: SlotHold,
}

/// How often a wait looks at its cancellation token and at the file claim.
const POLL: Duration = Duration::from_millis(25);

/// Waits for the host's gate, giving up when `cancel` fires first.
///
/// `None` means canceled while waiting, or that the claim could not be taken at
/// all, which is logged. The gate is not reentrant: never wait for a host's
/// gate while holding it.
pub(crate) async fn hold(
    registry: &Registry,
    host: InstalledHost,
    cancel: &CancellationToken,
) -> Option<HostRecipeGate> {
    match acquire(registry, host, cancel).await {
        Ok(gate) => gate,
        Err(error) => {
            warn!(%error, "a host's recipe could not be claimed");
            None
        }
    }
}

/// Like [`hold`], for an operation that fails when it is canceled.
pub(crate) async fn hold_for_change(
    registry: &Registry,
    host: InstalledHost,
    cancel: &CancellationToken,
) -> CoreResult<HostRecipeGate> {
    acquire(registry, host, cancel)
        .await?
        .ok_or_else(|| PackError::Canceled.into())
}

/// `Ok(None)` is a wait that `cancel` ended.
async fn acquire(
    registry: &Registry,
    host: InstalledHost,
    cancel: &CancellationToken,
) -> CoreResult<Option<HostRecipeGate>> {
    let slot = slot(host);
    loop {
        // Registered before the check, so a release between the two is not
        // missed.
        let mut freed = std::pin::pin!(slot.wake.notified());
        freed.as_mut().enable();
        if slot.try_take() {
            break;
        }
        tokio::select! {
            biased;
            () = &mut freed => {}
            () = cancel.cancelled() => return Ok(None),
        }
    }
    let hold = SlotHold(slot);
    loop {
        if let Some(claim) = registry.try_claim_recipe(host.record_ref())? {
            return Ok(Some(HostRecipeGate {
                _claim: claim,
                _slot: hold,
            }));
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(None),
            () = tokio::time::sleep(POLL) => {}
        }
    }
}

/// How long [`hold_blocking`] waits for another process's claim. A caller that
/// cannot be canceled is often a frontend's command loop, which must stay
/// responsive, so it gives up rather than waiting out a long change.
const CROSS_PROCESS_PATIENCE: Duration = Duration::from_secs(3);

/// Waits for the host's gate on the calling thread, and for another process's
/// claim only [`CROSS_PROCESS_PATIENCE`].
pub(crate) fn hold_blocking(
    registry: &Registry,
    host: InstalledHost,
) -> CoreResult<HostRecipeGate> {
    acquire_blocking(registry, host, None)
}

/// Like [`hold_blocking`], for a holder that may wait behind a long change:
/// gives up when `cancel` fires first, as `hold` does.
pub(crate) fn hold_blocking_for_change(
    registry: &Registry,
    host: InstalledHost,
    cancel: &CancellationToken,
) -> CoreResult<HostRecipeGate> {
    acquire_blocking(registry, host, Some(cancel))
}

fn recipe_busy(registry: &Registry, host: InstalledHost) -> CoreResult<crate::CoreError> {
    let path = match host {
        InstalledHost::Instance(id) => registry.client(id)?.root,
        InstalledHost::Server(id) => registry.server(id)?.root,
    };
    Ok(RegistryError::RecipeBusy { path }.into())
}

fn acquire_blocking(
    registry: &Registry,
    host: InstalledHost,
    cancel: Option<&CancellationToken>,
) -> CoreResult<HostRecipeGate> {
    let canceled = || cancel.is_some_and(CancellationToken::is_cancelled);
    let slot = slot(host);
    {
        let mut held = slot.held.lock().unwrap_or_else(PoisonError::into_inner);
        while *held {
            if canceled() {
                return Err(PackError::Canceled.into());
            }
            held = match cancel {
                Some(_) => {
                    slot.freed
                        .wait_timeout(held, POLL)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => slot
                    .freed
                    .wait(held)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
        *held = true;
    }
    let hold = SlotHold(slot);
    let give_up = cancel
        .is_none()
        .then(|| Instant::now() + CROSS_PROCESS_PATIENCE);
    loop {
        if let Some(claim) = registry.try_claim_recipe(host.record_ref())? {
            return Ok(HostRecipeGate {
                _claim: claim,
                _slot: hold,
            });
        }
        if canceled() {
            return Err(PackError::Canceled.into());
        }
        if give_up.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(recipe_busy(registry, host)?);
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::{Path, PathBuf};
    use std::pin::pin;
    use std::time::Duration;

    use ephervex_model::{InstanceId, LoaderSelection};
    use ephervex_storage::{NewClient, Paths};

    use super::*;

    fn host() -> InstalledHost {
        InstalledHost::Instance(InstanceId::new())
    }

    /// A launcher's own registry over the shared data root; its claims are
    /// the ones another launcher process would contend with.
    fn registry(base: &Path) -> Registry {
        Registry::open_in_memory(Paths::rooted_at(base)).expect("registry")
    }

    fn base() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    const SETTLE: Duration = Duration::from_millis(100);

    #[tokio::test]
    async fn a_second_holder_waits_until_the_first_releases() {
        let (_dir, base) = base();
        let (first_launcher, second_launcher) = (registry(&base), registry(&base));
        let host = host();
        let cancel = CancellationToken::new();
        let first = hold(&first_launcher, host, &cancel)
            .await
            .expect("first holder");

        let mut waiting = pin!(hold(&second_launcher, host, &cancel));
        assert!(
            tokio::time::timeout(SETTLE, waiting.as_mut())
                .await
                .is_err(),
            "the gate was shared while held"
        );

        drop(first);
        assert!(waiting.await.is_some());
    }

    #[tokio::test]
    async fn different_hosts_do_not_wait_for_each_other() {
        let (_dir, base) = base();
        let registry = registry(&base);
        let cancel = CancellationToken::new();
        let _first = hold(&registry, host(), &cancel).await.expect("first host");
        tokio::time::timeout(Duration::from_secs(5), hold(&registry, host(), &cancel))
            .await
            .expect("another host's gate is free")
            .expect("second host");
    }

    #[tokio::test]
    async fn cancellation_ends_the_wait_without_taking_the_gate() {
        let (_dir, base) = base();
        let (first_launcher, second_launcher) = (registry(&base), registry(&base));
        let host = host();
        let held = hold(&first_launcher, host, &CancellationToken::new())
            .await
            .expect("first holder");

        let cancel = CancellationToken::new();
        let waiting = hold(&second_launcher, host, &cancel);
        let cancel_soon = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (waited, ()) = tokio::join!(waiting, cancel_soon);
        assert!(waited.is_none());

        drop(held);
        assert!(
            hold(&second_launcher, host, &CancellationToken::new())
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_thread_waiting_synchronously_proceeds_once_the_gate_is_released() {
        let (_dir, base) = base();
        let registry = registry(&base);
        let host = host();
        let held = hold(&registry, host, &CancellationToken::new())
            .await
            .expect("first holder");

        let waiting = std::thread::spawn({
            let base = base.clone();
            move || drop(hold_blocking(&self::registry(&base), host).expect("the gate"))
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "the gate was shared while held");

        drop(held);
        waiting.join().expect("the waiting thread finishes");
        assert!(
            hold(&registry, host, &CancellationToken::new())
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_thread_waiting_for_a_change_stops_waiting_when_canceled() {
        let (_dir, base) = base();
        let registry = registry(&base);
        let host = host();
        let held = hold(&registry, host, &CancellationToken::new())
            .await
            .expect("first holder");

        let cancel = CancellationToken::new();
        let waiting = std::thread::spawn({
            let cancel = cancel.clone();
            let base = base.clone();
            move || hold_blocking_for_change(&self::registry(&base), host, &cancel).is_err()
        });
        tokio::time::sleep(SETTLE).await;
        assert!(!waiting.is_finished(), "the gate was shared while held");

        cancel.cancel();
        assert!(waiting.join().expect("the waiting thread finishes"));
        drop(held);
        assert!(
            hold(&registry, host, &CancellationToken::new())
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_async_waiter_proceeds_once_a_synchronous_holder_releases() {
        let (_dir, base) = base();
        let (first_launcher, second_launcher) = (registry(&base), registry(&base));
        let host = host();
        let held = hold_blocking(&first_launcher, host).expect("the gate");

        let cancel = CancellationToken::new();
        let mut waiting = pin!(hold(&second_launcher, host, &cancel));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), waiting.as_mut())
                .await
                .is_err(),
            "the gate was shared while held"
        );

        drop(held);
        assert!(waiting.await.is_some());
    }

    #[tokio::test]
    async fn a_free_slot_still_waits_while_another_launcher_holds_the_recipe_claim() {
        let (_dir, base) = base();
        let (this_launcher, other_launcher) = (registry(&base), registry(&base));
        let host = host();
        let other = other_launcher
            .try_claim_recipe(host.record_ref())
            .expect("try")
            .expect("free");

        let cancel = CancellationToken::new();
        let mut waiting = pin!(hold(&this_launcher, host, &cancel));
        assert!(
            tokio::time::timeout(SETTLE, waiting.as_mut())
                .await
                .is_err(),
            "the gate was taken under another launcher's claim"
        );

        drop(other);
        assert!(waiting.await.is_some());
    }

    #[tokio::test]
    async fn a_wait_for_another_launchers_claim_ends_when_canceled_and_frees_the_slot() {
        let (_dir, base) = base();
        let (this_launcher, other_launcher) = (registry(&base), registry(&base));
        let host = host();
        let other = other_launcher
            .try_claim_recipe(host.record_ref())
            .expect("try")
            .expect("free");

        let cancel = CancellationToken::new();
        let waiting = hold(&this_launcher, host, &cancel);
        let cancel_soon = async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            cancel.cancel();
        };
        let (waited, ()) = tokio::join!(waiting, cancel_soon);
        assert!(waited.is_none());

        drop(other);
        assert!(
            hold(&this_launcher, host, &CancellationToken::new())
                .await
                .is_some(),
            "the canceled wait left the slot taken"
        );
    }

    #[tokio::test]
    async fn an_uncancelable_wait_behind_another_launcher_gives_up_and_frees_the_slot() {
        let (_dir, base) = base();
        let app = crate::app::App::open_at(Paths::rooted_at(&base)).expect("app opens");
        let this_launcher = app.registry();
        let other_launcher = registry(&base);
        let client = app
            .create_client_for_test(NewClient::new("x", "1.20.1", LoaderSelection::vanilla()))
            .expect("create");
        let host = InstalledHost::Instance(client.id);
        let other = other_launcher
            .try_claim_recipe(host.record_ref())
            .expect("try")
            .expect("free");

        let refused = hold_blocking(this_launcher, host).expect_err("another launcher holds it");
        assert_eq!(refused.message_id(), "error-registry-recipe-busy");

        drop(other);
        assert!(hold_blocking(this_launcher, host).is_ok());
    }
}
