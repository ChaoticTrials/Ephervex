//! Non-blocking bridge from Slint callbacks to `ephervex-core` services.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ephervex_core::ManagedMutationReport;
use ephervex_core::live_diagnostics::TailedLine;
use ephervex_core::{
    App, AuthEvent, AuthObserver, CancellationToken, ClientFolder, ClientModOutcome, ClientModPlan,
    CoreError, CoreResult, CrashReview, DeclaredProject, DiagnosticKind, DiagnosticRequest,
    DiagnosticScope, ErrorChain, EulaStatus, ImportRequest, InstalledHost, IsolationError,
    IsolationPlan, IsolationReconciliation, IsolationReport, IsolationRequest, IsolationSession,
    LaunchAndConnectObserver, LaunchAndConnectOptions, LaunchEvent, LaunchObserver, LaunchOptions,
    LinkedModOutcome, LinkedModPlan, LinkedModRequest, LinkedTargetPlan, LinkedTargetResult,
    LinkedTargetState, LocalModOffer, LocalModPlan, LocalModReport, LoginOptions, ManagedModEntry,
    ManagedModSnapshot, MetadataAvailability, ModInstallPlan, ModMutability, ModMutation,
    ModMutationPlan, ModPresence, ModSearchQuery, MutationRestriction, NewClient, NewServer,
    OperationReporter, OptionalSelection, PackAnalysis, Paths, PlannedOutcome, ProcessMonitor,
    ProgressSink, ProviderFileIdentity, RoundAnswer, SearchOptions, ServerCommit,
    ServerContentSource, ServerCreateOptions, ServerEvent, ServerFolder, ServerNetworkSettings,
    ServerObserver, ServerRecord, SideRequirement, SnapshotOrigin, SnapshotRestoreOptions,
    SuspectReason, WatchTarget,
};
use ephervex_core::{
    ConfiguredPreferences, LanguageAvailability, LanguageEntry, MinecraftPreferencePreview,
    MinecraftPreferenceSnapshot, PreferenceEdit,
};
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute, lookup_attribute_args};
use ephervex_model::{
    AbsentModPolicy, InstanceId, LoaderKind, LoaderSelection, ModOrigin, ModProvider, Operation,
    OperationStatus, ServerId, SnapshotId, StorageClass,
};

use crate::presentation::{
    ActivityPresentation, ImportErrorKind, ImportFailure, ImportPresentation, InstancePresentation,
    ModImpactPresentation, ModRequirementPresentation, ServerPresentation, diagnostic_kind_label,
    diagnostic_scope_label,
};

type CancellationRegistry = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// Which dialog is asking for a published server archive.
///
/// A file dialog is modal to the desktop but not to this application's own
/// state, and both the create-server dialog and the import dialog can name one.
/// The request carries where its answer belongs, so a slow dialog cannot fill
/// in the field the user has since moved on from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerPackTarget {
    /// The create-server dialog.
    NewServer,
    /// The server an import builds alongside its client.
    Import,
}

#[derive(Debug)]
pub enum Command {
    Refresh,
    LoadMinecraftReleases,
    WarmLanguageCatalog(String),
    LoadLoaderVersions {
        form: String,
        loader: LoaderKind,
        minecraft: String,
    },
    /// Records the Minecraft releases to offer first, in the order given.
    SetFavoriteMinecraftVersions(Vec<String>),
    CancelOperation(String),
    /// Replace one instance's own Java and JVM settings. `None` on a field
    /// restores the global one.
    SaveInstanceRuntime {
        instance: InstanceId,
        java: Option<PathBuf>,
        min_memory_mib: Option<u32>,
        max_memory_mib: Option<u32>,
        jvm_args: Vec<String>,
    },
    /// Turns pack-owned mod mutation on or off for one instance.
    SetAllowPackModVersionChanges {
        instance: InstanceId,
        allow: bool,
    },
    /// Clear the runtime evidence a server left behind when its supervisor died.
    RepairServer(ServerId),
    /// Stop the game this launcher started for one instance.
    ///
    /// Addressed by the instance rather than by an operation id, because that
    /// is what the button on a card knows, and because a launch that is still
    /// preparing has no operation id on screen yet. Stopping it then cancels
    /// the preparation, which is what the button means in that state.
    KillInstance {
        instance: InstanceId,
        /// Which of the instance's launches to stop, for one holding more
        /// than one.
        launch: Option<String>,
    },
    /// `generation` is assigned by the controller the instant the attempt
    /// begins (before the file dialog even opens), not once a result comes
    /// back: that is what lets a later result be checked against the attempt
    /// that is still current instead of trusting whatever the backend hands
    /// back.
    PickImport {
        generation: u64,
    },
    /// The same attempt, for an archive the user has already named by dropping
    /// it on the window. No dialog, and no cancellation to report: the choice
    /// was made before this was sent.
    AnalyzeImport {
        generation: u64,
        archive: PathBuf,
    },
    /// Abandons whatever provider resolution is still running for the import
    /// modal: a superseding archive pick already does this on its own before
    /// starting the next one, so this is for the modal closing outright.
    CancelImportAnalysis,
    PickStorageRoot,
    /// Ask for a published server archive to build a server from.
    ///
    /// Carries which dialog asked, because both can name one and the answer
    /// arrives long after the click.
    PickServerPack {
        target: ServerPackTarget,
    },
    CreateClient {
        request_id: String,
        name: String,
        minecraft: String,
        loader: String,
        loader_version: String,
        persistent: bool,
    },
    ImportPack {
        archive: PathBuf,
        name: String,
        persistent: bool,
        /// The optional client components the modal's checkboxes selected, as
        /// analyzed and possibly resolved. Never inferred here: the modal
        /// built it from the same normalized components the core reports.
        selection: OptionalSelection,
        /// Blocked provider files the modal's "Skip" button was used on.
        /// Never inferred here: only the modal's own blocked-file list can
        /// know which decisions the user actually made.
        skip_blocked: Vec<ProviderFileIdentity>,
        /// The attempt this submission belongs to, echoed back on
        /// [`Event::ImportSubmissionFinished`] so the double-submit guard is
        /// only ever released by the completion of this exact submission.
        generation: u64,
        /// The pending card standing in for this import in the instances list,
        /// so its progress can be reported onto that card. `None` when the
        /// list already had a card for this archive.
        request_id: Option<String>,
        /// Whether to build a server from the same pack once the instance is
        /// published. Part of the same operation, so one card reports both.
        create_linked_server: bool,
        /// The pack's own published server files, when the user named them.
        /// The linked server is built from these instead of from the client's
        /// recipe (ADR 0024).
        server_pack: Option<PathBuf>,
    },
    LaunchClient {
        instance: InstanceId,
        /// Play as this account rather than the default. An account reference,
        /// or `None` for the default.
        account: Option<String>,
        /// Run alongside a launch the instance already has, under this handle.
        additional: Option<String>,
    },
    LaunchAndConnect {
        client: InstanceId,
        server: ServerId,
    },
    OpenClientFolder(InstanceId),
    OpenServerFolder(ServerId),
    /// Find out whether duplicating this instance has a question to ask first.
    PlanDuplicate(InstanceId),
    /// A modpack address a link carried. Asks before it downloads, unless the
    /// host has been agreed to before or `consent` carries the answer.
    OpenPackUrl {
        url: String,
        consent: Option<ephervex_core::HostConsent>,
    },
    /// Read what this copy remembers about modpack links.
    LoadLinkSettings,
    /// Stop trusting one site to serve modpacks.
    ForgetPackHost(String),
    /// Find out where making this instance persistent would put it, and which
    /// of its linked servers could come along.
    PlanPromotion(InstanceId),
    PromoteInstance {
        instance: InstanceId,
        servers: Vec<ServerId>,
    },
    DuplicateClient {
        instance: InstanceId,
        absent_mods: AbsentModPolicy,
    },
    DeleteClient {
        id: InstanceId,
        files: bool,
    },
    DeleteServer {
        id: ServerId,
        files: bool,
    },
    LoadInstanceWorkspace(InstanceId),
    LoadSnapshots(InstalledHost),
    CreateSnapshot {
        host: InstalledHost,
        label: String,
    },
    RestoreSnapshot {
        host: InstalledHost,
        snapshot: SnapshotId,
        safety_snapshot: bool,
    },
    RenameSnapshot {
        host: InstalledHost,
        snapshot: SnapshotId,
        label: String,
    },
    DeleteSnapshot {
        host: InstalledHost,
        snapshot: SnapshotId,
    },
    OpenIsolation {
        host: InstalledHost,
        generation: u64,
    },
    LoadIsolation(InstalledHost),
    BeginIsolation {
        host: InstalledHost,
        generation: u64,
        request: u64,
        pinned: Vec<PathBuf>,
        accept_unrecorded: bool,
        accept_missing: bool,
    },
    AnswerIsolation {
        host: InstalledHost,
        generation: u64,
        request: u64,
        answer: RoundAnswer,
    },
    UndoIsolation {
        host: InstalledHost,
        generation: u64,
        request: u64,
    },
    FinishIsolation {
        host: InstalledHost,
        generation: u64,
        request: u64,
    },
    CancelIsolation {
        host: InstalledHost,
        generation: u64,
        request: u64,
    },
    /// Tries again to give the host its session's intended mods.
    RecoverIsolation {
        host: InstalledHost,
        generation: u64,
        request: u64,
    },
    /// Checks a host's recorded files against their recorded digests, and a
    /// client's shared launch files as well.
    VerifyHost {
        host: InstalledHost,
        request: u64,
    },
    /// Replaces what a host is missing or holding wrongly, then checks again.
    RepairHost {
        host: InstalledHost,
        request: u64,
    },
    /// Measures what Ephervex's own directories hold.
    MeasureDiskUsage {
        request: u64,
    },
    /// Shows one of a storage category's own directories.
    OpenStorageLocation {
        category: ephervex_core::StorageCategory,
        index: usize,
    },
    /// Lists what a diagnostic bundle for a host could carry.
    PlanDiagnosticBundle(InstalledHost),
    /// Asks where to save, then writes the named parts to one local archive.
    ExportDiagnosticBundle {
        host: InstalledHost,
        request: u64,
        names: Vec<String>,
    },
    /// Shows the folder an exported bundle was written to.
    OpenBundleFolder(PathBuf),
    LoadModVersions {
        instance: InstanceId,
        path: PathBuf,
    },
    /// Works out what changing or updating one mod would do here and on every
    /// linked server, before anything is written. An empty `version` means the
    /// newest release that fits this client.
    PlanModApply {
        instance: InstanceId,
        path: PathBuf,
        version: String,
    },
    ChangeModVersion {
        instance: InstanceId,
        path: PathBuf,
        version: String,
        servers: Vec<ServerId>,
    },
    /// Abandons a version change that is waiting on a release the user has to
    /// fetch themselves. Nothing has been written by the time that wait
    /// starts, so this simply stops watching.
    CancelModAcquisition,
    UpdateMod {
        instance: InstanceId,
        path: PathBuf,
        servers: Vec<ServerId>,
    },
    PlanModRemoval {
        instance: InstanceId,
        path: PathBuf,
    },
    RemoveMod {
        instance: InstanceId,
        path: PathBuf,
        servers: Vec<ServerId>,
    },
    ChangeLoaderVersion {
        instance: InstanceId,
        version: String,
    },
    SearchMods {
        instance: InstanceId,
        /// Which catalog to search. Explicit rather than an application-wide
        /// setting: the answer, and every id in it, belongs to one catalog.
        provider: ModProvider,
        query: String,
        /// How far into the result list this page starts. Non-zero only when
        /// the user asked for more of a list they are already looking at.
        offset: u32,
        /// How many hits this one request may take. Clamped by the model to
        /// what a provider may be asked for at once, which is a request budget
        /// rather than a display preference: deciding compatibility costs one
        /// version lookup per hit.
        limit: u8,
        /// Opaque frontend bookkeeping, echoed back unchanged. Two requests
        /// can otherwise carry an identical instance, provider, query, and
        /// offset (a search resubmitted after switching catalog away and
        /// back), and this is what still tells their answers apart. Never
        /// read by a provider or recorded anywhere.
        request_id: u64,
    },
    PlanMod {
        instance: InstanceId,
        provider: ModProvider,
        project: String,
    },
    InstallMod {
        instance: InstanceId,
        provider: ModProvider,
        project: String,
        /// The exact release the confirmation showed. Empty only when nothing
        /// resolved one, in which case the newest that fits is installed.
        version: String,
        servers: Vec<ServerId>,
    },
    CreateServer {
        request_id: String,
        name: String,
        client: Option<InstanceId>,
        minecraft: String,
        loader: String,
        loader_version: String,
        persistent: bool,
        port: Option<u16>,
        /// A published server archive to install as the server's content,
        /// instead of reconstructing it from a client's recipe.
        server_pack: Option<PathBuf>,
    },
    StartServer(ServerId),
    StopServer(ServerId),
    /// Stops a running client or server, waits until it has stopped, and
    /// starts it again the way it was started. `request` identifies this
    /// restart in the stage events it produces.
    RestartHost {
        host: InstalledHost,
        request: u64,
    },
    KillServer(ServerId),
    SendConsole {
        server: ServerId,
        command: String,
    },
    LoadServerWorkspace(ServerId),
    LoadServerMods(ServerId),
    /// Turn off every mod the newest crash report stated, in one step.
    DisableSuggestedMods(ServerId),
    /// Ask a provider about this host's mods again, even though this run
    /// already has.
    RefreshMods(InstalledHost),
    /// What this launcher is running, with live resource use.
    LoadRunning,
    /// End a launch this process is not supervising, and release its lock.
    ForceStopLaunch {
        instance: InstanceId,
        /// Which of the instance's launches, for one running more than one.
        launch: Option<String>,
    },
    /// Turn one mod on or off in place, on either kind of host.
    ///
    /// Asks what else the change affects first. With nothing affected, or with
    /// the confirmation turned off in settings, it applies straight away; the
    /// answer still reaches the user, as a dialog in the first case and a
    /// toast in the second.
    SetModEnabled {
        host: InstalledHost,
        path: PathBuf,
        enabled: bool,
    },
    /// Stop asking before a mod change that affects other mods.
    ///
    /// Sent from the dialog's own "do not ask again", so the decision is
    /// recorded where the user made it rather than only in settings.
    SaveDependencyWarning(bool),
    /// Stop acting on what a mod change affects, or start again.
    SaveDependencyFollowing(bool),
    /// Ask each catalog what this host's mods need, and record the answers.
    ///
    /// `reload` is the mod whose own page is open, so the answer it is showing
    /// is replaced by the one that was just recorded rather than staying stale
    /// behind the toast that says the refresh worked.
    RefreshModDependencies {
        host: InstalledHost,
        reload: Option<PathBuf>,
    },
    /// What one mod declares and what declares it. Offline, so it answers
    /// while the catalog call beside it is still in flight.
    LoadModRelations {
        host: InstalledHost,
        path: PathBuf,
    },
    /// The same answer again, once the catalog has been asked about the
    /// projects the mod declares and the host does not have.
    ///
    /// Sent beside `LoadModRelations` rather than instead of it: the offline
    /// answer paints the lists immediately, and this one replaces "not looked
    /// up" with what the catalog actually said.
    ResolveDeclaredRelations {
        host: InstalledHost,
        path: PathBuf,
    },
    /// Delete several mods' jars in one call, in dependency order.
    ///
    /// What the dialog sends once the user has agreed that taking one jar off
    /// disk takes the ones that need it too.
    DeleteModFiles {
        host: InstalledHost,
        paths: Vec<PathBuf>,
    },
    /// Turn several mods on or off in one call, in dependency order.
    ///
    /// What the dialog sends once the user has agreed to a cascade.
    SetModsEnabled {
        host: InstalledHost,
        changes: Vec<(PathBuf, bool)>,
    },
    /// Work out what deleting one mod's jar would do to the rest.
    QueryDeleteImpact {
        host: InstalledHost,
        path: PathBuf,
        name: String,
    },
    /// Work out what dropping these files into a host's mods folder would do.
    ///
    /// Writes nothing. Its answer is what the name-clash question is asked
    /// from, so the decision is made by the person who can make it rather than
    /// by a thread that cannot ask.
    PlanLocalMods {
        host: InstalledHost,
        sources: Vec<PathBuf>,
    },
    /// Copy these files into a host's mods folder and reload the list.
    ///
    /// Only what the user agreed to: anything they chose to skip never gets
    /// here. `skipped` is carried through so the result can say so.
    AddLocalMods {
        host: InstalledHost,
        sources: Vec<PathBuf>,
        skipped: usize,
    },
    /// Delete one jar the user put in the mods folder themselves.
    /// Put a deleted mod's jar back from what the recipe already declares.
    RestoreModFile {
        host: InstalledHost,
        path: PathBuf,
    },
    DeleteHandPlacedMod {
        host: InstalledHost,
        path: PathBuf,
    },
    /// Change what an instance or a server is called.
    RenameClient {
        id: InstanceId,
        name: String,
    },
    RenameServer {
        id: ServerId,
        name: String,
    },
    OpenLiveLog(LiveLogSource),
    CloseLiveLog,
    SaveServerProperties {
        server: ServerId,
        network: ServerNetworkSettings,
        values: Vec<(String, String)>,
    },
    AcceptEula,
    RevokeEula,
    OpenEulaReference,
    LoadDiagnostics {
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    },
    OpenDiagnostic {
        scope: DiagnosticScope,
        kind: DiagnosticKind,
        name: String,
    },
    SearchDiagnostic {
        scope: DiagnosticScope,
        kind: DiagnosticKind,
        name: String,
        query: String,
    },
    OpenDiagnosticFolder {
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    },
    AddOfflineAccount(String),
    SetDefaultAccount(String),
    RemoveAccount(String),
    MicrosoftLogin,
    SaveApiSettings {
        client_id: String,
        user_agent: String,
    },
    ResetMicrosoftClientId,
    ResetUserAgent,
    /// Store a CurseForge API key, replacing any existing one.
    ///
    /// Carries the value the user just typed and nothing else: there is no
    /// command that reads a stored key back, because nothing in the frontend is
    /// allowed to have one.
    SaveCurseForgeApiKey(String),
    RemoveCurseForgeApiKey,
    OpenCurseForgeKeyHelp,
    SaveStorageRoot(String),
    PickWatchedFolder,
    /// Choose jars to add to a host's mods folder from a file dialog.
    ///
    /// The same act as dropping them, and the way in when a drag cannot be
    /// made: a Wayland session delivers no file drops at all, and a drag out of
    /// an application that offers no file path is refused by the windowing
    /// layer before this process sees it.
    PickLocalMods(InstalledHost),
    /// Replaces the whole configured watched-folder list. Sent with the full
    /// list after every add or remove, mirroring the settings screen's other
    /// list-shaped settings rather than diffing one entry at a time.
    SaveWatchedDownloadFolders(Vec<String>),
    /// Measure the reusable artifact cache.
    ///
    /// Its own command rather than part of the ordinary snapshot: measuring walks
    /// the store, and a list of instances has no reason to pay for that. Sent
    /// when the storage settings are opened and after anything that changes the
    /// cache.
    MeasureArtifactCache,
    SaveArtifactCacheLimit {
        /// The index the limit selector is on. See
        /// `presentation::cache_limit_choices`.
        selection: usize,
        custom_value: String,
        custom_unit: String,
    },
    ClearArtifactCache,
    /// Record which catalog the mod browser opens on, or clear the choice.
    SaveDefaultModCatalog {
        catalog: Option<ModProvider>,
    },
    /// Everything the General settings page edits, saved together.
    SaveGeneralSettings {
        text_scale: ephervex_core::ApplicationTextScale,
        technical_text_size: ephervex_core::TechnicalTextSize,
        color_scheme: ephervex_core::ApplicationColorScheme,
        show_globally: bool,
        make_operator: bool,
        operator_level: ephervex_model::OperatorPermissionLevel,
    },
    LoadJavaRuntimes,
    RefreshJava,
    /// The canonical executable of the runtime to remove.
    RemoveManagedJavaRuntime(String),
    SaveJavaSettings {
        auto_detect: bool,
        auto_download: bool,
        minimum_mib: u32,
        maximum_mib: u32,
        preferred_path: String,
    },
    /// Applies one whole draft of global Minecraft defaults, or none of it.
    ///
    /// A batch rather than a row: core validates every edit before writing
    /// anything, so a draft carrying one unusable value leaves the
    /// configuration exactly as it was.
    SaveMinecraftPreferences {
        edits: Vec<PreferenceEdit>,
    },
    /// Resolves what one exact release would do with a draft nobody has saved.
    ///
    /// `generation` is minted by the controller when the request is sent, so a
    /// late answer for a release the user has already moved off can be
    /// discarded instead of being rendered over the current one.
    PreviewMinecraftPreferences {
        generation: u64,
        minecraft: String,
        draft: ConfiguredPreferences,
        language: LanguageAvailability,
    },
    /// Resolves one release's own language catalog for the settings screen.
    ///
    /// Carries a `generation` for the same staleness reason as the preview.
    LoadMinecraftLanguages {
        generation: u64,
        minecraft: String,
    },
}

/// What a log view is currently showing.
///
/// Not `Copy`, because a file is named rather than numbered. The variants are
/// compared whole, which is what keeps a late update for one source out of a
/// view that has since been pointed at another.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiveLogSource {
    Client(InstanceId),
    Server(ServerId),
    /// One log file, opened from a diagnostics list.
    ///
    /// A file that has finished being written has no live half, but it is the
    /// same text carrying the same levels, and it is read for the same reasons.
    /// Routing it through the retained log is what gives a saved log the level
    /// filters, the search, and the copy that the live view has.
    File {
        scope: DiagnosticScope,
        /// The entry name a listing gave out. Never a path.
        name: String,
    },
}

impl LiveLogSource {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Client(_) => "client",
            Self::Server(_) => "server",
            Self::File { .. } => "file",
        }
    }

    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Client(id) => id.to_string(),
            Self::Server(id) => id.to_string(),
            Self::File { name, .. } => name.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum LiveLogLine {
    Client(TailedLine),
    Server(ephervex_core::CapturedLine),
    /// One line of an opened file, whose level is read from the text itself.
    File(String),
}

#[derive(Debug, Clone)]
pub struct LiveLogUpdate {
    pub source: LiveLogSource,
    pub reset: bool,
    pub missing: bool,
    pub source_truncated: bool,
    pub lines: Vec<LiveLogLine>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub instances: Vec<InstancePresentation>,
    pub servers: Vec<ServerPresentation>,
    pub accounts: Vec<AccountData>,
    /// Every shipped product preference and what is configured for it.
    pub minecraft_preferences: MinecraftPreferenceSnapshot,
    pub settings: SettingsData,
    pub eula: EulaData,
}

#[derive(Debug, Clone)]
pub struct AccountData {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub is_default: bool,
    pub credential_state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingOrigin {
    #[default]
    Default,
    Override,
}

impl SettingOrigin {
    #[must_use]
    pub const fn is_override(self) -> bool {
        matches!(self, Self::Override)
    }
}

#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SettingsData {
    pub storage_root: String,
    pub persistent_root: String,
    pub default_persistent_root: String,
    pub persistent_root_origin: SettingOrigin,
    /// Folders a download watcher checks for a manually placed copy of a
    /// file its provider refused to serve, as absolute paths. Never empty in
    /// practice unless the user explicitly cleared the list: an unconfigured
    /// list already resolves to the OS Downloads folder before it gets here.
    pub watched_download_folders: Vec<String>,
    pub microsoft_client_id: String,
    pub effective_microsoft_client_id: String,
    pub microsoft_client_id_origin: SettingOrigin,
    pub user_agent: String,
    pub effective_user_agent: String,
    pub user_agent_origin: SettingOrigin,
    /// Which CurseForge key is in effect.
    ///
    /// A state and nothing else. There is deliberately no field the key itself
    /// could travel in, so no view can render it and no snapshot can carry it
    /// into a log or a crash report.
    pub curseforge_key: ephervex_core::CurseForgeKeyState,
    /// Whether this build carries a key of its own, whatever is configured.
    pub curseforge_default_key_available: bool,
    /// The catalog the mod browser opens on, as a provider id, or empty when
    /// the recorded name is one this build cannot resolve.
    ///
    /// Policy only. It says which catalog a user wants to look in first and
    /// carries nothing about reaching it: `curseforge_key_present` above is a
    /// separate fact, from the keyring rather than from this setting.
    pub default_mod_catalog: String,
    /// The catalog name exactly as recorded, empty when nothing is recorded.
    ///
    /// Shown rather than resolved so a name written by a newer Ephervex is
    /// reported as itself instead of appearing as one of this build's catalogs.
    pub configured_mod_catalog: String,
    /// Whether the user chose a catalog, as opposed to taking the default.
    pub mod_catalog_origin: SettingOrigin,
    pub text_scale_percent: u16,
    pub technical_text_size_pixels: u16,
    /// Which color scheme the interface paints in, as its stable identifier.
    pub color_scheme: String,
    pub show_linked_servers: bool,
    /// Whether a mod change acts on the mods its relations name.
    pub follow_mod_dependencies: bool,
    /// Whether a mod change that affects other mods stops to be confirmed.
    pub warn_on_dependency_impact: bool,
    /// Minecraft releases the user pinned, in the order they should be offered.
    /// Kept exactly as recorded, including an entry no current release matches.
    pub favorite_minecraft_versions: Vec<String>,
    pub make_default_account_operator: bool,
    pub operator_permission_level: u8,
    pub java_auto_detect: bool,
    pub java_auto_download: bool,
    pub java_memory_min: u32,
    pub java_memory_max: u32,
    pub java_preferred_path: String,
    /// The artifact-cache budget as the selector shows it. Cheap to read: it
    /// comes from the configuration file, never from a scan of the cache.
    pub artifact_cache_limit_labels: Vec<String>,
    pub artifact_cache_limit_index: usize,
    pub artifact_cache_custom_index: usize,
    pub artifact_cache_units: Vec<String>,
    pub artifact_cache_custom_value: String,
    pub artifact_cache_custom_unit: String,
    /// The budget as a sentence names it, so the usage line can read
    /// "7.4 GiB of 10 GiB" without the view doing arithmetic.
    pub artifact_cache_limit_display: String,
}

#[derive(Debug, Clone)]
pub struct EulaData {
    pub accepted: bool,
    pub status: String,
}

/// One row of the Managed Mods list.
///
/// Everything here is already resolved to display text by the presentation
/// layer, and everything that could be absent is either a localized fallback or
/// an empty string that the view is expected to skip. No field is invented: an
/// unknown version says so rather than borrowing the file name.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct ManagedModData {
    pub project_id: String,
    /// Whether the jar is gone while the instance still wants the mod.
    pub deleted: bool,
    /// Where the mod lives inside the game directory. The durable, provider-
    /// neutral key every mutation is addressed by, and the one thing that names
    /// an entry with no catalog identity at all.
    pub logical_path: String,
    /// The project title, or the jar's file name when nothing better is known.
    pub title: String,
    /// The jar's own name, shown as the secondary line when it differs.
    pub file_name: String,
    pub version: String,
    pub version_known: bool,
    pub provider: String,
    /// The catalog's own identifier behind `provider`, or empty for an entry
    /// with no catalog identity. `provider` is what a reader sees; this is what
    /// the row is colored by.
    pub provider_kind: String,
    pub origin: String,
    /// `pack`, `user`, `dependency`, for the row's accent.
    pub origin_kind: String,
    /// Minecraft versions and loaders the installed release declares, when the
    /// provider reported them.
    pub compatibility: String,
    pub author: String,
    /// A cached icon file, or empty for the fallback glyph.
    pub icon: String,
    /// Whether the loader will load this mod, as opposed to it sitting in
    /// `mods/` under the suffix that hides it.
    pub enabled: bool,
    pub pack_owned: bool,
    /// Whether the row opens anything at all. False for an entry with no
    /// project, which has neither versions to show nor anything to change.
    pub can_manage: bool,
    pub can_change_version: bool,
    pub can_update: bool,
    pub can_remove: bool,
    /// Whether this row offers to delete the jar. True only for one the user
    /// put in `mods/` themselves.
    pub can_delete_file: bool,
    /// Why the mutating actions are unavailable, already localized. Empty when
    /// they are available.
    pub restriction: String,
    /// Why the newest crash report points at this mod, already localized.
    /// Empty when it does not.
    pub suspect_reason: String,
    /// `high`, `medium`, or `low`, so the row can weight what it says. Empty
    /// when this mod is not a suspect.
    pub suspect_confidence: String,
}

/// What dropping a set of files onto one host's mod list would do.
///
/// The host is carried so a late answer for a list the user has since navigated
/// away from is discarded rather than asked about, exactly as the mod list's
/// own results are.
#[derive(Debug, Clone)]
pub struct LocalModPlanData {
    pub host: String,
    pub host_kind: String,
    /// Files that can be added with nothing to ask about.
    pub ready: Vec<PathBuf>,
    /// Files whose name is already taken, in the order they were dropped.
    pub clashing: Vec<LocalModClashData>,
    /// Files that cannot be added, with the reason already localized.
    pub refused: Vec<LocalModRefusalData>,
}

/// One dropped file whose name something in the folder already has.
#[derive(Debug, Clone)]
pub struct LocalModClashData {
    pub source: PathBuf,
    pub file_name: String,
    /// Whether the jar already there is one the loader is ignoring, which is
    /// worth saying: replacing it turns the mod back on.
    pub existing_is_disabled: bool,
}

/// The recorded mods an instance has no jar for, as a prompt needs them.
///
/// Empty means there is nothing to ask, which is the common case and the one
/// that has to stay a single click.
#[derive(Debug, Clone)]
pub struct AbsentModsData {
    pub instance: String,
    pub names: Vec<String>,
}

/// The modpack-link settings, as the settings page needs them.
///
/// Whether the scheme is claimed at all is not here: that is settled by
/// installing, and the desktop application offers no control over it (ADR
/// 0028).
#[derive(Debug, Clone)]
pub struct LinkSettingsData {
    /// The sites agreed to serve modpacks.
    pub trusted_hosts: Vec<String>,
}

/// What making one instance persistent would move, as the prompt needs it.
#[derive(Debug, Clone)]
pub struct PromotionPlanData {
    pub instance: String,
    pub name: String,
    pub destination: String,
    /// Every temporary linked server, by id and display name. Each is offered
    /// with its own answer rather than the set being one yes or no.
    pub servers: Vec<(String, String)>,
}

/// One dropped file that cannot go in, and why, in the words a user reads.
#[derive(Debug, Clone)]
pub struct LocalModRefusalData {
    pub file_name: String,
    pub reason: String,
}

/// One thing this launcher is running, as the Running page shows it.
#[derive(Debug, Clone)]
pub struct RunningData {
    /// `"client"` or `"server"`, which decides where every action goes.
    pub kind: String,
    pub id: String,
    /// Which of that entity's launches this row is, empty for the first and
    /// for every server.
    pub launch: String,
    pub name: String,
    /// The process id, or empty where there is not one yet.
    pub pid: String,
    /// How long it has been running, already localized.
    pub uptime: String,
    /// Live CPU and memory, already localized. Empty until the second sample,
    /// because CPU is a rate and the first poll has nothing to compare against.
    pub usage: String,
    /// The most recent line of its output.
    pub last_line: String,
    /// Whether commands can be sent to it. Servers only.
    pub controllable: bool,
    /// Whether this run can be stopped the ordinary way: the run that owns it
    /// is this one, so cancelling it kills the game and waits for its exit.
    pub can_stop: bool,
}

/// What a host's newest crash report says.
///
/// Sent on its own rather than with the mods, because the workspace banner has
/// to be readable before anyone opens the Mods tab, and opening that tab is
/// what asks for the list. Addressed by host for the same reason the mods list
/// is: a late answer must not be drawn under another host's name.
#[derive(Debug, Clone)]
pub struct CrashNoticeData {
    pub host: String,
    /// `"client"` or `"server"`.
    pub host_kind: String,
    /// The crash report this was read from. Empty when the host has never
    /// crashed, which is what clears a banner left by a previous selection.
    pub report: String,
    /// The failure the report stated, exact.
    pub failure: String,
    /// Whether a missing class proved that client-only code ran on a server.
    pub client_code_on_server: bool,
    /// How many mods a bulk "disable the suggested mods" would act on. Zero
    /// when the report named nothing that is still enabled.
    pub suggested: usize,
}

/// One release of a managed mod's project, as the version picker shows it.
#[derive(Debug, Clone)]
pub struct ModVersionData {
    pub version_id: String,
    pub name: String,
    pub number: String,
    pub channel: String,
    pub published: String,
    /// The release the instance is running now.
    pub installed: bool,
    /// The release an update would move to.
    pub newest: bool,
    /// The project's author does not allow other launchers to download this
    /// release. Shown on the row rather than used to hide it: a copy already
    /// in the shared cache installs the ordinary way.
    pub restricted: bool,
}

/// The releases a version change is waiting on the user to fetch.
///
/// The same rows the import modal's blocked-file list shows, because it is
/// the same situation and the same folder watcher resolves it.
#[derive(Debug, Clone, Default)]
pub struct ModAcquisitionData {
    pub instance: String,
    pub logical_path: String,
    /// The mod the user acted on, by name.
    pub name: String,
    pub items: Vec<crate::presentation::BlockedFilePresentation>,
}

/// Everything the manage-a-mod view needs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct ModVersionsData {
    pub instance: String,
    pub logical_path: String,
    pub title: String,
    pub installed_version: String,
    pub versions: Vec<ModVersionData>,
    pub can_change_version: bool,
    pub can_update: bool,
    pub can_remove: bool,
    /// Localized: a refusal reason, or the reason the list is empty.
    pub notice: String,
    pub up_to_date: bool,
    /// The project's page on its provider's site. Empty when none could be
    /// built (e.g. a CurseForge project with no known slug).
    pub project_url: String,
    pub is_pack_owned: bool,
    /// The pack's originally-declared release, when one is on record and
    /// differs from what is installed now. Empty when there is nothing to
    /// reset to.
    pub pack_original_version: String,
}

/// What one mod's own page says about its relations.
#[derive(Debug, Clone, Default)]
pub struct ModRelationsData {
    pub host: String,
    pub logical_path: String,
    pub view: crate::presentation::ModRelationPresentation,
}

