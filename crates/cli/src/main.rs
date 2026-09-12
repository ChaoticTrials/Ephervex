//! Ephervex command-line interface.
//!
//! The CLI is a thin shell over the same services the GUI uses. It contains no
//! business logic of its own: it parses arguments, calls into the crates below
//! it, and formats the result as text or as JSON.
//!
//! Two conventions run through the whole file:
//!
//! * **stdout is data, stderr is commentary.** `tracing` writes to stderr, so
//!   `--json` output can be piped straight into `jq`.
//! * **Failures are localized summaries plus technical detail.** The typed
//!   errors below carry a stable message id, never English prose; the text a
//!   user reads is resolved here through `ephervex-i18n`.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, CommandFactory as _, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use ephervex_core::{
    AccountStatus, AccountSummary, ApiSettings, App, ArgumentForm, Artifact, ArtifactCacheHealth,
    AssetIndexRef, AuthEvent, AuthObserver, BundledDocumentHealth, CancellationToken,
    CaptureHealth, CapturedLine, ClientFolder, ClientRecord, Compatibility, Config,
    ConfigurationHealth, ControlResponse, CoreError, CrashReview, CreatedClient,
    CredentialBackendHealth, CredentialStorage, DEFAULT_DIAGNOSTIC_BYTES, DEFAULT_SEARCH_LIMIT,
    DiagnosticEntry, DiagnosticKind, DiagnosticRequest, DiagnosticScope, DiagnosticSearch,
    DirectoryState, DuplicateClient, EULA_CONFIRMATION_PHRASE, Entitlements, EulaStatus,
    FileDisposition, Freshness, ImportRequest, ImportedPack, IncompatibilityReason,
    InstallationHealth, InstalledMods, JavaHealth, JavaPathChoice, JavaPlan, JavaPlanRequest,
    JavaRuntime, JavaTarget, LaunchAndConnectEvent, LaunchAndConnectObserver,
    LaunchAndConnectOptions, LaunchAndConnectOutcome, LaunchEvent, LaunchFeatures, LaunchObserver,
    LaunchOptions, LaunchOutcome, LaunchState, LoginFlow, LoginOptions, LoginRequest,
    MAX_SEARCH_MATCHES, MINECRAFT_EULA_REFERENCE, ManagedInstallation, ManagedMod,
    ManagedModSnapshot, ModHost, ModInstallPlan, ModInstallRequest, ModPresence, ModProjectDetails,
    ModSearchQuery, ModSearchResults, NetworkAccess, NewClient, NewServer, OperationReporter,
    OptionalSelection, OutputLine, PackAnalysis, PackRecipe, PackSide, Paths, PlanWarning,
    PlannedMod, PreferenceApplication, PreferenceId, PreferenceKey, ProcessMonitor,
    ReconciliationReport, ReleaseListing, RequirementBasis, RuntimeClassification,
    RuntimeInventory, SearchOptions, SecretString, ServerCommitWarning, ServerContentSource,
    ServerCreateOptions, ServerEvent, ServerFolder, ServerNetworkSettings, ServerObserver,
    ServerPersistenceHealth, ServerRecord, ServerWarning, Session, SideRequirement, SkipReason,
    SuspectConfidence, SuspectReason, TextAnchor, TextView, UnknownServerFiles, VersionMetadata,
    VersionRecord,
};
use ephervex_i18n::Arg as MessageArg;
use ephervex_model::{
    AbsentModPolicy, DefaultModCatalog, InstanceId, JavaOverride, JvmSettings, LoaderKind,
    LoaderSelection, ModProvider, OperationId, ServerId, StorageClass,
};

#[derive(Debug, Parser)]
#[command(
    name = "ephervex",
    version,
    about = "Disposable Minecraft instances for mod development and bug reproduction"
)]
struct Cli {
    /// Emit machine-readable JSON where the command supports it.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a shell completion script on standard output.
    Completions {
        /// The shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Show build information.
    Info,
    /// Show the resolved application directories.
    Paths,
    /// Check this installation and report what is wrong with it.
    ///
    /// Local only: nothing here contacts a network service. Credentials are
    /// never printed, and whether one is stored is not reported at all -- only
    /// whether the system has somewhere to keep them.
    Doctor,
    /// Show or change the effective configuration.
    Config {
        /// Omit to show the configuration, as `ephervex config` always has.
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Inspect and change global Minecraft preferences.
    Preferences {
        #[command(subcommand)]
        command: PreferencesCommand,
    },
    /// Operate on the set of client instances.
    Instances {
        #[command(subcommand)]
        command: InstancesCommand,
    },
    /// Operate on one client instance.
    Instance {
        #[command(subcommand)]
        command: InstanceCommand,
    },
    /// Operate on the set of servers.
    Servers {
        #[command(subcommand)]
        command: ServersCommand,
    },
    /// Operate on one server.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Inspect Mojang's Minecraft release metadata.
    Minecraft {
        #[command(subcommand)]
        command: MinecraftCommand,
    },
    /// Inspect and manage Java runtimes.
    Java {
        #[command(subcommand)]
        command: JavaCommand,
    },
    /// Sign in, inspect, and sign out of the Microsoft account.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Inspect and change provider API settings.
    Api {
        #[command(subcommand)]
        command: ApiCommand,
    },
    /// Find and install mods in a client instance.
    Mod {
        #[command(subcommand)]
        command: ModCommand,
    },
    /// Inspect launcher, client, and server logs and crash reports.
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// Import a modpack archive into a client instance.
    Import(ImportArgs),
    /// Install and launch a client instance, streaming its log to the terminal.
    Launch(LaunchArgs),
    /// List everything Ephervex is currently running.
    ///
    /// Client instances with a live launch, and servers this launcher is still
    /// supervising. A server whose supervisor is gone is a recovery case: see
    /// `ephervex server recover`.
    Running,
    /// Reconcile the index against what is actually on disk.
    Reconcile,
}

#[derive(Debug, Args)]
struct LaunchArgs {
    /// The client instance to launch.
    id: InstanceId,
    /// Start or reuse this linked server and connect the client to it.
    #[arg(long, value_name = "SERVER")]
    server: Option<ServerId>,
    /// Use this Java runtime instead of the selected one.
    #[arg(long)]
    java_path: Option<PathBuf>,
    /// Download a managed runtime if nothing installed is compatible.
    #[arg(long)]
    install_java: bool,
    /// Run alongside a launch this instance already has.
    ///
    /// A second client, under a second account, sharing the instance's game
    /// directory: its saves, its options and its logs. Fine when both clients
    /// join a server. Two clients opening the same singleplayer world is
    /// decided by Minecraft's own world lock, not by Ephervex.
    #[arg(long, value_name = "HANDLE")]
    alongside: Option<String>,
    /// Play as this account instead of the default one.
    ///
    /// Takes an account id or name, as `ephervex account list` shows them.
    /// This applies to one launch and does not change which account is the
    /// default.
    #[arg(long, value_name = "ACCOUNT")]
    account: Option<String>,
    /// Start Minecraft in demo mode.
    #[arg(long)]
    demo: bool,
    /// Open the game window at this size, as `WIDTHxHEIGHT`.
    #[arg(long, value_name = "WIDTHxHEIGHT")]
    resolution: Option<String>,
    /// Prepare everything and print the command without starting Minecraft.
    ///
    /// Everything is still installed, because a command whose classpath names
    /// files that were never downloaded describes a launch that could not run.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct ImportArgs {
    /// The pack archive. The format is detected from its contents, not its
    /// name: a `.mrpack` renamed to `.zip` still imports.
    archive: PathBuf,
    /// The instance's name. Defaults to the pack's own name.
    #[arg(long)]
    name: Option<String>,
    #[command(flatten)]
    storage: StorageArgs,
    /// Read the pack and report what importing it would do, without importing.
    ///
    /// This reports optional files and planned changes before any instance is
    /// written.
    #[arg(long)]
    inspect: bool,
    /// Take this optional file, by its path inside the game directory.
    ///
    /// Repeatable. Optional files are off by default unless selected explicitly.
    #[arg(long = "optional", value_name = "PATH")]
    optional: Vec<PathBuf>,
    /// Take every optional file the pack offers.
    #[arg(long, conflicts_with = "optional")]
    all_optional: bool,
}

/// Which mod catalog a command reads.
///
/// CurseForge needs your own API key, stored with
/// `ephervex api set-curseforge-key`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CatalogArg {
    Modrinth,
    /// Spelled the way CurseForge spells it, rather than the hyphenated name
    /// derived from the variant.
    #[value(name = "curseforge")]
    CurseForge,
}

impl CatalogArg {
    const fn provider(self) -> ModProvider {
        match self {
            Self::Modrinth => ModProvider::Modrinth,
            Self::CurseForge => ModProvider::CurseForge,
        }
    }
}

/// The catalog a command works in: the one named on the command line, or the
/// one the settings record.
///
/// No clap default, so that leaving `--provider` off reads the same durable
/// preference the desktop app browses with instead of a second default that
/// could disagree with it.
fn catalog_or_preference(app: &App, named: Option<CatalogArg>) -> anyhow::Result<ModProvider> {
    match named {
        Some(catalog) => Ok(catalog.provider()),
        None => Ok(app.browsing_catalog()?),
    }
}

/// Mods are searched for, inspected, and installed against one instance,
/// because whether a mod fits depends entirely on that instance's exact
/// Minecraft version and mod loader.
#[derive(Debug, Subcommand)]
enum ModCommand {
    /// Search a mod catalog for mods that fit an instance.
    Search {
        /// The client instance the results are judged against.
        instance: InstanceId,
        /// What to search for.
        query: String,
        /// Which catalog to search. Defaults to your preferred catalog.
        #[arg(long, value_enum)]
        provider: Option<CatalogArg>,
        /// Show at most this many results.
        #[arg(long, default_value_t = DEFAULT_SEARCH_LIMIT)]
        limit: u8,
        /// Skip this many results, to page further into a long list.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Also list mods that do not fit, with the reason each one does not.
        ///
        /// They are shown so you can see why the mod you were looking for is
        /// not offered. They still cannot be installed.
        #[arg(long)]
        include_incompatible: bool,
    },
    /// Show a mod and the versions available for an instance.
    Show {
        instance: InstanceId,
        /// The mod, by its project id or slug in the chosen catalog.
        project: String,
        /// Which catalog the mod is on. Defaults to your preferred catalog.
        #[arg(long, value_enum)]
        provider: Option<CatalogArg>,
        /// Also list versions that do not fit, with the reason each one does
        /// not.
        #[arg(long)]
        include_incompatible: bool,
    },
    /// Show what installing a mod would involve, without installing it.
    ///
    /// Reports the version that would be installed, every dependency that
    /// comes with it, and the optional ones you could add. Writes nothing.
    Plan {
        /// The client instance the mod would be installed into.
        instance: InstanceId,
        /// The mod, by its project id or slug in the chosen catalog.
        project: String,
        #[command(flatten)]
        selection: ModSelectionArgs,
    },
    /// Install a mod and everything it requires.
    ///
    /// The newest version that fits the instance is chosen unless you name
    /// one. Required dependencies are installed with it; optional ones are not,
    /// unless you ask for them.
    Install {
        /// The client instance to install into.
        instance: InstanceId,
        /// The mod, by its project id or slug in the chosen catalog.
        project: String,
        #[command(flatten)]
        selection: ModSelectionArgs,
    },
    /// Show or set which catalog mod commands use when none is named.
    ///
    /// The same setting the desktop app's mod browser opens on. Local only:
    /// naming a catalog records a preference and contacts nothing, and a
    /// CurseForge API key is still needed at the moment a CurseForge request
    /// is actually made.
    Catalog {
        /// The catalog to prefer. Omit to show the current setting.
        #[arg(value_enum)]
        catalog: Option<CatalogArg>,
        /// Go back to Ephervex's own default catalog.
        #[arg(long, conflicts_with = "catalog")]
        clear: bool,
    },
    /// List the mods installed into an instance through a provider.
    ///
    /// Jars you placed in the mods folder yourself are not listed: Ephervex has
    /// no project or version for them and does not guess at one.
    List {
        /// The client instance to list.
        instance: InstanceId,
    },
    /// Let a mod load again after it was disabled.
    Enable {
        #[command(flatten)]
        host: ModHostArgs,
        /// The mod's path inside the game directory, such as `mods/foo.jar`.
        path: PathBuf,
    },
    /// Keep a mod installed but stop the loader from loading it.
    ///
    /// The jar is renamed to `<name>.jar.disabled`, which every loader ignores.
    /// Nothing is deleted and nothing is forgotten: the mod keeps its recorded
    /// version and provider, and enabling it again needs no download.
    Disable {
        #[command(flatten)]
        host: ModHostArgs,
        /// The mod's path inside the game directory, such as `mods/foo.jar`.
        path: PathBuf,
    },
}

/// Which instance or server a mod belongs to.
#[derive(Debug, Args)]
struct ModHostArgs {
    /// The client instance whose mods these are.
    #[arg(
        long,
        value_name = "INSTANCE",
        conflicts_with = "server",
        required_unless_present = "server"
    )]
    instance: Option<InstanceId>,
    /// The dedicated server whose mods these are.
    #[arg(long, value_name = "SERVER")]
    server: Option<ServerId>,
}

impl ModHostArgs {
    /// The named host.
    ///
    /// `clap` has already refused both and neither, so the remaining
    /// combination cannot occur. It is reported rather than asserted, so that
    /// a change to the argument definitions above cannot turn into a panic.
    fn host(&self) -> anyhow::Result<ModHost> {
        match (self.instance, self.server) {
            (Some(instance), None) => Ok(ModHost::Instance(instance)),
            (None, Some(server)) => Ok(ModHost::Server(server)),
            _ => Err(anyhow::anyhow!(
                "name exactly one of --instance or --server"
            )),
        }
    }
}

/// Which version, and which optional extras, an install should take.
#[derive(Debug, Args)]
struct ModSelectionArgs {
    /// Which catalog the mod is on. Defaults to your preferred catalog.
    #[arg(long, value_enum)]
    provider: Option<CatalogArg>,
    /// Install this exact version instead of the newest compatible one.
    ///
    /// Preserved exactly. If it does not fit the instance, the install is
    /// refused rather than moved to a nearby release.
    #[arg(long)]
    version: Option<String>,
    /// Also install this optional dependency, by its project id.
    ///
    /// Repeatable. Optional dependencies are left out unless named here.
    #[arg(long = "optional", value_name = "PROJECT")]
    optional: Vec<String>,
}

impl ModSelectionArgs {
    fn request(&self, project: &str, catalog: ModProvider) -> ModInstallRequest {
        let request = ModInstallRequest::new(project)
            .from_provider(catalog)
            .selecting_optional(self.optional.clone());
        match &self.version {
            Some(version) => request.at_version(version),
            None => request,
        }
    }
}

