//! Application services shared by the GUI and the CLI.
//!
//! This is the only crate either frontend depends on. Networking, archive
//! parsing, loader installation, authentication, and filesystem business logic
//! all live below this line - never in a frontend.
//!
//! # What a frontend gets
//!
//! * [`App`] - the services themselves: open the application, reconcile, and
//!   manage client instances and servers.
//! * [`CoreError`] - one error type, carrying a stable message id that the
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
//! so taking a shortcut means editing a manifest - a visible act someone has
//! to justify in review.

pub mod account;
mod acquisition;
pub mod app;
pub mod artifact_cache;
mod artwork;
mod blocking;
mod bounded_image;
pub mod bundle;
pub mod connection;
pub mod crash_analysis;
pub mod curseforge;
pub mod curseforge_pack;
pub mod declared_relations;
pub mod dependencies;
pub mod diagnosis;
pub mod diagnostics;
mod disk_usage;
pub mod download_watch;
pub mod error;
pub mod health;
mod host_recipe_gate;
pub mod import;
pub mod instance;
pub mod instance_listing;
pub mod isolation;
pub mod java_choices;
pub mod java_failure;
pub mod java_install;
pub mod java_status;
pub mod launch;
pub mod licenses;
pub mod linked_mods;
pub mod linked_pair;
mod linked_server_list;
pub mod live_diagnostics;
pub mod loader;
mod loader_version_range;
pub mod local_mods;
pub mod logging;
pub mod minecraft_preferences;
mod mod_display_cache;
pub mod mods;
pub mod mutation;
pub mod open_files;
pub mod organization;
pub mod pack_url;
pub mod promotion;
pub mod running;
pub mod seasonal_theme;
pub mod server;
mod server_capture;
#[cfg(test)]
mod server_catalog_install;
pub mod server_pack;
pub mod single_instance;
pub mod snapshot;
pub mod termination;
#[cfg(test)]
mod test_fixtures;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod theme_package;
pub mod url_handler;
pub mod verify;

