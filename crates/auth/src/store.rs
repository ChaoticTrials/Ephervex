//! Where credentials live: the desktop keyring, or memory, or nowhere
//! (ADR 0004).
//!
//! # The rule, and why it is enforced by types rather than by discipline
//!
//! No secret is ever written to a file by this application. Ephervex is a *bug
//! reproduction* tool whose users routinely zip up a configuration directory
//! and send it to a stranger, which makes plaintext credential storage more
//! dangerous here than in a launcher people keep to themselves.
//!
//! So there are exactly two implementations of [`CredentialStore`] and neither
//! of them can write to disk. [`KeyringStore`] hands the secret to the Secret
//! Service; [`MemoryStore`] keeps it for the life of the process and loses it
//! on exit. There is no third one to reach for on a bad day, and adding one
//! would be a visible change to this file rather than a line somewhere else.
//!
//! # When Secret Service is missing
//!
//! ADR 0004 requires explicit failure or session-only consent; never fall
//! back silently. That is three separate requirements and this module supplies
//! the first ([`CredentialStorage::probe`] reports the failure with the
//! platform's own words) and the second ([`MemoryStore`]). The third is
//! structural - the caller has to *ask* for the memory store by name, so
//! degrading cannot happen by accident.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use ephervex_model::APP_ID;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::{AuthError, AuthResult, CredentialOperation};
use crate::secret::SecretString;

/// Which credential a caller wants.
///
/// A typed key rather than a string, so no call site can invent an entry name,
/// and so adding a second account later is a matter of passing a different
/// [`Uuid`] rather than reformatting a string in three places.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CredentialKey {
    /// The Microsoft refresh token for one account.
    MicrosoftRefreshToken(Uuid),
    /// The user's CurseForge API key.
    ///
    /// Nothing in authentication reads this; CurseForge integrations do. It
    /// belongs here because it belongs in the same API settings group as the
    /// Client ID, and because the one thing that must be true of it from the
    /// start is that it never reaches `config.toml`.
    CurseForgeApiKey,
}

impl CredentialKey {
    /// The entry name inside the keyring's service.
    #[must_use]
    pub fn entry_name(&self) -> String {
        match self {
            Self::MicrosoftRefreshToken(account) => format!("microsoft-refresh:{account}"),
            Self::CurseForgeApiKey => "curseforge-api-key".to_owned(),
        }
    }
}

impl fmt::Display for CredentialKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.entry_name())
    }
}

/// Where a credential is being kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStorage {
    /// The desktop Secret Service. Survives a restart.
    Keyring,
    /// This process only. The user is signed in until they quit.
    SessionOnly,
}

impl CredentialStorage {
    /// Whether the Secret Service can be reached on this machine.
    ///
    /// The probe is a real one: `keyring` initializes its platform store
    /// lazily, and asking it for its status is what forces that to happen.
    /// Returns the platform's own explanation on failure, because "secure
    /// storage is unavailable" without a reason is not something a user can act
    /// on.
    pub fn probe() -> Result<Self, String> {
        match keyring::Entry::store_status() {
            Ok(()) => Ok(Self::Keyring),
            Err(error) => Err(error.to_string()),
        }
    }

    /// A stable identifier for logs and `--json` output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::SessionOnly => "session-only",
        }
    }

    /// The `ephervex-i18n` message id describing this to a user.
    #[must_use]
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Keyring => "auth-storage-keyring",
            Self::SessionOnly => "auth-storage-session-only",
        }
    }

    /// Whether a credential kept here survives a restart.
    #[must_use]
    pub const fn is_persistent(self) -> bool {
        matches!(self, Self::Keyring)
    }
}

/// What [`CredentialStore::delete`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRemoval {
    /// A stored credential was there and is gone.
    Removed,
    /// Nothing was stored under that key.
    Absent,
}

/// Somewhere a credential can be kept.
///
/// Synchronous on purpose: both implementations are, the Secret Service call is
/// a local D-Bus round trip, and an async trait here would infect
/// [`crate::provider::AccountProvider`] with a second lifetime for no benefit.
pub trait CredentialStore: fmt::Debug + Send + Sync {
    /// Stores or replaces a credential.
    fn store(&self, key: &CredentialKey, secret: &SecretString) -> AuthResult<()>;

    /// Reads a credential, or `None` if there is none.
    fn load(&self, key: &CredentialKey) -> AuthResult<Option<SecretString>>;

    /// Removes a credential. Removing one that is not there succeeds: logout
    /// must be idempotent, and "it was already gone" is the outcome the caller
    /// wanted. The result says which of the two happened, so a caller never
    /// reports a deletion that did not take place.
    fn delete(&self, key: &CredentialKey) -> AuthResult<CredentialRemoval>;