#[derive(Debug, Subcommand)]
enum AccountCommand {
    /// List every account this installation has.
    List,
    /// Add a local account with no credential and no network.
    ///
    /// The UUID is derived the way a Minecraft server derives it in offline
    /// mode, so a client and its linked test server agree on who the player is.
    AddOffline {
        /// Minecraft player name, 3-16 characters of letters, digits, or `_`.
        name: String,
    },
    /// Choose which account a launch uses.
    Use {
        /// Account UUID, or display name if it is unambiguous.
        account: String,
    },
    /// Remove one account, deleting only its own stored credential.
    Remove {
        /// Account UUID, or display name if it is unambiguous.
        account: String,
    },
    /// Sign in with a Microsoft account.
    Login {
        /// Show a code to enter on another device instead of opening a browser.
        ///
        /// The right choice over SSH or on a machine with no desktop.
        #[arg(long)]
        device_code: bool,
        /// Do not open a browser automatically; just print the address.
        #[arg(long, conflicts_with = "device_code")]
        no_open: bool,
        /// Sign in for this session only when no system keyring is available.
        ///
        /// Without this, a machine with no Secret Service refuses to sign in
        /// rather than storing a credential that disappears after exit.
        #[arg(long)]
        session_only: bool,
        /// How long to wait for the sign-in to be completed, in seconds.
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Show the signed-in account, without contacting anyone.
    Status,
    /// Refresh the stored credential and prove it still works.
    Refresh,
    /// Sign out and remove the stored credential.
    Logout,
}

#[derive(Debug, Subcommand)]
enum ApiCommand {
    /// Show the API settings and where credentials are kept.
    Show,
    /// Set the Microsoft Client ID, or clear it back to this build's default.
    SetClientId {
        /// The Azure application (client) ID. Omit to clear.
        client_id: Option<String>,
    },
    /// Set the advanced custom User-Agent, or clear it.
    SetUserAgent { user_agent: Option<String> },
    /// Read a CurseForge API key from standard input and store it in the
    /// system keyring.
    ///
    /// This is a CurseForge third-party API key, which you apply for through
    /// CurseForge/Overwolf. It is not the CurseForge Upload API token. The key
    /// is stored in the system keyring and never written to Ephervex's
    /// configuration.
    ///
    /// From stdin rather than an argument on purpose: process arguments are
    /// readable by every other process on the machine through `/proc`, and a
    /// shell would put the key in the history file too.
    SetCurseforgeKey,
    /// Remove the stored CurseForge API key.
    ClearCurseforgeKey,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show the effective configuration.
    Show,
    /// Show or move the persistent instance root.
    ///
    /// Instances that already exist do not move: their locations are recorded
    /// and absolute. The new root applies to instances created from here on.
    InstanceRoot {
        /// The directory to store persistent instances in. Omit to show it.
        path: Option<PathBuf>,
        /// Go back to the default under the XDG data directory.
        #[arg(long, conflicts_with = "path")]
        clear: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PreferencesCommand {
    /// Show every managed preference and the value it applies.
    Show,
    /// Set a preference and apply it to instances created from here on.
    Set {
        /// Preference name, e.g. `gui_scale`.
        #[arg(value_parser = parse_preference_key)]
        preference: PreferenceKey,
        /// The value. Run `ephervex preferences show` for the accepted syntax.
        value: String,
    },
    /// Stop applying a preference, keeping its value for later.
    Unmanage {
        #[arg(value_parser = parse_preference_key)]
        preference: PreferenceKey,
    },
}

#[derive(Debug, Subcommand)]
enum MinecraftCommand {
    /// List the Minecraft releases Ephervex supports.
    Versions {
        /// Show only the newest N releases.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Resolve one release's complete metadata.
    Version { id: String },
}

#[derive(Debug, Subcommand)]
enum JavaCommand {
    /// List the Java runtimes found on this computer.
    List,
    /// Rescan for Java runtimes, ignoring the cached result.
    Refresh,
    /// Probe one Java executable and report what it is.
    Test { path: PathBuf },
    /// Show which runtime would run a Minecraft version, and why.
    Select {
        /// Minecraft version id, e.g. `1.12.2`.
        minecraft: String,
        /// Apply this instance's Java and JVM overrides.
        #[arg(long)]
        instance: Option<InstanceId>,
        /// Use this runtime instead of selecting one. Outranks every setting.
        #[arg(long)]
        java_path: Option<PathBuf>,
        /// Download a managed runtime if nothing installed is compatible.
        #[arg(long)]
        install: bool,
    },
    /// Download and install a managed Temurin runtime.
    Install {
        /// Java major version, e.g. `21`.
        major: u32,
    },
}

#[derive(Debug, Subcommand)]
enum InstancesCommand {
    /// List every indexed client instance.
    List,
}

#[derive(Debug, Subcommand)]
enum InstanceCommand {
    /// Create a client instance.
    Create(CreateInstanceArgs),
    /// Show one client instance in full.
    Show { id: InstanceId },
    /// Copy an instance, files and all, into a new one.
    Duplicate {
        id: InstanceId,
        /// The copy's name. Defaults to the original's plus " (copy)".
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        storage: DuplicateStorageArgs,
        /// Leave out mods the original records but no longer has in its mods
        /// folder.
        ///
        /// By default the copy records them too, so repairing it fetches them
        /// again. Use this when the original is mid-bisection and the copy
        /// should describe only what it actually has.
        #[arg(long)]
        drop_absent_mods: bool,
    },
    /// Show the pack recipe this instance retained when it was imported.
    ///
    /// Read from the instance's own `.launcher/source/`, never from the
    /// original archive. Server creation uses this retained recipe.
    Recipe {
        id: InstanceId,
        /// Which side to plan for. The server plan is what "Create Server"
        /// will use.
        #[arg(long, value_enum, default_value_t = SideArg::Client)]
        side: SideArg,
        /// Include the optional files that were not selected at import time.
        #[arg(long)]
        all_optional: bool,
    },
    /// Open one of an instance's folders in the desktop's file manager.
    Open {
        id: InstanceId,
        /// Which folder: root, minecraft, mods, config, logs, crash-reports,
        /// resourcepacks, shaderpacks, or saves.
        #[arg(long, default_value = "root", value_parser = parse_client_folder)]
        folder: ClientFolder,
        /// Print the path without handing it to a file manager.
        #[arg(long)]
        print: bool,
    },
    /// Show or change this instance's own Java and JVM settings.
    ///
    /// Each value falls back to the global setting until it is pinned here.
    /// With no options, prints what is pinned.
    Runtime {
        id: InstanceId,
        /// Use this Java executable instead of the selected one.
        #[arg(long, value_name = "PATH")]
        java: Option<PathBuf>,
        /// Minimum heap size in MiB.
        #[arg(long, value_name = "MIB")]
        min_memory: Option<u32>,
        /// Maximum heap size in MiB.
        #[arg(long, value_name = "MIB")]
        max_memory: Option<u32>,
        /// One extra JVM argument. Repeatable, and replaces the pinned list.
        ///
        /// `allow_hyphen_values` because every JVM argument begins with one.
        #[arg(long = "jvm-arg", value_name = "ARG", allow_hyphen_values = true)]
        jvm_args: Vec<String>,
        /// Remove every override and follow the global settings again.
        #[arg(
            long,
            conflicts_with_all = ["java", "min_memory", "max_memory", "jvm_args"]
        )]
        clear: bool,
    },
    /// Remove a client instance from the index.
    Delete {
        id: InstanceId,
        /// Leave the instance directory on disk.
        #[arg(long)]
        keep_files: bool,
    },
}

#[derive(Debug, Subcommand)]
enum LogsCommand {
    /// List the log or crash-report files that exist, newest first.
    List {
        #[command(flatten)]
        scope: DiagnosticScopeArgs,
        #[command(flatten)]
        kind: DiagnosticKindArgs,
    },
    /// Print part of one log or crash report.
    ///
    /// Logs are shown from their end and crash reports from their beginning,
    /// which is where each keeps the part worth reading first. Large files are
    /// read in a bounded window rather than whole.
    Show {
        /// The file name, as listed by `ephervex logs list`.
        name: String,
        #[command(flatten)]
        scope: DiagnosticScopeArgs,
        #[command(flatten)]
        kind: DiagnosticKindArgs,
        /// Read the beginning of the file rather than its end.
        #[arg(long)]
        head: bool,
        /// How many bytes to read.
        #[arg(long, default_value_t = DEFAULT_DIAGNOSTIC_BYTES)]
        bytes: u64,
        /// Print the file's full path instead of its contents.
        #[arg(long)]
        path: bool,
    },
    /// Find text in one log or crash report.
    ///
    /// Reads the same bounded window `show` does and searches it. The file is
    /// never modified.
    Search {
        /// The file name, as listed by `ephervex logs list`.
        name: String,
        /// The text to find.
        query: String,
        #[command(flatten)]
        scope: DiagnosticScopeArgs,
        #[command(flatten)]
        kind: DiagnosticKindArgs,
        /// Match capitalization exactly.
        #[arg(long)]
        case_sensitive: bool,
        /// Search the beginning of the file rather than its end.
        #[arg(long)]
        head: bool,
        /// How many bytes to search.
        #[arg(long, default_value_t = DEFAULT_DIAGNOSTIC_BYTES)]
        bytes: u64,
        /// Stop after this many matches.
        #[arg(long, default_value_t = MAX_SEARCH_MATCHES)]
        limit: usize,
    },
    /// Open the folder holding these files in the desktop's file manager.
    Open {
        #[command(flatten)]
        scope: DiagnosticScopeArgs,
        #[command(flatten)]
        kind: DiagnosticKindArgs,
        /// Print the path without handing it to a file manager.
        #[arg(long)]
        print: bool,
    },
}

/// Whose diagnostics to inspect. Exactly one is required.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct DiagnosticScopeArgs {
    /// A client instance's own logs and crash reports.
    #[arg(long)]
    instance: Option<InstanceId>,
    /// A server's own logs and crash reports.
    #[arg(long)]
    server: Option<ServerId>,
    /// Ephervex's own log.
    #[arg(long)]
    launcher: bool,
}

impl DiagnosticScopeArgs {
    fn scope(&self) -> anyhow::Result<DiagnosticScope> {
        match (self.instance, self.server, self.launcher) {
            (Some(instance), None, false) => Ok(DiagnosticScope::Client(instance)),
            (None, Some(server), false) => Ok(DiagnosticScope::Server(server)),
            (None, None, true) => Ok(DiagnosticScope::Launcher),
            // Unreachable while the argument group holds; refused rather than
            // guessed, because guessing would inspect the wrong instance.
            _ => anyhow::bail!("choose exactly one of --instance, --server, or --launcher"),
        }
    }
}

#[derive(Debug, Args)]
struct DiagnosticKindArgs {
    /// Work on crash reports rather than logs.
    #[arg(long)]
    crash_reports: bool,
}

impl DiagnosticKindArgs {
    const fn kind(&self) -> DiagnosticKind {
        if self.crash_reports {
            DiagnosticKind::CrashReport
        } else {
            DiagnosticKind::Log
        }
    }
}

#[derive(Debug, Subcommand)]
enum ServersCommand {
    /// List every indexed server.
    List,
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Create and fully install an exact server selection.
    Create(CreateServerArgs),
    /// Install the exact base for an existing uninstalled server record.
    Install {
        id: ServerId,
        #[arg(long)]
        java: Option<PathBuf>,
        /// Install this published server archive as the server's content.
        ///
        /// See `ephervex server create --help` for what the archive decides
        /// and what Ephervex still installs itself.
        #[arg(long = "server-pack", value_name = "FILE")]
        server_pack: Option<PathBuf>,
    },
    /// Show canonical server metadata.
    Show { id: ServerId },
    /// List the mods a server has, and whether the loader will load each one.
    ///
    /// A server reconstructed from a client pack carries whatever the pack
    /// declared, including mods that only ever run on a client. Use
    /// `ephervex mod disable --server` to leave one out without removing it.
    Mods { id: ServerId },
    /// Explain the newest crash and name the mods it points at.
    ///
    /// Reads the server's most recent crash report, and the tail of its log,
    /// and lists the mods they attribute the failure to, most confident first.
    /// Nothing is changed: use `ephervex mod disable --server` to act on it.
    Crash { id: ServerId },
    /// Show persistence, installation, runtime, and capture status.
    Status { id: ServerId },
    /// Read or change `server.properties` through managed services.
    Properties {
        #[command(subcommand)]
        command: ServerPropertiesCommand,
    },
    /// Change typed network settings.
    Network(ServerNetworkArgs),
    /// Inspect, explicitly accept, or revoke global Minecraft EULA consent.
    Eula {
        #[command(subcommand)]
        command: ServerEulaCommand,
    },
    /// Start in the foreground and stream server output.
    Start {
        id: ServerId,
        #[arg(long)]
        java: Option<PathBuf>,
    },
    /// Request a graceful stop and wait up to 30 seconds.
    Stop { id: ServerId },
    /// Gracefully stop and respawn through the live supervisor.
    Restart { id: ServerId },
    /// Force termination through the live owner channel.
    Kill { id: ServerId },
    /// Send one line to the live server console.
    Console { id: ServerId, command: String },
    /// Show retained combined launcher output.
    Output {
        id: ServerId,
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
    /// Inspect or conservatively clear runtime evidence.
    Recover {
        #[command(subcommand)]
        command: ServerRecoverCommand,
    },
    /// Open one of a server's folders in the desktop's file manager.
    Open {
        id: ServerId,
        /// Which folder: root, server, mods, config, logs, crash-reports, or
        /// world.
        #[arg(long, default_value = "root", value_parser = parse_server_folder)]
        folder: ServerFolder,
        /// Print the path without handing it to a file manager.
        #[arg(long)]
        print: bool,
    },
    /// Delete a server and, unless requested, its files.
    Delete {
        id: ServerId,
        /// Leave the server directory on disk.
        #[arg(long)]
        keep_files: bool,
    },
}

#[derive(Debug, Args)]
struct CreateInstanceArgs {
    /// Display name. Never used to build a path.
    name: String,
    /// Minecraft version id, e.g. `1.20.1`.
    #[arg(long)]
    minecraft: String,
    #[arg(long, value_enum, default_value_t = LoaderArg::Vanilla)]
    loader: LoaderArg,
    /// Exact loader build. Required for Forge and NeoForge.
    #[arg(long)]
    loader_version: Option<String>,
    #[command(flatten)]
    storage: StorageArgs,
}

#[derive(Debug, Args)]
struct CreateServerArgs {
    /// Display name. Never used to build a path.
    name: String,
    /// Exact Minecraft version for a standalone server.
    #[arg(long)]
    minecraft: Option<String>,
    /// Loader for a standalone server.
    #[arg(long, value_enum, default_value_t = LoaderArg::Vanilla)]
    loader: LoaderArg,
    /// Exact loader build. Required for Forge and NeoForge.
    #[arg(long)]
    loader_version: Option<String>,
    /// Derive the exact Minecraft and loader selection from this client.
    #[arg(
        long,
        conflicts_with_all = ["minecraft", "loader", "loader_version"]
    )]
    client: Option<InstanceId>,
    /// Use this Java executable for installation.
    #[arg(long)]
    java: Option<PathBuf>,
    /// Reconstruct only these files whose server applicability is undeclared.
    ///
    /// A linked server reconstructs every such file by default, because a pack
    /// format that declares no server side (CurseForge does not) would
    /// otherwise leave the server holding none of its pack. Naming paths here
    /// narrows that to the ones given. Repeatable, and matched against the
    /// retained recipe's logical paths.
    #[arg(
        long = "include-unknown",
        value_name = "PATH",
        requires = "client",
        conflicts_with = "skip_unknown"
    )]
    include_unknown: Vec<PathBuf>,
    /// Reconstruct none of the files whose server applicability is undeclared.
    #[arg(long = "skip-unknown", requires = "client")]
    skip_unknown: bool,
    /// Install this published server archive as the server's content.
    ///
    /// Many packs publish server files of their own. Those files are what the
    /// author decided the server contains, so they are copied in as they are:
    /// nothing is looked up, no mod list is planned, and no optional file is
    /// decided. Works for a standalone server and for one built from a client.
    ///
    /// Ephervex still installs the loader and its libraries, for every server,
    /// even when the archive ships them. An archive's installer jar and start
    /// script are never run, and its `libraries` folder and `eula.txt` are left
    /// out. Everything else in the archive is installed.
    #[arg(
        long = "server-pack",
        value_name = "FILE",
        conflicts_with_all = ["include_unknown", "skip_unknown"]
    )]
    server_pack: Option<PathBuf>,
    #[command(flatten)]
    storage: StorageArgs,
}