/// What a removal would take with it, before it is carried out.
#[derive(Debug, Clone, Default)]
pub struct ModRemovalPlanData {
    pub instance: String,
    pub logical_path: String,
    pub name: String,
    /// The automatic dependencies that would go too, by display name.
    pub dependencies: Vec<String>,
    /// The catalog could not confirm which dependencies are still needed, so
    /// none are being collected.
    pub dependencies_unprovable: bool,
    pub linked: LinkedSectionData,
}

/// One linked server, or one finished target, as a row.
///
/// The same shape serves the confirmation and the result summary: both list
/// servers by stable id with a short state and a sentence explaining it, and
/// having one row type is what keeps the two from describing the same target
/// differently.
#[derive(Debug, Clone, Default)]
pub struct LinkedTargetData {
    /// The server's stable id. Never a list position: a server deleted while a
    /// confirmation is open must not let the next one inherit its row.
    pub id: String,
    pub name: String,
    /// The server's own exact Minecraft release and loader build.
    pub detail: String,
    /// The short state label.
    pub state: String,
    /// Why, or what it would involve.
    pub explanation: String,
    pub tone: String,
    pub actionable: bool,
    /// Whether `state` is a refusal rather than what this target would do.
    pub blocked: bool,
    pub selected: bool,
}

/// The linked-server half of a mod confirmation.
///
/// Carries what the frontend has to send back to apply the same operation, so
/// the decision the user made on this screen is the one that is carried out.
#[derive(Debug, Clone, Default)]
pub struct LinkedSectionData {
    pub instance: String,
    /// The operation, as `ephervex-core` names it.
    pub kind: String,
    pub path: String,
    /// The exact release every selected server would receive.
    pub version: String,
    pub name: String,
    pub version_number: String,
    /// A sentence about what the project declares for the server side.
    pub support: String,
    pub targets: Vec<LinkedTargetData>,
}

/// The per-target result of one finished linked operation.
#[derive(Debug, Clone)]
pub struct LinkedResultData {
    pub title: String,
    pub summary: String,
    pub rows: Vec<LinkedTargetData>,
}

/// The Managed Mods list plus whatever has to be said about how complete it is.
/// One instance's own runtime settings, and what it would otherwise inherit.
#[derive(Debug, Clone)]
pub struct InstanceRuntimeData {
    pub instance: String,
    /// Empty where the instance follows the global setting.
    pub java: String,
    pub min_memory: String,
    pub max_memory: String,
    pub jvm_args: String,
    /// Whether anything is pinned, which is what makes "follow global" an
    /// action worth offering.
    pub pinned: bool,
    /// An already-localized sentence naming what the global settings are, so
    /// an empty field reads as a choice rather than as missing information.
    pub global: String,
    pub allow_pack_mod_version_changes: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ManagedModsData {
    /// Which instance or server this list describes.
    ///
    /// Carried because the enriched list arrives after the durable one, and by
    /// then the user may be looking at a different one. Without it the frontend
    /// cannot tell a late answer from the current one, and would show one
    /// host's mods under another host's name.
    pub host: String,
    /// `"client"` or `"server"`, so a late answer for a server cannot be
    /// mistaken for one about the instance it is linked to.
    pub host_kind: String,
    pub mods: Vec<ManagedModData>,
    /// A localized note when provider metadata could not be fetched, empty
    /// otherwise. Never a reason to withhold the list.
    pub notice: String,
}

#[derive(Debug, Clone)]
pub struct ModSearchData {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub author: String,
    /// A local PNG for the project, or empty when there is none to draw. The
    /// same cached artwork the Managed Mods list uses, so a mod looks the same
    /// on both sides of the panel.
    pub icon: String,
    /// A short localized note about the result, such as whether it fits this
    /// instance. The catalogs do not publish one comparable statistic between
    /// them, and inventing a number here would only look like one.
    pub note: String,
}

/// One page of search results, and the request it answers.
///
/// Each search runs on its own thread, so two of them can finish in either
/// order. Carrying the instance and the query is what lets the frontend tell an
/// answer to the question it is currently asking from an answer to one it has
/// moved on from.
#[derive(Debug, Clone)]
pub struct ModResultsData {
    pub instance: String,
    /// The catalog this page came from, echoed back for the same reason the
    /// query is: switching catalog asks a new question, and the old answer
    /// must not land on it.
    pub provider: String,
    pub query: String,
    /// The offset this page starts at. A page after the first is appended to
    /// what is on screen rather than replacing it.
    pub offset: u32,
    pub rows: Vec<ModSearchData>,
    /// How many results the catalog says match. Informational: whether another
    /// page is worth a request is decided by `next_offset`, not this count.
    pub total: u64,
    /// Where the provider says its next page begins, `None` once there is no
    /// more. Read from how much of the raw page the provider consumed, never
    /// from how many of `rows` this launcher went on to keep.
    pub next_offset: Option<u32>,
    /// The request this answers, echoed back unchanged.
    pub request_id: u64,
    /// A localized sentence when the search failed, empty otherwise.
    ///
    /// Shown in the browser rather than only as a toast: a missing API key, a
    /// rejected one, and an unreachable catalog are all states of the panel the
    /// user is looking at, and a toast that has faded leaves an empty list with
    /// no explanation.
    pub notice: String,
}

#[derive(Debug, Clone, Default)]
pub struct ModPlanData {
    pub instance: String,
    /// The catalog the review asked, echoed back beside the reference. Two
    /// catalogs can spell a project id the same way, so the reference alone
    /// does not identify the question.
    pub provider: String,
    /// The reference the review asked about, echoed back. Two reviews can be in
    /// flight at once and finish in either order, so an answer says which
    /// question it answers.
    pub requested: String,
    /// The provider's own project id, resolved once at plan time. A human-typable
    /// slug becomes this before anything is keyed on it.
    pub project: String,
    pub title: String,
    /// The release's published number, for reading.
    pub version: String,
    /// The release's provider id, which is what applying this confirmation
    /// installs. Kept apart from the number above because one is for a person
    /// and the other is an identifier.
    pub version_id: String,
    pub install_count: usize,
    pub dependency_count: usize,
    pub optional_count: usize,
    pub already_installed_count: usize,
    pub warning_count: usize,
    pub linked: LinkedSectionData,
}

#[derive(Debug, Clone)]
pub struct DiagnosticData {
    pub scope_kind: String,
    pub scope_id: String,
    pub entity: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub modified: String,
    pub size: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct DiagnosticContent {
    pub title: String,
    pub path: String,
    pub text: String,
    pub truncated: bool,
    pub lossy: bool,
    pub matches: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SnapshotData {
    pub id: String,
    pub label: String,
    pub captured: String,
    pub size: String,
    pub mods: String,
    pub origin: String,
    pub copy_method: String,
    pub uses_full_space: bool,
}

#[derive(Debug, Clone)]
pub struct HostSnapshotsData {
    pub host: String,
    pub host_kind: String,
    pub rows: Vec<SnapshotData>,
    pub total: String,
    pub has_plain_copies: bool,
}

/// One recorded file, as a verification found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFileData {
    pub path: String,
    /// The core answer, by its stable identifier.
    pub integrity: &'static str,
    /// Nothing is wrong with it, which includes having nothing to prove.
    pub sound: bool,
    /// A repair would replace it, because it is wrong and has an address.
    pub repairable: bool,
}

/// What checking a client's shared launch files found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedArtifactData {
    /// Nothing is cached for the release yet, so nothing could be checked.
    pub metadata_cached: bool,
    pub checked: usize,
    /// Present with the wrong bytes, or unreadable.
    pub faults: Vec<VerifiedFileData>,
    /// Not downloaded yet, which a launch does and which is not damage.
    pub not_yet_fetched: usize,
    pub loader_libraries_uncovered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationData {
    /// Every file the host records, in the order core checked them.
    pub files: Vec<VerifiedFileData>,
    /// `None` for a server, whose shared files core does not check; an error
    /// message when the check itself could not run.
    pub shared: Option<Result<SharedArtifactData, String>>,
}

/// One thing a diagnostic bundle could carry, as the core planner offers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundlePartData {
    /// What to call it, and the handle a selection passes back.
    pub name: String,
    /// The core kind, by its stable identifier.
    pub kind: &'static str,
    pub size_bytes: u64,
}

/// What one bundle export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleExportData {
    pub path: PathBuf,
    pub included: usize,
    /// Chosen and could not be read, such as a log that rotated away.
    pub unreadable: Vec<String>,
    pub bytes: u64,
}

/// What one repair did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepairData {
    pub repaired: Vec<String>,
    /// Attempted and not fixed, so trying again can help.
    pub failed: Vec<String>,
    /// Wrong, with no address to fetch it from again.
    pub unrepairable: Vec<String>,
    /// Launcher scratch a failed rollback left behind for review.
    pub recovery: Vec<String>,
    /// The repair was canceled before it reached every file.
    pub stopped: bool,
}

#[derive(Debug, Clone)]
pub struct IsolationCandidateData {
    pub path: String,
    pub display_path: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct IsolationData {
    pub host: String,
    pub host_kind: String,
    pub active: bool,
    pub phase: String,
    pub round: usize,
    pub granularity: usize,
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    pub candidates: Vec<IsolationCandidateData>,
    pub unrecorded: Vec<String>,
    pub unidentifiable: Vec<String>,
    pub missing: Vec<String>,
    pub linked_servers: Vec<String>,
    pub reduced_confidence: bool,
    pub confidence: String,
    pub remaining_candidates: Vec<String>,
    pub pinned: Vec<String>,
    pub required_dependencies: Vec<String>,
    pub history: Vec<String>,
    pub summary: String,
    /// Why an active session cannot be answered yet: `running` or `failed`.
    pub recovery: String,
    pub recovery_detail: String,
    /// The fingerprint of the plan a setup was computed from.
    pub basis: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ServerWorkspaceData {
    pub motd: String,
    pub max_players: String,
    pub difficulty: String,
    pub gamemode: String,
    pub allow_flight: bool,
    pub pvp: bool,
    pub allowlist: bool,
    pub network: ServerNetworkSettings,
}

#[derive(Debug, Clone)]
pub struct JavaData {
    pub path: String,
    pub version: String,
    pub vendor: String,
    pub origin: String,
    pub compatible: String,
}

#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Box<Snapshot>),
    InstanceRuntime(InstanceRuntimeData),
    /// How a creation shown as a pending card is getting on.
    CreationProgress {
        request_id: String,
        detail: String,
        progress: f32,
        indeterminate: bool,
    },
    MinecraftReleases(Vec<String>),
    /// One save of the global Minecraft defaults reached a terminal outcome.
    ///
    /// Always sent, whichever way the batch ended, because the settings
    /// screen disables itself while a save is in flight and only a completion
    /// may release that. A generic error carries no such promise: it can come
    /// from any unrelated background work, so it must never be read as this
    /// save finishing.
    ///
    /// On success it follows the refreshed snapshot, so a receiver acting on
    /// it has already seen what core confirmed. On failure core wrote nothing
    /// and there is no snapshot to send, so the draft the user still has is
    /// the correct thing to keep.
    MinecraftPreferencesSaveFinished {
        succeeded: bool,
    },
    /// What one exact release would do with the draft that was previewed.
    ///
    /// A receiver must discard this unless `generation` still names the
    /// request it is waiting on: switching release and back mints a new
    /// generation, so the first request's late answer stays refusable even
    /// though its release matches again.
    MinecraftPreferencePreview {
        generation: u64,
        minecraft: String,
        preview: MinecraftPreferencePreview,
    },
    /// One release's language catalog, or `None` when it could not be
    /// resolved. Checked against `generation` exactly like the preview.
    MinecraftLanguages {
        generation: u64,
        minecraft: String,
        languages: Option<Vec<LanguageEntry>>,
    },
    LoaderVersions {
        form: String,
        loader: LoaderKind,
        minecraft: String,
        versions: Vec<String>,
    },
    ManagedMods(ManagedModsData),
    /// What a set of dropped files would do, and what has to be asked first.
    LocalModPlan(LocalModPlanData),
    /// The mods a copy would inherit without their jars, so the user can say
    /// whether the copy should carry them.
    AbsentMods(AbsentModsData),
    /// Where making an instance persistent would put it, and which servers the
    /// user can bring along.
    PromotionPlan(PromotionPlanData),
    /// A link wants a modpack downloaded from a host nobody has agreed to yet.
    PackUrlConsent {
        url: String,
        display: String,
    },
    /// A modpack that arrived over a link, ready for the import dialog.
    ///
    /// The archive is under the temporary root and is the import's to consume.
    PackDownloaded {
        archive: PathBuf,
    },
    /// The sites this copy may fetch a modpack archive from without asking.
    LinkSettings(LinkSettingsData),
    CrashNotice(CrashNoticeData),
    Running(Vec<RunningData>),
    ModVersions(ModVersionsData),
    ModRelations(ModRelationsData),
    ModRemovalPlan(ModRemovalPlanData),
    ModResults(ModResultsData),
    ModPlan(ModPlanData),
    /// A change or update that has linked servers worth asking about.
    LinkedApply(LinkedSectionData),
    /// What one finished operation did to each target it touched.
    LinkedResult(LinkedResultData),
    ServerWorkspace(ServerWorkspaceData),
    LiveLog(LiveLogUpdate),
    Diagnostics {
        rows: Vec<DiagnosticData>,
        scope: String,
        scope_id: String,
        kind: String,
    },
    DiagnosticContent(DiagnosticContent),
    DiagnosticUnavailable(String),
    HostSnapshots(HostSnapshotsData),
    Isolation {
        generation: u64,
        open: bool,
        data: IsolationData,
    },
    /// An isolation change the controller sent has finished, whether or not
    /// it succeeded. Sent after any state it produced.
    IsolationSettled {
        request: u64,
    },
    /// The plan isolation would make for the host now, as a fingerprint. A
    /// setup planned under a different one no longer describes the host.
    IsolationBasis {
        host: InstalledHost,
        basis: u64,
    },
    /// Where one host's restart is. `None` once it has ended, whichever way.
    /// A `request` of 0 is a stage the server reported on its own, which
    /// belongs to whatever restart is tracked for that host.
    HostRestart {
        host: InstalledHost,
        request: u64,
        stage: Option<RestartStage>,
    },
    /// Whether a client launch this run started is still running, which is
    /// what makes it restartable: its launch request is known.
    LaunchContext {
        instance: InstanceId,
        restartable: bool,
    },
    /// What a verification found on one host.
    HostVerified {
        host: InstalledHost,
        request: u64,
        data: VerificationData,
    },
    /// What a repair did. The verification after it says what is on disk now.
    HostRepaired {
        host: InstalledHost,
        request: u64,
        data: RepairData,
    },
    /// A verification or repair the controller sent has finished, whichever
    /// way it ended. Sent after any result it produced.
    IntegritySettled {
        request: u64,
    },
    /// One measurement of Ephervex's storage.
    DiskUsage {
        request: u64,
        report: Box<ephervex_core::DiskUsageReport>,
    },
    /// A measurement the controller asked for has ended, whichever way.
    DiskUsageSettled {
        request: u64,
    },
    /// What a diagnostic bundle for a host could carry.
    BundlePlanned {
        host: InstalledHost,
        parts: Vec<BundlePartData>,
    },
    BundleExported {
        host: InstalledHost,
        request: u64,
        report: BundleExportData,
    },
    BundleExportFailed {
        host: InstalledHost,
        request: u64,
        message: String,
    },
    /// An export the controller sent has finished, including when the save
    /// dialog was closed without choosing a file.
    BundleSettled {
        request: u64,
    },
    JavaRuntimes(Vec<JavaData>),
    Activity(ActivityData),
    Toast {
        message: String,
        error: bool,
    },
    /// What turning a mod off, or deleting its jar, would do to the rest.
    ModImpact(Box<ModImpactPresentation>),
    /// What turning a mod on needs first.
    ModRequirements(Box<ModRequirementPresentation>),
    ModalError(String),
    CloseModal,
    CreationFinished {
        request_id: String,
    },
    /// A client instance now exists, by whichever route made it. Carries the
    /// new instance's own id so the list can put the user in front of what
    /// they just made rather than leaving them on whatever was selected
    /// before.
    ClientCreated {
        instance: String,
    },
    /// One browser install ended, whichever way it ended. The browser installs
    /// serially and needs to know when it may start the next one, which a
    /// success-only report cannot tell it.
    ModInstallFinished {
        instance: String,
        provider: String,
        project: String,
    },
    /// A plan could not be produced. The reason has already been reported
    /// through [`Event::Error`]; this only names the question that will never
    /// be answered, so nothing waits on it forever.
    ModPlanFailed {
        instance: String,
        provider: String,
        requested: String,
    },
    /// The native file dialog for [`Command::PickImport`] closed without a
    /// choice. A receiver must discard this unless `generation` still names
    /// the attempt it is tracking, exactly like [`Event::ImportAnalyzed`].
    ImportPickCanceled {
        generation: u64,
    },
    /// Local analysis of a freshly chosen archive finished.
    ///
    /// Runs inline in the backend's single command loop, so results for one
    /// attempt are delivered in the order they were requested. A receiver
    /// must still discard this unless `generation` still names the attempt
    /// it is tracking: the attempt begins the moment it is chosen, in the
    /// controller, not the moment this event arrives, so a modal closed and
    /// reopened in between must not let a late analysis repopulate it.
    ImportAnalyzed {
        generation: u64,
        archive: PathBuf,
        result: Result<ImportPresentation, ImportFailure>,
    },
    /// Provider resolution for a pending archive finished.
    ///
    /// Runs on its own thread and can complete after a newer archive has
    /// already been picked. A receiver must discard this unless `generation`
    /// still names the attempt it is tracking.
    ImportResolved {
        generation: u64,
        result: Result<ImportPresentation, ImportFailure>,
    },
    /// An operation needs a release the provider does not permit this launcher
    /// to download, and no verified copy is on this machine yet.
    ///
    /// Nothing has been written: the operation stopped before it could be.
    /// The same folder watcher the import modal uses is already running over
    /// these files, and the requested change is applied by itself once one is
    /// found and verified.
    ModAcquisitionRequired(ModAcquisitionData),
    /// A folder watcher found and verified a hand-placed copy of one blocked
    /// file's bytes while the import modal was open on it.
    ///
    /// A receiver must discard this unless `generation` still names the
    /// attempt it is tracking, exactly like [`Event::ImportResolved`]. Does
    /// not by itself change what final import will do - that re-checks the
    /// shared cache independently - it only clears the row from the modal's
    /// own blocked-file list.
    BlockedFileMatched {
        generation: u64,
        identity: ProviderFileIdentity,
    },
    /// An `ImportPack` submission reached a terminal outcome (success,
    /// failure, cancellation, or a setup failure before the operation could
    /// even start). `generation` is the same identity the submission's
    /// attempt was analyzed under: only a receiver still tracking this exact
    /// attempt may release its double-submit guard on this event, so an
    /// unrelated background operation's `Toast`/`ModalError`/`CloseModal`
    /// can never do so by coincidence. A success carries no `CloseModal` of
    /// its own for the same reason: closing is only correct for a receiver
    /// that still owns this attempt, which only the receiver can judge from
    /// `generation` and whichever modal it currently has open.
    ImportSubmissionFinished {
        generation: u64,
        succeeded: bool,
        /// The pending card this attempt was showing on, so it stops standing
        /// in for an instance that now exists, or for one that never will.
        request_id: Option<String>,
    },
    StorageRootPicked(PathBuf),
    ServerPackPicked {
        target: ServerPackTarget,
        path: PathBuf,
    },
    WatchedFolderPicked(PathBuf),
    ArtifactCacheStatus(ArtifactCacheStatusData),
    /// A manual clear has finished without a fresh usage figure to show, because
    /// the clear was refused or the store could not be measured afterwards. The
    /// view still has to stop presenting the clear as running.
    ArtifactCacheClearFinished,
}

/// The reusable artifact cache as the storage settings show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactCacheStatusData {
    /// Already-formatted IEC sizes: the view renders text, it does not do
    /// arithmetic on byte counts.
    pub used: String,
    pub total: String,
    /// Whether the whole store could be measured. A `false` makes `used` a lower
    /// bound, and the view says so rather than presenting it as exact.
    pub exact: bool,
    pub over_budget: bool,
}

#[derive(Debug, Clone)]
pub struct ActivityData {
    pub item: ActivityPresentation,
    pub entity: String,
    pub time: String,
}

pub struct BackendHandle {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

impl std::fmt::Debug for BackendHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackendHandle")
            .finish_non_exhaustive()
    }
}

/// Which hosts have had their mods enriched from a provider during this run.
///
/// A mod's title, artwork and release notes do not change while the launcher is
/// open, so asking again on every workspace open spends a round trip to redraw
/// what is already on screen. The shared cache makes the first paint correct;
/// this keeps the second open from paying for it at all.
///
/// Session state rather than something durable: a restart is exactly when
/// asking again is worth it, and a user who wants it sooner has Refresh.
type RefreshedHosts = Arc<Mutex<HashSet<String>>>;

/// Hosts whose files an asynchronous change is rewriting, each with the
/// message that says which change it is.
///
/// Isolation answers, undo, and finish run on the worker thread itself, so
/// nothing can start in the middle of one. An isolation start or cancel, a
/// repair, and a snapshot capture or restore run beside it, and a launch or a
/// second change then would read or rewrite a directory that is half done.
type ChangingHosts = Arc<Mutex<HashMap<String, &'static str>>>;

/// Where one restart is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartStage {
    /// Asking the running process to stop, and waiting until it has.
    Stopping,
    /// The old process has stopped; the replacement is being started.
    Starting,
}

impl RestartStage {
    /// A stable identifier for the rows every view of the host reads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopping => "stopping",
            Self::Starting => "starting",
        }
    }
}

/// How one restart ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RestartOutcome {
    Restarted,
    /// The old process was not seen to stop, so nothing was started.
    NotStopped,
    /// The old process stopped and its replacement could not be started.
    StartFailed(String),
    /// The restart ended before its start phase, such as when a stop was
    /// asked for while it waited.
    Interrupted(String),
}

/// The exclusive client launches this run started and still supervises, with
/// the request each was made with.
///
/// Runtime only, and holding nothing secret: an account is named by reference,
/// and its credentials stay where every launch reads them from. A launch this
/// run did not start, one made alongside another, and one made by Launch &
/// Connect are not here, so none of them can be restarted.
type RunningLaunches = Arc<Mutex<HashMap<InstanceId, RunningLaunch>>>;

#[derive(Debug, Clone)]
struct RunningLaunch {
    generation: u64,
    account: Option<String>,
    /// Becomes true once the launch has returned: the game has exited and its
    /// lock is released, which is what a restart waits for.
    finished: tokio::sync::watch::Receiver<bool>,
}

/// How long a client restart waits for the old game to be gone. Stopping kills
/// the process, so this bounds only a supervisor that never reports back.
const CLIENT_STOP_WAIT: Duration = Duration::from_secs(60);

fn register_launch(
    launches: &RunningLaunches,
    events: &Sender<Event>,
    instance: InstanceId,
    account: Option<String>,
) -> (u64, tokio::sync::watch::Sender<bool>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let generation = NEXT.fetch_add(1, Ordering::Relaxed);
    let (sender, finished) = tokio::sync::watch::channel(false);
    if let Ok(mut entries) = launches.lock() {
        entries.insert(
            instance,
            RunningLaunch {
                generation,
                account,
                finished,
            },
        );
    }
    let _ = events.send(Event::LaunchContext {
        instance,
        restartable: true,
    });
    (generation, sender)
}

fn unregister_launch(
    launches: &RunningLaunches,
    events: &Sender<Event>,
    instance: InstanceId,
    generation: u64,
) {
    let removed = launches.lock().is_ok_and(|mut entries| {
        let ours = entries
            .get(&instance)
            .is_some_and(|entry| entry.generation == generation);
        if ours {
            entries.remove(&instance);
        }
        ours
    });
    if removed {
        let _ = events.send(Event::LaunchContext {
            instance,
            restartable: false,
        });
    }
}

/// Stops, waits until the stop is observed, then starts exactly once.
///
/// Nothing is started unless `stopped` reports the old process gone, so a stop
/// that fails or hangs can never leave two copies running.
async fn restart_sequence<Start, StartFuture>(
    stop: impl FnOnce(),
    stopped: impl std::future::Future<Output = bool>,
    start: Start,
    stage: &dyn Fn(RestartStage),
) -> RestartOutcome
where
    Start: FnOnce() -> StartFuture,
    StartFuture: std::future::Future<Output = Result<(), String>>,
{
    stage(RestartStage::Stopping);
    stop();
    if !stopped.await {
        return RestartOutcome::NotStopped;
    }
    stage(RestartStage::Starting);
    match start().await {
        Ok(()) => RestartOutcome::Restarted,
        Err(detail) => RestartOutcome::StartFailed(detail),
    }
}

fn host_key(host: InstalledHost) -> String {
    format!("{}:{}", host.kind(), host.id())
}

/// Marks a host as being rewritten until it is dropped, including when the
/// operation never gets to run.
struct HostChangeInFlight {
    hosts: ChangingHosts,
    key: String,
}

impl HostChangeInFlight {
    /// `refusal` is the message anything refused meanwhile is told.
    fn mark(hosts: &ChangingHosts, host: InstalledHost, refusal: &'static str) -> Self {
        let key = host_key(host);
        if let Ok(mut hosts) = hosts.lock() {
            hosts.insert(key.clone(), refusal);
        }
        Self {
            hosts: Arc::clone(hosts),
            key,
        }
    }
}

impl Drop for HostChangeInFlight {
    fn drop(&mut self) {
        if let Ok(mut hosts) = self.hosts.lock() {
            hosts.remove(&self.key);
        }
    }
}

/// Refuses to start a host, or to change it again, while a change is
/// rewriting its files.
fn refuse_while_changing(
    hosts: &ChangingHosts,
    host: InstalledHost,
    events: &Sender<Event>,
) -> bool {
    let refusal = hosts
        .lock()
        .ok()
        .and_then(|hosts| hosts.get(&host_key(host)).copied());
    if let Some(refusal) = refusal {
        let _ = events.send(Event::Toast {
            message: lookup(refusal),
            error: true,
        });
    }
    refusal.is_some()
}