pub use account::{
    AccountRemoval, AccountStatus, AccountSummary, ApiSettings, CredentialOutcome,
    CredentialPresence, CurseForgeKeyState, LoginOptions, TOKEN_REFRESH_MARGIN,
};
pub use app::{
    App, ApplicationDisplaySettings, JavaPlan, JavaPlanRequest, JavaTarget,
    PersistentStorageSettings, ServerApplicationSettings, global_java_settings,
};
pub use artifact_cache::ArtifactCacheStatus;
pub use bundle::{BundleContents, BundlePart, BundlePartKind, BundleReport, BundleSelection};
pub use connection::{
    ConnectError, DEFAULT_SERVER_READINESS_TIMEOUT, LaunchAndConnectAvailability,
    LaunchAndConnectEvent, LaunchAndConnectObserver, LaunchAndConnectOptions,
    LaunchAndConnectOutcome, ServerPreparation, StartedServerLifetime,
};
pub use crash_analysis::{
    CrashAnalysis, CrashReview, CrashSuspect, LoaderReportedDependency, SuspectConfidence,
    SuspectReason, SuspectedMod, analyze_crash_report, loader_dependency_failures,
};
pub use curseforge::StoredApiKey;
pub use declared_relations::{DeclaredProject, ProjectAvailability};
pub use dependencies::{
    DeclaredConflict, DependencyKnowledge, DependencyPolicy, DependencyRefreshReport,
    DependentMods, ImpactedMod, ModRelations, RelatedMod, RequiredMods, RequirementProblems,
    UnmetDependency, UnmetRequirement,
};
pub use diagnosis::{
    DiagnosisRequest, DiagnosticFix, Finding, FindingClass, FindingKind, FixOutcome, FixPreview,
    HostDiagnosis, IntegrityCheck, JavaChange, JavaCheck, JavaLayer, JavaMismatch,
    JavaRuntimeSummary, JavaUnavailable, LoaderReportedRequirement, LoaderRequest, ModCounts,
    ProposedChange,
};
pub use diagnostics::{
    ClientLogTail, DEFAULT_DIAGNOSTIC_BYTES, DiagnosticCatalog, DiagnosticRequest, DiagnosticScope,
    DiagnosticSearch, DiagnosticSection,
};
pub use disk_usage::{
    CategoryUsage, DiskUsageReport, RetentionUsage, StorageCategory, StorageRetention,
};
pub use download_watch::{DownloadWatcher, RESCAN_INTERVAL, WatchTarget};
pub use error::{CoreError, CoreResult, ErrorChain};
pub use health::{
    ApplicationIdentity, ArtifactCacheHealth, BundledDataHealth, BundledDocumentHealth,
    ConfigurationHealth, CredentialBackendHealth, DirectoryHealth, DirectoryState,
    InstallationHealth, JavaHealth, PlatformIdentity, installation_health,
};
pub use import::{ImportRequest, ImportedPack, suggested_instance_name};
pub use instance::{
    CreatedClient, MAX_ENTITY_NAME_CHARS, NameRefusal, PreferenceContext,
    global_minecraft_preferences, name_with_suffix, validated_entity_name,
};
pub use instance_listing::{
    GroupFilter, InstanceFacts, InstanceFilter, InstanceSort, RunFilter, SearchQuery, VersionOrder,
    list_instances, natural_compare,
};
pub use isolation::{
    IsolationCancel, IsolationCandidate, IsolationConfidence, IsolationError,
    IsolationLinkedServer, IsolationMissingDependency, IsolationOutcome, IsolationPlan,
    IsolationReconciliation, IsolationReport, IsolationRequest, IsolationResultCandidate,
    IsolationRound, IsolationRoundKind, IsolationRoundRecord, IsolationSession, IsolationStep,
    RoundAnswer, RoundContradiction,
};
pub use java_choices::{
    AutomaticJava, JavaChoice, JavaChoices, JavaPinIndex, JavaRuntimePins, LauncherJavaChoice,
    PinningHost,
};
pub use java_failure::{
    ClassVersionMismatch, JavaRefusal, JavaRuntimeFailure, RefusedJavaChoice,
    class_version_mismatch,
};
pub use java_install::HostJavaInstall;
pub use java_status::{
    ExpectedRuntime, HostJavaStatus, JavaInstallOffer, JavaSelectionMode, JavaSelectionState,
    JavaStatusTone, ResolvedJava,
};
pub use launch::{LaunchOptions, LaunchOutcome};
pub use licenses::{
    DependencyLicense, LICENSE_INVENTORY_FORMAT_VERSION, LicenseInventory, LicenseInventoryError,
    LicenseText,
};
pub use linked_mods::{
    ClientModOutcome, ClientModPlan, LinkedModOutcome, LinkedModPlan, LinkedModRequest,
    LinkedModRoot, LinkedTargetBlock, LinkedTargetFailure, LinkedTargetOutcome, LinkedTargetPlan,
    LinkedTargetResult, LinkedTargetState,
};
pub use linked_pair::{LinkedPair, LinkedStart, LinkingState};
pub use loader::{IncompatibleMod, LoaderVersionChange, LoaderVersionPlan};
pub use local_mods::{
    LocalModOffer, LocalModPlan, LocalModRefusal, LocalModReport, MAX_LOCAL_MOD_BYTES,
};
pub use logging::{DEFAULT_LOG_DIRECTIVES, init_tracing};
pub use minecraft_preferences::{
    MinecraftPreferenceEntry, MinecraftPreferencePreview, MinecraftPreferenceSnapshot,
    PreferenceChange, PreferenceEdit, configured_minecraft_preferences,
};
pub use mods::{
    DISABLED_SUFFIX, InstalledHost, InstalledMods, InstancePackSource, ManagedModEntry,
    ManagedModSnapshot, MetadataAvailability, ModDetails, ModInstallRequest, ModPresence,
    disabled_path,
};
pub use mutation::{
    ModMutability, ModMutation, ModMutationPlan, ModVersionChoice, ModVersionChoices,
    MutationRestriction, PlannedFile, PlannedOutcome, effective_mutability,
};
pub use organization::{IconRefusal, InstanceGroup, OrganizationView, take_organization_notices};
pub use pack_url::{
    HostConsent, LAUNCHER_URL_SCHEME, LauncherUrl, LauncherUrlError, MAX_LAUNCHER_URL_BYTES,
    MAX_PACK_ARCHIVE_BYTES, PackConsents, PackSource, StagedPack,
};
pub use promotion::{
    PromotableServer, PromotedServerWarning, PromotionOutcome, PromotionPlan, PromotionRequest,
    PromotionWarning,
};
pub use running::{ProcessMonitor, ProcessUsage, RunningProcess};
pub use server::{
    CaptureSnapshot, ServerCreateOptions, ServerNetworkChange, ServerPersistenceHealth,
    ServerStatus, stoppable_for_deletion,
};
pub use server_capture::{CaptureUpdate, ServerCaptureTail};
pub use server_pack::{
    IdentificationOutcome, ServerArchiveConflict, ServerArchiveEvidence, ServerContentSource,
    ServerPackOutcome, read_server_archive_evidence,
};
pub use single_instance::{PrimaryInstance, Startup as SingleInstanceStartup};
pub use snapshot::{
    SnapshotError, SnapshotOrigin, SnapshotRecord, SnapshotRestore, SnapshotRestoreOptions,
};
pub use termination::TerminationRequests;
pub use url_handler::{
    URL_HANDLER_ENTRY, UrlHandlerRemoval, UrlHandlerSource, UrlHandlerStatus, scheme_mime_type,
};
pub use verify::{
    Integrity, RepairRefusal, RepairReport, SharedArtifactReport, VerificationReport, VerifiedFile,
};