#[derive(Debug, Subcommand)]
enum ServerPropertiesCommand {
    /// Show the line-preserving properties document.
    Show { id: ServerId },
    /// Set an untyped property; managed network and EULA keys are refused.
    Set {
        id: ServerId,
        key: String,
        value: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NetworkAccessArg {
    Localhost,
    Lan,
    Custom,
}

#[derive(Debug, Args)]
struct ServerNetworkArgs {
    id: ServerId,
    /// Require authenticated Minecraft accounts.
    #[arg(long)]
    online_mode: bool,
    /// Network exposure for the server listener.
    #[arg(long, value_enum, default_value_t = NetworkAccessArg::Localhost)]
    access: NetworkAccessArg,
    /// TCP port; zero is invalid.
    #[arg(long, default_value_t = 25565)]
    port: u16,
    /// IP address required by `--access custom`.
    #[arg(long)]
    address: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ServerEulaCommand {
    /// Show global Minecraft EULA consent status.
    Status,
    /// Review and explicitly accept in an interactive terminal.
    Accept,
    /// Revoke current global consent.
    Revoke,
}

#[derive(Debug, Subcommand)]
enum ServerRecoverCommand {
    /// Classify recorded supervisor and Java-child identities.
    Status { id: ServerId },
    /// Clear runtime evidence only after every recorded process is proven dead.
    Clear { id: ServerId },
}

/// Temporary versus persistent, expressed as two mutually exclusive flags.
///
/// Neither flag means temporary, because that is the product's defining default.
/// `--temporary` lets a script state the choice explicitly
/// rather than relying on the default staying put.
#[derive(Debug, Args)]
#[group(multiple = false)]
struct StorageArgs {
    /// Store under the OS temporary directory (the default).
    #[arg(long)]
    temporary: bool,
    /// Store under the persistent instance root.
    #[arg(long)]
    persistent: bool,
}

impl StorageArgs {
    const fn class(&self) -> StorageClass {
        if self.persistent {
            StorageClass::Persistent
        } else {
            StorageClass::Temporary
        }
    }
}

/// The same choice for a duplicate, where "neither flag" means something else.
///
/// A duplicate defaults to the *original's* storage class rather than to
/// temporary: copying a persistent instance and silently getting a disposable
/// one back would be a surprise, so callers can choose either class explicitly.
#[derive(Debug, Args)]
#[group(multiple = false)]
struct DuplicateStorageArgs {
    /// Store the copy under the OS temporary directory.
    #[arg(long)]
    temporary: bool,
    /// Store the copy under the persistent instance root.
    #[arg(long)]
    persistent: bool,
}

impl DuplicateStorageArgs {
    const fn class(&self) -> Option<StorageClass> {
        match (self.temporary, self.persistent) {
            (true, _) => Some(StorageClass::Temporary),
            (_, true) => Some(StorageClass::Persistent),
            _ => None,
        }
    }
}

/// Parses a folder name, listing the alternatives when it is not one.
///
/// The names come from `ephervex-core`'s own enums rather than from a copy
/// here, so a folder added by another subsystem cannot go missing from the CLI.
fn parse_client_folder(name: &str) -> Result<ClientFolder, String> {
    ClientFolder::parse(name).ok_or_else(|| {
        format!(
            "expected one of: {}",
            join_names(ClientFolder::ALL.iter().map(|folder| folder.as_str()))
        )
    })
}

fn parse_server_folder(name: &str) -> Result<ServerFolder, String> {
    ServerFolder::parse(name).ok_or_else(|| {
        format!(
            "expected one of: {}",
            join_names(ServerFolder::ALL.iter().map(|folder| folder.as_str()))
        )
    })
}

fn parse_preference_key(name: &str) -> Result<PreferenceKey, String> {
    PreferenceKey::parse(name).ok_or_else(|| {
        format!(
            "expected one of: {}",
            join_names(PreferenceKey::ALL.iter().map(|key| key.as_str()))
        )
    })
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

/// Which side of a pack a command is about.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum SideArg {
    Client,
    Server,
}

impl From<SideArg> for PackSide {
    fn from(value: SideArg) -> Self {
        match value {
            SideArg::Client => Self::Client,
            SideArg::Server => Self::Server,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LoaderArg {
    Vanilla,
    Forge,
    Neoforge,
}

impl From<LoaderArg> for LoaderKind {
    fn from(value: LoaderArg) -> Self {
        match value {
            LoaderArg::Vanilla => Self::Vanilla,
            LoaderArg::Forge => Self::Forge,
            LoaderArg::Neoforge => Self::NeoForge,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Completions { shell } = &cli.command {
        generate_completions(*shell);
        return ExitCode::SUCCESS;
    }
    // Resolved before application work so the launcher's own log file exists
    // from the first event. Completion generation returns before application
    // logging is initialized so it does not create app logs or load config.
    let launcher_logs = Paths::from_environment()
        .ok()
        .map(|paths| paths.launcher_logs_dir());
    ephervex_core::init_tracing(
        ephervex_core::DEFAULT_LOG_DIRECTIVES,
        launcher_logs.as_deref(),
    );

    // Before any install: a session that starts this from a desktop entry gives
    // it a far lower limit than the same session's terminal does.
    match ephervex_core::open_files::raise_limit() {
        Ok(limit) => tracing::debug!(limit, "open file limit"),
        Err(error) => tracing::warn!(%error, "could not raise the open file limit"),
    }

    // Not routed through `run`, whose contract is "an error means the command
    // did not do its job". `doctor` always does its job; what varies is what it
    // found, and that is what its exit status reports.
    if matches!(cli.command, Command::Doctor) {
        return run_doctor(cli.json);
    }

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_failure(&error, cli.json);
            ExitCode::FAILURE
        }
    }
}

fn generate_completions(shell: Shell) {
    let mut command = Cli::command();
    let binary_name = command.get_name().to_owned();
    clap_complete::generate(shell, &mut command, binary_name, &mut std::io::stdout());
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Info => {
            show_info(cli.json);
            Ok(())
        }
        Command::Paths => run_paths(cli.json),
        // Both are handled in `main`: completions before application logging is
        // initialized, and `doctor` because its report decides the exit status.
        Command::Completions { .. } | Command::Doctor => Ok(()),
        Command::Config { command } => match command {
            None | Some(ConfigCommand::Show) => {
                let paths = Paths::from_environment()?;
                show_config(&Config::load_or_default(&paths)?, &paths, cli.json)
            }
            Some(ConfigCommand::InstanceRoot { path, clear }) => {
                let mut app = open_app()?;
                run_instance_root(&mut app, path.as_deref(), *clear, cli.json)
            }
        },
        Command::Preferences { command } => {
            let mut app = open_app()?;
            run_preferences_command(&mut app, command, cli.json)
        }
        Command::Instances { command } => {
            let app = open_app()?;
            match command {
                InstancesCommand::List => {
                    list_instances(&app, cli.json)?;
                }
            }
            Ok(())
        }
        Command::Instance { command } => {
            let app = open_app()?;
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_instance_command(&app, command, cli.json))
        }
        Command::Servers { command } => {
            let app = open_app()?;
            match command {
                ServersCommand::List => list_servers(&app, cli.json)?,
            }
            Ok(())
        }
        Command::Server { command } => {
            let mut app = open_app()?;
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_server_command(&mut app, command, cli.json))
        }
        Command::Minecraft { command } => {
            let app = open_app()?;
            // A runtime is built here rather than wrapping `main` in
            // `#[tokio::main]`, so the commands that only touch SQLite and the
            // filesystem keep running on a plain thread with no executor.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_minecraft_command(&app, command, cli.json))
        }
        Command::Java { command } => {
            let app = open_app()?;
            // Probing forks a JVM per candidate and the managed provider
            // downloads; both need an executor, for the same reason the
            // Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_java_command(&app, command, cli.json))
        }
        Command::Account { command } => {
            let mut app = open_app()?;
            // Signing in waits on a browser or a poll loop; both need an
            // executor, for the same reason the Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_account_command(&mut app, command, cli.json))
        }
        Command::Api { command } => {
            let mut app = open_app()?;
            run_api_command(&mut app, command, cli.json)
        }
        Command::Mod { command } => {
            let mut app = open_app()?;
            // All but the catalog setting talk to a provider, so they need an
            // executor, for the same reason the Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_mod_command(&mut app, command, cli.json))
        }
        Command::Logs { command } => {
            // Reading a file and listing a directory need no executor.
            let app = open_app()?;
            run_logs_command(&app, command, cli.json)
        }
        Command::Import(args) => {
            let app = open_app()?;
            if args.inspect {
                // Analysis touches nothing and needs no executor.
                return run_inspect(&app, args, cli.json);
            }
            // Downloading a pack's files needs one, for the same reason the
            // Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_import(&app, args, cli.json))
        }
        Command::Launch(args) => {
            let mut app = open_app()?;
            // Downloading, spawning, and streaming all need an executor, for
            // the same reason the Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_launch(&mut app, args, cli.json))
        }
        Command::Running => print_running(&open_app()?, cli.json),
        Command::Reconcile => {
            let app = open_app()?;
            let report = app.reconcile()?;
            show_reconciliation(&report, cli.json);
            Ok(())
        }
    }
}

/// Opens configuration and the registry, and selects the configured language.
///
/// This is the one place the language setting is applied, so every command
/// below it can call `ephervex_i18n::lookup` without worrying about ordering.
fn open_app() -> Result<App, CoreError> {
    let app = App::open()?;

    if let Err(error) = ephervex_i18n::select_language(&app.config().language) {
        // A language that is not bundled is worth saying out loud, but it is
        // not a reason to refuse to list instances.
        tracing::warn!(
            requested = %app.config().language,
            %error,
            "keeping English; the configured language is not available"
        );
    }

    Ok(app)
}

// ------------------------------------------------------------------- commands

async fn run_instance_command(
    app: &App,
    command: &InstanceCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        InstanceCommand::Create(args) => {
            let loader = build_loader(args.loader, args.loader_version.as_deref())?;
            let created = app
                .create_client_reporting(
                    NewClient::new(&args.name, &args.minecraft, loader)
                        .with_storage(args.storage.class()),
                )
                .await?;
            print_created_instance(app, &created, json)?;
        }
        InstanceCommand::Show { id } => {
            let record = app.client(*id)?;
            print_instance(app, &record, json)?;
        }
        InstanceCommand::Duplicate {
            id,
            name,
            storage,
            drop_absent_mods,
        } => {
            // Both fields default to "the same as the original", which is what
            // `ephervex instance duplicate <id>` with no flags should mean.
            let request = DuplicateClient {
                name: name.clone(),
                storage: storage.class(),
            };
            let absent = if *drop_absent_mods {
                AbsentModPolicy::Drop
            } else {
                AbsentModPolicy::Keep
            };
            let copy = app.duplicate_client(*id, &request, absent)?;
            print_instance(app, &copy.record, json)?;
            if !json {
                eprintln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "instance-duplicated",
                        [("name", MessageArg::from(copy.record.name.as_str()))]
                    )
                );
            }
            report_skipped_links(&copy.skipped_links, json);
        }
        InstanceCommand::Recipe {
            id,
            side,
            all_optional,
        } => {
            let Some(recipe) = app.pack_recipe(*id)? else {
                // Not an error: an instance created by hand has no pack.
                if json {
                    print_json(&serde_json::json!({ "recipe": serde_json::Value::Null }));
                } else {
                    println!("{}", ephervex_i18n::lookup("pack-recipe-missing"));
                }
                return Ok(());
            };
            let selection = if *all_optional {
                OptionalSelection::all()
            } else {
                OptionalSelection::none()
            };
            print_recipe(&recipe, (*side).into(), &selection, json);
        }
        InstanceCommand::Open { id, folder, print } => {
            let path = if *print {
                app.client_folder(*id, *folder)?
            } else {
                app.open_client_folder(*id, *folder)?
            };
            print_folder(path.as_deref(), folder.as_str(), json)?;
        }
        InstanceCommand::Runtime { .. } => run_instance_runtime(app, command, json)?,
        InstanceCommand::Delete { id, keep_files } => {
            let files = if *keep_files {
                FileDisposition::Keep
            } else {
                FileDisposition::Remove
            };
            app.delete_client(*id, files)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "deleted": id.to_string(),
                        "files": if *keep_files { "kept" } else { "removed" },
                    }))?
                );
            } else {
                println!("deleted {id}");
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_server_command(
    app: &mut App,
    command: &ServerCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        ServerCommand::Create(args) => {
            let request = if let Some(client) = args.client {
                NewServer::linked(&args.name, client).with_storage(args.storage.class())
            } else {
                let minecraft = args.minecraft.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("--minecraft is required for a standalone server")
                })?;
                let loader = build_loader(args.loader, args.loader_version.as_deref())?;
                NewServer::new(&args.name, minecraft, loader).with_storage(args.storage.class())
            };
            let committed = app
                .create_server(
                    request,
                    &ServerCreateOptions {
                        java_path: args.java.clone(),
                        install_java_if_missing: true,
                        unknown_server_files: unknown_server_files(
                            &args.include_unknown,
                            args.skip_unknown,
                        ),
                        content: server_content(args.server_pack.as_deref()),
                        reporter: None,
                    },
                    &CancellationToken::new(),
                )
                .await?;
            print_server_commit(&committed.record, &committed.warnings, json);
        }
        ServerCommand::Install {
            id,
            java,
            server_pack,
        } => {
            let committed = app
                .install_server(
                    *id,
                    &ServerCreateOptions {
                        java_path: java.clone(),
                        install_java_if_missing: true,
                        unknown_server_files: UnknownServerFiles::default(),
                        content: server_content(server_pack.as_deref()),
                        reporter: None,
                    },
                    &CancellationToken::new(),
                )
                .await?;
            print_server_commit(&committed.record, &committed.warnings, json);
        }
        ServerCommand::Show { id } => {
            if json {
                print_json(&server_status_json(app, *id)?);
            } else {
                print_server(&app.server(*id)?, false);
            }
        }
        ServerCommand::Mods { id } => {
            print_host_mods(&app.server_mod_snapshot(*id)?, json);
        }
        ServerCommand::Crash { id } => {
            print_crash_review(app.crash_review(ModHost::Server(*id))?.as_ref(), json);
        }
        ServerCommand::Status { id } => print_server_status(app, *id, json)?,
        ServerCommand::Properties { command } => match command {
            ServerPropertiesCommand::Show { id } => {
                let text = app.server_properties(*id)?.render();
                if json {
                    print_json(&serde_json::json!({ "server_id": id, "properties": text }));
                } else {
                    print!("{text}");
                }
            }
            ServerPropertiesCommand::Set { id, key, value } => {
                app.set_server_property(*id, key, value)?;
                if json {
                    print_json(&serde_json::json!({ "server_id": id, "key": key, "value": value }));
                } else {
                    println!("set {key}");
                }
            }
        },
        ServerCommand::Network(args) => {
            let access = match args.access {
                NetworkAccessArg::Localhost => NetworkAccess::Localhost,
                NetworkAccessArg::Lan => NetworkAccess::Lan,
                NetworkAccessArg::Custom => NetworkAccess::Custom,
            };
            let committed = app.set_server_network(
                args.id,
                ServerNetworkSettings {
                    online_mode: args.online_mode,
                    access,
                    port: args.port,
                    bind_address: args.address.clone(),
                },
            )?;
            print_server_commit(&committed.record, &committed.warnings, json);
        }
        ServerCommand::Eula { command } => match command {
            ServerEulaCommand::Status => print_eula_status(&app.server_eula_status(), json),
            ServerEulaCommand::Accept => {
                if json || !std::io::stdin().is_terminal() {
                    anyhow::bail!(
                        "EULA acceptance requires an interactive terminal; review {MINECRAFT_EULA_REFERENCE}"
                    );
                }
                println!("Review the Minecraft EULA: {MINECRAFT_EULA_REFERENCE}");
                print!("Type {EULA_CONFIRMATION_PHRASE} to accept: ");
                std::io::stdout().flush()?;
                let mut phrase = String::new();
                std::io::stdin().read_line(&mut phrase)?;
                if phrase.trim_end() != EULA_CONFIRMATION_PHRASE {
                    anyhow::bail!("EULA consent was not recorded");
                }
                let status = app.accept_server_eula()?;
                print_eula_status(&status, false);
            }
            ServerEulaCommand::Revoke => {
                app.revoke_server_eula()?;
                print_eula_status(&app.server_eula_status(), json);
            }
        },
        ServerCommand::Start { id, java } => {
            run_server_foreground(app, *id, java.clone(), json).await?;
        }
        ServerCommand::Stop { id } => {
            print_control_response(app.stop_server(*id).await?, json);
        }
        ServerCommand::Restart { id } => {
            print_control_response(app.restart_server(*id).await?, json);
        }
        ServerCommand::Kill { id } => {
            print_control_response(app.kill_server(*id).await?, json);
        }
        ServerCommand::Console { id, command } => {
            print_control_response(app.send_server_console(*id, command.clone()).await?, json);
        }
        ServerCommand::Output { id, lines } => {
            let snapshot = app.server_output(*id, *lines)?;
            if json {
                print_json(&serde_json::json!({
                    "server_id": id,
                    "capture_health": format!("{:?}", snapshot.health),
                    "retention": {
                        "removed_segments": snapshot.retention.removed_segments,
                        "removed_bytes": snapshot.retention.removed_bytes,
                        "removed_lines": snapshot.retention.removed_lines,
                        "earliest_retained_at": snapshot.retention.earliest_retained_at,
                    },
                    "lines": snapshot.lines.iter().map(captured_line_json).collect::<Vec<_>>(),
                }));
            } else {
                for line in snapshot.lines {
                    print_captured_line(&line);
                }
            }
        }
        ServerCommand::Recover { command } => match command {
            ServerRecoverCommand::Status { id } => print_server_status(app, *id, json)?,
            ServerRecoverCommand::Clear { id } => {
                app.recover_server_runtime(*id)?;
                if json {
                    print_json(&serde_json::json!({ "server_id": id, "cleared": true }));
                } else {
                    println!("cleared proven-stale runtime state for {id}");
                }
            }
        },
        ServerCommand::Open { id, folder, print } => {
            let path = if *print {
                app.server_folder(*id, *folder)?
            } else {
                app.open_server_folder(*id, *folder)?
            };
            print_folder(path.as_deref(), folder.as_str(), json)?;
        }
        ServerCommand::Delete { id, keep_files } => {
            let files = if *keep_files {
                FileDisposition::Keep
            } else {
                FileDisposition::Remove
            };
            app.delete_server(*id, files)?;
            if json {
                print_json(&serde_json::json!({
                    "deleted": id.to_string(),
                    "files": if *keep_files { "kept" } else { "removed" },
                }));
            } else {
                println!("deleted {id}");
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- diagnostics

/// Logs and crash reports through the CLI.
///
/// Every command names a file by the name the listing showed and never by a
/// path: which directory that name is resolved in, and whether it may be opened
/// at all, are decisions `ephervex-core` makes. That is also what makes the
/// same commands work for a client, a server, and the launcher itself without
/// this file knowing where any of them keep anything.
fn run_logs_command(app: &App, command: &LogsCommand, json: bool) -> anyhow::Result<()> {
    match command {
        LogsCommand::List { scope, kind } => {
            let entries = app.diagnostic_entries(scope.scope()?, kind.kind())?;
            print_diagnostic_entries(&entries, json);
        }
        LogsCommand::Show {
            name,
            scope,
            kind,
            head,
            bytes,
            path,
        } => {
            let scope = scope.scope()?;
            let kind = kind.kind();
            if *path {
                let resolved = app.diagnostic_path(scope, kind, name)?;
                if json {
                    print_json(&serde_json::json!({ "name": name, "path": resolved }));
                } else {
                    println!("{}", resolved.display());
                }
                return Ok(());
            }

            let view = app.read_diagnostic(&request(scope, kind, name, *head, *bytes))?;
            if json {
                print_json(&serde_json::json!({
                    "name": name,
                    "path": view.path,
                    "anchor": view.anchor.as_str(),
                    "file_bytes": view.file_bytes,
                    "returned_bytes": view.returned_bytes,
                    "truncated": view.truncated,
                    "lossy": view.lossy,
                    "read_at": view.read_at,
                    "text": view.text,
                }));
            } else {
                print!("{}", view.text);
                std::io::stdout().flush()?;
                report_window(&view);
            }
        }
        LogsCommand::Search {
            name,
            query,
            scope,
            kind,
            case_sensitive,
            head,
            bytes,
            limit,
        } => {
            let found = app.search_diagnostic(
                &request(scope.scope()?, kind.kind(), name, *head, *bytes),
                query,
                &SearchOptions {
                    case_sensitive: *case_sensitive,
                    limit: *limit,
                },
            )?;
            print_diagnostic_search(&found, json)?;
        }
        LogsCommand::Open { scope, kind, print } => {
            let scope = scope.scope()?;
            let kind = kind.kind();
            let path = if *print {
                app.diagnostic_folder(scope, kind)?
            } else {
                app.open_diagnostic_folder(scope, kind)?
            };
            print_folder(path.as_deref(), kind.as_str(), json)?;
        }
    }
    Ok(())
}

/// Builds the read request the show and search commands share.
///
/// `--head` reads the beginning; without it each kind's natural end is used.
fn request(
    scope: DiagnosticScope,
    kind: DiagnosticKind,
    name: &str,
    head: bool,
    bytes: u64,
) -> DiagnosticRequest<'_> {
    let request = DiagnosticRequest::new(scope, kind, name).with_budget(bytes);
    if head {
        request.with_anchor(TextAnchor::Head)
    } else {
        request
    }
}

fn print_diagnostic_entries(entries: &[DiagnosticEntry], json: bool) {
    if json {
        print_json(&serde_json::Value::Array(
            entries.iter().map(diagnostic_entry_json).collect(),
        ));
        return;
    }
    if entries.is_empty() {
        println!("{}", ephervex_i18n::lookup("diagnostics-nothing-recorded"));
        return;
    }
    for entry in entries {
        let modified = entry.modified.map(|at| at.to_rfc3339()).unwrap_or_default();
        let headline = entry
            .summary
            .as_ref()
            .and_then(ephervex_core::CrashSummary::headline)
            .unwrap_or_default();
        println!(
            "{}\t{}\t{}\t{}",
            entry.name, entry.size_bytes, modified, headline
        );
    }
}

fn diagnostic_entry_json(entry: &DiagnosticEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "path": entry.path,
        "kind": entry.kind.as_str(),
        "size_bytes": entry.size_bytes,
        "modified": entry.modified,
        "summary": entry.summary.as_ref().map(|summary| serde_json::json!({
            "comment": summary.comment,
            "reported_at": summary.reported_at,
            "description": summary.description,
            "failure": summary.failure,
            "excerpt": summary.excerpt,
        })),
    })
}

fn print_diagnostic_search(found: &DiagnosticSearch, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&serde_json::json!({
            "path": found.view.path,
            "lines_searched": found.outcome.lines_searched,
            "truncated": found.outcome.truncated,
            "matches": found.outcome.matches.iter().map(|found| serde_json::json!({
                "line_number": found.line_number,
                "line": found.line,
                "line_truncated": found.line_truncated,
                "start": found.start,
                "end": found.end,
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    for matched in &found.outcome.matches {
        println!("{}:{}", matched.line_number, matched.line);
    }
    std::io::stdout().flush()?;
    if found.outcome.matches.is_empty() {
        eprintln!("{}", ephervex_i18n::lookup("diagnostics-search-no-matches"));
    } else if found.outcome.truncated {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "diagnostics-search-truncated",
                [(
                    "matches",
                    MessageArg::from(
                        i64::try_from(found.outcome.matches.len()).unwrap_or(i64::MAX)
                    )
                )]
            )
        );
    }
    report_window(&found.view);
    Ok(())
}

/// Says what part of the file was actually shown.
///
/// On stderr because the text itself is data: `ephervex logs show` can be piped
/// into a pager or a file without the commentary landing in it.
fn report_window(view: &TextView) {
    if view.truncated {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "diagnostics-window-truncated",
                [
                    (
                        "returned",
                        MessageArg::from(i64::try_from(view.returned_bytes).unwrap_or(i64::MAX))
                    ),
                    (
                        "total",
                        MessageArg::from(i64::try_from(view.file_bytes).unwrap_or(i64::MAX))
                    ),
                ]
            )
        );
    }
    if view.lossy {
        eprintln!("{}", ephervex_i18n::lookup("diagnostics-window-lossy"));
    }
}

/// Shows the id that ties what the user is watching to the launcher log.
fn print_operation_id(operation: OperationId) {
    eprintln!(
        "{}",
        ephervex_i18n::lookup_args(
            "operation-identifier",
            [("operation", MessageArg::from(operation.to_string()))]
        )
    );
}

// ---------------------------------------------------------------- preferences

/// Global Minecraft preferences through the CLI.
///
/// Everything here goes through `ephervex-core`: the validation, the canonical
/// value, and the accepted syntax all come from the same place the GUI will
/// read them from, so the two frontends cannot disagree about what a valid FOV
/// is.
fn run_preferences_command(
    app: &mut App,
    command: &PreferencesCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        PreferencesCommand::Show => {
            show_preferences(app, json);
            Ok(())
        }
        PreferencesCommand::Set { preference, value } => {
            let stored = app.set_minecraft_preference(*preference, value)?;
            if json {
                print_json(&serde_json::json!({
                    "preference": preference.as_str(),
                    "managed": true,
                    "value": stored.to_setting_value(),
                }));
            } else {
                println!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "preference-set",
                        [
                            ("preference", MessageArg::from(preference.as_str())),
                            ("value", MessageArg::from(stored.to_setting_value())),
                        ]
                    )
                );
                // Preferences apply once during creation. Existing instances
                // remain unchanged.
                eprintln!(
                    "applies to instances created from now on; existing instances own their own settings"
                );
            }
            Ok(())
        }
        PreferencesCommand::Unmanage { preference } => {
            app.unmanage_minecraft_preference(*preference)?;
            if json {
                print_json(&serde_json::json!({
                    "preference": preference.as_str(),
                    "managed": false,
                }));
            } else {
                println!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "preference-unmanaged",
                        [("preference", MessageArg::from(preference.as_str()))]
                    )
                );
            }
            Ok(())
        }
    }
}

fn show_preferences(app: &App, json: bool) {
    let loaded = app.minecraft_preferences();
    let preferences = &loaded.preferences;

    if json {
        print_json(&serde_json::json!({
                    "preferences": PreferenceKey::ALL
        .iter()
        .map(|key| serde_json::json!({
                            "preference": key.as_str(),
                            "managed": preferences.is_managed(*key),
                            "value": preferences.value(*key).to_setting_value(),
                            "syntax": key.value_syntax(),
                        }))
        .collect::<Vec<_>>(),
                    "rejected": loaded
        .rejected
        .iter()
        .map(|error| serde_json::json!({
                            "preference": error.key.as_str(),
                            "value": error.value,
                            "syntax": error.syntax,
                        }))
        .collect::<Vec<_>>(),
                }));
        return;
    }

    for key in PreferenceKey::ALL {
        println!(
            "{:<20} {:<9} {:<12} {}",
            key.as_str(),
            if preferences.is_managed(*key) {
                "applied"
            } else {
                "ignored"
            },
            preferences.value(*key).to_setting_value(),
            key.value_syntax()
        );
    }

    let managed = preferences.managed_keys().len();
    if managed == 0 {
        println!("{}", ephervex_i18n::lookup("preferences-none-managed"));
    } else {
        println!(
            "{}",
            ephervex_i18n::lookup_args(
                "preferences-managed-count",
                [("count", MessageArg::from(managed))]
            )
        );
    }

    // A value that could not be parsed is not applied to anything, so it has
    // to be visible rather than merely absent.
    for error in &loaded.rejected {
        eprintln!("{error}");
    }
}

/// Shows or moves the persistent instance root.
fn run_instance_root(
    app: &mut App,
    path: Option<&Path>,
    clear: bool,
    json: bool,
) -> anyhow::Result<()> {
    if path.is_some() || clear {
        app.set_persistent_instance_root(path)?;
    }

    let root = app.paths().persistent_instances_dir();
    let overridden = app.paths().persistent_instance_root_is_overridden();
    if json {
        print_json(&serde_json::json!({
            "persistent_instance_root": root,
            "overridden": overridden,
        }));
    } else {
        println!("{}", root.display());
        if !overridden {
            eprintln!("the default, under the XDG data directory");
        }
        eprintln!("instances that already exist stay where they are");
    }
    Ok(())
}

// --------------------------------------------------------------------- launch

/// Installs and launches an instance, streaming the game's log.
///
/// Two streams, as everywhere else in this file: the game's own output is
/// **data** and goes to stdout, so it can be piped or redirected to a file;
/// everything the launcher has to say about the launch is commentary and goes
/// to stderr. With `--json` the game's output becomes one JSON object per line,
/// which is what makes `ephervex launch … --json | jq` work while the game is
/// still running.
async fn run_launch(app: &mut App, args: &LaunchArgs, json: bool) -> anyhow::Result<()> {
    let mut features = LaunchFeatures::none();
    if args.demo {
        features = features.demo();
    }
    if let Some(resolution) = &args.resolution {
        let (width, height) = parse_resolution(resolution)?;
        features = features.custom_resolution(width, height);
    }

    let options = LaunchOptions {
        features,
        java_path: args.java_path.clone(),
        install_java_if_missing: args.install_java,
        account: args.account.clone(),
        additional: args.alongside.clone(),
        dry_run: args.dry_run,
    };

    let (observer, mut events) = LaunchObserver::channel();
    let cancel = CancellationToken::new();

    // Ctrl-C stops the game rather than orphaning it. The task ends with the
    // process, so nothing has to shut it down.
    let interrupt = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("stopping Minecraft…");
            interrupt.cancel();
        }
    });

    // Drained on its own task so a long download reports progress while the
    // launch is still in `app.launch`.
    let printer = tokio::spawn(async move {
        let mut progress = LaunchProgressPrinter::default();
        while let Some(event) = events.recv().await {
            progress.print(&event, json);
        }
    });

    let connected = if let Some(server) = args.server {
        let (connect_observer, mut connect_events) = LaunchAndConnectObserver::channel();
        let connect_printer = tokio::spawn(async move {
            while let Some(event) = connect_events.recv().await {
                print_connect_event(event);
            }
        });
        let outcome = app
            .launch_and_connect(
                args.id,
                server,
                &LaunchAndConnectOptions {
                    launch: options.clone(),
                    ..LaunchAndConnectOptions::default()
                },
                &connect_observer,
                &ServerObserver::silent(),
                &observer,
                &cancel,
            )
            .await;
        drop(connect_observer);
        let _ = connect_printer.await;
        Some(outcome)
    } else {
        None
    };
    let outcome = match connected {
        Some(outcome) => outcome.map(ConnectedLaunch::Linked),
        None => app
            .launch(args.id, &options, &observer, &cancel)
            .await
            .map(ConnectedLaunch::Client),
    };
    // The printer ends when every sender is gone, so this drop is what lets it
    // finish rather than hang.
    drop(observer);
    let _ = printer.await;

    match outcome? {
        ConnectedLaunch::Client(outcome) => report_launch(&outcome, json),
        ConnectedLaunch::Linked(outcome) => report_connected_launch(&outcome, json),
    }
    Ok(())
}