    /// Where this store keeps things.
    fn storage(&self) -> CredentialStorage;
}

/// The desktop keyring, via Secret Service on Linux.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringStore {
    /// A store under this application's service name.
    #[must_use]
    pub fn new() -> Self {
        Self {
            // `APP_ID` is the single source of truth for this string; a literal
            // here would be a second one to keep in step.
            service: APP_ID.to_owned(),
        }
    }

    /// Opens the store, refusing if Secret Service is not there.
    ///
    /// This is where the requirement to explain that secure credential storage is
    /// unavailable" becomes a typed error the frontend can render, rather than
    /// a failure discovered at the moment the user finishes signing in.
    pub fn open() -> AuthResult<Self> {
        Self::open_from_probe(CredentialStorage::probe(), true)
    }

    /// [`Self::open`] for background work the user did not just ask for: a
    /// missing Secret Service is logged at debug level instead of as a warning.
    pub fn open_quietly() -> AuthResult<Self> {
        Self::open_from_probe(CredentialStorage::probe(), false)
    }

    fn open_from_probe(
        probe: Result<CredentialStorage, String>,
        warn_user: bool,
    ) -> AuthResult<Self> {
        match probe {
            Ok(_) => Ok(Self::new()),
            Err(detail) => {
                if warn_user {
                    warn!(%detail, "the desktop secret service is unavailable");
                } else {
                    debug!(%detail, "the desktop secret service is unavailable");
                }
                Err(AuthError::SecretStorageUnavailable { detail })
            }
        }
    }

    fn entry(&self, key: &CredentialKey) -> AuthResult<keyring::Entry> {
        keyring::Entry::new(&self.service, &key.entry_name()).map_err(|source| {
            // A failure to even name an entry means the platform store is gone,
            // which is the same situation `open` reports.
            AuthError::SecretStorageUnavailable {
                detail: source.to_string(),
            }
        })
    }
}

impl CredentialStore for KeyringStore {
    fn store(&self, key: &CredentialKey, secret: &SecretString) -> AuthResult<()> {
        self.entry(key)?
            .set_password(secret.expose())
            .map_err(|source| AuthError::CredentialStore {
                operation: CredentialOperation::Store,
                source: Box::new(source),
            })?;
        // The key name, never the value.
        debug!(entry = %key, "stored a credential in the keyring");
        Ok(())
    }