/// Cancellation, re-exported because the services take one.
///
/// A frontend that cannot name the type cannot cancel a login, and adding
/// `tokio-util` to a frontend's manifest to get at it would be exactly the kind
/// of dependency creep the boundary exists to prevent.
pub use tokio_util::sync::CancellationToken;

/// Hands a URL to the desktop.
///
/// Here rather than in a frontend because both of them need it - the CLI for
/// the sign-in address, the GUI for the same plus the project's open-folder
/// actions - and because `ephervex-auth` deliberately must not do it: a
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

/// Shows the folder holding a file in the desktop's file manager.
///
/// For a file Ephervex itself just wrote to a place the user chose, such as a
/// diagnostic bundle, so the user can find it. Returns whether the desktop
/// accepted it; the caller has the path to show either way.
pub fn open_containing_folder(path: &std::path::Path) -> bool {
    let Some(folder) = path.parent() else {
        return false;
    };
    match open::that_detached(folder) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, folder = %folder.display(), "could not hand the folder to a file manager");
            false
        }
    }
}

/// Data types that cross the frontend boundary.
///
/// These are plain records and requests, not services: a frontend needs to
/// render a `ClientRecord` and build a `NewClient`, but it never needs the
/// `Registry` that produced one - nor the `VersionCatalog` that produced a
/// `ReleaseListing`.
pub use ephervex_auth::{
    Account, AuthError, AuthEvent, AuthObserver, AuthStage, CredentialStorage,
    DEFAULT_MICROSOFT_CLIENT_ID, DeviceCodePrompt, EntitlementLookup, Entitlements, LoginFlow,
    LoginRequest, MINECRAFT_API_REVIEW_FORM, OfflineAccount, SecretString, Session, XstsDenial,
    offline_uuid,
};
/// The artifact-cache maintenance results a settings screen renders.
///
/// The collector itself is not here: a frontend displays a report and asks for a
/// clear, it does not run a collection.
pub use ephervex_download::{
    ArtifactCacheError, ArtifactCacheMaintenance, MaintenanceStage, MaintenanceTrouble,
};
/// Progress, re-exported because the import service takes a reporter and
/// `Operation` is the single progress abstraction both frontends
/// render. A frontend that could not name these would have to add
/// `ephervex-download` to its manifest to show a progress bar.
pub use ephervex_download::{Freshness, OperationReporter, ProgressSink};
pub use ephervex_java::{
    EffectiveJvmSettings, ExpectedJava, GlobalJavaSettings, JavaError, JavaOrigin, JavaPathChoice,
    JavaRuntime, JavaSourceKind, JavaVersion, JvmField, ManagedInstallation, ManagedRelease,
    PinSource, ProbeFailure, RejectedRuntime, RejectionReason, RequiredJava, RequirementBasis,
    RuleRejection, RuntimeInventory, VersionBound,
};
pub use ephervex_loaders::{InstalledServer, LoaderBuild, LoaderVersionListing, ServerLaunch};
pub use ephervex_minecraft::{
    ArgumentEntry, ArgumentForm, ArgumentGroup, Arguments, Artifact, AssetIndexRef,
    ExcludedLibrary, ExclusionReason, GameExit, Installation, InstallationPlan, JavaRequirement,
    LanguageCatalog, LanguageEntry, LatestVersions, LaunchCommand, LaunchError, LaunchEvent,
    LaunchFeatures, Library, LogRecord, LoggingConfig, MINIMUM_SUPPORTED_VERSION,
    MinecraftVersionId, OutputLine, OutputStream, Platform, PlayerIdentity, ReleaseListing,
    ReleaseType, VersionMetadata, VersionRecord,
};
pub use ephervex_minecraft::{LaunchEvents, LaunchObserver};
/// The mod catalog's data types, re-exported so a frontend can render a search
/// result and a dependency plan without naming `ephervex-mod-platform`.
///
/// The provider trait and the resolver itself are deliberately *not* here: a
/// frontend displays a plan, it does not build one.
pub use ephervex_mod_platform::{
    AnnotatedVersion, CURSEFORGE_API_KEY_HELP, Compatibility, DEFAULT_SEARCH_LIMIT, EmbeddedNote,
    ExternalDependency, IncompatibilityReason, InstanceTarget, MAX_SEARCH_LIMIT, ModFile,
    ModInstallPlan, ModPlatformError, ModProject, ModProjectDetails, ModSearchHit, ModSearchQuery,
    ModSearchResults, ModVersionInfo, OptionalCandidate, OptionalStatus, PlanWarning, PlannedMod,
    ReleaseChannel, SideSupport, UnverifiedOptional,
};
pub use ephervex_model::{
    ARTIFACT_CACHE_LIMIT_PRESETS, ArtifactCacheLimit, ArtifactCacheLimitError, ByteUnit,
    ConnectionTarget, DEFAULT_ARTIFACT_CACHE_LIMIT_BYTES, JavaOverride, JvmSettings,
    MAX_ARTIFACT_CACHE_LIMIT_BYTES, ModRelationKind, ServerInstallation, ServerRelativePath,
    SnapshotId, format_binary_bytes,
};
pub use ephervex_options::catalog::{
    EnumValuePresentation, KeyBindingDefaultPresentation, PreferenceExclusion, PreferenceField,
};
pub use ephervex_options::{
    CompatibilitySource, ConfiguredPreference, ConfiguredPreferences, FiniteDecimal,
    GlobalMinecraftPreferences, GuiScalePreference, InputBinding, InvalidPreferenceId,
    InvalidPreferenceValue, LanguageAvailability, LoadedConfiguredPreferences, LoadedPreferences,
    Managed, MinecraftPreferenceCatalog, MinecraftPreferenceDescriptor, MinecraftPreferenceValue,
    MouseButton, OPTIONS_FILE_NAME, OptionsCompatibility, PhysicalKey, PlannedPreference,
    PreferenceApplication, PreferenceCategory, PreferenceDisposition, PreferenceId, PreferenceKey,
    PreferencePlan, PreferencePresentation, PreferenceSupport, PreferenceValue,
    PreferenceValueError, PreferenceValueProblem, PreferenceValueShape, PresentationTier,
    ReleaseSupport, SkipReason, SkippedPreference, WindowSize,
};
pub use ephervex_pack::{
    AppliedLayer, BlockedProviderFile, DuplicateFileReason, DuplicateProviderFile,
    ExtractionLimits, ManagedMod, ManagedModInstallation, ManagedMutationReport, MaterializedFiles,
    MaterializedPack, MaterializedServerRecipe, ModIdentity, ModOrigin, OptionalComponent,
    OptionalSelection, OverrideLayer, OverrideScope, PackAnalysis, PackError, PackFileRecipe,
    PackFormat, PackPolicy, PackRecipe, PackSide, ProviderFileIdentity, ProviderRecipeMetadata,
    RecordedMod, ServerReconstructionPlan, SidePlan, SideRequirement, UnknownServerFiles,
};
pub use ephervex_server::{
    CaptureHealth, CaptureRetention, CapturedLine, ControlAction, ControlResponse,
    EULA_CONFIRMATION_PHRASE, EulaStatus, MINECRAFT_EULA_REFERENCE, ProcessEvidence, ProcessRole,
    PropertiesDocument, RuntimeClassification, RuntimeEvidence, ServerEvent, ServerEvents,
    ServerExit, ServerObserver, ServerWarning,
};
pub use ephervex_storage::{
    AccountConfig, ApiConfig, ApplicationColorScheme, ApplicationDisplayConfig,
    ApplicationTextScale, ApplicationTheme, CONFIG_SCHEMA_VERSION, ClientCommit,
    ClientCommitWarning, ClientFolder, ClientRecord, ClientRootConflict, CloneMethod, Config,
    CrashSummary, DUPLICATE_NAME_SUFFIX, DiagnosticEntry, DiagnosticError, DiagnosticKind,
    DuplicateClient, DuplicatedClient, EulaConfig, FileDisposition, ForcedStop, ImportSource,
    JavaConfig, LaunchState, LockDisposition, MAX_SEARCH_MATCHES, MAX_VIEW_BYTES, MissingFolder,
    NetworkAccess, NewClient, NewServer, PathError, Paths, PreferenceSetting, ReconciledRecord,
    ReconciliationReport, RecordRef, RunningLaunch, SECRET_KEY_MARKERS, SearchMatch, SearchOptions,
    SearchOutcome, ServerCommit, ServerCommitWarning, ServerDefaults, ServerFolder,
    ServerNetworkSettings, ServerOperatorIdentity, ServerOperatorProfile, ServerOperatorSetup,
    ServerPortSelection, ServerRecord, ServerRootConflict, SetAsideNotice, StorageError,
    TechnicalTextSize, TextAnchor, TextView, running_state,
};