enum ConnectedLaunch {
    Client(LaunchOutcome),
    Linked(LaunchAndConnectOutcome),
}

fn print_connect_event(event: LaunchAndConnectEvent) {
    match event {
        LaunchAndConnectEvent::StartingServer { server } => {
            eprintln!(
                "{}",
                ephervex_i18n::lookup_args(
                    "launch-connect-starting-server",
                    [("server", MessageArg::from(server.to_string()))],
                )
            );
        }
        LaunchAndConnectEvent::ReusingServer { server } => {
            eprintln!(
                "{}",
                ephervex_i18n::lookup_args(
                    "launch-connect-reusing-server",
                    [("server", MessageArg::from(server.to_string()))],
                )
            );
        }
        LaunchAndConnectEvent::WaitingForServer { target } => {
            eprintln!(
                "{}",
                ephervex_i18n::lookup_args(
                    "launch-connect-waiting",
                    [("target", MessageArg::from(target.to_string()))],
                )
            );
        }
        LaunchAndConnectEvent::ServerReady { target } => {
            eprintln!(
                "{}",
                ephervex_i18n::lookup_args(
                    "launch-connect-ready",
                    [("target", MessageArg::from(target.to_string()))],
                )
            );
        }
        _ => {}
    }
}

fn report_connected_launch(outcome: &LaunchAndConnectOutcome, json: bool) {
    if json {
        let value = serde_json::json!({
            "operation": outcome.launch.operation.to_string(),
            "instance": outcome.client.to_string(),
            "server": outcome.server.to_string(),
            "connection_target": outcome.target.to_string(),
            "server_started": outcome.server_started,
            "minecraft_version": outcome.launch.minecraft_version,
            "java": outcome.launch.java.runtime.executable,
            "account": outcome.launch.account,
            "authenticated": outcome.launch.authenticated,
            "command": outcome.launch.command.redacted_arguments(),
            "dry_run": outcome.launch.exit.is_none(),
            "exit_code": outcome.launch.exit.and_then(|exit| exit.code),
            "exit_signal": outcome.launch.exit.and_then(|exit| exit.signal),
        });
        print_json(&value);
    } else {
        report_launch(&outcome.launch, false);
    }
}

/// Renders launch events, keeping download progress to one line per decile.
///
/// The reporter already throttles to ten emissions a second, which is right for
/// a progress bar and far too much for a terminal that may be a log file. A
/// decile is enough to see that something is happening and few enough to read.
///
/// The *step* is deliberately not what resets the counter: inside a batch the
/// step is the file currently being fetched, so resetting on it would print a
/// line per asset — four and a half thousand of them on a modern release. A
/// fraction that goes *backwards* is what marks a new batch.
#[derive(Debug, Default)]
struct LaunchProgressPrinter {
    last_decile: Option<u64>,
}

impl LaunchProgressPrinter {
    fn print(&mut self, event: &LaunchEvent, json: bool) {
        match event {
            LaunchEvent::Preparing { operation } => {
                eprintln!("{}", ephervex_i18n::lookup("launch-preparing"));
                print_operation_id(*operation);
            }
            LaunchEvent::Downloading(operation) => self.print_progress(operation),
            LaunchEvent::Starting(command) => {
                eprintln!("{}", ephervex_i18n::lookup("launch-starting"));
                // Redacted, always: one of these arguments is an access token
                tracing::debug!(command = %command, "launching");
            }
            LaunchEvent::OutputLine(line) => print_game_line(line, json),
            LaunchEvent::Exited { code, signal } => eprintln!("{}", exit_message(*code, *signal)),
            LaunchEvent::Failed { detail, .. } => tracing::debug!(%detail, "launch failed"),
            // A variant added below this build is not a reason to stop
            // streaming a game that is running perfectly well.
            _ => {}
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "a decile boundary, not a measurement"
    )]
    fn print_progress(&mut self, operation: &ephervex_model::Operation) {
        let Some(fraction) = operation.progress.fraction() else {
            return;
        };
        // A decile. Computed by comparison rather than by casting a float,
        // because a `f64 as u64` that clippy cannot prove is in range is not
        // worth an allow for arithmetic this simple.
        let Some(decile) = (0..=10u64)
            .rev()
            .find(|step| fraction * 10.0 >= *step as f64)
        else {
            return;
        };
        match self.last_decile {
            Some(last) if decile > last => {}
            // A fraction that dropped means the next batch started.
            Some(last) if decile < last => {}
            Some(_) => return,
            None => {}
        }
        self.last_decile = Some(decile);
        // The title is a message id, not English.
        eprintln!(
            "  {} {}%",
            ephervex_i18n::lookup(&operation.title),
            decile * 10
        );
    }
}

/// One line of the game's output.
fn print_game_line(line: &OutputLine, json: bool) {
    if json {
        let value = serde_json::json!({
            "stream": line.stream.as_str(),
            "level": line.record.level,
            "logger": line.record.logger,
            "thread": line.record.thread,
            "message": line.record.message,
        });
        if let Ok(text) = serde_json::to_string(&value) {
            println!("{text}");
        }
    } else {
        // Minecraft's own log text is never translated.
        println!("{}", line.display());
    }
}

fn exit_message(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(code), _) if code != 0 => ephervex_i18n::lookup_args(
            "launch-exited-with-code",
            [("code", MessageArg::from(i64::from(code)))],
        ),
        (None, Some(signal)) => ephervex_i18n::lookup_args(
            "launch-exited-with-signal",
            [("signal", MessageArg::from(i64::from(signal)))],
        ),
        // Exit code 0, or a process that reported neither: the game closed.
        _ => ephervex_i18n::lookup("launch-exited-cleanly"),
    }
}

/// The summary printed once the game has exited.
fn report_launch(outcome: &LaunchOutcome, json: bool) {
    if json {
        let value = serde_json::json!({
            "operation": outcome.operation.to_string(),
            "instance": outcome.instance.to_string(),
            "minecraft_version": outcome.minecraft_version,
            "java": outcome.java.runtime.executable,
            "account": outcome.account,
            "authenticated": outcome.authenticated,
            "classpath_entries": outcome.installation.plan.classpath().len(),
            "natives_directory": outcome.installation.plan.natives_dir,
            "assets": outcome.installation.assets,
            // Never the real arguments: one of them is an access token.
            "command": outcome.command.redacted_arguments(),
            "dry_run": outcome.exit.is_none(),
            "exit_code": outcome.exit.and_then(|exit| exit.code),
            "exit_signal": outcome.exit.and_then(|exit| exit.signal),
        });
        print_json(&value);
        return;
    }

    if outcome.exit.is_none() {
        eprintln!("{}", ephervex_i18n::lookup("launch-dry-run"));
        println!("{}", outcome.command.java.display());
        for argument in outcome.command.redacted_arguments() {
            println!("  {argument}");
        }
        eprintln!("{}", ephervex_i18n::lookup("launch-command-redacted"));
    }
}

/// Describes a launch that is currently holding an instance.
fn describe_running(state: &LaunchState) -> String {
    match state.game_pid {
        Some(pid) => format!("minecraft pid {pid}, launched by {}", state.owner_pid),
        None => format!("preparing, launcher pid {}", state.owner_pid),
    }
}

/// Parses `--resolution 1280x720`.
fn parse_resolution(value: &str) -> anyhow::Result<(u32, u32)> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| anyhow::anyhow!("expected WIDTHxHEIGHT, e.g. 1280x720"))?;
    Ok((width.trim().parse()?, height.trim().parse()?))
}

/// Builds a loader selection, rejecting an incoherent one up front.
///
/// A modded loader without a version cannot be reproduced faithfully, and
/// guessing one would be exactly the silent substitution Ephervex does not allow.
/// How a `server create` invocation spells its policy for files whose server
/// applicability the pack never declared.
fn unknown_server_files(include: &[PathBuf], skip: bool) -> UnknownServerFiles {
    if skip {
        UnknownServerFiles::only(OptionalSelection::none())
    } else if include.is_empty() {
        UnknownServerFiles::IncludeAll
    } else {
        UnknownServerFiles::only(OptionalSelection::of(include.to_vec()))
    }
}

/// Where a `server create` or `server install` invocation takes its content
/// from.
fn server_content(archive: Option<&Path>) -> ServerContentSource {
    archive.map_or(ServerContentSource::Recipe, |path| {
        ServerContentSource::Archive {
            path: path.to_path_buf(),
        }
    })
}

fn build_loader(loader: LoaderArg, version: Option<&str>) -> anyhow::Result<LoaderSelection> {
    let kind = LoaderKind::from(loader);
    let selection = LoaderSelection {
        kind,
        version: version.map(str::to_owned),
    };

    if selection.is_coherent() {
        Ok(selection)
    } else if kind.requires_version() {
        anyhow::bail!("{kind} needs an exact build; pass --loader-version")
    } else {
        anyhow::bail!("{kind} takes no loader version")
    }
}

fn show_info(json: bool) {
    if json {
        let info = serde_json::json!({
            "name": ephervex_model::APP_NAME,
            "app_id": ephervex_model::APP_ID,
            "version": ephervex_model::APP_VERSION,
            "user_agent": ephervex_model::user_agent(),
            "language": ephervex_i18n::current_language().to_string(),
        });
        print_json(&info);
    } else {
        println!(
            "{} {}",
            ephervex_model::APP_NAME,
            ephervex_model::APP_VERSION
        );
        println!("app id:     {}", ephervex_model::APP_ID);
        println!("user agent: {}", ephervex_model::user_agent());
    }
}

fn show_paths(paths: &Paths, json: bool) {
    if json {
        let value = serde_json::json!({
            "config": paths.config_dir(),
            "config_file": paths.config_file(),
            "data": paths.data_dir(),
            "cache": paths.cache_dir(),
            "state": paths.state_dir(),
            "database": paths.database_file(),
            "logs": paths.logs_dir(),
            "temp": paths.temp_dir(),
            "temporary_instances": paths.temporary_instances_dir(),
            "persistent_instances": paths.persistent_instances_dir(),
            "persistent_servers": paths.persistent_servers_dir(),
            "persistent_instance_root_overridden": paths.persistent_instance_root_is_overridden(),
        });
        print_json(&value);
    } else {
        println!("config:               {}", paths.config_dir().display());
        println!("data:                 {}", paths.data_dir().display());
        println!("cache:                {}", paths.cache_dir().display());
        println!("state:                {}", paths.state_dir().display());
        println!("database:             {}", paths.database_file().display());
        println!(
            "temporary instances:  {}",
            paths.temporary_instances_dir().display()
        );
        println!(
            "persistent instances: {}{}",
            paths.persistent_instances_dir().display(),
            if paths.persistent_instance_root_is_overridden() {
                "  (configured override)"
            } else {
                ""
            }
        );
    }
}

/// Renders `ephervex doctor`.
///
/// Field labels are plain text, matching `info` and `paths`: this is a
/// technical report about one machine, and its rows are the same identifiers
/// the JSON form uses. Anything that is a *failure* still carries the localized
/// message the rest of the interface would show.
fn show_health(health: &InstallationHealth, json: bool) {
    if json {
        print_json(&health_json(health));
        return;
    }

    println!("{} {}", health.application.name, health.application.version);
    println!("app id:     {}", health.application.app_id);
    println!(
        "platform:   {}-{}",
        health.platform.os, health.platform.arch
    );
    println!("language:   {}", health.application.language);

    show_health_directories(health);
    show_health_configuration(health.configuration.as_ref());
    show_health_credentials(&health.credential_storage);

    println!();
    println!("bundled data");
    show_bundled_document(
        "options compatibility",
        &health.bundled_data.options_dataset,
    );
    show_bundled_document(
        "preference catalog",
        &health.bundled_data.preference_catalog,
    );

    show_health_artifact_cache(health.artifact_cache.as_ref());
    show_health_java(health.java.as_ref());

    if let Some(error) = &health.storage_error {
        println!();
        println!("storage");
        println!("  could not be opened: {error}");
    }
}

/// `ephervex paths`.
///
/// Resolved without opening storage, so this still works when the database or
/// the configuration is the thing that is broken.
fn run_paths(json: bool) -> anyhow::Result<()> {
    let paths = Paths::from_environment()?;
    let config = Config::load_or_default(&paths)?;
    show_paths(&config.apply_to(paths), json);
    Ok(())
}

/// `ephervex doctor`.
///
/// Deliberately not `open_app()`: a broken installation is exactly what this
/// reports, so it must not need one that works.
fn run_doctor(json: bool) -> ExitCode {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            report_failure(&error.into(), json);
            return ExitCode::FAILURE;
        }
    };
    let health = runtime.block_on(ephervex_core::installation_health());
    show_health(&health, json);
    // The report is the output and the exit status is its verdict, so a script
    // can gate on `ephervex doctor` without parsing anything. A fault is not an
    // error here: the report is still printed in full, and no failure banner is
    // added to it.
    if health.has_fault() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn show_health_directories(health: &InstallationHealth) {
    println!();
    println!("directories");
    if let Some(error) = &health.paths_error {
        println!("  could not be resolved: {error}");
    }
    for directory in &health.directories {
        let state = match &directory.state {
            DirectoryState::Absent => "missing (created on first use)".to_owned(),
            DirectoryState::Writable => "writable".to_owned(),
            DirectoryState::NotWritable { detail } => format!("NOT WRITABLE: {detail}"),
            DirectoryState::Unusable { detail } => format!("UNUSABLE: {detail}"),
        };
        println!(
            "  {:<21} {}  [{state}]",
            directory.purpose,
            directory.path.display()
        );
    }
}

fn show_health_configuration(configuration: Option<&ConfigurationHealth>) {
    println!();
    println!("configuration");
    let Some(configuration) = configuration else {
        println!("  could not be located: the storage directories did not resolve");
        return;
    };
    match configuration {
        ConfigurationHealth::Absent { path } => {
            println!("  {}  [not written yet]", path.display());
        }
        ConfigurationHealth::Loaded {
            path,
            schema_version,
        } => {
            println!("  {}  [schema {schema_version}]", path.display());
        }
        ConfigurationHealth::Unusable {
            path,
            message_id,
            detail,
        } => {
            println!("  {}  [UNUSABLE]", path.display());
            println!("  {}", ephervex_i18n::lookup(message_id));
            println!("  {detail}");
        }
    }
}

fn show_health_credentials(credentials: &CredentialBackendHealth) {
    println!();
    println!("credential storage");
    if credentials.available {
        println!("  {}  [available]", credentials.backend);
    } else {
        println!("  unavailable; sign-in is session-only for this run");
        if let Some(detail) = &credentials.detail {
            println!("  {detail}");
        }
    }
}

fn show_health_artifact_cache(cache: Option<&ArtifactCacheHealth>) {
    println!();
    println!("artifact cache");
    match cache {
        None => println!("  not measured; storage could not be opened"),
        Some(ArtifactCacheHealth::Unavailable { detail }) => {
            println!("  not measured: {detail}");
        }
        Some(ArtifactCacheHealth::Measured {
            used_bytes,
            entries,
            complete,
            limit,
            over_budget,
        }) => {
            println!("  {entries} entries, {used_bytes} bytes, budget {limit}");
            if !complete {
                println!("  the cache was in use; part of it was not measured");
            }
            if *over_budget {
                println!("  over budget; collection runs after the next cache operation");
            }
        }
    }
}

fn show_health_java(java: Option<&JavaHealth>) {
    println!();
    println!("java");
    match java {
        None => println!("  not measured: storage could not be opened"),
        Some(JavaHealth::Unavailable { detail }) => {
            // Never "no runtimes found": the scan did not finish, which is a
            // different thing to tell somebody than that they have no JDK.
            println!("  not measured: {detail}");
        }
        Some(JavaHealth::Discovered {
            discovered,
            managed,
            major_versions,
        }) => {
            println!("  {discovered} runtimes ({managed} managed)");
            if !major_versions.is_empty() {
                let versions: Vec<String> =
                    major_versions.iter().map(ToString::to_string).collect();
                println!("  major versions: {}", versions.join(", "));
            }
        }
    }
}

fn show_bundled_document(label: &str, document: &BundledDocumentHealth) {
    match document {
        BundledDocumentHealth::Loaded { entries, revision } => match revision {
            Some(revision) => println!("  {label:<22} {entries} entries, revision {revision}"),
            None => println!("  {label:<22} {entries} entries"),
        },
        BundledDocumentHealth::Unusable { detail } => {
            println!("  {label:<22} UNUSABLE: {detail}");
        }
    }
}

fn health_json(health: &InstallationHealth) -> serde_json::Value {
    serde_json::json!({
        "application": {
            "name": health.application.name,
            "version": health.application.version,
            "app_id": health.application.app_id,
            "user_agent": health.application.user_agent,
            "language": health.application.language,
        },
        "platform": {
            "os": health.platform.os,
            "arch": health.platform.arch,
            "family": health.platform.family,
        },
        "paths_error": health.paths_error,
        "directories": health
            .directories
            .iter()
            .map(|directory| {
                let (state, detail) = match &directory.state {
                    DirectoryState::Absent => ("absent", None),
                    DirectoryState::Writable => ("writable", None),
                    DirectoryState::NotWritable { detail } => ("not_writable", Some(detail)),
                    DirectoryState::Unusable { detail } => ("unusable", Some(detail)),
                };
                serde_json::json!({
                    "purpose": directory.purpose,
                    "path": directory.path,
                    "state": state,
                    "detail": detail,
                })
            })
            .collect::<Vec<_>>(),
        "configuration": match &health.configuration {
            None => serde_json::Value::Null,
            Some(ConfigurationHealth::Absent { path }) => serde_json::json!({
                "status": "absent",
                "path": path,
            }),
            Some(ConfigurationHealth::Loaded { path, schema_version }) => serde_json::json!({
                "status": "loaded",
                "path": path,
                "schema_version": schema_version,
            }),
            Some(ConfigurationHealth::Unusable { path, message_id, detail }) => serde_json::json!({
                "status": "unusable",
                "path": path,
                "message_id": message_id,
                "detail": detail,
            }),
        },
        "credential_storage": {
            "available": health.credential_storage.available,
            "backend": health.credential_storage.backend,
            "detail": health.credential_storage.detail,
        },
        "bundled_data": {
            "options_compatibility": bundled_document_json(&health.bundled_data.options_dataset),
            "preference_catalog": bundled_document_json(&health.bundled_data.preference_catalog),
        },
        "artifact_cache": match &health.artifact_cache {
            None => serde_json::Value::Null,
            Some(ArtifactCacheHealth::Unavailable { detail }) => serde_json::json!({
                "status": "unavailable",
                "detail": detail,
            }),
            Some(ArtifactCacheHealth::Measured {
                used_bytes,
                entries,
                complete,
                limit,
                over_budget,
            }) => serde_json::json!({
                "status": "measured",
                "used_bytes": used_bytes,
                "entries": entries,
                "complete": complete,
                "limit": limit.to_string(),
                "over_budget": over_budget,
            }),
        },
        "java": match &health.java {
            None => serde_json::Value::Null,
            Some(JavaHealth::Unavailable { detail }) => serde_json::json!({
                "status": "unavailable",
                "detail": detail,
            }),
            Some(JavaHealth::Discovered { discovered, managed, major_versions }) => {
                serde_json::json!({
                    "status": "discovered",
                    "discovered": discovered,
                    "managed": managed,
                    "major_versions": major_versions,
                })
            }
        },
        "storage_error": health.storage_error,
    })
}

fn bundled_document_json(document: &BundledDocumentHealth) -> serde_json::Value {
    match document {
        BundledDocumentHealth::Loaded { entries, revision } => serde_json::json!({
            "status": "loaded",
            "entries": entries,
            "revision": revision,
        }),
        BundledDocumentHealth::Unusable { detail } => serde_json::json!({
            "status": "unusable",
            "detail": detail,
        }),
    }
}

fn show_config(config: &Config, paths: &Paths, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&serde_json::json!({
            "path": paths.config_file(),
            "exists": paths.config_file().is_file(),
            "schema_version": config.schema_version,
            "language": config.language,
            "persistent_instance_root": config.persistent_instance_root,
        }));
    } else {
        println!("# {}", paths.config_file().display());
        // The serializer refuses to emit a credential-looking key, so this
        // cannot print a secret even if one somehow reached the struct
        // (ADR 0004).
        print!("{}", config.to_toml_string()?);
    }
    Ok(())
}

