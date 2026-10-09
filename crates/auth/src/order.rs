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
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::error::AuthResult;

/// Proof that credential changes are ordered for as long as it is alive.
///
/// Dropping it lets the next change proceed. Holding one while waiting on the
/// network stalls every other sign-in, so callers keep it to the decision and
/// the change it covers.
pub struct CredentialChangeGuard {
    _held: Box<dyn Any>,
}

impl CredentialChangeGuard {
    /// Wraps whatever releases the order when dropped.
    #[must_use]
    pub fn new(held: impl Any) -> Self {
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

/// Orders changes within this process only.
///
/// Process-wide rather than per provider: a launch builds its own provider,
/// and the account removal that races it runs in another `App`. A caller that
/// must also order against other processes supplies its own order.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessChangeOrder;

static PROCESS_CHANGES: Mutex<()> = Mutex::new(());

fn lock_process_changes() -> MutexGuard<'static, ()> {
    // The guard protects ordering, not data, so a panic elsewhere leaves
    // nothing to distrust.
    PROCESS_CHANGES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

impl CredentialChangeOrder for ProcessChangeOrder {
    fn hold(&self) -> AuthResult<CredentialChangeGuard> {
        Ok(CredentialChangeGuard::new(lock_process_changes()))
    }
}