pub fn spawn() -> BackendHandle {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let cancellations = Arc::new(Mutex::new(HashMap::new()));
    let worker_events = event_tx.clone();
    let worker_cancellations = Arc::clone(&cancellations);
    if let Err(error) = thread::Builder::new()
        .name("ephervex-core-worker".to_owned())
        .spawn(move || run_worker(command_rx, worker_events, worker_cancellations))
    {
        tracing::error!(%error, "could not start the GUI core worker");
        let _ = event_tx.send(Event::ModalError(localized_value(
            "gui-background-service-failed",
            &error.to_string(),
        )));
    }
    BackendHandle {
        commands: command_tx,
        events: event_rx,
    }
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn run_worker(
    commands: Receiver<Command>,
    events: Sender<Event>,
    cancellations: CancellationRegistry,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = events.send(Event::ModalError(localized_value(
                "gui-background-runtime-failed",
                &error.to_string(),
            )));
            return;
        }
    };
    let mut app = match App::open() {
        Ok(app) => app,
        Err(error) => {
            let _ = events.send(Event::ModalError(localized_error(&error)));
            return;
        }
    };
    if let Err(error) = app.reconcile() {
        send_error(&events, &error);
    }
    send_snapshot(&app, &events);
    let live_log_generation = Arc::new(AtomicU64::new(0));
    let refreshed_mods: RefreshedHosts = Arc::new(Mutex::new(HashSet::new()));
    let changing: ChangingHosts = Arc::new(Mutex::new(HashMap::new()));
    let launches: RunningLaunches = Arc::new(Mutex::new(HashMap::new()));
    // Kept between polls, because CPU is a rate rather than a reading.
    let mut process_monitor = ProcessMonitor::new();
    // A version change waiting on a release the user has to fetch, so closing
    // the dialog (or starting another one) stops the folder watcher it left
    // running.
    let mut acquisition_cancel: Option<CancellationToken> = None;
    // The still-running resolution's own cancellation, so a superseding pick
    // or a closed modal can stop wasting a network request on an answer
    // nothing will read. The attempt identity itself belongs to the
    // controller (see `Command::PickImport`); this loop only ever cancels by
    // it, it does not assign it.
    let mut import_resolve_cancel: Option<CancellationToken> = None;

    for command in commands {
        match command {
            Command::Refresh => send_snapshot(&app, &events),
            Command::LoadMinecraftReleases => match runtime.block_on(app.minecraft_releases()) {
                Ok(listing) => {
                    let versions = listing
                        .releases
                        .into_iter()
                        .map(|release| release.id)
                        .collect();
                    let _ = events.send(Event::MinecraftReleases(versions));
                }
                Err(error) => send_modal_error(&events, &error),
            },
            Command::WarmLanguageCatalog(minecraft) => {
                warm_language_catalog(app.paths().clone(), &minecraft);
            }
            Command::LoadLoaderVersions {
                form,
                loader,
                minecraft,
            } => match runtime.block_on(app.loader_versions(loader, &minecraft)) {
                Ok(listing) => {
                    let versions = listing
                        .builds
                        .into_iter()
                        .map(|build| build.version)
                        .collect();
                    let _ = events.send(Event::LoaderVersions {
                        form,
                        loader,
                        minecraft,
                        versions,
                    });
                }
                Err(error) => send_modal_error(&events, &error),
            },
            Command::SetFavoriteMinecraftVersions(versions) => {
                // No toast. A star is toggled while looking straight at the
                // list it reorders, so the list itself is the confirmation, and
                // a message per click would be noise. A refused write is still
                // reported, because then nothing on screen would have changed.
                match app.set_favorite_minecraft_versions(versions) {
                    Ok(()) => send_snapshot(&app, &events),
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::CancelOperation(id) => cancel_operation(&cancellations, &events, &id),
            Command::KillInstance { instance, launch } => {
                let supervised = cancellations.lock().is_ok_and(|entries| {
                    entries.contains_key(&launch_key(instance, launch.as_deref()))
                });
                // A game whose launcher run is gone has no supervisor to cancel.
                // The proven force stop is the one way to end it, and it
                // signals nothing it cannot prove is that game.
                let orphaned = launch.is_none()
                    && app
                        .launch_state(instance)
                        .is_ok_and(|state| state.is_some_and(|state| state.is_orphaned()));
                if !supervised && orphaned {
                    match app.force_stop_launch(instance, None) {
                        Ok(_) => {
                            let _ = events.send(Event::Toast {
                                message: lookup("gui-force-stop-signaled"),
                                error: false,
                            });
                            send_snapshot(&app, &events);
                        }
                        Err(error) => send_error(&events, &error),
                    }
                } else {
                    kill_instance(&cancellations, &events, instance, launch.as_deref());
                }
            }
            Command::SaveInstanceRuntime {
                instance,
                java,
                min_memory_mib,
                max_memory_mib,
                jvm_args,
            } => {
                let jvm = ephervex_model::JvmSettings {
                    min_memory_mib,
                    max_memory_mib,
                    jvm_args,
                };
                let result = app.set_client_runtime_settings(
                    instance,
                    java.map(ephervex_model::JavaOverride::new),
                    (!jvm.pins_nothing()).then_some(jvm),
                );
                match result {
                    Ok(_) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-instance-runtime-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    // Shown on the screen the fields are on, not as a toast the
                    // user has to catch: a refused save leaves them looking at
                    // values that are not what the instance has.
                    Err(error) => send_modal_error(&events, &error),
                }
            }
            Command::SetAllowPackModVersionChanges { instance, allow } => {
                // No toast. The toggle itself is the confirmation; a refusal
                // is still reported, because then nothing on screen changed.
                match app.set_allow_pack_mod_version_changes(instance, allow) {
                    Ok(_) => {
                        send_snapshot(&app, &events);
                        // The setting decides what each row may have done to
                        // it, so the list it decides for is re-read here
                        // rather than at the next time the tab happens to be
                        // opened.
                        send_host_mods(
                            &app,
                            InstalledHost::Instance(instance),
                            &events,
                            &refreshed_mods,
                            false,
                        );
                    }
                    Err(error) => send_error(&events, &error),
                }
                // Either way, because the snapshot does not carry this flag:
                // after a refusal the toggle shows what the user asked for
                // rather than what was stored, and it is the only thing on
                // screen saying which it is.
                if let Ok(runtime) = instance_runtime_data(&app, instance) {
                    let _ = events.send(Event::InstanceRuntime(runtime));
                }
            }
            Command::RepairServer(id) => {
                match app.recover_server_runtime(id) {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-server-repaired"),
                            error: false,
                        });
                    }
                    Err(error) => send_error(&events, &error),
                }
                send_snapshot(&app, &events);
            }
            Command::PickImport { generation } => {
                // The attempt begins here, at the controller's request, not
                // at whichever later point this handler happens to finish:
                // any resolution still running for the previous attempt is
                // no longer worth its network request the moment a new pick
                // starts, regardless of whether this one is completed by a
                // choice or a cancellation.
                if let Some(previous) = import_resolve_cancel.take() {
                    previous.cancel();
                }
                match rfd::FileDialog::new()
                    .set_title(lookup("gui-choose-modpack-archive"))
                    .add_filter(lookup("gui-modpack-archives"), &["mrpack", "zip"])
                    .pick_file()
                {
                    Some(path) => {
                        import_resolve_cancel = analyze_import(&app, &events, generation, &path);
                    }
                    None => {
                        let _ = events.send(Event::ImportPickCanceled { generation });
                    }
                }
            }
            Command::AnalyzeImport {
                generation,
                archive,
            } => {
                if let Some(previous) = import_resolve_cancel.take() {
                    previous.cancel();
                }
                import_resolve_cancel = analyze_import(&app, &events, generation, &archive);
            }
            Command::CancelImportAnalysis => {
                if let Some(previous) = import_resolve_cancel.take() {
                    previous.cancel();
                }
            }
            Command::PickStorageRoot => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(lookup("gui-choose-persistent-root"))
                    .pick_folder()
                {
                    let _ = events.send(Event::StorageRootPicked(path));
                }
            }
            Command::PickServerPack { target } => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(lookup("gui-choose-server-pack"))
                    .add_filter(lookup("gui-server-pack-archives"), &["zip"])
                    .pick_file()
                {
                    let _ = events.send(Event::ServerPackPicked { target, path });
                }
            }
            Command::PickLocalMods(host) => {
                if let Some(paths) = rfd::FileDialog::new()
                    .set_title(lookup("gui-choose-mods"))
                    .add_filter(lookup("gui-mod-jars"), &["jar"])
                    .pick_files()
                {
                    match app.plan_local_mods(host, &paths) {
                        Ok(plan) => {
                            let _ =
                                events.send(Event::LocalModPlan(local_mod_plan_data(host, &plan)));
                        }
                        Err(error) => send_error(&events, &error),
                    }
                }
            }
            Command::PickWatchedFolder => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(lookup("gui-choose-watched-folder"))
                    .pick_folder()
                {
                    let _ = events.send(Event::WatchedFolderPicked(path));
                }
            }
            Command::CreateClient {
                request_id,
                name,
                minecraft,
                loader,
                loader_version,
                persistent,
            } => match loader_selection(&loader, &loader_version) {
                Ok(loader) => {
                    let paths = app.paths().clone();
                    let entity = name.clone();
                    let reports = events.clone();
                    spawn_creation_operation(
                        paths,
                        &events,
                        Arc::clone(&cancellations),
                        CreationRequest {
                            id: request_id,
                            title: "operation-creating-instance",
                            entity,
                            report_progress: false,
                        },
                        move |app, _reporter, _cancel| async move {
                            app.create_client_reporting(
                                NewClient::new(name, minecraft, loader)
                                    .with_storage(storage_class(persistent)),
                            )
                            .await
                            .map(|created| {
                                let _ = reports.send(Event::ClientCreated {
                                    instance: created.record.id.to_string(),
                                });
                                CreationOutcome::Created(localized_value(
                                    "gui-created-entity",
                                    &created.record.name,
                                ))
                            })
                        },
                    );
                }
                Err(error) => {
                    send_error(&events, &error);
                    let _ = events.send(Event::CreationFinished { request_id });
                }
            },
            Command::ImportPack {
                archive,
                name,
                persistent,
                selection,
                skip_blocked,
                generation,
                request_id,
                create_linked_server,
                server_pack,
            } => {
                let paths = app.paths().clone();
                spawn_import_submission(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    ImportSubmission {
                        generation,
                        archive,
                        name,
                        persistent,
                        selection,
                        skip_blocked,
                        request_id,
                        create_linked_server,
                        server_pack,
                    },
                );
            }
            Command::LaunchClient {
                instance,
                account,
                additional,
            } => {
                if refuse_while_changing(&changing, InstalledHost::Instance(instance), &events) {
                    continue;
                }
                let paths = app.paths().clone();
                let entity = app
                    .client(instance)
                    .map_or_else(|_| instance.to_string(), |record| record.name);
                spawn_launch(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    Arc::clone(&launches),
                    ClientLaunch {
                        instance,
                        entity,
                        account,
                        additional,
                    },
                    None,
                );
            }
            Command::LaunchAndConnect { client, server } => {
                if refuse_while_changing(&changing, InstalledHost::Instance(client), &events)
                    || refuse_while_changing(&changing, InstalledHost::Server(server), &events)
                {
                    continue;
                }
                let paths = app.paths().clone();
                let entity = app
                    .client(client)
                    .map_or_else(|_| format!("{client} -> {server}"), |record| record.name);
                spawn_connect(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    client,
                    server,
                    entity,
                );
            }
            Command::OpenClientFolder(instance) => {
                report_sync(
                    &events,
                    app.open_client_folder(instance, ClientFolder::Root)
                        .map(|path| {
                            path.map_or_else(
                                || lookup("gui-folder-unavailable"),
                                |path| {
                                    localized_value("gui-opened-path", &path.display().to_string())
                                },
                            )
                        }),
                );
            }
            Command::OpenServerFolder(server) => {
                report_sync(
                    &events,
                    app.open_server_folder(server, ServerFolder::Root)
                        .map(|path| {
                            path.map_or_else(
                                || lookup("gui-folder-unavailable"),
                                |path| {
                                    localized_value("gui-opened-path", &path.display().to_string())
                                },
                            )
                        }),
                );
            }
            Command::PlanDuplicate(instance) => {
                match app.absent_recorded_mods(InstalledHost::Instance(instance)) {
                    Ok(absent) => {
                        let _ = events.send(Event::AbsentMods(AbsentModsData {
                            instance: instance.to_string(),
                            names: absent.into_iter().map(|entry| entry.name).collect(),
                        }));
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::LoadLinkSettings => {
                send_link_settings(&app, &events);
            }
            Command::ForgetPackHost(host) => match app.withdraw_pack_host(&host) {
                Ok(_) => send_link_settings(&app, &events),
                Err(error) => send_error(&events, &error),
            },
            Command::OpenPackUrl { url, consent } => {
                runtime.block_on(open_pack_url(&mut app, &url, consent, &events));
            }
            Command::PlanPromotion(instance) => match app.plan_instance_promotion(instance) {
                Ok(plan) => {
                    let _ = events.send(Event::PromotionPlan(PromotionPlanData {
                        instance: plan.instance.to_string(),
                        name: plan.name,
                        destination: plan.destination.display().to_string(),
                        servers: plan
                            .servers
                            .into_iter()
                            .map(|server| (server.id.to_string(), server.name))
                            .collect(),
                    }));
                }
                Err(error) => send_error(&events, &error),
            },
            Command::PromoteInstance { instance, servers } => {
                promote_instance(&app, instance, servers, &events);
            }
            Command::DuplicateClient {
                instance,
                absent_mods,
            } => {
                match app.duplicate_client(
                    instance,
                    &ephervex_core::DuplicateClient::default(),
                    absent_mods,
                ) {
                    Ok(copy) => {
                        // Before the toast: the dialog that asked the question
                        // has its answer, and leaving it open invites the same
                        // answer again and a second copy nobody asked for.
                        let _ = events.send(Event::CloseModal);
                        let _ = events.send(Event::Toast {
                            message: localized_value("gui-created-entity", &copy.record.name),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::DeleteClient { id, files } => {
                let disposition = if files {
                    ephervex_core::FileDisposition::Remove
                } else {
                    ephervex_core::FileDisposition::Keep
                };
                match app.delete_client(id, disposition) {
                    Ok(()) => {
                        let _ = events.send(Event::CloseModal);
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-instance-deleted"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_modal_error(&events, &error),
                }
            }
            Command::DeleteServer { id, files } => {
                let disposition = if files {
                    ephervex_core::FileDisposition::Remove
                } else {
                    ephervex_core::FileDisposition::Keep
                };
                match app.delete_server(id, disposition) {
                    Ok(()) => {
                        let _ = events.send(Event::CloseModal);
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-server-deleted"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_modal_error(&events, &error),
                }
            }
            Command::ChangeLoaderVersion { instance, version } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result = runtime.block_on(app.change_loader_version(
                        instance,
                        &version,
                        // Never assumed. A known incompatibility comes back as a
                        // refusal the user reads before deciding again.
                        false,
                        &OperationReporter::silent("changing-loader-version"),
                        &CancellationToken::new(),
                    ));
                    match result {
                        Ok(change) => {
                            let _ = events.send(Event::CloseModal);
                            let _ = events.send(Event::Toast {
                                message: localized_value(
                                    "gui-loader-changed",
                                    &change.loader.to_string(),
                                ),
                                error: false,
                            });
                            send_snapshot(&app, &events);
                        }
                        Err(error) => send_modal_error(&events, &error),
                    }
                });
            }
            Command::LoadModVersions { instance, path } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let data = runtime.block_on(mod_versions(&app, instance, &path));
                    let _ = events.send(Event::ModVersions(data));
                });
            }
            Command::PlanModApply {
                instance,
                path,
                version,
            } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let request = version_change_request(&path, &version);
                    let reporter = OperationReporter::silent("planning-mod-change");
                    let cancel = CancellationToken::new();
                    match runtime.block_on(
                        app.plan_linked_mod_operation(instance, &request, &reporter, &cancel),
                    ) {
                        // Nothing to ask about: no linked server could take part
                        // either way, so an empty confirmation would be a click
                        // for its own sake.
                        Ok(plan) if !plan.has_relevant_targets() => {
                            let result = runtime.block_on(app.apply_linked_mod_operation(
                                instance,
                                &planned_request(&plan),
                                &[],
                                &reporter,
                                &cancel,
                            ));
                            finish_linked(&app, &events, instance, result);
                        }
                        Ok(plan) => {
                            let _ = events.send(Event::LinkedApply(linked_section(&plan)));
                        }
                        Err(error) => send_modal_error(&events, &error),
                    }
                });
            }
            Command::ChangeModVersion {
                instance,
                path,
                version,
                servers,
            } => {
                acquisition_cancel = Some(apply_mod_operation(
                    app.paths().clone(),
                    &events,
                    acquisition_cancel.take(),
                    ModOperation {
                        instance,
                        path,
                        version: Some(version),
                        servers,
                        operation: "changing-mod-version",
                    },
                ));
            }
            Command::UpdateMod {
                instance,
                path,
                servers,
            } => {
                acquisition_cancel = Some(apply_mod_operation(
                    app.paths().clone(),
                    &events,
                    acquisition_cancel.take(),
                    ModOperation {
                        instance,
                        path,
                        version: None,
                        servers,
                        operation: "updating-mod",
                    },
                ));
            }
            Command::CancelModAcquisition => {
                if let Some(cancel) = acquisition_cancel.take() {
                    cancel.cancel();
                }
            }
            Command::PlanModRemoval { instance, path } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result = runtime.block_on(app.plan_linked_mod_operation(
                        instance,
                        &LinkedModRequest::Remove { target: path },
                        &OperationReporter::silent("planning-mod-removal"),
                        &CancellationToken::new(),
                    ));
                    match result {
                        Ok(plan) => {
                            let _ = events.send(Event::ModRemovalPlan(removal_plan_data(&plan)));
                        }
                        Err(error) => send_modal_error(&events, &error),
                    }
                });
            }
            Command::RemoveMod {
                instance,
                path,
                servers,
            } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result = runtime.block_on(app.apply_linked_mod_operation(
                        instance,
                        &LinkedModRequest::Remove { target: path },
                        &servers,
                        &OperationReporter::silent("removing-mod"),
                        &CancellationToken::new(),
                    ));
                    finish_linked(&app, &events, instance, result);
                });
            }
            Command::LoadInstanceWorkspace(instance) => {
                // The durable view first, so the list is on screen before
                // anything talks to a provider. Enrichment then replaces it
                // with the same mods plus their titles and artwork; a failure
                // there leaves what is already shown standing.
                match app.managed_mod_snapshot(instance) {
                    Ok(snapshot) => {
                        let host = InstalledHost::Instance(instance);
                        let review = app.crash_review(host).ok().flatten();
                        let _ = events
                            .send(Event::CrashNotice(crash_notice_data(host, review.as_ref())));
                        let _ = events.send(Event::ManagedMods(managed_mods_data(
                            &snapshot,
                            review.as_ref(),
                        )));
                    }
                    Err(error) => {
                        send_error(&events, &error);
                        continue;
                    }
                }

                if let Ok(runtime) = instance_runtime_data(&app, instance) {
                    let _ = events.send(Event::InstanceRuntime(runtime));
                }
                send_isolation_state(
                    &app,
                    InstalledHost::Instance(instance),
                    0,
                    false,
                    &events,
                    &refreshed_mods,
                );
                refresh_enriched_mods(app.paths().clone(), &events, instance);
            }
            Command::LoadSnapshots(host) => match host_snapshots_data(&app, host) {
                Ok(data) => {
                    let _ = events.send(Event::HostSnapshots(data));
                }
                Err(error) => send_error(&events, &error),
            },
            Command::CreateSnapshot { host, label } => {
                if refuse_while_changing(&changing, host, &events) {
                    continue;
                }
                let paths = app.paths().clone();
                let reports = events.clone();
                let in_flight = HostChangeInFlight::mark(&changing, host, "gui-snapshot-busy");
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-creating-snapshot",
                        entity: label.clone(),
                        allow_cancel: true,
                        finished: None,
                    },
                    move |app, _reporter, cancel| async move {
                        let _in_flight = in_flight;
                        app.create_snapshot(host, &label, SnapshotOrigin::Manual, &cancel)
                            .map_err(CoreError::from)?;
                        let data = host_snapshots_data(&app, host)?;
                        let _ = reports.send(Event::HostSnapshots(data));
                        Ok(OperationReport::Toast(lookup("gui-snapshot-created")))
                    },
                );
            }
            Command::RestoreSnapshot {
                host,
                snapshot,
                safety_snapshot,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    continue;
                }
                let paths = app.paths().clone();
                let reports = events.clone();
                let refreshed = Arc::clone(&refreshed_mods);
                let in_flight = HostChangeInFlight::mark(&changing, host, "gui-snapshot-busy");
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-restoring-snapshot",
                        entity: snapshot.to_string(),
                        allow_cancel: true,
                        finished: None,
                    },
                    move |app, _reporter, cancel| async move {
                        let _in_flight = in_flight;
                        app.restore_snapshot(
                            host,
                            snapshot,
                            SnapshotRestoreOptions { safety_snapshot },
                            &cancel,
                        )
                        .map_err(CoreError::from)?;
                        let data = host_snapshots_data(&app, host)?;
                        let _ = reports.send(Event::HostSnapshots(data));
                        // A restore rewrites the game directory underneath the
                        // workspace, so the mods the other tabs are showing are
                        // the ones from before it. Forced, because a restore can
                        // bring back a mod this run has never enriched.
                        send_host_mods(&app, host, &reports, &refreshed, true);
                        // An isolation setup was planned from the mods the
                        // restore replaced. Reading the durable state back
                        // retires that plan, and shows a real session as is.
                        send_isolation_state(&app, host, 0, false, &reports, &refreshed);
                        // It replaces the host's own metadata as well, which is
                        // where a client keeps its Java and JVM overrides and a
                        // server its network settings, and `server.properties`
                        // lives in the restored directory itself.
                        match host {
                            InstalledHost::Instance(instance) => {
                                if let Ok(runtime) = instance_runtime_data(&app, instance) {
                                    let _ = reports.send(Event::InstanceRuntime(runtime));
                                }
                            }
                            InstalledHost::Server(server) => {
                                send_server_workspace(&app, server, &reports);
                            }
                        }
                        Ok(OperationReport::Toast(lookup("gui-snapshot-restored")))
                    },
                );
            }
            Command::RenameSnapshot {
                host,
                snapshot,
                label,
            } => {
                let paths = app.paths().clone();
                let reports = events.clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-renaming-snapshot",
                        entity: label.clone(),
                        allow_cancel: false,
                        finished: None,
                    },
                    move |app, _reporter, cancel| async move {
                        if cancel.is_cancelled() {
                            return Err(CoreError::from(ephervex_core::SnapshotError::Canceled));
                        }
                        app.rename_snapshot(host, snapshot, &label)
                            .map_err(CoreError::from)?;
                        let data = host_snapshots_data(&app, host)?;
                        let _ = reports.send(Event::HostSnapshots(data));
                        Ok(OperationReport::Toast(lookup("gui-snapshot-renamed")))
                    },
                );
            }
            Command::DeleteSnapshot { host, snapshot } => {
                let paths = app.paths().clone();
                let reports = events.clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-deleting-snapshot",
                        entity: snapshot.to_string(),
                        allow_cancel: false,
                        finished: None,
                    },
                    move |app, _reporter, cancel| async move {
                        if cancel.is_cancelled() {
                            return Err(CoreError::from(ephervex_core::SnapshotError::Canceled));
                        }
                        app.delete_snapshot(host, snapshot)
                            .map_err(CoreError::from)?;
                        let data = host_snapshots_data(&app, host)?;
                        let _ = reports.send(Event::HostSnapshots(data));
                        Ok(OperationReport::Toast(lookup("gui-snapshot-deleted")))
                    },
                );
            }
            Command::OpenIsolation { host, generation } => {
                send_isolation_state(&app, host, generation, true, &events, &refreshed_mods);
            }
            Command::LoadIsolation(host) => {
                send_isolation_state(&app, host, 0, false, &events, &refreshed_mods);
            }
            Command::RecoverIsolation {
                host,
                generation,
                request,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IsolationSettled { request });
                    continue;
                }
                send_isolation_state(&app, host, generation, false, &events, &refreshed_mods);
                let _ = events.send(Event::IsolationSettled { request });
            }
            Command::BeginIsolation {
                host,
                generation,
                request,
                pinned,
                accept_unrecorded,
                accept_missing,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IsolationSettled { request });
                    continue;
                }
                let paths = app.paths().clone();
                let reports = events.clone();
                let refreshed = Arc::clone(&refreshed_mods);
                let in_flight = HostChangeInFlight::mark(&changing, host, "gui-isolation-busy");
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-starting-isolation",
                        entity: host.id(),
                        allow_cancel: true,
                        finished: Some(Event::IsolationSettled { request }),
                    },
                    move |app, reporter, cancel| async move {
                        let _in_flight = in_flight;
                        app.begin_isolation_reporting(
                            host,
                            IsolationRequest {
                                pinned,
                                accept_unrecorded,
                                accept_missing,
                            },
                            &reporter,
                            &cancel,
                        )?;
                        send_isolation_state(&app, host, generation, true, &reports, &refreshed);
                        send_host_mods(&app, host, &reports, &refreshed, false);
                        Ok(OperationReport::Reported)
                    },
                );
            }
            Command::AnswerIsolation {
                host,
                generation,
                request,
                answer,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IsolationSettled { request });
                    continue;
                }
                match app.answer_isolation_round(host, answer) {
                    Ok(_) => {
                        send_isolation_state(
                            &app,
                            host,
                            generation,
                            true,
                            &events,
                            &refreshed_mods,
                        );
                        send_host_mods(&app, host, &events, &refreshed_mods, false);
                    }
                    // The answer described mods the host no longer had, so it
                    // is dropped and the round is reapplied to be tested again.
                    Err(CoreError::Isolation(IsolationError::NotApplied { .. })) => {
                        send_isolation_state(
                            &app,
                            host,
                            generation,
                            true,
                            &events,
                            &refreshed_mods,
                        );
                    }
                    Err(error) => send_modal_error(&events, &error),
                }
                let _ = events.send(Event::IsolationSettled { request });
            }
            Command::UndoIsolation {
                host,
                generation,
                request,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IsolationSettled { request });
                    continue;
                }
                match app.undo_isolation_round(host) {
                    Ok(_) => {
                        send_isolation_state(
                            &app,
                            host,
                            generation,
                            true,
                            &events,
                            &refreshed_mods,
                        );
                        send_host_mods(&app, host, &events, &refreshed_mods, false);
                    }
                    Err(error) => send_modal_error(&events, &error),
                }
                let _ = events.send(Event::IsolationSettled { request });
            }
            Command::FinishIsolation {
                host,
                generation,
                request,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IsolationSettled { request });
                    continue;
                }
                match app.finish_isolation(host) {
                    Ok(report) => {
                        let _ = events.send(Event::Isolation {
                            generation,
                            open: true,
                            data: isolation_report_data(host, &report),
                        });
                        send_host_mods(&app, host, &events, &refreshed_mods, false);
                    }
                    Err(error) => send_modal_error(&events, &error),
                }
                let _ = events.send(Event::IsolationSettled { request });
            }
            Command::CancelIsolation {
                host,
                generation,
                request,
            } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IsolationSettled { request });
                    continue;
                }
                let paths = app.paths().clone();
                let reports = events.clone();
                let refreshed = Arc::clone(&refreshed_mods);
                let in_flight = HostChangeInFlight::mark(&changing, host, "gui-isolation-busy");
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-canceling-isolation",
                        entity: host.id(),
                        allow_cancel: true,
                        finished: Some(Event::IsolationSettled { request }),
                    },
                    move |app, _reporter, cancel| async move {
                        let _in_flight = in_flight;
                        app.cancel_isolation(host, &cancel)?;
                        let _ = reports.send(Event::Isolation {
                            generation,
                            open: false,
                            data: inactive_isolation_data(host),
                        });
                        send_host_mods(&app, host, &reports, &refreshed, false);
                        Ok(OperationReport::Toast(lookup("gui-isolation-canceled")))
                    },
                );
            }
            Command::VerifyHost { host, request } => {
                let paths = app.paths().clone();
                let reports = events.clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-verifying-host",
                        entity: host.id(),
                        allow_cancel: true,
                        finished: Some(Event::IntegritySettled { request }),
                    },
                    move |app, reporter, cancel| async move {
                        let data = verification_data(&app, host, &reporter, &cancel).await?;
                        let _ = reports.send(Event::HostVerified {
                            host,
                            request,
                            data,
                        });
                        Ok(OperationReport::Reported)
                    },
                );
            }
            Command::RepairHost { host, request } => {
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::IntegritySettled { request });
                    continue;
                }
                let paths = app.paths().clone();
                let reports = events.clone();
                let refreshed = Arc::clone(&refreshed_mods);
                let in_flight = HostChangeInFlight::mark(&changing, host, "gui-repair-busy");
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-repairing-host",
                        entity: host.id(),
                        allow_cancel: true,
                        finished: Some(Event::IntegritySettled { request }),
                    },
                    move |app, reporter, cancel| async move {
                        let _in_flight = in_flight;
                        let report = app.repair_host(host, &reporter, &cancel).await?;
                        let stopped = cancel.is_cancelled();
                        let _ = reports.send(Event::HostRepaired {
                            host,
                            request,
                            data: repair_data(&report, stopped),
                        });
                        send_host_mods(&app, host, &reports, &refreshed, false);
                        // What the repair meant to do is not evidence of what
                        // is on disk now; only checking again is.
                        if !stopped {
                            reporter.set_stage(Some("operation-stage-rechecking-host".to_owned()));
                            let data = verification_data(&app, host, &reporter, &cancel).await?;
                            let _ = reports.send(Event::HostVerified {
                                host,
                                request,
                                data,
                            });
                        }
                        Ok(OperationReport::Reported)
                    },
                );
            }
            Command::MeasureDiskUsage { request } => {
                let paths = app.paths().clone();
                let reports = events.clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-measuring-disk-usage",
                        entity: lookup("gui-settings-disk-usage"),
                        allow_cancel: true,
                        finished: Some(Event::DiskUsageSettled { request }),
                    },
                    move |app, reporter, cancel| async move {
                        let report = app.measure_disk_usage(&reporter, &cancel)?;
                        let _ = reports.send(Event::DiskUsage {
                            request,
                            report: Box::new(report),
                        });
                        Ok(OperationReport::Reported)
                    },
                );
            }
            Command::OpenStorageLocation { category, index } => {
                let _ = events.send(Event::Toast {
                    message: app.open_storage_location(category, index).map_or_else(
                        || lookup("gui-folder-unavailable"),
                        |path| localized_value("gui-opened-path", &path.display().to_string()),
                    ),
                    error: false,
                });
            }
            Command::PlanDiagnosticBundle(host) => {
                let parts = match app.plan_diagnostic_bundle(diagnostic_scope(host)) {
                    Ok(contents) => contents
                        .parts
                        .iter()
                        .map(|part| BundlePartData {
                            name: part.name.clone(),
                            kind: part.kind.as_str(),
                            size_bytes: part.size_bytes,
                        })
                        .collect(),
                    Err(error) => {
                        send_error(&events, &error);
                        Vec::new()
                    }
                };
                let _ = events.send(Event::BundlePlanned { host, parts });
            }
            Command::ExportDiagnosticBundle {
                host,
                request,
                names,
            } => {
                let Some(destination) = rfd::FileDialog::new()
                    .set_title(lookup("gui-bundle-save-title"))
                    .add_filter(lookup("gui-bundle-archives"), &["zip"])
                    .set_file_name(bundle_file_name(host))
                    .save_file()
                else {
                    let _ = events.send(Event::BundleSettled { request });
                    continue;
                };
                let paths = app.paths().clone();
                let reports = events.clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-exporting-diagnostic-bundle",
                        entity: host.id(),
                        allow_cancel: false,
                        finished: Some(Event::BundleSettled { request }),
                    },
                    move |app, _reporter, _cancel| async move {
                        let exported = app.export_diagnostic_bundle(
                            diagnostic_scope(host),
                            &ephervex_core::BundleSelection { names },
                            &destination,
                        );
                        match exported {
                            Ok(report) => {
                                let _ = reports.send(Event::BundleExported {
                                    host,
                                    request,
                                    report: BundleExportData {
                                        path: report.path,
                                        included: report.included.len(),
                                        unreadable: report.unreadable,
                                        bytes: report.bytes,
                                    },
                                });
                                Ok(OperationReport::Reported)
                            }
                            Err(error) => {
                                let _ = reports.send(Event::BundleExportFailed {
                                    host,
                                    request,
                                    message: localized_error(&error),
                                });
                                Err(error)
                            }
                        }
                    },
                );
            }
            Command::OpenBundleFolder(path) => {
                if !ephervex_core::open_containing_folder(&path) {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-folder-unavailable"),
                        error: true,
                    });
                }
            }
            Command::SearchMods {
                instance,
                provider,
                query,
                offset,
                limit,
                request_id,
            } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let request = ModSearchQuery::new(query.clone())
                        .from_offset(offset)
                        .limited(limit);
                    let result = runtime.block_on(app.search_mods(instance, provider, &request));
                    let answer = match result {
                        Ok(results) => {
                            let total = results.total_hits;
                            let next_offset = results.next_offset;
                            let icons = runtime.block_on(app.cache_result_artwork(
                                provider,
                                &results,
                                &CancellationToken::new(),
                            ));
                            let rows = results
                                .hits
                                .into_iter()
                                .map(|hit| ModSearchData {
                                    icon: icons
                                        .get(&hit.project.id)
                                        .map(|path| path.display().to_string())
                                        .unwrap_or_default(),
                                    project_id: hit.project.id,
                                    title: hit.project.title,
                                    description: hit.project.description,
                                    author: hit
                                        .project
                                        .author
                                        .unwrap_or_else(|| lookup("gui-unknown-author")),
                                    note: lookup("gui-compatible"),
                                })
                                .collect();
                            ModResultsData {
                                instance: instance.to_string(),
                                provider: provider.as_str().to_owned(),
                                query,
                                offset,
                                rows,
                                total,
                                next_offset,
                                request_id,
                                notice: String::new(),
                            }
                        }
                        Err(error) => {
                            // Reported in the panel the search happens in, so
                            // the reason survives longer than a toast does.
                            tracing::warn!(
                                message_id = error.message_id(),
                                detail = %ErrorChain(&error),
                                "mod search failed"
                            );
                            ModResultsData {
                                instance: instance.to_string(),
                                provider: provider.as_str().to_owned(),
                                query,
                                offset,
                                rows: Vec::new(),
                                total: 0,
                                next_offset: None,
                                request_id,
                                notice: localized_error(&error),
                            }
                        }
                    };
                    let _ = events.send(Event::ModResults(answer));
                });
            }
            Command::PlanMod {
                instance,
                provider,
                project,
            } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result = runtime.block_on(app.plan_linked_mod_operation(
                        instance,
                        &LinkedModRequest::Install {
                            provider,
                            project: project.clone(),
                            version: None,
                        },
                        &OperationReporter::silent("planning-mod-install"),
                        &CancellationToken::new(),
                    ));
                    match result {
                        Ok(plan) => {
                            let _ = events.send(Event::ModPlan(install_plan_data(&plan, &project)));
                        }
                        Err(error) => {
                            send_error(&events, &error);
                            let _ = events.send(Event::ModPlanFailed {
                                instance: instance.to_string(),
                                provider: provider.as_str().to_owned(),
                                requested: project,
                            });
                        }
                    }
                });
            }
            Command::InstallMod {
                instance,
                provider,
                project,
                version,
                servers,
            } => {
                let paths = app.paths().clone();
                let reports = events.clone();
                // A release the provider will not let this launcher download
                // stops the install and asks the user for it instead, so the
                // dialog that does the asking needs a way to be left.
                let waiting = CancellationToken::new();
                if let Some(previous) = acquisition_cancel.replace(waiting.clone()) {
                    previous.cancel();
                }
                let acquisition_events = events.clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    AsyncOperationRequest {
                        title: "operation-installing-mods",
                        entity: project.clone(),
                        allow_cancel: true,
                        finished: Some(Event::ModInstallFinished {
                            instance: instance.to_string(),
                            provider: provider.as_str().to_owned(),
                            project: project.clone(),
                        }),
                    },
                    move |app, reporter, cancel| async move {
                        let request = LinkedModRequest::Install {
                            provider,
                            project,
                            // Pinned to what the user agreed to, so a release
                            // published since the confirmation cannot take its
                            // place here or on any selected server.
                            version: (!version.is_empty()).then_some(version),
                        };
                        // Ending the operation from the progress list ends the
                        // wait too, so there is no dialog left over a job the
                        // user already stopped.
                        let stopped = waiting.clone();
                        let operation_cancel = cancel.clone();
                        tokio::spawn(async move {
                            operation_cancel.cancelled().await;
                            stopped.cancel();
                        });

                        let mut result = app
                            .apply_linked_mod_operation(
                                instance, &request, &servers, &reporter, &cancel,
                            )
                            .await;
                        match await_manual_acquisition(
                            &app,
                            &acquisition_events,
                            &instance.to_string(),
                            &result,
                            &waiting,
                        )
                        .await
                        {
                            Acquisition::Acquired => {
                                result = app
                                    .apply_linked_mod_operation(
                                        instance, &request, &servers, &reporter, &cancel,
                                    )
                                    .await;
                            }
                            Acquisition::NotNeeded => {}
                            // The user decided against fetching the release
                            // themselves. Nothing was installed and nothing
                            // went wrong, so there is nothing to report.
                            Acquisition::Abandoned => return Ok(OperationReport::Reported),
                        }
                        let outcome = result?;
                        let message = client_outcome_message(&outcome);
                        // The Managed Mods panel may already be open on this
                        // instance, and a newly installed mod must appear on it
                        // without the workspace being reopened. Recorded state
                        // is authoritative about which mods are there and is
                        // shown at once; the provider title, version, and
                        // artwork follow, so the new row does not sit there as a
                        // file name until the workspace is opened again.
                        if let Ok(snapshot) = app.managed_mod_snapshot(instance) {
                            let review = app
                                .crash_review(InstalledHost::Instance(instance))
                                .ok()
                                .flatten();
                            let _ = reports.send(Event::ManagedMods(managed_mods_data(
                                &snapshot,
                                review.as_ref(),
                            )));
                        }
                        refresh_enriched_mods(app.paths().clone(), &reports, instance);
                        if outcome.targets.is_empty() {
                            return Ok(OperationReport::Toast(message));
                        }
                        // Several targets took part, so several results have to
                        // be shown. One toast could only be right about one.
                        let _ = reports.send(Event::CloseModal);
                        let _ = reports.send(Event::LinkedResult(linked_result_data(&outcome)));
                        Ok(OperationReport::Reported)
                    },
                );
            }
            Command::CreateServer {
                request_id,
                name,
                client,
                minecraft,
                loader,
                loader_version,
                persistent,
                port,
                server_pack,
            } => {
                let acquisition_host = client.as_ref().map(ToString::to_string);
                let request = if let Some(client) = client {
                    Ok(NewServer::linked(name, client).with_storage(storage_class(persistent)))
                } else {
                    loader_selection(&loader, &loader_version).map(|loader| {
                        NewServer::new(name, minecraft, loader)
                            .with_storage(storage_class(persistent))
                    })
                };
                match request {
                    Ok(mut request) => {
                        if let Some(port) = port {
                            let mut network = request.network.clone();
                            network.port = port;
                            request = request.with_network(network);
                        }
                        let paths = app.paths().clone();
                        let waiting = acquisition_host.as_ref().map(|_| CancellationToken::new());
                        if let Some(waiting) = waiting.as_ref()
                            && let Some(previous) = acquisition_cancel.replace(waiting.clone())
                        {
                            previous.cancel();
                        }
                        let acquisition_events = events.clone();
                        spawn_creation_operation(
                            paths,
                            &events,
                            Arc::clone(&cancellations),
                            CreationRequest {
                                id: request_id,
                                title: "operation-creating-server",
                                entity: request.name.clone(),
                                report_progress: true,
                            },
                            move |app, reporter, cancel| async move {
                                let options = ServerCreateOptions {
                                    install_java_if_missing: true,
                                    content: server_pack
                                        .map_or(ServerContentSource::Recipe, |path| {
                                            ServerContentSource::Archive { path }
                                        }),
                                    reporter: Some(reporter),
                                    ..ServerCreateOptions::default()
                                };
                                if let Some(waiting) = waiting.as_ref() {
                                    let stopped = waiting.clone();
                                    let operation_cancel = cancel.clone();
                                    tokio::spawn(async move {
                                        operation_cancel.cancelled().await;
                                        stopped.cancel();
                                    });
                                }
                                let completed = |result: CoreResult<ServerCommit<ServerRecord>>| {
                                    result.map(|commit| {
                                        CreationOutcome::Created(localized_value(
                                            "gui-created-entity",
                                            &commit.record.name,
                                        ))
                                    })
                                };
                                loop {
                                    let result =
                                        app.create_server(request.clone(), &options, &cancel).await;
                                    let Some((host, waiting)) =
                                        acquisition_host.as_deref().zip(waiting.as_ref())
                                    else {
                                        break completed(result);
                                    };
                                    match await_manual_acquisition(
                                        &app,
                                        &acquisition_events,
                                        host,
                                        &result,
                                        waiting,
                                    )
                                    .await
                                    {
                                        Acquisition::Acquired => {}
                                        Acquisition::NotNeeded => {
                                            break completed(result);
                                        }
                                        Acquisition::Abandoned => {
                                            break Ok(CreationOutcome::Abandoned);
                                        }
                                    }
                                }
                            },
                        );
                    }
                    Err(error) => {
                        send_error(&events, &error);
                        let _ = events.send(Event::CreationFinished { request_id });
                    }
                }
            }
            Command::StartServer(server) => {
                if refuse_while_changing(&changing, InstalledHost::Server(server), &events) {
                    continue;
                }
                let paths = app.paths().clone();
                let entity = app
                    .server(server)
                    .map_or_else(|_| server.to_string(), |record| record.name);
                spawn_server(paths, &events, server, entity);
            }
            Command::StopServer(server) => {
                report_async_control(
                    &runtime,
                    &events,
                    app.stop_server(server),
                    "gui-stop-requested",
                );
                send_snapshot(&app, &events);
            }
            Command::RestartHost { host, request } => {
                let refused = |message: &str| {
                    let _ = events.send(Event::Toast {
                        message: lookup(message),
                        error: true,
                    });
                    let _ = events.send(Event::HostRestart {
                        host,
                        request,
                        stage: None,
                    });
                };
                if refuse_while_changing(&changing, host, &events) {
                    let _ = events.send(Event::HostRestart {
                        host,
                        request,
                        stage: None,
                    });
                    continue;
                }
                let paths = app.paths().clone();
                match host {
                    InstalledHost::Instance(instance) => {
                        let running = launches
                            .lock()
                            .ok()
                            .and_then(|entries| entries.get(&instance).cloned());
                        let token = cancellations
                            .lock()
                            .ok()
                            .and_then(|entries| entries.get(&launch_key(instance, None)).cloned());
                        let (Some(running), Some(token)) = (running, token) else {
                            refused("gui-restart-client-unavailable");
                            continue;
                        };
                        let entity = app
                            .client(instance)
                            .map_or_else(|_| instance.to_string(), |record| record.name);
                        let in_flight =
                            HostChangeInFlight::mark(&changing, host, "gui-restart-busy");
                        spawn_client_restart(
                            paths,
                            &events,
                            LaunchTracking {
                                cancellations: Arc::clone(&cancellations),
                                launches: Arc::clone(&launches),
                            },
                            ClientRestart {
                                instance,
                                entity,
                                request,
                                running,
                                token,
                            },
                            in_flight,
                        );
                    }
                    InstalledHost::Server(server) => {
                        let controlled = app.server_status(server).is_ok_and(|status| {
                            matches!(
                                status.runtime,
                                ephervex_core::RuntimeClassification::Controlled { .. }
                            )
                        });
                        if !controlled {
                            refused("gui-restart-server-unavailable");
                            continue;
                        }
                        let entity = app
                            .server(server)
                            .map_or_else(|_| server.to_string(), |record| record.name);
                        let in_flight =
                            HostChangeInFlight::mark(&changing, host, "gui-restart-busy");
                        spawn_server_restart(paths, &events, server, entity, request, in_flight);
                    }
                }
            }
            Command::KillServer(server) => {
                report_async_control(
                    &runtime,
                    &events,
                    app.kill_server(server),
                    "gui-force-kill-requested",
                );
                send_snapshot(&app, &events);
            }
            Command::SendConsole { server, command } => {
                report_async_control(
                    &runtime,
                    &events,
                    app.send_server_console(server, command),
                    "gui-command-sent",
                );
                send_server_workspace(&app, server, &events);
            }
            Command::LoadServerWorkspace(server) => {
                // The crash notice comes with the workspace rather than with
                // the mods, so a server that failed to start says so on the tab
                // the user lands on instead of only on the one they might open.
                let host = InstalledHost::Server(server);
                let review = app.crash_review(host).ok().flatten();
                let _ = events.send(Event::CrashNotice(crash_notice_data(host, review.as_ref())));
                send_server_workspace(&app, server, &events);
                send_isolation_state(
                    &app,
                    InstalledHost::Server(server),
                    0,
                    false,
                    &events,
                    &refreshed_mods,
                );
            }
            Command::LoadServerMods(server) => {
                send_host_mods(
                    &app,
                    InstalledHost::Server(server),
                    &events,
                    &refreshed_mods,
                    false,
                );
            }
            Command::LoadRunning => {
                let _ = events.send(Event::Running(running_data(
                    &app,
                    &mut process_monitor,
                    &cancellations,
                )));
            }
            Command::ForceStopLaunch { instance, launch } => {
                match app.force_stop_launch(instance, launch.as_deref()) {
                    Ok(outcome) => {
                        let _ = events.send(Event::Toast {
                            message: lookup(if outcome.signaled_game || outcome.signaled_owner {
                                "gui-force-stop-signaled"
                            } else {
                                "gui-force-stop-released"
                            }),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::RefreshMods(host) => {
                send_host_mods(&app, host, &events, &refreshed_mods, true);
            }
            Command::PlanLocalMods { host, sources } => match app.plan_local_mods(host, &sources) {
                Ok(plan) => {
                    let _ = events.send(Event::LocalModPlan(local_mod_plan_data(host, &plan)));
                }
                Err(error) => send_error(&events, &error),
            },
            Command::AddLocalMods {
                host,
                sources,
                skipped,
            } => match app.add_local_mods(host, &sources) {
                Ok(report) => {
                    let _ = events.send(Event::Toast {
                        message: local_mods_added_message(&report, skipped),
                        error: !report.refused.is_empty(),
                    });
                    // What the Refresh button does, so the jars just added are
                    // identified without the user asking a second time.
                    send_host_mods(&app, host, &events, &refreshed_mods, true);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::RestoreModFile { host, path } => {
                let reporter = OperationReporter::silent("operation-restoring-mod");
                let restored = runtime.block_on(app.restore_mod_file(
                    host,
                    &path,
                    &reporter,
                    &CancellationToken::new(),
                ));
                match restored {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-mod-restored"),
                            error: false,
                        });
                        send_host_mods(&app, host, &events, &refreshed_mods, false);
                    }
                    // The provider will not let this launcher fetch the jar
                    // back, so the user is asked for it instead. The wait runs
                    // off the command loop, which has other work to do.
                    Err(error @ CoreError::ModAcquisitionRestricted { .. }) => {
                        acquisition_cancel = Some(restore_after_acquisition(
                            app.paths().clone(),
                            &events,
                            acquisition_cancel.take(),
                            RestoreRequest {
                                host,
                                path,
                                refusal: error,
                                refreshed: Arc::clone(&refreshed_mods),
                            },
                        ));
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::DeleteHandPlacedMod { host, path } => match app.delete_mod_file(host, &path) {
                Ok(()) => {
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message: localized_value(
                            "gui-mod-deleted",
                            &path
                                .file_name()
                                .unwrap_or(path.as_os_str())
                                .to_string_lossy(),
                        ),
                        error: false,
                    });
                    send_host_mods(&app, host, &events, &refreshed_mods, false);
                }
                Err(error) => send_modal_error(&events, &error),
            },
            Command::RenameClient { id, name } => match app.rename_client(id, &name) {
                Ok(record) => {
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message: localized_value("gui-renamed", &record.name),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_modal_error(&events, &error),
            },
            Command::RenameServer { id, name } => match app.rename_server(id, &name) {
                Ok(record) => {
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message: localized_value("gui-renamed", &record.name),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_modal_error(&events, &error),
            },
            Command::DisableSuggestedMods(server) => {
                disable_suggested_mods(&app, server, &events, &refreshed_mods);
            }
            Command::SetModEnabled {
                host,
                path,
                enabled,
            } => {
                set_mod_enabled(&app, host, &path, enabled, &events, &refreshed_mods);
            }
            Command::SaveDependencyWarning(warn) => {
                if let Err(error) = app.set_warn_on_dependency_impact(warn) {
                    send_error(&events, &error);
                } else {
                    send_snapshot(&app, &events);
                }
            }
            Command::SaveDependencyFollowing(follow) => {
                if let Err(error) = app.set_follow_mod_dependencies(follow) {
                    send_error(&events, &error);
                } else {
                    send_snapshot(&app, &events);
                }
            }
            Command::LoadModRelations { host, path } => {
                send_mod_relations(&app, host, &path, &BTreeMap::new(), &events);
            }
            Command::ResolveDeclaredRelations { host, path } => {
                resolve_declared_relations(&app, &runtime, host, &path, &events);
            }
            Command::RefreshModDependencies { host, reload } => {
                let reporter = OperationReporter::silent("refreshing-mod-dependencies");
                match runtime.block_on(app.refresh_mod_dependencies(
                    host,
                    &reporter,
                    &CancellationToken::new(),
                )) {
                    Ok(report) => {
                        let _ = events.send(Event::Toast {
                            message: lookup_args(
                                "gui-mod-dependencies-refreshed",
                                [(
                                    "count",
                                    Arg::from(i64::try_from(report.recorded).unwrap_or(0)),
                                )],
                            ),
                            error: !report.is_complete(),
                        });
                        send_host_mods(&app, host, &events, &refreshed_mods, false);
                        if let Some(path) = reload {
                            send_mod_relations(&app, host, &path, &BTreeMap::new(), &events);
                            resolve_declared_relations(&app, &runtime, host, &path, &events);
                        }
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::DeleteModFiles { host, paths } => match app.delete_mod_files(host, &paths) {
                Ok(_) => send_host_mods(&app, host, &events, &refreshed_mods, false),
                Err(error) => send_error(&events, &error),
            },
            Command::SetModsEnabled { host, changes } => match app.set_mods_enabled(host, &changes)
            {
                // Re-read rather than patched in place: the durable state is
                // what the panel shows, and it has just changed.
                Ok(_) => send_host_mods(&app, host, &events, &refreshed_mods, false),
                Err(error) => send_error(&events, &error),
            },
            Command::QueryDeleteImpact { host, path, name } => {
                delete_mod(&app, host, &path, name, &events, &refreshed_mods);
            }
            Command::OpenLiveLog(source) => {
                open_live_log(&app, source, &events, &live_log_generation);
            }
            Command::CloseLiveLog => {
                live_log_generation.fetch_add(1, Ordering::Relaxed);
            }
            Command::SaveServerProperties {
                server,
                network,
                values,
            } => {
                let result = app.set_server_network(server, network).and_then(|_| {
                    for (key, value) in values {
                        app.set_server_property(server, &key, &value)?;
                    }
                    Ok(())
                });
                match result {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-server-properties-saved"),
                            error: false,
                        });
                        send_server_workspace(&app, server, &events);
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::AcceptEula => match app.accept_server_eula() {
                Ok(_) => {
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-eula-consent-recorded"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::RevokeEula => match app.revoke_server_eula() {
                Ok(()) => {
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-eula-consent-revoked"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::OpenEulaReference => {
                let opened =
                    ephervex_core::open_in_browser(ephervex_core::MINECRAFT_EULA_REFERENCE);
                let _ = events.send(Event::Toast {
                    message: if opened {
                        lookup("gui-eula-browser-opened")
                    } else {
                        lookup("gui-eula-browser-failed")
                    },
                    error: !opened,
                });
            }
            Command::LoadDiagnostics { scope, kind } => {
                send_diagnostics(&app, scope, kind, &events);
            }
            Command::OpenDiagnostic { scope, kind, name } => {
                send_diagnostic_content(&app, scope, kind, &name, None, &events);
            }
            Command::SearchDiagnostic {
                scope,
                kind,
                name,
                query,
            } => send_diagnostic_content(&app, scope, kind, &name, Some(&query), &events),
            Command::OpenDiagnosticFolder { scope, kind } => {
                report_sync(
                    &events,
                    app.open_diagnostic_folder(scope, kind).map(|path| {
                        path.map_or_else(
                            || lookup("gui-folder-unavailable"),
                            |path| localized_value("gui-opened-path", &path.display().to_string()),
                        )
                    }),
                );
            }
            Command::AddOfflineAccount(name) => match app.add_offline_account(&name) {
                Ok(account) => {
                    let _ = events.send(Event::Toast {
                        message: localized_value("gui-offline-account-added", &account.name),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::SetDefaultAccount(reference) => match app.set_default_account(&reference) {
                Ok(account) => {
                    let _ = events.send(Event::Toast {
                        message: localized_value("gui-default-account-set", &account.name),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::RemoveAccount(reference) => match app.remove_account(&reference) {
                Ok(_) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-account-removed"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::MicrosoftLogin => {
                let paths = app.paths().clone();
                spawn_login(paths, &events);
            }
            Command::SaveApiSettings {
                client_id,
                user_agent,
            } => {
                let result = app
                    .set_microsoft_client_id(nonempty(&client_id))
                    .and_then(|()| app.set_custom_user_agent(nonempty(&user_agent)));
                match result {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-provider-settings-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::ResetMicrosoftClientId => match app.set_microsoft_client_id(None) {
                Ok(()) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-client-id-reset"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::ResetUserAgent => match app.set_custom_user_agent(None) {
                Ok(()) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-user-agent-reset"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::SaveCurseForgeApiKey(key) => {
                // The value is moved into the credential store and dropped
                // here. Nothing echoes it, and the snapshot that follows
                // reports presence rather than a value.
                match app.set_curseforge_api_key(&ephervex_core::SecretString::new(key)) {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-curseforge-key-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::RemoveCurseForgeApiKey => match app.clear_curseforge_api_key() {
                Ok(()) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-curseforge-key-removed"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::OpenCurseForgeKeyHelp => {
                let opened = ephervex_core::open_in_browser(ephervex_core::CURSEFORGE_API_KEY_HELP);
                let _ = events.send(Event::Toast {
                    message: if opened {
                        lookup("gui-curseforge-key-help-opened")
                    } else {
                        lookup("gui-curseforge-key-help-failed")
                    },
                    error: !opened,
                });
            }
            Command::SaveStorageRoot(root) => {
                let path = nonempty(&root).map(Path::new);
                match app.set_persistent_instance_root(path) {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-storage-root-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::SaveWatchedDownloadFolders(folders) => {
                let folders: Vec<PathBuf> = folders.into_iter().map(PathBuf::from).collect();
                match app.set_watched_download_folders(Some(&folders)) {
                    Ok(()) => send_snapshot(&app, &events),
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::MeasureArtifactCache => match runtime.block_on(app.artifact_cache_status()) {
                Ok(status) => {
                    let _ = events.send(Event::ArtifactCacheStatus(artifact_cache_status_data(
                        &status,
                    )));
                }
                Err(error) => send_error(&events, &error),
            },
            Command::SaveArtifactCacheLimit {
                selection,
                custom_value,
                custom_unit,
            } => {
                // Parsed before anything is written, so a refused size leaves the
                // budget that is in force exactly as it was.
                let limit = crate::presentation::cache_limit_from_selection(
                    selection,
                    &custom_value,
                    &custom_unit,
                );
                match limit {
                    Ok(limit) => match runtime.block_on(app.set_artifact_cache_limit(limit)) {
                        Ok(status) => {
                            let _ = events.send(Event::Toast {
                                message: lookup("gui-artifact-cache-limit-saved"),
                                error: false,
                            });
                            let _ = events.send(Event::ArtifactCacheStatus(
                                artifact_cache_status_data(&status),
                            ));
                            send_snapshot(&app, &events);
                        }
                        Err(error) => send_error(&events, &error),
                    },
                    Err(error) => send_error(&events, &CoreError::from(error)),
                }
            }
            Command::ClearArtifactCache => {
                let cleared = runtime.block_on(app.clear_artifact_cache());
                for event in clear_artifact_cache_events(cleared, || {
                    runtime.block_on(app.artifact_cache_status())
                }) {
                    let _ = events.send(event);
                }
            }
            Command::SaveDefaultModCatalog { catalog } => {
                let stored = app.set_default_mod_catalog(catalog);
                let confirmed = stored.is_ok();
                for event in default_mod_catalog_events(&stored) {
                    let _ = events.send(event);
                }
                // Only a write that reached disk refreshes the view. The chips
                // read the stored value and nothing else, so a refused write
                // leaves the previous choice selected rather than the one that
                // was asked for.
                if confirmed {
                    send_snapshot(&app, &events);
                }
            }
            Command::SaveGeneralSettings {
                text_scale,
                technical_text_size,
                color_scheme,
                show_globally,
                make_operator,
                operator_level,
            } => {
                // Both halves, then one snapshot: each control saves as it is
                // changed, so a confirmation per click would be noise. A
                // failure in either is reported as itself and leaves the other
                // where it was.
                let saved = app
                    .set_application_display_settings(text_scale, technical_text_size, color_scheme)
                    .and_then(|()| {
                        app.set_linked_server_settings(show_globally, make_operator, operator_level)
                    });
                match saved {
                    Ok(()) => send_snapshot(&app, &events),
                    Err(error) => send_error(&events, &error),
                }
            }
            // Opening the page asks for the inventory, which is cached after
            // the first scan; the page's own button is what forces a rescan.
            // Probing forks a JVM per runtime, so a page that rescanned every
            // time it was opened would pay for it every time.
            Command::LoadJavaRuntimes => match runtime.block_on(app.java_runtimes()) {
                Ok(inventory) => {
                    let _ = events.send(Event::JavaRuntimes(describe_runtimes(&inventory)));
                }
                Err(error) => send_error(&events, &error),
            },
            Command::RefreshJava => match runtime.block_on(app.refresh_java_runtimes()) {
                Ok(inventory) => {
                    let _ = events.send(Event::JavaRuntimes(describe_runtimes(&inventory)));
                }
                Err(error) => send_error(&events, &error),
            },
            Command::RemoveManagedJavaRuntime(path) => {
                match runtime.block_on(app.remove_managed_java_runtime(Path::new(&path))) {
                    Ok(_) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-java-runtime-removed"),
                            error: false,
                        });
                        // The list the page is showing now names a directory
                        // that is gone, so it is re-read rather than left.
                        match runtime.block_on(app.java_runtimes()) {
                            Ok(inventory) => {
                                let _ =
                                    events.send(Event::JavaRuntimes(describe_runtimes(&inventory)));
                            }
                            Err(error) => send_error(&events, &error),
                        }
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::SaveJavaSettings {
                auto_detect,
                auto_download,
                minimum_mib,
                maximum_mib,
                preferred_path,
            } => {
                let mut settings = app.java_settings();
                settings.auto_detect = auto_detect;
                settings.auto_download = auto_download;
                settings.min_memory_mib = minimum_mib;
                settings.max_memory_mib = maximum_mib;
                settings.preferred_path = nonempty(&preferred_path).map(PathBuf::from);
                match app.set_java_settings(&settings) {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-java-settings-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::SaveMinecraftPreferences { edits } => {
                let outcome = app.save_global_minecraft_preferences(&edits);
                let succeeded = outcome.is_ok();
                match outcome {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-minecraft-defaults-saved"),
                            error: false,
                        });
                        // The snapshot first, so the confirmed baseline is in
                        // place before the draft is reconciled against it.
                        send_snapshot(&app, &events);
                    }
                    // The batch is validated before anything is written, so a
                    // refusal leaves the configuration exactly as it was and
                    // the user keeps the draft they still have, with the
                    // reason for the refusal.
                    Err(error) => send_error(&events, &error),
                }
                let _ = events.send(Event::MinecraftPreferencesSaveFinished { succeeded });
            }
            Command::PreviewMinecraftPreferences {
                generation,
                minecraft,
                draft,
                language,
            } => {
                // Uses whatever compatibility knowledge is already cached and
                // never reaches the network, so a preview cannot make the
                // settings screen wait on a metadata host.
                let _ = runtime.block_on(app.cached_minecraft_options());
                match app.preview_minecraft_preference_draft(&minecraft, &draft, language) {
                    Ok(preview) => {
                        let _ = events.send(Event::MinecraftPreferencePreview {
                            generation,
                            minecraft,
                            preview,
                        });
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::LoadMinecraftLanguages {
                generation,
                minecraft,
            } => {
                load_language_catalog(app.paths().clone(), generation, minecraft, &events);
            }
        }
    }
}

/// Resolves one release's language catalog and reports it back.
///
/// On its own thread for the same reason warming has one: resolving may fetch
/// Mojang metadata, and the command loop must stay responsive. A failure is
/// reported as "no catalog" rather than as an error, because it concerns
/// exactly one preference and must leave every other one editable.
fn load_language_catalog(paths: Paths, generation: u64, minecraft: String, events: &Sender<Event>) {
    let spawned = events.clone();
    let requested = minecraft.clone();
    if let Err(error) = thread::Builder::new()
        .name("ephervex-language-list".to_owned())
        .spawn(move || {
            let languages = language_catalog_of(paths, &requested);
            let _ = spawned.send(Event::MinecraftLanguages {
                generation,
                minecraft: requested,
                languages,
            });
        })
    {
        tracing::warn!(%error, "could not schedule language catalog resolution");
        let _ = events.send(Event::MinecraftLanguages {
            generation,
            minecraft,
            languages: None,
        });
    }
}

/// One release's languages, or `None` when nothing could be resolved.
fn language_catalog_of(paths: Paths, minecraft: &str) -> Option<Vec<LanguageEntry>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .inspect_err(|error| tracing::warn!(%error, "could not start language catalog resolution"))
        .ok()?;
    let app = App::open_at(paths)
        .inspect_err(|error| tracing::warn!(%error, "could not open the language catalog cache"))
        .ok()?;
    runtime
        .block_on(app.minecraft_language_catalog(minecraft))
        .map(|catalog| catalog.languages)
}

fn warm_language_catalog(paths: Paths, minecraft: &str) {
    let version = minecraft.to_owned();
    if let Err(error) = thread::Builder::new()
        .name("ephervex-language-catalog".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::warn!(%error, %version, "could not start language catalog warming");
                    return;
                }
            };
            let app = match App::open_at(paths) {
                Ok(app) => app,
                Err(error) => {
                    tracing::warn!(%error, %version, "could not open language catalog cache");
                    return;
                }
            };
            if runtime
                .block_on(app.minecraft_language_catalog(&version))
                .is_none()
            {
                tracing::debug!(%version, "language catalog was not available to warm");
            }
        })
    {
        tracing::warn!(%error, %minecraft, "could not schedule language catalog warming");
    }
}

/// One row per runtime an inventory holds, in the order it reports them.
fn describe_runtimes(inventory: &ephervex_core::RuntimeInventory) -> Vec<JavaData> {
    inventory
        .runtimes
        .iter()
        .map(|java| JavaData {
            path: java.executable.display().to_string(),
            version: java.version_string.clone(),
            vendor: java.vendor.clone(),
            origin: java
                .origins
                .first()
                .map_or_else(|| "detected".to_owned(), |origin| origin.kind.to_string()),
            compatible: format!("Java {}", java.version.major),
        })
        .collect()
}

fn send_snapshot(app: &App, events: &Sender<Event>) {
    match snapshot(app) {
        Ok(snapshot) => {
            let _ = events.send(Event::Snapshot(Box::new(snapshot)));
        }
        Err(error) => send_error(events, &error),
    }
}

fn snapshot(app: &App) -> Result<Snapshot, CoreError> {
    let servers = app.list_servers()?;
    let mut connection_states = std::collections::HashMap::new();
    let mut instance_rows = Vec::new();
    for record in app.list_clients()? {
        let linked = servers
            .iter()
            .filter(|server| server.linked_client_id == Some(record.id))
            .cloned()
            .collect::<Vec<_>>();
        let connections = linked.iter().cloned().map(|server| {
            let availability = app
                .launch_and_connect_availability(record.id, server.id)
                .map_err(|error| localized_error(&error));
            connection_states.insert(
                server.id,
                availability
                    .as_ref()
                    .is_ok_and(|availability| !availability.client_running),
            );
            (server, availability)
        });
        let managed_mods = app.managed_mods(record.id).map_or(0, |mods| mods.len());
        let launch_state = app.launch_state(record.id)?;
        let mut presentation = InstancePresentation::from_record(
            &record,
            linked.len(),
            managed_mods,
            launch_state.as_ref(),
            connections,
        );
        presentation.snapshot_deletion_note =
            snapshot_deletion_note(app, InstalledHost::Instance(record.id));
        instance_rows.push(presentation);
    }
    let mut server_rows = Vec::new();
    for server in servers {
        let status = app.server_status(server.id)?;
        let mut presentation = ServerPresentation::from_status(&status);
        presentation.snapshot_deletion_note =
            snapshot_deletion_note(app, InstalledHost::Server(server.id));
        presentation.can_connect = connection_states.get(&server.id).copied().unwrap_or(false);
        server_rows.push(presentation);
    }
    let accounts = app
        .account_status()?
        .accounts
        .into_iter()
        .map(|account| AccountData {
            id: account.id.to_string(),
            name: account.name,
            kind: account.kind.to_string(),
            is_default: account.is_default,
            credential_state: if account.kind.is_authenticated() {
                if account.has_stored_credential {
                    lookup("gui-credential-stored")
                } else {
                    lookup("gui-signin-required")
                }
            } else {
                lookup("gui-no-credential")
            },
        })
        .collect();
    let minecraft_preferences = app.global_minecraft_preference_snapshot()?;
    if !minecraft_preferences.unusable.is_empty() {
        tracing::warn!(
            count = minecraft_preferences.unusable.len(),
            "some configured Minecraft preferences are invalid"
        );
    }
    Ok(Snapshot {
        instances: instance_rows,
        servers: server_rows,
        accounts,
        minecraft_preferences,
        settings: application_settings(app)?,
        eula: eula_data(&app.server_eula_status()),
    })
}

fn application_settings(app: &App) -> Result<SettingsData, CoreError> {
    let api = app.api_settings()?;
    let display = app.application_display_settings();
    let storage = app.persistent_storage_settings();
    let java = app.java_settings();
    let server = app.server_application_settings();
    let cache_limit = app.artifact_cache_limit();
    let catalog = crate::presentation::CatalogPreferenceView::of(&app.default_mod_catalog());
    let choices = crate::presentation::cache_limit_choices();
    let (custom_value, custom_unit) = crate::presentation::cache_limit_custom_fields(cache_limit);
    Ok(SettingsData {
        storage_root: storage.effective_root.display().to_string(),
        persistent_root: storage
            .configured_root
            .as_deref()
            .map_or_else(String::new, |path| path.display().to_string()),
        default_persistent_root: storage.default_root.display().to_string(),
        persistent_root_origin: origin(storage.configured_root.is_some()),
        watched_download_folders: app
            .watched_download_folders()
            .into_iter()
            .map(|path| path.display().to_string())
            .collect(),
        microsoft_client_id_origin: origin(api.microsoft_client_id_is_custom),
        user_agent_origin: origin(!api.user_agent.is_empty()),
        microsoft_client_id: api.microsoft_client_id,
        effective_microsoft_client_id: api.effective_microsoft_client_id,
        user_agent: api.user_agent,
        effective_user_agent: api.effective_user_agent,
        curseforge_key: api.curseforge_key,
        curseforge_default_key_available: api.curseforge_default_key_available,
        default_mod_catalog: catalog.selected,
        configured_mod_catalog: catalog.configured,
        mod_catalog_origin: origin(catalog.chosen),
        text_scale_percent: display.text_scale.percent(),
        technical_text_size_pixels: display.technical_text_size.pixels(),
        color_scheme: display.color_scheme.as_str().to_owned(),
        show_linked_servers: display.show_linked_servers,
        follow_mod_dependencies: app.follow_mod_dependencies(),
        warn_on_dependency_impact: app.warn_on_dependency_impact(),
        favorite_minecraft_versions: app.favorite_minecraft_versions().to_vec(),
        make_default_account_operator: server.make_default_account_operator,
        operator_permission_level: server.operator_permission_level.value(),
        java_auto_detect: java.auto_detect,
        java_auto_download: java.auto_download,
        java_memory_min: java.min_memory_mib,
        java_memory_max: java.max_memory_mib,
        java_preferred_path: java
            .preferred_path
            .map_or_else(String::new, |path| path.display().to_string()),
        artifact_cache_limit_labels: choices.iter().map(|choice| choice.label.clone()).collect(),
        artifact_cache_limit_index: crate::presentation::cache_limit_selection(cache_limit),
        artifact_cache_custom_index: choices
            .iter()
            .position(|choice| choice.kind == crate::presentation::CacheLimitKind::Custom)
            .unwrap_or_default(),
        artifact_cache_units: crate::presentation::cache_limit_units(),
        artifact_cache_custom_value: custom_value,
        artifact_cache_custom_unit: custom_unit,
        artifact_cache_limit_display: cache_limit.bytes().map_or_else(
            || lookup("gui-artifact-cache-unlimited"),
            ephervex_core::format_binary_bytes,
        ),
    })
}

/// What a finished clear tells the user.
///
/// Three outcomes rather than one sentence, because "freed 6 GiB", "freed 6 GiB
/// but some is still cached", and "there was nothing to free" are different
/// things and a user acting on the wrong one goes looking for a bug.
fn cleared_cache_message(report: &ephervex_core::ArtifactCacheMaintenance) -> String {
    let freed = ephervex_core::format_binary_bytes(report.freed_bytes);
    if report.usage_after > 0 || !report.is_clean() {
        if report.removed_entries == 0 && report.usage_before == 0 {
            return lookup("gui-artifact-cache-already-empty");
        }
        let remaining = ephervex_core::format_binary_bytes(report.usage_after);
        return ephervex_i18n::lookup_args(
            "gui-artifact-cache-cleared-partly",
            [
                ("value", ephervex_i18n::Arg::from(freed.as_str())),
                ("remaining", ephervex_i18n::Arg::from(remaining.as_str())),
            ],
        );
    }
    if report.removed_entries == 0 {
        return lookup("gui-artifact-cache-already-empty");
    }
    ephervex_i18n::lookup_args(
        "gui-artifact-cache-cleared",
        [("value", ephervex_i18n::Arg::from(freed.as_str()))],
    )
}

/// Everything a manual clear reports back, in the order the view receives it.
///
/// Each outcome ends with an event that takes the view out of its clearing
/// state: a fresh usage figure when the store could be measured, and an explicit
/// "finished" otherwise. A clear that only reported an error would otherwise
/// leave the button that started it disabled with nothing on the way to enable
/// it again.
fn clear_artifact_cache_events(
    cleared: Result<ephervex_core::ArtifactCacheMaintenance, CoreError>,
    measure: impl FnOnce() -> Result<ephervex_core::ArtifactCacheStatus, CoreError>,
) -> Vec<Event> {
    let report = match cleared {
        Ok(report) => report,
        Err(error) => return vec![error_toast(&error), Event::ArtifactCacheClearFinished],
    };
    let mut events = vec![Event::Toast {
        message: cleared_cache_message(&report),
        error: false,
    }];
    match measure() {
        Ok(status) => events.push(Event::ArtifactCacheStatus(artifact_cache_status_data(
            &status,
        ))),
        Err(error) => {
            events.push(error_toast(&error));
            events.push(Event::ArtifactCacheClearFinished);
        }
    }
    events
}

/// The measured cache, formatted for the storage settings.
fn artifact_cache_status_data(
    status: &ephervex_core::ArtifactCacheStatus,
) -> ArtifactCacheStatusData {
    ArtifactCacheStatusData {
        used: ephervex_core::format_binary_bytes(status.used_bytes),
        total: status.limit.bytes().map_or_else(
            || lookup("gui-artifact-cache-unlimited"),
            ephervex_core::format_binary_bytes,
        ),
        exact: status.complete,
        over_budget: status.is_over_budget(),
    }
}

const fn origin(is_override: bool) -> SettingOrigin {
    if is_override {
        SettingOrigin::Override
    } else {
        SettingOrigin::Default
    }
}

/// Sends one host's durable mod list, then asks a provider to decorate it.
///
/// A fingerprint of a plan, compared only within this process.
const fn diagnostic_scope(host: InstalledHost) -> DiagnosticScope {
    match host {
        InstalledHost::Instance(instance) => DiagnosticScope::Client(instance),
        InstalledHost::Server(server) => DiagnosticScope::Server(server),
    }
}

/// A name the save dialog suggests, which says what the archive is and when it
/// was made without naming anything the user did not choose to share.
fn bundle_file_name(host: InstalledHost) -> String {
    format!(
        "ephervex-{}-diagnostics-{}.zip",
        host.kind(),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// Checks a host, and a client's shared launch files, for the Diagnostics surface.
async fn verification_data(
    app: &App,
    host: InstalledHost,
    reporter: &OperationReporter,
    cancel: &CancellationToken,
) -> CoreResult<VerificationData> {
    let report = app.verify_host(host, reporter, cancel).await?;
    let files = report.files.iter().map(verified_file_data).collect();
    let shared = match host {
        InstalledHost::Instance(instance) => {
            reporter.set_stage(Some("operation-stage-checking-shared-files".to_owned()));
            Some(
                app.verify_shared_artifacts(instance)
                    .await
                    .map(|shared| SharedArtifactData {
                        metadata_cached: shared.metadata_cached,
                        checked: shared.files.len(),
                        faults: shared.faults().map(verified_file_data).collect(),
                        not_yet_fetched: shared.not_yet_fetched().count(),
                        loader_libraries_uncovered: shared.loader_libraries_uncovered,
                    })
                    .map_err(|error| localized_error(&error)),
            )
        }
        InstalledHost::Server(_) => None,
    };
    Ok(VerificationData { files, shared })
}

fn verified_file_data(file: &ephervex_core::VerifiedFile) -> VerifiedFileData {
    VerifiedFileData {
        path: file.logical_path.display().to_string(),
        integrity: file.integrity.as_str(),
        sound: file.integrity.is_sound(),
        repairable: file.integrity.is_repairable() && file.has_source,
    }
}

fn repair_data(report: &ephervex_core::RepairReport, stopped: bool) -> RepairData {
    let paths = |paths: &[PathBuf]| {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    };
    RepairData {
        repaired: paths(&report.repaired),
        failed: paths(&report.failed),
        unrepairable: paths(&report.unrepairable),
        recovery: report
            .recovery_issues
            .iter()
            .map(|issue| issue.path.display().to_string())
            .collect(),
        stopped,
    }
}

fn isolation_plan_basis(plan: &IsolationPlan) -> u64 {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    let mut hasher = DefaultHasher::new();
    plan.hash(&mut hasher);
    hasher.finish()
}

/// Reports the host's current isolation plan fingerprint.
///
/// Every path that refreshes a host's mods after changing them goes through
/// here, so a setup planned before the change can be retired however the
/// change was made. A refresh that changed nothing reports the same basis.
fn send_isolation_basis(app: &App, host: InstalledHost, events: &Sender<Event>) {
    if let Ok(plan) = app.plan_isolation(host, &[]) {
        let _ = events.send(Event::IsolationBasis {
            host,
            basis: isolation_plan_basis(&plan),
        });
    }
}

/// Durable first and always: the list is correct without a catalog, and a
/// server that will not start is the moment a user least wants to wait for one.
fn send_host_mods(
    app: &App,
    host: InstalledHost,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
    force: bool,
) {
    match app.host_mod_snapshot(host) {
        Ok(snapshot) => {
            let review = app.crash_review(host).ok().flatten();
            let _ = events.send(Event::CrashNotice(crash_notice_data(host, review.as_ref())));
            let _ = events.send(Event::ManagedMods(managed_mods_data(
                &snapshot,
                review.as_ref(),
            )));
            send_isolation_basis(app, host, events);
            if claim_refresh(refreshed, host, force) {
                refresh_enriched_host_mods(app.paths().clone(), events, host);
            }
        }
        Err(error) => send_error(events, &error),
    }
}

/// Turns off the mods the newest crash report stated, and says how many.
///
/// Moves one instance, and the servers the user ticked, into persistent storage.
///
/// A server that could not be moved is a toast of its own rather than a failed
/// operation: the instance is already persistent by then, and saying so is more
/// use than reporting the whole thing as broken.
fn promote_instance(
    app: &App,
    instance: InstanceId,
    servers: Vec<ServerId>,
    events: &Sender<Event>,
) {
    let request = ephervex_core::PromotionRequest { servers };
    let reporter = OperationReporter::silent("operation-promoting-instance");
    match app.promote_instance(instance, &request, &reporter) {
        Ok(outcome) => {
            // Before the toast, so the dialog that asked cannot be answered a
            // second time while the first answer is still being applied.
            let _ = events.send(Event::CloseModal);
            let _ = events.send(Event::Toast {
                message: localized_value(
                    "gui-made-persistent",
                    &outcome.instance.root.display().to_string(),
                ),
                error: false,
            });
            for warning in &outcome.warnings {
                let _ = events.send(Event::Toast {
                    message: localized_values(
                        "gui-server-not-made-persistent",
                        &warning.name,
                        &warning.error.to_string(),
                    ),
                    error: true,
                });
            }
            send_snapshot(app, events);
        }
        Err(error) => send_modal_error(events, &error),
    }
}

/// Publishes everything the link settings section shows.
fn send_link_settings(app: &App, events: &Sender<Event>) {
    let _ = events.send(Event::LinkSettings(LinkSettingsData {
        trusted_hosts: app.trusted_pack_hosts().to_vec(),
    }));
}

/// Turns an address a link carried into an archive the import dialog can open.
///
/// Asks first whenever the host has not been agreed to before. The question is
/// the whole point of the feature's safety story, so it is asked here rather
/// than assumed by the caller: a frontend that forgot would otherwise download
/// from whatever a web page named (ADR 0027).
async fn open_pack_url(
    app: &mut App,
    url: &str,
    consent: Option<ephervex_core::HostConsent>,
    events: &Sender<Event>,
) {
    let source = match app.pack_source(url) {
        Ok(source) => source,
        Err(error) => {
            send_modal_error(events, &error);
            return;
        }
    };

    if consent.is_none() && !app.pack_host_is_trusted(&source) {
        let _ = events.send(Event::PackUrlConsent {
            url: url.to_owned(),
            display: source.display().to_owned(),
        });
        return;
    }

    let reporter = OperationReporter::silent("operation-downloading-pack");
    let outcome = app
        .fetch_pack(&source, consent, &reporter, &CancellationToken::new())
        .await;
    // Either way: the host is agreed to before a byte is fetched, so a download
    // that then failed still added a site the settings page has to show. Left
    // out, the list only caught up on the next start.
    send_link_settings(app, events);
    match outcome {
        // `keep` because the import that follows owns the file now: it runs as
        // a separate command, so tying the archive's life to this scope would
        // delete it before anything read it.
        Ok(staged) => {
            let _ = events.send(Event::PackDownloaded {
                archive: staged.keep(),
            });
        }
        Err(error) => send_modal_error(events, &error),
    }
}

/// Only what the report stated: a compat trigger is an inference and stays a
/// choice the user makes one row at a time. Only mods that are still enabled,
/// so pressing this twice does nothing the second time.
fn disable_suggested_mods(
    app: &App,
    server: ServerId,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
) {
    let host = InstalledHost::Server(server);
    let review = match app.crash_review(host) {
        Ok(Some(review)) => review,
        Ok(None) => return,
        Err(error) => {
            send_error(events, &error);
            return;
        }
    };

    let paths: Vec<PathBuf> = review
        .suggested()
        .filter_map(|entry| entry.logical_path.clone())
        .collect();
    if paths.is_empty() {
        let _ = events.send(Event::Toast {
            message: lookup("gui-crash-nothing-to-disable"),
            error: false,
        });
        return;
    }

    let mut disabled = 0usize;
    for path in &paths {
        match app.set_mod_enabled(host, path, false) {
            Ok(_) => disabled += 1,
            // One refusal must not leave the rest untried: the server being
            // busy is reported once, and whatever did get switched off is
            // still reported honestly below.
            Err(error) => send_error(events, &error),
        }
    }

    if disabled > 0 {
        let _ = events.send(Event::Toast {
            message: localized_count("gui-crash-disabled-count", disabled),
            error: false,
        });
    }
    // The mods just changed, so the remembered list is stale by definition.
    send_host_mods(app, host, events, refreshed, true);
}

/// What this launcher is running, with live resource use.
/// How one launch is addressed while it runs.
///
/// An instance holding two launches has two rows, two cancellations and two
/// locks, and all three have to agree on which is which.
fn launch_key(instance: InstanceId, launch: Option<&str>) -> String {
    match launch {
        Some(id) => format!("{instance}#{id}"),
        None => instance.to_string(),
    }
}

fn running_data(
    app: &App,
    monitor: &mut ProcessMonitor,
    cancellations: &CancellationRegistry,
) -> Vec<RunningData> {
    let Ok(mut running) = app.running_processes() else {
        return Vec::new();
    };
    monitor.observe(&mut running);

    // A launch can only be stopped the ordinary way by the run supervising it,
    // which is this one exactly when it still holds the cancellation.
    let supervised: HashSet<String> = cancellations
        .lock()
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default();

    running
        .into_iter()
        .map(|process| {
            let id = process.host.id();
            let can_stop = match process.host {
                InstalledHost::Instance(instance) => {
                    supervised.contains(&launch_key(instance, process.launch.as_deref()))
                }
                InstalledHost::Server(_) => process.controllable,
            };
            let uptime = format_uptime(process.uptime_seconds());
            RunningData {
                kind: process.host.kind().to_owned(),
                name: process.name,
                pid: process.pid.map_or_else(String::new, |pid| pid.to_string()),
                uptime,
                usage: format_usage(process.usage),
                last_line: process.last_line.unwrap_or_default(),
                controllable: process.controllable,
                can_stop,
                launch: process.launch.unwrap_or_default(),
                id,
            }
        })
        .collect()
}

/// How long a run has been going, in the largest unit that says it.
fn format_uptime(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return String::new();
    };
    if seconds < 60 {
        localized_count("gui-uptime-seconds", usize::try_from(seconds).unwrap_or(0))
    } else if seconds < 3_600 {
        localized_count(
            "gui-uptime-minutes",
            usize::try_from(seconds / 60).unwrap_or(0),
        )
    } else {
        lookup_args(
            "gui-uptime-hours",
            [
                ("hours", Arg::from(seconds / 3_600)),
                ("minutes", Arg::from((seconds % 3_600) / 60)),
            ],
        )
    }
}

/// Live CPU and memory, or nothing on the first sample of a process.
fn format_usage(usage: Option<ephervex_core::ProcessUsage>) -> String {
    let Some(usage) = usage else {
        return String::new();
    };
    lookup_args(
        "gui-process-usage",
        [
            // Formatted rather than cast: a percentage of one core has no
            // useful fractional part, and rounding it into an integer type is
            // a truncation this does not need to perform.
            ("cpu", Arg::from(format!("{:.0}", usage.cpu_percent))),
            ("memory", Arg::from(format_bytes(usage.memory_bytes))),
        ],
    )
}

fn host_snapshots_data(app: &App, host: InstalledHost) -> CoreResult<HostSnapshotsData> {
    let snapshots = app.snapshots(host).map_err(CoreError::from)?;
    let total_bytes = snapshots.iter().fold(0u64, |total, snapshot| {
        total.saturating_add(snapshot.apparent_bytes)
    });
    let has_plain_copies = snapshots
        .iter()
        .any(|snapshot| snapshot.copy_method == ephervex_core::CloneMethod::Copy);
    let rows = snapshots
        .into_iter()
        .map(|snapshot| {
            let origin = match &snapshot.origin {
                SnapshotOrigin::Manual => lookup("gui-snapshot-origin-manual"),
                SnapshotOrigin::BeforeRestore => lookup("gui-snapshot-origin-before-restore"),
                SnapshotOrigin::BeforeIsolation => lookup("gui-snapshot-origin-before-isolation"),
                SnapshotOrigin::Unknown(value) => value.clone(),
            };
            let uses_full_space = snapshot.copy_method == ephervex_core::CloneMethod::Copy;
            SnapshotData {
                id: snapshot.id.to_string(),
                label: snapshot.label,
                captured: relative_time(snapshot.captured_at),
                size: ephervex_core::format_binary_bytes(snapshot.apparent_bytes),
                mods: lookup_args(
                    "gui-snapshot-mod-counts",
                    [
                        ("enabled", Arg::from(snapshot.enabled_mods)),
                        ("disabled", Arg::from(snapshot.disabled_mods)),
                    ],
                ),
                origin,
                copy_method: lookup(if uses_full_space {
                    "gui-snapshot-copy"
                } else {
                    "gui-snapshot-reflink"
                }),
                uses_full_space,
            }
        })
        .collect();
    Ok(HostSnapshotsData {
        host: host.id(),
        host_kind: host_kind(host).to_owned(),
        rows,
        total: ephervex_core::format_binary_bytes(total_bytes),
        has_plain_copies,
    })
}

/// Sends the host's isolation state, first making the host match an active
/// session's intended mods.
///
/// Every read of an active session goes through here, so a round interrupted
/// by a crash or an outside change is reapplied before it can be answered.
fn send_isolation_state(
    app: &App,
    host: InstalledHost,
    generation: u64,
    open: bool,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
) {
    let data = match app.isolation_state(host) {
        Ok(Some(session)) => {
            let mut data = isolation_session_data(&session);
            (data.recovery, data.recovery_detail) =
                reconcile_isolation(app, host, events, refreshed);
            data
        }
        Ok(None) if open => match app.plan_isolation(host, &[]) {
            Ok(plan) => isolation_plan_data(host, &plan),
            Err(error) => {
                send_modal_error(events, &error);
                return;
            }
        },
        Ok(None) => inactive_isolation_data(host),
        Err(error) => {
            send_modal_error(events, &error);
            return;
        }
    };
    let _ = events.send(Event::Isolation {
        generation,
        open,
        data,
    });
}

/// Reapplies an active session's round when the host's mods differ from it,
/// and says what still blocks answering: nothing, a running host, or a failure.
fn reconcile_isolation(
    app: &App,
    host: InstalledHost,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
) -> (String, String) {
    match app.reconcile_isolation(host) {
        Ok(IsolationReconciliation::AlreadyApplied) => (String::new(), String::new()),
        Ok(IsolationReconciliation::Reapplied) => {
            let _ = events.send(Event::Toast {
                message: lookup("gui-isolation-reapplied"),
                error: false,
            });
            send_host_mods(app, host, events, refreshed, false);
            (String::new(), String::new())
        }
        Err(CoreError::Isolation(IsolationError::HostRunning { .. })) => {
            ("running".to_owned(), String::new())
        }
        Err(error) => ("failed".to_owned(), localized_error(&error)),
    }
}

fn inactive_isolation_data(host: InstalledHost) -> IsolationData {
    IsolationData {
        host: host.id(),
        host_kind: host_kind(host).to_owned(),
        active: false,
        phase: "none".to_owned(),
        round: 0,
        granularity: 0,
        enabled: Vec::new(),
        disabled: Vec::new(),
        candidates: Vec::new(),
        unrecorded: Vec::new(),
        unidentifiable: Vec::new(),
        missing: Vec::new(),
        linked_servers: Vec::new(),
        reduced_confidence: false,
        confidence: String::new(),
        remaining_candidates: Vec::new(),
        pinned: Vec::new(),
        required_dependencies: Vec::new(),
        history: Vec::new(),
        summary: String::new(),
        recovery: String::new(),
        recovery_detail: String::new(),
        basis: None,
    }
}

fn isolation_plan_data(host: InstalledHost, plan: &IsolationPlan) -> IsolationData {
    let names: HashMap<_, _> = plan
        .candidates
        .iter()
        .map(|candidate| (&candidate.root, candidate.name.as_str()))
        .collect();
    let mut candidates: Vec<_> = plan
        .initially_enabled
        .iter()
        .map(|path| IsolationCandidateData {
            path: path.to_string_lossy().into_owned(),
            display_path: isolation_mod_display(path),
            name: names.get(path).map_or_else(
                || {
                    path.file_name().map_or_else(
                        || path.to_string_lossy().into_owned(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                },
                |name| (*name).to_owned(),
            ),
        })
        .collect();
    candidates.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut data = inactive_isolation_data(host);
    "setup".clone_into(&mut data.phase);
    data.basis = Some(isolation_plan_basis(plan));
    data.candidates = candidates;
    data.unrecorded = isolation_mod_list(&plan.unrecorded);
    data.unidentifiable = isolation_mod_list(&plan.unidentifiable);
    data.missing = isolation_missing_list(&plan.missing);
    data.linked_servers = plan
        .linked_servers
        .iter()
        .map(|server| format!("{} ({})", server.name, server.id))
        .collect();
    data.summary = lookup_args(
        "gui-isolation-setup-summary",
        [("count", Arg::from(plan.candidates.len()))],
    );
    data
}

fn isolation_session_data(session: &IsolationSession) -> IsolationData {
    let host_kind = session.host_kind.clone();
    let host = session.host_id.clone();
    let (phase, round, granularity, enabled, disabled, confidence, remaining_candidates) =
        if let Some(round) = &session.current_round {
            (
                "round",
                usize::try_from(round.index).unwrap_or(usize::MAX),
                round.granularity,
                isolation_mod_list(&round.enabled),
                isolation_mod_list(&round.disabled),
                String::new(),
                Vec::new(),
            )
        } else if session.contradiction.is_some() {
            (
                "contradiction",
                session.history.len(),
                session.granularity,
                isolation_mod_list(session.intended_enabled()),
                Vec::new(),
                String::new(),
                Vec::new(),
            )
        } else if let Some(outcome) = &session.outcome {
            (
                "ready",
                session.history.len(),
                session.granularity,
                isolation_mod_list(&outcome.enabled),
                isolation_mod_list(&outcome.disabled),
                outcome.confidence.as_str().to_owned(),
                Vec::new(),
            )
        } else {
            (
                "contradiction",
                0,
                0,
                Vec::new(),
                Vec::new(),
                String::new(),
                Vec::new(),
            )
        };
    let (remaining_candidates, pinned, required_dependencies) =
        session.pending_report().map_or_else(
            || (remaining_candidates, Vec::new(), Vec::new()),
            |report| {
                (
                    isolation_remaining_candidates(&report),
                    isolation_pinned_result(&report),
                    isolation_mod_list(&report.required_dependencies),
                )
            },
        );
    IsolationData {
        host,
        host_kind,
        active: true,
        phase: phase.to_owned(),
        round,
        granularity,
        enabled,
        disabled,
        candidates: Vec::new(),
        unrecorded: isolation_mod_list(&session.plan.unrecorded),
        unidentifiable: isolation_mod_list(&session.plan.unidentifiable),
        missing: Vec::new(),
        linked_servers: session
            .plan
            .linked_servers
            .iter()
            .map(|server| format!("{} ({})", server.name, server.id))
            .collect(),
        reduced_confidence: session.reduced_confidence,
        confidence,
        remaining_candidates,
        pinned,
        required_dependencies,
        history: isolation_history(session),
        summary: if phase == "round" {
            lookup_args("gui-isolation-round-summary", [("round", Arg::from(round))])
        } else if phase == "contradiction" {
            lookup("gui-isolation-contradiction")
        } else {
            lookup("gui-isolation-ready-summary")
        },
        recovery: String::new(),
        recovery_detail: String::new(),
        basis: None,
    }
}

fn isolation_history(session: &IsolationSession) -> Vec<String> {
    session
        .history
        .iter()
        .rev()
        .map(|record| {
            lookup_args(
                "gui-isolation-history-entry",
                [
                    ("round", Arg::from(record.round.index)),
                    (
                        "answer",
                        Arg::from(lookup(match record.answer {
                            RoundAnswer::Reproduced => "gui-isolation-issue-present",
                            RoundAnswer::DidNotReproduce => "gui-isolation-issue-gone",
                        })),
                    ),
                    ("kind", Arg::from(record.round.kind.as_str())),
                    ("count", Arg::from(record.round.enabled.len())),
                ],
            )
        })
        .collect()
}

fn isolation_report_data(host: InstalledHost, report: &IsolationReport) -> IsolationData {
    let mut data = inactive_isolation_data(host);
    "result".clone_into(&mut data.phase);
    data.round = report.rounds;
    data.enabled = isolation_mod_list(&report.enabled);
    data.disabled = isolation_mod_list(&report.disabled);
    data.reduced_confidence = report.reduced_confidence;
    report.confidence.as_str().clone_into(&mut data.confidence);
    data.remaining_candidates = isolation_remaining_candidates(report);
    data.pinned = isolation_pinned_result(report);
    data.required_dependencies = isolation_mod_list(&report.required_dependencies);
    data.unrecorded = isolation_mod_list(&report.unrecorded);
    data.unidentifiable = isolation_mod_list(&report.unidentifiable);
    data.missing = isolation_missing_list(&report.missing);
    data.linked_servers = report
        .linked_servers
        .iter()
        .map(|server| format!("{} ({})", server.name, server.id))
        .collect();
    data.summary = lookup_args(
        "gui-isolation-result-summary",
        [("count", Arg::from(report.remaining_candidates.len()))],
    );
    data
}

fn isolation_remaining_candidates(report: &IsolationReport) -> Vec<String> {
    isolation_mod_list(
        &report
            .remaining_candidates
            .iter()
            .map(|candidate| candidate.root.clone())
            .collect::<Vec<_>>(),
    )
}

/// Pinned roots, then the files they require, which were equally never tested for removal.
fn isolation_pinned_result(report: &IsolationReport) -> Vec<String> {
    isolation_mod_list(&report.pinned)
        .into_iter()
        .chain(
            isolation_mod_list(&report.pinned_dependencies)
                .into_iter()
                .map(|path| {
                    lookup_args(
                        "gui-isolation-pinned-dependency",
                        [("path", Arg::from(path))],
                    )
                }),
        )
        .collect()
}

/// Isolation lists mod paths as they appear inside the host's mods directory,
/// ordered case-insensitively with the logical path breaking ties.
fn isolation_mod_list(paths: &[PathBuf]) -> Vec<String> {
    let mut rows: Vec<_> = paths
        .iter()
        .map(|path| (isolation_mod_display(path), path))
        .collect();
    rows.sort_by(|(left, left_path), (right, right_path)| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left_path.cmp(right_path))
    });
    rows.into_iter().map(|(display, _)| display).collect()
}

/// A logical mod path without the `mods/` prefix every isolation path shares.
/// Only for display: the logical path stays the identity everywhere else.
fn isolation_mod_display(path: &Path) -> String {
    path.strip_prefix("mods")
        .ok()
        .filter(|rest| !rest.as_os_str().is_empty())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn isolation_missing_list(missing: &[ephervex_core::IsolationMissingDependency]) -> Vec<String> {
    let mut rows: Vec<_> = missing
        .iter()
        .map(|missing| {
            format!(
                "{}: {}",
                isolation_mod_display(&missing.required_by),
                missing.dependency
            )
        })
        .collect();
    rows.sort_by_key(|row| (row.to_lowercase(), row.clone()));
    rows
}

fn snapshot_deletion_note(app: &App, host: InstalledHost) -> String {
    match app.snapshots(host) {
        Ok(snapshots) => localized_count("gui-delete-host-snapshots", snapshots.len()),
        Err(_) => lookup("gui-delete-host-snapshots-unreadable"),
    }
}

fn send_server_workspace(app: &App, server: ServerId, events: &Sender<Event>) {
    let result = (|| {
        let record = app.server(server)?;
        let properties = app.server_properties(server)?;
        Ok::<_, CoreError>(ServerWorkspaceData {
            motd: properties
                .get("motd")
                .unwrap_or_else(|| lookup("gui-default-motd")),
            max_players: properties
                .get("max-players")
                .unwrap_or_else(|| "20".to_owned()),
            difficulty: properties
                .get("difficulty")
                .unwrap_or_else(|| "normal".to_owned()),
            gamemode: properties
                .get("gamemode")
                .unwrap_or_else(|| "survival".to_owned()),
            allow_flight: property_bool(&properties, "allow-flight", false),
            pvp: property_bool(&properties, "pvp", true),
            allowlist: property_bool(&properties, "white-list", false),
            network: record.network,
        })
    })();
    match result {
        Ok(workspace) => {
            let _ = events.send(Event::ServerWorkspace(workspace));
        }
        Err(error) => {
            let message = localized_error(&error);
            let _ = events.send(Event::DiagnosticUnavailable(message.clone()));
            send_error(events, &error);
        }
    }
}

fn open_live_log(
    app: &App,
    source: LiveLogSource,
    events: &Sender<Event>,
    generation: &Arc<AtomicU64>,
) {
    let current_generation = generation.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    match source.clone() {
        // A file is read once. There is no tail to follow: a rotated log is
        // finished, and the one being written to is reachable as the live view.
        LiveLogSource::File { scope, ref name } => {
            match app.read_diagnostic(&DiagnosticRequest::new(scope, DiagnosticKind::Log, name)) {
                Ok(view) => {
                    let _ = events.send(Event::LiveLog(LiveLogUpdate {
                        source,
                        reset: true,
                        missing: false,
                        // A window rather than the whole file, exactly as the
                        // plain reader took it, and said so for the same reason.
                        source_truncated: view.truncated,
                        lines: view
                            .text
                            .lines()
                            .map(|line| LiveLogLine::File(line.to_owned()))
                            .collect(),
                    }));
                }
                Err(error) => send_error(events, &error),
            }
        }
        LiveLogSource::Server(server) => match app.server_output(server, 2_001) {
            Ok(mut capture) => {
                let bounded = capture.lines.len() > 2_000;
                if bounded {
                    capture.lines.remove(0);
                }
                let source_truncated = capture.retention.removed_lines > 0 || bounded;
                let _ = events.send(Event::LiveLog(LiveLogUpdate {
                    source,
                    reset: true,
                    missing: false,
                    source_truncated,
                    lines: capture.lines.into_iter().map(LiveLogLine::Server).collect(),
                }));
            }
            Err(error) => send_error(events, &error),
        },
        LiveLogSource::Client(instance) => {
            let mut tail = match app.client_log_tail(instance) {
                Ok(tail) => tail,
                Err(error) => {
                    send_error(events, &error);
                    return;
                }
            };
            let paths = app.paths().clone();
            let events = events.clone();
            let generation = Arc::clone(generation);
            if let Err(error) = thread::Builder::new()
                .name("ephervex-client-log-tail".to_owned())
                .spawn(move || {
                    let app = match App::open_at(paths) {
                        Ok(app) => app,
                        Err(error) => {
                            send_error(&events, &error);
                            return;
                        }
                    };
                    let mut first = true;
                    while generation.load(Ordering::Relaxed) == current_generation {
                        match app.poll_client_log(&mut tail) {
                            Ok(mut update) => {
                                if app
                                    .launch_state(instance)
                                    .is_ok_and(|state| state.is_none())
                                    && let Some(line) = tail.finish_partial_line()
                                {
                                    update.lines.push(line);
                                }
                                let has_update = first
                                    || update.reset
                                    || !update.lines.is_empty()
                                    || update.skipped_bytes > 0;
                                if has_update {
                                    let _ = events.send(Event::LiveLog(LiveLogUpdate {
                                        source: LiveLogSource::Client(instance),
                                        reset: first || update.reset,
                                        missing: update.missing,
                                        source_truncated: update.skipped_bytes > 0,
                                        lines: update
                                            .lines
                                            .into_iter()
                                            .map(LiveLogLine::Client)
                                            .collect(),
                                    }));
                                }
                                first = false;
                            }
                            Err(error) => {
                                tracing::warn!(%error, "client latest.log tail poll failed");
                            }
                        }
                        thread::sleep(Duration::from_millis(120));
                    }
                })
            {
                tracing::error!(%error, "could not start the client latest.log tail");
            }
        }
    }
}

fn send_diagnostics(
    app: &App,
    scope: DiagnosticScope,
    kind: DiagnosticKind,
    events: &Sender<Event>,
) {
    let result = app.diagnostic_entries(scope, kind).map(|entries| {
        let entity = scope_entity(app, scope);
        let (scope_kind, scope_id) = scope_parts(scope);
        entries
            .into_iter()
            .map(|entry| DiagnosticData {
                scope_kind: scope_kind.to_owned(),
                scope_id: scope_id.clone(),
                entity: entity.clone(),
                kind: entry.kind.as_str().to_owned(),
                name: entry.name,
                path: entry.path.display().to_string(),
                modified: entry
                    .modified
                    .map_or_else(|| lookup("gui-unknown-time"), relative_time),
                size: format_bytes(entry.size_bytes),
                summary: entry
                    .summary
                    .and_then(|summary| summary.headline().map(str::to_owned))
                    .unwrap_or_default(),
            })
            .collect::<Vec<_>>()
    });
    match result {
        Ok(rows) => {
            let (scope_kind, scope_id) = scope_parts(scope);
            let _ = events.send(Event::Diagnostics {
                rows,
                scope: scope_kind.to_owned(),
                scope_id,
                kind: kind.as_str().to_owned(),
            });
        }
        Err(error) => send_error(events, &error),
    }
}

fn send_diagnostic_content(
    app: &App,
    scope: DiagnosticScope,
    kind: DiagnosticKind,
    name: &str,
    query: Option<&str>,
    events: &Sender<Event>,
) {
    let request = DiagnosticRequest::new(scope, kind, name);
    let result = if let Some(query) = query.filter(|query| !query.is_empty()) {
        app.search_diagnostic(&request, query, &SearchOptions::default())
            .map(|search| (search.view, Some(search.outcome.matches.len())))
    } else {
        app.read_diagnostic(&request).map(|view| (view, None))
    };
    match result {
        Ok((view, matches)) => {
            let mut text = view.text;
            if view.truncated {
                text.push_str("\n\n[");
                text.push_str(&lookup("gui-diagnostic-window-bounded"));
                text.push_str("]\n");
            }
            if let Some(matches) = matches {
                let summary = lookup_args(
                    "gui-diagnostic-search-window-matches",
                    [("count", Arg::from(matches))],
                );
                text.insert_str(0, &format!("[{summary}]\n\n"));
            }
            let _ = events.send(Event::DiagnosticContent(DiagnosticContent {
                title: format!(
                    "{} | {} | {}",
                    scope_entity(app, scope),
                    diagnostic_kind_label(kind),
                    name
                ),
                path: view.path.display().to_string(),
                text,
                truncated: view.truncated,
                lossy: view.lossy,
                matches,
            }));
        }
        Err(error) => {
            let message = localized_error(&error);
            let _ = events.send(Event::DiagnosticUnavailable(message.clone()));
            send_error(events, &error);
        }
    }
}

/// One client launch, as the thread that supervises it needs it.
struct ClientLaunch {
    instance: InstanceId,
    entity: String,
    account: Option<String>,
    additional: Option<String>,
}

/// Told once whether a launch became the instance's running game; see
/// [`confirm_replacement`].
type LaunchStarted = tokio::sync::oneshot::Sender<Result<(), String>>;

fn spawn_launch(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    launches: RunningLaunches,
    launch: ClientLaunch,
    started: Option<LaunchStarted>,
) {
    let ClientLaunch {
        instance,
        entity,
        account,
        additional,
    } = launch;
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |mut app, events, runtime| {
            let (observer, mut receiver) = LaunchObserver::channel();
            let cancel = CancellationToken::new();
            // Canceling is what stops a game: the supervising task kills the
            // child and then waits for its real exit, so the instance's lock is
            // released rather than left claiming a process that is gone.
            let killing = Arc::clone(&cancellations);
            // Keyed by the launch rather than by the instance: one instance can
            // hold two, and a single key would let stopping either stop both.
            hold_cancellation(
                &killing,
                launch_key(instance, additional.as_deref()),
                &cancel,
            );
            let event_sender = events.clone();
            let entity_for_events = entity.clone();
            let paths_for_events = paths.clone();
            let observed_operation = Arc::new(Mutex::new(None::<String>));
            let forwarded_operation = Arc::clone(&observed_operation);
            let forwarded_cancellations = Arc::clone(&cancellations);
            let forwarded_token = cancel.clone();
            // Only an exclusive launch can be restarted: it is the one the
            // instance's own Stop and Restart act on.
            let registered = additional
                .is_none()
                .then(|| register_launch(&launches, &events, instance, account.clone()));
            // Whether this launch became the instance's running game, for a
            // restart waiting to find out. The recorded game is the evidence,
            // not the first line it prints.
            let (running_sender, running) = tokio::sync::oneshot::channel::<u32>();
            let (ended_sender, ended) = tokio::sync::oneshot::channel::<String>();
            let mut running_sender = Some(running_sender);
            let confirmation_paths = paths.clone();
            runtime.block_on(async move {
                let confirming = started.map(|started| {
                    spawn_confirmation(started, running, ended, confirmation_paths, instance)
                });
                let forward = tokio::spawn(async move {
                    while let Some(event) = receiver.recv().await {
                        if let LaunchEvent::Running { pid } = event
                            && let Some(sender) = running_sender.take()
                        {
                            let _ = sender.send(pid);
                        }
                        forward_launch_event(
                            &event_sender,
                            &entity_for_events,
                            &forwarded_operation,
                            &forwarded_cancellations,
                            &forwarded_token,
                            &paths_for_events,
                            event,
                        );
                    }
                });
                let result = app
                    .launch(
                        instance,
                        &LaunchOptions {
                            install_java_if_missing: true,
                            account,
                            additional,
                            ..LaunchOptions::default()
                        },
                        &observer,
                        &cancel,
                    )
                    .await;
                drop(observer);
                let _ = forward.await;
                let _ = ended_sender.send(replacement_end_reason(&result));
                if let Some(confirming) = confirming {
                    let _ = confirming.await;
                }
                report_launch_result(&events, instance, &entity, &observed_operation, &result);
                release_launch_cancellation(&killing, instance, &observed_operation);
                // Announced only after everything this launch held is
                // released, so a restart waiting on it cannot start the next
                // launch while this one still claims the instance.
                if let Some((generation, finished)) = registered {
                    unregister_launch(&launches, &events, instance, generation);
                    let _ = finished.send(true);
                }
                if let Ok(app) = App::open_at(paths) {
                    send_snapshot(&app, &events);
                }
            });
        },
    );
}

/// Says how a launch ended, on the activity row it has had since it began.
fn report_launch_result(
    events: &Sender<Event>,
    instance: InstanceId,
    entity: &str,
    observed_operation: &Mutex<Option<String>>,
    result: &CoreResult<ephervex_core::LaunchOutcome>,
) {
    match result {
        Ok(outcome) => {
            let detail = if outcome.is_success() {
                lookup("gui-activity-exited-cleanly")
            } else {
                lookup("gui-activity-game-error")
            };
            send_activity_state(
                events,
                outcome.operation.to_string(),
                &lookup("gui-activity-launch-minecraft"),
                entity,
                &detail,
                if outcome.is_success() {
                    "completed"
                } else {
                    "failed"
                },
                false,
                !outcome.is_success(),
                false,
            );
        }
        Err(error) => {
            let id = observed_operation
                .lock()
                .ok()
                .and_then(|observed| observed.clone())
                .unwrap_or_else(|| instance.to_string());
            send_activity_state(
                events,
                id,
                &lookup("gui-activity-launch-minecraft"),
                entity,
                &localized_error(error),
                "failed",
                false,
                true,
                false,
            );
            send_error(events, error);
        }
    }
}

/// How long a replacement's game must stay the instance's running game before
/// a restart counts it as started. Long enough for a JVM that cannot start to
/// have exited; a crash after it is the game's own, not a failed restart.
const RESTART_CONFIRMATION: Duration = Duration::from_secs(3);

/// Decides whether a replacement launch became the instance's running game.
///
/// `running` resolves with the game's pid once the launch has recorded it in
/// the instance's lock, or `None` when the launch ended first. `ended` resolves
/// when the launch returns, with why it is no longer running. The game has to
/// outlast `window` and still be the process the lock names: a JVM that prints
/// an error and exits, or exits without a word, is a failed start.
async fn confirm_replacement(
    running: impl std::future::Future<Output = Option<u32>>,
    ended: impl std::future::Future<Output = String>,
    still_recorded: impl FnOnce(u32) -> bool,
    window: Duration,
) -> Result<(), String> {
    let ended = std::pin::pin!(ended);
    let mut ended = ended;
    let pid = tokio::select! {
        biased;
        reason = &mut ended => return Err(reason),
        pid = running => match pid {
            Some(pid) => pid,
            None => return Err(ended.await),
        },
    };
    tokio::select! {
        biased;
        reason = &mut ended => Err(reason),
        () = tokio::time::sleep(window) => {
            if still_recorded(pid) {
                Ok(())
            } else {
                Err(lookup("gui-restart-exited-at-once"))
            }
        }
    }
}

/// Watches a launch a restart is waiting on, and tells the restart once whether
/// it became the instance's running game.
fn spawn_confirmation(
    started: LaunchStarted,
    running: tokio::sync::oneshot::Receiver<u32>,
    ended: tokio::sync::oneshot::Receiver<String>,
    paths: Paths,
    instance: InstanceId,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let outcome = confirm_replacement(
            async { running.await.ok() },
            async {
                ended
                    .await
                    .unwrap_or_else(|_| lookup("gui-restart-launch-lost"))
            },
            |pid| recorded_game_is(&paths, instance, pid),
            RESTART_CONFIRMATION,
        )
        .await;
        let _ = started.send(outcome);
    })
}

/// Why a launch that has returned is not a running replacement.
fn replacement_end_reason(result: &CoreResult<ephervex_core::LaunchOutcome>) -> String {
    match result {
        Ok(outcome) if outcome.is_success() => lookup("gui-restart-exited-at-once"),
        Ok(_) => lookup("gui-restart-game-failed"),
        Err(error) => localized_error(error),
    }
}

/// Whether the instance's lock still names `pid` as its live, proven game.
fn recorded_game_is(paths: &Paths, instance: InstanceId, pid: u32) -> bool {
    App::open_at(paths.clone())
        .and_then(|app| app.launch_state(instance))
        .is_ok_and(|state| {
            state.is_some_and(|state| state.game_pid == Some(pid) && state.game_is_alive())
        })
}

/// What the worker keeps about the client launches it started.
struct LaunchTracking {
    cancellations: CancellationRegistry,
    launches: RunningLaunches,
}

/// One client restart, as its thread needs it.
struct ClientRestart {
    instance: InstanceId,
    entity: String,
    request: u64,
    /// The launch being replaced, and the request it was made with.
    running: RunningLaunch,
    /// What stops it: the same token its own Stop cancels.
    token: CancellationToken,
}

/// Reports a restart's stage to every view of the host and to the activity
/// list, under one activity row for the whole restart.
fn restart_stage_reporter(
    events: &Sender<Event>,
    host: InstalledHost,
    request: u64,
    entity: &str,
) -> impl Fn(RestartStage) {
    let events = events.clone();
    let entity = entity.to_owned();
    move |stage| {
        let _ = events.send(Event::HostRestart {
            host,
            request,
            stage: Some(stage),
        });
        send_activity_state(
            &events,
            restart_activity_id(host, request),
            &lookup("gui-activity-restarting"),
            &entity,
            &lookup(match stage {
                RestartStage::Stopping => "gui-restart-stage-stopping",
                RestartStage::Starting => "gui-restart-stage-starting",
            }),
            "running",
            true,
            false,
            false,
        );
    }
}

fn restart_activity_id(host: InstalledHost, request: u64) -> String {
    format!("restart:{}:{request}", host_key(host))
}

/// Ends a restart: every view of the host stops showing a stage, and the
/// outcome is said once, in the activity list and as a toast.
fn report_restart_outcome(
    events: &Sender<Event>,
    host: InstalledHost,
    request: u64,
    entity: &str,
    outcome: &RestartOutcome,
) {
    let _ = events.send(Event::HostRestart {
        host,
        request,
        stage: None,
    });
    let (message, failed) = match outcome {
        RestartOutcome::Restarted => (localized_value("gui-restart-done", entity), false),
        RestartOutcome::NotStopped => (localized_value("gui-restart-not-stopped", entity), true),
        RestartOutcome::StartFailed(detail) => (
            lookup_args(
                "gui-restart-start-failed",
                [
                    ("value", Arg::from(entity)),
                    ("detail", Arg::from(detail.as_str())),
                ],
            ),
            true,
        ),
        RestartOutcome::Interrupted(detail) => (
            lookup_args(
                "gui-restart-interrupted",
                [
                    ("value", Arg::from(entity)),
                    ("detail", Arg::from(detail.as_str())),
                ],
            ),
            true,
        ),
    };
    send_activity_state(
        events,
        restart_activity_id(host, request),
        &lookup("gui-activity-restarting"),
        entity,
        &message,
        if failed { "failed" } else { "completed" },
        false,
        failed,
        false,
    );
    let _ = events.send(Event::Toast {
        message,
        error: failed,
    });
}

/// Restarts a client this run launched: stop it the way its own Stop does,
/// wait until that launch has returned and nothing holds the instance, then
/// launch it again through the ordinary path with the same account.
///
/// Not cancelable once accepted. The host-change claim is held until the new
/// launch is running or has failed, so nothing rewrites the instance in the
/// moment between the two.
fn spawn_client_restart(
    paths: Paths,
    events: &Sender<Event>,
    tracking: LaunchTracking,
    restart: ClientRestart,
    in_flight: HostChangeInFlight,
) {
    let ClientRestart {
        instance,
        entity,
        request,
        running,
        token,
    } = restart;
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |app, events, runtime| {
            let _in_flight = in_flight;
            let host = InstalledHost::Instance(instance);
            let stage = restart_stage_reporter(&events, host, request, &entity);
            let mut finished = running.finished.clone();
            let outcome = runtime.block_on(restart_sequence(
                || token.cancel(),
                async {
                    let returned =
                        tokio::time::timeout(CLIENT_STOP_WAIT, finished.wait_for(|done| *done))
                            .await
                            .is_ok();
                    returned
                        && app
                            .launch_state(instance)
                            .is_ok_and(|state| state.is_none())
                },
                || {
                    let (sender, started) = tokio::sync::oneshot::channel();
                    spawn_launch(
                        paths.clone(),
                        &events,
                        tracking.cancellations,
                        tracking.launches,
                        ClientLaunch {
                            instance,
                            entity: entity.clone(),
                            account: running.account.clone(),
                            additional: None,
                        },
                        Some(sender),
                    );
                    async move {
                        started
                            .await
                            .unwrap_or_else(|_| Err(lookup("gui-restart-launch-lost")))
                    }
                },
                &stage,
            ));
            report_restart_outcome(&events, host, request, &entity, &outcome);
            refresh_snapshot(&paths, &events);
        },
    );
}

/// What the supervisor's answer to a restart means for the restart.
fn server_restart_outcome(response: CoreResult<ephervex_core::ControlResponse>) -> RestartOutcome {
    use ephervex_core::ControlResponse;
    match response {
        Ok(ControlResponse::Restarted { .. }) => RestartOutcome::Restarted,
        Ok(ControlResponse::StartFailed(detail)) => RestartOutcome::StartFailed(detail),
        Ok(ControlResponse::Exited(_)) => {
            RestartOutcome::Interrupted(lookup("gui-restart-stopped-instead"))
        }
        Ok(ControlResponse::Refused(detail)) => RestartOutcome::Interrupted(detail),
        Ok(ControlResponse::Running | ControlResponse::Accepted) => {
            RestartOutcome::Interrupted(lookup("gui-restart-unexpected-answer"))
        }
        Err(error) => RestartOutcome::Interrupted(localized_error(&error)),
    }
}

/// Restarts a server through its own supervisor, which stops the child, waits
/// for it to exit, and starts the same command once. The supervisor reports
/// the move from stopping to starting itself.
fn spawn_server_restart(
    paths: Paths,
    events: &Sender<Event>,
    server: ServerId,
    entity: String,
    request: u64,
    in_flight: HostChangeInFlight,
) {
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |app, events, runtime| {
            let _in_flight = in_flight;
            let host = InstalledHost::Server(server);
            restart_stage_reporter(&events, host, request, &entity)(RestartStage::Stopping);
            let outcome = server_restart_outcome(runtime.block_on(app.restart_server(server)));
            report_restart_outcome(&events, host, request, &entity, &outcome);
            refresh_snapshot(&paths, &events);
            if let Ok(app) = App::open_at(paths) {
                send_server_workspace(&app, server, &events);
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn forward_launch_event(
    events: &Sender<Event>,
    entity: &str,
    observed_operation: &Mutex<Option<String>>,
    cancellations: &CancellationRegistry,
    cancel: &CancellationToken,
    paths: &Paths,
    event: LaunchEvent,
) {
    match event {
        LaunchEvent::Preparing { operation } => {
            if let Ok(mut observed) = observed_operation.lock() {
                *observed = Some(operation.to_string());
            }
            hold_cancellation(cancellations, operation.to_string(), cancel);
            send_activity_state(
                events,
                operation.to_string(),
                &lookup("gui-activity-launching-minecraft"),
                entity,
                &lookup("gui-activity-preparing-environment"),
                "running",
                true,
                false,
                true,
            );
            refresh_snapshot(paths, events);
        }
        LaunchEvent::Downloading(operation) => {
            send_operation(events, &operation, entity.to_owned(), false);
        }
        LaunchEvent::Starting(_) => {
            let _ = events.send(Event::Toast {
                message: localized_value("gui-starting-minecraft", entity),
                error: false,
            });
            // The same row, still active, now saying what it is actually doing.
            // Preparation is over and the game is running; without this the
            // activity list describes a download for as long as the session
            // lasts, and the status bar's count is the only sign anything is
            // happening at all.
            if let Ok(observed) = observed_operation.lock()
                && let Some(operation) = observed.clone()
            {
                send_activity_state(
                    events,
                    operation,
                    &lookup("gui-activity-launching-minecraft"),
                    entity,
                    &lookup("gui-activity-minecraft-running"),
                    "running",
                    true,
                    false,
                    true,
                );
            }
            refresh_snapshot(paths, events);
        }
        LaunchEvent::Failed { message_id, .. } => {
            let _ = events.send(Event::Toast {
                message: lookup(message_id),
                error: true,
            });
        }
        _ => {}
    }
}

fn spawn_server(paths: Paths, events: &Sender<Event>, server: ServerId, entity: String) {
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |app, events, runtime| {
            let (observer, mut receiver) = ServerObserver::channel();
            let event_sender = events.clone();
            let entity_for_events = entity.clone();
            let paths_for_events = paths.clone();
            let observed_operation = Arc::new(Mutex::new(None::<String>));
            let forwarded_operation = Arc::clone(&observed_operation);
            runtime.block_on(async move {
                let forward = tokio::spawn(async move {
                    while let Some(event) = receiver.recv().await {
                        if let ServerEvent::Output(line) = event {
                            let mut lines = vec![LiveLogLine::Server(line)];
                            while lines.len() < 256 {
                                match receiver.try_recv() {
                                    Ok(ServerEvent::Output(line)) => {
                                        lines.push(LiveLogLine::Server(line));
                                    }
                                    Ok(other) => forward_server_event(
                                        &event_sender,
                                        &entity_for_events,
                                        &paths_for_events,
                                        &forwarded_operation,
                                        server,
                                        other,
                                    ),
                                    Err(_) => break,
                                }
                            }
                            let _ = event_sender.send(Event::LiveLog(LiveLogUpdate {
                                source: LiveLogSource::Server(server),
                                reset: false,
                                missing: false,
                                source_truncated: false,
                                lines,
                            }));
                        } else {
                            forward_server_event(
                                &event_sender,
                                &entity_for_events,
                                &paths_for_events,
                                &forwarded_operation,
                                server,
                                event,
                            );
                        }
                    }
                });
                let result = app.start_server(server, None, &observer).await;
                drop(observer);
                let _ = forward.await;
                match result {
                    Ok(exit) => {
                        let failed = exit.code != Some(0);
                        let id = observed_operation
                            .lock()
                            .ok()
                            .and_then(|observed| observed.clone())
                            .unwrap_or_else(|| server.to_string());
                        send_activity_state(
                            &events,
                            id,
                            &lookup("gui-activity-server-run"),
                            &entity,
                            &lookup("gui-activity-server-exited"),
                            if failed { "failed" } else { "completed" },
                            false,
                            failed,
                            false,
                        );
                    }
                    Err(error) => {
                        let id = observed_operation
                            .lock()
                            .ok()
                            .and_then(|observed| observed.clone())
                            .unwrap_or_else(|| server.to_string());
                        send_activity_state(
                            &events,
                            id,
                            &lookup("gui-activity-starting-server"),
                            &entity,
                            &localized_error(&error),
                            "failed",
                            false,
                            true,
                            false,
                        );
                        send_error(&events, &error);
                    }
                }
                if let Ok(app) = App::open_at(paths) {
                    send_snapshot(&app, &events);
                    send_server_workspace(&app, server, &events);
                }
            });
        },
    );
}

fn forward_server_event(
    events: &Sender<Event>,
    entity: &str,
    paths: &Paths,
    observed_operation: &Mutex<Option<String>>,
    server: ServerId,
    event: ServerEvent,
) {
    match event {
        ServerEvent::Preparing { operation } => {
            if let Ok(mut observed) = observed_operation.lock() {
                *observed = Some(operation.to_string());
            }
            send_activity_state(
                events,
                operation.to_string(),
                &lookup("gui-activity-starting-server"),
                entity,
                &lookup("gui-activity-validating-server"),
                "running",
                true,
                false,
                false,
            );
            refresh_snapshot(paths, events);
        }
        ServerEvent::Started { .. } => {
            let _ = events.send(Event::Toast {
                message: localized_value("gui-server-running", entity),
                error: false,
            });
            refresh_snapshot(paths, events);
        }
        ServerEvent::CapturePruned(_) | ServerEvent::CaptureUnavailable { .. } => {
            let _ = events.send(Event::LiveLog(LiveLogUpdate {
                source: LiveLogSource::Server(server),
                reset: false,
                missing: false,
                source_truncated: true,
                lines: Vec::new(),
            }));
        }
        ServerEvent::Warning(warning) => {
            let _ = events.send(Event::Toast {
                message: server_warning_message(&warning),
                error: false,
            });
        }
        // The supervisor saw the old child exit for a restart. Stopping is
        // over; what the restart is waiting for now is the new start.
        ServerEvent::Restarting => {
            let _ = events.send(Event::HostRestart {
                host: InstalledHost::Server(server),
                request: 0,
                stage: Some(RestartStage::Starting),
            });
        }
        _ => {}
    }
}

// The three observers intentionally stay in one scope so their channel and
// runtime ownership cannot drift apart from the combined operation.
#[allow(clippy::too_many_lines)]
fn spawn_connect(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    client: InstanceId,
    server: ServerId,
    entity: String,
) {
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |mut app, events, runtime| {
            runtime.block_on(async move {
                let (connect_observer, mut connect_events) = LaunchAndConnectObserver::channel();
                let (server_observer, mut server_events) = ServerObserver::channel();
                let (launch_observer, mut launch_events) = LaunchObserver::channel();
                let cancel = CancellationToken::new();
                // A client started this way is running just as much as one
                // started from the Launch button, so it is stoppable by the same
                // instance id.
                hold_cancellation(&cancellations, client.to_string(), &cancel);
                let refresh_paths = paths.clone();
                let refresh_events = events.clone();
                let connect_forward = tokio::spawn(async move {
                    while connect_events.recv().await.is_some() {
                        refresh_snapshot(&refresh_paths, &refresh_events);
                    }
                });
                let server_paths = paths.clone();
                let server_event_sender = events.clone();
                let server_entity = entity.clone();
                let observed_operation = Arc::new(Mutex::new(None::<String>));
                let forwarded_operation = Arc::clone(&observed_operation);
                let server_forward = tokio::spawn(async move {
                    while let Some(event) = server_events.recv().await {
                        if let ServerEvent::Output(line) = event {
                            let mut lines = vec![LiveLogLine::Server(line)];
                            while lines.len() < 256 {
                                match server_events.try_recv() {
                                    Ok(ServerEvent::Output(line)) => {
                                        lines.push(LiveLogLine::Server(line));
                                    }
                                    Ok(other) => forward_server_event(
                                        &server_event_sender,
                                        &server_entity,
                                        &server_paths,
                                        &forwarded_operation,
                                        server,
                                        other,
                                    ),
                                    Err(_) => break,
                                }
                            }
                            let _ = server_event_sender.send(Event::LiveLog(LiveLogUpdate {
                                source: LiveLogSource::Server(server),
                                reset: false,
                                missing: false,
                                source_truncated: false,
                                lines,
                            }));
                        } else {
                            forward_server_event(
                                &server_event_sender,
                                &server_entity,
                                &server_paths,
                                &forwarded_operation,
                                server,
                                event,
                            );
                        }
                    }
                });
                let launch_paths = paths.clone();
                let launch_event_sender = events.clone();
                let launch_entity = entity.clone();
                let launch_operation = Arc::new(Mutex::new(None::<String>));
                let forwarded_launch_operation = Arc::clone(&launch_operation);
                let forwarded_cancellations = Arc::clone(&cancellations);
                let forwarded_token = cancel.clone();
                let launch_forward = tokio::spawn(async move {
                    while let Some(event) = launch_events.recv().await {
                        forward_launch_event(
                            &launch_event_sender,
                            &launch_entity,
                            &forwarded_launch_operation,
                            &forwarded_cancellations,
                            &forwarded_token,
                            &launch_paths,
                            event,
                        );
                    }
                });
                let result = app
                    .launch_and_connect(
                        client,
                        server,
                        &LaunchAndConnectOptions::default(),
                        &connect_observer,
                        &server_observer,
                        &launch_observer,
                        &cancel,
                    )
                    .await;
                drop(connect_observer);
                drop(server_observer);
                drop(launch_observer);
                connect_forward.abort();
                let _ = launch_forward.await;
                match result {
                    Ok(outcome) => send_activity_state(
                        &events,
                        outcome.launch.operation.to_string(),
                        &lookup("gui-launch-and-connect"),
                        &entity,
                        &localized_value("gui-activity-connected-to", &outcome.target.to_string()),
                        "completed",
                        false,
                        false,
                        false,
                    ),
                    Err(error) => send_error(&events, &error),
                }
                release_launch_cancellation(&cancellations, client, &launch_operation);
                if let Ok(app) = App::open_at(paths.clone()) {
                    send_snapshot(&app, &events);
                }
                // If this operation started the server, its supervisor is a
                // task on this runtime. Keep the runtime alive until that
                // server exits instead of canceling it when the client closes.
                let _ = server_forward.await;
                if let Ok(app) = App::open_at(paths) {
                    send_snapshot(&app, &events);
                }
            });
        },
    );
}

fn spawn_login(paths: Paths, events: &Sender<Event>) {
    spawn_detached(paths, events.clone(), move |mut app, events, runtime| {
        runtime.block_on(async move {
            let (observer, mut receiver) = AuthObserver::channel();
            let auth_events = events.clone();
            let forward = tokio::spawn(async move {
                while let Some(event) = receiver.recv().await {
                    match event {
                        AuthEvent::OpenBrowser { url } => {
                            if !ephervex_core::open_in_browser(&url) {
                                let _ = auth_events.send(Event::Toast {
                                    message: lookup("gui-signin-browser-failed"),
                                    error: true,
                                });
                            }
                        }
                        AuthEvent::DeviceCode(prompt) => {
                            let _ = auth_events.send(Event::Toast {
                                message: localized_values(
                                    "gui-device-code-prompt",
                                    &prompt.verification_uri,
                                    &prompt.user_code,
                                ),
                                error: false,
                            });
                        }
                        AuthEvent::Stage(stage) => {
                            let _ = auth_events.send(Event::Toast {
                                message: localized_value("gui-signin-stage", &stage.to_string()),
                                error: false,
                            });
                        }
                        _ => {}
                    }
                }
            });
            let result = app
                .login(
                    &LoginOptions::default(),
                    &observer,
                    &CancellationToken::new(),
                )
                .await;
            drop(observer);
            let _ = forward.await;
            match result {
                Ok(login) => {
                    let _ = events.send(Event::Toast {
                        message: localized_value("gui-signed-in", &login.account.name),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            }
        });
    });
}

/// What a finished background operation wants shown.
///
/// Most operations have one line to say. A multi-target one does not, and
/// letting it say so is what keeps the shared helper from overwriting a report
/// with a toast.
enum OperationReport {
    Toast(String),
    /// The operation has already sent its own result.
    Reported,
}

/// How a pending client or server creation reached its terminal state.
enum CreationOutcome {
    Created(String),
    /// The user left a manual artifact-acquisition prompt. The staged server
    /// was already discarded, so there is no error and no success to report.
    Abandoned,
}

struct AsyncOperationRequest {
    title: &'static str,
    entity: String,
    allow_cancel: bool,
    finished: Option<Event>,
}

fn refresh_snapshot(paths: &Paths, events: &Sender<Event>) {
    if let Ok(app) = App::open_at(paths.clone()) {
        send_snapshot(&app, events);
    }
}

/// `finished` is sent whichever way the operation ended, including a
/// cancellation and a setup failure. Anything waiting its turn behind this
/// operation has to hear about every ending, not only the successful one.
fn spawn_async_operation<F, Fut>(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    request: AsyncOperationRequest,
    operation: F,
) where
    F: FnOnce(App, OperationReporter, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<OperationReport, CoreError>> + 'static,
{
    let AsyncOperationRequest {
        title,
        entity,
        allow_cancel,
        finished,
    } = request;
    let announcement = finished.clone();
    spawn_detached_with_setup_failure(
        paths.clone(),
        events.clone(),
        move |events| {
            if let Some(finished) = announcement {
                let _ = events.send(finished);
            }
        },
        move |app, events, runtime| {
            let sink = Arc::new(UiProgressSink {
                events: events.clone(),
                entity,
                allow_cancel,
                pending: None,
            });
            let reporter = OperationReporter::new(title, allow_cancel, sink);
            reporter.begin(None);
            let cancel = CancellationToken::new();
            register_cancellation(&cancellations, &reporter, &cancel);
            let result = runtime.block_on(operation(app, reporter.clone(), cancel));
            remove_cancellation(&cancellations, &reporter);
            match result {
                Ok(OperationReport::Toast(message)) => {
                    reporter.finish(OperationStatus::Completed);
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message,
                        error: false,
                    });
                }
                // The operation has already said what it did, in more detail
                // than one line could. Closing the modal here would take that
                // report off the screen before it was read.
                Ok(OperationReport::Reported) => {
                    reporter.finish(OperationStatus::Completed);
                }
                Err(error) => {
                    if error.is_cancellation() {
                        reporter.finish(OperationStatus::Canceled);
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-operation-canceled"),
                            error: false,
                        });
                    } else {
                        reporter.finish(OperationStatus::Failed(localized_error(&error)));
                        send_error(&events, &error);
                    }
                }
            }
            if let Some(finished) = finished {
                let _ = events.send(finished);
            }
            if let Ok(app) = App::open_at(paths) {
                send_snapshot(&app, &events);
            }
        },
    );
}

/// What one creation operation is and how it should report itself.
struct CreationRequest {
    id: String,
    title: &'static str,
    entity: String,
    /// Whether the pending card for this creation should receive live
    /// stage-by-stage progress. `false` for a caller with no pending card
    /// watching this specific request id.
    report_progress: bool,
}

fn spawn_creation_operation<F, Fut>(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    request: CreationRequest,
    operation: F,
) where
    F: FnOnce(App, OperationReporter, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<CreationOutcome, CoreError>> + 'static,
{
    let CreationRequest {
        id: request_id,
        title,
        entity,
        report_progress,
    } = request;
    let failed_request_id = request_id.clone();
    let progress_request_id = request_id.clone();
    spawn_detached_with_setup_failure(
        paths.clone(),
        events.clone(),
        move |events| {
            let _ = events.send(Event::CreationFinished {
                request_id: failed_request_id,
            });
        },
        move |app, events, runtime| {
            let sink = Arc::new(UiProgressSink {
                events: events.clone(),
                entity,
                allow_cancel: false,
                pending: report_progress.then_some(progress_request_id),
            });
            let reporter = OperationReporter::new(title, false, sink);
            reporter.begin(Some(lookup("gui-activity-preparing-environment")));
            let cancel = CancellationToken::new();
            register_cancellation(&cancellations, &reporter, &cancel);
            let result = runtime.block_on(operation(app, reporter.clone(), cancel));
            remove_cancellation(&cancellations, &reporter);
            match result {
                Ok(CreationOutcome::Created(message)) => {
                    reporter.finish(OperationStatus::Completed);
                    let _ = events.send(Event::Toast {
                        message,
                        error: false,
                    });
                }
                Ok(CreationOutcome::Abandoned) => {
                    reporter.finish(OperationStatus::Canceled);
                }
                Err(error) => {
                    reporter.finish(OperationStatus::Failed(localized_error(&error)));
                    send_error(&events, &error);
                }
            }
            let _ = events.send(Event::CreationFinished { request_id });
            if let Ok(app) = App::open_at(paths) {
                send_snapshot(&app, &events);
            }
        },
    );
}

fn register_cancellation(
    cancellations: &CancellationRegistry,
    reporter: &OperationReporter,
    token: &CancellationToken,
) {
    hold_cancellation(cancellations, reporter.id().to_string(), token);
}

/// Files a cancellation token under a key a frontend can address it by.
fn hold_cancellation(cancellations: &CancellationRegistry, key: String, token: &CancellationToken) {
    if let Ok(mut entries) = cancellations.lock() {
        entries.insert(key, token.clone());
    }
}

/// Drops the token a launch was filed under, by both of its keys.
///
/// A launch is addressable twice: by its instance, which is what a card's Kill
/// Instance action knows, and by the operation id it turns out to have, which is
/// what the activity list's Cancel knows. Both go when the launch ends.
fn release_launch_cancellation(
    cancellations: &CancellationRegistry,
    instance: InstanceId,
    observed_operation: &Mutex<Option<String>>,
) {
    if let Ok(mut entries) = cancellations.lock() {
        entries.remove(&instance.to_string());
        if let Ok(observed) = observed_operation.lock()
            && let Some(operation) = observed.as_deref()
        {
            entries.remove(operation);
        }
    }
}

fn remove_cancellation(cancellations: &CancellationRegistry, reporter: &OperationReporter) {
    if let Ok(mut entries) = cancellations.lock() {
        entries.remove(&reporter.id().to_string());
    }
}

/// Inspects an archive and reports what importing it would involve.
///
/// Returns the token for the provider resolution this started, where the pack
/// needs one, so the caller can cancel it when a newer attempt replaces this.
fn analyze_import(
    app: &App,
    events: &Sender<Event>,
    generation: u64,
    archive: &Path,
) -> Option<CancellationToken> {
    match app.analyze_pack(archive) {
        Ok(analysis) => {
            let presentation = ImportPresentation::from_analysis(&analysis);
            let needs_resolution = presentation.needs_provider_resolution;
            let _ = events.send(Event::ImportAnalyzed {
                generation,
                archive: archive.to_path_buf(),
                result: Ok(presentation),
            });
            needs_resolution.then(|| {
                let cancel = CancellationToken::new();
                spawn_import_resolution(
                    app.paths().clone(),
                    events.clone(),
                    generation,
                    analysis,
                    cancel.clone(),
                );
                cancel
            })
        }
        Err(error) => {
            tracing::warn!(
                message_id = error.message_id(),
                detail = %ErrorChain(&error),
                "pack analysis failed"
            );
            let _ = events.send(Event::ImportAnalyzed {
                generation,
                archive: archive.to_path_buf(),
                result: Err(import_failure(&error)),
            });
            None
        }
    }
}

fn instance_runtime_data(app: &App, instance: InstanceId) -> CoreResult<InstanceRuntimeData> {
    let record = app.client(instance)?;
    let global = app.java_settings();
    let jvm = record.jvm_override.as_ref();
    let number = |value: Option<u32>| value.map(|mib| mib.to_string()).unwrap_or_default();
    Ok(InstanceRuntimeData {
        instance: instance.to_string(),
        java: record
            .java_override
            .as_ref()
            .map(|java| java.path.display().to_string())
            .unwrap_or_default(),
        min_memory: number(jvm.and_then(|jvm| jvm.min_memory_mib)),
        max_memory: number(jvm.and_then(|jvm| jvm.max_memory_mib)),
        jvm_args: jvm.map(|jvm| jvm.jvm_args.join(" ")).unwrap_or_default(),
        pinned: record.java_override.is_some() || jvm.is_some(),
        global: localized_values(
            "gui-instance-runtime-global",
            &format!("{} - {} MiB", global.min_memory_mib, global.max_memory_mib),
            &global.preferred_path.map_or_else(
                || lookup("gui-java-auto-selected"),
                |path| path.display().to_string(),
            ),
        ),
        allow_pack_mod_version_changes: record.allow_pack_mod_version_changes,
    })
}

fn cancel_operation(cancellations: &CancellationRegistry, events: &Sender<Event>, id: &str) {
    let token = cancellations
        .lock()
        .ok()
        .and_then(|entries| entries.get(id).cloned());
    if let Some(token) = token {
        token.cancel();
        let _ = events.send(Event::Toast {
            message: lookup("gui-cancellation-requested"),
            error: false,
        });
    } else {
        let _ = events.send(Event::Toast {
            message: lookup("gui-operation-not-cancellable"),
            error: true,
        });
    }
}

/// Stops the game this launcher started for one instance.
///
/// Canceling the launch is what stops it: the supervising task kills the child
/// and waits for its real exit, so the instance's own lock is released rather
/// than left claiming a process that is gone. An instance this launcher did not
/// start has no token here, and says so rather than pretending to have acted.
fn kill_instance(
    cancellations: &CancellationRegistry,
    events: &Sender<Event>,
    instance: InstanceId,
    launch: Option<&str>,
) {
    let key = launch_key(instance, launch);
    let token = cancellations
        .lock()
        .ok()
        .and_then(|entries| entries.get(&key).cloned());
    match token {
        Some(token) => {
            token.cancel();
            let _ = events.send(Event::Toast {
                message: lookup("gui-kill-instance-requested"),
                error: false,
            });
        }
        None => {
            let _ = events.send(Event::Toast {
                message: lookup("gui-kill-instance-unavailable"),
                error: true,
            });
        }
    }
}

/// One version change or update, as the thread that carries it out needs it.
///
/// `version` is the exact release to move to, or `None` for the newest that
/// fits this client - the same two choices [`Command::ChangeModVersion`] and
/// [`Command::UpdateMod`] name.
struct ModOperation {
    instance: InstanceId,
    path: PathBuf,
    version: Option<String>,
    servers: Vec<ServerId>,
    operation: &'static str,
}

impl ModOperation {
    fn request(&self) -> LinkedModRequest {
        match &self.version {
            Some(version) => LinkedModRequest::ChangeVersion {
                target: self.path.clone(),
                version: version.clone(),
            },
            None => LinkedModRequest::Update {
                target: self.path.clone(),
            },
        }
    }
}

/// Carries out one mod operation, offering the user-assisted acquisition path
/// when a release's provider does not permit this launcher to download it.
///
/// The second attempt is the same operation rather than a second install
/// path: once the folder watcher has adopted the copy the user fetched, the
/// ordinary mutation finds it in the shared cache and proceeds. Returns the
/// token that stops the wait, which the caller keeps so closing the dialog
/// (or starting another operation) ends it.
/// One restore that is waiting on a release the user has to fetch.
struct RestoreRequest {
    host: InstalledHost,
    path: PathBuf,
    /// The refusal the first attempt produced, which names the files to ask
    /// for.
    refusal: CoreError,
    refreshed: RefreshedHosts,
}

/// Asks the user for the jar a restore could not fetch, then puts it back.
///
/// Returns the token that stops the wait, exactly as
/// [`apply_mod_operation`] does, so leaving the dialog ends it.
fn restore_after_acquisition(
    paths: Paths,
    events: &Sender<Event>,
    previous: Option<CancellationToken>,
    request: RestoreRequest,
) -> CancellationToken {
    if let Some(previous) = previous {
        previous.cancel();
    }
    let cancel = CancellationToken::new();
    let watching = cancel.clone();
    spawn_detached(paths, events.clone(), move |app, events, runtime| {
        let RestoreRequest {
            host,
            path,
            refusal,
            refreshed,
        } = request;
        let refusal: Result<(), CoreError> = Err(refusal);
        runtime.block_on(async {
            if await_manual_acquisition(&app, &events, &host.id(), &refusal, &watching).await
                != Acquisition::Acquired
            {
                return;
            }
            let reporter = OperationReporter::silent("operation-restoring-mod");
            match app
                .restore_mod_file(host, &path, &reporter, &CancellationToken::new())
                .await
            {
                Ok(()) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-mod-restored"),
                        error: false,
                    });
                    send_host_mods(&app, host, &events, &refreshed, false);
                }
                Err(error) => send_error(&events, &error),
            }
        });
    });
    cancel
}

fn apply_mod_operation(
    paths: Paths,
    events: &Sender<Event>,
    previous: Option<CancellationToken>,
    operation: ModOperation,
) -> CancellationToken {
    if let Some(previous) = previous {
        previous.cancel();
    }
    let cancel = CancellationToken::new();
    let watching = cancel.clone();
    spawn_detached(paths, events.clone(), move |app, events, runtime| {
        let reporter = OperationReporter::silent(operation.operation);
        let instance = operation.instance;
        runtime.block_on(async {
            let result = app
                .apply_linked_mod_operation(
                    instance,
                    &operation.request(),
                    &operation.servers,
                    &reporter,
                    &watching,
                )
                .await;
            match await_manual_acquisition(&app, &events, &instance.to_string(), &result, &watching)
                .await
            {
                Acquisition::Acquired => {}
                Acquisition::NotNeeded => {
                    finish_linked(&app, &events, instance, result);
                    return;
                }
                Acquisition::Abandoned => return,
            }
            // Everything the user was asked for is now a verified copy in the
            // shared cache, so the requested change goes through the ordinary
            // path - including its own re-resolution, which is what catches a
            // release withdrawn in the meantime.
            let result = app
                .apply_linked_mod_operation(
                    instance,
                    &operation.request(),
                    &operation.servers,
                    &reporter,
                    &CancellationToken::new(),
                )
                .await;
            finish_linked(&app, &events, instance, result);
        });
    });
    cancel
}

/// Offers the user-assisted acquisition path for a failed operation, and
/// waits for the releases it named.
///
/// One implementation for every operation that needs bytes: installing a mod,
/// changing its version, and updating it all reach a
/// [`CoreError::ModAcquisitionRestricted`] the same way, and a second place
/// to answer it would be a second manual-download flow.
///
async fn await_manual_acquisition(
    app: &App,
    events: &Sender<Event>,
    host: &str,
    result: &Result<impl Sized, CoreError>,
    cancel: &CancellationToken,
) -> Acquisition {
    let Err(CoreError::ModAcquisitionRestricted { path, blocked }) = result else {
        return Acquisition::NotNeeded;
    };
    let _ = events.send(Event::ModAcquisitionRequired(ModAcquisitionData {
        instance: host.to_owned(),
        logical_path: path.display().to_string(),
        name: blocked
            .first()
            .and_then(|file| file.project_name.clone())
            .unwrap_or_default(),
        items: blocked
            .iter()
            .map(crate::presentation::BlockedFilePresentation::from_blocked)
            .collect(),
    }));

    let targets = blocked.iter().map(WatchTarget::from_blocked).collect();
    let mut watcher = app.download_watcher(targets);
    while !watcher.is_finished() {
        if cancel.is_cancelled() {
            return Acquisition::Abandoned;
        }
        if watcher.scan_once().await.is_none() {
            tokio::time::sleep(ephervex_core::RESCAN_INTERVAL).await;
        }
    }
    if cancel.is_cancelled() {
        return Acquisition::Abandoned;
    }
    // The dialog asked for exactly these files and has them, so it is done
    // whatever the retry goes on to do: a failure after this is an ordinary
    // failure, reported the ordinary way.
    let _ = events.send(Event::CloseModal);
    Acquisition::Acquired
}

/// What [`await_manual_acquisition`] did about one failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Acquisition {
    /// The failure was about something else, so it is still the caller's to
    /// report.
    NotNeeded,
    /// Every file is now a verified copy in the shared cache. Try again.
    Acquired,
    /// The user left the dialog. Nothing was written and nothing is owed: the
    /// refusal that started this is not worth showing as a failure on top.
    Abandoned,
}

fn spawn_detached(
    paths: Paths,
    events: Sender<Event>,
    work: impl FnOnce(App, Sender<Event>, tokio::runtime::Runtime) + Send + 'static,
) {
    spawn_detached_with_setup_failure(paths, events, |_| {}, work);
}

fn spawn_detached_with_setup_failure(
    paths: Paths,
    events: Sender<Event>,
    setup_failed: impl FnOnce(&Sender<Event>) + Send + 'static,
    work: impl FnOnce(App, Sender<Event>, tokio::runtime::Runtime) + Send + 'static,
) {
    thread::spawn(move || {
        let mut setup_failed = Some(setup_failed);
        let app = match App::open_at(paths) {
            Ok(app) => app,
            Err(error) => {
                send_error(&events, &error);
                if let Some(setup_failed) = setup_failed.take() {
                    setup_failed(&events);
                }
                return;
            }
        };
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = events.send(Event::Toast {
                    message: localized_value("gui-background-operation-failed", &error.to_string()),
                    error: true,
                });
                if let Some(setup_failed) = setup_failed.take() {
                    setup_failed(&events);
                }
                return;
            }
        };
        work(app, events, runtime);
    });
}

/// Resolves a provider-backed pack's pending files on a thread of its own, so
/// the network work behind [`App::resolve_pack`] never blocks the worker's
/// command loop.
///
/// `generation` is opaque here: this function does not decide whether the
/// result still matters, it only carries the identity back unchanged so the
/// receiver can.
fn spawn_import_resolution(
    paths: Paths,
    events: Sender<Event>,
    generation: u64,
    mut analysis: PackAnalysis,
    cancel: CancellationToken,
) {
    spawn_detached_with_setup_failure(
        paths,
        events,
        move |events| {
            let _ = events.send(Event::ImportResolved {
                generation,
                result: Err(ImportFailure {
                    message: lookup("gui-import-resolution-unavailable"),
                    kind: ImportErrorKind::default(),
                }),
            });
        },
        move |app, events, runtime| {
            // Nothing has been decided about a blocked file yet at preview
            // time; this call's only job is to populate `analysis.blocked`
            // (after a cache-probe auto-resolution) for the modal to show.
            let result = runtime.block_on(app.resolve_pack(&mut analysis, &[], &cancel));
            let outcome = match result {
                Ok(()) => Ok(ImportPresentation::from_analysis(&analysis)),
                Err(error) => Err(import_failure(&error)),
            };
            let _ = events.send(Event::ImportResolved {
                generation,
                result: outcome,
            });

            // A blocked file may still turn up in a watched folder while the
            // modal stays open on it. This keeps rescanning until every
            // target is matched or `cancel` fires - the same cancellation a
            // new archive pick or closing the modal already triggers, so
            // nothing further needs to stop this loop on its own.
            if !analysis.blocked.is_empty() {
                let targets = analysis
                    .blocked
                    .iter()
                    .map(WatchTarget::from_blocked)
                    .collect();
                let mut watcher = app.download_watcher(targets);
                while !watcher.is_finished() && !cancel.is_cancelled() {
                    match runtime.block_on(watcher.scan_once()) {
                        Some(identity) => {
                            let _ = events.send(Event::BlockedFileMatched {
                                generation,
                                identity,
                            });
                        }
                        None => std::thread::sleep(ephervex_core::RESCAN_INTERVAL),
                    }
                }
            }
        },
    );
}

/// One `ImportPack` command's worth of work, bundled so the function that
/// runs it stays under the shared argument-count limit.
struct ImportSubmission {
    /// The attempt this submission belongs to, echoed back on every terminal
    /// event so the controller can tell its own submission apart from any
    /// other operation's.
    generation: u64,
    archive: PathBuf,
    name: String,
    persistent: bool,
    selection: OptionalSelection,
    skip_blocked: Vec<ProviderFileIdentity>,
    request_id: Option<String>,
    create_linked_server: bool,
    server_pack: Option<PathBuf>,
}

/// Runs an `ImportPack` submission on a thread of its own and, on every
/// terminal path, including a setup failure that never reaches
/// [`App::import_pack`], reports back which attempt finished.
///
/// This does not reuse [`spawn_async_operation`]: that helper is shared by
/// operations with no submission identity of their own, and folding one in
/// there would make every other caller carry a concept only import needs.
fn spawn_import_submission(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    submission: ImportSubmission,
) {
    let ImportSubmission {
        generation,
        archive,
        name,
        persistent,
        selection,
        skip_blocked,
        request_id,
        create_linked_server,
        server_pack,
    } = submission;
    // Cloned for the setup-failure path, which runs instead of the body rather
    // than after it.
    let setup_request_id = request_id.clone();
    let entity = archive.display().to_string();
    spawn_detached_with_setup_failure(
        paths.clone(),
        events.clone(),
        move |events| {
            let _ = events.send(Event::ImportSubmissionFinished {
                generation,
                succeeded: false,
                request_id: setup_request_id,
            });
        },
        move |app, events, runtime| {
            let sink = Arc::new(UiProgressSink {
                events: events.clone(),
                entity,
                allow_cancel: true,
                pending: request_id.clone(),
            });
            let reporter = OperationReporter::new("operation-importing-pack", true, sink);
            reporter.begin(None);
            let cancel = CancellationToken::new();
            register_cancellation(&cancellations, &reporter, &cancel);
            let mut request = ImportRequest::new(archive)
                .with_storage(storage_class(persistent))
                .selecting(selection)
                .skipping_blocked(skip_blocked);
            if !name.trim().is_empty() {
                request = request.named(name);
            }
            let result = runtime.block_on(app.import_pack(&request, &reporter, &cancel));
            remove_cancellation(&cancellations, &reporter);
            // Closing the modal is not decided here: a receiver only closes
            // its own still-current attempt, which this thread cannot know.
            // `Event::ImportSubmissionFinished` carries `succeeded` so the
            // receiver can decide on its own state.
            let succeeded = match result {
                Ok(pack) => {
                    // Part of the same operation, so the card the user is
                    // already watching reports it too. A server that fails
                    // leaves the imported instance alone and says so on its
                    // own: the import the user asked for did happen.
                    if create_linked_server {
                        let request = NewServer::linked(
                            format!("{} Server", pack.record.name),
                            pack.record.id,
                        )
                        .with_storage(pack.record.storage);
                        let options = ServerCreateOptions {
                            install_java_if_missing: true,
                            content: server_pack.map_or(ServerContentSource::Recipe, |path| {
                                ServerContentSource::Archive { path }
                            }),
                            reporter: Some(reporter.clone()),
                            ..ServerCreateOptions::default()
                        };
                        if let Err(error) =
                            runtime.block_on(app.create_server(request, &options, &cancel))
                        {
                            send_error(&events, &error);
                        }
                    }
                    reporter.finish(OperationStatus::Completed);
                    let _ = events.send(Event::ClientCreated {
                        instance: pack.record.id.to_string(),
                    });
                    let _ = events.send(Event::Toast {
                        message: localized_value("gui-imported-pack", &pack.record.name),
                        error: false,
                    });
                    true
                }
                Err(error) => {
                    if error.is_cancellation() {
                        reporter.finish(OperationStatus::Canceled);
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-operation-canceled"),
                            error: false,
                        });
                    } else {
                        reporter.finish(OperationStatus::Failed(localized_error(&error)));
                        send_error(&events, &error);
                    }
                    false
                }
            };
            let _ = events.send(Event::ImportSubmissionFinished {
                generation,
                succeeded,
                request_id,
            });
            if let Ok(app) = App::open_at(paths) {
                send_snapshot(&app, &events);
            }
        },
    );
}

#[derive(Debug)]
struct UiProgressSink {
    events: Sender<Event>,
    entity: String,
    allow_cancel: bool,
    /// A pending card standing in for this work in a list, which is shown
    /// where the user is looking rather than on the activity page they would
    /// have to go and find.
    pending: Option<String>,
}

impl ProgressSink for UiProgressSink {
    fn operation_changed(&self, operation: &Operation) {
        send_operation(
            &self.events,
            operation,
            self.entity.clone(),
            self.allow_cancel,
        );
        if let Some(request_id) = self.pending.clone() {
            let item = ActivityPresentation::from(operation);
            let _ = self.events.send(Event::CreationProgress {
                request_id,
                detail: item.detail,
                progress: item.progress,
                indeterminate: item.indeterminate,
            });
        }
    }
}

fn send_operation(
    events: &Sender<Event>,
    operation: &Operation,
    entity: String,
    allow_cancel: bool,
) {
    let title = lookup(&operation.title);
    let mut item = ActivityPresentation::from(operation);
    item.cancellable &= allow_cancel;
    if title != operation.title {
        item.title = title;
    }
    let _ = events.send(Event::Activity(ActivityData {
        item,
        entity,
        time: Utc::now().format("%H:%M:%S").to_string(),
    }));
}

#[allow(clippy::too_many_arguments)]
fn send_activity_state(
    events: &Sender<Event>,
    id: String,
    title: &str,
    entity: &str,
    detail: &str,
    status_kind: &'static str,
    active: bool,
    failed: bool,
    cancellable: bool,
) {
    let _ = events.send(Event::Activity(ActivityData {
        item: ActivityPresentation {
            id,
            title: title.to_owned(),
            status: lookup(match status_kind {
                "queued" => "gui-state-queued",
                "running" => "gui-state-running",
                "canceled" => "gui-state-canceled",
                "completed" => "gui-state-completed",
                _ => "gui-state-failed",
            }),
            status_kind,
            detail: detail.to_owned(),
            progress: 0.0,
            indeterminate: active,
            cancellable,
            failed,
        },
        entity: entity.to_owned(),
        time: Utc::now().format("%H:%M:%S").to_string(),
    }));
}

fn report_async_control<T>(
    runtime: &tokio::runtime::Runtime,
    events: &Sender<Event>,
    future: impl std::future::Future<Output = Result<T, CoreError>>,
    success_message_id: &str,
) {
    report_sync(
        events,
        runtime.block_on(future).map(|_| lookup(success_message_id)),
    );
}

fn report_sync(events: &Sender<Event>, result: Result<String, CoreError>) {
    match result {
        Ok(message) => {
            let _ = events.send(Event::Toast {
                message,
                error: false,
            });
        }
        Err(error) => send_error(events, &error),
    }
}

fn loader_selection(loader: &str, version: &str) -> Result<LoaderSelection, CoreError> {
    match loader.trim().to_ascii_lowercase().as_str() {
        "vanilla" => Ok(LoaderSelection::vanilla()),
        "forge" if !version.trim().is_empty() => {
            Ok(LoaderSelection::exact(LoaderKind::Forge, version.trim()))
        }
        "neoforge" if !version.trim().is_empty() => {
            Ok(LoaderSelection::exact(LoaderKind::NeoForge, version.trim()))
        }
        unsupported => Err(ephervex_core::PackError::LoaderUnsupported {
            loader: unsupported.to_owned(),
        }
        .into()),
    }
}

fn localized_error(error: &CoreError) -> String {
    // `message_args` deliberately excludes the values these accessors expose
    // (see their doc comments), so a message or an action that reads one of
    // their placeholders renders it literally unless every one of them is
    // folded in here too. Every available argument is supplied rather than
    // only the ones a given message happens to use - Fluent ignores the
    // extras, and the alternative is a branch per combination that silently
    // drops an argument the moment a message gains a second placeholder.
    let mut raw_args = error.message_args();
    if let Some(path) = error.failing_path() {
        raw_args.push(("path", path.display().to_string()));
    }
    if let Some(url) = error.help_url() {
        raw_args.push(("url", url.to_owned()));
    }
    if let Some(subject) = error.help_subject() {
        raw_args.push(("client_id", subject.to_owned()));
    }
    if let Some(syntax) = error.expected_syntax() {
        raw_args.push(("syntax", syntax.to_owned()));
    }
    if let Some(loader) = error.unsupported_loader() {
        raw_args.push(("loader", loader.to_owned()));
    }
    let args = raw_args
        .into_iter()
        .map(|(name, value)| (name, Arg::from(value)))
        .collect::<Vec<_>>();
    let mut message = if args.is_empty() {
        lookup(error.message_id())
    } else {
        lookup_args(error.message_id(), args.clone())
    };
    // Filtered rather than shown: an action that still has an unresolved
    // placeholder after every known argument was supplied means this message
    // needs one this function does not yet know about, and a raw `{$…}` in
    // the user's face is worse than no action line at all.
    let action = if args.is_empty() {
        lookup_attribute(error.message_id(), "action")
    } else {
        lookup_attribute_args(error.message_id(), "action", args)
    }
    .filter(|action| !action.contains("{$") && !action.contains("{ $"));
    if let Some(action) = action {
        message.push('\n');
        message.push_str(&action);
    }
    message
}

/// The import modal's own error type: the same localized text
/// [`localized_error`] produces, plus the coarse category
/// [`ImportErrorKind::from_message_id`] reads off the error's stable id.
/// Classifying here, where the typed [`CoreError`] is still in hand, is what
/// keeps the modal from ever having to pattern-match localized prose.
fn import_failure(error: &CoreError) -> ImportFailure {
    ImportFailure {
        message: localized_error(error),
        kind: ImportErrorKind::from_message_id(error.message_id()),
    }
}

fn localized_value(message_id: &str, value: &str) -> String {
    lookup_args(message_id, [("value", Arg::from(value))])
}

/// Loads one mod's releases, or the reason there are none to show.
///
/// A catalog that cannot be reached is reported in the view rather than as a
/// failed operation: the mod list behind it is still correct, and the user
/// asked to look at versions, not to change anything.
/// Answers the relation question for one mod.
///
/// Silent on failure: the mod page it fills is opened for other reasons too,
/// and an instance whose recipe cannot be read has already said so through the
/// list the page was opened from.
/// Asks the catalog about the projects one mod declares and this host does not
/// have, and repaints its relation lists with the answer.
///
/// Silent on failure for the same reason `send_mod_relations` is: the lists are
/// already on screen with the offline answer, and a catalog that could not be
/// reached leaves every unmatched row saying it was not looked up, which is
/// true.
fn resolve_declared_relations(
    app: &App,
    runtime: &tokio::runtime::Runtime,
    host: InstalledHost,
    path: &Path,
    events: &Sender<Event>,
) {
    let reporter = OperationReporter::silent("resolving-declared-relations");
    match runtime.block_on(app.declared_projects_of(
        host,
        path,
        &reporter,
        &CancellationToken::new(),
    )) {
        Ok(declared) if !declared.is_empty() => {
            send_mod_relations(app, host, path, &declared, events);
        }
        // Nothing the offline answer did not already say, so the lists are
        // left alone rather than repainted with the same rows.
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "declared projects could not be resolved"),
    }
}

fn send_mod_relations(
    app: &App,
    host: InstalledHost,
    path: &Path,
    declared: &BTreeMap<String, DeclaredProject>,
    events: &Sender<Event>,
) {
    match app.relations_of(host, path) {
        Ok(relations) => {
            let _ = events.send(Event::ModRelations(ModRelationsData {
                host: host.id(),
                logical_path: path.display().to_string(),
                view: crate::presentation::ModRelationPresentation::new(&relations, declared),
            }));
        }
        Err(error) => tracing::warn!(%error, "a mod's relations could not be read"),
    }
}

async fn mod_versions(app: &App, instance: InstanceId, path: &Path) -> ModVersionsData {
    let reporter = OperationReporter::silent("listing-mod-versions");
    let cancel = CancellationToken::new();
    match app
        .managed_mod_versions(instance, path, &reporter, &cancel)
        .await
    {
        Ok(choices) => {
            let installed = choices.installed_version_id.clone().unwrap_or_default();
            let newest = choices.newest_compatible.clone().unwrap_or_default();
            let up_to_date = choices.is_current();
            let versions: Vec<ModVersionData> = choices
                .compatible
                .iter()
                .map(|annotated| ModVersionData {
                    version_id: annotated.version.id.clone(),
                    name: annotated.version.name.clone(),
                    number: annotated.version.version_number.clone(),
                    channel: annotated.version.channel.as_str().to_owned(),
                    published: annotated
                        .version
                        .published_at
                        .format("%Y-%m-%d")
                        .to_string(),
                    installed: annotated.version.id == installed,
                    newest: annotated.version.id == newest,
                    restricted: annotated.version.file.distribution.is_restricted(),
                })
                .collect();
            ModVersionsData {
                instance: instance.to_string(),
                logical_path: path.display().to_string(),
                title: choices.project.title.clone(),
                installed_version: installed,
                notice: restriction_text(choices.mutability.restriction),
                can_change_version: choices.mutability.can_change_version,
                can_update: choices.mutability.can_update && !up_to_date,
                can_remove: choices.mutability.can_remove,
                up_to_date,
                project_url: choices.project_url().unwrap_or_default(),
                is_pack_owned: choices.origin.is_pack_owned(),
                pack_original_version: choices.original_version_id.clone().unwrap_or_default(),
                versions,
            }
        }
        Err(error) => {
            tracing::warn!(%error, "mod versions could not be listed");
            ModVersionsData {
                instance: instance.to_string(),
                logical_path: path.display().to_string(),
                notice: localized_error(&error),
                ..ModVersionsData::default()
            }
        }
    }
}

/// What to tell the user one finished mutation did.
fn mutation_message(mutation: &ModMutation) -> String {
    match &mutation.plan.outcome {
        PlannedOutcome::AlreadyCurrent => {
            localized_value("gui-mod-already-current", &mutation.plan.name)
        }
        PlannedOutcome::Repair => localized_value("gui-mod-repaired", &mutation.plan.name),
        PlannedOutcome::ChangeVersion { .. } => {
            localized_value("gui-mod-version-changed", &mutation.plan.name)
        }
        PlannedOutcome::Remove => {
            localized_count("gui-mod-removed", mutation.plan.removes.len().max(1))
        }
    }
}

fn removal_plan_data(linked: &LinkedModPlan) -> ModRemovalPlanData {
    let Some(plan) = mutation_plan_of(linked) else {
        return ModRemovalPlanData::default();
    };
    ModRemovalPlanData {
        instance: plan.instance.to_string(),
        logical_path: plan.target.display().to_string(),
        name: plan.name.clone(),
        dependencies: plan
            .collected_dependencies()
            .map(|file| file.name.clone())
            .collect(),
        dependencies_unprovable: plan.orphans_unprovable,
        linked: linked_section(linked),
    }
}

/// What installing a mod would involve, on this client and on its servers.
fn install_plan_data(linked: &LinkedModPlan, requested: &str) -> ModPlanData {
    let Some(plan) = install_plan_of(linked) else {
        return ModPlanData::default();
    };
    ModPlanData {
        instance: linked.client.to_string(),
        provider: linked.root.provider.as_str().to_owned(),
        requested: requested.to_owned(),
        project: linked.root.project_id.clone(),
        title: plan.root.project.title.clone(),
        version: plan.root.version.version_number.clone(),
        version_id: linked.root.version_id.clone().unwrap_or_default(),
        install_count: plan.to_install().len(),
        dependency_count: plan.dependencies.len(),
        optional_count: plan.optional.len(),
        already_installed_count: plan.already_installed.len(),
        warning_count: plan.warnings.len(),
        linked: linked_section(linked),
    }
}

fn mutation_plan_of(plan: &LinkedModPlan) -> Option<&ModMutationPlan> {
    match &plan.client_plan {
        ClientModPlan::Mutate(plan) => Some(plan),
        ClientModPlan::Install(_) => None,
    }
}

fn install_plan_of(plan: &LinkedModPlan) -> Option<&ModInstallPlan> {
    match &plan.client_plan {
        ClientModPlan::Install(plan) => Some(plan),
        ClientModPlan::Mutate(_) => None,
    }
}

/// The request a version picker's selection names.
///
/// An empty version is "the newest release that fits this client", which is a
/// different operation from an exact choice and not a missing value.
fn version_change_request(path: &Path, version: &str) -> LinkedModRequest {
    if version.is_empty() {
        LinkedModRequest::Update {
            target: path.to_path_buf(),
        }
    } else {
        LinkedModRequest::ChangeVersion {
            target: path.to_path_buf(),
            version: version.to_owned(),
        }
    }
}

/// The same operation, pinned to the release its plan resolved.
///
/// An update asks for "the newest release that fits this client", and applying
/// that request would ask again. The two lookups can disagree, so the release
/// the plan settled on is the one that gets applied — the same property the
/// confirmation gives every other path. A request the plan named no release for
/// is unchanged.
fn planned_request(plan: &LinkedModPlan) -> LinkedModRequest {
    match (plan.request.target(), plan.root.version_id.clone()) {
        (Some(target), Some(version)) => LinkedModRequest::ChangeVersion {
            target: target.to_path_buf(),
            version,
        },
        _ => plan.request.clone(),
    }
}

/// The linked-server half of a confirmation, and what applying it needs.
fn linked_section(plan: &LinkedModPlan) -> LinkedSectionData {
    // A mod no server could ever run is not a decision, and neither is a client
    // with no linked servers. Dropping the rows here rather than in each modal
    // keeps "is this worth asking about" one answer.
    let relevant = plan.has_relevant_targets();
    LinkedSectionData {
        instance: plan.client.to_string(),
        kind: plan.request.as_str().to_owned(),
        path: plan
            .request
            .target()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        version: plan.root.version_id.clone().unwrap_or_default(),
        name: plan.root.name.clone(),
        version_number: plan.root.version_number.clone().unwrap_or_default(),
        support: if relevant {
            support_note(plan)
        } else {
            String::new()
        },
        targets: if relevant {
            plan.targets.iter().map(linked_target_row).collect()
        } else {
            Vec::new()
        },
    }
}

/// What the project declares about the server side, said out loud.
///
/// A removal claims nothing: it is decided by what a server durably owns, not
/// by what the project says.
fn support_note(plan: &LinkedModPlan) -> String {
    if plan.request.is_removal() {
        return String::new();
    }
    lookup(match plan.root.server_side {
        SideRequirement::Required => "gui-linked-support-required",
        SideRequirement::Optional => "gui-linked-support-optional",
        SideRequirement::Unsupported => "gui-linked-support-unsupported",
        SideRequirement::Unknown => "gui-linked-support-unknown",
    })
}

fn linked_target_row(target: &LinkedTargetPlan) -> LinkedTargetData {
    LinkedTargetData {
        id: target.server.to_string(),
        name: target.name.clone(),
        // Technical values stay exact and untranslated.
        detail: format!("{} | {}", target.minecraft_version, target.loader),
        state: lookup(match target.state {
            LinkedTargetState::Blocked(_) => "gui-linked-unavailable",
            ref state => state.message_id(),
        }),
        explanation: target_explanation(target),
        tone: if target.state.is_actionable() {
            "ok".to_owned()
        } else if target.state.block().is_some() {
            "warn".to_owned()
        } else {
            "muted".to_owned()
        },
        actionable: target.state.is_actionable(),
        blocked: target.state.block().is_some(),
        selected: target.selected_by_default,
    }
}

/// Why a target cannot take part, or what taking part would involve.
fn target_explanation(target: &LinkedTargetPlan) -> String {
    if let Some(block) = target.state.block() {
        return lookup(block.message_id());
    }
    if !target.state.is_actionable() {
        return String::new();
    }
    let mut parts = Vec::new();
    if !target.installs.is_empty() {
        parts.push(localized_count(
            "gui-linked-installs",
            target.installs.len(),
        ));
    }
    if !target.removes.is_empty() {
        parts.push(localized_count("gui-linked-removes", target.removes.len()));
    }
    if target.orphans_unprovable {
        parts.push(lookup("gui-linked-dependencies-unprovable"));
    }
    parts.join(", ")
}

/// Reports one finished linked operation.
///
/// A single target keeps the ordinary toast. Several get a list, because the
/// truth about them is per target and a summary line would have to pick one.
/// Replaces the Managed Mods panel with the same instance's enriched view.
///
/// The durable record is authoritative about *which* mods an instance has, and
/// it is what a workspace load and every mutation return. It carries no
/// provider title, version, or artwork, so a panel left holding it shows jar
/// file names. Enrichment is a provider round trip, so it runs off the command
/// thread and only replaces the list once it has an answer.
/// Whether this host's mods should be asked of a provider now.
///
/// `force` is an explicit Refresh, which also clears the mark so the answer it
/// fetches is the one the rest of the run reuses.
fn claim_refresh(refreshed: &RefreshedHosts, host: InstalledHost, force: bool) -> bool {
    let Ok(mut refreshed) = refreshed.lock() else {
        // A poisoned lock is not a reason to stop showing mod names.
        return true;
    };
    let key = format!("{}:{}", host.kind(), host.id());
    if force {
        refreshed.insert(key);
        return true;
    }
    refreshed.insert(key)
}

fn refresh_enriched_mods(paths: Paths, events: &Sender<Event>, instance: InstanceId) {
    refresh_enriched_host_mods(paths, events, InstalledHost::Instance(instance));
}

fn refresh_enriched_host_mods(paths: Paths, events: &Sender<Event>, host: InstalledHost) {
    spawn_detached(paths, events.clone(), move |app, events, runtime| {
        let enriched = runtime.block_on(app.enriched_host_mods(
            host,
            &OperationReporter::silent("managed-mods"),
            &CancellationToken::new(),
        ));
        match enriched {
            Ok(snapshot) => {
                let review = app.crash_review(host).ok().flatten();
                let _ = events.send(Event::ManagedMods(managed_mods_data(
                    &snapshot,
                    review.as_ref(),
                )));
            }
            // Enrichment is decoration. The list the user is already looking at
            // is correct without it, so this is logged rather than shown as a
            // failed operation.
            Err(error) => {
                tracing::warn!(%error, "managed mod metadata could not be loaded");
            }
        }
    });
}

fn finish_linked(
    app: &App,
    events: &Sender<Event>,
    instance: InstanceId,
    result: CoreResult<LinkedModOutcome>,
) {
    match result {
        Ok(outcome) => {
            let _ = events.send(Event::CloseModal);
            if outcome.targets.is_empty() {
                let _ = events.send(Event::Toast {
                    message: client_outcome_message(&outcome),
                    error: false,
                });
            } else {
                let _ = events.send(Event::LinkedResult(linked_result_data(&outcome)));
            }
            send_isolation_basis(app, InstalledHost::Instance(instance), events);
            for target in &outcome.targets {
                send_isolation_basis(app, InstalledHost::Server(target.server), events);
            }
            if let ClientModOutcome::Mutated(mutation) = &outcome.client {
                let review = app
                    .crash_review(InstalledHost::Instance(instance))
                    .ok()
                    .flatten();
                let _ = events.send(Event::ManagedMods(managed_mods_data(
                    &mutation.snapshot,
                    review.as_ref(),
                )));
                refresh_enriched_mods(app.paths().clone(), events, instance);
            }
            send_snapshot(app, events);
        }
        Err(error) => send_modal_error(events, &error),
    }
}

/// What the client half of a linked operation did.
fn client_outcome_message(outcome: &LinkedModOutcome) -> String {
    match &outcome.client {
        ClientModOutcome::Installed(installed) => localized_count(
            "gui-installed-managed-mods",
            installed.plan.all_mods().len(),
        ),
        ClientModOutcome::Mutated(mutation) => mutation_message(mutation),
    }
}

/// Every target's own result, so a partial success is visibly partial.
fn linked_result_data(outcome: &LinkedModOutcome) -> LinkedResultData {
    let mut rows = vec![LinkedTargetData {
        name: lookup("gui-linked-result-client"),
        state: lookup("gui-linked-applied"),
        explanation: client_outcome_message(outcome),
        tone: "ok".to_owned(),
        ..LinkedTargetData::default()
    }];
    rows.extend(outcome.targets.iter().map(linked_result_row));

    LinkedResultData {
        title: outcome.root.name.clone(),
        summary: lookup(if outcome.unfinished().count() == 0 {
            "gui-linked-all-applied"
        } else {
            "gui-linked-partial"
        }),
        rows,
    }
}

fn linked_result_row(target: &ephervex_core::LinkedTargetOutcome) -> LinkedTargetData {
    let (state, explanation, tone) = match &target.result {
        LinkedTargetResult::Applied(report) => (
            "gui-linked-applied",
            applied_summary(report),
            "ok".to_owned(),
        ),
        LinkedTargetResult::Skipped(state) => (
            "gui-linked-skipped",
            lookup(state.message_id()),
            "muted".to_owned(),
        ),
        LinkedTargetResult::Failed(failure) => (
            "gui-linked-failed",
            lookup(failure.message_id),
            "error".to_owned(),
        ),
    };
    LinkedTargetData {
        id: target.server.to_string(),
        name: target.name.clone(),
        state: lookup(state),
        explanation,
        tone,
        ..LinkedTargetData::default()
    }
}

/// What one target's transaction actually wrote.
fn applied_summary(report: &ManagedMutationReport) -> String {
    let written = report.installed.len() + report.replaced.len() + report.repaired.len();
    let mut parts = Vec::new();
    if written > 0 {
        parts.push(localized_count("gui-linked-installs", written));
    }
    if !report.removed.is_empty() {
        parts.push(localized_count("gui-linked-removes", report.removed.len()));
    }
    parts.join(", ")
}

/// A refusal as something to show, or nothing when there is none.
fn restriction_text(restriction: Option<MutationRestriction>) -> String {
    restriction.map_or_else(String::new, |restriction| lookup(restriction.message_id()))
}

/// Turns a local-mod plan into what the clash question is asked from.
fn local_mod_plan_data(host: InstalledHost, plan: &LocalModPlan) -> LocalModPlanData {
    let mut ready = Vec::new();
    let mut clashing = Vec::new();
    let mut refused = Vec::new();
    for offer in &plan.offers {
        match offer {
            LocalModOffer::Ready { source, .. } => ready.push(source.clone()),
            LocalModOffer::Clashing {
                source,
                file_name,
                existing_is_disabled,
            } => clashing.push(LocalModClashData {
                source: source.clone(),
                file_name: file_name.clone(),
                existing_is_disabled: *existing_is_disabled,
            }),
            LocalModOffer::Refused { source, reason } => refused.push(LocalModRefusalData {
                // The file's own name, not the whole path: the user just
                // dragged it and knows where it came from.
                file_name: source
                    .file_name()
                    .unwrap_or(source.as_os_str())
                    .to_string_lossy()
                    .into_owned(),
                reason: lookup(reason.message_id()),
            }),
        }
    }
    LocalModPlanData {
        host: host.id(),
        host_kind: host.kind().to_owned(),
        ready,
        clashing,
        refused,
    }
}

/// What a finished drop has to say for itself.
///
/// Everything that did not happen is counted, because a drop of six jars that
/// added four is not a success the user should have to notice for themselves.
fn local_mods_added_message(report: &LocalModReport, skipped: usize) -> String {
    let mut parts = vec![counted("gui-mods-added", report.written())];
    if skipped > 0 {
        parts.push(counted("gui-mods-skipped", skipped));
    }
    if !report.refused.is_empty() {
        // Only a file that passed the plan and failed the copy reaches here;
        // everything refused up front was already reported by name.
        parts.push(counted("gui-mods-refused", report.refused.len()));
    }
    parts.join(" ")
}

/// One pluralized message about a count of files.
fn counted(message_id: &str, count: usize) -> String {
    lookup_args(
        message_id,
        [("count", Arg::from(i64::try_from(count).unwrap_or(i64::MAX)))],
    )
}

/// Turns the application service's mod view into rows.
///
/// The one place a managed mod becomes display text. Everything it reads is
/// already decided by `ephervex-core`; nothing here inspects a recipe, parses a
/// file name, or decides what a mod is.
fn managed_mods_data(
    snapshot: &ManagedModSnapshot,
    review: Option<&CrashReview>,
) -> ManagedModsData {
    ManagedModsData {
        host: snapshot.host.id(),
        host_kind: snapshot.host.kind().to_owned(),
        mods: snapshot
            .mods
            .iter()
            // A recorded mod whose jar is not in `mods/` is left out, because a
            // row for a file that is not there names nothing the reader can act
            // on. The exception is one this list offered to delete: it can put
            // that back, so the row stays and says so, and the bin is not a
            // one-way door (ADR 0019).
            .filter(|entry| {
                entry.presence != ModPresence::Missing || ModMutability::of(entry).can_delete_file
            })
            .map(|entry| managed_mod_data(entry, snapshot.mods_are_the_users, review))
            .collect(),
        notice: match snapshot.metadata {
            MetadataAvailability::Unavailable => lookup("gui-mod-metadata-offline"),
            MetadataAvailability::Local | MetadataAvailability::Provider => String::new(),
        },
    }
}

fn crash_notice_data(host: InstalledHost, review: Option<&CrashReview>) -> CrashNoticeData {
    CrashNoticeData {
        host: host.id(),
        host_kind: host.kind().to_owned(),
        report: review.map(|it| it.report_name.clone()).unwrap_or_default(),
        failure: review
            .and_then(|it| it.analysis.failure.clone())
            .unwrap_or_default(),
        client_code_on_server: review.is_some_and(|it| it.analysis.client_code_on_server),
        suggested: review.map_or(0, |it| it.suggested().count()),
    }
}

/// Why a crash report points at one mod, in the words a row shows.
fn suspect_reason_text(reason: &SuspectReason) -> String {
    match reason {
        SuspectReason::Named => lookup("crash-reason-named"),
        SuspectReason::InFailingFrames => lookup("crash-reason-in-failing-frames"),
        SuspectReason::CompatTrigger { triggered_by } => lookup_args(
            "crash-reason-compat-trigger",
            [("mod", Arg::from(triggered_by.as_str()))],
        ),
    }
}

fn managed_mod_data(
    entry: &ManagedModEntry,
    mods_are_the_users: bool,
    review: Option<&CrashReview>,
) -> ManagedModData {
    let title = entry.display_name().to_owned();
    // The same reading the mutation itself will make, so a row never offers an
    // action the operation behind it would refuse.
    let mutability = ephervex_core::effective_mutability(entry, mods_are_the_users);
    let suspect = review.and_then(|review| {
        review
            .mods
            .iter()
            .find(|found| found.logical_path.as_ref() == Some(&entry.logical_path))
    });
    ManagedModData {
        project_id: entry.identity.project_id().unwrap_or_default().to_owned(),
        logical_path: entry.logical_path.display().to_string(),
        can_manage: mutability.can_list_versions,
        can_change_version: mutability.can_change_version,
        can_update: mutability.can_update,
        can_remove: mutability.can_remove,
        can_delete_file: mutability.can_delete_file,
        restriction: restriction_text(mutability.restriction),
        suspect_reason: suspect
            .map(|found| suspect_reason_text(&found.suspect.reason))
            .unwrap_or_default(),
        suspect_confidence: suspect
            .map(|found| found.suspect.confidence().as_str().to_owned())
            .unwrap_or_default(),
        // The file name is the secondary line only when it adds something. For
        // an unidentified mod the title already is the file name, and printing
        // it twice would read as a bug.
        file_name: if title == entry.file_name {
            String::new()
        } else {
            entry.file_name.clone()
        },
        title,
        version: entry
            .installed_version()
            .map_or_else(|| lookup("gui-mod-version-unknown"), ToOwned::to_owned),
        version_known: entry.installed_version().is_some(),
        provider: entry
            .identity
            .provider()
            .map_or_else(|| lookup("gui-mod-provider-unknown"), |it| it.to_string()),
        provider_kind: entry
            .identity
            .provider()
            .map(|it| it.as_str().to_owned())
            .unwrap_or_default(),
        origin: lookup(entry.origin.message_id()),
        origin_kind: origin_kind(entry.origin).to_owned(),
        compatibility: compatibility_summary(entry.game_versions(), entry.loaders()),
        author: entry.author().unwrap_or_default().to_owned(),
        icon: entry
            .icon
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        enabled: entry.presence.is_enabled(),
        // Deleted, and the recipe still declares it, so the row stays and
        // offers to put it back rather than vanishing.
        deleted: entry.presence == ModPresence::Missing,
        pack_owned: entry.is_pack_owned(),
    }
}

/// The row accent an origin gets. Three groups rather than four: an optional
/// dependency and a required one are both "something else asked for this".
fn origin_kind(origin: ModOrigin) -> &'static str {
    if origin == ModOrigin::HandPlaced {
        // Its own accent: this is the one row the launcher does not own, and
        // reading as "user" would put it beside mods the user chose in the
        // browser, which Ephervex installed and can act on.
        "hand-placed"
    } else if origin.is_pack_owned() {
        "pack"
    } else if origin.is_dependency() {
        "dependency"
    } else {
        "user"
    }
}

/// What the installed release declares it runs on, as one line.
///
/// Empty when the provider reported neither, which is what an entry whose exact
/// version could not be resolved looks like.
fn compatibility_summary(game_versions: &[String], loaders: &[String]) -> String {
    let versions = game_versions.join(", ");
    let loaders = loaders.join(", ");
    match (versions.is_empty(), loaders.is_empty()) {
        (true, true) => String::new(),
        (false, true) => versions,
        (true, false) => loaders,
        (false, false) => localized_values("gui-target-summary", &versions, &loaders),
    }
}

fn localized_values(message_id: &str, first: &str, second: &str) -> String {
    lookup_args(
        message_id,
        [("first", Arg::from(first)), ("second", Arg::from(second))],
    )
}

fn localized_count(message_id: &str, count: usize) -> String {
    lookup_args(message_id, [("count", Arg::from(count))])
}

/// What a finished write of the preferred catalog tells the user.
///
/// A refusal produces the failure and nothing else: no "saved" toast, and no
/// caller-visible signal that the view should start showing the new value.
fn default_mod_catalog_events(stored: &Result<(), CoreError>) -> Vec<Event> {
    match stored {
        Ok(()) => vec![Event::Toast {
            message: lookup("gui-preferred-mod-catalog-saved"),
            error: false,
        }],
        Err(error) => vec![error_toast(error)],
    }
}

fn send_error(events: &Sender<Event>, error: &CoreError) {
    let _ = events.send(error_toast(error));
}

/// A failure as the user sees it, logged with the technical detail the toast
/// deliberately leaves out.
fn error_toast(error: &CoreError) -> Event {
    tracing::warn!(message_id = error.message_id(), detail = %ErrorChain(error), "GUI operation failed");
    Event::Toast {
        message: localized_error(error),
        error: true,
    }
}

fn send_modal_error(events: &Sender<Event>, error: &CoreError) {
    tracing::warn!(message_id = error.message_id(), detail = %ErrorChain(error), "GUI form submission failed");
    let _ = events.send(Event::ModalError(localized_error(error)));
}

fn storage_class(persistent: bool) -> StorageClass {
    if persistent {
        StorageClass::Persistent
    } else {
        StorageClass::Temporary
    }
}

fn nonempty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn property_bool(document: &ephervex_core::PropertiesDocument, key: &str, fallback: bool) -> bool {
    document
        .get(key)
        .map_or(fallback, |value| value.eq_ignore_ascii_case("true"))
}

fn eula_data(status: &EulaStatus) -> EulaData {
    match status {
        EulaStatus::NotAccepted => EulaData {
            accepted: false,
            status: lookup("gui-eula-not-accepted"),
        },
        EulaStatus::Accepted { accepted_at, .. } => EulaData {
            accepted: true,
            status: localized_value(
                "gui-eula-accepted-at",
                &accepted_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            ),
        },
        EulaStatus::Malformed => EulaData {
            accepted: false,
            status: lookup("gui-eula-malformed"),
        },
    }
}

fn scope_parts(scope: DiagnosticScope) -> (&'static str, String) {
    match scope {
        DiagnosticScope::Client(id) => ("client", id.to_string()),
        DiagnosticScope::Server(id) => ("server", id.to_string()),
        DiagnosticScope::Launcher => ("launcher", String::new()),
    }
}

fn scope_entity(app: &App, scope: DiagnosticScope) -> String {
    match scope {
        DiagnosticScope::Client(id) => app
            .client(id)
            .map_or_else(|_| format!("Client {id}"), |record| record.name),
        DiagnosticScope::Server(id) => app
            .server(id)
            .map_or_else(|_| format!("Server {id}"), |record| record.name),
        DiagnosticScope::Launcher => diagnostic_scope_label(scope),
    }
}

fn relative_time(timestamp: DateTime<Utc>) -> String {
    let age = Utc::now().signed_duration_since(timestamp);
    if age.num_minutes() < 1 {
        lookup("gui-just-now")
    } else if age.num_hours() < 1 {
        lookup_args("gui-minutes-ago", [("count", Arg::from(age.num_minutes()))])
    } else if age.num_days() < 1 {
        lookup_args("gui-hours-ago", [("count", Arg::from(age.num_hours()))])
    } else {
        timestamp.format("%Y-%m-%d %H:%M").to_string()
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format_decimal_unit(bytes, 1024, "KiB")
    } else {
        format_decimal_unit(bytes, 1024 * 1024, "MiB")
    }
}

fn format_decimal_unit(bytes: u64, divisor: u64, unit: &str) -> String {
    let tenths = bytes.saturating_mul(10) / divisor;
    format!("{}.{:01} {unit}", tenths / 10, tenths % 10)
}

fn server_warning_message(warning: &ephervex_core::ServerWarning) -> String {
    match warning {
        ephervex_core::ServerWarning::OfflineModeExposed => lookup("gui-warning-offline-exposed"),
        ephervex_core::ServerWarning::PortUnavailable { address, port, .. } => {
            localized_values("gui-warning-port-unavailable", address, &port.to_string())
        }
        _ => lookup("gui-warning-server-generic"),
    }
}

pub fn parse_instance_id(value: &str) -> Option<InstanceId> {
    InstanceId::from_str(value).ok()
}

pub fn parse_snapshot_id(value: &str) -> Option<SnapshotId> {
    SnapshotId::from_str(value).ok()
}

/// Which kind of host this is, in the spelling the markup uses.
const fn host_kind(host: InstalledHost) -> &'static str {
    match host {
        InstalledHost::Instance(_) => "client",
        InstalledHost::Server(_) => "server",
    }
}

/// Turns one mod on or off, asking first what else the change affects.
///
/// Which of the outcomes happens is decided here rather than in the markup.
/// Nothing else affected: apply it. Something affected and the confirmation
/// wanted: send the answer up and change nothing yet. Something affected and
/// the confirmation turned off: apply the whole cascade and say what happened
/// in a toast, because somebody who turned the question off has already agreed
/// to it and leaving the other mods behind would hand them a set that does not
/// load. Relations not followed at all: change this mod, and say nothing about
/// any other.
fn set_mod_enabled(
    app: &App,
    host: InstalledHost,
    path: &Path,
    enabled: bool,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
) {
    let policy = app.dependency_policy();
    if !policy.cascades() {
        match app.set_mods_enabled(host, &[(path.to_path_buf(), enabled)]) {
            Ok(_) => send_host_mods(app, host, events, refreshed, false),
            Err(error) => send_error(events, &error),
        }
        return;
    }
    let warn = policy.asks();
    let changes: Vec<(PathBuf, bool)> = if enabled {
        match app.dependencies_of(host, path) {
            Ok(needs) => {
                let view = ModRequirementPresentation::new(host_kind(host), &host.id(), &needs);
                if !view.is_silent() && warn {
                    let _ = events.send(Event::ModRequirements(Box::new(view)));
                    return;
                }
                if view.has_cascade() {
                    let _ = events.send(Event::Toast {
                        message: lookup_args(
                            "gui-mod-enabled-with-dependencies",
                            [(
                                "count",
                                Arg::from(i64::try_from(view.disabled.len()).unwrap_or(0)),
                            )],
                        ),
                        error: false,
                    });
                }
                view.cascade_paths()
                    .into_iter()
                    .map(|path| (PathBuf::from(path), true))
                    .collect()
            }
            Err(error) => {
                send_error(events, &error);
                return;
            }
        }
    } else {
        match app.dependents_of(host, path) {
            Ok(impact) => {
                let view = ModImpactPresentation::new(host_kind(host), &host.id(), false, &impact);
                if !view.is_silent() && warn {
                    let _ = events.send(Event::ModImpact(Box::new(view)));
                    return;
                }
                if view.has_cascade() {
                    let _ = events.send(Event::Toast {
                        message: lookup_args(
                            "gui-mod-disabled-with-dependents",
                            [(
                                "count",
                                Arg::from(i64::try_from(view.breaks.len()).unwrap_or(0)),
                            )],
                        ),
                        error: false,
                    });
                }
                view.cascade_paths()
                    .into_iter()
                    .map(|path| (PathBuf::from(path), false))
                    .collect()
            }
            Err(error) => {
                send_error(events, &error);
                return;
            }
        }
    };

    match app.set_mods_enabled(host, &changes) {
        Ok(_) => send_host_mods(app, host, events, refreshed, false),
        Err(error) => send_error(events, &error),
    }
}

/// Takes one mod's jar off disk, asking first what else that takes with it.
///
/// The same three outcomes a disable has, decided the same way, because the
/// loader sees the same absence either way. Nothing else affected: delete it,
/// which is what the confirmation the user just gave was for. Something
/// affected and the confirmation wanted: put it in front of them and delete
/// nothing yet. Something affected and the confirmation turned off: take the
/// jars that needed it too and say so, because leaving them behind would leave
/// the instance holding mods that cannot load.
fn delete_mod(
    app: &App,
    host: InstalledHost,
    path: &Path,
    name: String,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
) {
    let policy = app.dependency_policy();
    let view = if policy.cascades() {
        let mut impact = match app.dependents_of(host, path) {
            Ok(impact) => impact,
            Err(error) => {
                send_error(events, &error);
                return;
            }
        };
        // The row's own label, which carries a provider title the durable read
        // does not have.
        impact.target.name = name;
        ModImpactPresentation::new(host_kind(host), &host.id(), true, &impact)
    } else {
        ModImpactPresentation::default()
    };
    if !view.is_silent() && policy.asks() {
        let _ = events.send(Event::ModImpact(Box::new(view)));
        return;
    }

    let taken = view.breaks.len();
    let paths: Vec<PathBuf> = if view.has_cascade() {
        view.cascade_paths()
            .into_iter()
            .map(PathBuf::from)
            .collect()
    } else {
        vec![path.to_path_buf()]
    };
    match app.delete_mod_files(host, &paths) {
        Ok(_) => {
            let _ = events.send(Event::CloseModal);
            let file = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let _ = events.send(Event::Toast {
                message: if taken > 0 {
                    lookup_args(
                        "gui-mod-deleted-with-dependents",
                        [
                            ("value", Arg::from(file.as_str())),
                            ("count", Arg::from(i64::try_from(taken).unwrap_or(0))),
                        ],
                    )
                } else {
                    localized_value("gui-mod-deleted", &file)
                },
                error: false,
            });
            send_host_mods(app, host, events, refreshed, false);
        }
        Err(error) => send_modal_error(events, &error),
    }
}

pub fn parse_server_id(value: &str) -> Option<ServerId> {
    ServerId::from_str(value).ok()
}

/// The instance or server a mod action names, from what the markup carries.
pub fn parse_mod_host(kind: &str, id: &str) -> Option<InstalledHost> {
    match kind {
        "client" => parse_instance_id(id).map(InstalledHost::Instance),
        "server" => parse_server_id(id).map(InstalledHost::Server),
        _ => None,
    }
}

pub fn parse_scope(kind: &str, id: &str) -> Option<DiagnosticScope> {
    match kind {
        "client" => parse_instance_id(id).map(DiagnosticScope::Client),
        "server" => parse_server_id(id).map(DiagnosticScope::Server),
        "launcher" => Some(DiagnosticScope::Launcher),
        _ => None,
    }
}

pub fn parse_diagnostic_kind(value: &str) -> Option<DiagnosticKind> {
    DiagnosticKind::parse(value)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::cell::RefCell;

    use super::*;

    fn logical(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn isolation_lists_drop_the_shared_mods_prefix_and_sort_case_insensitively() {
        assert_eq!(
            isolation_mod_list(&logical(&[
                "mods/b.jar",
                "mods/Apple.jar",
                "mods/a.jar",
                "mods/sub/C.jar",
                "mods/A.jar",
            ])),
            ["A.jar", "a.jar", "Apple.jar", "b.jar", "sub/C.jar"]
        );
        assert_eq!(isolation_mod_display(Path::new("mods")), "mods");
        assert_eq!(
            isolation_mod_display(Path::new("other/x.jar")),
            "other/x.jar"
        );
    }

    #[test]
    fn isolation_setup_shows_display_paths_but_keeps_logical_paths_for_pinning() {
        let host = InstalledHost::Instance(ephervex_model::InstanceId::new());
        let plan = IsolationPlan {
            pinned: Vec::new(),
            pinned_closure: Vec::new(),
            candidates: Vec::new(),
            available: logical(&["mods/zeta.jar", "mods/Alpha.jar"]),
            initially_enabled: logical(&["mods/zeta.jar", "mods/Alpha.jar"]),
            unrecorded: logical(&["mods/zeta.jar", "mods/Alpha.jar"]),
            unidentifiable: Vec::new(),
            missing: vec![ephervex_core::IsolationMissingDependency {
                required_by: PathBuf::from("mods/zeta.jar"),
                dependency: "library".to_owned(),
            }],
            linked_servers: Vec::new(),
        };
        let data = isolation_plan_data(host, &plan);
        assert_eq!(
            data.candidates
                .iter()
                .map(|candidate| (candidate.path.as_str(), candidate.display_path.as_str()))
                .collect::<Vec<_>>(),
            [
                ("mods/Alpha.jar", "Alpha.jar"),
                ("mods/zeta.jar", "zeta.jar")
            ]
        );
        assert_eq!(data.unrecorded, ["Alpha.jar", "zeta.jar"]);
        assert_eq!(data.missing, ["zeta.jar: library"]);
    }

    #[test]
    fn a_setups_basis_follows_the_plan_it_was_computed_from() {
        let plan = |enabled: &[&str]| IsolationPlan {
            pinned: Vec::new(),
            pinned_closure: Vec::new(),
            candidates: Vec::new(),
            available: logical(&["mods/a.jar", "mods/b.jar"]),
            initially_enabled: logical(enabled),
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
            linked_servers: Vec::new(),
        };
        let host = InstalledHost::Instance(ephervex_model::InstanceId::new());
        let data = isolation_plan_data(host, &plan(&["mods/a.jar"]));
        assert_eq!(
            data.basis,
            Some(isolation_plan_basis(&plan(&["mods/a.jar"])))
        );
        assert_ne!(
            data.basis,
            Some(isolation_plan_basis(&plan(&["mods/a.jar", "mods/b.jar"])))
        );
        assert_eq!(inactive_isolation_data(host).basis, None);
    }

    #[test]
    fn isolation_results_list_display_paths_in_case_insensitive_order() {
        let host = InstalledHost::Instance(ephervex_model::InstanceId::new());
        let report = IsolationReport {
            safety_snapshot: ephervex_model::SnapshotId::new(),
            remaining_candidates: Vec::new(),
            pinned: logical(&["mods/Pin.jar"]),
            pinned_dependencies: logical(&["mods/zlib.jar", "mods/Base.jar"]),
            required_dependencies: logical(&["mods/shared.jar", "mods/API.jar"]),
            excluded_roots: Vec::new(),
            enabled: logical(&["mods/b.jar", "mods/API.jar", "mods/a.jar"]),
            disabled: logical(&["mods/Zed.jar", "mods/ace.jar"]),
            rounds: 3,
            confidence: ephervex_core::IsolationConfidence::OneMinimal,
            reduced_confidence: false,
            unrecorded: Vec::new(),
            unidentifiable: Vec::new(),
            missing: Vec::new(),
            linked_servers: Vec::new(),
        };
        let data = isolation_report_data(host, &report);
        assert_eq!(data.enabled, ["a.jar", "API.jar", "b.jar"]);
        assert_eq!(data.disabled, ["ace.jar", "Zed.jar"]);
        assert_eq!(data.required_dependencies, ["API.jar", "shared.jar"]);
        assert_eq!(
            data.pinned,
            [
                "Pin.jar".to_owned(),
                lookup_args(
                    "gui-isolation-pinned-dependency",
                    [("path", Arg::from("Base.jar"))]
                ),
                lookup_args(
                    "gui-isolation-pinned-dependency",
                    [("path", Arg::from("zlib.jar"))]
                ),
            ]
        );
    }

    /// A mod's title does not change while the launcher is open, so the second
    /// time a workspace is opened it repaints rather than asking again.
    #[test]
    fn a_hosts_mods_are_fetched_once_per_run_unless_refresh_is_asked_for() {
        let refreshed: RefreshedHosts = Arc::new(Mutex::new(HashSet::new()));
        let instance = InstalledHost::Instance(ephervex_model::InstanceId::new());
        let other = InstalledHost::Instance(ephervex_model::InstanceId::new());

        assert!(claim_refresh(&refreshed, instance, false), "the first open");
        assert!(
            !claim_refresh(&refreshed, instance, false),
            "the second open repaints what is already known"
        );
        assert!(
            claim_refresh(&refreshed, other, false),
            "a different instance has not been asked about yet"
        );
        assert!(
            claim_refresh(&refreshed, instance, true),
            "Refresh is how a re-fetch stays reachable within one run"
        );
        assert!(
            !claim_refresh(&refreshed, instance, false),
            "and the answer it fetched is what the rest of the run reuses"
        );
    }

    /// A server and an instance are different hosts even where their ids would
    /// not tell them apart.
    #[test]
    fn a_server_and_an_instance_are_tracked_separately() {
        let refreshed: RefreshedHosts = Arc::new(Mutex::new(HashSet::new()));
        assert!(claim_refresh(
            &refreshed,
            InstalledHost::Instance(ephervex_model::InstanceId::new()),
            false
        ));
        assert!(claim_refresh(
            &refreshed,
            InstalledHost::Server(ephervex_model::ServerId::new()),
            false
        ));
    }

    /// Consent is recorded before a byte is fetched, so the settings page has a
    /// new site to show whether or not the download that asked for it worked.
    /// It used to learn about one only on the next start.
    #[test]
    fn agreeing_to_a_site_publishes_it_even_when_the_download_then_fails() {
        let base = tempfile::tempdir().expect("a disposable root");
        let mut app =
            App::open_at(ephervex_core::Paths::rooted_at(base.path())).expect("an app opens");
        let (events, received) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");

        // Nothing is listening, so the fetch fails after the host is agreed to.
        runtime.block_on(open_pack_url(
            &mut app,
            "https://packs.invalid.localhost:1/pack.mrpack",
            Some(ephervex_core::HostConsent::Remember),
            &events,
        ));

        let hosts: Vec<Vec<String>> = received
            .try_iter()
            .filter_map(|event| match event {
                Event::LinkSettings(data) => Some(data.trusted_hosts),
                _ => None,
            })
            .collect();
        assert_eq!(
            hosts,
            vec![vec!["packs.invalid.localhost".to_owned()]],
            "the site the user just agreed to has to reach the settings page"
        );
    }

    fn busy_cache() -> CoreError {
        CoreError::from(ephervex_core::ArtifactCacheError::Busy {
            path: PathBuf::from("/cache/artifact-cache.lock"),
        })
    }

    #[test]
    fn a_refused_clear_reports_the_reason_and_ends_the_clearing_state() {
        let measured = std::cell::Cell::new(false);
        let events = clear_artifact_cache_events(Err(busy_cache()), || {
            measured.set(true);
            Err(busy_cache())
        });

        assert!(
            !measured.get(),
            "a clear that never ran leaves the store unchanged, so nothing needs re-measuring"
        );
        assert!(matches!(
            events.first(),
            Some(Event::Toast { error: true, .. })
        ));
        assert!(matches!(
            events.last(),
            Some(Event::ArtifactCacheClearFinished)
        ));
    }

    #[test]
    fn a_clear_that_cannot_be_measured_afterwards_still_ends_the_clearing_state() {
        let events = clear_artifact_cache_events(
            Ok(ephervex_core::ArtifactCacheMaintenance {
                removed_entries: 3,
                freed_bytes: 4096,
                ..ephervex_core::ArtifactCacheMaintenance::default()
            }),
            || Err(busy_cache()),
        );

        assert!(
            matches!(events.first(), Some(Event::Toast { error: false, .. })),
            "the clear succeeded, so its result is still reported as such"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Toast { error: true, .. }))
        );
        assert!(matches!(
            events.last(),
            Some(Event::ArtifactCacheClearFinished)
        ));
    }

    #[test]
    fn a_measured_clear_ends_on_the_figure_it_measured() {
        let events = clear_artifact_cache_events(
            Ok(ephervex_core::ArtifactCacheMaintenance {
                removed_entries: 1,
                freed_bytes: 1024,
                ..ephervex_core::ArtifactCacheMaintenance::default()
            }),
            || {
                Ok(ephervex_core::ArtifactCacheStatus {
                    used_bytes: 0,
                    entries: 0,
                    complete: true,
                    limit: ephervex_core::ArtifactCacheLimit::default(),
                })
            },
        );

        assert!(matches!(events.last(), Some(Event::ArtifactCacheStatus(_))));
        // The status event is itself terminal for the view, so a second
        // "finished" beside it would say nothing new.
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::ArtifactCacheClearFinished))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Toast { error: true, .. }))
        );
    }

    #[test]
    fn diagnostic_scope_mapping_never_guesses_an_entity_type() {
        assert!(matches!(
            parse_scope("launcher", ""),
            Some(DiagnosticScope::Launcher)
        ));
        assert!(parse_scope("client", "not-an-id").is_none());
        assert!(parse_scope("unknown", "").is_none());
    }

    #[test]
    fn loader_input_supports_only_the_core_loader_families() {
        assert!(loader_selection("Vanilla", "").is_ok());
        assert!(loader_selection("Forge", "47.3.0").is_ok());
        assert!(loader_selection("NeoForge", "21.1.72").is_ok());
        assert!(loader_selection("Fabric", "0.16").is_err());
    }

    #[test]
    fn localized_preference_errors_resolve_action_arguments() {
        let error = ephervex_core::PreferenceValue::parse(
            ephervex_core::PreferenceKey::Fov,
            "not-a-number",
        )
        .expect_err("invalid FOV should be rejected");
        let rendered = localized_error(&CoreError::from(error));

        assert!(!rendered.contains("{$"));
        assert!(!rendered.contains("{ $"));
        assert!(rendered.contains("Expected"));
    }

    /// A regression guard for a real report: the path this failure names comes
    /// only from `failing_path`, not `message_args`, and the action that reads
    /// it rendered a literal `{$path}` in the user's face until both were
    /// merged before the lookup.
    #[test]
    fn a_destination_occupied_action_names_the_actual_path() {
        let error = CoreError::from(ephervex_core::PackError::DestinationOccupied {
            path: PathBuf::from("/instance/minecraft/mods/theirs.jar"),
        });
        let rendered = localized_error(&error);

        assert!(!rendered.contains("{$"));
        assert!(!rendered.contains("{ $"));
        assert!(rendered.contains("/instance/minecraft/mods/theirs.jar"));
    }

    /// Same failure mode as the path case, for the accessor the CLI's own
    /// renderer already merges but the GUI's had not: a message naming the
    /// unsupported loader by name rather than "$loader".
    #[test]
    fn a_loader_unsupported_message_names_the_actual_loader() {
        let error = CoreError::from(ephervex_core::PackError::LoaderUnsupported {
            loader: "fabric-loader".to_owned(),
        });
        let rendered = localized_error(&error);

        assert!(!rendered.contains("{$"));
        assert!(!rendered.contains("{ $"));
        assert!(rendered.contains("fabric-loader"));
    }

    #[test]
    fn cancellable_operations_are_addressed_by_core_operation_id() {
        let registry = Arc::new(Mutex::new(HashMap::new()));
        let reporter = OperationReporter::silent("operation-importing-pack");
        let token = CancellationToken::new();
        register_cancellation(&registry, &reporter, &token);
        let (events, receiver) = mpsc::channel();

        cancel_operation(&registry, &events, &reporter.id().to_string());

        assert!(token.is_cancelled());
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::Toast { error: false, .. })
        ));
        remove_cancellation(&registry, &reporter);
        assert!(registry.lock().is_ok_and(|entries| entries.is_empty()));
    }

    #[test]
    fn launch_and_connect_keeps_runtime_and_live_state_forwarding_owned() {
        let source = include_str!("backend.rs");
        assert!(source.contains("let (launch_observer, mut launch_events)"));
        assert!(source.contains("let (server_observer, mut server_events)"));
        assert!(source.contains("while lines.len() < 256"));
        assert!(source.contains("let _ = launch_forward.await"));
        assert!(source.contains("let _ = server_forward.await"));
        assert!(source.matches("send_snapshot(&app, &events)").count() >= 2);
    }
    #[test]
    fn a_recorded_catalog_is_confirmed_once() {
        let events = default_mod_catalog_events(&Ok(()));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events.first(),
            Some(Event::Toast { error: false, .. })
        ));
    }

    #[test]
    fn a_catalog_that_could_not_be_written_is_never_reported_as_saved() {
        let events = default_mod_catalog_events(&Err(busy_cache()));
        assert_eq!(events.len(), 1);
        assert!(
            matches!(events.first(), Some(Event::Toast { error: true, .. })),
            "a refused write produces the failure and no confirmation"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Snapshot(_))),
            "nothing refreshes the view, so the chips keep showing the stored catalog"
        );
    }

    fn run<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    /// Records every step of one restart in the order it happened.
    #[derive(Default)]
    struct Journal(RefCell<Vec<String>>);

    impl Journal {
        fn note(&self, step: &str) {
            self.0.borrow_mut().push(step.to_owned());
        }
        fn steps(&self) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    #[test]
    fn a_restart_stops_waits_for_the_stop_and_then_starts_once() {
        let journal = Journal::default();
        let outcome = run(restart_sequence(
            || journal.note("stop"),
            async {
                journal.note("observed stopped");
                true
            },
            || {
                journal.note("start");
                async { Ok(()) }
            },
            &|stage| journal.note(stage.as_str()),
        ));
        assert_eq!(outcome, RestartOutcome::Restarted);
        assert_eq!(
            journal.steps(),
            ["stopping", "stop", "observed stopped", "starting", "start"]
        );
    }

    #[test]
    fn nothing_starts_when_the_old_process_was_not_seen_to_stop() {
        let journal = Journal::default();
        let outcome = run(restart_sequence(
            || journal.note("stop"),
            async { false },
            || {
                journal.note("start");
                async { Ok(()) }
            },
            &|stage| journal.note(stage.as_str()),
        ));
        assert_eq!(outcome, RestartOutcome::NotStopped);
        assert_eq!(journal.steps(), ["stopping", "stop"], "no second copy");
    }

    #[test]
    fn a_failed_start_after_a_clean_stop_is_reported_as_that() {
        let outcome = run(restart_sequence(
            || {},
            async { true },
            || async { Err("no Java".to_owned()) },
            &|_| {},
        ));
        assert_eq!(outcome, RestartOutcome::StartFailed("no Java".to_owned()));
    }

    #[test]
    fn a_client_restart_waits_for_the_launch_it_replaces_to_return() {
        let launches: RunningLaunches = Arc::new(Mutex::new(HashMap::new()));
        let (events, received) = mpsc::channel();
        let instance = InstanceId::new();
        let (generation, finished) = register_launch(
            &launches,
            &events,
            instance,
            Some("second-account".to_owned()),
        );
        let running = launches
            .lock()
            .expect("registry")
            .get(&instance)
            .cloned()
            .expect("the launch is known");
        assert_eq!(
            running.account.as_deref(),
            Some("second-account"),
            "the replacement is made as the same account"
        );

        let mut waiting = running.finished.clone();
        let journal = Journal::default();
        let outcome = run(async {
            let restart = restart_sequence(
                || journal.note("stop"),
                async { waiting.wait_for(|done| *done).await.is_ok() },
                || {
                    journal.note("start");
                    async { Ok(()) }
                },
                &|_| {},
            );
            let old_launch_returns = async {
                tokio::task::yield_now().await;
                journal.note("old launch returned");
                unregister_launch(&launches, &events, instance, generation);
                let _ = finished.send(true);
            };
            tokio::join!(restart, old_launch_returns).0
        });

        assert_eq!(outcome, RestartOutcome::Restarted);
        assert_eq!(journal.steps(), ["stop", "old launch returned", "start"]);
        let contexts: Vec<bool> = received
            .try_iter()
            .filter_map(|event| match event {
                Event::LaunchContext { restartable, .. } => Some(restartable),
                _ => None,
            })
            .collect();
        assert_eq!(contexts, [true, false]);
    }

    #[test]
    fn a_finished_launch_cannot_remove_the_one_that_replaced_it() {
        let launches: RunningLaunches = Arc::new(Mutex::new(HashMap::new()));
        let (events, _received) = mpsc::channel();
        let instance = InstanceId::new();
        let (old, _) = register_launch(&launches, &events, instance, None);
        let (new, _) = register_launch(&launches, &events, instance, None);

        unregister_launch(&launches, &events, instance, old);
        assert_eq!(
            launches
                .lock()
                .expect("registry")
                .get(&instance)
                .map(|launch| launch.generation),
            Some(new)
        );
    }

    const WINDOW: Duration = Duration::from_millis(50);

    #[test]
    fn a_replacement_that_prints_an_error_and_exits_is_not_a_restart() {
        // Recorded as the instance's game, then gone before the window ends:
        // the first line it printed proves nothing.
        let checked = std::cell::Cell::new(false);
        let outcome = run(confirm_replacement(
            async { Some(7) },
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                "the game exited with an error".to_owned()
            },
            |_| {
                checked.set(true);
                true
            },
            WINDOW,
        ));
        assert_eq!(outcome, Err("the game exited with an error".to_owned()));
        assert!(
            !checked.get(),
            "the launch ended, so nothing was left to confirm"
        );
    }

    #[test]
    fn a_replacement_that_exits_before_it_is_recorded_is_not_a_restart() {
        let outcome = run(confirm_replacement(
            async { None },
            async { "no Java".to_owned() },
            |_| true,
            WINDOW,
        ));
        assert_eq!(outcome, Err("no Java".to_owned()));
    }

    #[test]
    fn a_replacement_still_recorded_after_the_window_is_a_restart_once() {
        let checks = std::cell::Cell::new(0);
        let outcome = run(confirm_replacement(
            async { Some(7) },
            std::future::pending::<String>(),
            |pid| {
                checks.set(checks.get() + 1);
                pid == 7
            },
            WINDOW,
        ));
        assert_eq!(outcome, Ok(()));
        assert_eq!(checks.get(), 1, "decided exactly once");
    }

    #[test]
    fn a_replacement_the_lock_no_longer_names_is_not_a_restart() {
        let outcome = run(confirm_replacement(
            async { Some(7) },
            std::future::pending::<String>(),
            |_| false,
            WINDOW,
        ));
        assert!(outcome.is_err());
    }

    #[test]
    fn a_server_restart_that_did_not_reach_its_start_says_why() {
        use ephervex_core::{ControlResponse, ServerExit};
        assert_eq!(
            server_restart_outcome(Ok(ControlResponse::Restarted { pid: 7 })),
            RestartOutcome::Restarted
        );
        assert_eq!(
            server_restart_outcome(Ok(ControlResponse::StartFailed("gone".to_owned()))),
            RestartOutcome::StartFailed("gone".to_owned())
        );
        assert!(matches!(
            server_restart_outcome(Ok(ControlResponse::Exited(ServerExit {
                code: Some(0),
                signal: None
            }))),
            RestartOutcome::Interrupted(_)
        ));
        assert!(matches!(
            server_restart_outcome(Ok(ControlResponse::Refused("stopping".to_owned()))),
            RestartOutcome::Interrupted(detail) if detail == "stopping"
        ));
    }

    #[test]
    fn only_a_damaged_file_with_an_address_is_offered_to_repair() {
        use ephervex_core::{Integrity, VerifiedFile};
        let file = |integrity, has_source| {
            verified_file_data(&VerifiedFile {
                logical_path: PathBuf::from("mods/a.jar"),
                integrity,
                has_source,
            })
        };

        assert!(file(Integrity::Absent, true).repairable);
        assert!(file(Integrity::Corrupt, true).repairable);
        assert!(file(Integrity::Unreadable, true).repairable);
        let shipped = file(Integrity::Corrupt, false);
        assert!(
            !shipped.repairable && !shipped.sound,
            "wrong, with nowhere to fetch it from"
        );
        let unproven = file(Integrity::Unverifiable, false);
        assert!(
            !unproven.repairable && unproven.sound,
            "nothing proves it wrong"
        );
        assert!(!file(Integrity::Intact, true).repairable);
    }

    #[test]
    fn a_host_cannot_start_while_a_change_rewrites_its_files() {
        let hosts: ChangingHosts = Arc::new(Mutex::new(HashMap::new()));
        let (events, received) = mpsc::channel();
        let host = InstalledHost::Instance(ephervex_model::InstanceId::new());
        let other = InstalledHost::Server(ephervex_model::ServerId::new());

        let in_flight = HostChangeInFlight::mark(&hosts, host, "gui-repair-busy");
        assert!(refuse_while_changing(&hosts, host, &events));
        let refusal = received.try_recv().expect("a refusal");
        assert!(
            matches!(&refusal, Event::Toast { error: true, message } if *message == lookup("gui-repair-busy")),
            "the change in flight is named: {refusal:?}"
        );
        assert!(!refuse_while_changing(&hosts, other, &events));

        drop(in_flight);
        assert!(!refuse_while_changing(&hosts, host, &events));
        assert!(received.try_recv().is_err());
    }
}