fn list_instances(app: &App, json: bool) -> anyhow::Result<()> {
    let records = app.list_clients()?;

    if json {
        let values: Vec<_> = records
            .iter()
            .map(|record| instance_json(app, record))
            .collect::<Result<_, _>>()?;
        print_json(&serde_json::Value::Array(values));
        return Ok(());
    }

    if records.is_empty() {
        println!("{}", ephervex_i18n::lookup("instances-list-empty"));
        return Ok(());
    }

    for record in &records {
        // The summary message already carries the name, so there is no separate
        // name column: one source of that string, not two.
        println!(
            "{}  {:<11} {}{}",
            record.id,
            record.storage.to_string(),
            summarize(record),
            if record.needs_attention() {
                "  [needs attention]"
            } else {
                ""
            }
        );
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "instances-list-count",
            [("count", MessageArg::from(records.len()))]
        )
    );
    Ok(())
}

fn list_servers(app: &App, json: bool) -> anyhow::Result<()> {
    let records = app.list_servers()?;

    if json {
        let values = records
            .iter()
            .map(|record| server_status_json(app, record.id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        print_json(&serde_json::Value::Array(values));
        return Ok(());
    }

    if records.is_empty() {
        println!("{}", ephervex_i18n::lookup("servers-list-empty"));
        return Ok(());
    }

    for record in &records {
        println!(
            "{}  {:<11} {}",
            record.id,
            record.storage.to_string(),
            ephervex_i18n::lookup_args(
                "instance-summary",
                [
                    ("name", MessageArg::from(record.name.as_str())),
                    (
                        "version",
                        MessageArg::from(record.minecraft_version.as_str()),
                    ),
                    ("loader", MessageArg::from(record.loader.to_string())),
                ],
            )
        );
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "servers-list-count",
            [("count", MessageArg::from(records.len()))]
        )
    );
    Ok(())
}

// ------------------------------------------------------------------- mods

async fn run_mod_command(app: &mut App, command: &ModCommand, json: bool) -> anyhow::Result<()> {
    match command {
        ModCommand::Search {
            instance,
            query,
            provider,
            limit,
            offset,
            include_incompatible,
        } => {
            let query = ModSearchQuery::new(query)
                .limited(*limit)
                .from_offset(*offset)
                .including_incompatible(*include_incompatible);
            let catalog = catalog_or_preference(app, *provider)?;
            let results = app.search_mods(*instance, catalog, &query).await?;
            print_mod_search(&results, json);
        }
        ModCommand::Show {
            instance,
            project,
            provider,
            include_incompatible,
        } => {
            let catalog = catalog_or_preference(app, *provider)?;
            let details = app
                .mod_project(*instance, catalog, project, *include_incompatible)
                .await?;
            print_mod_project(&details, json);
        }
        ModCommand::Plan {
            instance,
            project,
            selection,
        } => {
            let catalog = catalog_or_preference(app, selection.provider)?;
            let plan = app
                .plan_mod_installation(*instance, &selection.request(project, catalog))
                .await?;
            print_mod_plan(&plan, json);
        }
        ModCommand::Install {
            instance,
            project,
            selection,
        } => {
            let catalog = catalog_or_preference(app, selection.provider)?;
            let installed = app
                .install_mods(
                    *instance,
                    &selection.request(project, catalog),
                    &OperationReporter::silent("installing-mods"),
                    &CancellationToken::new(),
                )
                .await?;
            print_installed_mods(&installed, json);
        }
        ModCommand::Catalog { catalog, clear } => {
            run_mod_catalog(app, *catalog, *clear, json)?;
        }
        ModCommand::List { instance } => {
            let mods = app.managed_mods(*instance)?;
            print_managed_mods(&mods, json);
        }
        ModCommand::Enable { host, path } => {
            print_mod_state(app.set_mod_enabled(host.host()?, path, true)?, path, json);
        }
        ModCommand::Disable { host, path } => {
            print_mod_state(app.set_mod_enabled(host.host()?, path, false)?, path, json);
        }
    }
    Ok(())
}

/// Reports what a mod's file is now, after being enabled or disabled.
fn print_mod_state(presence: ModPresence, path: &Path, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "path": path,
            "presence": presence.as_str(),
            "enabled": presence.is_enabled(),
        }));
        return;
    }
    println!("{}  {}", presence.as_str(), path.display());
}

/// Shows or records which catalog mod commands use when none is named.
///
/// Nothing here is asked of a catalog: the setting is which one to look in, so
/// it is answerable with no network and with no key stored. A name this build
/// cannot resolve is reported as exactly that and left in the file, because the
/// Ephervex that wrote it knows a catalog this one does not.
fn run_mod_catalog(
    app: &mut App,
    catalog: Option<CatalogArg>,
    clear: bool,
    json: bool,
) -> anyhow::Result<()> {
    if catalog.is_some() || clear {
        app.set_default_mod_catalog(catalog.map(CatalogArg::provider))?;
    }

    let preference = app.default_mod_catalog();
    let effective = preference.resolved();
    if json {
        print_json(&serde_json::json!({
            "catalog": effective.map(ModProvider::as_str),
            "configured": preference.stored(),
            "chosen": preference.is_chosen(),
            "recognized": effective.is_some(),
        }));
    } else {
        match effective {
            Some(provider) => println!("{}", provider.as_str()),
            None => println!("{}", preference.stored()),
        }
        if !preference.is_chosen() {
            eprintln!(
                "the default, used because no catalog is recorded: {}",
                DefaultModCatalog::BUILT_IN.as_str()
            );
        } else if effective.is_none() {
            eprintln!("not a catalog this version knows; the recorded value is kept");
        }
    }
    Ok(())
}

/// Why something does not fit, as untranslated technical detail plus a
/// localized sentence.
fn incompatibility_text(reason: &IncompatibilityReason) -> String {
    match reason {
        IncompatibilityReason::MinecraftVersion { required, .. } => ephervex_i18n::lookup_args(
            reason.message_id(),
            [("required", MessageArg::from(required.as_str()))],
        ),
        IncompatibilityReason::Loader { required, .. } => ephervex_i18n::lookup_args(
            reason.message_id(),
            [("required", MessageArg::from(required.to_string()))],
        ),
    }
}

fn incompatibility_json(reason: &IncompatibilityReason) -> serde_json::Value {
    match reason {
        IncompatibilityReason::MinecraftVersion {
            required,
            supported,
        } => serde_json::json!({
            "reason": reason.as_str(),
            "required": required,
            "supported": supported,
        }),
        IncompatibilityReason::Loader {
            required,
            supported,
        } => serde_json::json!({
            "reason": reason.as_str(),
            "required": required.to_string(),
            "supported": supported,
        }),
    }
}

fn compatibility_json(compatibility: &Compatibility) -> serde_json::Value {
    serde_json::json!({
        "compatible": compatibility.is_compatible(),
        "reasons": compatibility
            .reasons()
            .iter()
            .map(incompatibility_json)
            .collect::<Vec<_>>(),
    })
}

fn print_mod_search(results: &ModSearchResults, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "provider": results.provider.as_str(),
            "total_hits": results.total_hits,
            "offset": results.offset,
            "hits": results.hits.iter().map(|hit| serde_json::json!({
                "project_id": hit.project.id,
                "slug": hit.project.slug,
                "title": hit.project.title,
                "description": hit.project.description,
                "author": hit.project.author,
                "client_side": hit.project.client_side.to_string(),
                "server_side": hit.project.server_side.to_string(),
                "best_version": hit.best_version.as_ref().map(|version| serde_json::json!({
                    "id": version.id,
                    "version_number": version.version_number,
                    "file_name": version.file.file_name,
                })),
                "compatibility": compatibility_json(&hit.compatibility),
            })).collect::<Vec<_>>(),
        }));
        return;
    }

    if results.hits.is_empty() {
        println!("{}", ephervex_i18n::lookup("mod-search-empty"));
        if let Some(action) = ephervex_i18n::lookup_attribute("mod-search-empty", "action") {
            println!("  {action}");
        }
        return;
    }

    for hit in &results.hits {
        println!("{}  {}", hit.project.id, hit.project.title);
        println!("  {}", hit.project.description);
        match &hit.best_version {
            Some(version) => println!("  version: {} ({})", version.version_number, version.id),
            None => {
                for reason in hit.compatibility.reasons() {
                    println!("  {}", incompatibility_text(reason));
                }
            }
        }
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "mod-search-count",
            [("count", MessageArg::from(results.hits.len()))]
        )
    );
}

fn print_mod_project(details: &ModProjectDetails, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "project_id": details.project.id,
            "slug": details.project.slug,
            "title": details.project.title,
            "description": details.project.description,
            "client_side": details.project.client_side.to_string(),
            "server_side": details.project.server_side.to_string(),
            "default_version": details.default_version,
            "versions": details.versions.iter().map(|entry| serde_json::json!({
                "id": entry.version.id,
                "name": entry.version.name,
                "version_number": entry.version.version_number,
                "channel": entry.version.channel.as_str(),
                "game_versions": entry.version.game_versions,
                "loaders": entry.version.loaders,
                "published_at": entry.version.published_at.to_rfc3339(),
                "file_name": entry.version.file.file_name,
                "compatibility": compatibility_json(&entry.compatibility),
                "default": Some(&entry.version.id) == details.default_version.as_ref(),
            })).collect::<Vec<_>>(),
        }));
        return;
    }

    println!("{}  {}", details.project.id, details.project.title);
    println!("  {}", details.project.description);
    println!(
        "  client: {}  server: {}",
        details.project.client_side, details.project.server_side
    );
    for entry in &details.versions {
        let marker = if Some(&entry.version.id) == details.default_version.as_ref() {
            "*"
        } else {
            " "
        };
        println!(
            "{marker} {}  {}  ({})",
            entry.version.id, entry.version.version_number, entry.version.file.file_name
        );
        for reason in entry.compatibility.reasons() {
            println!("    {}", incompatibility_text(reason));
        }
    }
}

fn planned_json(planned: &PlannedMod) -> serde_json::Value {
    serde_json::json!({
        "project_id": planned.version.project_id,
        "title": planned.project.title,
        "version_id": planned.version.id,
        "version_number": planned.version.version_number,
        "file_name": planned.version.file.file_name,
        "origin": planned.origin.as_str(),
        "requested_by": planned.requested_by,
        "server_side": planned.project.server_side.to_string(),
    })
}

fn warning_text(warning: &PlanWarning) -> String {
    match warning {
        PlanWarning::ClientSideUnsupported { project_id }
        | PlanWarning::OptionalNotOffered { project_id } => ephervex_i18n::lookup_args(
            warning.message_id(),
            [("project", MessageArg::from(project_id.as_str()))],
        ),
        PlanWarning::UncheckableConflict {
            declared_by,
            file_name,
        } => ephervex_i18n::lookup_args(
            warning.message_id(),
            [
                ("declared_by", MessageArg::from(declared_by.as_str())),
                ("file_name", MessageArg::from(file_name.as_str())),
            ],
        ),
    }
}

fn print_mod_plan(plan: &ModInstallPlan, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "provider": plan.provider.as_str(),
            "root": planned_json(&plan.root),
            "dependencies": plan.dependencies.iter().map(planned_json).collect::<Vec<_>>(),
            "optional": plan.optional.iter().map(|candidate| serde_json::json!({
                "project_id": candidate.project_id,
                "declared_by": candidate.declared_by,
                "selected": false,
                "version_id": candidate.version.as_ref().map(|version| version.id.clone()),
                "compatibility": compatibility_json(&candidate.compatibility),
            })).collect::<Vec<_>>(),
            "external": plan.external.iter().map(|external| serde_json::json!({
                "kind": external.kind.as_str(),
                "file_name": external.file_name,
                "declared_by": external.declared_by,
            })).collect::<Vec<_>>(),
            "embedded": plan.embedded.iter().map(|embedded| serde_json::json!({
                "project_id": embedded.project_id,
                "file_name": embedded.file_name,
                "declared_by": embedded.declared_by,
            })).collect::<Vec<_>>(),
            "already_installed": plan.already_installed.iter().map(|installed| serde_json::json!({
                "project_id": installed.project_id,
                "version_id": installed.version_id,
            })).collect::<Vec<_>>(),
            "warnings": plan.warnings.iter().map(warning_text).collect::<Vec<_>>(),
        }));
        return;
    }

    println!(
        "{}  {}  ({})",
        plan.root.version.project_id, plan.root.project.title, plan.root.version.version_number
    );
    for planned in &plan.dependencies {
        println!(
            "  + {}  {}  ({})",
            planned.version.project_id, planned.project.title, planned.version.version_number
        );
    }
    for candidate in &plan.optional {
        // Unchecked by default, and shown as such rather than hidden.
        println!("  [ ] {} (optional)", candidate.project_id);
    }
    for external in &plan.external {
        println!("  ? {} ({})", external.file_name, external.kind.as_str());
    }
    for installed in &plan.already_installed {
        println!("  = {} (already installed)", installed.project_id);
    }
    for warning in &plan.warnings {
        eprintln!("{}", warning_text(warning));
    }
}

fn print_installed_mods(installed: &InstalledMods, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "installed": installed.installation.installed,
            "repaired": installed.installation.repaired,
            "already_present": installed.installation.already_present,
            "reused_from_cache": installed.installation.reused_from_cache,
            "bytes": installed.installation.bytes,
            "plan": {
                "root": planned_json(&installed.plan.root),
                "dependencies": installed.plan.dependencies.iter().map(planned_json).collect::<Vec<_>>(),
            },
        }));
        return;
    }

    if installed.installation.changed_anything() {
        println!(
            "{}",
            ephervex_i18n::lookup_args(
                "mod-install-completed",
                [(
                    "name",
                    MessageArg::from(installed.plan.root.project.title.as_str())
                )]
            )
        );
        for path in &installed.installation.installed {
            println!("  {}", path.display());
        }
    } else {
        println!("{}", ephervex_i18n::lookup("mod-install-nothing-to-do"));
    }
    for warning in &installed.plan.warnings {
        eprintln!("{}", warning_text(warning));
    }
}

/// Lists one host's recorded mods and whether each one loads.
///
/// Wider than [`print_managed_mods`], which lists only what a resolver can
/// re-address. A server's list has to include mods with no provider identity,
/// because a jar the pack shipped and the loader chokes on is exactly the one
/// the reader is looking for.
fn print_host_mods(snapshot: &ManagedModSnapshot, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "host": snapshot.host.id(),
            "host_kind": snapshot.host.kind(),
            "mods": snapshot.mods.iter().map(|entry| serde_json::json!({
                "path": entry.logical_path,
                "file_name": entry.file_name,
                "name": entry.display_name(),
                "version": entry.installed_version(),
                "origin": entry.origin.as_str(),
                "presence": entry.presence.as_str(),
                "enabled": entry.presence.is_enabled(),
            })).collect::<Vec<_>>(),
        }));
        return;
    }

    if snapshot.is_empty() {
        println!("{}", ephervex_i18n::lookup("mod-list-empty"));
        return;
    }
    for entry in &snapshot.mods {
        println!(
            "{}  {}  ({})",
            entry.presence.as_str(),
            entry.display_name(),
            entry.logical_path.display()
        );
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "mod-installed-count",
            [("count", MessageArg::from(snapshot.len()))]
        )
    );
}

/// What a crash report says about which mods to try without.
///
/// Suspects are printed even when the host has no row for them, because a jar
/// dropped into `mods/` by hand is still worth naming; the missing path is what
/// says the mods list cannot switch it off.
fn print_crash_review(review: Option<&CrashReview>, json: bool) {
    let Some(review) = review else {
        if json {
            print_json(&serde_json::json!({ "crash": serde_json::Value::Null }));
        } else {
            println!("{}", ephervex_i18n::lookup("crash-none"));
        }
        return;
    };

    if json {
        print_json(&serde_json::json!({
            "report": review.report_name,
            "reported_at": review.reported_at,
            "failure": review.analysis.failure,
            "missing_class": review.analysis.missing_class,
            "client_code_on_server": review.analysis.client_code_on_server,
            "suspects": review.mods.iter().map(|entry| serde_json::json!({
                "mod_id": entry.suspect.mod_id,
                "name": entry.suspect.label(),
                "jar": entry.suspect.jar,
                "path": entry.logical_path,
                "presence": entry.presence.map(ModPresence::as_str),
                "reason": entry.suspect.reason.as_str(),
                "triggered_by": match &entry.suspect.reason {
                    SuspectReason::CompatTrigger { triggered_by } => Some(triggered_by),
                    _ => None,
                },
                "confidence": entry.suspect.confidence().as_str(),
                "suggested": entry.is_actionable()
                    && entry.suspect.confidence() > SuspectConfidence::Low,
            })).collect::<Vec<_>>(),
        }));
        return;
    }

    println!("{}", review.report_name);
    if let Some(failure) = &review.analysis.failure {
        println!("  {failure}");
    }
    if review.analysis.client_code_on_server {
        println!("  {}", ephervex_i18n::lookup("crash-client-code-on-server"));
    }
    if review.mods.is_empty() {
        println!("{}", ephervex_i18n::lookup("crash-no-suspects"));
        return;
    }
    for entry in &review.mods {
        let reason = match &entry.suspect.reason {
            SuspectReason::Named => ephervex_i18n::lookup("crash-reason-named"),
            SuspectReason::InFailingFrames => {
                ephervex_i18n::lookup("crash-reason-in-failing-frames")
            }
            SuspectReason::CompatTrigger { triggered_by } => ephervex_i18n::lookup_args(
                "crash-reason-compat-trigger",
                [("mod", MessageArg::from(triggered_by.as_str()))],
            ),
        };
        println!(
            "{}  {}  {reason}",
            entry.suspect.confidence().as_str(),
            entry.suspect.label()
        );
        match &entry.logical_path {
            Some(path) => println!("    {}", path.display()),
            None => println!("    {}", ephervex_i18n::lookup("crash-not-in-mod-list")),
        }
    }
}