    fn load(&self, key: &CredentialKey) -> AuthResult<Option<SecretString>> {
        match self.entry(key)?.get_password() {
            Ok(secret) => Ok(Some(SecretString::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(AuthError::CredentialStore {
                operation: CredentialOperation::Load,
                source: Box::new(source),
            }),
        }
    }

    fn delete(&self, key: &CredentialKey) -> AuthResult<CredentialRemoval> {
        match self.entry(key)?.delete_credential() {
            Ok(()) => {
                debug!(entry = %key, "removed a credential from the keyring");
                Ok(CredentialRemoval::Removed)
            }
            // Logout is idempotent: a credential that was already gone is the
            // state the caller asked for.
            Err(keyring::Error::NoEntry) => Ok(CredentialRemoval::Absent),
            Err(source) => Err(AuthError::CredentialStore {
                operation: CredentialOperation::Delete,
                source: Box::new(source),
            }),
        }
    }

    fn storage(&self) -> CredentialStorage {
        CredentialStorage::Keyring
    }
}

/// Credentials held for the life of the process and no longer.
///
/// This is the explicit session-only login path. It is also what the test suite uses, and
/// those two being the same implementation is deliberate: the degraded path is
/// exercised by every test run rather than only by users who happen to have no
/// keyring.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<CredentialKey, SecretString>>,
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A poisoned lock means another thread panicked while holding it. The map
    /// itself is still consistent - nothing here can panic part-way through a
    /// mutation - so recovering beats refusing to log a user in.
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<CredentialKey, SecretString>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CredentialStore for MemoryStore {
    fn store(&self, key: &CredentialKey, secret: &SecretString) -> AuthResult<()> {
        self.entries().insert(key.clone(), secret.clone());
        Ok(())
    }

    fn load(&self, key: &CredentialKey) -> AuthResult<Option<SecretString>> {
        Ok(self.entries().get(key).cloned())
    }

    fn delete(&self, key: &CredentialKey) -> AuthResult<CredentialRemoval> {
        Ok(if self.entries().remove(key).is_some() {
            CredentialRemoval::Removed
        } else {
            CredentialRemoval::Absent
        })
    }

    fn storage(&self) -> CredentialStorage {
        CredentialStorage::SessionOnly
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn account() -> Uuid {
        Uuid::parse_str("069a79f4-44e9-4726-a5be-fca90e38aaf5").expect("uuid")
    }

    #[test]
    fn entry_names_are_derived_from_the_key_and_are_distinct_per_account() {
        let first = CredentialKey::MicrosoftRefreshToken(account());
        let second = CredentialKey::MicrosoftRefreshToken(Uuid::nil());
        assert_ne!(first.entry_name(), second.entry_name());
        // One account ships, but the entry name already carries the account id
        // so a second one is a new key rather than a schema change.
        assert!(first.entry_name().contains(&account().to_string()));
        assert_eq!(
            CredentialKey::CurseForgeApiKey.entry_name(),
            "curseforge-api-key"
        );
    }

    /// Captures the level of every event emitted while `run` executes.
    fn levels_logged_by(run: impl FnOnce()) -> Vec<tracing::Level> {
        use std::sync::Arc;
        use tracing_subscriber::layer::SubscriberExt as _;

        struct Levels(Arc<Mutex<Vec<tracing::Level>>>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Levels {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _context: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(*event.metadata().level());
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Levels(Arc::clone(&seen)));
        tracing::subscriber::with_default(subscriber, run);
        seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    #[test]
    fn opening_without_a_secret_service_warns_only_when_the_caller_asked_for_it() {
        let missing = || Err("no secret service".to_owned());

        let loud = levels_logged_by(|| {
            assert!(KeyringStore::open_from_probe(missing(), true).is_err());
        });
        assert_eq!(loud, [tracing::Level::WARN]);

        let quiet = levels_logged_by(|| {
            let error = KeyringStore::open_from_probe(missing(), false).expect_err("unavailable");
            assert!(matches!(
                error,
                AuthError::SecretStorageUnavailable { detail } if detail == "no secret service"
            ));
        });
        assert_eq!(quiet, [tracing::Level::DEBUG]);
    }

    #[test]
    fn the_keyring_service_name_is_the_one_application_identifier() {
        assert_eq!(KeyringStore::new().service, APP_ID);
    }

    #[test]
    fn the_session_only_store_round_trips_and_forgets_on_delete() {
        let store = MemoryStore::new();
        let key = CredentialKey::MicrosoftRefreshToken(account());
        assert!(store.load(&key).expect("load").is_none());

        store
            .store(&key, &SecretString::new("M.C1_refresh"))
            .expect("store");
        assert_eq!(
            store.load(&key).expect("load").expect("present").expose(),
            "M.C1_refresh"
        );

        assert_eq!(
            store.delete(&key).expect("delete"),
            CredentialRemoval::Removed
        );
        assert!(store.load(&key).expect("load").is_none());
    }

    #[test]
    fn deleting_a_credential_that_is_not_there_succeeds_and_says_it_was_absent() {
        // Logout must be idempotent: a user who logs out twice, or who logs out
        // after the keyring was cleared externally, has got what they asked for.
        let store = MemoryStore::new();
        assert_eq!(
            store
                .delete(&CredentialKey::CurseForgeApiKey)
                .expect("delete must be idempotent"),
            CredentialRemoval::Absent
        );
    }

    #[test]
    fn the_two_stores_disagree_about_surviving_a_restart_and_say_so() {
        assert!(CredentialStorage::Keyring.is_persistent());
        assert!(!CredentialStorage::SessionOnly.is_persistent());
        assert_eq!(MemoryStore::new().storage(), CredentialStorage::SessionOnly);
        assert_ne!(
            CredentialStorage::Keyring.message_id(),
            CredentialStorage::SessionOnly.message_id()
        );
    }

    #[test]
    fn credentials_are_kept_apart_by_key() {
        let store = MemoryStore::new();
        store
            .store(
                &CredentialKey::MicrosoftRefreshToken(account()),
                &SecretString::new("refresh"),
            )
            .expect("store");
        store
            .store(&CredentialKey::CurseForgeApiKey, &SecretString::new("api"))
            .expect("store");

        assert_eq!(
            store
                .load(&CredentialKey::CurseForgeApiKey)
                .expect("load")
                .expect("present")
                .expose(),
            "api"
        );
        // Deleting one must not disturb the other: logging out of Minecraft
        // must not silently throw away a CurseForge key.
        store
            .delete(&CredentialKey::MicrosoftRefreshToken(account()))
            .expect("delete");
        assert!(
            store
                .load(&CredentialKey::CurseForgeApiKey)
                .expect("load")
                .is_some()
        );
    }

    #[test]
    fn a_store_never_renders_a_secret_it_holds() {
        let store = MemoryStore::new();
        store
            .store(
                &CredentialKey::CurseForgeApiKey,
                &SecretString::new("$2a$10$notarealkey"),
            )
            .expect("store");
        assert!(!format!("{store:?}").contains("notarealkey"));
    }
}
