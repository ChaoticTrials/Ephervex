//! Application services shared by the GUI and the CLI.
//!
//! This is the only crate either frontend depends on. Networking, archive
//! parsing, loader installation, authentication, and filesystem business logic
//! all live below this line — never in a frontend.
//!
//! # What a frontend gets
//!
//! * [`App`] — the services themselves: open the application, reconcile, and
//!   manage client instances and servers.
//! * [`CoreError`] — one error type, carrying a stable message id that the
//!   frontend resolves through `ephervex-i18n`.
//! * The record and request types those services take and return, re-exported
//!   below so a frontend never names a provider crate.
//!
//! # Why the re-exports are here rather than in the frontend's manifest
//!
//! A frontend that lists a provider crate as a dependency can reach past this
//! layer whenever something is momentarily easier that way, and the boundary
//! quietly stops being real. Re-exporting the data types keeps the frontends'
//! manifests down to `ephervex-core`, `ephervex-model`, and `ephervex-i18n`,
//! so taking a shortcut means editing a manifest — a visible act someone has
//! to justify in review.

pub mod app;
pub mod error;
pub mod logging;

pub use app::App;
pub use error::{CoreError, CoreResult};
pub use logging::init_tracing;

/// Data types that cross the frontend boundary.
///
/// These are plain records and requests, not services: a frontend needs to
/// render a `ClientRecord` and build a `NewClient`, but it never needs the
/// `Registry` that produced one.
pub use ephervex_storage::{
    CONFIG_SCHEMA_VERSION, ClientRecord, Config, EulaConfig, FileDisposition, ImportSource,
    JavaConfig, JavaOverride, JvmSettings, NetworkAccess, NewClient, NewServer, PathError, Paths,
    PreferenceSetting, ReconciledRecord, ReconciliationReport, RecordRef, SECRET_KEY_MARKERS,
    ServerDefaults, ServerNetworkSettings, ServerRecord, StorageError,
};
