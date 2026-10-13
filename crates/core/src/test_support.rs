//! Credential storage for the tests of crates that link this one.
//!
//! Compiled only with the `test-support` feature, which a dev-dependency
//! enables. A test that opens an [`App`](crate::App) on a temporary root
//! isolates files but not the desktop keyring, so reading an account status
//! would reach the developer's Secret Service and can raise an unlock prompt.

use std::sync::{Arc, OnceLock};

use ephervex_auth::{CredentialStore, MemoryStore};

static STORE: OnceLock<Arc<dyn CredentialStore>> = OnceLock::new();

/// Replaces the desktop keyring with an empty in-memory store for the rest of
/// the process.
///
/// Applies to every [`App`](crate::App), including ones the code under test
/// opens itself. Calling it again keeps the store already installed.
pub fn use_in_memory_credential_store() {
    STORE.get_or_init(|| Arc::new(MemoryStore::new()));
}

pub(crate) fn installed_store() -> Option<Arc<dyn CredentialStore>> {
    STORE.get().map(Arc::clone)
}