fn print_managed_mods(mods: &[ManagedMod], json: bool) {
    if json {
        print_json(&serde_json::json!({
            "mods": mods.iter().map(|managed| serde_json::json!({
                "provider": managed.provider.as_str(),
                "project_id": managed.project_id,
                "version_id": managed.version_id,
                "file_name": managed.file_name,
                "origin": managed.origin.as_str(),
                "hashes": managed.hashes.iter().map(ToString::to_string).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }));
        return;
    }

    if mods.is_empty() {
        println!("{}", ephervex_i18n::lookup("mod-list-empty"));
        return;
    }
    for managed in mods {
        println!(
            "{}  {}  {}  ({})",
            managed.provider, managed.project_id, managed.version_id, managed.file_name
        );
        println!("  {}  {}", managed.origin.as_str(), managed.file_name);
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "mod-installed-count",
            [("count", MessageArg::from(mods.len()))]
        )
    );
}

async fn run_minecraft_command(
    app: &App,
    command: &MinecraftCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        MinecraftCommand::Versions { limit } => {
            let listing = app.minecraft_releases().await?;
            show_releases(&listing, *limit, json);
        }
        MinecraftCommand::Version { id } => {
            let version = app.minecraft_version(id).await?;
            show_version(&version, json)?;
        }
    }
    Ok(())
}

async fn run_java_command(app: &App, command: &JavaCommand, json: bool) -> anyhow::Result<()> {
    match command {
        JavaCommand::List => show_runtimes(app.java_runtimes().await?.as_ref(), json),
        JavaCommand::Refresh => {
            show_runtimes(app.refresh_java_runtimes().await?.as_ref(), json);
        }
        JavaCommand::Test { path } => {
            let runtime = app.test_java_runtime(path).await?;
            show_runtime_detail(&runtime, json);
        }
        JavaCommand::Select {
            minecraft,
            instance,
            java_path,
            install,
        } => {
            let plan = app
                .plan_java(
                    minecraft,
                    &JavaPlanRequest {
                        target: instance.map_or(JavaTarget::VersionOnly, JavaTarget::Client),
                        explicit_path: java_path.clone(),
                        install_if_missing: *install,
                    },
                )
                .await?;
            show_java_plan(&plan, json);
        }
        JavaCommand::Install { major } => {
            let installed = app.install_java(*major).await?;
            show_managed_install(&installed, json);
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- accounts

async fn run_account_command(
    app: &mut App,
    command: &AccountCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        // The four that only touch configuration and the keyring are handled
        // together and synchronously; only signing in needs an executor.
        AccountCommand::List
        | AccountCommand::AddOffline { .. }
        | AccountCommand::Use { .. }
        | AccountCommand::Remove { .. } => run_account_management(app, command, json)?,
        AccountCommand::Login {
            device_code,
            no_open,
            session_only,
            timeout,
        } => {
            let options = LoginOptions {
                request: LoginRequest {
                    flow: if *device_code {
                        LoginFlow::DeviceCode
                    } else {
                        LoginFlow::Browser
                    },
                    timeout: std::time::Duration::from_secs(*timeout),
                },
                allow_session_only: *session_only,
            };

            // The prompts arrive *while* the login is running, so they are
            // drained on a separate task rather than after the await.
            let (observer, receiver) = AuthObserver::channel();
            let prompts = tokio::spawn(show_login_prompts(receiver, !*no_open));

            let result = app
                .login(&options, &observer, &CancellationToken::new())
                .await;
            // Dropping the observer closes the channel so the printer finishes.
            drop(observer);
            let _ = prompts.await;

            let session = result?;
            let signed_in = ephervex_i18n::lookup_args(
                "auth-signed-in",
                [("name", MessageArg::from(session.account.name.as_str()))],
            );
            let session_only = !session
                .storage()
                .is_some_and(CredentialStorage::is_persistent);
            if json {
                print_json(&session_json(session));
            } else {
                println!("{signed_in}");
                println!("uuid:     {}", session.account.id);
                println!(
                    "storage:  {}",
                    session.storage().map_or_else(
                        || ephervex_i18n::lookup("auth-storage-none-needed"),
                        |storage| ephervex_i18n::lookup(storage.message_id()),
                    )
                );
            }
            if session_only {
                eprintln!("{}", ephervex_i18n::lookup("auth-session-only-warning"));
            }
        }
        AccountCommand::Status => {
            let status = app.account_status()?;
            show_account_status(&status, json);
        }
        AccountCommand::Refresh => {
            let session = app
                .resume_account(&AuthObserver::silent(), &CancellationToken::new())
                .await?;
            if json {
                print_json(&session_json(session));
            } else {
                println!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "auth-signed-in",
                        [("name", MessageArg::from(session.account.name.as_str()))],
                    )
                );
                if let Some(expires) = session.expires_at() {
                    println!("expires:  {}", expires.to_rfc3339());
                }
            }
        }
        AccountCommand::Logout => {
            let removed = app.logout()?;
            let message = if removed {
                "auth-logged-out"
            } else {
                "auth-nothing-to-log-out-of"
            };
            if json {
                print_json(&serde_json::json!({"signed_out": removed}));
            } else {
                println!("{}", ephervex_i18n::lookup(message));
            }
        }
    }
    Ok(())
}

/// The account-management commands: everything that does not talk to Microsoft.
///
/// Split out from [`run_account_command`] because none of it is async — adding
/// an offline account, choosing a default, and removing one all touch only
/// `config.toml` and, for a Microsoft account, one keyring entry.
fn run_account_management(
    app: &mut App,
    command: &AccountCommand,
    json: bool,
) -> anyhow::Result<()> {
    match command {
        AccountCommand::List => {
            let status = app.account_status()?;
            show_accounts(&status, json);
        }
        AccountCommand::AddOffline { name } => {
            let account = app.add_offline_account(name)?;
            if json {
                print_json(&account_json(&account));
            } else {
                println!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "account-added-offline",
                        [("name", MessageArg::from(account.name.as_str()))]
                    )
                );
                println!("uuid:     {}", account.id);
                if account.is_default {
                    println!("default:  yes");
                }
            }
        }
        AccountCommand::Use { account } => {
            let account = app.set_default_account(account)?;
            if json {
                print_json(&account_json(&account));
            } else {
                println!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "account-default-set",
                        [("name", MessageArg::from(account.name.as_str()))]
                    )
                );
            }
        }
        AccountCommand::Remove { account } => {
            // Resolved before removal so the message can name what went, and
            // can say whether a credential was actually deleted — an offline
            // account never had one.
            let removed = app.account(account)?;
            let remaining = app.remove_account(account)?;
            if json {
                print_json(&serde_json::json!({
                    "removed": account_json(&removed),
                    "default": remaining.as_ref().map(account_json),
                }));
            } else {
                println!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        if removed.kind.has_stored_credential() {
                            "account-removed-with-credential"
                        } else {
                            "account-removed"
                        },
                        [("name", MessageArg::from(removed.name.as_str()))]
                    )
                );
                match &remaining {
                    Some(account) => println!(
                        "{}",
                        ephervex_i18n::lookup_args(
                            "account-default-set",
                            [("name", MessageArg::from(account.name.as_str()))]
                        )
                    ),
                    None => println!("{}", ephervex_i18n::lookup("auth-signed-out")),
                }
            }
        }
        // Handled by the caller; listed so a new variant is a compile error
        // here rather than a silently ignored command.
        AccountCommand::Login { .. }
        | AccountCommand::Status
        | AccountCommand::Refresh
        | AccountCommand::Logout => {}
    }
    Ok(())
}

/// Prints what the user has to do, as the login asks for it.
///
/// `open_browser` is the one place this application talks to the desktop. The
/// address is printed either way: an `open` that silently launches nothing —
/// over SSH, in a container, on a machine with no `xdg-open` — would otherwise
/// leave the user watching a spinner with no idea what was expected of them.
async fn show_login_prompts(
    mut events: tokio::sync::mpsc::UnboundedReceiver<AuthEvent>,
    open_browser: bool,
) {
    while let Some(event) = events.recv().await {
        match event {
            AuthEvent::OpenBrowser { url } => {
                eprintln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "auth-open-browser",
                        [("url", MessageArg::from(url.as_str()))]
                    )
                );
                if open_browser {
                    ephervex_core::open_in_browser(&url);
                }
            }
            AuthEvent::DeviceCode(prompt) => {
                eprintln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "auth-device-code-prompt",
                        [
                            ("url", MessageArg::from(prompt.verification_uri.as_str())),
                            ("code", MessageArg::from(prompt.user_code.as_str())),
                        ]
                    )
                );
            }
            // Progress, on stderr so `--json` stdout stays machine-readable.
            AuthEvent::Stage(stage) => {
                eprintln!("{}…", ephervex_i18n::lookup(stage.message_id()));
            }
            _ => {}
        }
    }
}

fn show_account_status(status: &AccountStatus, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "signed_in": status.is_signed_in(),
            "default": status.active.as_ref().map(account_json),
            "accounts": status.accounts.iter().map(account_json).collect::<Vec<_>>(),
            "session_active": status.session_active,
            "credential_storage": status.credential_storage.map(CredentialStorage::as_str),
        }));
        return;
    }

    let Some(active) = &status.active else {
        println!("{}", ephervex_i18n::lookup("auth-signed-out"));
        // Said here rather than only when a login fails: a user on a machine
        // with no keyring should find that out before they try. Offline
        // accounts still work, which is why this is a note and not an error
        if status.credential_storage.is_none() {
            eprintln!(
                "{}",
                ephervex_i18n::lookup("error-auth-secret-storage-unavailable")
            );
        }
        return;
    };

    println!(
        "{}",
        ephervex_i18n::lookup_args(
            if active.is_authenticated() {
                "auth-signed-in"
            } else {
                "account-active-offline"
            },
            [("name", MessageArg::from(active.name.as_str()))]
        )
    );
    println!("uuid:      {}", active.id);
    println!(
        "kind:      {}",
        ephervex_i18n::lookup(active.kind.message_id())
    );
    if !active.last_authenticated_at.is_empty() {
        println!("last auth: {}", active.last_authenticated_at);
    }
    println!(
        "storage:   {}",
        account_storage_label(active, status.credential_storage)
    );
    println!(
        "session:   {}",
        if status.session_active {
            "active"
        } else {
            "not established in this process"
        }
    );
    if status.accounts.len() > 1 {
        println!(
            "accounts:  {} (see `ephervex account list`)",
            status.accounts.len()
        );
    }
}

fn show_accounts(status: &AccountStatus, json: bool) {
    if json {
        print_json(&serde_json::Value::Array(
            status.accounts.iter().map(account_json).collect(),
        ));
        return;
    }

    if status.accounts.is_empty() {
        println!("{}", ephervex_i18n::lookup("accounts-list-empty"));
        return;
    }

    for account in &status.accounts {
        // The marker matters more than it looks: with several accounts, which
        // one a launch uses is the single thing the user is checking for.
        println!(
            "{} {}  {:<9} {:<26} {}",
            if account.is_default { "*" } else { " " },
            account.id,
            account.kind.as_str(),
            account.name,
            account_storage_label(account, status.credential_storage)
        );
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "accounts-list-count",
            [("count", MessageArg::from(status.accounts.len()))]
        )
    );
}

/// How an account's credential is being kept, in one phrase.
///
/// An offline account is not "missing" a credential; it has none by
/// construction. Saying so avoids making it look like a degraded Microsoft
/// account.
fn account_storage_label(account: &AccountSummary, storage: Option<CredentialStorage>) -> String {
    if !account.kind.has_stored_credential() {
        return ephervex_i18n::lookup("auth-storage-none-needed");
    }
    if !account.has_stored_credential {
        return ephervex_i18n::lookup("auth-storage-not-saved");
    }
    storage.map_or_else(
        || ephervex_i18n::lookup("error-auth-secret-storage-unavailable"),
        |storage| ephervex_i18n::lookup(storage.message_id()),
    )
}

fn account_json(account: &AccountSummary) -> serde_json::Value {
    serde_json::json!({
        "id": account.id.to_string(),
        "name": account.name,
        "kind": account.kind.as_str(),
        "authenticated": account.is_authenticated(),
        "default": account.is_default,
        "added_at": account.added_at,
        "last_authenticated_at": account.last_authenticated_at,
        "has_stored_credential": account.has_stored_credential,
    })
}

/// A session as JSON. Carries the account and the expiry, never the token.
fn session_json(session: &Session) -> serde_json::Value {
    serde_json::json!({
            "id": session.account.id.to_string(),
            "name": session.account.name,
            "kind": session.account.kind.as_str(),
            "authenticated": session.is_authenticated(),
            "authenticated_at": session.account.authenticated_at,
            "expires_at": session.expires_at(),
            "entitlements": session
    .entitlements()
    .map(|entitlements| entitlements.items.clone()),
            "owns_game": session.entitlements().is_some_and(Entitlements::owns_game),
            "game_pass": session.entitlements().is_some_and(Entitlements::has_game_pass),
            "credential_storage": session.storage().map(CredentialStorage::as_str),
            "persistent": session.storage().is_some_and(CredentialStorage::is_persistent),
        })
}

// ----------------------------------------------------------------- api keys

fn run_api_command(app: &mut App, command: &ApiCommand, json: bool) -> anyhow::Result<()> {
    match command {
        ApiCommand::Show => show_api_settings(&app.api_settings()?, json),
        ApiCommand::SetClientId { client_id } => {
            app.set_microsoft_client_id(client_id.as_deref())?;
            show_api_settings(&app.api_settings()?, json);
        }
        ApiCommand::SetUserAgent { user_agent } => {
            app.set_custom_user_agent(user_agent.as_deref())?;
            show_api_settings(&app.api_settings()?, json);
        }
        ApiCommand::SetCurseforgeKey => {
            let mut key = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut key)?;
            app.set_curseforge_api_key(&SecretString::new(key))?;
            // Never echo it back, not even truncated.
            println!("stored the CurseForge API key in the system keyring");
        }
        ApiCommand::ClearCurseforgeKey => {
            app.clear_curseforge_api_key()?;
            println!("removed the CurseForge API key from the system keyring");
        }
    }
    Ok(())
}

fn show_api_settings(settings: &ApiSettings, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "microsoft_client_id": settings.effective_microsoft_client_id,
            "microsoft_client_id_is_custom": settings.microsoft_client_id_is_custom,
            "user_agent": settings.effective_user_agent,
            "user_agent_is_custom": !settings.user_agent.is_empty(),
            "curseforge_api_key_present": settings.curseforge_api_key_present,
            "credential_storage": settings.credential_storage.map(CredentialStorage::as_str),
        }));
        return;
    }

    // The Client ID is printed in full because it is not a secret: Ephervex is
    // a public/native client, which has no client secret and cannot have one
    println!(
        "microsoft client id:  {}{}",
        settings.effective_microsoft_client_id,
        if settings.microsoft_client_id_is_custom {
            "  (configured)"
        } else {
            "  (this build's default)"
        }
    );
    println!(
        "user agent:           {}{}",
        settings.effective_user_agent,
        if settings.user_agent.is_empty() {
            ""
        } else {
            "  (configured)"
        }
    );
    // Presence, never the value.
    println!(
        "curseforge api key:   {}",
        if settings.curseforge_api_key_present {
            "stored in the system keyring"
        } else {
            "not set"
        }
    );
    if !settings.curseforge_api_key_present {
        // The address a user needs at exactly the moment they discover they
        // have no key. Not the Upload API, which is a different credential.
        println!(
            "  apply for one:      {}",
            ephervex_core::CURSEFORGE_API_KEY_HELP
        );
    }
    println!(
        "credential storage:   {}",
        settings.credential_storage.map_or_else(
            || ephervex_i18n::lookup("error-auth-secret-storage-unavailable"),
            |storage| ephervex_i18n::lookup(storage.message_id()),
        )
    );
}

fn show_runtimes(inventory: &RuntimeInventory, json: bool) {
    if json {
        print_json(&serde_json::json!({
                    "runtimes": inventory.runtimes.iter().map(runtime_json).collect::<Vec<_>>(),
                    "failures": inventory
        .failures
        .iter()
        .map(|failure| serde_json::json!({
                            "path": failure.origin.path,
                            "source": failure.origin.kind.as_str(),
                            "error": failure.error.to_string(),
                        }))
        .collect::<Vec<_>>(),
                    "candidates_considered": inventory.candidates,
                    "executables_probed": inventory.probed,
                    "scanned_at": inventory.scanned_at,
                }));
        return;
    }

    if inventory.runtimes.is_empty() {
        println!("{}", ephervex_i18n::lookup("java-runtimes-list-empty"));
    } else {
        for runtime in &inventory.runtimes {
            println!(
                "{:<4} {:<12} {:<8} {:<14} {}",
                runtime.version.major,
                runtime.version_string,
                runtime.arch,
                runtime.primary_source().as_str(),
                runtime.executable.display()
            );
            println!("     {}", runtime.vendor);
        }
        println!(
            "{}",
            ephervex_i18n::lookup_args(
                "java-runtimes-list-count",
                [("count", MessageArg::from(inventory.runtimes.len()))]
            )
        );
    }

    // The deduplication is the whole reason a scan of a typical /usr/lib/jvm
    // does not report the same JDK four times; saying so makes it checkable.
    eprintln!(
        "{} candidate paths considered, {} distinct executables probed",
        inventory.candidates, inventory.probed
    );
    for failure in &inventory.failures {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "java-runtime-probe-failed",
                [("path", MessageArg::from(failure.origin.path.as_path()))]
            )
        );
    }
}

fn show_runtime_detail(runtime: &JavaRuntime, json: bool) {
    if json {
        print_json(&runtime_json(runtime));
        return;
    }
    println!("path:     {}", runtime.executable.display());
    println!(
        "java:     {} (major {})",
        runtime.version_string, runtime.version.major
    );
    println!("runtime:  {}", runtime.runtime_version);
    println!("vendor:   {}", runtime.vendor);
    println!("arch:     {}", runtime.arch);
    if let Some(home) = &runtime.home {
        println!("home:     {}", home.display());
    }
    for origin in &runtime.origins {
        println!("found:    {} via {}", origin.path.display(), origin.kind);
    }
}

fn show_java_plan(plan: &JavaPlan, json: bool) {
    if json {
        print_json(&serde_json::json!({
                    "minecraft_version": plan.minecraft_version,
                    "required_major": plan.required.major,
                    "requirement_basis": plan.required.basis.as_str(),
                    "mojang_runtime_component": match &plan.required.basis {
                        RequirementBasis::Metadata { component } => Some(component.clone()),
                        RequirementBasis::FallbackTable => None,
                    },
                    "choice": java_choice_json(&plan.choice),
                    "runtime": runtime_json(&plan.runtime),
                    "installed_now": plan.installed_now,
                    "rejected": plan
        .rejected
        .iter()
        .map(|rejected| serde_json::json!({
                            "path": rejected.executable,
                            "version": rejected.version.to_string(),
                            "rule": rejected.rejection.rule,
                            "reason": rejected.rejection.reason.to_string(),
                        }))
        .collect::<Vec<_>>(),
                    "jvm": {
                        "min_memory_mib": plan.jvm.min_memory_mib,
                        "max_memory_mib": plan.jvm.max_memory_mib,
                        "jvm_args": plan.jvm.jvm_args,
                        "overrides_global": plan.jvm.overrides_global,
                        "overridden_fields": plan
        .jvm
        .overridden
        .iter()
        .map(|field| field.as_str())
        .collect::<Vec<_>>(),
                    },
                }));
        return;
    }

    println!("minecraft: {}", plan.minecraft_version);
    match &plan.required.basis {
        RequirementBasis::Metadata { component } => println!(
            "requires:  java {} (Mojang runtime {component})",
            plan.required.major
        ),
        // Saying which of the two answered matters: one is Mojang's statement,
        // the other is this build's table.
        RequirementBasis::FallbackTable => println!(
            "requires:  java {} (compatibility table; the metadata declares none)",
            plan.required.major
        ),
    }
    match &plan.choice {
        JavaPathChoice::Pinned { path, source } => println!(
            "selected:  {} (pinned by {})",
            path.display(),
            source.as_str()
        ),
        JavaPathChoice::Automatic => println!("selected:  {}", plan.runtime.executable.display()),
    }
    println!(
        "runtime:   {} {} ({}, {})",
        plan.runtime.version_string,
        plan.runtime.arch,
        plan.runtime.vendor,
        plan.runtime.primary_source().as_str()
    );
    if plan.installed_now {
        println!("installed: downloaded during this command");
    }
    println!(
        "memory:    {} MiB – {} MiB{}",
        plan.jvm.min_memory_mib,
        plan.jvm.max_memory_mib,
        if plan.jvm.overrides_global {
            "  (instance overrides global settings)"
        } else {
            ""
        }
    );
    if !plan.jvm.jvm_args.is_empty() {
        println!("jvm args:  {}", plan.jvm.jvm_args.join(" "));
    }
    for rejected in &plan.rejected {
        println!(
            "rejected:  {} — {}",
            rejected.executable.display(),
            rejected.rejection
        );
    }
}

fn show_managed_install(installed: &ManagedInstallation, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "release": installed.release.release_name,
            "vendor": installed.release.vendor,
            "architecture": installed.release.architecture.as_str(),
            "directory": installed.directory,
            "newly_installed": installed.newly_installed,
            "runtime": runtime_json(&installed.runtime),
        }));
        return;
    }
    println!("release:   {}", installed.release.release_name);
    println!("directory: {}", installed.directory.display());
    println!("java:      {}", installed.runtime.executable.display());
    println!(
        "status:    {}",
        if installed.newly_installed {
            "downloaded and installed"
        } else {
            "already installed"
        }
    );
}

fn runtime_json(runtime: &JavaRuntime) -> serde_json::Value {
    serde_json::json!({
            "path": runtime.executable,
            "major": runtime.version.major,
            "version": runtime.version_string,
            "runtime_version": runtime.runtime_version,
            "vendor": runtime.vendor,
            "arch": runtime.arch,
            "home": runtime.home,
            "managed": runtime.is_managed(),
            "source": runtime.primary_source().as_str(),
            "origins": runtime
    .origins
    .iter()
    .map(|origin| serde_json::json!({
                    "source": origin.kind.as_str(),
                    "path": origin.path,
                }))
    .collect::<Vec<_>>(),
        })
}

fn java_choice_json(choice: &JavaPathChoice) -> serde_json::Value {
    match choice {
        JavaPathChoice::Pinned { path, source } => serde_json::json!({
            "kind": "pinned",
            "path": path,
            "pinned_by": source.as_str(),
        }),
        JavaPathChoice::Automatic => serde_json::json!({"kind": "automatic"}),
    }
}

fn show_releases(listing: &ReleaseListing, limit: Option<usize>, json: bool) {
    let shown = limit
        .unwrap_or(listing.releases.len())
        .min(listing.releases.len());
    let releases = listing.releases.get(..shown).unwrap_or(&listing.releases);

    if json {
        print_json(&serde_json::json!({
            "releases": releases.iter().map(release_json).collect::<Vec<_>>(),
            "latest_release": listing.latest.release,
            "latest_snapshot": listing.latest.snapshot,
            "supported_count": listing.releases.len(),
            "manifest_versions": listing.total_versions,
            "minimum_supported": ephervex_core::MINIMUM_SUPPORTED_VERSION,
            "freshness": freshness_label(listing.freshness),
            "stale": listing.is_stale(),
            "fetched_at": listing.fetched_at,
        }));
        return;
    }

    if listing.releases.is_empty() {
        println!("{}", ephervex_i18n::lookup("minecraft-versions-list-empty"));
        return;
    }

    for record in releases {
        println!(
            "{:<10} {}{}",
            record.id,
            record.release_time.format("%Y-%m-%d"),
            if record.has_recognized_version_id() {
                ""
            } else {
                "  [unrecognized version scheme]"
            }
        );
    }
    println!(
        "{}",
        ephervex_i18n::lookup_args(
            "minecraft-versions-list-count",
            [("count", MessageArg::from(listing.releases.len()))]
        )
    );

    // A user debugging a network problem must not mistake yesterday's cached
    // list for a fresh response.
    if listing.is_stale() {
        eprintln!("{}", ephervex_i18n::lookup("minecraft-versions-stale"));
    }
    for record in listing.unrecognized_version_ids() {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "minecraft-version-id-unrecognized",
                [
                    ("version", MessageArg::from(record.id.as_str())),
                    (
                        "minimum",
                        MessageArg::from(ephervex_core::MINIMUM_SUPPORTED_VERSION)
                    ),
                ]
            )
        );
    }
}

