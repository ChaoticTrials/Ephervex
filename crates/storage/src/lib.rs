//! XDG paths, versioned configuration, the SQLite index and its migrations, and
//! the instance/server registry.
//!
//! # The one idea this crate is built around
//!
//! The database is a *cache of authority*, not the authority (ADR 0002). Every
//! instance additionally owns a `.launcher/instance.json` inside its own
//! directory, so losing `launcher.sqlite` costs an index rebuild rather than a
//! user's instances. Two consequences run through the whole crate:
//!
//! * Writes go to the metadata file first and the index second. A crash between
//!   them leaves a re-indexable directory instead of a record pointing at
//!   nothing.
//! * [`reconcile`] exists, runs on every start, and is not optional.
//!
//! # Errors
//!
//! Typed per concern and carrying structured data, never pre-formatted English.
//! Each error exposes a `message_id()` that the frontend resolves through
//! `ephervex-i18n`. Nothing here knows what language the user
//! reads.
//!
//! # Secrets
//!
//! None are stored. Configuration may record that a credential exists and who
//! it belongs to; the credential itself lives in the desktop keyring
//! (ADR 0004), and [`Config::save`] refuses to write anything whose key name
//! looks like one.

pub mod atomic;
pub mod clone;
pub mod config;
pub(crate) mod copy;
pub mod diagnostics;
pub mod folders;
pub mod index;
pub mod lock;
pub mod metadata;
pub mod paths;
pub mod reconcile;
pub mod records;
pub mod registry;
pub(crate) mod relocate;

#[cfg(test)]
mod test_support;

pub use atomic::{AtomicWriteOptions, sync_parent, write_atomic, write_bytes_atomic};
pub use clone::{CloneMethod, CloneReport, clone_file, clone_tree, clone_tree_with_check};
pub use config::{
    AccountConfig, AccountLookup, ApiConfig, ApplicationColorScheme, ApplicationDisplayConfig,
    ApplicationTextScale, ArtifactCacheLimitMode, CONFIG_SCHEMA_VERSION, CacheConfig, Config,
    ConfigError, EulaConfig, ImportConfig, JavaConfig, ModsConfig, NetworkAccess,
    PreferenceSetting, SECRET_KEY_MARKERS, ServerDefaults, TechnicalTextSize,
};
pub use diagnostics::{
    CrashSummary, DEFAULT_VIEW_BYTES, DiagnosticEntry, DiagnosticError, DiagnosticKind,
    MAX_SEARCH_MATCHES, MAX_VIEW_BYTES, OwnedDirectory, Refusal, SearchMatch, SearchOptions,
    SearchOutcome, TextAnchor, TextView,
};
pub use folders::{ClientFolder, MissingFolder, ServerFolder};
pub use index::{Index, IndexError, MIGRATIONS, Migration, MigrationOutcome, apply_migrations};
pub use lock::{
    ForcedStop, LAUNCH_LOCK_FILE, LaunchLock, LaunchState, LockError, RunningLaunch, force_release,
    running_launches, running_state,
};
pub use metadata::{METADATA_VERSION, MetadataError};
pub use paths::{PathError, Paths};
pub use reconcile::{
    ReconciledRecord, ReconciliationReport, RecordRef, ServerRootConflict, reconcile,
};
pub use records::{
    ClientRecord, DUPLICATE_NAME_SUFFIX, DuplicateClient, DuplicatedClient, FileDisposition,
    ImportSource, JavaOverride, JvmSettings, NewClient, NewServer, ServerCommit,
    ServerCommitWarning, ServerCreationSource, ServerNetworkSettings, ServerOperatorIdentity,
    ServerOperatorProfile, ServerOperatorSetup, ServerPortSelection, ServerRecord,
};
pub use registry::{Registry, RegistryError};

/// Anything that can go wrong bringing storage up.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Path(#[from] PathError),
    /// The per-instance launch lock.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Discovering or reading a log or crash report.
    #[error(transparent)]
    Diagnostics(#[from] DiagnosticError),
}

impl StorageError {
    /// The `ephervex-i18n` message id describing this failure to a user.
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::Config(error) => error.message_id(),
            Self::Registry(error) => error.message_id(),
            Self::Path(error) => error.message_id(),
            Self::Lock(error) => error.message_id(),
            Self::Diagnostics(error) => error.message_id(),
        }
    }
}

/// The application's storage, opened and ready.
#[derive(Debug)]
pub struct Storage {
    pub config: Config,
    pub registry: Registry,
}

/// Opens the application's storage: paths, configuration, and registry.
///
/// This is the startup path both frontends take. It reads the configuration,
/// applies the configured persistent-instance-root override to the resolver -
/// the one place those two meet - creates the directories, and opens the index.
///
/// A configuration file that exists but cannot be parsed is a hard error rather
/// than a silent fall back to defaults. Quietly ignoring settings a user
/// deliberately wrote is worse than telling them which line to fix.
///
/// Reconciliation is deliberately *not* run here: it mutates the index, and a
/// caller that only wants to read should not have to know that. Call
/// [`reconcile`] explicitly once the registry is open.
pub fn open(paths: Paths) -> Result<Storage, StorageError> {
    let config = Config::load_or_default(&paths)?;
    let paths = config.apply_to(paths);
    paths.ensure_base_directories()?;
    let registry = Registry::open(paths)?;
    Ok(Storage { config, registry })
}

/// Opens storage using the XDG roots resolved from the environment.
pub fn open_from_environment() -> Result<Storage, StorageError> {
    open(Paths::from_environment()?)
}
