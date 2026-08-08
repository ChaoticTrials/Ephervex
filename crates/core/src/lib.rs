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

pub mod account;
pub mod app;
pub mod error;
pub mod launch;
pub mod logging;

pub use account::{AccountStatus, AccountSummary, ApiSettings, LoginOptions, TOKEN_REFRESH_MARGIN};
pub use app::{App, JavaPlan, JavaPlanRequest, global_java_settings};
pub use error::{CoreError, CoreResult};
pub use launch::{LaunchOptions, LaunchOutcome};
pub use logging::init_tracing;

/// Cancellation, re-exported because the services take one.
///
/// A frontend that cannot name the type cannot cancel a login, and adding
/// `tokio-util` to a frontend's manifest to get at it would be exactly the kind
/// of dependency creep the boundary exists to prevent.
pub use tokio_util::sync::CancellationToken;

/// Hands a URL to the desktop.
///
/// Here rather than in a frontend because both of them need it — the CLI for
/// the sign-in address, the GUI for the same plus the project's open-folder
/// actions — and because `ephervex-auth` deliberately must not do it: a
/// headless machine has no desktop to hand anything to, so the crate that runs
/// the login emits the URL and the frontend decides what to do with it.
///
/// Returns whether the desktop accepted it. A `false` is not a failure worth
/// aborting a login over: the caller has already printed the address.
pub fn open_in_browser(url: &str) -> bool {
    match open::that_detached(url) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "could not hand the address to a browser");
            false
        }
    }
}

/// Data types that cross the frontend boundary.
///
/// These are plain records and requests, not services: a frontend needs to
/// render a `ClientRecord` and build a `NewClient`, but it never needs the
/// `Registry` that produced one — nor the `VersionCatalog` that produced a
/// `ReleaseListing`.
pub use ephervex_auth::{
    Account, AuthError, AuthEvent, AuthObserver, AuthStage, CredentialStorage,
    DEFAULT_MICROSOFT_CLIENT_ID, DeviceCodePrompt, Entitlements, LoginFlow, LoginRequest,
    MINECRAFT_API_REVIEW_FORM, OfflineAccount, SecretString, Session, XstsDenial, offline_uuid,
};
pub use ephervex_download::Freshness;
pub use ephervex_java::{
    EffectiveJvmSettings, GlobalJavaSettings, JavaError, JavaOrigin, JavaPathChoice, JavaRuntime,
    JavaSourceKind, JavaVersion, JvmField, ManagedInstallation, ManagedRelease, PinSource,
    ProbeFailure, RejectedRuntime, RejectionReason, RequiredJava, RequirementBasis, RuleRejection,
    RuntimeInventory,
};
pub use ephervex_minecraft::LaunchObserver;
pub use ephervex_minecraft::{
    ArgumentEntry, ArgumentForm, ArgumentGroup, Arguments, Artifact, AssetIndexRef,
    ExcludedLibrary, ExclusionReason, GameExit, Installation, InstallationPlan, JavaRequirement,
    LatestVersions, LaunchCommand, LaunchError, LaunchEvent, LaunchFeatures, Library, LogRecord,
    LoggingConfig, MINIMUM_SUPPORTED_VERSION, MinecraftVersionId, OutputLine, OutputStream,
    Platform, PlayerIdentity, ReleaseListing, ReleaseType, VersionMetadata, VersionRecord,
};
pub use ephervex_storage::{
    AccountConfig, ApiConfig, CONFIG_SCHEMA_VERSION, ClientRecord, Config, EulaConfig,
    FileDisposition, ImportSource, JavaConfig, JavaOverride, JvmSettings, LaunchState,
    NetworkAccess, NewClient, NewServer, PathError, Paths, PreferenceSetting, ReconciledRecord,
    ReconciliationReport, RecordRef, SECRET_KEY_MARKERS, ServerDefaults, ServerNetworkSettings,
    ServerRecord, StorageError, running_state,
};
