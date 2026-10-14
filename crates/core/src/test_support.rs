//! Credential storage for the tests of crates that link this one.
//!
//! Compiled only with the `test-support` feature, which a dev-dependency
//! enables. A test that opens an [`App`](crate::App) on a temporary root
//! isolates files but not the desktop keyring, so reading an account status
//! would reach the developer's Secret Service and can raise an unlock prompt.
//!
//! It also builds client fixtures that need no provider metadata, so a
//! frontend test can exercise a client reader without a network.

use std::sync::{Arc, OnceLock};

use ephervex_auth::{CredentialStore, MemoryStore};
use ephervex_model::InstanceId;
use ephervex_storage::{ClientRecord, NewClient};

use crate::App;
use crate::error::CoreResult;

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

/// Creates a client without checking its Minecraft or loader selection against
/// provider metadata.
pub fn create_client_offline(app: &App, request: NewClient) -> CoreResult<ClientRecord> {
    app.create_client_for_test(request)
}

/// Commits a change to a client's canonical metadata alone, leaving its index
/// row as it was: the state after an index write failed behind a committed
/// update.
pub fn commit_client_without_index(
    app: &App,
    id: InstanceId,
    change: impl FnOnce(&mut ClientRecord),
) -> CoreResult<ClientRecord> {
    app.commit_client_without_index_for_test(id, change)
}
