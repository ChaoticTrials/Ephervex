//! Non-blocking bridge from Slint callbacks to `ephervex-core` services.

use std::collections::{HashMap, HashSet};
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
    CoreError, CoreResult, CrashReview, DiagnosticKind, DiagnosticRequest, DiagnosticScope,
    ErrorChain, EulaStatus, ImportRequest, LaunchAndConnectObserver, LaunchAndConnectOptions,
    LaunchEvent, LaunchObserver, LaunchOptions, LinkedModOutcome, LinkedModPlan, LinkedModRequest,
    LinkedTargetPlan, LinkedTargetResult, LinkedTargetState, LocalModOffer, LocalModPlan,
    LocalModReport, LoginOptions, ManagedModEntry, ManagedModSnapshot, MetadataAvailability,
    ModHost, ModInstallPlan, ModMutability, ModMutation, ModMutationPlan, ModPresence,
    ModSearchQuery, MutationRestriction, NewClient, NewServer, OperationReporter,
    OptionalSelection, PackAnalysis, Paths, PlannedOutcome, ProcessMonitor, ProgressSink,
    ProviderFileIdentity, SearchOptions, ServerContentSource, ServerCreateOptions, ServerEvent,
    ServerFolder, ServerNetworkSettings, ServerObserver, SideRequirement, SuspectReason,
    WatchTarget,
};
use ephervex_core::{
    ConfiguredPreferences, LanguageAvailability, LanguageEntry, MinecraftPreferencePreview,
    MinecraftPreferenceSnapshot, PreferenceEdit,
};
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute, lookup_attribute_args};
use ephervex_model::{
    AbsentModPolicy, InstanceId, LoaderKind, LoaderSelection, ModOrigin, ModProvider, Operation,
    OperationStatus, ServerId, StorageClass,
};