fn show_version(version: &VersionMetadata, json: bool) -> anyhow::Result<()> {
    // Reached through the accessors rather than the fields, so a document that
    // is missing something a launch needs fails here with a typed error rather
    // than printing a half-populated summary.
    let client = version.client_jar()?;
    let assets = version.asset_index()?;

    if json {
        print_version_json(version, client, assets);
    } else {
        print_version_text(version, client, assets);
    }
    Ok(())
}

fn print_version_json(version: &VersionMetadata, client: &Artifact, assets: &AssetIndexRef) {
    print_json(&serde_json::json!({
                "id": version.id,
                "type": version.release_type.as_str(),
                "release_time": version.release_time,
                "main_class": version.main_class,
                "compliance_level": version.compliance_level,
                "java": version.java_version.as_ref().map(|java| serde_json::json!({
                    "component": java.component,
                    "major_version": java.major_version,
                })),
                "assets": {
                    "id": assets.id,
                    "sha1": assets.artifact.sha1.digest(),
                    "total_size": assets.total_size,
                },
                "downloads": version.downloads.keys().collect::<Vec<_>>(),
                "client": {"sha1": client.sha1.digest(), "size": client.size},
                "arguments": {
                    "form": argument_form_label(version.arguments.form()),
                    "groups": version
    .arguments
    .groups()
    .map(|(group, entries)| (group.to_string(), entries.len()))
    .collect::<std::collections::BTreeMap<_, _>>(),
                },
                "libraries": {
                    "total": version.libraries.len(),
                    "rule_gated": version.libraries.iter().filter(|l| !l.rules.is_empty()).count(),
                    "legacy_natives": version.libraries.iter().filter(|l| l.is_legacy_natives()).count(),
                },
                "logging": version.logging.as_ref().map(|logging| logging.id.clone()),
                "inherits_from": version.inherits_from(),
                "complete": version.is_complete_vanilla(),
        }));
}

fn print_version_text(version: &VersionMetadata, client: &Artifact, assets: &AssetIndexRef) {
    println!("id:         {}", version.id);
    println!("type:       {}", version.release_type);
    println!("released:   {}", version.release_time.format("%Y-%m-%d"));
    println!("main class: {}", version.main_class);
    match &version.java_version {
        // Metadata records the requirement; Java selection resolves it.
        Some(java) => println!(
            "java:       {} (Mojang runtime {})",
            java.major_version, java.component
        ),
        None => println!("java:       not declared"),
    }
    println!(
        "arguments:  {} form",
        argument_form_label(version.arguments.form())
    );
    for (group, entries) in version.arguments.groups() {
        let conditional = entries
            .iter()
            .filter(|entry| !entry.is_unconditional())
            .count();
        println!(
            "  {group:<17} {} entries, {conditional} conditional",
            entries.len()
        );
    }
    println!(
        "libraries:  {} ({} rule-gated, {} with a legacy natives map)",
        version.libraries.len(),
        version
            .libraries
            .iter()
            .filter(|library| !library.rules.is_empty())
            .count(),
        version
            .libraries
            .iter()
            .filter(|library| library.is_legacy_natives())
            .count()
    );
    println!(
        "client jar: {} bytes, sha1 {}",
        client.size,
        client.sha1.digest()
    );
    println!(
        "assets:     index {} ({} bytes of objects)",
        assets.id,
        assets
            .total_size
            .map_or_else(|| "unknown".to_owned(), |size| size.to_string())
    );
    println!(
        "downloads:  {}",
        version
            .downloads
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(logging) = &version.logging {
        println!("logging:    {}", logging.id);
    }
    if let Some(parent) = version.inherits_from() {
        println!("inherits:   {parent}  (merging lands with loader support)");
    }
}

fn release_json(record: &VersionRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "type": record.release_type.as_str(),
        "release_time": record.release_time,
        "sha1": record.sha1.digest(),
        "compliance_level": record.compliance_level,
        "recognized_version_id": record.has_recognized_version_id(),
    })
}

const fn freshness_label(freshness: Freshness) -> &'static str {
    match freshness {
        Freshness::Fetched => "fetched",
        Freshness::Cached => "cached",
        Freshness::Stale => "stale",
    }
}

const fn argument_form_label(form: ArgumentForm) -> &'static str {
    match form {
        ArgumentForm::Legacy => "legacy",
        ArgumentForm::Modern => "modern",
    }
}

fn show_reconciliation(report: &ReconciliationReport, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "removed_stale_temporary": reconciled_json(&report.removed_stale_temporary),
            "missing_persistent": reconciled_json(&report.missing_persistent),
            "reindexed": reconciled_json(&report.reindexed),
            "recovered": reconciled_json(&report.recovered),
            "unfinished_instances": reconciled_json(&report.unfinished_instances),
            "needs_attention": report.needs_attention(),
        }));
        return;
    }

    if report.is_empty() {
        println!("{}", ephervex_i18n::messages::nothing_to_reconcile());
        return;
    }

    if !report.removed_stale_temporary.is_empty() {
        println!(
            "{}",
            ephervex_i18n::messages::removed_stale_temporary(report.removed_stale_temporary.len())
        );
    }
    if !report.reindexed.is_empty() {
        println!(
            "{}",
            ephervex_i18n::messages::reindexed(report.reindexed.len())
        );
    }
    if !report.missing_persistent.is_empty() {
        println!(
            "{}",
            ephervex_i18n::messages::missing_persistent(report.missing_persistent.len())
        );
        for entry in &report.missing_persistent {
            println!(
                "  {}",
                ephervex_i18n::lookup_args(
                    "reconcile-persistent-root-missing-detail",
                    [
                        ("name", MessageArg::from(entry.name.as_str())),
                        ("path", MessageArg::from(entry.root.as_path())),
                    ]
                )
            );
        }
        println!("{}", ephervex_i18n::messages::missing_persistent_action());
    }
    if !report.unfinished_instances.is_empty() {
        println!(
            "{}",
            ephervex_i18n::messages::unfinished_instances(report.unfinished_instances.len())
        );
        for entry in &report.unfinished_instances {
            println!(
                "  {}",
                ephervex_i18n::lookup_args(
                    "reconcile-unfinished-instance-detail",
                    [
                        ("name", MessageArg::from(entry.name.as_str())),
                        ("path", MessageArg::from(entry.root.as_path())),
                    ]
                )
            );
        }
        println!("{}", ephervex_i18n::messages::unfinished_instances_action());
    }
}

// -------------------------------------------------------------------- output

/// Implements `ephervex import <file> --inspect` without writing an instance.
fn run_inspect(app: &App, args: &ImportArgs, json: bool) -> anyhow::Result<()> {
    let analysis = app.analyze_pack(&args.archive)?;
    print_analysis(&analysis, json);
    Ok(())
}

/// `ephervex import <file>`.
async fn run_import(app: &App, args: &ImportArgs, json: bool) -> anyhow::Result<()> {
    let selection = if args.all_optional {
        OptionalSelection::all()
    } else {
        OptionalSelection::of(args.optional.clone())
    };
    let mut request = ImportRequest::new(&args.archive)
        .with_storage(args.storage.class())
        .selecting(selection);
    if let Some(name) = &args.name {
        request = request.named(name);
    }

    // Progress goes to stderr through the reporter's log, so `--json` output
    // stays pipeable (see this file's header).
    let reporter = ephervex_core::OperationReporter::silent("operation-importing-pack");
    let imported = app
        .import_pack(&request, &reporter, &CancellationToken::new())
        .await?;
    print_imported(app, &imported, json)
}

/// Prints what a pack is, without importing it.
fn print_analysis(analysis: &PackAnalysis, json: bool) {
    if json {
        print_json(&analysis_json(analysis));
        return;
    }

    println!("pack:      {}", analysis.name());
    if let Some(version) = &analysis.recipe.original_version {
        println!("version:   {version}");
    }
    if let Some(author) = analysis.author() {
        println!("author:    {author}");
    }
    println!("source:    {}", analysis.format);
    println!("minecraft: {}", analysis.minecraft_version());
    println!("loader:    {}", analysis.loader());
    println!(
        "files:     {} required, {} optional",
        analysis.required_file_count(PackSide::Client),
        analysis.optional_file_count(PackSide::Client)
    );
    if analysis.needs_provider_resolution() {
        // The counts above come from the pack's own manifest. Which files they
        // are is a question only the provider can answer, and answering it
        // needs the key.
        println!(
            "blocked:   a {} API key is required before these files can be listed",
            analysis.format
        );
    }
    for component in analysis.optional_components(PackSide::Client) {
        println!(
            "optional:  {} [{}]",
            component.display_name(),
            if component.starts_selected() {
                "x"
            } else {
                " "
            }
        );
    }
    for layer in analysis.layers() {
        println!("layer:     {}", layer.as_str());
    }
    if analysis.has_files_for(PackSide::Server) {
        eprintln!(
            "{}",
            ephervex_i18n::lookup("pack-analysis-server-preserved")
        );
    }
    report_rejected_addresses(&analysis.rejected_urls, json);
}

/// Prints an imported instance and what the import did.
fn print_imported(app: &App, imported: &ImportedPack, json: bool) -> anyhow::Result<()> {
    if json {
        let mut value = instance_json(app, &imported.record)?;
        value["preferences"] = preferences_applied_json(imported.preferences.as_ref());
        value["pack"] = analysis_json(&imported.analysis);
        value["installed"] = serde_json::json!(imported.materialized.files.installed);
        value["preserved"] = serde_json::json!(imported.materialized.files.skipped);
        value["recipe"] = serde_json::json!({
                    "path": imported.materialized.recipe_path,
                    "files": imported.recipe().files.len(),
                    "layers": imported
        .recipe()
        .override_layers
        .iter()
        .map(|layer| serde_json::json!({
                            "scope": layer.scope.as_str(),
                            "retained": layer.is_retained(),
                            "files": layer.files,
                        }))
        .collect::<Vec<_>>(),
                });
        print_json(&value);
        return Ok(());
    }

    print_instance(app, &imported.record, json)?;
    println!("pack:      {}", imported.analysis.name());
    println!(
        "installed: {} files",
        imported.materialized.files.installed.len()
    );
    println!("recipe:    {}", imported.materialized.recipe_path.display());

    // A client import preserves the server side rather than installing it;
    // saying nothing would look like the files went missing.
    let preserved = imported.materialized.files.skipped.len();
    if preserved > 0 {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "pack-import-files-preserved",
                [("count", MessageArg::from(preserved))]
            )
        );
    }
    report_rejected_addresses(&imported.analysis.rejected_urls, json);
    Ok(())
}

/// Prints one side's plan out of a retained recipe.
fn print_recipe(recipe: &PackRecipe, side: PackSide, selection: &OptionalSelection, json: bool) {
    let plan = recipe.plan(side, selection);
    if json {
        print_json(&serde_json::json!({
                    "side": side.as_str(),
                    "source": recipe.source_kind,
                    "pack": recipe.original_name,
                    "pack_version": recipe.original_version,
                    "minecraft_version": plan.minecraft_version,
                    "loader": {
                        "kind": plan.loader.kind.to_string(),
                        "version": plan.loader.version,
                    },
                    "files": plan
        .files
        .iter()
        .map(|file| serde_json::json!({
                            "path": file.logical_path,
                            "client": file.client.as_str(),
                            "server": file.server.as_str(),
                            "selected": file.selected,
                        }))
        .collect::<Vec<_>>(),
                    "layers": plan
        .layers
        .iter()
        .map(|layer| serde_json::json!({
                            "scope": layer.scope.as_str(),
                            "retained": layer.is_retained(),
                            "files": layer.files,
                        }))
        .collect::<Vec<_>>(),
                    "layers_retained": plan.layers_are_retained(),
                    "total_bytes": plan.total_bytes(),
                }));
        return;
    }

    println!("pack:      {}", recipe.original_name);
    println!("source:    {}", recipe.source_kind);
    println!("side:      {}", side.as_str());
    println!("minecraft: {}", plan.minecraft_version);
    println!("loader:    {}", plan.loader);
    for file in &plan.files {
        println!("file:      {}", file.display_name());
    }
    for file in &recipe.files {
        if file.requirement(side) == SideRequirement::Unsupported {
            println!("excluded:  {}", file.display_name());
        }
    }
    for layer in &plan.layers {
        println!(
            "layer:     {} ({} files, {})",
            layer.scope.as_str(),
            layer.files,
            if layer.is_retained() {
                "retained"
            } else {
                "in the instance"
            }
        );
    }
    if !plan.layers_are_retained() && side == PackSide::Server {
        eprintln!("{}", ephervex_i18n::lookup("pack-recipe-layer-missing"));
    }
}

fn analysis_json(analysis: &PackAnalysis) -> serde_json::Value {
    serde_json::json!({
            "format": analysis.format.to_string(),
            "source_kind": analysis.format.source_kind(),
            "name": analysis.name(),
            "version": analysis.recipe.original_version,
            "summary": analysis.recipe.summary,
            "minecraft_version": analysis.minecraft_version(),
            "loader": {
                "kind": analysis.loader().kind.to_string(),
                "version": analysis.loader().version,
            },
            "author": analysis.author(),
            "required_files": analysis.required_file_count(PackSide::Client),
            "optional_file_count": analysis.optional_file_count(PackSide::Client),
            "needs_provider_resolution": analysis.needs_provider_resolution(),
            "optional_files": analysis
    .optional_components(PackSide::Client)
    .iter()
    .map(|component| serde_json::json!({
                    "path": component.logical_path,
                    "size": component.size,
                    "selected": component.starts_selected(),
                    "server": component.opposite_side.as_str(),
                }))
    .collect::<Vec<_>>(),
            "layers": analysis
    .layers()
    .map(ephervex_core::OverrideScope::as_str)
    .collect::<Vec<_>>(),
            "has_server_files": analysis.has_files_for(PackSide::Server),
            "rejected_addresses": analysis.rejected_urls,
        })
}

/// Reports download addresses the URL policy refused.
fn report_rejected_addresses(rejected: &[String], json: bool) {
    if rejected.is_empty() || json {
        return;
    }
    eprintln!(
        "{}",
        ephervex_i18n::lookup_args(
            "pack-import-rejected-addresses",
            [("count", MessageArg::from(rejected.len()))]
        )
    );
    for address in rejected {
        eprintln!("  {address}");
    }
}

fn print_instance(app: &App, record: &ClientRecord, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&instance_json(app, record)?);
    } else {
        println!("id:        {}", record.id);
        println!("name:      {}", record.name);
        println!("storage:   {}", record.storage);
        println!("minecraft: {}", record.minecraft_version);
        println!("loader:    {}", record.loader);
        println!("root:      {}", record.root.display());
        if let Some(state) = app.launch_state(record.id)? {
            println!("running:   {}", describe_running(&state));
        }
        for id in app.linked_server_ids(record.id)? {
            println!("server:    {id}");
        }
        for reason in &record.attention {
            println!("attention: {reason:?}");
        }
    }
    Ok(())
}

/// Prints a newly created instance and what the one-time preference
/// application did.
fn print_created_instance(app: &App, created: &CreatedClient, json: bool) -> anyhow::Result<()> {
    if json {
        let mut value = instance_json(app, &created.record)?;
        value["preferences"] = preferences_applied_json(created.preferences.as_ref());
        print_json(&value);
        return Ok(());
    }

    print_instance(app, &created.record, json)?;
    let Some(applied) = &created.preferences else {
        return Ok(());
    };
    if !applied.applied.is_empty() {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "preferences-applied",
                [("count", MessageArg::from(applied.applied.len()))]
            )
        );
    }
    // A preference that was not written is reported with the reason, and
    // silence would look like a bug. "This release has no such setting" and
    // "nobody has established what this release does" are different answers
    // and get different sentences.
    let version = created.record.minecraft_version.as_str();
    for id in &applied.unsupported {
        print_preference_note("preferences-unsupported-for-version", id, version);
    }
    for id in &applied.unknown {
        print_preference_note("preferences-unverified-for-version", id, version);
    }
    for skipped in &applied.skipped {
        let message = match skipped.reason {
            SkipReason::LanguageUnverified => "preferences-language-unverified",
            SkipReason::LanguageAbsent => "preferences-language-absent",
            _ => "preferences-value-not-applied",
        };
        print_preference_note(message, &skipped.id, version);
    }
    Ok(())
}

/// Prints one "this preference was not applied" line.
fn print_preference_note(message: &str, id: &PreferenceId, version: &str) {
    eprintln!(
        "{}",
        ephervex_i18n::lookup_args(
            message,
            [
                ("preference", MessageArg::from(id.as_str())),
                ("version", MessageArg::from(version)),
            ]
        )
    );
}

fn preferences_applied_json(applied: Option<&PreferenceApplication>) -> serde_json::Value {
    applied.map_or(serde_json::Value::Null, |applied| {
        serde_json::json!({
            "schema": applied.schema,
            "source": applied.source.as_str(),
            "created_file": applied.created_file,
            "applied": preference_names(&applied.applied),
            "unsupported": preference_names(&applied.unsupported),
            "unknown": preference_names(&applied.unknown),
            "skipped": applied
                .skipped
                .iter()
                .map(|skipped| {
                    serde_json::json!({
                        "preference": skipped.id.as_str(),
                        "reason": skipped.reason.as_str(),
                    })
                })
                .collect::<Vec<_>>(),
        })
    })
}

fn preference_names(ids: &[PreferenceId]) -> Vec<&str> {
    ids.iter().map(PreferenceId::as_str).collect()
}

/// Prints the outcome of an open-folder action.
///
/// An unavailable folder is a failure with an exit code, not a silent success:
/// a script that asked for the crash reports of an instance that has never
/// crashed should be able to tell.
/// What one `instance runtime` invocation asked for.
#[derive(Clone, Copy)]
struct RuntimeArgs<'a> {
    java: Option<&'a Path>,
    min_memory: Option<u32>,
    max_memory: Option<u32>,
    jvm_args: &'a [String],
    clear: bool,
}

fn run_instance_runtime(app: &App, command: &InstanceCommand, json: bool) -> anyhow::Result<()> {
    let InstanceCommand::Runtime {
        id,
        java,
        min_memory,
        max_memory,
        jvm_args,
        clear,
    } = command
    else {
        return Ok(());
    };
    let record = apply_instance_runtime(
        app,
        *id,
        RuntimeArgs {
            java: java.as_deref(),
            min_memory: *min_memory,
            max_memory: *max_memory,
            jvm_args,
            clear: *clear,
        },
    )?;
    print_instance_runtime(&record, json);
    Ok(())
}

/// Applies a runtime change, or reads the instance back when none was asked for.
///
/// Pinned values replace what was pinned before rather than merging into it, so
/// one invocation describes one intended state.
fn apply_instance_runtime(
    app: &App,
    id: InstanceId,
    args: RuntimeArgs<'_>,
) -> anyhow::Result<ClientRecord> {
    if args.clear {
        return Ok(app.set_client_runtime_settings(id, None, None)?);
    }
    let asked = args.java.is_some()
        || args.min_memory.is_some()
        || args.max_memory.is_some()
        || !args.jvm_args.is_empty();
    if !asked {
        return Ok(app.client(id)?);
    }
    let jvm = JvmSettings {
        min_memory_mib: args.min_memory,
        max_memory_mib: args.max_memory,
        jvm_args: args.jvm_args.to_vec(),
    };
    Ok(app.set_client_runtime_settings(
        id,
        args.java.map(JavaOverride::new),
        (!jvm.pins_nothing()).then_some(jvm),
    )?)
}

/// What one instance pins for itself, and what it leaves to the global setting.
fn print_instance_runtime(record: &ClientRecord, json: bool) {
    let java = record
        .java_override
        .as_ref()
        .map(|java| java.path.display().to_string());
    let jvm = record.jvm_override.as_ref();
    if json {
        print_json(&serde_json::json!({
            "instance": record.id.to_string(),
            "java": java,
            "min_memory_mib": jvm.and_then(|jvm| jvm.min_memory_mib),
            "max_memory_mib": jvm.and_then(|jvm| jvm.max_memory_mib),
            "jvm_args": jvm.map(|jvm| jvm.jvm_args.clone()).unwrap_or_default(),
        }));
        return;
    }
    let global = "follows the global setting";
    println!("java:       {}", java.as_deref().unwrap_or(global));
    println!(
        "min memory: {}",
        jvm.and_then(|jvm| jvm.min_memory_mib)
            .map_or_else(|| global.to_owned(), |mib| format!("{mib} MiB"))
    );
    println!(
        "max memory: {}",
        jvm.and_then(|jvm| jvm.max_memory_mib)
            .map_or_else(|| global.to_owned(), |mib| format!("{mib} MiB"))
    );
    let args = jvm.map(|jvm| jvm.jvm_args.as_slice()).unwrap_or_default();
    println!(
        "jvm args:   {}",
        if args.is_empty() {
            global.to_owned()
        } else {
            args.join(" ")
        }
    );
}

fn print_folder(path: Option<&Path>, folder: &str, json: bool) -> anyhow::Result<()> {
    if let Some(path) = path {
        if json {
            print_json(&serde_json::json!({ "folder": folder, "path": path }));
        } else {
            println!("{}", path.display());
        }
        return Ok(());
    }

    let summary = ephervex_i18n::lookup("instance-folder-unavailable");
    let action = ephervex_i18n::lookup_attribute_args(
        "instance-folder-unavailable",
        "action",
        [("folder", MessageArg::from(folder))],
    );
    if json {
        print_json(&serde_json::json!({
            "folder": folder,
            "path": serde_json::Value::Null,
            "error": summary,
        }));
    }
    match action {
        Some(action) => anyhow::bail!("{summary} {action}"),
        None => anyhow::bail!("{summary}"),
    }
}

/// Reports the symlinked folders a duplication did not follow.
fn report_skipped_links(skipped: &[PathBuf], json: bool) {
    if skipped.is_empty() || json {
        return;
    }
    eprintln!(
        "{}",
        ephervex_i18n::lookup_args(
            "instance-duplicate-skipped-links",
            [("count", MessageArg::from(skipped.len()))]
        )
    );
    for link in skipped {
        eprintln!("  {}", link.display());
    }
}

fn print_server(record: &ServerRecord, json: bool) {
    if json {
        print_json(&server_json(record));
    } else {
        println!("id:        {}", record.id);
        println!("name:      {}", record.name);
        println!("storage:   {}", record.storage);
        println!("minecraft: {}", record.minecraft_version);
        println!("loader:    {}", record.loader);
        println!("root:      {}", record.root.display());
        println!("installed: {}", record.installation.is_installed());
        if record.source.kind == ephervex_model::ImportSourceKind::ServerPack {
            match record.source.reference.as_deref() {
                Some(archive) => println!("content:   server files from {archive}"),
                None => println!("content:   server files from an archive"),
            }
        }
        for reason in &record.attention {
            if let ephervex_model::AttentionReason::UnknownServerApplicability(path) = reason {
                eprintln!(
                    "server applicability is unknown for {path}; it was omitted (use --include-unknown {path} to confirm it on a new server)"
                );
            }
        }
    }
}

async fn run_server_foreground(
    app: &App,
    id: ServerId,
    java: Option<PathBuf>,
    json: bool,
) -> anyhow::Result<()> {
    let (observer, events) = ServerObserver::channel();
    let mut events = events;
    let printer = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ServerEvent::Preparing { operation } => {
                    if json {
                        print_json(&serde_json::json!({
                            "event": "preparing",
                            "operation": operation.to_string(),
                        }));
                    } else {
                        print_operation_id(operation);
                    }
                }
                ServerEvent::Started { pid } => {
                    if json {
                        print_json(&serde_json::json!({ "event": "started", "pid": pid }));
                    } else {
                        eprintln!("server started as pid {pid}");
                    }
                }
                ServerEvent::Output(line) => {
                    if json {
                        print_json(&serde_json::json!({
                            "event": "output",
                            "line": captured_line_json(&line),
                        }));
                    } else {
                        print_captured_line(&line);
                    }
                }
                ServerEvent::Warning(warning) => print_server_warning(&warning, json),
                ServerEvent::CapturePruned(retention) => {
                    if json {
                        print_json(&serde_json::json!({
                            "event": "capture_pruned",
                            "removed_segments": retention.removed_segments,
                            "removed_bytes": retention.removed_bytes,
                            "removed_lines": retention.removed_lines,
                        }));
                    } else {
                        eprintln!(
                            "capture pruned {} segments ({} bytes)",
                            retention.removed_segments, retention.removed_bytes
                        );
                    }
                }
                ServerEvent::CaptureUnavailable { detail } => {
                    if json {
                        print_json(&serde_json::json!({
                            "event": "capture_unavailable",
                            "detail": detail,
                        }));
                    } else {
                        eprintln!("durable capture unavailable: {detail}");
                    }
                }
                ServerEvent::Exited(exit) if json => {
                    print_json(&serde_json::json!({
                        "event": "exited",
                        "code": exit.code,
                        "signal": exit.signal,
                    }));
                }
                _ => {}
            }
        }
    });

    let start = app.start_server(id, java, &observer);
    tokio::pin!(start);
    let outcome = tokio::select! {
        result = &mut start => result,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            if let Err(error) = app.stop_server(id).await {
                eprintln!("graceful stop was not confirmed: {error}");
            }
            start.await
        }
    };
    tokio::task::yield_now().await;
    printer.abort();
    let exit = outcome?;
    if !json {
        match (exit.code, exit.signal) {
            (Some(code), _) => println!("server exited with code {code}"),
            (_, Some(signal)) => println!("server exited from signal {signal}"),
            _ => println!("server exited"),
        }
    }
    Ok(())
}

fn print_server_warning(warning: &ServerWarning, json: bool) {
    match warning {
        ServerWarning::OfflineModeExposed => {
            if json {
                print_json(&serde_json::json!({
                    "event": "warning",
                    "kind": "offline_mode_exposed",
                }));
            } else {
                eprintln!("warning: offline mode is exposed beyond the local machine");
            }
        }
        ServerWarning::PortUnavailable {
            address,
            port,
            detail,
        } => {
            if json {
                print_json(&serde_json::json!({
                    "event": "warning",
                    "kind": "port_unavailable",
                    "address": address,
                    "port": port,
                    "detail": detail,
                }));
            } else {
                eprintln!(
                    "warning: {address}:{port} was unavailable during the advisory check: {detail}"
                );
            }
        }
        _ => {}
    }
}

fn print_server_status(app: &App, id: ServerId, json: bool) -> anyhow::Result<()> {
    let status = app.server_status(id)?;
    let persistence = persistence_name(&status.persistence);
    let runtime = runtime_name(&status.runtime);
    let capture = app.server_output(id, 0)?;
    if json {
        print_json(&server_status_json_from_parts(&status, &capture));
    } else {
        print_server(&status.record, false);
        println!("persistence: {persistence}");
        println!("runtime:     {runtime}");
        println!("capture:     {}", capture_health_name(&capture.health));
        if let RuntimeClassification::Orphaned { child } = &status.runtime {
            println!("orphan pid:  {}", child.pid);
            println!("executable:  {}", child.executable.display());
            println!("action:      terminate this process with explicit operating-system tools");
        }
    }
    Ok(())
}

fn server_status_json(app: &App, id: ServerId) -> anyhow::Result<serde_json::Value> {
    let status = app.server_status(id)?;
    let capture = app.server_output(id, 0)?;
    Ok(server_status_json_from_parts(&status, &capture))
}

fn server_status_json_from_parts(
    status: &ephervex_core::ServerStatus,
    capture: &ephervex_core::CaptureSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "server": server_json(&status.record),
        "persistence_health": persistence_name(&status.persistence),
        "installation_state": if status.record.installation.is_installed() { "installed" } else { "not_installed" },
        "runtime_classification": runtime_name(&status.runtime),
        "capture_health": capture_health_name(&capture.health),
        "retention": {
            "removed_segments": capture.retention.removed_segments,
            "removed_bytes": capture.retention.removed_bytes,
            "removed_lines": capture.retention.removed_lines,
            "earliest_retained_at": capture.retention.earliest_retained_at,
        },
    })
}

fn persistence_name(health: &ServerPersistenceHealth) -> &'static str {
    match health {
        ServerPersistenceHealth::Healthy => "healthy",
        ServerPersistenceHealth::IndexOutOfSync => "index_out_of_sync",
        ServerPersistenceHealth::MissingRoot => "missing_root",
        ServerPersistenceHealth::MetadataMissing => "metadata_missing",
        ServerPersistenceHealth::MetadataCorrupt => "metadata_corrupt",
        ServerPersistenceHealth::MetadataUnsupported => "metadata_unsupported",
        ServerPersistenceHealth::IdentityConflict => "identity_conflict",
    }
}

fn runtime_name(runtime: &RuntimeClassification) -> &'static str {
    match runtime {
        RuntimeClassification::Stopped => "stopped",
        RuntimeClassification::Controlled { .. } => "controlled",
        RuntimeClassification::UnknownRunning { .. } => "unknown_running",
        RuntimeClassification::Orphaned { .. } => "orphaned",
        RuntimeClassification::ProvenStale { .. } => "proven_stale",
    }
}

fn capture_health_name(health: &CaptureHealth) -> &'static str {
    match health {
        CaptureHealth::Available => "available",
        CaptureHealth::Unavailable { .. } => "unavailable",
    }
}

fn print_eula_status(status: &EulaStatus, json: bool) {
    match status {
        EulaStatus::NotAccepted => {
            if json {
                print_json(
                    &serde_json::json!({ "status": "not_accepted", "reference": MINECRAFT_EULA_REFERENCE }),
                );
            } else {
                println!("not accepted ({MINECRAFT_EULA_REFERENCE})");
            }
        }
        EulaStatus::Accepted {
            accepted_at,
            reference,
        } => {
            if json {
                print_json(&serde_json::json!({
                    "status": "accepted",
                    "accepted_at": accepted_at,
                    "reference": reference,
                }));
            } else {
                println!("accepted at {accepted_at} ({reference})");
            }
        }
        EulaStatus::Malformed => {
            if json {
                print_json(&serde_json::json!({ "status": "malformed" }));
            } else {
                println!("malformed; revoke or accept again interactively");
            }
        }
    }
}

fn print_control_response(response: ControlResponse, json: bool) {
    if json {
        print_json(
            &serde_json::to_value(response)
                .unwrap_or_else(|_| serde_json::json!({ "status": "invalid_response" })),
        );
    } else {
        match response {
            ControlResponse::Running => println!("running"),
            ControlResponse::Accepted => println!("accepted"),
            ControlResponse::Exited(exit) => println!("exited: {exit:?}"),
            ControlResponse::Restarted { pid } => println!("restarted as pid {pid}"),
            ControlResponse::Refused(detail) => println!("refused: {detail}"),
        }
    }
}

fn print_server_commit(record: &ServerRecord, warnings: &[ServerCommitWarning], json: bool) {
    if json {
        let mut value = server_json(record);
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "warnings".to_owned(),
                serde_json::Value::Array(
                    warnings
                        .iter()
                        .map(|warning| match warning {
                            ServerCommitWarning::IndexOutOfSync { detail } => serde_json::json!({
                                "kind": "index_out_of_sync",
                                "detail": detail,
                            }),
                        })
                        .collect(),
                ),
            );
        }
        print_json(&value);
        return;
    }
    print_server(record, false);
    for warning in warnings {
        match warning {
            ServerCommitWarning::IndexOutOfSync { detail } => {
                eprintln!("server committed, but its query index is out of sync: {detail}");
            }
        }
    }
}

fn captured_line_json(line: &CapturedLine) -> serde_json::Value {
    serde_json::json!({
        "timestamp": line.timestamp,
        "stream": line.stream,
        "text": line.text,
        "truncated": line.truncated,
        "omitted_bytes": line.omitted_bytes,
    })
}

fn print_captured_line(line: &CapturedLine) {
    if line.truncated {
        println!(
            "{} [{}] {} [truncated; {} bytes omitted]",
            line.timestamp,
            serde_json::to_value(line.stream)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "output".to_owned()),
            line.text,
            line.omitted_bytes
        );
    } else {
        println!("{} {}", line.timestamp, line.text);
    }
}

fn instance_json(app: &App, record: &ClientRecord) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::json!({
            "id": record.id.to_string(),
            "name": record.name,
            "storage": record.storage.to_string(),
            "minecraft_version": record.minecraft_version,
            "loader": {
                "kind": record.loader.kind.to_string(),
                "version": record.loader.version,
            },
            "root": record.root,
            "source": record.source,
            "attention": record.attention,
            "needs_attention": record.needs_attention(),
            "created_at": record.created_at,
            "last_launched_at": record.last_launched_at,
            // Read from the instance's own launch lock, so it is true across the
            // CLI, the GUI, and a second terminal.
            "running": app.launch_state(record.id)?.map(|state| serde_json::json!({
                "launcher_pid": state.owner_pid,
                "game_pid": state.game_pid,
                "since": state.since,
            })),
            "linked_server_ids": app
    .linked_server_ids(record.id)?
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>(),
        }))
}

fn server_json(record: &ServerRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id.to_string(),
        "name": record.name,
        "storage": record.storage.to_string(),
        "minecraft_version": record.minecraft_version,
        "loader": {
            "kind": record.loader.kind.to_string(),
            "version": record.loader.version,
        },
        "root": record.root,
        "linked_client_id": record.linked_client_id.map(|id| id.to_string()),
        "network": record.network,
        "installation": record.installation,
        "source": record.source,
        "java_override": record.java_override,
        "jvm_override": record.jvm_override,
        "attention": record.attention,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
        "last_started_at": record.last_started_at,
    })
}

/// What this launcher is running, most recently started first.
fn print_running(app: &App, json: bool) -> anyhow::Result<()> {
    let mut running = app.running_processes()?;
    // Two samples are needed before CPU is a rate rather than a guess, and a
    // one-shot command has only one. Memory and the rest are true immediately,
    // so the numbers that are meaningful are still shown.
    ProcessMonitor::new().observe(&mut running);

    if json {
        print_json(&serde_json::json!({
            "running": running.iter().map(|process| serde_json::json!({
                "kind": process.host.kind(),
                "id": process.host.id(),
                "name": process.name,
                "pid": process.pid,
                "owner_pid": process.owner_pid,
                "since": process.since,
                "controllable": process.controllable,
                "memory_bytes": process.usage.map(|usage| usage.memory_bytes),
                "last_line": process.last_line,
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    if running.is_empty() {
        println!("{}", ephervex_i18n::lookup("running-nothing"));
        return Ok(());
    }
    for process in &running {
        let pid = process
            .pid
            .map_or_else(|| "-".to_owned(), |pid| pid.to_string());
        println!(
            "{}  {}  pid {pid}  {}",
            process.host.kind(),
            process.name,
            format_uptime(process.uptime_seconds())
        );
        if let Some(line) = &process.last_line {
            println!("    {line}");
        }
    }
    Ok(())
}

/// How long something has been running, in the largest unit that says it.
fn format_uptime(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return "-".to_owned();
    };
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

fn reconciled_json(records: &[ephervex_core::ReconciledRecord]) -> serde_json::Value {
    serde_json::Value::Array(
        records
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.reference.to_string(),
                    "name": entry.name,
                    "root": entry.root,
                    "storage": entry.storage.to_string(),
                })
            })
            .collect(),
    )
}

/// Prints a `--json` result.
///
/// Indented on purpose: command output is read in a terminal, so readability
/// wins here, unlike the launcher's own stored JSON, which stays compact.
fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        // Serializing a `serde_json::Value` cannot fail in practice, and a
        // half-written document would be worse than a message on stderr.
        Err(error) => tracing::error!(%error, "could not format JSON output"),
    }
}

fn summarize(record: &ClientRecord) -> String {
    ephervex_i18n::lookup_args(
        "instance-summary",
        [
            ("name", MessageArg::from(record.name.as_str())),
            (
                "version",
                MessageArg::from(record.minecraft_version.as_str()),
            ),
            ("loader", MessageArg::from(record.loader.to_string())),
        ],
    )
}

/// Prints a failure as a localized summary, an actionable next step where one
/// exists, and the technical chain underneath.
fn report_failure(error: &anyhow::Error, json: bool) {
    let technical: Vec<String> = error.chain().map(ToString::to_string).collect();
    let message_id = error.downcast_ref::<CoreError>().map(CoreError::message_id);

    let (summary, action) = error.downcast_ref::<CoreError>().map_or_else(
        || (technical.first().cloned().unwrap_or_default(), None),
        localized_failure,
    );

    if json {
        let value = serde_json::json!({
            "message_id": message_id,
            "error": summary,
            "action": action,
            "detail": technical,
        });
        if let Ok(text) = serde_json::to_string_pretty(&value) {
            eprintln!("{text}");
        }
        return;
    }

    eprintln!("error: {summary}");
    if let Some(action) = action {
        eprintln!("       {action}");
    }
    // A failure that is not a `CoreError` has no localized message, so the
    // summary *is* its first technical line. Printing the chain unfiltered
    // would then say the same sentence twice.
    for line in technical.iter().filter(|line| **line != summary) {
        eprintln!("  {line}");
    }
}

/// Turns a typed storage error into user-facing text.
///
/// The error itself never held any: it carries a stable message id and
/// structured fields, and this is where they become a sentence.
fn localized_failure(error: &CoreError) -> (String, Option<String>) {
    let id = error.message_id();

    // Next steps come in three shapes, and each needs different arguments. One
    // names the file to look at; one names a page to visit and the value to
    // paste into it; one, typically a network failure, needs no argument at all
    // and must still be shown.
    //
    // Every available argument is supplied rather than only the ones this
    // message happens to use — Fluent ignores the extras, and the alternative
    // is a branch per combination that silently drops an action the moment a
    // message gains a second placeholder.
    let failing_path = error.failing_path();
    let mut args = Vec::new();
    if let Some(path) = &failing_path {
        args.push(("path", MessageArg::from(path.as_path())));
    }
    if let Some(url) = error.help_url() {
        args.push(("url", MessageArg::from(url)));
    }
    if let Some(subject) = error.help_subject() {
        args.push(("client_id", MessageArg::from(subject)));
    }
    if let Some(syntax) = error.expected_syntax() {
        args.push(("syntax", MessageArg::from(syntax)));
    }
    if let Some(loader) = error.unsupported_loader() {
        args.push(("loader", MessageArg::from(loader)));
    }
    // Failures that name several technical values state their own arguments,
    // so a message gaining a placeholder does not need a branch here.
    let supplied = error.message_args();
    for (key, value) in &supplied {
        args.push((key, MessageArg::from(value.as_str())));
    }

    let summary = if args.is_empty() {
        ephervex_i18n::lookup(id)
    } else {
        ephervex_i18n::lookup_args(id, args.iter().cloned())
    };

    let action = if args.is_empty() {
        // Nothing to substitute, so an action that wanted an argument would
        // render a literal `{$…}` in the user's face.
        ephervex_i18n::lookup_attribute(id, "action").filter(|action| !action.contains("{$"))
    } else {
        ephervex_i18n::lookup_attribute_args(id, "action", args)
            .filter(|action| !action.contains("{$"))
    };

    (summary, action)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn import_args(argv: &[&str]) -> Result<ImportArgs, clap::Error> {
        let mut full = vec!["ephervex", "import"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full)?.command {
            Command::Import(parsed) => Ok(parsed),
            other => panic!("expected an Import command, got {other:?}"),
        }
    }

    #[test]
    fn repeated_optional_flags_collect_every_declared_path_in_order() {
        let args = import_args(&[
            "pack.zip",
            "--optional",
            "mods/a.jar",
            "--optional",
            "mods/b.jar",
        ])
        .expect("valid arguments");
        assert_eq!(
            args.optional,
            vec![PathBuf::from("mods/a.jar"), PathBuf::from("mods/b.jar")]
        );
        assert!(!args.all_optional);
    }

    #[test]
    fn all_optional_needs_no_explicit_path() {
        let args = import_args(&["pack.zip", "--all-optional"]).expect("valid arguments");
        assert!(args.all_optional);
        assert!(args.optional.is_empty());
    }

    #[test]
    fn neither_flag_selects_nothing_by_default() {
        let args = import_args(&["pack.zip"]).expect("valid arguments");
        assert!(!args.all_optional);
        assert!(args.optional.is_empty());
    }

    #[test]
    fn optional_and_all_optional_together_are_rejected_by_argument_parsing() {
        let error = import_args(&["pack.zip", "--optional", "mods/a.jar", "--all-optional"])
            .expect_err("the flags are mutually exclusive");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "the conflict must be caught at the argument-parsing boundary, before any pack is read"
        );
    }
}