use crate::presentation::{
    ActivityPresentation, ImportErrorKind, ImportFailure, ImportPresentation, InstancePresentation,
    ServerPresentation, diagnostic_kind_label, diagnostic_scope_label,
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
    RestartServer(ServerId),
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
    RefreshMods(ModHost),
    /// What this launcher is running, with live resource use.
    LoadRunning,
    /// End a launch this process is not supervising, and release its lock.
    ForceStopLaunch {
        instance: InstanceId,
        /// Which of the instance's launches, for one running more than one.
        launch: Option<String>,
    },
    /// Turn one mod on or off in place, on either kind of host.
    SetModEnabled {
        host: ModHost,
        path: PathBuf,
        enabled: bool,
    },
    /// Work out what dropping these files into a host's mods folder would do.
    ///
    /// Writes nothing. Its answer is what the name-clash question is asked
    /// from, so the decision is made by the person who can make it rather than
    /// by a thread that cannot ask.
    PlanLocalMods {
        host: ModHost,
        sources: Vec<PathBuf>,
    },
    /// Copy these files into a host's mods folder and reload the list.
    ///
    /// Only what the user agreed to: anything they chose to skip never gets
    /// here. `skipped` is carried through so the result can say so.
    AddLocalMods {
        host: ModHost,
        sources: Vec<PathBuf>,
        skipped: usize,
    },
    /// Delete one jar the user put in the mods folder themselves.
    DeleteHandPlacedMod {
        host: ModHost,
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
    SaveDisplaySettings {
        text_scale: ephervex_core::ApplicationTextScale,
        technical_text_size: ephervex_core::TechnicalTextSize,
    },
    SaveLinkedServerSettings {
        show_globally: bool,
        make_operator: bool,
        operator_level: ephervex_model::OperatorPermissionLevel,
    },
    RefreshJava,
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
    /// Whether a CurseForge API key is stored.
    ///
    /// A `bool` and nothing else. There is deliberately no field the key itself
    /// could travel in, so no view can render it and no snapshot can carry it
    /// into a log or a crash report.
    pub curseforge_key_present: bool,
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
    pub show_linked_servers: bool,
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
    CrashNotice(CrashNoticeData),
    Running(Vec<RunningData>),
    ModVersions(ModVersionsData),
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
    JavaRuntimes(Vec<JavaData>),
    Activity(ActivityData),
    Toast {
        message: String,
        error: bool,
    },
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
    /// A folder watcher found and verified a hand-placed copy of one blocked
    /// file's bytes while the import modal was open on it.
    ///
    /// A receiver must discard this unless `generation` still names the
    /// attempt it is tracking, exactly like [`Event::ImportResolved`]. Does
    /// not by itself change what final import will do — that re-checks the
    /// shared cache independently — it only clears the row from the modal's
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
    // Kept between polls, because CPU is a rate rather than a reading.
    let mut process_monitor = ProcessMonitor::new();
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
                kill_instance(&cancellations, &events, instance, launch.as_deref());
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
                        request_id,
                        "operation-creating-instance",
                        entity,
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
                                localized_value("gui-created-entity", &created.record.name)
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
                let paths = app.paths().clone();
                let entity = app
                    .client(instance)
                    .map_or_else(|_| instance.to_string(), |record| record.name);
                spawn_launch(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    instance,
                    entity,
                    account,
                    additional,
                );
            }
            Command::LaunchAndConnect { client, server } => {
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
                match app.absent_recorded_mods(ModHost::Instance(instance)) {
                    Ok(absent) => {
                        let _ = events.send(Event::AbsentMods(AbsentModsData {
                            instance: instance.to_string(),
                            names: absent.into_iter().map(|entry| entry.name).collect(),
                        }));
                    }
                    Err(error) => send_error(&events, &error),
                }
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
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result = runtime.block_on(app.apply_linked_mod_operation(
                        instance,
                        &LinkedModRequest::ChangeVersion {
                            target: path,
                            version,
                        },
                        &servers,
                        &OperationReporter::silent("changing-mod-version"),
                        &CancellationToken::new(),
                    ));
                    finish_linked(&app, &events, instance, result);
                });
            }
            Command::UpdateMod {
                instance,
                path,
                servers,
            } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result = runtime.block_on(app.apply_linked_mod_operation(
                        instance,
                        &LinkedModRequest::Update { target: path },
                        &servers,
                        &OperationReporter::silent("updating-mod"),
                        &CancellationToken::new(),
                    ));
                    finish_linked(&app, &events, instance, result);
                });
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
                        let host = ModHost::Instance(instance);
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
                refresh_enriched_mods(app.paths().clone(), &events, instance);
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
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    "operation-installing-mods",
                    project.clone(),
                    Some(Event::ModInstallFinished {
                        instance: instance.to_string(),
                        provider: provider.as_str().to_owned(),
                        project: project.clone(),
                    }),
                    move |app, reporter, cancel| async move {
                        let request = LinkedModRequest::Install {
                            provider,
                            project,
                            // Pinned to what the user agreed to, so a release
                            // published since the confirmation cannot take its
                            // place here or on any selected server.
                            version: (!version.is_empty()).then_some(version),
                        };
                        let outcome = app
                            .apply_linked_mod_operation(
                                instance, &request, &servers, &reporter, &cancel,
                            )
                            .await?;
                        let message = client_outcome_message(&outcome);
                        // The Managed Mods panel may already be open on this
                        // instance, and a newly installed mod must appear on it
                        // without the workspace being reopened. Recorded state
                        // is authoritative about which mods are there and is
                        // shown at once; the provider title, version, and
                        // artwork follow, so the new row does not sit there as a
                        // file name until the workspace is opened again.
                        if let Ok(snapshot) = app.managed_mod_snapshot(instance) {
                            let review =
                                app.crash_review(ModHost::Instance(instance)).ok().flatten();
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
                        spawn_creation_operation(
                            paths,
                            &events,
                            Arc::clone(&cancellations),
                            request_id,
                            "operation-creating-server",
                            request.name.clone(),
                            move |app, _reporter, cancel| async move {
                                app.create_server(
                                    request,
                                    &ServerCreateOptions {
                                        install_java_if_missing: true,
                                        content: server_pack
                                            .map_or(ServerContentSource::Recipe, |path| {
                                                ServerContentSource::Archive { path }
                                            }),
                                        ..ServerCreateOptions::default()
                                    },
                                    &cancel,
                                )
                                .await
                                .map(|commit| {
                                    localized_value("gui-created-entity", &commit.record.name)
                                })
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
            Command::RestartServer(server) => {
                report_async_control(
                    &runtime,
                    &events,
                    app.restart_server(server),
                    "gui-restart-requested",
                );
                send_snapshot(&app, &events);
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
                let host = ModHost::Server(server);
                let review = app.crash_review(host).ok().flatten();
                let _ = events.send(Event::CrashNotice(crash_notice_data(host, review.as_ref())));
                send_server_workspace(&app, server, &events);
            }
            Command::LoadServerMods(server) => {
                send_host_mods(
                    &app,
                    ModHost::Server(server),
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
            Command::DeleteHandPlacedMod { host, path } => {
                match app.delete_hand_placed_mod(host, &path) {
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
                }
            }
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
            } => match app.set_mod_enabled(host, &path, enabled) {
                // Re-read rather than patched in place: the durable state is
                // what the panel shows, and it has just changed.
                Ok(_) => send_host_mods(&app, host, &events, &refreshed_mods, false),
                Err(error) => send_error(&events, &error),
            },
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
            Command::SaveDisplaySettings {
                text_scale,
                technical_text_size,
            } => match app.set_application_display_settings(text_scale, technical_text_size) {
                Ok(()) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-display-settings-saved"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
            Command::SaveLinkedServerSettings {
                show_globally,
                make_operator,
                operator_level,
            } => {
                let result =
                    app.set_linked_server_settings(show_globally, make_operator, operator_level);
                match result {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-server-defaults-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::RefreshJava => match runtime.block_on(app.refresh_java_runtimes()) {
                Ok(inventory) => {
                    let rows = inventory
                        .runtimes
                        .iter()
                        .map(|java| JavaData {
                            path: java.executable.display().to_string(),
                            version: java.version_string.clone(),
                            vendor: java.vendor.clone(),
                            origin: java.origins.first().map_or_else(
                                || "detected".to_owned(),
                                |origin| origin.kind.to_string(),
                            ),
                            compatible: format!("Java {}", java.version.major),
                        })
                        .collect();
                    let _ = events.send(Event::JavaRuntimes(rows));
                }
                Err(error) => send_error(&events, &error),
            },
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
        instance_rows.push(InstancePresentation::from_record(
            &record,
            linked.len(),
            managed_mods,
            launch_state.as_ref(),
            connections,
        ));
    }
    let mut server_rows = Vec::new();
    for server in servers {
        let status = app.server_status(server.id)?;
        let mut presentation = ServerPresentation::from_status(&status);
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
        curseforge_key_present: api.curseforge_api_key_present,
        default_mod_catalog: catalog.selected,
        configured_mod_catalog: catalog.configured,
        mod_catalog_origin: origin(catalog.chosen),
        text_scale_percent: display.text_scale.percent(),
        technical_text_size_pixels: display.technical_text_size.pixels(),
        show_linked_servers: display.show_linked_servers,
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
/// Durable first and always: the list is correct without a catalog, and a
/// server that will not start is the moment a user least wants to wait for one.
fn send_host_mods(
    app: &App,
    host: ModHost,
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
            if claim_refresh(refreshed, host, force) {
                refresh_enriched_host_mods(app.paths().clone(), events, host);
            }
        }
        Err(error) => send_error(events, &error),
    }
}

/// Turns off the mods the newest crash report stated, and says how many.
///
/// Only what the report stated: a compat trigger is an inference and stays a
/// choice the user makes one row at a time. Only mods that are still enabled,
/// so pressing this twice does nothing the second time.
fn disable_suggested_mods(
    app: &App,
    server: ServerId,
    events: &Sender<Event>,
    refreshed: &RefreshedHosts,
) {
    let host = ModHost::Server(server);
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
                ModHost::Instance(instance) => {
                    supervised.contains(&launch_key(instance, process.launch.as_deref()))
                }
                ModHost::Server(_) => process.controllable,
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

fn spawn_launch(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    instance: InstanceId,
    entity: String,
    account: Option<String>,
    additional: Option<String>,
) {
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
            runtime.block_on(async move {
                let forward = tokio::spawn(async move {
                    while let Some(event) = receiver.recv().await {
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
                match result {
                    Ok(outcome) => {
                        let detail = if outcome.is_success() {
                            lookup("gui-activity-exited-cleanly")
                        } else {
                            lookup("gui-activity-game-error")
                        };
                        send_activity_state(
                            &events,
                            outcome.operation.to_string(),
                            &lookup("gui-activity-launch-minecraft"),
                            &entity,
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
                            &events,
                            id,
                            &lookup("gui-activity-launch-minecraft"),
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
                release_launch_cancellation(&killing, instance, &observed_operation);
                if let Ok(app) = App::open_at(paths) {
                    send_snapshot(&app, &events);
                }
            });
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
    title: &'static str,
    entity: String,
    finished: Option<Event>,
    operation: F,
) where
    F: FnOnce(App, OperationReporter, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<OperationReport, CoreError>> + 'static,
{
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
                allow_cancel: true,
                pending: None,
            });
            let reporter = OperationReporter::new(title, true, sink);
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

fn spawn_creation_operation<F, Fut>(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    request_id: String,
    title: &'static str,
    entity: String,
    operation: F,
) where
    F: FnOnce(App, OperationReporter, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<String, CoreError>> + 'static,
{
    let failed_request_id = request_id.clone();
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
                pending: None,
            });
            let reporter = OperationReporter::new(title, false, sink);
            reporter.begin(Some(lookup("gui-activity-preparing-environment")));
            let cancel = CancellationToken::new();
            register_cancellation(&cancellations, &reporter, &cancel);
            let result = runtime.block_on(operation(app, reporter.clone(), cancel));
            remove_cancellation(&cancellations, &reporter);
            match result {
                Ok(message) => {
                    reporter.finish(OperationStatus::Completed);
                    let _ = events.send(Event::Toast {
                        message,
                        error: false,
                    });
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
            // target is matched or `cancel` fires — the same cancellation a
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
    let args = error
        .message_args()
        .into_iter()
        .map(|(name, value)| (name, Arg::from(value)))
        .collect::<Vec<_>>();
    let mut message = if args.is_empty() {
        lookup(error.message_id())
    } else {
        lookup_args(error.message_id(), args.clone())
    };
    let action = if args.is_empty() {
        lookup_attribute(error.message_id(), "action")
    } else {
        lookup_attribute_args(error.message_id(), "action", args)
    };
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
fn claim_refresh(refreshed: &RefreshedHosts, host: ModHost, force: bool) -> bool {
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
    refresh_enriched_host_mods(paths, events, ModHost::Instance(instance));
}

fn refresh_enriched_host_mods(paths: Paths, events: &Sender<Event>, host: ModHost) {
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
            if let ClientModOutcome::Mutated(mutation) = &outcome.client {
                let review = app.crash_review(ModHost::Instance(instance)).ok().flatten();
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
fn local_mod_plan_data(host: ModHost, plan: &LocalModPlan) -> LocalModPlanData {
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
            // A recorded mod whose jar is not in `mods/` is left out. A row for
            // a file that is not there names nothing the reader can act on, and
            // the record is kept so a repair still puts the jar back.
            .filter(|entry| entry.presence != ModPresence::Missing)
            .map(|entry| managed_mod_data(entry, review))
            .collect(),
        notice: match snapshot.metadata {
            MetadataAvailability::Unavailable => lookup("gui-mod-metadata-offline"),
            MetadataAvailability::Local | MetadataAvailability::Provider => String::new(),
        },
    }
}

fn crash_notice_data(host: ModHost, review: Option<&CrashReview>) -> CrashNoticeData {
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

fn managed_mod_data(entry: &ManagedModEntry, review: Option<&CrashReview>) -> ManagedModData {
    let title = entry.display_name().to_owned();
    let mutability = ModMutability::of(entry);
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

pub fn parse_server_id(value: &str) -> Option<ServerId> {
    ServerId::from_str(value).ok()
}

/// The instance or server a mod action names, from what the markup carries.
pub fn parse_mod_host(kind: &str, id: &str) -> Option<ModHost> {
    match kind {
        "client" => parse_instance_id(id).map(ModHost::Instance),
        "server" => parse_server_id(id).map(ModHost::Server),
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

    use super::*;

    /// A mod's title does not change while the launcher is open, so the second
    /// time a workspace is opened it repaints rather than asking again.
    #[test]
    fn a_hosts_mods_are_fetched_once_per_run_unless_refresh_is_asked_for() {
        let refreshed: RefreshedHosts = Arc::new(Mutex::new(HashSet::new()));
        let instance = ModHost::Instance(ephervex_model::InstanceId::new());
        let other = ModHost::Instance(ephervex_model::InstanceId::new());

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
            ModHost::Instance(ephervex_model::InstanceId::new()),
            false
        ));
        assert!(claim_refresh(
            &refreshed,
            ModHost::Server(ephervex_model::ServerId::new()),
            false
        ));
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
}
