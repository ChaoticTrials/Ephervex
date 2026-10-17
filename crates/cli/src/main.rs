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

use std::fmt::Write as _;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Args, CommandFactory as _, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use ephervex_core::{
    AccountRemoval, AccountStatus, AccountSummary, ApiSettings, App, ArgumentForm, Artifact,
    ArtifactCacheHealth, AssetIndexRef, AuthEvent, AuthObserver, BundleContents, BundleSelection,
    BundledDocumentHealth, CancellationToken, CaptureHealth, CapturedLine, ClientCommit,
    ClientCommitWarning, ClientFolder, ClientRecord, CloneMethod, Compatibility, Config,
    ConfigurationHealth, ControlResponse, CoreError, CrashReview, CreatedClient,
    CredentialBackendHealth, CredentialOutcome, CredentialPresence, CredentialStorage,
    DEFAULT_DIAGNOSTIC_BYTES, DEFAULT_SEARCH_LIMIT, DependencyKnowledge, DependencyRefreshReport,
    DependentMods, DiagnosticEntry, DiagnosticFix, DiagnosticKind, DiagnosticRequest,
    DiagnosticScope, DiagnosticSearch, DirectoryState, DuplicateClient, EULA_CONFIRMATION_PHRASE,
    EntitlementLookup, Entitlements, EulaStatus, ExpectedRuntime, FileDisposition, FindingClass,
    FindingKind, Freshness, GameExit, GroupFilter, HostJavaInstall, HostJavaStatus, ImpactedMod,
    ImportRequest, ImportedPack, IncompatibilityReason, InstallationHealth, InstalledHost,
    InstalledMods, InstanceFilter, InstanceGroup, InstanceSort, Integrity, IsolationRequest,
    IsolationRound, IsolationSession, IsolationStep, JavaCheck, JavaHealth, JavaMismatch,
    JavaPathChoice, JavaPinIndex, JavaPlan, JavaPlanRequest, JavaRefusal, JavaRuntime,
    JavaRuntimeFailure, JavaRuntimePins, JavaRuntimeSummary, JavaSelectionMode, JavaTarget,
    JavaUnavailable, LaunchAndConnectEvent, LaunchAndConnectObserver, LaunchAndConnectOptions,
    LaunchAndConnectOutcome, LaunchEvent, LaunchFeatures, LaunchObserver, LaunchOptions,
    LaunchOutcome, LaunchState, LinkingState, LoginFlow, LoginOptions, LoginRequest,
    MAX_SEARCH_MATCHES, MINECRAFT_EULA_REFERENCE, ManagedInstallation, ManagedMod,
    ManagedModInstallation, ManagedModSnapshot, ModInstallPlan, ModInstallRequest, ModPresence,
    ModProjectDetails, ModRelations, ModSearchQuery, ModSearchResults, NetworkAccess, NewClient,
    NewServer, OperationReporter, OptionalCandidate, OptionalSelection, OptionalStatus,
    OrganizationView, OutputLine, PackAnalysis, PackRecipe, PackSide, Paths, PlanWarning,
    PlannedMod, PreferenceApplication, PreferenceId, PreferenceKey, ProcessMonitor,
    PromotionRequest, PromotionWarning, ReconciliationReport, RefusedJavaChoice, RelatedMod,
    ReleaseListing, RequiredMods, RequirementBasis, RoundAnswer, RunFilter, RuntimeClassification,
    RuntimeInventory, SearchOptions, SecretString, ServerCommitWarning, ServerContentSource,
    ServerCreateOptions, ServerEvent, ServerExit, ServerFolder, ServerNetworkChange,
    ServerObserver, ServerPersistenceHealth, ServerRecord, ServerWarning, Session,
    SharedArtifactReport, SideSupport, SkipReason, SnapshotOrigin, SnapshotRecord,
    SnapshotRestoreOptions, StartedServerLifetime, SuspectConfidence, SuspectReason,
    TerminationRequests, TextAnchor, TextView, UnknownServerFiles, UnverifiedOptional,
    VerificationReport, VersionMetadata, VersionOrder, VersionRecord, format_binary_bytes,
};
use ephervex_i18n::Arg as MessageArg;
use ephervex_model::{
    AbsentModPolicy, DefaultModCatalog, InstanceId, JavaOverride, JvmSettings, LoaderKind,
    LoaderSelection, ModProvider, OperationId, ServerId, SnapshotId, StorageClass,
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
    /// Manage the groups instances are sorted into.
    ///
    /// A group is a plain label for finding instances. It never moves an
    /// instance's files or changes its id.
    Groups {
        #[command(subcommand)]
        command: GroupsCommand,
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
    /// Manage and troubleshoot mods in client instances and local servers.
    Mod {
        #[command(subcommand)]
        command: ModCommand,
    },
    /// Capture and restore local client or server state.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Inspect launcher, client, and server logs and crash reports.
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// Make Ephervex open ephervex:// links from your browser, or stop it.
    ///
    /// Writes a desktop entry of Ephervex's own into your applications folder
    /// and asks your desktop to prefer it for the scheme. Nothing else is
    /// touched: the entry Ephervex ships is left alone, and removing this one
    /// undoes the whole thing.
    ///
    /// Only run this when you want it. Claiming a link scheme is not something
    /// Ephervex does on its own.
    UrlHandler {
        #[command(subcommand)]
        command: UrlHandlerCommand,
    },
    /// Import a modpack into a client instance, from a file or an address.
    Import(ImportArgs),
    /// Install and launch a client instance, streaming its log to the terminal.
    ///
    /// Exits with a non-zero status when the game exits with an error or from a
    /// signal, unless the launch was ended with Ctrl-C, a terminal hangup, a
    /// termination signal, or output that is no longer being read. A dry run
    /// exits with 0.
    Launch(LaunchArgs),
    /// List everything Ephervex is currently running.
    ///
    /// Client instances with a live launch, and servers this launcher is still
    /// supervising. A server whose supervisor is gone is a recovery case: see
    /// `ephervex server recover`.
    Running,
    /// Reconcile the index against what is actually on disk.
    Reconcile,
    /// Show the open-source software Ephervex is built on, and its licenses.
    ///
    /// Local only: the list is built into this release, so it describes exactly
    /// what this copy of Ephervex contains and contacts nothing.
    ///
    /// Ephervex's own license is Apache-2.0; this covers its dependencies. The
    /// complete text of every one of them also ships beside the program, as
    /// THIRD-PARTY-LICENSES.md.
    Licenses(LicensesArgs),
}

#[derive(Debug, Args)]
struct LicensesArgs {
    /// List every dependency and the license it is under.
    ///
    /// Without this, only a count per license is shown, because a release
    /// depends on several hundred packages.
    #[arg(long)]
    all: bool,

    /// Print the full text of one license, named by its identifier.
    ///
    /// Identifiers are what the summary lists, for example `MIT` or
    /// `Apache-2.0`. A license several packages ship under slightly different
    /// copyright lines has one text printed per distinct version.
    #[arg(long, value_name = "LICENSE")]
    text: Option<String>,
}

#[derive(Debug, Args)]
struct LaunchArgs {
    /// The client instance to launch.
    id: InstanceId,
    /// Start or reuse this linked server and connect the client to it. A server
    /// this command starts is stopped when the command ends.
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
    /// The pack archive, as a file or as an https address.
    ///
    /// The format is detected from its contents, not its name: a `.mrpack`
    /// renamed to `.zip` still imports. An address is downloaded first, and
    /// Ephervex asks before it downloads from a host for the first time.
    #[arg(value_name = "ARCHIVE_OR_URL")]
    archive: String,
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
    /// Agree to download from this address without being asked.
    ///
    /// Only meaningful when the pack is named by an address. Without it, an
    /// address whose host has not been agreed to before prompts, and refuses
    /// when there is no terminal to prompt on.
    #[arg(long, value_enum, value_name = "SCOPE")]
    trust: Option<TrustArg>,
}

/// How far an answer about a download address goes.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum TrustArg {
    /// Download from it this once, and ask again next time.
    Once,
    /// Download from it and remember the host, so the next import from the
    /// same site asks nothing.
    Always,
}

impl TrustArg {
    const fn consent(self) -> ephervex_core::HostConsent {
        match self {
            Self::Once => ephervex_core::HostConsent::JustOnce,
            Self::Always => ephervex_core::HostConsent::Remember,
        }
    }
}

/// Which mod catalog a command reads.
///
/// CurseForge needs your own API key, stored with
/// `ephervex api set-curseforge-key`.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CatalogArg {
    Modrinth,
    // Spelled the way CurseForge spells it, rather than the hyphenated name
    // derived from the variant.
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

// `mod search --limit` names this ceiling in its help.
const _: () = assert!(ephervex_core::MAX_SEARCH_LIMIT == 25);

/// Mods are searched for, inspected, and installed against one instance,
/// because whether a mod fits depends entirely on that instance's exact
/// Minecraft version and mod loader.
#[derive(Debug, Subcommand)]
enum ModCommand {
    /// Search a mod catalog for mods that fit a client or a server.
    ///
    /// Results are judged against the exact Minecraft version and loader of
    /// the instance or server you name. Searching for a server leaves out mods
    /// that declare they do not run on one.
    Search {
        #[command(flatten)]
        host: HostArgs,
        /// What to search for.
        query: String,
        /// Which catalog to search. Defaults to your preferred catalog.
        #[arg(long, value_enum)]
        provider: Option<CatalogArg>,
        /// Show at most this many results, from 1 to 25. A larger number
        /// shows 25.
        #[arg(
            long,
            default_value_t = DEFAULT_SEARCH_LIMIT,
            value_parser = parse_search_limit
        )]
        limit: u8,
        /// Skip this many results, to page further into a long list.
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Also list mods that do not fit, with the reason each one does not.
        ///
        /// They are shown so you can see why the mod you were looking for is
        /// not offered. They still cannot be installed. For a server this
        /// includes mods that only run on a client.
        #[arg(long)]
        include_incompatible: bool,
    },
    /// Show a mod and the versions available for a client or a server.
    Show {
        #[command(flatten)]
        host: HostArgs,
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
        #[command(flatten)]
        host: HostArgs,
        /// The mod, by its project id or slug in the chosen catalog.
        project: String,
        #[command(flatten)]
        selection: ModSelectionArgs,
    },
    /// Install a mod and everything it requires.
    ///
    /// The newest version that fits the instance or server is chosen unless
    /// you name one. Required dependencies are installed with it; optional ones
    /// are not, unless you ask for them.
    ///
    /// Installing onto a server touches that server only, and works with no
    /// linked client. A mod that runs only on a client is refused, and a
    /// required dependency that runs only on a client is installed with a
    /// warning because the mod that needs it cannot load without it. A running
    /// server picks the change up on its next start.
    Install {
        #[command(flatten)]
        host: HostArgs,
        /// The mod, by its project id or slug in the chosen catalog.
        project: String,
        #[command(flatten)]
        selection: ModSelectionArgs,
        #[command(flatten)]
        consent: InstallConsentArgs,
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
    /// Check that every file a client or server records is there, with the
    /// right bytes.
    ///
    /// The recipe already carries the digest each provider published, so this
    /// reaches no network and changes nothing. It answers the question looking
    /// for the file cannot: a jar truncated by a full disk, or left half
    /// written when a machine lost power, is present and still wrong.
    ///
    /// A file the recipe records no digest for is reported as unproven rather
    /// than sound or broken. A pack that shipped a jar inside itself, rather
    /// than pointing at one, published no hash for it and never will.
    Verify {
        #[command(flatten)]
        host: HostArgs,
        /// Also check the shared libraries and client JAR this instance
        /// launches from.
        ///
        /// Those live in a store every instance shares, on Maven-style paths
        /// rather than under their own digests, so nothing re-checks them once
        /// they are written. Their digests come from the release's metadata,
        /// which has to have been cached already - launch the instance once if
        /// it has not. Only a client instance has them, so this cannot be
        /// combined with --server.
        #[arg(long, requires = "instance", conflicts_with = "server")]
        shared: bool,
    },
    /// Replace every file a client or server is missing or holding wrongly.
    ///
    /// Checks first, so nothing sound is rewritten, and fetches each
    /// replacement before touching anything on disk, so a file that cannot be
    /// obtained is left exactly where it is rather than removed.
    ///
    /// A file the pack shipped inside itself has no address to go back to, and
    /// a server built from published server files records none for its jars.
    /// Those are named separately, because trying again does not bring them
    /// back: re-import the pack for a client, or put the file back in the
    /// server folder by hand.
    ///
    /// Refused while the host is running or an isolation session is active.
    Repair {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Show what each mod needs, and what needs it.
    ///
    /// Answered from what is recorded here, so it reaches no catalog and works
    /// offline. A mod nothing has asked about says so rather than appearing to
    /// need nothing; `ephervex mod refresh` is what answers for it.
    ///
    /// Relations are shown exactly as the catalog stated them, including the
    /// optional and incompatible ones, which no change ever acts on. Only the
    /// direct ones: this says what was recorded about a mod, not what a whole
    /// cascade would reach.
    Dependencies {
        #[command(flatten)]
        host: HostArgs,
        /// One mod's relations instead of every mod's, by its path inside the
        /// game directory, such as `mods/foo.jar`.
        #[arg(long, value_name = "PATH")]
        of: Option<PathBuf>,
    },
    /// Ask each catalog what a client's or server's mods need, and record the
    /// answers.
    ///
    /// Ephervex records what a mod requires when it installs it, and when it
    /// imports a CurseForge pack. Anything else starts out with nothing
    /// recorded: a Modrinth pack states no dependencies at all, and an instance
    /// built by an older Ephervex has none. Run this once and the instance can
    /// answer what breaks when a mod is turned off, offline and from then on.
    ///
    /// A catalog that cannot be reached costs only its own mods' answers. Mods
    /// it could not answer for stay unrecorded rather than being recorded as
    /// needing nothing. The other catalogs' answers are still recorded, and the
    /// command then exits with an error naming the catalogs to retry.
    Refresh {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Narrow a reproducing mod set by testing dependency-safe configurations.
    Isolate {
        #[command(subcommand)]
        command: IsolateCommand,
    },
    /// Take a mod's jar off disk, keeping the record that wants it.
    ///
    /// Not the same as removing it. The instance still says it should have
    /// this mod, so the row offers to put it back, a server built from this
    /// recipe still contains it, and a repair fetches it again. What changes
    /// is that the loader cannot see it, which is how a crash gets bisected
    /// down to one jar.
    ///
    /// If other mods need this one, their jars go too, because leaving them
    /// installed and unable to load is the state worth avoiding. Ephervex names
    /// them and stops, and it also stops for mods that use this one optionally
    /// or whose requirements are not recorded, since it cannot tell whether
    /// they need it. `--with-dependents` takes the mods that need it as well,
    /// and `--only-this` takes just the jar you named.
    Delete {
        #[command(flatten)]
        host: HostArgs,
        /// The mod's path inside the game directory, such as `mods/foo.jar`.
        path: PathBuf,
        /// Also delete every mod that needs this one.
        #[arg(long, conflicts_with = "only_this")]
        with_dependents: bool,
        /// Delete only the jar named, leaving what needs it unable to load.
        #[arg(long)]
        only_this: bool,
    },
    /// Let a mod load again after it was disabled.
    ///
    /// A mod that needs others usually cannot load alone. Ephervex says which
    /// of them are turned off or not installed, and which installed mods it
    /// is declared incompatible with, and stops; `--with-dependencies` turns
    /// the ones that are turned off on too, and `--only-this` turns on just
    /// the mod you named.
    Enable {
        #[command(flatten)]
        host: HostArgs,
        /// The mod's path inside the game directory, such as `mods/foo.jar`.
        path: PathBuf,
        /// Also turn on every mod this one needs.
        #[arg(long, conflicts_with = "only_this")]
        with_dependencies: bool,
        /// Turn on only the mod named, leaving what it needs turned off.
        #[arg(long)]
        only_this: bool,
    },
    /// Keep a mod installed but stop the loader from loading it.
    ///
    /// The jar is renamed to `<name>.jar.disabled`, which every loader ignores.
    /// Nothing is deleted and nothing is forgotten: the mod keeps its recorded
    /// version and provider, and enabling it again needs no download.
    ///
    /// If other mods need this one, they stop loading too. Ephervex names them
    /// and stops without changing anything, and it also stops for mods that use
    /// this one optionally or whose requirements are not recorded, since it
    /// cannot tell whether they need it. `--with-dependents` turns off the mods
    /// that need it as well, and `--only-this` turns off just the mod you
    /// named, which leaves the others installed but possibly unable to load.
    Disable {
        #[command(flatten)]
        host: HostArgs,
        /// The mod's path inside the game directory, such as `mods/foo.jar`.
        path: PathBuf,
        /// Also turn off every mod that needs this one.
        #[arg(long, conflicts_with = "only_this")]
        with_dependents: bool,
        /// Turn off only the mod named, leaving what needs it unable to load.
        #[arg(long)]
        only_this: bool,
    },
}

#[derive(Debug, Subcommand)]
enum IsolateCommand {
    /// Capture a safety snapshot and apply the first test configuration.
    Start {
        #[command(flatten)]
        host: HostArgs,
        /// Keep this mod and everything it requires enabled in every round.
        #[arg(long = "keep", value_name = "PATH")]
        pinned: Vec<PathBuf>,
        /// Continue even though refreshable dependency information is absent.
        ///
        /// The final result is marked as reduced confidence because a mod that
        /// appears independent may have an unrecorded required dependency.
        #[arg(long)]
        accept_unrecorded: bool,
    },
    /// Show the durable session without changing its current configuration.
    Status {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Reapply the current round after a restart or interrupted mutation.
    Resume {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Record whether the current configuration still reproduces the issue.
    Answer {
        #[command(flatten)]
        host: HostArgs,
        #[arg(value_enum)]
        answer: IsolationAnswerArg,
    },
    /// Remove the latest answer and restore that round for another test.
    Undo {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Keep the smallest tested configuration and retain the safety snapshot.
    Finish {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Restore the pre-isolation snapshot and end the session.
    Cancel {
        #[command(flatten)]
        host: HostArgs,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum IsolationAnswerArg {
    /// The tested configuration still shows the issue.
    #[value(name = "issue-still-present", alias = "reproduced")]
    IssueStillPresent,
    /// The issue is gone in the tested configuration.
    #[value(name = "issue-gone", alias = "did-not-reproduce")]
    IssueGone,
}

impl IsolationAnswerArg {
    const fn answer(self) -> RoundAnswer {
        match self {
            Self::IssueStillPresent => RoundAnswer::Reproduced,
            Self::IssueGone => RoundAnswer::DidNotReproduce,
        }
    }
}

/// Which installed local host an operation targets.
///
/// One required group rather than a requirement on `--instance` alone, so the
/// usage line offers `--server` as well.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct HostArgs {
    /// The client instance to operate on.
    #[arg(long, value_name = "INSTANCE")]
    instance: Option<InstanceId>,
    /// The dedicated server to operate on.
    #[arg(long, value_name = "SERVER")]
    server: Option<ServerId>,
}

impl HostArgs {
    /// The named host.
    ///
    /// `clap` has already refused both and neither, so the remaining
    /// combination cannot occur. It is reported rather than asserted, so that
    /// a change to the argument definitions above cannot turn into a panic.
    fn host(&self) -> anyhow::Result<InstalledHost> {
        match (self.instance, self.server) {
            (Some(instance), None) => Ok(InstalledHost::Instance(instance)),
            (None, Some(server)) => Ok(InstalledHost::Server(server)),
            _ => Err(anyhow::anyhow!(
                "name exactly one of --instance or --server"
            )),
        }
    }
}

#[derive(Debug, Subcommand)]
enum SnapshotCommand {
    /// List snapshots retained inside a client or server.
    List {
        #[command(flatten)]
        host: HostArgs,
    },
    /// Capture the host's complete local development state.
    Create {
        #[command(flatten)]
        host: HostArgs,
        /// A short name shown in snapshot lists.
        label: String,
    },
    /// Replace the live host state with one snapshot.
    ///
    /// Files created since the snapshot are removed because restore replaces
    /// the captured state rather than merging it.
    Restore {
        #[command(flatten)]
        host: HostArgs,
        /// The snapshot to restore.
        id: SnapshotId,
        /// Skip the default snapshot of the current state.
        #[arg(long)]
        no_safety_snapshot: bool,
        /// Confirm replacement of the live state.
        #[arg(long)]
        yes: bool,
    },
    /// Change a snapshot's display label.
    Rename {
        #[command(flatten)]
        host: HostArgs,
        /// The snapshot to rename.
        id: SnapshotId,
        /// The new display label.
        label: String,
    },
    /// Permanently delete one retained snapshot.
    Delete {
        #[command(flatten)]
        host: HostArgs,
        /// The snapshot to delete.
        id: SnapshotId,
        /// Confirm permanent deletion.
        #[arg(long)]
        yes: bool,
    },
}

/// What the user agrees to for one install.
#[derive(Debug, Args)]
struct InstallConsentArgs {
    /// Install onto a server even when the catalog does not say whether the
    /// mod, or a mod it needs, runs on a server.
    ///
    /// Without it, such an install is refused and the mods are named.
    #[arg(long)]
    accept_unknown_server_support: bool,
    /// Let a jar already in the mods folder stand in for the dependency with
    /// this project id, when that jar provides the same mod.
    ///
    /// Repeatable. The dependency is still downloaded and checked, and is left
    /// out only if one jar already there declares every mod id it does. On a
    /// server it then needs no --accept-unknown-server-support. Without it, a
    /// dependency that another jar already provides is refused, because the
    /// game does not start with two copies of one mod.
    #[arg(long = "use-installed", value_name = "PROJECT")]
    use_installed: Vec<String>,
}

/// Which version, and which optional extras, an install should take.
#[derive(Debug, Args)]
struct ModSelectionArgs {
    /// Which catalog the mod is on. Defaults to your preferred catalog.
    #[arg(long, value_enum)]
    provider: Option<CatalogArg>,
    /// Install this exact version instead of the newest compatible one.
    ///
    /// Preserved exactly. If it does not fit the instance or server, the
    /// install is refused rather than moved to a nearby release.
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
    /// Show or forget the sites you have agreed to download modpacks from.
    ///
    /// A site gets on this list when you confirm the address a link named.
    /// Being on it means Ephervex may download an archive from that site
    /// without asking again, and nothing else: a pack's own file downloads are
    /// restricted to the same hosts they always were.
    PackHosts {
        /// Stop trusting this site. Repeatable.
        #[arg(long = "forget", value_name = "HOST")]
        forget: Vec<String>,
        /// Stop trusting every site on the list.
        #[arg(long, conflicts_with = "forget")]
        forget_all: bool,
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
    /// List the Java runtimes found on this computer, and the hosts and the
    /// launcher preference that pin each one.
    List,
    /// Rescan for Java runtimes, ignoring the cached result.
    Refresh,
    /// Probe one Java executable and report what it is.
    Test { path: PathBuf },
    /// Show which runtime would run a Minecraft version, and why.
    ///
    /// With `--instance` or `--server`, shows the runtime that host would
    /// launch with, at its own Minecraft version.
    Select {
        /// Minecraft version id, e.g. `1.12.2`. Optional with `--instance` or
        /// `--server`; when given, it must be that host's version.
        #[arg(required_unless_present_any = ["instance", "server"])]
        minecraft: Option<String>,
        /// Plan for this instance: its version, loader, and Java and JVM
        /// overrides.
        #[arg(long)]
        instance: Option<InstanceId>,
        /// Plan for this server: its version, loader, and Java and JVM
        /// overrides.
        #[arg(long, conflicts_with = "instance")]
        server: Option<ServerId>,
        /// Use this runtime instead of selecting one. Outranks every setting.
        #[arg(long)]
        java_path: Option<PathBuf>,
        /// Download a managed runtime if nothing installed is compatible.
        #[arg(long)]
        install: bool,
    },
    /// Download and install a managed Temurin runtime.
    ///
    /// With `--instance` or `--server`, installs the Java that host expects:
    /// its required version, no later than the update its loader accepts. The
    /// host keeps its Java setting; when it would not use the new runtime on its
    /// own, the command prints how to pin it.
    Install {
        /// Java major version, e.g. `21`.
        #[arg(required_unless_present_any = ["instance", "server"])]
        major: Option<u32>,
        /// Install the Java this instance expects.
        #[arg(long, conflicts_with_all = ["major", "server"])]
        instance: Option<InstanceId>,
        /// Install the Java this server expects.
        #[arg(long, conflicts_with_all = ["major", "instance"])]
        server: Option<ServerId>,
    },
}

#[derive(Debug, Subcommand)]
enum InstancesCommand {
    /// List client instances, optionally narrowed and ordered.
    ///
    /// Every filter that is given must match. Favorites come first unless
    /// `--sort` or the interface's chosen order says otherwise. Equal values
    /// are ordered newest first, then by id, so the order is stable.
    List(InstanceListArgs),
}

#[derive(Debug, Args)]
struct InstanceListArgs {
    /// Only instances whose name, Minecraft version, loader, source or group
    /// contains this text, without regard to case.
    #[arg(long, value_name = "TEXT")]
    search: Option<String>,
    /// Only favorites.
    #[arg(long)]
    favorite: bool,
    /// Only instances in this group.
    #[arg(long, value_name = "NAME", conflicts_with = "ungrouped")]
    group: Option<String>,
    /// Only instances that are in no group.
    #[arg(long)]
    ungrouped: bool,
    /// Only instances on this exact Minecraft version.
    #[arg(long, value_name = "VERSION")]
    minecraft: Option<String>,
    /// Only instances using this loader.
    #[arg(long, value_enum)]
    loader: Option<LoaderArg>,
    #[command(flatten)]
    run_state: RunStateArgs,
    /// Only instances with this relation to linked servers: `paired` has a
    /// paired server, `servers` has linked servers but none paired, `none` has
    /// no linked server.
    #[arg(long, value_enum)]
    linking: Option<LinkingArg>,
    /// Order the list by this key. Defaults to the order chosen in the
    /// interface, which starts as `favorite`.
    ///
    /// `favorite` puts favorites first, `name` ignores case and orders numbers
    /// by value ("test 2" before "test 10"), `minecraft` puts
    /// the newest release first, and `loader` orders by loader and then by
    /// newest loader version.
    #[arg(long, value_enum)]
    sort: Option<SortArg>,
}

#[derive(Debug, Args)]
struct RunStateArgs {
    /// Only instances that are launching or running.
    #[arg(long, conflicts_with = "stopped")]
    running: bool,
    /// Only instances that are neither launching nor running.
    #[arg(long)]
    stopped: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SortArg {
    Favorite,
    Name,
    Minecraft,
    Loader,
}

impl From<SortArg> for InstanceSort {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Favorite => Self::Favorite,
            SortArg::Name => Self::Name,
            SortArg::Minecraft => Self::Minecraft,
            SortArg::Loader => Self::Loader,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LinkingArg {
    Paired,
    Servers,
    None,
}

impl From<LinkingArg> for LinkingState {
    fn from(value: LinkingArg) -> Self {
        match value {
            LinkingArg::Paired => Self::Paired,
            LinkingArg::Servers => Self::LinkedServers,
            LinkingArg::None => Self::None,
        }
    }
}

#[derive(Debug, Subcommand)]
enum GroupsCommand {
    /// List every group and how many instances it holds.
    List,
    /// Create an empty group.
    ///
    /// Names are compared without regard to case, so `Bisect` and `bisect`
    /// are the same group.
    Create {
        /// The new group's name.
        name: String,
    },
    /// Rename a group. Its instances stay in it.
    Rename {
        /// The group's current name, without regard to case.
        name: String,
        /// The name it takes.
        new_name: String,
    },
    /// Delete a group. Its instances become ungrouped and are otherwise
    /// untouched.
    Delete {
        /// The group's name, without regard to case.
        name: String,
    },
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
        /// Which side to plan for. The server plan lists what "Create Server"
        /// installs by default, including mods the pack does not say belong on
        /// a server.
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
    /// An option changes only its own value and keeps the others as pinned.
    /// With no options, prints what is pinned, the Java runtime the instance
    /// would run with, the Java its Minecraft and loader expect, and whether
    /// the two fit.
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
        // Hyphen values are allowed because every JVM argument begins with one.
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
    ///
    /// Its linked servers are deleted with it, and so are their worlds unless
    /// `--keep-files` is given. A running one refuses the deletion unless
    /// `--stop-linked-servers` is given.
    ///
    /// Unless `--keep-files` is given, this asks for confirmation first,
    /// naming the instance and every linked server that goes with it. Without
    /// a terminal to ask on, pass `--yes`. The servers that were deleted are
    /// listed afterwards.
    Delete {
        id: InstanceId,
        /// Leave the instance directory on disk.
        #[arg(long)]
        keep_files: bool,
        /// Delete the instance and its linked servers without asking.
        #[arg(long)]
        yes: bool,
        /// Stop running linked servers first instead of refusing. With
        /// `--keep-files` they are shut down cleanly where they allow it, and
        /// killed otherwise.
        #[arg(long)]
        stop_linked_servers: bool,
    },
    /// Keep a temporary instance for good, by moving it out of temporary
    /// storage.
    ///
    /// The instance keeps its name, its mods, its saves, its logs and its
    /// linked servers. Only its location changes, from the system temporary
    /// directory, which the operating system clears whenever it likes, to your
    /// persistent instance folder. This is not the same as duplicating it into
    /// that folder, which would leave the original behind.
    ///
    /// Every temporary server linked to the instance moves with it unless you
    /// say otherwise. This cannot be undone by running it in reverse: delete
    /// the instance when you no longer want to keep it.
    ///
    /// The command exits with an error when the instance moved but a server you
    /// asked to keep could not be moved. It is still temporary then, and the
    /// output says why.
    Persist {
        id: InstanceId,
        /// Move only this linked server. Repeatable, and naming any server at
        /// all leaves every server you did not name where it is.
        #[arg(long = "server", value_name = "SERVER", conflicts_with = "no_servers")]
        servers: Vec<ServerId>,
        /// Move the instance alone and leave its linked servers temporary.
        #[arg(long)]
        no_servers: bool,
    },
    /// Mark an instance as a favorite so it lists first.
    Favorite { id: InstanceId },
    /// Remove an instance's favorite mark.
    Unfavorite { id: InstanceId },
    /// Put an instance in a group, or take it out of its group with `--none`.
    ///
    /// The group must already exist; see `ephervex groups create`. Grouping
    /// never moves the instance or changes its id.
    Group {
        id: InstanceId,
        /// The group's name, without regard to case.
        #[arg(required_unless_present = "none")]
        name: Option<String>,
        /// Leave the instance in no group.
        #[arg(long, conflicts_with = "name")]
        none: bool,
    },
    /// Set an instance's icon from a PNG or JPEG file, or remove it with
    /// `--clear`.
    ///
    /// The image is checked, scaled down to at most 256 pixels and stored by
    /// content; the original file is not kept or referenced. Without an icon,
    /// the instance shows its loader's symbol. `--json` reports the stored
    /// file as `icon`.
    Icon {
        id: InstanceId,
        /// The image file to use.
        #[arg(required_unless_present = "clear")]
        path: Option<PathBuf>,
        /// Remove the icon.
        #[arg(long, conflicts_with = "path")]
        clear: bool,
    },
}

#[derive(Debug, Subcommand)]
enum UrlHandlerCommand {
    /// Show whether Ephervex currently opens its own links.
    ///
    /// This is where link handling is turned on and off. The desktop
    /// application has no switch for it: claiming a system-wide scheme is part
    /// of installing, and the Debian package does it for you.
    Status,
    /// Open ephervex:// links with this copy of Ephervex.
    ///
    /// Run it again after moving the AppImage: the entry records where the
    /// launcher was, and a moved one leaves a link that resolves to nothing.
    Register,
    /// Stop opening ephervex:// links.
    ///
    /// Removes this user's own entry. A handler your package manager installed
    /// stays where it is; removing the Ephervex package is what takes that one
    /// back.
    Unregister,
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
    /// Collect diagnostics into one archive to attach to a bug report.
    ///
    /// Nothing is uploaded: the archive is written where you say and goes no
    /// further than you take it.
    ///
    /// Files go in exactly as they are on disk. A crash report that has been
    /// tidied is a crash report missing the line somebody needed, so Ephervex
    /// does not tidy one. What it guarantees instead is that it adds no secret
    /// of its own, and the configuration file is never included.
    ///
    /// Without `--include`, everything on offer goes in. Run `ephervex logs
    /// bundle --list` to see what that is, then name the parts you want.
    Bundle {
        #[command(flatten)]
        scope: DiagnosticScopeArgs,
        /// Where to write the archive.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Take only these parts, by the names `--list` prints. Repeatable.
        #[arg(long, value_name = "NAME")]
        include: Vec<String>,
        /// Show what could go in, with what each part costs, and write nothing.
        #[arg(long)]
        list: bool,
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
        /// The server to install.
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
    Show {
        /// The server to show.
        id: ServerId,
    },
    /// Show or change this server's own Java and JVM settings.
    ///
    /// Each value falls back to the global setting until it is pinned here.
    /// An option changes only its own value and keeps the others as pinned.
    /// With no options, prints what is pinned, the Java runtime the server
    /// would run with, the Java its Minecraft and loader expect, and whether
    /// the two fit. A change needs the server to be stopped.
    Runtime {
        /// The server to show or change.
        id: ServerId,
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
        // Hyphen values are allowed because every JVM argument begins with one.
        #[arg(long = "jvm-arg", value_name = "ARG", allow_hyphen_values = true)]
        jvm_args: Vec<String>,
        /// Remove every override and follow the global settings again.
        #[arg(
            long,
            conflicts_with_all = ["java", "min_memory", "max_memory", "jvm_args"]
        )]
        clear: bool,
    },
    /// Mark a server as a favorite so it lists first in the interface.
    Favorite {
        /// The server to mark as a favorite.
        id: ServerId,
    },
    /// Remove a server's favorite mark.
    Unfavorite {
        /// The server to remove the mark from.
        id: ServerId,
    },
    /// Set a server's icon from a PNG or JPEG file, or remove it with
    /// `--clear`.
    ///
    /// The image is checked, scaled down to at most 256 pixels and stored by
    /// content. `--json` reports the stored file as `icon`.
    Icon {
        /// The server whose icon to set.
        id: ServerId,
        /// The image file to use.
        #[arg(required_unless_present = "clear")]
        path: Option<PathBuf>,
        /// Remove the icon.
        #[arg(long, conflicts_with = "path")]
        clear: bool,
    },
    /// List the mods a server has, and whether the loader will load each one.
    ///
    /// A server reconstructed from a client pack carries whatever the pack
    /// declared, including mods that only ever run on a client. Use
    /// `ephervex mod disable --server` to leave one out without removing it.
    Mods {
        /// The server whose mods to list.
        id: ServerId,
    },
    /// Explain the newest crash and name the mods it points at.
    ///
    /// Reads the server's most recent crash report, and the tail of its log,
    /// and lists the mods they attribute the failure to, most confident first.
    /// Nothing is changed: use `ephervex mod disable --server` to act on it.
    Crash {
        /// The server whose newest crash to explain.
        id: ServerId,
    },
    /// Show persistence, installation, runtime, and capture status.
    Status {
        /// The server to show.
        id: ServerId,
    },
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
    ///
    /// Ctrl-C, a terminal hangup, a termination signal, or output that is no
    /// longer being read stops the server gracefully; Ctrl-C or a signal again
    /// kills the server. Exits with a non-zero status when the server exits before it finished
    /// starting or ends with an error without having been asked to stop.
    Start {
        /// The server to start.
        id: ServerId,
        #[arg(long)]
        java: Option<PathBuf>,
    },
    /// Request a graceful stop and wait up to 30 seconds.
    ///
    /// Exits with a non-zero status when the running server refused the request.
    Stop {
        /// The server to stop.
        id: ServerId,
    },
    /// Gracefully stop and respawn through the live supervisor.
    ///
    /// Exits with a non-zero status when the request was refused or the server
    /// stopped but could not be started again.
    Restart {
        /// The server to restart.
        id: ServerId,
    },
    /// Force termination through the live owner channel.
    ///
    /// Exits with a non-zero status when the running server refused the request.
    Kill {
        /// The server to kill.
        id: ServerId,
    },
    /// Send one line to the live server console.
    ///
    /// Exits with a non-zero status when the server did not accept the line.
    Console {
        /// The server to send the line to.
        id: ServerId,
        /// The console line, as typed into the server console.
        command: String,
    },
    /// Show retained combined launcher output.
    Output {
        /// The server whose output to show.
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
        /// The server whose folder to open.
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
        /// The server to delete.
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
    ///
    /// The Minecraft release and loader must match the ones the archive was
    /// made for. An archive with mods needs a loader, and one whose installer
    /// jar or libraries name a loader build is refused for any other release,
    /// loader, or build.
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
    Show {
        /// The server whose properties to show.
        id: ServerId,
    },
    /// Set an untyped property; managed network and EULA keys are refused.
    ///
    /// A stopped server reads the value when it next starts. A server running
    /// under Ephervex keeps running and reads it once restarted; use
    /// `ephervex server restart` when you want that.
    Set {
        /// The server whose property to set.
        id: ServerId,
        /// The property key, as written in `server.properties`.
        key: String,
        /// The value to store.
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
#[command(group(
    clap::ArgGroup::new("change")
        .required(true)
        .multiple(true)
        .args(["online_mode", "access", "port", "address"])
))]
struct ServerNetworkArgs {
    /// The server to change.
    id: ServerId,
    /// Require authenticated Minecraft accounts. Pass `--online-mode=false` to turn it off.
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    online_mode: Option<bool>,
    /// Network exposure for the server listener.
    #[arg(long, value_enum)]
    access: Option<NetworkAccessArg>,
    /// TCP port; zero is invalid.
    #[arg(long)]
    port: Option<u16>,
    /// IP address for `--access custom`; the stored address is kept when omitted.
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
    Status {
        /// The server to inspect.
        id: ServerId,
    },
    /// Clear runtime evidence only after every recorded process is proven dead.
    Clear {
        /// The server whose runtime evidence to clear.
        id: ServerId,
    },
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

/// A page size for `mod search`: zero is refused, and anything above the
/// catalog page size is left for the search to cap.
fn parse_search_limit(text: &str) -> Result<u8, String> {
    match text.parse::<u8>() {
        Ok(0) => Err("expected at least 1".to_owned()),
        Ok(limit) => Ok(limit),
        Err(_) => Err("expected a whole number from 1 to 255".to_owned()),
    }
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

/// Writes one line to standard output, through the pipe-safe writer.
///
/// A drop-in for `outln!`, and the reason there is no `outln!` left in this
/// file: that macro panics when whoever was reading has gone, which turns
/// `ephervex ... | head` into a stack trace. One spelling for all of it means a
/// new output line cannot reintroduce the problem by being written the obvious
/// way.
macro_rules! outln {
    () => { write_out(b"\n") };
    ($($arg:tt)*) => { write_out(format!("{}\n", format_args!($($arg)*)).as_bytes()) };
}

/// The same without the newline, for a prompt that waits on the same line.
macro_rules! out {
    ($($arg:tt)*) => { write_out(format!("{}", format_args!($($arg)*)).as_bytes()) };
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

    let outcome = run(&cli);
    report_organization_notices();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_failure(&error, cli.json);
            ExitCode::FAILURE
        }
    }
}

/// Writes bytes to standard output, treating a closed consumer as a normal end.
///
/// `head`, `less` and `grep -q` all stop reading before a long document is
/// finished, and the kernel answers the next write with `EPIPE`. Rust ignores
/// `SIGPIPE` at startup, so that error reaches the writer instead of killing
/// the process, and `outln!` turns it into a panic: a stack trace, exit 101,
/// and a build path in the message, for what is an ordinary way to use a
/// command.
///
/// Nothing here restores the signal's default, because that needs an `unsafe`
/// call and this workspace denies unsafe code in handwritten Rust. Handling the
/// error where the writing happens costs one function and does not weaken that
/// rule.
///
/// A command that supervises a child (`launch`, `server start`) registers
/// itself with [`watch_stdout`]. Ending the process here would skip the
/// destructors that stop the child, so for those commands a failed write
/// cancels the command and later output is dropped instead.
fn write_out(data: &[u8]) {
    use std::io::Write as _;

    let watch = STREAMING_STDOUT.get();
    if watch.is_some_and(StdoutWatch::is_closed) {
        return;
    }
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    match handle.write_all(data).and_then(|()| handle.flush()) {
        Ok(()) => {}
        Err(error) => {
            if let Some(watch) = watch {
                watch.record(&error);
                return;
            }
            // Whoever was reading has gone. There is nobody left to report to,
            // and the work this command was asked for is done as far as anyone
            // can tell.
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
            tracing::error!(%error, "could not write to standard output");
            std::process::exit(1);
        }
    }
}

/// The standard output of a command that keeps a child running while it prints.
static STREAMING_STDOUT: std::sync::OnceLock<StdoutWatch> = std::sync::OnceLock::new();

/// What a streaming command learns when its standard output stops working.
struct StdoutWatch {
    /// Canceled by the first failed write, so the command stops its child
    /// through the same path as a termination request.
    cancel: CancellationToken,
    failure: std::sync::Mutex<Option<WriteFailure>>,
}

#[derive(Clone, Copy)]
struct WriteFailure {
    kind: std::io::ErrorKind,
    /// Whoever was reading has gone: a closed pipe, or a terminal that hung up.
    reader_gone: bool,
}

/// `EIO`, which is what a write to a terminal that hung up fails with.
const TERMINAL_HUNG_UP: i32 = 5;

impl StdoutWatch {
    fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            failure: std::sync::Mutex::new(None),
        }
    }

    fn record(&self, error: &std::io::Error) {
        let failure = WriteFailure {
            kind: error.kind(),
            reader_gone: error.kind() == std::io::ErrorKind::BrokenPipe
                || error.raw_os_error() == Some(TERMINAL_HUNG_UP),
        };
        if let Ok(mut recorded) = self.failure.lock() {
            recorded.get_or_insert(failure);
        }
        self.cancel.cancel();
    }

    fn failure(&self) -> Option<WriteFailure> {
        self.failure.lock().ok().and_then(|failure| *failure)
    }

    fn is_closed(&self) -> bool {
        self.failure().is_some()
    }

    /// A reader that went away is an ordinary end; any other write error is a
    /// failure the exit status has to show.
    fn result(&self) -> anyhow::Result<()> {
        match self.failure() {
            Some(WriteFailure {
                kind,
                reader_gone: false,
            }) => anyhow::bail!("could not write to standard output: {kind}"),
            _ => Ok(()),
        }
    }
}

/// Makes a failed write to standard output cancel `cancel` instead of ending
/// the process. Meant for commands that must stop a child on every way out.
fn watch_stdout(cancel: &CancellationToken) {
    let _ = STREAMING_STDOUT.set(StdoutWatch::new(cancel.clone()));
}

/// The exit status contribution of the output stream, once the child is gone.
fn stdout_result() -> anyhow::Result<()> {
    STREAMING_STDOUT.get().map_or(Ok(()), StdoutWatch::result)
}

fn generate_completions(shell: Shell) {
    let mut command = Cli::command();
    let binary_name = command.get_name().to_owned();
    // Rendered into memory first: `clap_complete` writes straight to the sink
    // it is handed and panics on any error, so handing it standard output means
    // `ephervex completions bash | head` dies with a stack trace.
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut command, binary_name, &mut script);
    write_out(&script);
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Info => {
            show_info(cli.json);
            Ok(())
        }
        // No `open_app`: the inventory is compiled in, so this answers on a
        // machine whose storage is broken or not set up yet.
        Command::Licenses(args) => show_licenses(args, cli.json),
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
            Some(ConfigCommand::PackHosts { forget, forget_all }) => {
                run_pack_hosts_command(forget, *forget_all, cli.json)
            }
        },
        Command::Preferences { command } => {
            let mut app = open_app()?;
            run_preferences_command(&mut app, command, cli.json)
        }
        Command::Instances { command } => run_instances_command(command, cli.json),
        Command::Groups { command } => {
            let app = open_app()?;
            run_groups_command(&app, command, cli.json)
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
        Command::Snapshot { command } => run_snapshot_command(&open_app()?, command, cli.json),
        Command::Logs { command } => {
            // Reading a file and listing a directory need no executor.
            let app = open_app()?;
            run_logs_command(&app, command, cli.json)
        }
        Command::UrlHandler { command } => run_url_handler_command(command, cli.json),
        Command::Import(args) => {
            let mut app = open_app()?;
            // Both paths may download: an address is fetched before it can be
            // inspected, so both need an executor.
            let runtime = tokio::runtime::Runtime::new()?;
            if args.inspect {
                return runtime.block_on(run_inspect(&mut app, args, cli.json));
            }
            runtime.block_on(run_import(&mut app, args, cli.json))
        }
        Command::Launch(args) => {
            let mut app = open_app()?;
            // Downloading, spawning, and streaming all need an executor, for
            // the same reason the Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_launch(&mut app, args, cli.json))
        }
        Command::Running => print_running(&open_app()?, cli.json),
        Command::Reconcile => run_reconciliation_command(cli.json),
    }
}

fn run_reconciliation_command(json: bool) -> anyhow::Result<()> {
    let report = open_app()?.reconcile()?;
    show_reconciliation(&report, json);
    Ok(())
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

fn run_snapshot_command(app: &App, command: &SnapshotCommand, json: bool) -> anyhow::Result<()> {
    match command {
        SnapshotCommand::List { host } => {
            let snapshots = app.snapshots(host.host()?).map_err(CoreError::from)?;
            print_snapshots(&snapshots, json)?;
        }
        SnapshotCommand::Create { host, label } => {
            let snapshot = app
                .create_snapshot(
                    host.host()?,
                    label,
                    SnapshotOrigin::Manual,
                    &CancellationToken::new(),
                )
                .map_err(CoreError::from)?;
            print_snapshot(&snapshot, json)?;
        }
        SnapshotCommand::Restore {
            host,
            id,
            no_safety_snapshot,
            yes,
        } => {
            require_snapshot_confirmation(
                *yes,
                "snapshot restore requires --yes because it replaces the live state",
            )?;
            let restored = app
                .restore_snapshot(
                    host.host()?,
                    *id,
                    SnapshotRestoreOptions {
                        safety_snapshot: !*no_safety_snapshot,
                    },
                    &CancellationToken::new(),
                )
                .map_err(CoreError::from)?;
            if json {
                print_json(&serde_json::json!({
                    "restored": restored.restored,
                    "safety_snapshot": restored.safety_snapshot,
                    "warnings": client_commit_warnings_json(&restored.warnings)
                        .into_iter()
                        .chain(server_commit_warnings_json(&restored.server_warnings))
                        .collect::<Vec<_>>(),
                }));
            } else {
                outln!(
                    "Restored {} ({})",
                    restored.restored.label,
                    restored.restored.id
                );
                if let Some(safety) = restored.safety_snapshot {
                    outln!("Safety snapshot: {} ({})", safety.label, safety.id);
                }
                print_client_commit_warnings(&restored.warnings);
                print_server_commit_warnings(&restored.server_warnings);
            }
        }
        SnapshotCommand::Rename { host, id, label } => {
            let snapshot = app
                .rename_snapshot(host.host()?, *id, label)
                .map_err(CoreError::from)?;
            print_snapshot(&snapshot, json)?;
        }
        SnapshotCommand::Delete { host, id, yes } => {
            require_snapshot_confirmation(
                *yes,
                "snapshot deletion requires --yes because it cannot be undone",
            )?;
            app.delete_snapshot(host.host()?, *id)?;
            if json {
                print_json(&serde_json::json!({ "deleted": id }));
            } else {
                outln!("Deleted snapshot {id}");
            }
        }
    }
    Ok(())
}

fn require_snapshot_confirmation(confirmed: bool, message: &str) -> anyhow::Result<()> {
    if confirmed {
        Ok(())
    } else {
        Err(anyhow::anyhow!(message.to_owned()))
    }
}

fn print_snapshots(snapshots: &[SnapshotRecord], json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&serde_json::to_value(snapshots)?);
        return Ok(());
    }
    if snapshots.is_empty() {
        outln!("No snapshots.");
        return Ok(());
    }
    for snapshot in snapshots {
        print_snapshot(snapshot, false)?;
    }
    let total = snapshots.iter().fold(0u64, |total, snapshot| {
        total.saturating_add(snapshot.apparent_bytes)
    });
    outln!("Total apparent size: {}", format_binary_bytes(total));
    if snapshots
        .iter()
        .any(|snapshot| snapshot.copy_method == CloneMethod::Copy)
    {
        outln!("Snapshots marked copy consume their full apparent size on disk.");
    }
    Ok(())
}

fn print_snapshot(snapshot: &SnapshotRecord, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&serde_json::to_value(snapshot)?);
    } else {
        let method = match snapshot.copy_method {
            CloneMethod::Reflink => "reflink",
            CloneMethod::Copy => "copy",
        };
        outln!(
            "{}  {}  {}  {}  {}  mods: {} enabled, {} disabled  {}",
            snapshot.id,
            snapshot.label,
            snapshot.captured_at.to_rfc3339(),
            format_binary_bytes(snapshot.apparent_bytes),
            method,
            snapshot.enabled_mods,
            snapshot.disabled_mods,
            snapshot.origin.as_str(),
        );
    }
    Ok(())
}

// ------------------------------------------------------------------- commands

#[allow(clippy::too_many_lines)]
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
            let record = app.current_client(*id)?;
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
            if json {
                let mut value = instance_json(app, &copy.record)?;
                value["skipped_links"] = json_paths(&copy.skipped_links);
                print_json(&value);
            } else {
                print_instance(app, &copy.record, json)?;
                eprintln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "instance-duplicated",
                        [("name", MessageArg::from(copy.record.name.as_str()))]
                    )
                );
                report_skipped_links(&copy.skipped_links);
            }
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
                    outln!("{}", ephervex_i18n::lookup("pack-recipe-missing"));
                }
                return Ok(());
            };
            let selection = if *all_optional {
                OptionalSelection::all()
            } else {
                OptionalSelection::none()
            };
            print_recipe(&recipe, (*side).into(), &selection, json)?;
        }
        InstanceCommand::Open { id, folder, print } => {
            let path = if *print {
                app.client_folder(*id, *folder)?
            } else {
                app.open_client_folder(*id, *folder)?
            };
            print_folder(path.as_deref(), folder.as_str(), json)?;
        }
        InstanceCommand::Runtime { .. } => run_instance_runtime(app, command, json).await?,
        InstanceCommand::Delete {
            id,
            keep_files,
            yes,
            stop_linked_servers,
        } => {
            let files = if *keep_files {
                FileDisposition::Keep
            } else {
                FileDisposition::Remove
            };
            run_instance_delete(app, *id, files, *yes, *stop_linked_servers, json).await?;
        }
        InstanceCommand::Persist {
            id,
            servers,
            no_servers,
        } => run_instance_persist(app, *id, servers, *no_servers, json)?,
        InstanceCommand::Favorite { .. }
        | InstanceCommand::Unfavorite { .. }
        | InstanceCommand::Group { .. }
        | InstanceCommand::Icon { .. } => run_instance_organization_command(app, command, json)?,
    }
    Ok(())
}

fn run_instance_organization_command(
    app: &App,
    command: &InstanceCommand,
    json: bool,
) -> anyhow::Result<()> {
    let id = match command {
        InstanceCommand::Favorite { id } => {
            app.set_client_favorite(*id, true)?;
            *id
        }
        InstanceCommand::Unfavorite { id } => {
            app.set_client_favorite(*id, false)?;
            *id
        }
        InstanceCommand::Group { id, name, none } => {
            let group = match (name, none) {
                (Some(name), false) => Some(named_group(app, name)?.id),
                _ => None,
            };
            app.set_client_group(*id, group)?;
            *id
        }
        InstanceCommand::Icon { id, path, clear } => {
            match (path, clear) {
                (Some(path), false) => app.set_client_icon(*id, path)?,
                _ => app.clear_client_icon(*id)?,
            }
            *id
        }
        _ => return Ok(()),
    };
    print_instance(app, &app.current_client(id)?, json)
}

async fn run_instance_delete(
    app: &App,
    id: InstanceId,
    files: FileDisposition,
    yes: bool,
    stop_linked_servers: bool,
    json: bool,
) -> anyhow::Result<()> {
    let keep_files = matches!(files, FileDisposition::Keep);
    if !keep_files && !yes {
        let client = app.client(id)?;
        let servers = app
            .linked_server_ids(id)?
            .into_iter()
            .map(|server| app.server(server))
            .collect::<Result<Vec<_>, _>>()?;
        confirm_instance_deletion(&client.name, id, &servers, json)?;
    }
    let servers = if stop_linked_servers {
        app.delete_client_stopping_linked_servers(id, files).await?
    } else {
        app.delete_client(id, files)?
    };
    if json {
        outln!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "deleted": id.to_string(),
                "files": if keep_files { "kept" } else { "removed" },
                "servers": servers
                    .iter()
                    .map(|server| serde_json::json!({
                        "id": server.id.to_string(),
                        "name": server.name,
                    }))
                    .collect::<Vec<_>>(),
            }))?
        );
    } else {
        outln!("deleted {id}");
        for server in &servers {
            outln!("deleted linked server {} ({})", server.name, server.id);
        }
    }
    Ok(())
}

/// Asks before an instance and its linked servers are removed from disk.
///
/// The prompt names every server that goes with the instance, so the answer is
/// about the hosts that will be deleted and not only the id that was typed.
/// Without a terminal to ask on, or with `--json`, the deletion is refused
/// instead. The question goes to standard error so output stays parseable.
fn confirm_instance_deletion(
    name: &str,
    id: InstanceId,
    servers: &[ephervex_core::ServerRecord],
    json: bool,
) -> anyhow::Result<()> {
    let mut summary = format!("{name} ({id}) and all of its files");
    for server in servers {
        let _ = write!(
            summary,
            ", linked server {} ({}) and all of its files",
            server.name, server.id
        );
    }
    if json || !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "deleting this instance removes {summary}, which cannot be undone; pass --yes to confirm, or --keep-files to leave the files on disk"
        );
    }
    let answer = ask(&format!("Delete {summary}? This cannot be undone. [y/N] "))?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => anyhow::bail!("the instance was not deleted"),
    }
}

/// Moves a temporary instance into the persistent instance folder.
///
/// The plan is read first so that naming no servers can mean "all of them"
/// without the caller having to list ids it would have to look up. A server the
/// plan did not offer is already persistent, and naming one is refused by the
/// application service rather than skipped here.
fn run_instance_persist(
    app: &App,
    id: InstanceId,
    servers: &[ServerId],
    no_servers: bool,
    json: bool,
) -> anyhow::Result<()> {
    let plan = app.plan_instance_promotion(id)?;
    let request = if no_servers {
        PromotionRequest::client_only()
    } else if servers.is_empty() {
        PromotionRequest::all_of(&plan)
    } else {
        PromotionRequest {
            servers: servers.to_vec(),
        }
    };

    let outcome = app.promote_instance(
        id,
        &request,
        &OperationReporter::silent("operation-promoting-instance"),
    )?;

    if json {
        print_json(&serde_json::json!({
            "instance": outcome.instance.id.to_string(),
            "root": json_path(&outcome.instance.root),
            "storage": outcome.instance.storage.to_string(),
            "instance_warnings": client_commit_warnings_json(&outcome.instance_warnings),
            "server_commit_warnings": outcome
                .server_commit_warnings
                .iter()
                .map(|warning| {
                    let mut value = server_commit_warnings_json(std::slice::from_ref(
                        &warning.warning,
                    ));
                    let mut entry = value.pop().unwrap_or_default();
                    if let Some(object) = entry.as_object_mut() {
                        object.insert(
                            "server".to_owned(),
                            serde_json::Value::String(warning.server.to_string()),
                        );
                    }
                    entry
                })
                .collect::<Vec<_>>(),
            "servers": outcome
                .servers
                .iter()
                .map(|server| serde_json::json!({
                    "id": server.id.to_string(),
                    "root": json_path(&server.root),
                }))
                .collect::<Vec<_>>(),
            "warnings": outcome
                .warnings
                .iter()
                .map(|warning| serde_json::json!({
                    "server": warning.server.to_string(),
                    "detail": warning.error.to_string(),
                }))
                .collect::<Vec<_>>(),
        }));
        return unmoved_servers_result(&outcome.warnings);
    }

    outln!(
        "{}",
        ephervex_i18n::lookup_args(
            "instance-made-persistent",
            [
                ("name", MessageArg::from(outcome.instance.name.as_str())),
                ("path", MessageArg::from(outcome.instance.root.as_path())),
            ]
        )
    );
    print_client_commit_warnings(&outcome.instance_warnings);
    for server in &outcome.servers {
        outln!(
            "  {}",
            ephervex_i18n::lookup_args(
                "server-made-persistent",
                [
                    ("name", MessageArg::from(server.name.as_str())),
                    ("path", MessageArg::from(server.root.as_path())),
                ]
            )
        );
    }
    for warning in &outcome.server_commit_warnings {
        eprintln!(
            "  {}",
            ephervex_core::promotion::promoted_server_warning_message(warning)
        );
    }
    for warning in &outcome.warnings {
        eprintln!(
            "  {}",
            ephervex_i18n::lookup_args(
                "server-not-made-persistent",
                [
                    ("name", MessageArg::from(warning.name.as_str())),
                    ("value", MessageArg::from(warning.error.to_string())),
                ]
            )
        );
    }
    unmoved_servers_result(&outcome.warnings)
}

/// Whether every requested server left temporary storage.
///
/// The instance itself has already moved by the time a server fails to, so the
/// outcome is reported in full first. A server that stayed behind is still at
/// risk of being cleared, which a script must not see as success.
fn unmoved_servers_result(warnings: &[PromotionWarning]) -> anyhow::Result<()> {
    if warnings.is_empty() {
        return Ok(());
    }
    let names = warnings
        .iter()
        .map(|warning| warning.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!("the instance was moved, but these servers are still temporary: {names}")
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
        ServerCommand::Runtime { .. } => run_server_runtime(app, command, json).await?,
        ServerCommand::Show { id } => print_server_with_organization(app, *id, json)?,
        ServerCommand::Favorite { id } => {
            app.set_server_favorite(*id, true)?;
            print_server_with_organization(app, *id, json)?;
        }
        ServerCommand::Unfavorite { id } => {
            app.set_server_favorite(*id, false)?;
            print_server_with_organization(app, *id, json)?;
        }
        ServerCommand::Icon { id, path, clear } => {
            match (path, clear) {
                (Some(path), false) => app.set_server_icon(*id, path)?,
                _ => app.clear_server_icon(*id)?,
            }
            print_server_with_organization(app, *id, json)?;
        }
        ServerCommand::Mods { id } => {
            print_host_mods(&app.server_mod_snapshot(*id)?, json);
        }
        ServerCommand::Crash { id } => {
            print_crash_review(app.crash_review(InstalledHost::Server(*id))?.as_ref(), json);
        }
        ServerCommand::Status { id } => print_server_status(app, *id, json)?,
        ServerCommand::Properties { command } => match command {
            ServerPropertiesCommand::Show { id } => {
                let text = app.server_properties(*id)?.render();
                if json {
                    print_json(&serde_json::json!({ "server_id": id, "properties": text }));
                } else {
                    out!("{text}");
                }
            }
            ServerPropertiesCommand::Set { id, key, value } => {
                let takes_effect = app.set_server_property(*id, key, value)?;
                let restart_required = takes_effect == ephervex_core::PropertiesTakeEffect::Restart;
                if json {
                    print_json(&serde_json::json!({
                        "server_id": id,
                        "key": key,
                        "value": value,
                        "restart_required": restart_required,
                    }));
                } else if restart_required {
                    outln!("set {key}; the running server reads it once restarted");
                } else {
                    outln!("set {key}");
                }
            }
        },
        ServerCommand::Network(args) => {
            let committed = app.update_server_network(
                args.id,
                &ServerNetworkChange {
                    online_mode: args.online_mode,
                    access: args.access.map(|access| match access {
                        NetworkAccessArg::Localhost => NetworkAccess::Localhost,
                        NetworkAccessArg::Lan => NetworkAccess::Lan,
                        NetworkAccessArg::Custom => NetworkAccess::Custom,
                    }),
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
                let phrase = ask(&format!(
                    "Review the Minecraft EULA: {MINECRAFT_EULA_REFERENCE}\nType {EULA_CONFIRMATION_PHRASE} to accept: "
                ))?;
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
            let response = app.stop_server(*id).await?;
            print_control_response(&response, json);
            control_response_result(&response)?;
        }
        ServerCommand::Restart { id } => {
            let response = app.restart_server(*id).await?;
            print_control_response(&response, json);
            control_response_result(&response)?;
        }
        ServerCommand::Kill { id } => {
            let response = app.kill_server(*id).await?;
            print_control_response(&response, json);
            control_response_result(&response)?;
        }
        ServerCommand::Console { id, command } => {
            let response = app.send_server_console(*id, command.clone()).await?;
            print_control_response(&response, json);
            control_response_result(&response)?;
        }
        ServerCommand::Output { id, lines } => {
            let snapshot = app.server_output(*id, *lines)?;
            if json {
                print_json(&server_output_json(*id, &snapshot));
            } else {
                report_capture_gaps(&snapshot);
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
                    outln!("cleared proven-stale runtime state for {id}");
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
                outln!("deleted {id}");
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
        LogsCommand::Bundle {
            scope,
            output,
            include,
            list,
        } => {
            return run_logs_bundle(app, scope.scope()?, output.as_deref(), include, *list, json);
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
                    print_json(&serde_json::json!({ "name": name, "path": json_path(&resolved) }));
                } else {
                    outln!("{}", resolved.display());
                }
                return Ok(());
            }

            let view = app.read_diagnostic(&request(scope, kind, name, *head, *bytes))?;
            if json {
                print_json(&serde_json::json!({
                    "name": name,
                    "path": json_path(&view.path),
                    "anchor": view.anchor.as_str(),
                    "file_bytes": view.file_bytes,
                    "returned_bytes": view.returned_bytes,
                    "truncated": view.truncated,
                    "lossy": view.lossy,
                    "read_at": view.read_at,
                    "text": view.text,
                }));
            } else {
                out!("{}", view.text);
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
        outln!("{}", ephervex_i18n::lookup("diagnostics-nothing-recorded"));
        return;
    }
    for entry in entries {
        let modified = entry.modified.map(|at| at.to_rfc3339()).unwrap_or_default();
        let headline = entry
            .summary
            .as_ref()
            .and_then(ephervex_core::CrashSummary::headline)
            .unwrap_or_default();
        outln!(
            "{}\t{}\t{}\t{}",
            entry.name,
            entry.size_bytes,
            modified,
            headline
        );
    }
}

fn diagnostic_entry_json(entry: &DiagnosticEntry) -> serde_json::Value {
    serde_json::json!({
        "name": entry.name,
        "path": json_path(&entry.path),
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
            "path": json_path(&found.view.path),
            "file_bytes": found.view.file_bytes,
            "returned_bytes": found.view.returned_bytes,
            "window_truncated": found.view.truncated,
            "lossy": found.view.lossy,
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
        outln!("{}:{}", matched.line_number, matched.line);
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
                outln!(
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
                outln!(
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
        outln!(
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
        outln!("{}", ephervex_i18n::lookup("preferences-none-managed"));
    } else {
        outln!(
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
            "persistent_instance_root": json_path(&root),
            "overridden": overridden,
        }));
    } else {
        outln!("{}", root.display());
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
    // Output the reader no longer takes stops the game like a termination
    // request does; exiting on the failed write would leave it running.
    watch_stdout(&cancel);

    // Ending the launcher stops the game rather than orphaning it. The game
    // runs in its own process group, so none of these reach it from the
    // terminal. The task ends with the process, so nothing has to shut it down.
    let kill_server = CancellationToken::new();
    let stopping_server = Arc::new(AtomicBool::new(false));
    // Watched before the task is spawned so a request cannot arrive while the
    // handlers are still being installed.
    if let Ok(requests) = TerminationRequests::watch() {
        tokio::spawn(end_launch_on_requests(
            requests,
            LaunchStopSignals {
                stop: cancel.clone(),
                kill_server: kill_server.clone(),
                server_stopping: Arc::clone(&stopping_server),
            },
        ));
    }

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
                if matches!(event, LaunchAndConnectEvent::StoppingServer { .. }) {
                    stopping_server.store(true, Ordering::Relaxed);
                }
                print_connect_event(event);
            }
        });
        let (server_observer, warning_printer) = spawn_launch_server_warning_printer(json);
        let outcome = app
            .launch_and_connect(
                args.id,
                server,
                &LaunchAndConnectOptions {
                    launch: options.clone(),
                    // The command's runtime ends with the command, which would
                    // kill a server left running under it.
                    started_server: StartedServerLifetime::StopOnReturn,
                    started_server_kill: kill_server,
                    ..LaunchAndConnectOptions::default()
                },
                &connect_observer,
                &server_observer,
                &observer,
                &cancel,
            )
            .await;
        drop(connect_observer);
        drop(server_observer);
        let _ = connect_printer.await;
        let _ = warning_printer.await;
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
    // Read before the printer drains: a reader that leaves while the last lines
    // of a game that already ended are printed did not stop the game.
    let stop_requested = cancel.is_cancelled();
    // The printer ends when every sender is gone, so this drop is what lets it
    // finish rather than hang.
    drop(observer);
    let _ = printer.await;

    let exit = match outcome? {
        ConnectedLaunch::Client(outcome) => {
            report_launch(&outcome, json);
            outcome.exit
        }
        ConnectedLaunch::Linked(outcome) => {
            report_connected_launch(&outcome, json);
            outcome.launch.exit
        }
    };
    game_exit_result(exit, stop_requested).and(stdout_result())
}

/// Shows a linked server's advisories during a launch.
///
/// The server's own output is left out because it would bury the game's. The
/// task keeps draining until the observer is dropped, so the queue stays empty
/// for as long as the supervised server runs.
fn spawn_launch_server_warning_printer(
    json: bool,
) -> (ServerObserver, tokio::task::JoinHandle<()>) {
    let (observer, mut events) = ServerObserver::channel();
    let printer = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let Some(warning) = launch_server_warning(event) {
                print_server_warning(&warning, json);
            }
        }
    });
    (observer, printer)
}

/// The advisory a linked server's event carries, which is all a launch shows
/// of that server's events.
fn launch_server_warning(event: ServerEvent) -> Option<ServerWarning> {
    match event {
        ServerEvent::Warning(warning) => Some(warning),
        _ => None,
    }
}

/// What the requests to end a launch act on.
struct LaunchStopSignals {
    /// Canceled by the first request, which stops the game.
    stop: CancellationToken,
    /// Canceled by the next request, which kills a linked server that is slow
    /// to stop.
    kill_server: CancellationToken,
    /// Set once a linked server is being stopped, after the game has ended.
    server_stopping: Arc<AtomicBool>,
}

/// Turns the first request into a stop of the game and the next, or the first
/// once the linked server is being stopped, into a kill of that server.
async fn end_launch_on_requests(mut requests: impl StopRequests, signals: LaunchStopSignals) {
    requests.next().await;
    // The game is already gone once the server is being stopped, so this
    // request is the one that hurries the server rather than a stop of the game.
    if !signals.server_stopping.load(Ordering::Relaxed) {
        // Canceled first: after a hangup the terminal may refuse the write.
        signals.stop.cancel();
        note(&ephervex_i18n::lookup("launch-stopping"));
        requests.next().await;
    }
    // Said by the stop itself, only when a server this launch started is
    // actually killed.
    signals.kill_server.cancel();
}

/// Whether the game's end means the command did its job.
///
/// A dry run never ran the game. A game stopped because the launcher was told
/// to end is a stop that was asked for, whatever status the game left with.
fn game_exit_result(exit: Option<GameExit>, stop_requested: bool) -> anyhow::Result<()> {
    match exit {
        Some(exit) if !exit.is_success() && !stop_requested => {
            anyhow::bail!("Minecraft did not exit normally")
        }
        _ => Ok(()),
    }
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
        // After a hangup the terminal can refuse the write, and the server is
        // still being stopped.
        LaunchAndConnectEvent::StoppingServer { server } => {
            note(&ephervex_i18n::lookup_args(
                "launch-connect-stopping-server",
                [("server", MessageArg::from(server.to_string()))],
            ));
        }
        LaunchAndConnectEvent::KillingServer { server } => {
            note(&ephervex_i18n::lookup_args(
                "launch-connect-killing-server",
                [("server", MessageArg::from(server.to_string()))],
            ));
        }
        LaunchAndConnectEvent::ServerStopped { server } => {
            note(&ephervex_i18n::lookup_args(
                "launch-connect-server-stopped",
                [("server", MessageArg::from(server.to_string()))],
            ));
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
            "java": json_path(&outcome.launch.java.runtime.executable),
            "account": outcome.launch.account,
            "authenticated": outcome.launch.authenticated,
            "command": outcome.launch.command.redacted_arguments(),
            "dry_run": outcome.launch.exit.is_none(),
            "exit_code": outcome.launch.exit.and_then(|exit| exit.code),
            "exit_signal": outcome.launch.exit.and_then(|exit| exit.signal),
            "java_failure": outcome.launch.java_failure.as_deref().map(java_failure_json),
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
/// line per asset - four and a half thousand of them on a modern release. A
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
            LaunchEvent::OutputDropped { lines } => eprintln!(
                "{}",
                ephervex_i18n::lookup_args(
                    "launch-output-dropped",
                    [(
                        "lines",
                        MessageArg::from(i64::try_from(*lines).unwrap_or(i64::MAX))
                    )],
                )
            ),
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

/// Where a foreground command hears that it was asked to end.
trait StopRequests {
    /// Resolves once per request.
    async fn next(&mut self);
}

impl StopRequests for TerminationRequests {
    async fn next(&mut self) {
        TerminationRequests::next(self).await;
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
            outln!("{text}");
        }
    } else {
        // Minecraft's own log text is never translated.
        outln!("{}", line.display());
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
            "java": json_path(&outcome.java.runtime.executable),
            "account": outcome.account,
            "authenticated": outcome.authenticated,
            "classpath_entries": outcome.installation.plan.classpath().len(),
            "natives_directory": json_path(&outcome.installation.plan.natives_dir),
            "assets": outcome.installation.assets,
            // Never the real arguments: one of them is an access token.
            "command": outcome.command.redacted_arguments(),
            "dry_run": outcome.exit.is_none(),
            "exit_code": outcome.exit.and_then(|exit| exit.code),
            "exit_signal": outcome.exit.and_then(|exit| exit.signal),
            "java_failure": outcome.java_failure.as_deref().map(java_failure_json),
        });
        print_json(&value);
        return;
    }

    if outcome.exit.is_none() {
        eprintln!("{}", ephervex_i18n::lookup("launch-dry-run"));
        outln!("{}", outcome.command.java.display());
        for argument in outcome.command.redacted_arguments() {
            outln!("  {argument}");
        }
        eprintln!("{}", ephervex_i18n::lookup("launch-command-redacted"));
    }
    if let Some(failure) = &outcome.java_failure {
        for line in java_failure_lines(failure) {
            eprintln!("{}", terminal_safe(&line));
        }
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
        outln!(
            "{} {}",
            ephervex_model::APP_NAME,
            ephervex_model::APP_VERSION
        );
        outln!("app id:     {}", ephervex_model::APP_ID);
        outln!("user agent: {}", ephervex_model::user_agent());
    }
}

fn show_licenses(args: &LicensesArgs, json: bool) -> anyhow::Result<()> {
    // The error is borrowed from a process-wide cache, so it is described here
    // rather than carried.
    let inventory = App::third_party_licenses().map_err(|error| {
        anyhow::anyhow!("this build's third-party license list could not be read: {error}")
    })?;

    if let Some(wanted) = &args.text {
        return show_license_text(inventory, wanted, json);
    }
    show_license_listing(inventory, args.all, json);
    Ok(())
}

/// Every text under one identifier.
///
/// More than one is the ordinary case rather than an oddity: crates under MIT
/// ship an MIT file each, differing in the copyright line the license exists to
/// carry, so printing one of them would be printing somebody else's notice.
fn show_license_text(
    inventory: &ephervex_core::LicenseInventory,
    wanted: &str,
    json: bool,
) -> anyhow::Result<()> {
    let matching: Vec<_> = inventory
        .licenses()
        .iter()
        .filter(|license| license.id.eq_ignore_ascii_case(wanted))
        .collect();
    anyhow::ensure!(
        !matching.is_empty(),
        "no dependency of this build is licensed under {wanted}. \
         Run `ephervex licenses` to see which licenses there are."
    );

    if json {
        print_json(&serde_json::json!({
            "licenses": matching.iter().map(|license| license_json(license))
                .collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    for (position, license) in matching.iter().enumerate() {
        if position > 0 {
            outln!();
        }
        // A license SPDX does not list has no name but its identifier, and
        // printing that twice reads like a mistake.
        if license.name == license.id {
            outln!("{}", license.id);
        } else {
            outln!("{} ({})", license.name, license.id);
        }
        outln!();
        outln!("{}", license.text.trim_end());
    }
    Ok(())
}

fn license_json(license: &ephervex_core::LicenseText) -> serde_json::Value {
    serde_json::json!({
        "id": license.id,
        "name": license.name,
        "text": license.text,
    })
}

fn show_license_listing(inventory: &ephervex_core::LicenseInventory, all: bool, json: bool) {
    if json {
        // The whole inventory, texts included, because a machine-readable
        // listing that named licenses without carrying them would send the
        // reader back to the file this command exists to replace.
        print_json(&serde_json::json!({
            "licenses": inventory.licenses().iter().map(license_json).collect::<Vec<_>>(),
            "dependencies": inventory
                .dependencies()
                .iter()
                .map(|dependency| serde_json::json!({
                    "name": dependency.name,
                    "version": dependency.version,
                    "license": dependency.license,
                    "texts": dependency.texts,
                }))
                .collect::<Vec<_>>(),
        }));
        return;
    }

    outln!(
        "{} {} is built on {} open-source packages.",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION,
        inventory.dependencies().len()
    );
    outln!();
    if all {
        let width = inventory
            .dependencies()
            .iter()
            .map(|dependency| dependency.name.len())
            .max()
            .unwrap_or(0);
        for dependency in inventory.dependencies() {
            outln!(
                "{:width$}  {:<12}  {}",
                dependency.name,
                dependency.version,
                dependency.license
            );
        }
    } else {
        for (identifier, count) in inventory.identifier_counts() {
            outln!("{count:5}  {identifier}");
        }
        outln!();
        outln!("`ephervex licenses --all` lists each package.");
    }
    outln!("`ephervex licenses --text <LICENSE>` prints a license in full.");
    outln!();
    outln!(
        "Ephervex itself is licensed under Apache-2.0. Every license text above \
         also ships beside the program as THIRD-PARTY-LICENSES.md."
    );
}

fn show_paths(paths: &Paths, json: bool) {
    if json {
        let value = serde_json::json!({
            "config": json_path(paths.config_dir()),
            "config_file": json_path(&paths.config_file()),
            "data": json_path(paths.data_dir()),
            "cache": json_path(paths.cache_dir()),
            "state": json_path(paths.state_dir()),
            "database": json_path(&paths.database_file()),
            "logs": json_path(&paths.logs_dir()),
            "temp": json_path(paths.temp_dir()),
            "temporary_instances": json_path(&paths.temporary_instances_dir()),
            "persistent_instances": json_path(&paths.persistent_instances_dir()),
            "persistent_servers": json_path(&paths.persistent_servers_dir()),
            "persistent_instance_root_overridden": paths.persistent_instance_root_is_overridden(),
        });
        print_json(&value);
    } else {
        outln!("config:               {}", paths.config_dir().display());
        outln!("data:                 {}", paths.data_dir().display());
        outln!("cache:                {}", paths.cache_dir().display());
        outln!("state:                {}", paths.state_dir().display());
        outln!("database:             {}", paths.database_file().display());
        outln!(
            "temporary instances:  {}",
            paths.temporary_instances_dir().display()
        );
        outln!(
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

/// Collects diagnostics into one archive, or says what one would hold.
fn run_logs_bundle(
    app: &App,
    scope: DiagnosticScope,
    output: Option<&Path>,
    include: &[String],
    list: bool,
    json: bool,
) -> anyhow::Result<()> {
    let contents = app.plan_diagnostic_bundle(scope)?;
    if list {
        print_bundle_contents(&contents, json);
        return Ok(());
    }

    let destination = match output {
        Some(path) => path.to_path_buf(),
        None => anyhow::bail!("name where the archive should go with --output"),
    };
    let selection = if include.is_empty() {
        contents.everything()
    } else {
        BundleSelection {
            names: include.to_vec(),
        }
    };

    let report = app.export_diagnostic_bundle(scope, &selection, &destination)?;
    if json {
        print_json(&serde_json::json!({
            "path": json_path(&report.path),
            "bytes": report.bytes,
            "included": report.included,
            "omitted": report.omitted,
            "unreadable": report.unreadable,
        }));
    } else {
        outln!(
            "{}  {} files, {}",
            report.path.display(),
            report.included.len(),
            format_binary_bytes(report.bytes)
        );
    }
    for name in &report.unreadable {
        eprintln!("asked for and could not be read: {name}");
    }
    Ok(())
}

/// Lists what a bundle could hold, and what each part costs.
fn print_bundle_contents(contents: &BundleContents, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "total_bytes": contents.total_bytes(),
            "parts": contents
                .parts
                .iter()
                .map(|part| serde_json::json!({
                    "name": part.name,
                    "kind": part.kind.as_str(),
                    "size_bytes": part.size_bytes,
                }))
                .collect::<Vec<_>>(),
        }));
        return;
    }
    for part in &contents.parts {
        outln!(
            "{:>10}  {}",
            format_binary_bytes(part.size_bytes),
            part.name
        );
    }
    outln!("{:>10}  total", format_binary_bytes(contents.total_bytes()));
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

    outln!("{} {}", health.application.name, health.application.version);
    outln!("app id:     {}", health.application.app_id);
    outln!(
        "platform:   {}-{}",
        health.platform.os,
        health.platform.arch
    );
    outln!("language:   {}", health.application.language);

    show_health_directories(health);
    show_health_configuration(health.configuration.as_ref());
    show_health_credentials(&health.credential_storage);

    outln!();
    outln!("bundled data");
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
        outln!();
        outln!("storage");
        outln!("  could not be opened: {error}");
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
    outln!();
    outln!("directories");
    if let Some(error) = &health.paths_error {
        outln!("  could not be resolved: {error}");
    }
    for directory in &health.directories {
        let state = match &directory.state {
            DirectoryState::Absent => "missing (created on first use)".to_owned(),
            DirectoryState::Writable => "writable".to_owned(),
            DirectoryState::NotWritable { detail } => format!("NOT WRITABLE: {detail}"),
            DirectoryState::Unusable { detail } => format!("UNUSABLE: {detail}"),
        };
        outln!(
            "  {:<21} {}  [{state}]",
            directory.purpose,
            directory.path.display()
        );
    }
}

fn show_health_configuration(configuration: Option<&ConfigurationHealth>) {
    outln!();
    outln!("configuration");
    let Some(configuration) = configuration else {
        outln!("  could not be located: the storage directories did not resolve");
        return;
    };
    match configuration {
        ConfigurationHealth::Absent { path } => {
            outln!("  {}  [not written yet]", path.display());
        }
        ConfigurationHealth::Loaded {
            path,
            schema_version,
        } => {
            outln!("  {}  [schema {schema_version}]", path.display());
        }
        ConfigurationHealth::Unusable {
            path,
            message_id,
            detail,
        } => {
            outln!("  {}  [UNUSABLE]", path.display());
            outln!("  {}", ephervex_i18n::lookup(message_id));
            outln!("  {detail}");
        }
    }
}

fn show_health_credentials(credentials: &CredentialBackendHealth) {
    outln!();
    outln!("credential storage");
    if credentials.available {
        outln!("  {}  [available]", credentials.backend);
    } else {
        outln!("  unavailable; sign-in is session-only for this run");
        if let Some(detail) = &credentials.detail {
            outln!("  {detail}");
        }
    }
}

fn show_health_artifact_cache(cache: Option<&ArtifactCacheHealth>) {
    outln!();
    outln!("artifact cache");
    match cache {
        None => outln!("  not measured; storage could not be opened"),
        Some(ArtifactCacheHealth::Unavailable { detail }) => {
            outln!("  not measured: {detail}");
        }
        Some(ArtifactCacheHealth::Measured {
            used_bytes,
            entries,
            complete,
            limit,
            over_budget,
        }) => {
            outln!("  {entries} entries, {used_bytes} bytes, budget {limit}");
            if !complete {
                outln!("  the cache was in use; part of it was not measured");
            }
            if *over_budget {
                outln!("  over budget; collection runs after the next cache operation");
            }
        }
    }
}

fn show_health_java(java: Option<&JavaHealth>) {
    outln!();
    outln!("java");
    match java {
        None => outln!("  not measured: storage could not be opened"),
        Some(JavaHealth::Unavailable { detail }) => {
            // Never "no runtimes found": the scan did not finish, which is a
            // different thing to tell somebody than that they have no JDK.
            outln!("  not measured: {detail}");
        }
        Some(JavaHealth::Discovered {
            discovered,
            managed,
            major_versions,
        }) => {
            outln!("  {discovered} runtimes ({managed} managed)");
            if !major_versions.is_empty() {
                let versions: Vec<String> =
                    major_versions.iter().map(ToString::to_string).collect();
                outln!("  major versions: {}", versions.join(", "));
            }
        }
    }
}

fn show_bundled_document(label: &str, document: &BundledDocumentHealth) {
    match document {
        BundledDocumentHealth::Loaded { entries, revision } => match revision {
            Some(revision) => outln!("  {label:<22} {entries} entries, revision {revision}"),
            None => outln!("  {label:<22} {entries} entries"),
        },
        BundledDocumentHealth::Unusable { detail } => {
            outln!("  {label:<22} UNUSABLE: {detail}");
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
                    "path": json_path(&directory.path),
                    "state": state,
                    "detail": detail,
                })
            })
            .collect::<Vec<_>>(),
        "configuration": match &health.configuration {
            None => serde_json::Value::Null,
            Some(ConfigurationHealth::Absent { path }) => serde_json::json!({
                "status": "absent",
                "path": json_path(path),
            }),
            Some(ConfigurationHealth::Loaded { path, schema_version }) => serde_json::json!({
                "status": "loaded",
                "path": json_path(path),
                "schema_version": schema_version,
            }),
            Some(ConfigurationHealth::Unusable { path, message_id, detail }) => serde_json::json!({
                "status": "unusable",
                "path": json_path(path),
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
        // Same credential refusal as the text form below (ADR 0004).
        print_json(&serde_json::json!({
            "path": json_path(&paths.config_file()),
            "exists": paths.config_file().is_file(),
            "config": config.to_json_value()?,
        }));
    } else {
        outln!("# {}", paths.config_file().display());
        // The serializer refuses to emit a credential-looking key, so this
        // cannot print a secret even if one somehow reached the struct
        // (ADR 0004).
        out!("{}", config.to_toml_string()?);
    }
    Ok(())
}

/// Says on standard error that an unreadable favorites, groups and icons file
/// was set aside, and whether its previous copy took its place, whichever
/// call did it.
fn report_organization_notices() {
    for notice in ephervex_core::take_organization_notices() {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                if notice.restored {
                    "organization-restored"
                } else {
                    "organization-set-aside"
                },
                [(
                    "path",
                    MessageArg::from(notice.set_aside.display().to_string())
                )]
            )
        );
    }
}

/// What the list and the JSON need beyond a record: favorites, groups, icons
/// and how each client relates to linked servers.
struct Organized {
    view: OrganizationView,
    linking: std::collections::HashMap<InstanceId, LinkingState>,
}

impl Organized {
    fn load(app: &App) -> anyhow::Result<Self> {
        Ok(Self {
            view: app.organization()?,
            linking: app.linking_states()?,
        })
    }

    /// For output that only decorates a record: without favorites, groups,
    /// icons and links when they cannot be read, rather than failing a command
    /// that already did its job.
    fn load_lenient(app: &App) -> Self {
        Self {
            view: app.organization_or_empty(),
            linking: app.linking_states().unwrap_or_default(),
        }
    }
}

fn run_instances_command(command: &InstancesCommand, json: bool) -> anyhow::Result<()> {
    let app = open_app()?;
    match command {
        InstancesCommand::List(args) => print_instance_list(&app, args, json),
    }
}

fn instance_filter(app: &App, args: &InstanceListArgs) -> anyhow::Result<InstanceFilter> {
    let group = match (&args.group, args.ungrouped) {
        (Some(name), _) => GroupFilter::Group(
            app.organization()?
                .group_named(name)
                .ok_or_else(|| CoreError::GroupNotFound {
                    group: name.clone(),
                })?
                .id,
        ),
        (None, true) => GroupFilter::Ungrouped,
        (None, false) => GroupFilter::Any,
    };
    Ok(InstanceFilter {
        query: args.search.clone().unwrap_or_default(),
        storage: None,
        favorites_only: args.favorite,
        minecraft_version: args.minecraft.clone(),
        loader: args.loader.map(Into::into),
        run_state: match (args.run_state.running, args.run_state.stopped) {
            (true, _) => Some(RunFilter::Running),
            (_, true) => Some(RunFilter::Stopped),
            _ => None,
        },
        linking: args.linking.map(Into::into),
        group,
    })
}

fn print_instance_list(app: &App, args: &InstanceListArgs, json: bool) -> anyhow::Result<()> {
    let filter = instance_filter(app, args)?;
    let sort = args.sort.map_or_else(|| app.instance_sort(), Into::into);
    let order = if sort == InstanceSort::Minecraft {
        tokio::runtime::Runtime::new()?.block_on(app.minecraft_version_order())
    } else {
        VersionOrder::default()
    };
    let listed = ephervex_core::list_instances(app.instance_facts()?, &filter, sort, &order);
    let mut records = app
        .list_clients()?
        .into_iter()
        .map(|record| (record.id, record))
        .collect::<std::collections::HashMap<_, _>>();
    let organized = Organized::load(app)?;

    if json {
        let values: Vec<_> = listed
            .iter()
            .filter_map(|facts| records.get(&facts.id))
            .map(|record| instance_json_with(app, record, &organized))
            .collect::<Result<_, _>>()?;
        print_json(&serde_json::Value::Array(values));
        return Ok(());
    }

    if listed.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::lookup(if filter.is_active() && !records.is_empty() {
                "instances-list-none-match"
            } else {
                "instances-list-empty"
            })
        );
        return Ok(());
    }

    for facts in &listed {
        let Some(record) = records.remove(&facts.id) else {
            continue;
        };
        // The summary message already carries the name, so there is no separate
        // name column: one source of that string, not two.
        outln!(
            "{}  {:<11} {}{}{}{}",
            record.id,
            record.storage.to_string(),
            summarize(&record),
            if facts.favorite {
                format!(
                    "  {}",
                    ephervex_i18n::lookup("instances-list-favorite-marker")
                )
            } else {
                String::new()
            },
            facts
                .group
                .as_ref()
                .map_or_else(String::new, |group| format!(
                    "  {}",
                    ephervex_i18n::lookup_args(
                        "instances-list-group-marker",
                        [("group", MessageArg::from(group.name.as_str()))]
                    )
                )),
            if record.needs_attention() {
                "  [needs attention]"
            } else {
                ""
            }
        );
    }
    let records = listed;
    outln!(
        "{}",
        ephervex_i18n::lookup_args(
            "instances-list-count",
            [("count", MessageArg::from(records.len()))]
        )
    );
    Ok(())
}

fn run_groups_command(app: &App, command: &GroupsCommand, json: bool) -> anyhow::Result<()> {
    match command {
        GroupsCommand::List => {
            let view = app.organization()?;
            let facts = app.instance_facts()?;
            let counts: Vec<(InstanceGroup, usize)> = view
                .groups()
                .into_iter()
                .map(|group| {
                    let members = facts
                        .iter()
                        .filter(|facts| facts.group.as_ref().is_some_and(|g| g.id == group.id))
                        .count();
                    (group, members)
                })
                .collect();
            if json {
                print_json(&serde_json::Value::Array(
                    counts
                        .iter()
                        .map(|(group, members)| group_json(group, *members))
                        .collect(),
                ));
            } else if counts.is_empty() {
                outln!("{}", ephervex_i18n::lookup("groups-list-empty"));
            } else {
                for (group, members) in &counts {
                    outln!(
                        "{}  {}  ({})",
                        group.id,
                        group.name,
                        ephervex_i18n::lookup_args(
                            "groups-list-members",
                            [("count", MessageArg::from(*members))]
                        )
                    );
                }
            }
        }
        GroupsCommand::Create { name } => print_group(app, &app.create_group(name)?, json),
        GroupsCommand::Rename { name, new_name } => {
            let group = named_group(app, name)?;
            print_group(app, &app.rename_group(group.id, new_name)?, json);
        }
        GroupsCommand::Delete { name } => {
            let group = named_group(app, name)?;
            let members = group_member_count(app, &group);
            app.delete_group(group.id)?;
            if json {
                print_json(&serde_json::json!({ "deleted": group_json(&group, members) }));
            } else {
                eprintln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "group-deleted",
                        [("name", MessageArg::from(group.name.as_str()))]
                    )
                );
            }
        }
    }
    Ok(())
}

fn named_group(app: &App, name: &str) -> anyhow::Result<InstanceGroup> {
    Ok(app
        .organization()?
        .group_named(name)
        .ok_or_else(|| CoreError::GroupNotFound {
            group: name.to_owned(),
        })?)
}

fn group_member_count(app: &App, group: &InstanceGroup) -> usize {
    app.instance_facts().map_or(0, |facts| {
        facts
            .iter()
            .filter(|facts| facts.group.as_ref().is_some_and(|g| g.id == group.id))
            .count()
    })
}

fn group_json(group: &InstanceGroup, instances: usize) -> serde_json::Value {
    serde_json::json!({
        "id": group.id.to_string(),
        "name": group.name,
        "instances": instances,
    })
}

fn print_group(app: &App, group: &InstanceGroup, json: bool) {
    if json {
        print_json(&group_json(group, group_member_count(app, group)));
    } else {
        outln!("{}  {}", group.id, group.name);
    }
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
        outln!("{}", ephervex_i18n::lookup("servers-list-empty"));
        return Ok(());
    }

    for record in &records {
        outln!(
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
    outln!(
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
            host,
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
            let host = host.host()?;
            let results = app.search_mods(host, catalog, &query).await?;
            print_mod_search(&results, host, json);
        }
        ModCommand::Show {
            host,
            project,
            provider,
            include_incompatible,
        } => {
            return run_mod_show(
                app,
                host.host()?,
                project,
                *provider,
                *include_incompatible,
                json,
            )
            .await;
        }
        ModCommand::Plan {
            host,
            project,
            selection,
        } => return run_mod_plan(app, host.host()?, project, selection, json).await,
        ModCommand::Install {
            host,
            project,
            selection,
            consent,
        } => return run_mod_install(app, host.host()?, project, selection, consent, json).await,
        ModCommand::Catalog { catalog, clear } => {
            run_mod_catalog(app, *catalog, *clear, json)?;
        }
        ModCommand::List { instance } => {
            let mods = app.managed_mods(*instance)?;
            print_managed_mods(&mods, json);
        }
        ModCommand::Verify { host, shared } => {
            return run_mod_verify(app, host, *shared, json).await;
        }
        ModCommand::Repair { host } => return run_mod_repair(app, host.host()?, json).await,
        ModCommand::Dependencies { host, of } => {
            return run_mod_dependencies(app, host.host()?, of.as_deref(), json);
        }
        ModCommand::Refresh { host } => return run_mod_refresh(app, host.host()?, json).await,
        ModCommand::Isolate { command } => return run_mod_isolate(app, command, json),
        ModCommand::Delete {
            host,
            path,
            with_dependents,
            only_this,
        } => {
            return run_mod_delete(app, host.host()?, path, *with_dependents, *only_this, json);
        }
        ModCommand::Enable {
            host,
            path,
            with_dependencies,
            only_this,
        } => {
            return run_mod_enable(
                app,
                host.host()?,
                path,
                *with_dependencies,
                *only_this,
                json,
            );
        }
        ModCommand::Disable {
            host,
            path,
            with_dependents,
            only_this,
        } => {
            return run_mod_disable(app, host.host()?, path, *with_dependents, *only_this, json);
        }
    }
    Ok(())
}

fn run_mod_isolate(app: &App, command: &IsolateCommand, json: bool) -> anyhow::Result<()> {
    match command {
        IsolateCommand::Start {
            host,
            pinned,
            accept_unrecorded,
        } => {
            let round = app.begin_isolation(
                host.host()?,
                IsolationRequest {
                    pinned: pinned.clone(),
                    accept_unrecorded: *accept_unrecorded,
                    accept_missing: false,
                },
                &CancellationToken::new(),
            )?;
            print_isolation_step(IsolationStep::AwaitingAnswer(round), json)?;
        }
        IsolateCommand::Status { host } => {
            let state = app.isolation_state(host.host()?)?;
            if json {
                print_json(&serde_json::to_value(&state)?);
            } else if let Some(state) = state {
                print_isolation_status(&state);
            } else {
                outln!("No isolation session is active.");
            }
        }
        IsolateCommand::Resume { host } => {
            print_isolation_step(app.resume_isolation(host.host()?)?, json)?;
        }
        IsolateCommand::Answer { host, answer } => {
            print_isolation_step(
                app.answer_isolation_round(host.host()?, answer.answer())?,
                json,
            )?;
        }
        IsolateCommand::Undo { host } => {
            let round = app.undo_isolation_round(host.host()?)?;
            print_isolation_step(IsolationStep::AwaitingAnswer(round), json)?;
        }
        IsolateCommand::Finish { host } => {
            let report = app.finish_isolation(host.host()?)?;
            if json {
                print_json(&serde_json::to_value(&report)?);
            } else {
                outln!("Isolation finished after {} rounds.", report.rounds);
                outln!("Confidence: {}", report.confidence.as_str());
                outln!("Remaining testable candidates:");
                for candidate in &report.remaining_candidates {
                    outln!("  {} ({})", candidate.name, candidate.root.display());
                }
                if !report.pinned.is_empty() {
                    outln!("Pinned throughout, not tested for removal:");
                    for path in &report.pinned {
                        outln!("  {}", path.display());
                    }
                }
                if !report.pinned_dependencies.is_empty() {
                    outln!("Required by pinned mods, not tested for removal:");
                    for path in &report.pinned_dependencies {
                        outln!("  {}", path.display());
                    }
                }
                if !report.required_dependencies.is_empty() {
                    outln!("Required by the remaining candidates:");
                    for path in &report.required_dependencies {
                        outln!("  {}", path.display());
                    }
                }
                if report.reduced_confidence {
                    outln!("Dependency information was incomplete; treat this result as unproven.");
                }
                outln!("Safety snapshot retained: {}", report.safety_snapshot);
            }
        }
        IsolateCommand::Cancel { host } => {
            let canceled = app.cancel_isolation(host.host()?, &CancellationToken::new())?;
            if json {
                print_json(&serde_json::json!({
                    "canceled": true,
                    "warnings": client_commit_warnings_json(&canceled.warnings)
                        .into_iter()
                        .chain(server_commit_warnings_json(&canceled.server_warnings))
                        .collect::<Vec<_>>(),
                }));
            } else {
                outln!("Isolation canceled. The pre-isolation state was restored.");
                print_client_commit_warnings(&canceled.warnings);
                print_server_commit_warnings(&canceled.server_warnings);
            }
        }
    }
    Ok(())
}

fn print_isolation_step(step: IsolationStep, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&serde_json::to_value(&step)?);
        return Ok(());
    }
    match step {
        IsolationStep::AwaitingAnswer(round) => print_isolation_round(&round),
        IsolationStep::Complete(outcome) => {
            outln!(
                "Isolation is ready to finish ({}).",
                outcome.confidence.as_str()
            );
            outln!("Enabled configuration:");
            for path in &outcome.enabled {
                outln!("  {}", path.display());
            }
            outln!("Run `ephervex mod isolate finish` with the same host option to keep it.");
        }
        IsolationStep::Contradiction(contradiction) => {
            outln!(
                "Rounds {} and {} cannot both be right. Undo the latest answer and test again.",
                contradiction.reproducing_round,
                contradiction.conflicting_round
            );
        }
    }
    Ok(())
}

fn print_isolation_round(round: &IsolationRound) {
    outln!(
        "Isolation round {}: test this {} configuration ({}/{}, granularity {}).",
        round.index,
        round.kind.as_str(),
        round.group_index + 1,
        round.group_count,
        round.granularity
    );
    outln!("Enabled:");
    for path in &round.enabled {
        outln!("  {}", path.display());
    }
    outln!("Disabled:");
    for path in &round.disabled {
        outln!("  {}", path.display());
    }
    outln!("Launch and test the host, then answer `issue-still-present` or `issue-gone`.");
}

fn print_isolation_status(session: &IsolationSession) {
    outln!(
        "Isolation session for {} {} ({} answered rounds).",
        session.host_kind,
        session.host_id,
        session.history.len()
    );
    if session.reduced_confidence {
        outln!("Dependency information is incomplete; the result will be unproven.");
    }
    if !session.plan.linked_servers.is_empty() {
        outln!("Linked servers are not changed by this session:");
        for server in &session.plan.linked_servers {
            outln!("  {} ({})", server.name, server.id);
        }
    }
    if let Some(round) = &session.current_round {
        print_isolation_round(round);
    } else if let Some(contradiction) = &session.contradiction {
        outln!(
            "Rounds {} and {} cannot both be right. Undo the latest answer and test again.",
            contradiction.reproducing_round,
            contradiction.conflicting_round
        );
    } else if let Some(outcome) = &session.outcome {
        outln!(
            "Isolation is ready to finish ({}), with {} enabled files.",
            outcome.confidence.as_str(),
            outcome.enabled.len()
        );
    }
}

/// Takes a mod's jar off disk, saying first what else goes with it.
///
/// Refuses by default for the same reason disabling does: a command cannot ask,
/// so it says what would happen and changes nothing until told. The record each
/// jar leaves behind is what makes this recoverable, and what makes refusing a
/// courtesy rather than a safety net.
fn run_mod_delete(
    app: &App,
    host: InstalledHost,
    path: &Path,
    with_dependents: bool,
    only_this: bool,
    json: bool,
) -> anyhow::Result<()> {
    let policy = app.dependency_policy();
    let impact = app.dependents_of(host, path)?;
    let cascade = !only_this && (with_dependents || (policy.cascades() && !policy.asks()));
    if policy.asks() && !only_this && !cascade && !impact.is_silent() {
        print_dependency_impact(&impact, json);
        anyhow::bail!(
            "{}",
            impact_refusal(
                &impact,
                "Pass --with-dependents to delete the jars of the mods that need it too, or \
                 --only-this to delete just this one.",
                "Pass --only-this to delete this one."
            )
        );
    }

    let mut paths: Vec<PathBuf> = Vec::new();
    if cascade {
        paths.extend(impact.breaks.iter().map(|entry| entry.logical_path.clone()));
    }
    paths.push(path.to_path_buf());
    print_deleted_mods(&app.delete_mod_files(host, &paths)?, json);
    Ok(())
}

/// Reports every jar one call took off disk, in the order it did.
fn print_deleted_mods(deleted: &[PathBuf], json: bool) {
    if json {
        let rows: Vec<serde_json::Value> = deleted
            .iter()
            .map(|path| serde_json::json!({ "path": json_path(path), "deleted": true }))
            .collect();
        print_json(&serde_json::Value::Array(rows));
        return;
    }
    for path in deleted {
        outln!("deleted  {}", path.display());
    }
}

/// Turns a mod off, saying first what else stops loading or may.
///
/// Refusing by default is what a command with no prompt can do instead of
/// asking. It is not a warning that scrolled past: nothing is changed, and the
/// exit status says so, so a script cannot walk into a mod set that does not
/// load. Which of those happens is the recorded dependency policy, the same one
/// the desktop application follows: it refuses while changes are confirmed,
/// carries the dependents along while they are applied, and turns off the one
/// named mod while relations are not followed at all. A flag given here is an
/// instruction for this run and wins over all three.
fn run_mod_disable(
    app: &App,
    host: InstalledHost,
    path: &Path,
    with_dependents: bool,
    only_this: bool,
    json: bool,
) -> anyhow::Result<()> {
    let policy = app.dependency_policy();
    let impact = app.dependents_of(host, path)?;
    let cascade = !only_this && (with_dependents || (policy.cascades() && !policy.asks()));
    if policy.asks() && !only_this && !cascade && !impact.is_silent() {
        print_dependency_impact(&impact, json);
        anyhow::bail!(
            "{}",
            impact_refusal(
                &impact,
                "Pass --with-dependents to turn off the mods that need it too, or --only-this \
                 to turn off just this mod.",
                "Pass --only-this to turn off this mod."
            )
        );
    }

    let mut changes: Vec<(PathBuf, bool)> = Vec::new();
    if cascade {
        changes.extend(
            impact
                .breaks
                .iter()
                .map(|entry| (entry.logical_path.clone(), false)),
        );
    }
    changes.push((path.to_path_buf(), false));
    print_mod_states(&app.set_mods_enabled(host, &changes)?, json);
    Ok(())
}

/// Turns a mod on, saying first what it needs that is turned off or missing.
///
/// Enabling breaks nothing, so this refuses for a different reason than
/// disabling does: a mod turned on without what it needs does not load, and
/// reporting that after the fact would be a worse answer than not doing it.
/// What counts as worth stopping for is the core's own rule, the one the
/// desktop application follows: required mods that are turned off or not
/// installed, and installed mods the target declares itself incompatible with.
fn run_mod_enable(
    app: &App,
    host: InstalledHost,
    path: &Path,
    with_dependencies: bool,
    only_this: bool,
    json: bool,
) -> anyhow::Result<()> {
    let policy = app.dependency_policy();
    let needs = app.dependencies_of(host, path)?;
    let cascade = !only_this && (with_dependencies || (policy.cascades() && !policy.asks()));
    if policy.asks() && !only_this && !cascade && !needs.is_silent() {
        print_required_mods(&needs, json);
        anyhow::bail!("{}", requirement_refusal(&needs));
    }

    let mut changes: Vec<(PathBuf, bool)> = Vec::new();
    if cascade {
        changes.extend(
            needs
                .disabled
                .iter()
                .map(|entry| (entry.logical_path.clone(), true)),
        );
    }
    changes.push((path.to_path_buf(), true));
    print_mod_states(&app.set_mods_enabled(host, &changes)?, json);
    if cascade || policy.cascades() {
        for entry in &needs.absent {
            eprintln!("still missing: {} is not installed here", entry.name);
        }
        for entry in &needs.incompatible {
            eprintln!("conflicts with {}, which is also loaded", entry.name);
        }
    }
    Ok(())
}

/// Why turning a mod off, or deleting it, stopped.
///
/// Mods that need the target, mods that merely use it optionally, and mods
/// whose requirements are not known all stop the command, but they are not the
/// same claim, so each is counted on its own. `with_dependents` is the advice
/// when proven dependents exist and `only_this` when there are none to carry
/// along.
fn impact_refusal(impact: &DependentMods, with_dependents: &str, only_this: &str) -> String {
    let mut sentences: Vec<String> = Vec::new();
    if !impact.breaks.is_empty() {
        sentences.push(counted(
            impact.breaks.len(),
            "1 mod needs this one.",
            "mods need this one.",
        ));
    }
    if !impact.degrades.is_empty() {
        sentences.push(counted(
            impact.degrades.len(),
            "1 mod uses it optionally.",
            "mods use it optionally.",
        ));
    }
    let unanswered = impact.unknown.len() + impact.unidentifiable.len();
    if unanswered > 0 {
        sentences.push(counted(
            unanswered,
            "1 mod has no recorded requirements, so it is not known whether it needs this one.",
            "mods have no recorded requirements, so it is not known whether they need this one.",
        ));
    }
    sentences.push(
        if impact.breaks.is_empty() {
            only_this
        } else {
            with_dependents
        }
        .to_owned(),
    );
    sentences.join(" ")
}

/// Why turning a mod on stopped.
fn requirement_refusal(needs: &RequiredMods) -> String {
    let mut sentences: Vec<String> = Vec::new();
    if !needs.disabled.is_empty() {
        sentences.push(counted(
            needs.disabled.len(),
            "1 required mod is turned off.",
            "required mods are turned off.",
        ));
    }
    if !needs.absent.is_empty() {
        sentences.push(counted(
            needs.absent.len(),
            "1 required mod is not installed here.",
            "required mods are not installed here.",
        ));
    }
    if !needs.incompatible.is_empty() {
        sentences.push(match needs.incompatible.len() {
            1 => "It is declared incompatible with 1 installed mod.".to_owned(),
            count => format!("It is declared incompatible with {count} installed mods."),
        });
    }
    sentences.push(if needs.disabled.is_empty() {
        "Pass --only-this to turn on this mod.".to_owned()
    } else {
        "Pass --with-dependencies to turn on the required mods that are turned off, or \
         --only-this to turn on just this mod."
            .to_owned()
    });
    sentences.join(" ")
}

/// A sentence about how many mods something holds for: `one` as written for
/// a single mod, otherwise the count followed by `many`.
fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        one.to_owned()
    } else {
        format!("{count} {many}")
    }
}

/// What turning one mod off would do to the rest.
fn print_dependency_impact(impact: &DependentMods, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "mod": impact.target.name,
            "path": json_path(&impact.target.logical_path),
            "breaks": impacted_json(&impact.breaks),
            "degrades": impacted_json(&impact.degrades),
            "unknown": impacted_json(&impact.unknown),
            "unidentifiable": impacted_json(&impact.unidentifiable),
            "complete": impact.is_complete(),
        }));
        return;
    }
    for entry in &impact.breaks {
        eprintln!("needs it:   {}", entry.name);
    }
    for entry in &impact.degrades {
        eprintln!("optional:   {}", entry.name);
    }
    for entry in &impact.unknown {
        eprintln!("not known:  {} (run `ephervex mod refresh`)", entry.name);
    }
    for entry in &impact.unidentifiable {
        eprintln!("unknowable: {}", entry.name);
    }
}

/// What turning one mod on would need.
fn print_required_mods(needs: &RequiredMods, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "mod": needs.target.name,
            "path": json_path(&needs.target.logical_path),
            "disabled": impacted_json(&needs.disabled),
            "absent": impacted_json(&needs.absent),
            "incompatible": impacted_json(&needs.incompatible),
        }));
        return;
    }
    for entry in &needs.disabled {
        eprintln!("turned off: {}", entry.name);
    }
    for entry in &needs.absent {
        eprintln!("missing:    {}", entry.name);
    }
    for entry in &needs.incompatible {
        eprintln!("conflicts:  {}", entry.name);
    }
}

/// Asks each catalog what the host's mods need, and records the answers.
async fn run_mod_refresh(app: &App, host: InstalledHost, json: bool) -> anyhow::Result<()> {
    let report = app
        .refresh_mod_dependencies(
            host,
            &OperationReporter::silent("refreshing-mod-dependencies"),
            &CancellationToken::new(),
        )
        .await?;
    print_dependency_refresh(&report, json);
    unreachable_catalogs_result(&report)
}

/// Whether every catalog the refresh needed answered.
///
/// The report has been printed in full by then. Mods a catalog could not answer
/// for are still unrecorded, which a script must not see as success.
fn unreachable_catalogs_result(report: &DependencyRefreshReport) -> anyhow::Result<()> {
    if report.is_complete() {
        return Ok(());
    }
    let names = report
        .providers_unavailable
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "these catalogs could not be reached, so their mods are still unrecorded: {names}"
    )
}

/// Shows what the host's mods declare about each other.
///
/// Offline: it reads the recipe and the directory, so it answers the same on a
/// machine with no network as on one with.
fn run_mod_dependencies(
    app: &App,
    host: InstalledHost,
    of: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let relations = match of {
        Some(path) => vec![app.relations_of(host, path)?],
        None => app.mod_relations(host)?,
    };
    print_mod_relations(&relations, json);
    Ok(())
}

/// What each mod declares, and what declares it.
///
/// One block per mod, and an empty direction is left out rather than printed
/// as a heading with nothing under it. A mod whose catalog was never asked
/// says so, because otherwise "nobody asked" and "needs nothing" would read
/// the same - which is the confusion this whole feature exists to avoid.
fn print_mod_relations(relations: &[ModRelations], json: bool) {
    if json {
        let rows: Vec<serde_json::Value> = relations
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "mod": entry.target.name,
                    "path": json_path(&entry.target.logical_path),
                    "presence": entry.target.presence.as_str(),
                    "knowledge": entry.target.knowledge.as_str(),
                    "requires": related_json(&entry.requires),
                    "required_by": related_json(&entry.required_by),
                })
            })
            .collect();
        print_json(&serde_json::Value::Array(rows));
        return;
    }
    for entry in relations {
        outln!("{}", entry.target.logical_path.display());
        match entry.target.knowledge {
            DependencyKnowledge::Recorded => {}
            DependencyKnowledge::Unrecorded => {
                outln!("  not asked  (run `ephervex mod refresh`)");
            }
            DependencyKnowledge::Unidentified => {
                outln!("  not asked  (no exact release recorded to ask about)");
            }
            DependencyKnowledge::Unidentifiable => {
                outln!("  not asked  (nothing records what this JAR is)");
            }
        }
        for related in &entry.requires {
            outln!("  needs      {}", related_line(related));
        }
        for related in &entry.required_by {
            outln!("  needed by  {}", related_line(related));
        }
    }
}

/// One relation as a line: what it was, who it is about, and how that mod
/// stands here when a jar for it is here at all.
fn related_line(related: &RelatedMod) -> String {
    let state = match related.presence {
        Some(ModPresence::Installed) => String::new(),
        Some(presence) => format!("  ({})", presence.as_str()),
        None => "  (not installed here)".to_owned(),
    };
    format!(
        "{:<12} {}{state}",
        related.kind.as_str(),
        related
            .logical_path
            .as_deref()
            .map_or_else(|| related.name.clone(), |path| path.display().to_string())
    )
}

fn related_json(relations: &[RelatedMod]) -> Vec<serde_json::Value> {
    relations
        .iter()
        .map(|related| {
            serde_json::json!({
                "kind": related.kind.as_str(),
                "name": related.name,
                "path": related.logical_path.as_deref().map(json_path),
                "presence": related.presence.map(ModPresence::as_str),
                "project_id": related.project_id,
            })
        })
        .collect()
}

fn impacted_json(mods: &[ImpactedMod]) -> Vec<serde_json::Value> {
    mods.iter()
        .map(|entry| {
            serde_json::json!({
                "name": entry.name,
                "path": json_path(&entry.logical_path),
                "distance": entry.distance,
                "knowledge": entry.knowledge.as_str(),
            })
        })
        .collect()
}

/// Reports every mod one call turned on or off, in the order it did.
fn print_mod_states(applied: &[(PathBuf, ModPresence)], json: bool) {
    if json {
        let rows: Vec<serde_json::Value> = applied
            .iter()
            .map(|(path, presence)| {
                serde_json::json!({
                    "path": json_path(path),
                    "presence": presence.as_str(),
                    "enabled": presence.is_enabled(),
                })
            })
            .collect();
        print_json(&serde_json::Value::Array(rows));
        return;
    }
    for (path, presence) in applied {
        outln!("{}  {}", presence.as_str(), path.display());
    }
}

/// Works out what installing one mod would do, without doing it.
async fn run_mod_plan(
    app: &App,
    host: InstalledHost,
    project: &str,
    selection: &ModSelectionArgs,
    json: bool,
) -> anyhow::Result<()> {
    let catalog = catalog_or_preference(app, selection.provider)?;
    let plan = app
        .plan_mod_installation(host, &selection.request(project, catalog))
        .await?;
    print_mod_plan(&plan, host, json);
    Ok(())
}

/// Installs one mod, and whatever it needs, into an instance or a server.
///
/// A server install needs the caller to have said that mods whose catalog never
/// stated server support belong there. The plan is read first so those mods can
/// be named, and so the refusal happens before anything is written.
async fn run_mod_install(
    app: &App,
    host: InstalledHost,
    project: &str,
    selection: &ModSelectionArgs,
    consent: &InstallConsentArgs,
    json: bool,
) -> anyhow::Result<()> {
    let catalog = catalog_or_preference(app, selection.provider)?;
    let mut request = selection
        .request(project, catalog)
        .letting_installed_stand_in_for(consent.use_installed.iter().cloned());
    if matches!(host, InstalledHost::Server(_)) {
        let plan = app.plan_mod_installation(host, &request).await?;
        request = request.confirming_server_support(server_support_confirmed(&plan, consent)?);
    }
    let reporter = OperationReporter::silent("installing-mods");
    let installed = match app
        .install_mods(host, &request, &reporter, &CancellationToken::new())
        .await
    {
        Err(CoreError::ModDependencyAlreadyProvided {
            dependency,
            project_id,
            path,
            ..
        }) => return Err(dependency_already_provided(&dependency, &project_id, &path)),
        Err(CoreError::ModServerSupportUnconfirmed { projects }) => {
            return Err(unconfirmed_server_support(&projects));
        }
        installed => installed?,
    };
    print_installed_mods(&installed, host, json);
    Ok(())
}

/// The projects with no stated server side a server install confirms, or the
/// refusal naming the ones it would need confirmed.
///
/// `--accept-unknown-server-support` confirms every one of them. Without it, a
/// dependency a jar already on the server may stand in for is left to the
/// install, which refuses it only when nothing stands in for it.
fn server_support_confirmed(
    plan: &ModInstallPlan,
    consent: &InstallConsentArgs,
) -> anyhow::Result<Vec<String>> {
    let undeclared = plan.undeclared_server_support();
    if consent.accept_unknown_server_support {
        return Ok(undeclared
            .iter()
            .map(|planned| planned.project.id.clone())
            .collect());
    }
    let unconfirmed: Vec<&PlannedMod> = undeclared
        .into_iter()
        .filter(|planned| {
            planned.project.id == plan.root.project.id
                || !consent.use_installed.contains(&planned.project.id)
        })
        .collect();
    if unconfirmed.is_empty() {
        Ok(Vec::new())
    } else {
        Err(unconfirmed_server_support(&mod_names(&unconfirmed)))
    }
}

/// Refuses an install whose dependency a jar already installed provides,
/// naming the option that lets that jar stand in for it.
fn dependency_already_provided(dependency: &str, project: &str, path: &Path) -> anyhow::Error {
    let args = || {
        [
            ("dependency", MessageArg::from(dependency)),
            ("project", MessageArg::from(project)),
            ("path", MessageArg::from(path)),
        ]
    };
    let next = ephervex_i18n::lookup_attribute_args(
        "mod-install-dependency-already-provided",
        "action",
        args(),
    )
    .unwrap_or_default();
    anyhow::anyhow!(next).context(ephervex_i18n::lookup_args(
        "mod-install-dependency-already-provided",
        args(),
    ))
}

/// Refuses a server install that would add mods with no stated server side.
///
/// The next step is its own line of the failure, as every other failure's is,
/// rather than a newline inside the first, which the terminal output escapes.
fn unconfirmed_server_support(mods: &[String]) -> anyhow::Error {
    let next = ephervex_i18n::lookup_attribute("mod-install-server-support-unconfirmed", "action")
        .unwrap_or_default();
    anyhow::anyhow!(next).context(ephervex_i18n::lookup_args(
        "mod-install-server-support-unconfirmed",
        [("mods", MessageArg::from(mods.join(", ")))],
    ))
}

/// The display names of planned mods, falling back to the project id.
fn mod_names(planned: &[&PlannedMod]) -> Vec<String> {
    planned
        .iter()
        .map(|planned| {
            if planned.project.title.is_empty() {
                planned.project.id.clone()
            } else {
                planned.project.title.clone()
            }
        })
        .collect()
}

/// Shows one mod and the releases available for an instance.
async fn run_mod_show(
    app: &App,
    host: InstalledHost,
    project: &str,
    provider: Option<CatalogArg>,
    include_incompatible: bool,
    json: bool,
) -> anyhow::Result<()> {
    let catalog = catalog_or_preference(app, provider)?;
    let details = app
        .mod_project(host, catalog, project, include_incompatible)
        .await?;
    print_mod_project(&details, host, json);
    Ok(())
}

/// Checks a host's recorded files and reports what it found.
///
/// A fault sets the exit status, so a script asking whether a host is sound
/// gets its answer without parsing prose.
async fn run_mod_verify(
    app: &App,
    host: &HostArgs,
    shared: bool,
    json: bool,
) -> anyhow::Result<()> {
    let host = host.host()?;
    let shared_instance = match (shared, host) {
        (false, _) => None,
        (true, InstalledHost::Instance(instance)) => Some(instance),
        (true, InstalledHost::Server(_)) => {
            anyhow::bail!("--shared checks a client instance's shared files; a server has none")
        }
    };
    let report = app
        .verify_host(
            host,
            &OperationReporter::silent("verifying-instance"),
            &CancellationToken::new(),
        )
        .await?;
    let artifacts = match shared_instance {
        Some(instance) => Some(app.verify_shared_artifacts(instance).await?),
        None => None,
    };
    print_verification(&report, artifacts.as_ref(), json);
    let shared_sound = artifacts
        .as_ref()
        .is_none_or(SharedArtifactReport::is_sound);

    if report.is_sound() && shared_sound {
        return Ok(());
    }
    if !shared_sound {
        anyhow::bail!("a shared file this instance launches from is missing or wrong");
    }
    let unrepairable = report.unrepairable().count();
    if unrepairable > 0 {
        anyhow::bail!(
            "{unrepairable} of these record no address, so replacing them is not something \
             Ephervex can do"
        );
    }
    anyhow::bail!("this {} does not hold what it recorded", host_noun(host))
}

/// The word a message uses for the kind of host it is about.
const fn host_noun(host: InstalledHost) -> &'static str {
    match host {
        InstalledHost::Instance(_) => "instance",
        InstalledHost::Server(_) => "server",
    }
}

/// Replaces what a host is missing or holding wrongly.
async fn run_mod_repair(app: &App, host: InstalledHost, json: bool) -> anyhow::Result<()> {
    let report = app
        .repair_host(
            host,
            &OperationReporter::silent("repairing-instance"),
            &CancellationToken::new(),
        )
        .await?;

    if json {
        print_json(&serde_json::json!({
            "complete": report.is_complete(),
            "repaired": json_paths(&report.repaired),
            "failed": json_paths(&report.failed),
            "skipped": json_paths(&report.skipped),
            "unrepairable": json_paths(&report.unrepairable),
            "second_copies": report
                .second_copies
                .iter()
                .map(|copy| serde_json::json!({
                    "path": json_path(&copy.logical_path),
                    "mod_id": copy.mod_id,
                    "existing": json_path(&copy.existing),
                }))
                .collect::<Vec<_>>(),
        }));
    } else {
        for path in &report.repaired {
            outln!("repaired  {}", path.display());
        }
        if report.repaired.is_empty() && report.is_complete() {
            outln!("nothing needed replacing");
        }
    }

    if report.is_complete() {
        return Ok(());
    }
    // A server has no pack to import again, and one built from published
    // server files records no address for any of its jars.
    let way_back = match host {
        InstalledHost::Instance(_) => "re-import the pack to get it back",
        InstalledHost::Server(_) => "put the file back in the server folder by hand",
    };
    for path in &report.unrepairable {
        eprintln!("no address recorded: {} - {way_back}", path.display());
    }
    for path in &report.failed {
        eprintln!("could not be replaced: {}", path.display());
    }
    for copy in &report.second_copies {
        eprintln!(
            "not put back: {} - {} already provides mod id {}; remove or disable one of them first",
            copy.logical_path.display(),
            copy.existing.display(),
            copy.mod_id
        );
    }
    for path in &report.skipped {
        eprintln!("not reached: {}", path.display());
    }
    anyhow::bail!(
        "this {} still does not hold what it recorded",
        host_noun(host)
    )
}

/// The shared-store report as JSON.
fn shared_artifacts_json(report: &SharedArtifactReport) -> serde_json::Value {
    serde_json::json!({
        "metadata_cached": report.metadata_cached,
        "sound": report.is_sound(),
        "loader_libraries_uncovered": report.loader_libraries_uncovered,
        "files": report
            .files
            .iter()
            .map(|file| serde_json::json!({
                "artifact": json_path(&file.logical_path),
                "integrity": file.integrity.as_str(),
            }))
            .collect::<Vec<_>>(),
    })
}

/// Prints what checking the shared stores found.
fn print_shared_artifacts(report: &SharedArtifactReport) {
    if !report.metadata_cached {
        outln!("shared files: not checked, no metadata cached for this release");
        return;
    }
    for file in report.faults() {
        outln!(
            "{}  {}",
            file.integrity.as_str(),
            file.logical_path.display()
        );
    }
    let waiting = report.not_yet_fetched().count();
    let checked = report.files.len() - waiting;
    if report.is_sound() && checked > 0 {
        outln!("{checked} shared files checked, none wrong");
    }
    if waiting > 0 {
        outln!("{waiting} are not downloaded yet; launching fetches them");
    }
    if report.loader_libraries_uncovered {
        eprintln!("the loader's own libraries were not among them");
    }
}

/// Reports what checking an instance's recorded files found.
///
/// The shared-store report, when one was asked for, goes into the same JSON
/// document under `shared` so the output stays a single value.
fn print_verification(
    report: &VerificationReport,
    shared: Option<&SharedArtifactReport>,
    json: bool,
) {
    if json {
        let mut document = serde_json::json!({
            "sound": report.is_sound(),
            "files": report
                .files
                .iter()
                .map(|file| serde_json::json!({
                    "path": json_path(&file.logical_path),
                    "integrity": file.integrity.as_str(),
                    "repairable": file.integrity.is_repairable() && file.has_source,
                }))
                .collect::<Vec<_>>(),
        });
        if let Some(shared) = shared {
            document["shared"] = shared_artifacts_json(shared);
        }
        print_json(&document);
        return;
    }
    for file in &report.files {
        match file.integrity {
            Integrity::Intact => {}
            Integrity::Unverifiable => outln!("unproven  {}", file.logical_path.display()),
            _ => outln!(
                "{}  {}",
                file.integrity.as_str(),
                file.logical_path.display()
            ),
        }
    }
    if report.is_sound() {
        use std::fmt::Write as _;

        let unproven = report.count(&Integrity::Unverifiable);
        let aside = report.count(&Integrity::PutAside);
        let mut summary = if unproven == 0 && aside == 0 {
            format!("{} files, all intact", report.files.len())
        } else {
            format!("{} files intact", report.files.len() - unproven - aside)
        };
        if unproven > 0 {
            let _ = write!(
                summary,
                ", {unproven} unproven: no digest is recorded to check them against"
            );
        }
        if aside > 0 {
            let _ = write!(summary, ", {aside} put aside and kept to restore");
        }
        outln!("{summary}");
    }
    if let Some(shared) = shared {
        print_shared_artifacts(shared);
    }
}

/// Reports what a dependency refresh learned.
///
/// An unreachable catalog is named in the JSON, and in the error the command
/// then fails with, rather than folded into a count: the mods it covers are
/// still unrecorded, and knowing which catalog to retry is the difference
/// between running this again and giving up on it.
fn print_dependency_refresh(report: &DependencyRefreshReport, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "recorded": report.recorded,
            "unchanged": report.unchanged,
            "unidentified": report.unidentified,
            "providers_unavailable": report
                .providers_unavailable
                .iter()
                .map(|provider| provider.as_str())
                .collect::<Vec<_>>(),
            "complete": report.is_complete(),
        }));
        return;
    }
    outln!(
        "recorded {}  unchanged {}  no exact release {}",
        report.recorded,
        report.unchanged,
        report.unidentified
    );
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
            Some(provider) => outln!("{}", provider.as_str()),
            None => outln!("{}", preference.stored()),
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
        IncompatibilityReason::ClientOnly => ephervex_i18n::lookup(reason.message_id()),
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
        IncompatibilityReason::ClientOnly => serde_json::json!({ "reason": reason.as_str() }),
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

/// Like [`compatibility_json`], with `compatible` null while the optional
/// dependency could not be checked, and the reason named.
fn optional_status_json(status: &OptionalStatus) -> serde_json::Value {
    serde_json::json!({
        "compatible": match status {
            OptionalStatus::Compatible => Some(true),
            OptionalStatus::Incompatible(_) => Some(false),
            OptionalStatus::Unverified(_) => None,
        },
        "reasons": status.reasons().iter().map(incompatibility_json).collect::<Vec<_>>(),
        "unverified": match status {
            OptionalStatus::Unverified(unverified) => Some(unverified.as_str()),
            _ => None,
        },
    })
}

fn optional_unverified_text(candidate: &OptionalCandidate) -> Option<String> {
    let OptionalStatus::Unverified(unverified) = &candidate.status else {
        return None;
    };
    let Some(project_id) = candidate.project_id.as_deref() else {
        return Some(ephervex_i18n::lookup_args(
            "mod-optional-release-unverified",
            [(
                "release",
                MessageArg::from(candidate.pinned_version_id.as_deref().unwrap_or_default()),
            )],
        ));
    };
    let project = MessageArg::from(project_id);
    Some(match unverified {
        UnverifiedOptional::ConflictingIdentity { version_project_id } => {
            ephervex_i18n::lookup_args(
                unverified.message_id(),
                [
                    ("project", project),
                    ("owner", MessageArg::from(version_project_id.as_str())),
                ],
            )
        }
        UnverifiedOptional::Unavailable | UnverifiedOptional::UnknownIdentity => {
            ephervex_i18n::lookup_args(unverified.message_id(), [("project", project)])
        }
    })
}

fn print_mod_search(results: &ModSearchResults, host: InstalledHost, json: bool) {
    if json {
        print_json(&search_json(results));
        return;
    }

    if results.hits.is_empty() {
        outln!("{}", ephervex_i18n::lookup("mod-search-empty"));
        if let Some(action) = ephervex_i18n::lookup_attribute("mod-search-empty", "action") {
            outln!("  {action}");
        }
    }

    for hit in &results.hits {
        outln!("{}  {}", hit.project.id, hit.project.title);
        outln!("  {}", hit.project.description);
        if let Some(note) = server_support_note(host, hit.effective_sides().1) {
            outln!("  {note}");
        }
        match &hit.best_version {
            Some(version) => outln!("  version: {} ({})", version.version_number, version.id),
            None => {
                // The server note above already says a mod is client-only.
                for reason in hit
                    .compatibility
                    .reasons()
                    .iter()
                    .filter(|reason| **reason != IncompatibilityReason::ClientOnly)
                {
                    outln!("  {}", incompatibility_text(reason));
                }
            }
        }
    }
    if !results.hits.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::lookup_args(
                "mod-search-count",
                [("count", MessageArg::from(results.hits.len()))]
            )
        );
    }
    // A page can show fewer mods than asked for, or none, when some did not
    // fit, so where the next one starts is said rather than left to be
    // worked out from what was shown.
    if let Some(next) = results.next_offset {
        outln!(
            "{}",
            ephervex_i18n::lookup_args(
                "mod-search-next-page",
                [("offset", MessageArg::from(next))]
            )
        );
    }
}

/// A page of search results for `--json`, with where the next page starts.
fn search_json(results: &ModSearchResults) -> serde_json::Value {
    serde_json::json!({
        "provider": results.provider.as_str(),
        "total_hits": results.total_hits,
        "offset": results.offset,
        "next_offset": results.next_offset,
        "hits": results.hits.iter().map(|hit| serde_json::json!({
            "project_id": hit.project.id,
            "slug": hit.project.slug,
            "title": hit.project.title,
            "description": hit.project.description,
            "author": hit.project.author,
            "client_side": hit.project.client_side.to_string(),
            "server_side": hit.project.server_side.to_string(),
            "effective_server_side": hit.effective_sides().1.to_string(),
            "best_version": hit.best_version.as_ref().map(|version| serde_json::json!({
                "id": version.id,
                "version_number": version.version_number,
                "file_name": version.file.file_name,
            })),
            "compatibility": compatibility_json(&hit.compatibility),
        })).collect::<Vec<_>>(),
    })
}

/// What a server's listing says about a mod whose catalog does not confirm it
/// runs on a server. Nothing for a client, or for a mod that does.
fn server_support_note(host: InstalledHost, server_side: SideSupport) -> Option<String> {
    if !matches!(host, InstalledHost::Server(_)) {
        return None;
    }
    match server_side {
        SideSupport::Unknown => Some(ephervex_i18n::lookup("mod-search-server-support-unstated")),
        SideSupport::Unsupported => Some(ephervex_i18n::lookup("mod-search-client-only")),
        SideSupport::Required | SideSupport::Optional => None,
    }
}

fn print_mod_project(details: &ModProjectDetails, host: InstalledHost, json: bool) {
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

    outln!("{}  {}", details.project.id, details.project.title);
    outln!("  {}", details.project.description);
    outln!(
        "  client: {}  server: {}",
        details.project.client_side,
        details.project.server_side
    );
    if let Some(note) = server_support_note(host, details.project.server_side) {
        outln!("  {note}");
    }
    for entry in &details.versions {
        let marker = if Some(&entry.version.id) == details.default_version.as_ref() {
            "*"
        } else {
            " "
        };
        outln!(
            "{marker} {}  {}  ({})",
            entry.version.id,
            entry.version.version_number,
            entry.version.file.file_name
        );
        for reason in entry.compatibility.reasons() {
            outln!("    {}", incompatibility_text(reason));
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
        "server_side": planned.server_side().to_string(),
    })
}

fn warning_text(warning: &PlanWarning) -> String {
    match warning {
        PlanWarning::ClientSideUnsupported { project_id }
        | PlanWarning::OptionalNotOffered { project_id } => ephervex_i18n::lookup_args(
            warning.message_id(),
            [("project", MessageArg::from(project_id.as_str()))],
        ),
        PlanWarning::ServerSideUnsupported {
            project_id,
            required_by,
        } => ephervex_i18n::lookup_args(
            warning.message_id(),
            [
                ("project", MessageArg::from(project_id.as_str())),
                ("required_by", MessageArg::from(required_by.join(", "))),
            ],
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

fn warnings_json(warnings: &[PlanWarning]) -> Vec<String> {
    warnings.iter().map(warning_text).collect()
}

fn print_mod_plan(plan: &ModInstallPlan, host: InstalledHost, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "provider": plan.provider.as_str(),
            "root": planned_json(&plan.root),
            "dependencies": plan.dependencies.iter().map(planned_json).collect::<Vec<_>>(),
            "optional": plan.optional.iter().map(|candidate| serde_json::json!({
                "project_id": candidate.project_id,
                "title": candidate.title,
                "pinned_version_id": candidate.pinned_version_id,
                "declared_by": candidate.declared_by,
                "selected": false,
                "version_id": candidate.version.as_ref().map(|version| version.id.clone()),
                "compatibility": optional_status_json(&candidate.status),
            })).collect::<Vec<_>>(),
            "external": plan.external.iter().map(|external| serde_json::json!({
                "kind": external.kind.as_str(),
                "file_name": external.file_name,
                "declared_by": external.declared_by,
            })).collect::<Vec<_>>(),
            "embedded": plan.embedded.iter().map(|embedded| serde_json::json!({
                "project_id": embedded.project_id,
                "version_id": embedded.version_id,
                "file_name": embedded.file_name,
                "declared_by": embedded.declared_by,
            })).collect::<Vec<_>>(),
            "already_installed": plan.already_installed.iter().map(|installed| serde_json::json!({
                "project_id": installed.project_id,
                "title": planned_title(plan, &installed.project_id),
                "version_id": installed.version_id,
            })).collect::<Vec<_>>(),
            "unconfirmed_server_support": plan
                .undeclared_server_support()
                .iter()
                .map(|planned| planned.project.id.clone())
                .collect::<Vec<_>>(),
            "warnings": warnings_json(&plan.warnings),
        }));
        return;
    }

    for line in plan_lines(plan, host) {
        outln!("{line}");
    }
    for warning in &plan.warnings {
        eprintln!("{}", warning_text(warning));
    }
}

/// What a plan prints to standard output, one line each.
fn plan_lines(plan: &ModInstallPlan, host: InstalledHost) -> Vec<String> {
    let mut lines = Vec::new();
    let server = matches!(host, InstalledHost::Server(_));
    lines.push(format!(
        "{}  {}  ({}){}",
        plan.root.version.project_id,
        plan.root.project.title,
        plan.root.version.version_number,
        server_side_suffix(&plan.root, server)
    ));
    // A dependency already present at the planned release is listed under
    // `=` below, and nothing is fetched for it.
    let to_install = plan.to_install();
    for planned in plan
        .dependencies
        .iter()
        .filter(|planned| to_install.contains(planned))
    {
        lines.push(format!(
            "  + {}  {}  ({}){}",
            planned.version.project_id,
            planned.project.title,
            planned.version.version_number,
            server_side_suffix(planned, server)
        ));
    }
    if server {
        for name in mod_names(&plan.undeclared_server_support()) {
            lines.push(format!(
                "  ? {}",
                ephervex_i18n::lookup_args(
                    "mod-plan-server-support-unstated",
                    [("name", MessageArg::from(name.as_str()))]
                )
            ));
        }
    }
    for candidate in &plan.optional {
        // Unchecked by default, and shown as such rather than hidden.
        let name = candidate
            .title
            .as_deref()
            .or(candidate.project_id.as_deref())
            .or(candidate.pinned_version_id.as_deref())
            .unwrap_or_default();
        match optional_unverified_text(candidate) {
            Some(note) => lines.push(format!("  [ ] {name} (optional): {note}")),
            None => lines.push(format!("  [ ] {name} (optional)")),
        }
    }
    for external in &plan.external {
        lines.push(format!(
            "  ? {} ({})",
            external.file_name,
            external.kind.as_str()
        ));
    }
    for installed in &plan.already_installed {
        match planned_title(plan, &installed.project_id) {
            Some(title) => lines.push(format!(
                "  = {}  {title}  (already installed)",
                installed.project_id
            )),
            None => lines.push(format!("  = {}  (already installed)", installed.project_id)),
        }
    }
    lines
}

/// The title of the planned mod for `project_id`, when the catalog gave one.
fn planned_title<'a>(plan: &'a ModInstallPlan, project_id: &str) -> Option<&'a str> {
    plan.all_mods()
        .into_iter()
        .find(|planned| planned.version.project_id == project_id)
        .map(|planned| planned.project.title.as_str())
        .filter(|title| !title.is_empty())
}

/// The server side a planned mod's catalog declared, for a server's output.
fn server_side_suffix(planned: &PlannedMod, server: bool) -> String {
    if server {
        format!("  server: {}", planned.server_side())
    } else {
        String::new()
    }
}

/// What an installation did, one line each. A recorded jar left alone because
/// its contents differ is named, since "nothing to do" would hide that the
/// requested release was not restored.
fn installation_lines(name: &str, installation: &ManagedModInstallation) -> Vec<String> {
    let mut lines = Vec::new();
    if installation.changed_anything() {
        lines.push(ephervex_i18n::lookup_args(
            "mod-install-completed",
            [("name", MessageArg::from(name))],
        ));
        lines.extend(
            installation
                .installed
                .iter()
                .map(|path| format!("  {}", path.display())),
        );
        lines.extend(
            installation
                .recorded
                .iter()
                .map(|path| install_path_line("mod-install-record-updated", path)),
        );
    } else if installation.modified.is_empty() {
        lines.push(ephervex_i18n::lookup("mod-install-nothing-to-do"));
    }
    lines.extend(
        installation
            .modified
            .iter()
            .map(|path| install_path_line("mod-install-kept-modified", path)),
    );
    lines.extend(installation.stood_in.iter().map(|stand_in| {
        ephervex_i18n::lookup_args(
            "mod-install-stood-in",
            [
                ("planned", MessageArg::from(stand_in.planned.as_path())),
                ("installed", MessageArg::from(stand_in.installed.as_path())),
                ("mod_ids", MessageArg::from(stand_in.mod_ids.join(", "))),
            ],
        )
    }));
    lines
}

fn install_path_line(message_id: &str, path: &Path) -> String {
    ephervex_i18n::lookup_args(message_id, [("path", MessageArg::from(path))])
}

/// The JSON an installation reports. Its plan carries the same warnings
/// `mod plan --json` does, so a script sees what text mode prints to stderr.
fn installed_mods_json(installed: &InstalledMods) -> serde_json::Value {
    serde_json::json!({
        "installed": json_paths(&installed.installation.installed),
        "repaired": json_paths(&installed.installation.repaired),
        "already_present": json_paths(&installed.installation.already_present),
        "recorded": json_paths(&installed.installation.recorded),
        "modified": json_paths(&installed.installation.modified),
        "stood_in": installed.installation.stood_in.iter().map(|stand_in| serde_json::json!({
            "planned": json_path(&stand_in.planned),
            "installed": json_path(&stand_in.installed),
            "mod_ids": stand_in.mod_ids,
        })).collect::<Vec<_>>(),
        "reused_from_cache": installed.installation.reused_from_cache,
        "bytes": installed.installation.bytes,
        "plan": {
            "root": planned_json(&installed.plan.root),
            "dependencies": installed.plan.dependencies.iter().map(planned_json).collect::<Vec<_>>(),
            "warnings": warnings_json(&installed.plan.warnings),
        },
    })
}

fn print_installed_mods(installed: &InstalledMods, host: InstalledHost, json: bool) {
    if json {
        print_json(&installed_mods_json(installed));
        return;
    }

    for line in installation_lines(&installed.plan.root.project.title, &installed.installation) {
        outln!("{line}");
    }
    if matches!(host, InstalledHost::Server(_)) {
        for planned in installed.plan.all_mods() {
            outln!(
                "  {}{}",
                if planned.project.title.is_empty() {
                    planned.project.id.as_str()
                } else {
                    planned.project.title.as_str()
                },
                server_side_suffix(planned, true)
            );
        }
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
                "path": json_path(&entry.logical_path),
                "file_name": entry.file_name,
                // Null rather than the file name, which is no evidence of
                // what the mod is.
                "name": entry.title(),
                "version": entry.installed_version(),
                "provider": entry.identity.provider().map(ModProvider::as_str),
                "project_id": entry.identity.project_id(),
                "version_id": entry.identity.version_id(),
                "origin": entry.origin.as_str(),
                "presence": entry.presence.as_str(),
                "enabled": entry.presence.is_enabled(),
            })).collect::<Vec<_>>(),
        }));
        return;
    }

    if snapshot.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::lookup(match snapshot.host {
                InstalledHost::Instance(_) => "mod-host-list-empty",
                InstalledHost::Server(_) => "mod-host-list-empty-server",
            })
        );
        return;
    }
    for entry in &snapshot.mods {
        outln!(
            "{}  {}  ({})",
            entry.presence.as_str(),
            entry.display_name(),
            entry.logical_path.display()
        );
    }
    let count = |presence: ModPresence| {
        snapshot
            .mods
            .iter()
            .filter(|entry| entry.presence == presence)
            .count()
    };
    outln!(
        "{}",
        ephervex_i18n::lookup_args(
            "mod-host-count",
            [
                ("installed", MessageArg::from(count(ModPresence::Installed))),
                ("disabled", MessageArg::from(count(ModPresence::Disabled))),
                ("missing", MessageArg::from(count(ModPresence::Missing))),
            ]
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
            outln!("{}", ephervex_i18n::lookup("crash-none"));
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
                "reported_file": entry.suspect.reported_file,
                "path": entry.logical_path.as_deref().map(json_path),
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

    outln!("{}", review.report_name);
    if let Some(failure) = &review.analysis.failure {
        outln!("  {failure}");
    }
    if review.analysis.client_code_on_server {
        outln!("  {}", ephervex_i18n::lookup("crash-client-code-on-server"));
    }
    if review.mods.is_empty() {
        outln!("{}", ephervex_i18n::lookup("crash-no-suspects"));
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
        outln!(
            "{}  {}  {reason}",
            entry.suspect.confidence().as_str(),
            entry.suspect.label()
        );
        match &entry.logical_path {
            Some(path) => outln!("    {}", path.display()),
            None => outln!("    {}", ephervex_i18n::lookup("crash-not-in-mod-list")),
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
        outln!("{}", ephervex_i18n::lookup("mod-list-empty"));
        return;
    }
    for managed in mods {
        outln!(
            "{}  {}  {}  ({})",
            managed.provider,
            managed.project_id,
            managed.version_id,
            managed.file_name
        );
        outln!("  {}  {}", managed.origin.as_str(), managed.file_name);
    }
    outln!(
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
        JavaCommand::List => show_runtimes(
            app.java_runtimes().await?.as_ref(),
            readable_pin_index(app).as_ref(),
            json,
        ),
        JavaCommand::Refresh => {
            show_runtimes(
                app.refresh_java_runtimes().await?.as_ref(),
                readable_pin_index(app).as_ref(),
                json,
            );
        }
        JavaCommand::Test { path } => {
            let runtime = app.test_java_runtime(path).await?;
            show_runtime_detail(&runtime, json);
        }
        JavaCommand::Select {
            minecraft,
            instance,
            server,
            java_path,
            install,
        } => {
            let target = match (instance, server) {
                (Some(instance), _) => JavaTarget::Client(*instance),
                (None, Some(server)) => JavaTarget::Server(*server),
                (None, None) => JavaTarget::VersionOnly,
            };
            let plan = app
                .plan_java(
                    minecraft.as_deref(),
                    &JavaPlanRequest {
                        target,
                        explicit_path: java_path.clone(),
                        install_if_missing: *install,
                        ..JavaPlanRequest::default()
                    },
                )
                .await?;
            show_java_plan(&plan, json);
        }
        JavaCommand::Install {
            major,
            instance,
            server,
        } => {
            let host = instance
                .map(InstalledHost::Instance)
                .or_else(|| server.map(InstalledHost::Server));
            match (host, major) {
                (Some(host), _) => {
                    let installed = app
                        .install_java_for_host(
                            host,
                            &OperationReporter::silent("installing-java"),
                            &CancellationToken::new(),
                        )
                        .await?;
                    show_host_java_install(host, &installed, json);
                }
                (None, Some(major)) => {
                    let installed = app.install_java(*major).await?;
                    show_managed_install(&installed, json);
                }
                (None, None) => anyhow::bail!("name a Java version, an instance, or a server"),
            }
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
                outln!("{signed_in}");
                outln!("uuid:     {}", session.account.id);
                outln!(
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
                outln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "auth-signed-in",
                        [("name", MessageArg::from(session.account.name.as_str()))],
                    )
                );
                if let Some(expires) = session.expires_at() {
                    outln!("expires:  {}", expires.to_rfc3339());
                }
            }
        }
        AccountCommand::Logout => {
            let outcome = app.logout()?;
            let message = match outcome {
                Some(CredentialOutcome::Deleted) => "auth-logged-out-credential-deleted",
                Some(CredentialOutcome::ScheduledForDeletion) => {
                    "auth-logged-out-credential-pending"
                }
                Some(CredentialOutcome::NoneStored) => "auth-logged-out-no-credential",
                None => "auth-nothing-to-log-out-of",
            };
            if json {
                print_json(&serde_json::json!({
                    "signed_out": outcome.is_some(),
                    "credential": outcome.map(CredentialOutcome::as_str),
                }));
            } else {
                outln!("{}", ephervex_i18n::lookup(message));
            }
        }
    }
    Ok(())
}

/// The account-management commands: everything that does not talk to Microsoft.
///
/// Split out from [`run_account_command`] because none of it is async - adding
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
                outln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "account-added-offline",
                        [("name", MessageArg::from(account.name.as_str()))]
                    )
                );
                outln!("uuid:     {}", account.id);
                if account.is_default {
                    outln!("default:  yes");
                }
            }
        }
        AccountCommand::Use { account } => {
            let account = app.set_default_account(account)?;
            if json {
                print_json(&account_json(&account));
            } else {
                outln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "account-default-set",
                        [("name", MessageArg::from(account.name.as_str()))]
                    )
                );
            }
        }
        AccountCommand::Remove { account } => {
            // Resolved before removal so the message can name what went.
            let removed = app.account(account)?;
            let AccountRemoval {
                remaining,
                credential,
            } = app.remove_account(account)?;
            if json {
                print_json(&serde_json::json!({
                    "removed": account_json(&removed),
                    "default": remaining.as_ref().map(account_json),
                    "credential": credential.as_str(),
                }));
            } else {
                outln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        match credential {
                            CredentialOutcome::Deleted => "account-removed-with-credential",
                            CredentialOutcome::ScheduledForDeletion => {
                                "account-removed-credential-pending"
                            }
                            CredentialOutcome::NoneStored => "account-removed",
                        },
                        [("name", MessageArg::from(removed.name.as_str()))]
                    )
                );
                match &remaining {
                    Some(account) => outln!(
                        "{}",
                        ephervex_i18n::lookup_args(
                            "account-default-set",
                            [("name", MessageArg::from(account.name.as_str()))]
                        )
                    ),
                    None => outln!("{}", ephervex_i18n::lookup("auth-signed-out")),
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
                eprintln!("{}...", ephervex_i18n::lookup(stage.message_id()));
            }
            _ => {}
        }
    }
}

/// `signed_in` is true only for an authenticated default account, so an offline
/// account reads the same as in text mode; its `kind` is under `default`.
fn account_status_json(status: &AccountStatus) -> serde_json::Value {
    serde_json::json!({
        "signed_in": status.active.as_ref().is_some_and(AccountSummary::is_authenticated),
        "default": status.active.as_ref().map(account_json),
        "accounts": status.accounts.iter().map(account_json).collect::<Vec<_>>(),
        "session_active": status.session_active,
        "credential_storage": status.credential_storage.map(CredentialStorage::as_str),
    })
}

fn show_account_status(status: &AccountStatus, json: bool) {
    if json {
        print_json(&account_status_json(status));
        return;
    }

    let Some(active) = &status.active else {
        outln!("{}", ephervex_i18n::lookup("auth-signed-out"));
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

    outln!(
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
    outln!("uuid:      {}", active.id);
    outln!(
        "kind:      {}",
        ephervex_i18n::lookup(active.kind.message_id())
    );
    if !active.last_authenticated_at.is_empty() {
        outln!("last auth: {}", active.last_authenticated_at);
    }
    outln!(
        "storage:   {}",
        account_storage_label(active, status.credential_storage)
    );
    outln!(
        "session:   {}",
        if status.session_active {
            "active"
        } else {
            "not established in this process"
        }
    );
    if status.accounts.len() > 1 {
        outln!(
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
        outln!("{}", ephervex_i18n::lookup("accounts-list-empty"));
        return;
    }

    for account in &status.accounts {
        // The marker matters more than it looks: with several accounts, which
        // one a launch uses is the single thing the user is checking for.
        outln!(
            "{} {}  {:<9} {:<26} {}",
            if account.is_default { "*" } else { " " },
            account.id,
            account.kind.as_str(),
            account.name,
            account_storage_label(account, status.credential_storage)
        );
    }
    outln!(
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
    match account.credential {
        CredentialPresence::NotApplicable => ephervex_i18n::lookup("auth-storage-none-needed"),
        CredentialPresence::Missing => ephervex_i18n::lookup("auth-storage-not-saved"),
        CredentialPresence::Unknown => ephervex_i18n::lookup("auth-storage-unknown"),
        CredentialPresence::Stored => storage.map_or_else(
            || ephervex_i18n::lookup("auth-storage-unknown"),
            |storage| ephervex_i18n::lookup(storage.message_id()),
        ),
    }
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
        "has_stored_credential": match account.credential {
            CredentialPresence::Stored => serde_json::Value::Bool(true),
            CredentialPresence::Missing | CredentialPresence::NotApplicable => {
                serde_json::Value::Bool(false)
            }
            CredentialPresence::Unknown => serde_json::Value::Null,
        },
        "credential": account.credential.as_str(),
    })
}

/// One ownership fact about a session, `null` when the entitlement lookup did
/// not answer: an unknown must not read as a definite `false`.
fn ownership_json(
    lookup: Option<&EntitlementLookup>,
    fact: fn(&Entitlements) -> bool,
) -> serde_json::Value {
    match lookup {
        Some(EntitlementLookup::Unavailable) => serde_json::Value::Null,
        Some(EntitlementLookup::Answered(entitlements)) => {
            serde_json::Value::Bool(fact(entitlements))
        }
        None => serde_json::Value::Bool(false),
    }
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
            "owns_game": ownership_json(session.entitlement_lookup(), Entitlements::owns_game),
            "game_pass": ownership_json(session.entitlement_lookup(), Entitlements::has_game_pass),
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
            write_out(
                curseforge_key_change_output(
                    json,
                    "stored the CurseForge API key in the system keyring",
                    || Ok(app.api_settings()?),
                )?
                .as_bytes(),
            );
        }
        ApiCommand::ClearCurseforgeKey => {
            app.clear_curseforge_api_key()?;
            write_out(
                curseforge_key_change_output(
                    json,
                    "removed the CurseForge API key from the system keyring",
                    || Ok(app.api_settings()?),
                )?
                .as_bytes(),
            );
        }
    }
    Ok(())
}

/// The non-secret API settings as `--json` reports them.
fn api_settings_document(settings: &ApiSettings) -> serde_json::Value {
    serde_json::json!({
        "microsoft_client_id": settings.effective_microsoft_client_id,
        "microsoft_client_id_is_custom": settings.microsoft_client_id_is_custom,
        "user_agent": settings.effective_user_agent,
        "user_agent_is_custom": !settings.user_agent.is_empty(),
        "curseforge_api_key": settings.curseforge_key.as_str(),
        "curseforge_default_key_available": settings.curseforge_default_key_available,
        "credential_storage": settings.credential_storage.map(CredentialStorage::as_str),
    })
}

/// What a CurseForge key change prints: the resulting settings under `--json`,
/// otherwise one sentence. The settings are read only when they are printed.
fn curseforge_key_change_output(
    json: bool,
    sentence: &str,
    settings: impl FnOnce() -> anyhow::Result<ApiSettings>,
) -> anyhow::Result<String> {
    if !json {
        return Ok(format!("{sentence}\n"));
    }
    let text = serde_json::to_string_pretty(&api_settings_document(&settings()?))?;
    Ok(format!("{text}\n"))
}

fn show_api_settings(settings: &ApiSettings, json: bool) {
    if json {
        print_json(&api_settings_document(settings));
        return;
    }

    // The Client ID is printed in full because it is not a secret: Ephervex is
    // a public/native client, which has no client secret and cannot have one
    outln!(
        "microsoft client id:  {}{}",
        settings.effective_microsoft_client_id,
        if settings.microsoft_client_id_is_custom {
            "  (configured)"
        } else {
            "  (this build's default)"
        }
    );
    outln!(
        "user agent:           {}{}",
        settings.effective_user_agent,
        if settings.user_agent.is_empty() {
            ""
        } else {
            "  (configured)"
        }
    );
    // Presence, never the value.
    outln!(
        "curseforge api key:   {}",
        match settings.curseforge_key {
            ephervex_core::CurseForgeKeyState::Configured => "stored in the system keyring",
            ephervex_core::CurseForgeKeyState::BuildDefault => {
                "not set (using this build's built-in key)"
            }
            ephervex_core::CurseForgeKeyState::Missing => "not set",
        }
    );
    if !settings.curseforge_key.is_configured() {
        // The address a user needs at exactly the moment they discover they
        // have no key. Not the Upload API, which is a different credential.
        outln!(
            "  apply for one:      {}",
            ephervex_core::CURSEFORGE_API_KEY_HELP
        );
    }
    outln!(
        "credential storage:   {}",
        settings.credential_storage.map_or_else(
            || ephervex_i18n::lookup("error-auth-secret-storage-unavailable"),
            |storage| ephervex_i18n::lookup(storage.message_id()),
        )
    );
}

/// What pins each runtime, or `None` after a warning when a host record
/// cannot be read, so the runtimes are still listed.
fn readable_pin_index(app: &App) -> Option<JavaPinIndex> {
    match app.java_pin_index() {
        Ok(pins) => Some(pins),
        Err(error) => {
            let (summary, _) = localized_failure(&error);
            eprintln!(
                "warning: could not check which settings pin each runtime: {}",
                terminal_safe(&summary)
            );
            None
        }
    }
}

/// A runtime's `pinned_by` value: null when the pins could not be read.
fn runtime_pinned_by_json(runtime: &JavaRuntime, pins: Option<&JavaPinIndex>) -> serde_json::Value {
    pins.map_or(serde_json::Value::Null, |pins| {
        runtime_pins_json(&pins.pins_for(runtime))
    })
}

fn show_runtimes(inventory: &RuntimeInventory, pins: Option<&JavaPinIndex>, json: bool) {
    if json {
        print_json(&serde_json::json!({
                    "runtimes": inventory
                        .runtimes
                        .iter()
                        .map(|runtime| {
                            let mut value = runtime_json(runtime);
                            value["pinned_by"] = runtime_pinned_by_json(runtime, pins);
                            value
                        })
                        .collect::<Vec<_>>(),
                    "failures": inventory
        .failures
        .iter()
        .map(|failure| serde_json::json!({
                            "path": json_path(&failure.origin.path),
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
        outln!("{}", ephervex_i18n::lookup("java-runtimes-list-empty"));
    } else {
        for runtime in &inventory.runtimes {
            outln!(
                "{:<4} {:<12} {:<8} {:<14} {}",
                runtime.version.major,
                runtime.version_string,
                runtime.arch,
                runtime.primary_source().as_str(),
                runtime.executable.display()
            );
            outln!("     {}", runtime.vendor);
            let Some(pinned) = pins.map(|pins| pins.pins_for(runtime)) else {
                continue;
            };
            for host in &pinned.hosts {
                outln!("     pinned by {} {}", host.host.kind(), host.name);
            }
            if pinned.launcher {
                outln!("     pinned by the launcher preference");
            }
        }
        outln!(
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
    outln!("path:     {}", runtime.executable.display());
    outln!(
        "java:     {} (major {})",
        runtime.version_string,
        runtime.version.major
    );
    outln!("runtime:  {}", runtime.runtime_version);
    outln!("vendor:   {}", runtime.vendor);
    outln!("arch:     {}", runtime.arch);
    if let Some(home) = &runtime.home {
        outln!("home:     {}", home.display());
    }
    for origin in &runtime.origins {
        outln!("found:    {} via {}", origin.path.display(), origin.kind);
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
                            "path": json_path(&rejected.executable),
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

    outln!("minecraft: {}", plan.minecraft_version);
    match &plan.required.basis {
        RequirementBasis::Metadata { component } => outln!(
            "requires:  java {} (Mojang runtime {component})",
            plan.required.major
        ),
        // Saying which of the two answered matters: one is Mojang's statement,
        // the other is this build's table.
        RequirementBasis::FallbackTable => outln!(
            "requires:  java {} (compatibility table; the metadata declares none)",
            plan.required.major
        ),
    }
    match &plan.choice {
        JavaPathChoice::Pinned { path, source } => outln!(
            "selected:  {} (pinned by {})",
            path.display(),
            source.as_str()
        ),
        JavaPathChoice::Automatic => outln!("selected:  {}", plan.runtime.executable.display()),
    }
    outln!(
        "runtime:   {} {} ({}, {})",
        plan.runtime.version_string,
        plan.runtime.arch,
        plan.runtime.vendor,
        plan.runtime.primary_source().as_str()
    );
    if plan.installed_now {
        outln!("installed: downloaded during this command");
    }
    outln!(
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
        outln!("jvm args:  {}", plan.jvm.jvm_args.join(" "));
    }
    for rejected in &plan.rejected {
        outln!(
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
            "directory": json_path(&installed.directory),
            "newly_installed": installed.newly_installed,
            "runtime": runtime_json(&installed.runtime),
        }));
        return;
    }
    outln!("release:   {}", installed.release.release_name);
    outln!("directory: {}", installed.directory.display());
    outln!("java:      {}", installed.runtime.executable.display());
    outln!(
        "status:    {}",
        if installed.newly_installed {
            "downloaded and installed"
        } else {
            "already installed"
        }
    );
}

/// A runtime installed for one host, and how the host would start using it.
fn show_host_java_install(host: InstalledHost, installed: &HostJavaInstall, json: bool) {
    let pin = match &installed.follow_up {
        Some(DiagnosticFix::PinJava(path)) => Some(path),
        _ => None,
    };
    if json {
        print_json(&serde_json::json!({
            "host": { "kind": host.kind(), "id": host.id() },
            "installation": {
                "release": installed.installation.release.release_name,
                "directory": json_path(&installed.installation.directory),
                "newly_installed": installed.installation.newly_installed,
                "runtime": runtime_json(&installed.installation.runtime),
            },
            "status": java_status_json(&installed.status),
            "pin_offered": pin.map(|path| json_path(path)),
        }));
        return;
    }
    show_managed_install(&installed.installation, false);
    print_java_status(&installed.status);
    if let Some(path) = pin {
        outln!(
            "to use it:  ephervex {} runtime {} --java {}",
            host_command(host),
            host.id(),
            path.display()
        );
    }
}

/// The command family a host is addressed through.
const fn host_command(host: InstalledHost) -> &'static str {
    match host {
        InstalledHost::Instance(_) => "instance",
        InstalledHost::Server(_) => "server",
    }
}

fn runtime_json(runtime: &JavaRuntime) -> serde_json::Value {
    serde_json::json!({
            "path": json_path(&runtime.executable),
            "major": runtime.version.major,
            "version": runtime.version_string,
            "runtime_version": runtime.runtime_version,
            "vendor": runtime.vendor,
            "arch": runtime.arch,
            "home": runtime.home.as_deref().map(json_path),
            "managed": runtime.is_managed(),
            "source": runtime.primary_source().as_str(),
            "origins": runtime
    .origins
    .iter()
    .map(|origin| serde_json::json!({
                    "source": origin.kind.as_str(),
                    "path": json_path(&origin.path),
                }))
    .collect::<Vec<_>>(),
        })
}

fn java_choice_json(choice: &JavaPathChoice) -> serde_json::Value {
    match choice {
        JavaPathChoice::Pinned { path, source } => serde_json::json!({
            "kind": "pinned",
            "path": json_path(path),
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
        outln!("{}", ephervex_i18n::lookup("minecraft-versions-list-empty"));
        return;
    }

    for record in releases {
        outln!(
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
    outln!(
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
    outln!("id:         {}", version.id);
    outln!("type:       {}", version.release_type);
    outln!("released:   {}", version.release_time.format("%Y-%m-%d"));
    outln!("main class: {}", version.main_class);
    match &version.java_version {
        // Metadata records the requirement; Java selection resolves it.
        Some(java) => outln!(
            "java:       {} (Mojang runtime {})",
            java.major_version,
            java.component
        ),
        None => outln!("java:       not declared"),
    }
    outln!(
        "arguments:  {} form",
        argument_form_label(version.arguments.form())
    );
    for (group, entries) in version.arguments.groups() {
        let conditional = entries
            .iter()
            .filter(|entry| !entry.is_unconditional())
            .count();
        outln!(
            "  {group:<17} {} entries, {conditional} conditional",
            entries.len()
        );
    }
    outln!(
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
    outln!(
        "client jar: {} bytes, sha1 {}",
        client.size,
        client.sha1.digest()
    );
    outln!(
        "assets:     index {} ({} bytes of objects)",
        assets.id,
        assets
            .total_size
            .map_or_else(|| "unknown".to_owned(), |size| size.to_string())
    );
    outln!(
        "downloads:  {}",
        version
            .downloads
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(logging) = &version.logging {
        outln!("logging:    {}", logging.id);
    }
    if let Some(parent) = version.inherits_from() {
        outln!("inherits:   {parent}  (merging lands with loader support)");
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
            "relocated": reconciled_json(&report.relocated),
            "unfinished_instances": reconciled_json(&report.unfinished_instances),
            "index_repaired": reconciled_json(&report.index_repaired),
            "interrupted_hosts": reconciled_json(&report.interrupted_hosts),
            "restores_rolled_back": reconciled_json(&report.restores_rolled_back),
            "persistence_unhealthy": reconciled_json(&report.persistence_unhealthy),
            "non_utf8_roots": reconciled_json(&report.non_utf8_roots),
            "client_conflicts": conflicts_json(
                report.client_conflicts.iter().map(|c| (c.id.to_string(), &c.roots)),
            ),
            "server_conflicts": conflicts_json(
                report.server_conflicts.iter().map(|c| (c.id.to_string(), &c.roots)),
            ),
            "needs_attention": report.needs_attention(),
        }));
        return;
    }

    if report.is_empty() {
        outln!("{}", ephervex_i18n::messages::nothing_to_reconcile());
        return;
    }

    if !report.removed_stale_temporary.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::removed_stale_temporary(report.removed_stale_temporary.len())
        );
    }
    if !report.reindexed.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::reindexed(report.reindexed.len())
        );
    }
    if !report.relocated.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::relocated(report.relocated.len())
        );
    }
    if !report.missing_persistent.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::missing_persistent(report.missing_persistent.len())
        );
        for entry in &report.missing_persistent {
            outln!(
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
        outln!("{}", ephervex_i18n::messages::missing_persistent_action());
    }
    if !report.unfinished_instances.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::unfinished_instances(report.unfinished_instances.len())
        );
        for entry in &report.unfinished_instances {
            outln!(
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
        outln!("{}", ephervex_i18n::messages::unfinished_instances_action());
    }
    show_reconciliation_warnings(report);
}

/// The states that change what the user may do next, after the routine summaries.
fn show_reconciliation_warnings(report: &ReconciliationReport) {
    if !report.index_repaired.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::index_repaired(report.index_repaired.len())
        );
    }
    if !report.restores_rolled_back.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::restores_rolled_back(report.restores_rolled_back.len())
        );
    }
    if !report.interrupted_hosts.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::interrupted_hosts(report.interrupted_hosts.len())
        );
        show_attention_details(&report.interrupted_hosts);
        outln!("{}", ephervex_i18n::messages::interrupted_hosts_action());
    }
    if !report.persistence_unhealthy.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::persistence_unhealthy(report.persistence_unhealthy.len())
        );
        show_attention_details(&report.persistence_unhealthy);
        outln!(
            "{}",
            ephervex_i18n::messages::persistence_unhealthy_action()
        );
    }
    if !report.non_utf8_roots.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::non_utf8_roots(report.non_utf8_roots.len())
        );
        for entry in &report.non_utf8_roots {
            outln!(
                "  {}",
                ephervex_i18n::lookup_args(
                    "reconcile-non-utf8-root-detail",
                    [
                        ("name", MessageArg::from(entry.name.as_str())),
                        ("path", MessageArg::from(entry.root.as_path())),
                    ]
                )
            );
        }
        outln!("{}", ephervex_i18n::messages::non_utf8_roots_action());
    }
    if !report.client_conflicts.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::client_conflicts(report.client_conflicts.len())
        );
        for conflict in &report.client_conflicts {
            show_conflict_detail(&conflict.id.to_string(), &conflict.roots);
        }
        outln!("{}", ephervex_i18n::messages::client_conflicts_action());
    }
    if !report.server_conflicts.is_empty() {
        outln!(
            "{}",
            ephervex_i18n::messages::server_conflicts(report.server_conflicts.len())
        );
        for conflict in &report.server_conflicts {
            show_conflict_detail(&conflict.id.to_string(), &conflict.roots);
        }
        outln!("{}", ephervex_i18n::messages::server_conflicts_action());
    }
}

fn show_attention_details(entries: &[ephervex_core::ReconciledRecord]) {
    for entry in entries {
        outln!(
            "  {}",
            ephervex_i18n::lookup_args(
                "reconcile-attention-detail",
                [
                    ("name", MessageArg::from(entry.name.as_str())),
                    ("path", MessageArg::from(entry.root.as_path())),
                ]
            )
        );
    }
}

fn show_conflict_detail(id: &str, roots: &[PathBuf]) {
    let paths = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    outln!(
        "  {}",
        ephervex_i18n::lookup_args(
            "reconcile-conflict-detail",
            [
                ("id", MessageArg::from(id)),
                ("paths", MessageArg::from(paths)),
            ]
        )
    );
}

// -------------------------------------------------------------------- output

/// `ephervex config pack-hosts [--forget HOST] [--forget-all]`.
fn run_pack_hosts_command(forget: &[String], forget_all: bool, json: bool) -> anyhow::Result<()> {
    let mut app = open_app()?;

    let mut forgotten = Vec::new();
    let mut unknown = Vec::new();
    let targets: Vec<String> = if forget_all {
        app.trusted_pack_hosts().to_vec()
    } else {
        forget.to_vec()
    };
    for host in targets {
        if app.withdraw_pack_host(&host)? {
            forgotten.push(host);
        } else {
            unknown.push(host);
        }
    }

    let remaining = app.trusted_pack_hosts();
    if json {
        print_json(&serde_json::json!({
            "trusted": remaining,
            "forgotten": forgotten,
            "not_trusted": unknown,
        }));
        return Ok(());
    }

    for host in &forgotten {
        outln!("forgot {host}");
    }
    // Named rather than passed over: asking to forget a site that was never
    // trusted usually means a typo, and silence would look like success.
    for host in &unknown {
        eprintln!("{host} was not on the list");
    }
    if remaining.is_empty() {
        outln!("No sites are trusted to serve modpacks.");
    } else {
        outln!("Trusted to serve modpacks:");
        for host in remaining {
            outln!("  {host}");
        }
    }
    Ok(())
}

/// `ephervex url-handler <status|register|unregister>`.
fn run_url_handler_command(command: &UrlHandlerCommand, json: bool) -> anyhow::Result<()> {
    let app = open_app()?;
    let status = match command {
        UrlHandlerCommand::Status => app.url_handler_status(),
        UrlHandlerCommand::Register => app.register_url_handler()?,
        UrlHandlerCommand::Unregister => {
            print_url_handler_removal(&app.unregister_url_handler()?, json);
            return Ok(());
        }
    };
    print_url_handler(&status, json);
    Ok(())
}

/// The fields `url-handler status` and `url-handler unregister` share in JSON.
fn url_handler_json(status: &ephervex_core::UrlHandlerStatus) -> serde_json::Value {
    serde_json::json!({
        "registered": status.registered,
        "entry": json_path(&status.entry),
        "command": status.command,
        "command_exists": status.command_exists(),
        "scheme": ephervex_core::scheme_mime_type(),
    })
}

/// Reports whether Ephervex opens its own links, and what it would run.
fn print_url_handler(status: &ephervex_core::UrlHandlerStatus, json: bool) {
    if json {
        print_json(&url_handler_json(status));
        return;
    }

    if !status.registered {
        outln!("Ephervex does not open ephervex:// links.");
        outln!("Run `ephervex url-handler register` to change that.");
        return;
    }
    outln!("Ephervex opens ephervex:// links.");
    print_url_handler_entry(status);
}

/// The entry in force and what it runs.
fn print_url_handler_entry(status: &ephervex_core::UrlHandlerStatus) {
    outln!("entry:   {}", status.entry.display());
    if let Some(command) = &status.command {
        outln!("runs:    {command}");
    }
    // A registration pointing at a moved artifact resolves, is run, and does
    // nothing. Saying so is more use than reporting it as registered.
    if !status.command_exists() {
        outln!(
            "warning: that path no longer exists. Run `ephervex url-handler register` again from where Ephervex is now."
        );
    }
}

/// Reports what `url-handler unregister` did and whether links still open.
///
/// The answer comes from the status read after removal: an entry a package
/// manager installed is not removed here and keeps handling links.
fn print_url_handler_removal(removal: &ephervex_core::UrlHandlerRemoval, json: bool) {
    if json {
        let mut report = url_handler_json(&removal.status);
        report["removed"] = serde_json::Value::Bool(removal.removed);
        print_json(&report);
        return;
    }

    match (removal.removed, removal.status.registered) {
        (true, false) => outln!("Ephervex no longer opens ephervex:// links."),
        (false, false) => outln!("Ephervex was not opening ephervex:// links."),
        (removed, true) => {
            if removed {
                outln!("Removed your own link handler entry.");
            } else {
                outln!("You have no link handler entry of your own to remove.");
            }
            outln!("Ephervex still opens ephervex:// links through an entry that is not yours:");
            print_url_handler_entry(&removal.status);
            outln!("Removing the Ephervex package is what takes that one back.");
        }
    }
}

/// Implements `ephervex import <archive-or-url> --inspect` without writing an
/// instance.
///
/// An address is downloaded first and asked about exactly as it would be for a
/// real import. Inspecting is the safer of the two operations, but it is the
/// download that reaches somebody's machine, and that is the part consent is
/// about.
async fn run_inspect(app: &mut App, args: &ImportArgs, json: bool) -> anyhow::Result<()> {
    let downloaded = fetch_if_address(app, args, json).await?;
    let archive = downloaded.as_ref().map_or_else(
        || PathBuf::from(&args.archive),
        |pack| pack.path().to_path_buf(),
    );
    let analysis = app.analyze_pack(&archive)?;
    print_analysis(&analysis, json);
    Ok(())
}

/// `ephervex import <file>`.
async fn run_import(app: &mut App, args: &ImportArgs, json: bool) -> anyhow::Result<()> {
    // Downloaded before anything else, and held for the whole import: dropping
    // it removes the archive, so a failed import leaves no file behind.
    let downloaded = fetch_if_address(app, args, json).await?;
    let archive = downloaded.as_ref().map_or_else(
        || PathBuf::from(&args.archive),
        |pack| pack.path().to_path_buf(),
    );

    let selection = if args.all_optional {
        OptionalSelection::all()
    } else {
        OptionalSelection::of(args.optional.clone())
    };
    let mut request = ImportRequest::new(&archive)
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

/// Downloads the pack when the argument is an address, or does nothing.
///
/// A file path and a URL are told apart by the argument parsing as an absolute
/// `http`/`https` address, which no local path does. Anything else is a path,
/// including one that does not exist: reporting "no such file" is more use than
/// guessing that a typo was meant to be a link.
async fn fetch_if_address(
    app: &mut App,
    args: &ImportArgs,
    json: bool,
) -> anyhow::Result<Option<ephervex_core::StagedPack>> {
    if !looks_like_an_address(&args.archive) {
        return Ok(None);
    }

    let source = app.pack_source(&args.archive)?;
    let mut consents = ephervex_core::PackConsents::none();
    if let Some(consent) = resolve_host_consent(app, &source, args, json)? {
        consents = consents.agreeing(source.host(), consent);
    }
    let reporter = ephervex_core::OperationReporter::silent("operation-downloading-pack");
    // A redirect to another site asks about that site, and the download is tried
    // again with every answer given so far.
    let staged = loop {
        match app
            .fetch_pack(&source, &consents, &reporter, &CancellationToken::new())
            .await
        {
            Ok(staged) => break staged,
            Err(ephervex_core::CoreError::PackHostNotTrusted { url, host })
                if host != source.host() =>
            {
                let consent = resolve_redirect_consent(&url, &host, json)?;
                consents = consents.agreeing(&host, consent);
            }
            Err(error) => return Err(error.into()),
        }
    };
    if !json {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "import-downloaded-pack",
                [("value", MessageArg::from(source.display()))]
            )
        );
    }
    Ok(Some(staged))
}

/// URI schemes are case-insensitive (RFC 3986 section 3.1), so `HTTPS://` is an address too.
fn looks_like_an_address(argument: &str) -> bool {
    let trimmed = argument.trim();
    ["http://", "https://"].iter().any(|scheme| {
        trimmed
            .get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    })
}

/// Whether this address may be downloaded from, and whether to remember it.
///
/// `None` means the host is already agreed to and the application service will
/// find that for itself. A flag answers without asking; otherwise an
/// interactive terminal is prompted and anything else is refused, because
/// downloading from a host a web page chose is not a decision to make on a
/// user's behalf (ADR 0027).
fn resolve_host_consent(
    app: &App,
    source: &ephervex_core::PackSource,
    args: &ImportArgs,
    json: bool,
) -> anyhow::Result<Option<ephervex_core::HostConsent>> {
    if let Some(trust) = args.trust {
        return Ok(Some(trust.consent()));
    }
    if app.pack_host_is_trusted(source) {
        return Ok(None);
    }
    if json || !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "downloading a pack from {} has not been agreed to; pass --trust always to allow it and remember the host, or --trust once for this import only",
            source.display()
        );
    }

    let answer = ask(&format!(
        "Ephervex would download a pack from:\n  {}\nDownload from {} and remember it? [y/N/once] ",
        source.display(),
        source.host()
    ))?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(Some(ephervex_core::HostConsent::Remember)),
        "once" | "o" => Ok(Some(ephervex_core::HostConsent::JustOnce)),
        _ => anyhow::bail!("the download was not agreed to"),
    }
}

/// Whether a download may follow a redirect to another site.
///
/// `--trust` answers for the address that was typed, not for a site a server
/// sent the download to, so this asks on an interactive terminal and is refused
/// anywhere else (ADR 0027).
fn resolve_redirect_consent(
    target: &str,
    host: &str,
    json: bool,
) -> anyhow::Result<ephervex_core::HostConsent> {
    if json || !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "the download was redirected to {target}, which has not been agreed to; import from that address directly to agree to it"
        );
    }

    let answer = ask(&format!(
        "The download was redirected to another site. Ephervex would download from:\n  {target}\nDownload from {host} and remember it? [y/N/once] "
    ))?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(ephervex_core::HostConsent::Remember),
        "once" | "o" => Ok(ephervex_core::HostConsent::JustOnce),
        _ => anyhow::bail!("the download was not agreed to"),
    }
}

/// Makes text that came from an archive safe to print on a terminal.
///
/// Control characters and bidirectional formatting characters are replaced by
/// a visible `\u{..}` escape, so a pack name cannot clear the screen, recolor
/// the output, or add lines of its own. Everything else is left exact.
fn terminal_safe(text: &str) -> std::borrow::Cow<'_, str> {
    fn needs_escape(character: char) -> bool {
        character.is_control()
            || matches!(
                character,
                '\u{200e}'
                    | '\u{200f}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
    }

    if !text.chars().any(needs_escape) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut safe = String::with_capacity(text.len());
    for character in text.chars() {
        if needs_escape(character) {
            let _ = write!(safe, "\\u{{{:x}}}", u32::from(character));
        } else {
            safe.push(character);
        }
    }
    std::borrow::Cow::Owned(safe)
}

/// Prints what a pack is, without importing it.
fn print_analysis(analysis: &PackAnalysis, json: bool) {
    if json {
        print_json(&analysis_json(analysis));
        return;
    }

    outln!("pack:      {}", terminal_safe(analysis.name()));
    if let Some(version) = &analysis.recipe.original_version {
        outln!("version:   {}", terminal_safe(version));
    }
    if let Some(author) = analysis.author() {
        outln!("author:    {}", terminal_safe(author));
    }
    outln!("source:    {}", analysis.format);
    outln!("minecraft: {}", terminal_safe(analysis.minecraft_version()));
    outln!(
        "loader:    {}",
        terminal_safe(&analysis.loader().to_string())
    );
    outln!(
        "files:     {} required, {} optional",
        analysis.required_file_count(PackSide::Client),
        analysis.optional_file_count(PackSide::Client)
    );
    if analysis.needs_provider_resolution() {
        // The counts above come from the pack's own manifest. Which files they
        // are is a question only the provider can answer, and answering it
        // needs the key.
        outln!(
            "blocked:   a {} API key is required before these files can be listed",
            analysis.format
        );
    }
    for component in analysis.optional_components(PackSide::Client) {
        outln!(
            "optional:  {} [{}]",
            terminal_safe(&component.display_name()),
            if component.starts_selected() {
                "x"
            } else {
                " "
            }
        );
    }
    for layer in analysis.layers() {
        outln!("layer:     {}", layer.as_str());
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
        value["installed"] = json_paths(&imported.materialized.files.installed);
        value["preserved"] = json_paths(&imported.materialized.files.skipped);
        value["recipe"] = serde_json::json!({
                    "path": json_path(&imported.materialized.recipe_path),
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
    outln!("pack:      {}", terminal_safe(imported.analysis.name()));
    outln!(
        "installed: {} files",
        imported.materialized.files.installed.len()
    );
    outln!("recipe:    {}", imported.materialized.recipe_path.display());

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
///
/// The server side is the plan Create Server builds with its default policy for
/// files the pack never declared a server side for.
fn print_recipe(
    recipe: &PackRecipe,
    side: PackSide,
    selection: &OptionalSelection,
    json: bool,
) -> anyhow::Result<()> {
    let plan = match side {
        PackSide::Server => {
            recipe.server_preview_plan(selection, &UnknownServerFiles::default())?
        }
        PackSide::Client => recipe.plan(side, selection),
    };
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
                            "path": json_path(&file.logical_path),
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
        return Ok(());
    }

    outln!("pack:      {}", terminal_safe(&recipe.original_name));
    outln!("source:    {}", recipe.source_kind);
    outln!("side:      {}", side.as_str());
    outln!("minecraft: {}", terminal_safe(plan.minecraft_version));
    outln!("loader:    {}", terminal_safe(&plan.loader.to_string()));
    for file in &plan.files {
        outln!("file:      {}", terminal_safe(&file.display_name()));
    }
    for file in &recipe.files {
        if file.is_excluded_from(side) {
            outln!("excluded:  {}", terminal_safe(&file.display_name()));
        }
    }
    for layer in &plan.layers {
        outln!(
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
    Ok(())
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
                    "path": json_path(&component.logical_path),
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
        eprintln!("  {}", terminal_safe(address));
    }
}

fn print_instance(app: &App, record: &ClientRecord, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&instance_json(app, record)?);
    } else {
        outln!("id:        {}", record.id);
        outln!("name:      {}", record.name);
        outln!("storage:   {}", record.storage);
        outln!("minecraft: {}", record.minecraft_version);
        outln!("loader:    {}", record.loader);
        outln!("root:      {}", record.root.display());
        let view = app.organization_or_empty();
        if view.is_favorite_client(record.id) {
            outln!("favorite:  yes");
        }
        if let Some(group) = view.group_of(record.id) {
            outln!("group:     {}", group.name);
        }
        if let Some(state) = app.launch_state(record.id)? {
            outln!("running:   {}", describe_running(&state));
        }
        for id in app.linked_server_ids(record.id)? {
            outln!("server:    {id}");
        }
        for reason in &record.attention {
            outln!("attention: {reason:?}");
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
/// What one `instance runtime` or `server runtime` invocation asked for.
#[derive(Clone, Copy)]
struct RuntimeArgs<'a> {
    java: Option<&'a Path>,
    min_memory: Option<u32>,
    max_memory: Option<u32>,
    jvm_args: &'a [String],
    clear: bool,
}

async fn run_instance_runtime(
    app: &App,
    command: &InstanceCommand,
    json: bool,
) -> anyhow::Result<()> {
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
    let args = RuntimeArgs {
        java: java.as_deref(),
        min_memory: *min_memory,
        max_memory: *max_memory,
        jvm_args,
        clear: *clear,
    };
    let record = app.current_client(*id)?;
    let committed = match args.requested(record.java_override.clone(), record.jvm_override.clone())
    {
        Some((java, jvm)) => app.set_client_runtime_settings(*id, java, jvm)?,
        None => ClientCommit::clean(record),
    };
    let status = if args.asks_for_a_change() {
        None
    } else {
        Some(app.host_java_status(InstalledHost::Instance(*id)).await?)
    };
    let settings = HostRuntimeSettings {
        java: committed.record.java_override.as_ref(),
        jvm: committed.record.jvm_override.as_ref(),
        status: status.as_ref(),
    };
    if json {
        let mut value = settings.json();
        value["instance"] = serde_json::json!(committed.record.id.to_string());
        value["warnings"] = serde_json::json!(client_commit_warnings_json(&committed.warnings));
        print_json(&value);
    } else {
        settings.print();
        print_client_commit_warnings(&committed.warnings);
    }
    Ok(())
}

async fn run_server_runtime(app: &App, command: &ServerCommand, json: bool) -> anyhow::Result<()> {
    let ServerCommand::Runtime {
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
    let args = RuntimeArgs {
        java: java.as_deref(),
        min_memory: *min_memory,
        max_memory: *max_memory,
        jvm_args,
        clear: *clear,
    };
    let record = app.current_server(*id)?;
    let (record, warnings) =
        match args.requested(record.java_override.clone(), record.jvm_override.clone()) {
            Some((java, jvm)) => {
                let committed = app.set_server_runtime_settings(*id, java, jvm)?;
                (committed.record, committed.warnings)
            }
            None => (record, Vec::new()),
        };
    let status = if args.asks_for_a_change() {
        None
    } else {
        Some(app.host_java_status(InstalledHost::Server(*id)).await?)
    };
    let settings = HostRuntimeSettings {
        java: record.java_override.as_ref(),
        jvm: record.jvm_override.as_ref(),
        status: status.as_ref(),
    };
    if json {
        let mut value = settings.json();
        value["server"] = serde_json::json!(record.id.to_string());
        value["warnings"] = serde_json::json!(server_commit_warnings_json(&warnings));
        print_json(&value);
    } else {
        settings.print();
        print_server_commit_warnings(&warnings);
    }
    Ok(())
}

impl RuntimeArgs<'_> {
    const fn asks_for_a_change(&self) -> bool {
        self.clear
            || self.java.is_some()
            || self.min_memory.is_some()
            || self.max_memory.is_some()
            || !self.jvm_args.is_empty()
    }

    /// The overrides a host stores after this invocation, or `None` when it
    /// asked for no change.
    ///
    /// Each option changes only the value it names and leaves every other
    /// pinned value as it was. `--jvm-arg` replaces the pinned argument list
    /// as a whole.
    fn requested(
        &self,
        java: Option<JavaOverride>,
        jvm: Option<JvmSettings>,
    ) -> Option<(Option<JavaOverride>, Option<JvmSettings>)> {
        if self.clear {
            return Some((None, None));
        }
        if !self.asks_for_a_change() {
            return None;
        }
        let java = self.java.map(JavaOverride::new).or(java);
        let mut jvm = jvm.unwrap_or_default();
        jvm.min_memory_mib = self.min_memory.or(jvm.min_memory_mib);
        jvm.max_memory_mib = self.max_memory.or(jvm.max_memory_mib);
        if !self.jvm_args.is_empty() {
            jvm.jvm_args = self.jvm_args.to_vec();
        }
        Some((java, (!jvm.pins_nothing()).then_some(jvm)))
    }
}

/// What one host pins for itself, and, when asked, how its Java fits.
struct HostRuntimeSettings<'a> {
    java: Option<&'a JavaOverride>,
    jvm: Option<&'a JvmSettings>,
    status: Option<&'a HostJavaStatus>,
}

impl HostRuntimeSettings<'_> {
    fn json(&self) -> serde_json::Value {
        let jvm = self.jvm;
        let mut value = serde_json::json!({
            "java": self.java.map(|java| java.path.display().to_string()),
            "min_memory_mib": jvm.and_then(|jvm| jvm.min_memory_mib),
            "max_memory_mib": jvm.and_then(|jvm| jvm.max_memory_mib),
            "jvm_args": jvm.map(|jvm| jvm.jvm_args.clone()).unwrap_or_default(),
        });
        if let Some(status) = self.status {
            value["status"] = java_status_json(status);
        }
        value
    }

    fn print(&self) {
        let global = "follows the global setting";
        let jvm = self.jvm;
        outln!(
            "java:       {}",
            self.java
                .map_or_else(|| global.to_owned(), |java| java.path.display().to_string())
        );
        outln!(
            "min memory: {}",
            jvm.and_then(|jvm| jvm.min_memory_mib)
                .map_or_else(|| global.to_owned(), |mib| format!("{mib} MiB"))
        );
        outln!(
            "max memory: {}",
            jvm.and_then(|jvm| jvm.max_memory_mib)
                .map_or_else(|| global.to_owned(), |mib| format!("{mib} MiB"))
        );
        let args = jvm.map(|jvm| jvm.jvm_args.as_slice()).unwrap_or_default();
        outln!(
            "jvm args:   {}",
            if args.is_empty() {
                global.to_owned()
            } else {
                args.join(" ")
            }
        );
        if let Some(status) = self.status {
            print_java_status(status);
        }
    }
}

/// The Java a host would run with, what it expects, and whether the two fit.
fn print_java_status(status: &HostJavaStatus) {
    if let Some(runtime) = status.runtime() {
        outln!(
            "runtime:    {} ({})",
            runtime_summary_text(runtime),
            selection_text(status.mode, runtime.managed)
        );
        outln!("            {}", runtime.path.display());
    } else if let JavaCheck::Failed(error) = &status.check {
        outln!("runtime:    not checked ({error})");
    } else if let JavaCheck::DownloadsAtLaunch { required_major } = &status.check {
        outln!("runtime:    managed java {required_major}, downloaded at the next launch");
    } else if let Some(path) = &status.pinned_path {
        outln!(
            "runtime:    none usable ({}): {}",
            selection_text(status.mode, false),
            path.display()
        );
    } else {
        outln!("runtime:    none usable");
    }
    outln!("expected:   {}", expected_text(&status.expected));
    outln!("status:     {}", status.tone().as_str());
    for finding in status
        .findings
        .iter()
        .filter(|finding| finding.class() == FindingClass::Problem)
    {
        outln!("problem:    {}", java_problem_text(&finding.kind));
    }
    if let Some(offer) = status.install {
        outln!(
            "install:    ephervex java install --{} {} (java {}{})",
            host_command(status.host),
            status.host.id(),
            offer.major,
            offer
                .maximum_update
                .map_or_else(String::new, |update| format!(", update {update} or older"))
        );
    }
}

/// "java 21.0.8 (Eclipse Adoptium, amd64)": the values the runtime reported.
/// A vendor or architecture the runtime did not report reads as such.
fn runtime_summary_text(runtime: &JavaRuntimeSummary) -> String {
    let reported = |value: &str, missing: &'static str| {
        if value.trim().is_empty() {
            missing.to_owned()
        } else {
            value.to_owned()
        }
    };
    format!(
        "java {} ({}, {})",
        runtime.version,
        reported(&runtime.vendor, "vendor not reported"),
        reported(&runtime.arch, "architecture not reported")
    )
}

/// How a runtime was chosen, and whether Ephervex manages it.
fn selection_text(mode: JavaSelectionMode, managed: bool) -> String {
    let chosen = match mode {
        JavaSelectionMode::Automatic => "chosen automatically",
        JavaSelectionMode::PinnedByHost => "pinned by this host",
        JavaSelectionMode::PinnedByLauncher => "pinned by the launcher preference",
    };
    if managed {
        format!("{chosen}, managed by Ephervex")
    } else {
        chosen.to_owned()
    }
}

/// "java 17", or "java 8, update 320 or older (Forge)".
fn expected_text(expected: &ExpectedRuntime) -> String {
    let major = expected.major();
    let Some(loader) = expected.bounded_by() else {
        return format!("java {major}");
    };
    match (expected.java.minimum, expected.java.maximum) {
        (Some(minimum), Some(maximum)) => format!(
            "java {major}, update {} to {} ({loader})",
            minimum.update, maximum.update
        ),
        (None, Some(maximum)) => {
            format!(
                "java {major}, update {} or older ({loader})",
                maximum.update
            )
        }
        (Some(minimum), None) => {
            format!(
                "java {major}, update {} or newer ({loader})",
                minimum.update
            )
        }
        (None, None) => format!("java {major}"),
    }
}

/// A Java problem a host's status reports, in one line.
fn java_problem_text(kind: &FindingKind) -> String {
    match kind {
        FindingKind::JavaIncompatible { mismatch, .. } => mismatch.to_string(),
        FindingKind::JavaUnusable { path, .. } => {
            format!("{} is not a Java runtime that can be run", path.display())
        }
        FindingKind::JavaUnavailable {
            reason: JavaUnavailable::DetectionDisabled,
            ..
        } => "Java detection is off and nothing is pinned".to_owned(),
        FindingKind::JavaUnavailable {
            reason: JavaUnavailable::NothingCompatible,
            ..
        } => "no compatible runtime is installed and automatic download is off".to_owned(),
        _ => String::new(),
    }
}

/// What a refused runtime was, what the host expected, and how they differ.
fn java_refusal_lines(refusal: &JavaRefusal) -> Vec<String> {
    let chosen = match refusal.chosen {
        RefusedJavaChoice::GivenForRun => "given on the command line".to_owned(),
        RefusedJavaChoice::Selected(mode) => selection_text(
            mode,
            refusal
                .attempted
                .as_ref()
                .is_some_and(|runtime| runtime.managed),
        ),
    };
    let mut lines = Vec::new();
    match &refusal.attempted {
        Some(runtime) => {
            lines.push(format!(
                "tried:    {} ({chosen})",
                runtime_summary_text(runtime)
            ));
            lines.push(format!("          {}", runtime.path.display()));
        }
        None => lines.push(format!("tried:    no usable runtime ({chosen})")),
    }
    lines.push(format!("expected: {}", expected_text(&refusal.expected)));
    if let Some(mismatch) = &refusal.mismatch {
        lines.push(format!("problem:  {mismatch}"));
    }
    lines
}

fn java_refusal_json(refusal: &JavaRefusal) -> serde_json::Value {
    serde_json::json!({
        "mode": refusal.chosen.as_str(),
        "attempted": refusal.attempted.as_ref().map(runtime_summary_json),
        "expected": expected_json(&refusal.expected),
        "mismatch": refusal.mismatch.as_ref().map(java_mismatch_json),
    })
}

/// How a game or server failed because its runtime could not load its code.
fn java_failure_lines(failure: &JavaRuntimeFailure) -> Vec<String> {
    let mut lines = vec![match failure.mismatch.runtime_major {
        Some(runtime) => format!(
            "java: {} needs java {}, and the runtime only supports up to java {runtime}",
            failure.mismatch.class, failure.mismatch.class_major
        ),
        None => format!(
            "java: {} needs java {}, newer than the runtime",
            failure.mismatch.class, failure.mismatch.class_major
        ),
    }];
    if let Some(runtime) = &failure.attempted {
        lines.push(format!(
            "tried:    {} at {}",
            runtime_summary_text(runtime),
            runtime.path.display()
        ));
    }
    lines.push(format!("expected: {}", expected_text(&failure.expected)));
    lines
}

fn java_failure_json(failure: &JavaRuntimeFailure) -> serde_json::Value {
    serde_json::json!({
        "class": failure.mismatch.class,
        "class_major": failure.mismatch.class_major,
        "runtime_major": failure.mismatch.runtime_major,
        "attempted": failure.attempted.as_ref().map(runtime_summary_json),
        "expected": expected_json(&failure.expected),
    })
}

fn java_status_json(status: &HostJavaStatus) -> serde_json::Value {
    serde_json::json!({
        "status": status.tone().as_str(),
        "mode": status.mode.as_str(),
        "pinned_path": status.pinned_path.as_deref().map(json_path),
        "runtime": status.runtime().map(runtime_summary_json),
        "downloads_at_launch": matches!(status.check, JavaCheck::DownloadsAtLaunch { .. }),
        "check_error": match &status.check {
            JavaCheck::Failed(error) => Some(error.to_string()),
            _ => None,
        },
        "expected": expected_json(&status.expected),
        "problems": status
            .findings
            .iter()
            .filter(|finding| finding.class() == FindingClass::Problem)
            .map(|finding| java_finding_json(&finding.kind))
            .collect::<Vec<_>>(),
        "install": status.install.map(|offer| serde_json::json!({
            "major": offer.major,
            "maximum_update": offer.maximum_update,
        })),
    })
}

fn runtime_summary_json(runtime: &JavaRuntimeSummary) -> serde_json::Value {
    serde_json::json!({
        "path": json_path(&runtime.path),
        "major": runtime.major,
        "version": runtime.version,
        "vendor": runtime.vendor,
        "arch": runtime.arch,
        "managed": runtime.managed,
        "source": runtime.source.as_str(),
    })
}

fn expected_json(expected: &ExpectedRuntime) -> serde_json::Value {
    serde_json::json!({
        "major": expected.major(),
        "minimum": expected.java.minimum.map(|version| version.to_string()),
        "maximum": expected.java.maximum.map(|version| version.to_string()),
        "bounded_by": expected.bounded_by(),
    })
}

fn java_mismatch_json(mismatch: &JavaMismatch) -> serde_json::Value {
    match mismatch {
        JavaMismatch::Major { required, found } => serde_json::json!({
            "kind": mismatch.as_str(), "required": required, "found": found,
        }),
        JavaMismatch::TooOld { minimum, found } => serde_json::json!({
            "kind": mismatch.as_str(), "minimum": minimum, "found": found,
        }),
        JavaMismatch::TooNew { maximum, found } => serde_json::json!({
            "kind": mismatch.as_str(), "maximum": maximum, "found": found,
        }),
        JavaMismatch::Architecture { expected, found } => serde_json::json!({
            "kind": mismatch.as_str(), "expected": expected, "found": found,
        }),
    }
}

fn java_finding_json(kind: &FindingKind) -> serde_json::Value {
    match kind {
        FindingKind::JavaIncompatible {
            layer,
            path,
            mismatch,
            ..
        } => serde_json::json!({
            "kind": "incompatible",
            "layer": layer.as_str(),
            "path": json_path(path),
            "mismatch": java_mismatch_json(mismatch),
        }),
        FindingKind::JavaUnusable { layer, path } => serde_json::json!({
            "kind": "unusable",
            "layer": layer.as_str(),
            "path": json_path(path),
        }),
        FindingKind::JavaUnavailable { reason, .. } => serde_json::json!({
            "kind": "unavailable",
            "reason": reason.as_str(),
        }),
        _ => serde_json::Value::Null,
    }
}

fn runtime_pins_json(pins: &JavaRuntimePins) -> serde_json::Value {
    serde_json::json!({
        "hosts": pins
            .hosts
            .iter()
            .map(|pin| serde_json::json!({
                "kind": pin.host.kind(),
                "id": pin.host.id(),
                "name": pin.name,
            }))
            .collect::<Vec<_>>(),
        "launcher": pins.launcher,
    })
}

fn client_commit_warnings_json(warnings: &[ClientCommitWarning]) -> Vec<serde_json::Value> {
    warnings
        .iter()
        .map(|warning| match warning {
            ClientCommitWarning::IndexOutOfSync { detail } => serde_json::json!({
                "kind": "index_out_of_sync",
                "detail": detail,
            }),
            ClientCommitWarning::MetadataNotDurable { detail } => serde_json::json!({
                "kind": "metadata_not_durable",
                "detail": detail,
            }),
        })
        .collect()
}

fn print_server_commit_warnings(warnings: &[ServerCommitWarning]) {
    for warning in warnings {
        eprintln!(
            "{}",
            ephervex_core::server::server_commit_warning_message(warning)
        );
    }
}

fn print_client_commit_warnings(warnings: &[ClientCommitWarning]) {
    for warning in warnings {
        eprintln!(
            "{}",
            ephervex_core::instance::client_commit_warning_message(warning)
        );
    }
}

fn print_folder(path: Option<&Path>, folder: &str, json: bool) -> anyhow::Result<()> {
    if let Some(path) = path {
        if json {
            print_json(&serde_json::json!({ "folder": folder, "path": json_path(path) }));
        } else {
            outln!("{}", path.display());
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

/// Reports the symlinked folders a duplication did not follow. `--json` output
/// carries them in the `skipped_links` field instead.
fn report_skipped_links(skipped: &[PathBuf]) {
    if skipped.is_empty() {
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
        outln!("id:        {}", record.id);
        outln!("name:      {}", record.name);
        outln!("storage:   {}", record.storage);
        outln!("minecraft: {}", record.minecraft_version);
        outln!("loader:    {}", record.loader);
        outln!("root:      {}", record.root.display());
        outln!("installed: {}", record.installation.is_installed());
        if record.source.kind == ephervex_model::ImportSourceKind::ServerPack {
            match record.source.reference.as_deref() {
                Some(archive) => outln!("content:   server files from {archive}"),
                None => outln!("content:   server files from an archive"),
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

fn print_server_event(event: ServerEvent, json: bool, progress: &mut LaunchProgressPrinter) {
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
        ServerEvent::Downloading(operation) => progress.print_progress(&operation),
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
        ServerEvent::OutputDropped { lines } => {
            if json {
                print_json(&serde_json::json!({ "event": "output_dropped", "lines": lines }));
            } else {
                eprintln!(
                    "{}",
                    ephervex_i18n::lookup_args(
                        "server-output-dropped",
                        [(
                            "lines",
                            MessageArg::from(i64::try_from(lines).unwrap_or(i64::MAX))
                        )],
                    )
                );
            }
        }
        ServerEvent::Warning(warning) => print_server_warning(&warning, json),
        ServerEvent::CapturePruned(prune) => {
            let retention = prune.retention;
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

async fn run_server_foreground(
    app: &App,
    id: ServerId,
    java: Option<PathBuf>,
    json: bool,
) -> anyhow::Result<()> {
    let (observer, mut events) = ServerObserver::channel();
    let printer = tokio::spawn(async move {
        let mut progress = LaunchProgressPrinter::default();
        while let Some(event) = events.recv().await {
            print_server_event(event, json, &mut progress);
        }
    });

    let cancel = CancellationToken::new();
    // Output the reader no longer takes stops the server like a termination
    // request does; exiting on the failed write would orphan it.
    watch_stdout(&cancel);
    let mut requests = TerminationRequests::watch()
        .map_err(|error| anyhow::anyhow!("could not watch for termination requests: {error}"))?;
    let outcome = supervise_foreground(
        Box::pin(app.start_server_until_canceled(id, java.clone(), &observer, &cancel)),
        &cancel,
        &mut requests,
        || async {
            if let Err(error) = app.kill_server(id).await {
                note(&format!("the server could not be killed: {error}"));
            }
        },
    )
    .await;
    // The printer ends when every sender is gone, so this drop is what lets it
    // finish the last lines and the exited event rather than hang.
    drop(observer);
    let _ = printer.await;
    let exit = match outcome {
        Ok(exit) => exit,
        Err(error) if error.is_cancellation() => {
            note(&localized_failure(&error).0);
            return stdout_result();
        }
        Err(error) => return Err(error.into()),
    };
    // The explanation is extra: failing to read it never hides how the
    // server ended.
    let failure = match app.server_java_failure(id, &exit, java.as_deref()).await {
        Ok(failure) => failure,
        Err(error) => {
            tracing::debug!(%error, "could not read the server's output for a Java failure");
            None
        }
    };
    if json {
        if let Some(failure) = &failure {
            print_json(&serde_json::json!({
                "event": "java_failure",
                "java_failure": java_failure_json(failure),
            }));
        }
    } else {
        note(&server_exit_description(&exit));
        for line in failure.iter().flat_map(java_failure_lines) {
            note(&line);
        }
    }
    server_exit_result(&exit).and(stdout_result())
}

/// The one-line account of how a server ended, for a person rather than a script.
fn server_exit_description(exit: &ServerExit) -> String {
    match (exit.code, exit.signal) {
        (Some(code), _) => format!("server exited with code {code}"),
        (_, Some(signal)) => format!("server exited from signal {signal}"),
        _ => "server exited".to_owned(),
    }
}

/// Asks a question on standard error and returns the line typed in answer.
///
/// Standard output carries data only, so a question there would land in a file
/// or pipe the user redirected it to while the command waits on an answer no
/// one sees. The caller has already checked that standard input is a terminal.
fn ask(question: &str) -> anyhow::Result<String> {
    Ok(ask_on(
        &mut std::io::stderr(),
        &mut std::io::stdin().lock(),
        question,
    )?)
}

fn ask_on(
    question_sink: &mut impl std::io::Write,
    answers: &mut impl std::io::BufRead,
    question: &str,
) -> std::io::Result<String> {
    question_sink.write_all(question.as_bytes())?;
    question_sink.flush()?;
    let mut answer = String::new();
    answers.read_line(&mut answer)?;
    Ok(answer)
}

/// Tells the user something while a child may still be running.
///
/// `eprintln!` panics when the write fails, and after a terminal hangup it
/// does: the panic would unwind past the supervisor and kill the child it was
/// stopping.
fn note(message: &str) {
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// Drives a foreground command that supervises a child until the child ends.
///
/// The first stop request, or a canceled `cancel` from elsewhere such as a
/// closed standard output, asks the child to stop; `start` keeps being polled
/// because the supervisor inside it is what answers. A further request calls
/// `kill`. Ending the command any other way would drop `start` and orphan the
/// child.
async fn supervise_foreground<T, K, KF>(
    start: impl std::future::Future<Output = T>,
    cancel: &CancellationToken,
    requests: &mut impl StopRequests,
    kill: K,
) -> T
where
    K: FnOnce() -> KF,
    KF: std::future::Future<Output = ()>,
{
    tokio::pin!(start);
    let mut kill = Some(kill);
    let mut announced = false;
    loop {
        tokio::select! {
            result = &mut start => return result,
            () = requests.next() => {
                if !cancel.is_cancelled() {
                    cancel.cancel();
                } else if let Some(kill) = kill.take() {
                    kill().await;
                }
            }
            () = cancel.cancelled(), if !announced => {
                announced = true;
                note("stopping the server... press Ctrl-C again to kill it");
            }
        }
    }
}

/// Whether a foreground server's end means the command did its job.
///
/// A stop asked for through the control channel or Ctrl-C always does. Without
/// one, only a server that finished starting and then exited with status 0
/// does: a server that dies while starting can still exit with 0.
fn server_exit_result(exit: &ServerExit) -> anyhow::Result<()> {
    if exit.stop_requested {
        return Ok(());
    }
    if !exit.finished_starting {
        anyhow::bail!("the server exited before it finished starting");
    }
    if exit.code != Some(0) {
        anyhow::bail!("the server exited without being asked to stop");
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
        outln!("persistence: {persistence}");
        outln!("runtime:     {runtime}");
        outln!("capture:     {}", capture_health_name(&capture.health));
        if !status.properties_awaiting_restart.is_empty() {
            outln!(
                "restart:     required for {}, saved while running",
                status.properties_awaiting_restart.join(", ")
            );
        }
        if let RuntimeClassification::Orphaned { child } = &status.runtime {
            outln!("orphan pid:  {}", child.pid);
            outln!("executable:  {}", child.executable.display());
            outln!("action:      terminate this process with explicit operating-system tools");
        }
    }
    Ok(())
}

fn server_status_json(app: &App, id: ServerId) -> anyhow::Result<serde_json::Value> {
    let status = app.server_status(id)?;
    let capture = app.server_output(id, 0)?;
    let mut value = server_status_json_from_parts(&status, &capture);
    let view = app.organization_or_empty();
    if let Some(server) = value
        .get_mut("server")
        .and_then(|server| server.as_object_mut())
    {
        server.insert("favorite".to_owned(), view.is_favorite_server(id).into());
        server.insert(
            "icon".to_owned(),
            view.server_icon(id)
                .map_or(serde_json::Value::Null, |path| {
                    serde_json::Value::String(json_path(&path))
                }),
        );
    }
    Ok(value)
}

/// A server's record, with whether it is a favorite when it is.
fn print_server_with_organization(app: &App, id: ServerId, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&server_status_json(app, id)?);
    } else {
        print_server(&app.server(id)?, false);
        if app.organization_or_empty().is_favorite_server(id) {
            outln!("favorite:  yes");
        }
    }
    Ok(())
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
        "properties_awaiting_restart": status.properties_awaiting_restart,
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

fn server_output_json(
    id: ServerId,
    snapshot: &ephervex_core::CaptureSnapshot,
) -> serde_json::Value {
    let detail = match &snapshot.health {
        CaptureHealth::Available => None,
        CaptureHealth::Unavailable { detail } => Some(detail.as_str()),
    };
    serde_json::json!({
        "server_id": id,
        "capture_health": capture_health_name(&snapshot.health),
        "capture_unavailable_detail": detail,
        "retention": {
            "removed_segments": snapshot.retention.removed_segments,
            "removed_bytes": snapshot.retention.removed_bytes,
            "removed_lines": snapshot.retention.removed_lines,
            "earliest_retained_at": snapshot.retention.earliest_retained_at,
        },
        "pruned_while_reading": snapshot.pruned_while_reading,
        "lines": snapshot.lines.iter().map(captured_line_json).collect::<Vec<_>>(),
    })
}

/// Says why the lines shown are not the whole history.
///
/// On stderr so stdout stays the captured lines, like `report_window`.
fn report_capture_gaps(snapshot: &ephervex_core::CaptureSnapshot) {
    for notice in capture_gap_notices(snapshot) {
        eprintln!("{notice}");
    }
}

fn capture_gap_notices(snapshot: &ephervex_core::CaptureSnapshot) -> Vec<String> {
    let mut notices = Vec::new();
    if let CaptureHealth::Unavailable { detail } = &snapshot.health {
        notices.push(ephervex_i18n::lookup_args(
            "server-output-capture-unavailable",
            [("detail", MessageArg::from(detail.clone()))],
        ));
    }
    let retention = &snapshot.retention;
    if retention.removed_lines > 0 || retention.removed_segments > 0 {
        let lines = MessageArg::from(i64::try_from(retention.removed_lines).unwrap_or(i64::MAX));
        notices.push(match retention.earliest_retained_at {
            Some(since) => ephervex_i18n::lookup_args(
                "server-output-pruned-since",
                [
                    ("lines", lines),
                    ("since", MessageArg::from(since.to_string())),
                ],
            ),
            None => ephervex_i18n::lookup_args("server-output-pruned", [("lines", lines)]),
        });
    }
    if snapshot.pruned_while_reading {
        notices.push(ephervex_i18n::lookup("server-output-pruned-while-reading"));
    }
    notices
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
                outln!("not accepted ({MINECRAFT_EULA_REFERENCE})");
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
                outln!("accepted at {accepted_at} ({reference})");
            }
        }
        EulaStatus::Malformed => {
            if json {
                print_json(&serde_json::json!({ "status": "malformed" }));
            } else {
                outln!("malformed; revoke or accept again interactively");
            }
        }
    }
}

/// Whether the supervisor performed the requested control action.
///
/// A refusal means nothing was done, and a failed restart leaves the server
/// stopped, so neither may look like success to a script.
fn control_response_result(response: &ControlResponse) -> anyhow::Result<()> {
    match response {
        ControlResponse::Refused(detail) => {
            anyhow::bail!("the server refused the request: {detail}")
        }
        ControlResponse::StartFailed(detail) => {
            anyhow::bail!("the server stopped but could not be started again: {detail}")
        }
        ControlResponse::Running
        | ControlResponse::Accepted
        | ControlResponse::Exited(_)
        | ControlResponse::Restarted { .. } => Ok(()),
    }
}

fn print_control_response(response: &ControlResponse, json: bool) {
    if json {
        print_json(
            &serde_json::to_value(response)
                .unwrap_or_else(|_| serde_json::json!({ "status": "invalid_response" })),
        );
    } else {
        match response {
            ControlResponse::Running => outln!("running"),
            ControlResponse::Accepted => outln!("accepted"),
            ControlResponse::Exited(exit) => outln!("exited: {exit:?}"),
            ControlResponse::Restarted { pid } => outln!("restarted as pid {pid}"),
            ControlResponse::StartFailed(detail) => {
                outln!("stopped; starting it again failed: {detail}");
            }
            ControlResponse::Refused(detail) => outln!("refused: {detail}"),
        }
    }
}

fn print_server_commit(record: &ServerRecord, warnings: &[ServerCommitWarning], json: bool) {
    if json {
        let mut value = server_json(record);
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "warnings".to_owned(),
                serde_json::Value::Array(server_commit_warnings_json(warnings)),
            );
        }
        print_json(&value);
        return;
    }
    print_server(record, false);
    print_server_commit_warnings(warnings);
}

fn server_commit_warnings_json(warnings: &[ServerCommitWarning]) -> Vec<serde_json::Value> {
    warnings
        .iter()
        .map(|warning| match warning {
            ServerCommitWarning::IndexOutOfSync { detail } => serde_json::json!({
                "kind": "index_out_of_sync",
                "detail": detail,
            }),
            ServerCommitWarning::MetadataNotDurable { detail } => serde_json::json!({
                "kind": "metadata_not_durable",
                "detail": detail,
            }),
        })
        .collect()
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
        outln!(
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
        outln!("{} {}", line.timestamp, line.text);
    }
}

fn instance_json(app: &App, record: &ClientRecord) -> anyhow::Result<serde_json::Value> {
    instance_json_with(app, record, &Organized::load_lenient(app))
}

/// `icon` is the stored image file, or null when there is none or it is gone.
fn instance_json_with(
    app: &App,
    record: &ClientRecord,
    organized: &Organized,
) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::json!({
            "favorite": organized.view.is_favorite_client(record.id),
            "linking": organized
                .linking
                .get(&record.id)
                .copied()
                .unwrap_or(LinkingState::None)
                .as_str(),
            "group": organized.view.group_of(record.id).map(|group| serde_json::json!({
                "id": group.id.to_string(),
                "name": group.name,
            })),
            "icon": organized.view.client_icon(record.id).map(|path| json_path(&path)),
            "id": record.id.to_string(),
            "name": record.name,
            "storage": record.storage.to_string(),
            "minecraft_version": record.minecraft_version,
            "loader": {
                "kind": record.loader.kind.to_string(),
                "version": record.loader.version,
            },
            "root": json_path(&record.root),
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
        "root": json_path(&record.root),
        "linked_client_id": record.linked_client_id.map(|id| id.to_string()),
        "network": record.network,
        "installation": record.installation,
        "source": record.source,
        "java_override": record.java_override.as_ref().map(|java| serde_json::json!({ "path": json_path(&java.path) })),
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
        outln!("{}", ephervex_i18n::lookup("running-nothing"));
        return Ok(());
    }
    for process in &running {
        let pid = process
            .pid
            .map_or_else(|| "-".to_owned(), |pid| pid.to_string());
        outln!(
            "{}  {}  pid {pid}  {}",
            process.host.kind(),
            process.name,
            format_uptime(process.uptime_seconds())
        );
        if let Some(line) = &process.last_line {
            outln!("    {line}");
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

fn conflicts_json<'a>(
    conflicts: impl Iterator<Item = (String, &'a Vec<PathBuf>)>,
) -> serde_json::Value {
    serde_json::Value::Array(
        conflicts
            .map(|(id, roots)| serde_json::json!({ "id": id, "roots": json_paths(roots) }))
            .collect(),
    )
}

fn reconciled_json(records: &[ephervex_core::ReconciledRecord]) -> serde_json::Value {
    serde_json::Value::Array(
        records
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "id": entry.reference.to_string(),
                    "name": entry.name,
                    "root": json_path(&entry.root),
                    "storage": entry.storage.to_string(),
                })
            })
            .collect(),
    )
}

/// A path as a JSON string.
///
/// JSON strings are UTF-8 and a path need not be, so bytes that are not valid
/// UTF-8 are written as U+FFFD. The text names the entry for a person or a
/// script but is not guaranteed to reopen it.
fn json_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A list of paths as JSON strings, each encoded by [`json_path`].
fn json_paths<P: AsRef<Path>>(paths: &[P]) -> serde_json::Value {
    serde_json::Value::Array(
        paths
            .iter()
            .map(|path| serde_json::Value::String(json_path(path.as_ref())))
            .collect(),
    )
}

/// Prints a `--json` result.
///
/// Indented on purpose: command output is read in a terminal, so readability
/// wins here, unlike the launcher's own stored JSON, which stays compact.
fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        // Through `write_out`, because a machine-readable listing is exactly
        // what gets piped into something that stops reading early.
        Ok(text) => write_out(format!("{text}\n").as_bytes()),
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

    let refusal = error
        .downcast_ref::<CoreError>()
        .and_then(CoreError::java_refusal);
    if json {
        let value = serde_json::json!({
            "message_id": message_id,
            "error": summary,
            "action": action,
            "detail": technical,
            "java": refusal.map(java_refusal_json),
        });
        if let Ok(text) = serde_json::to_string_pretty(&value) {
            eprintln!("{text}");
        }
        return;
    }

    // Errors can quote archive-supplied paths and URLs, so every line is made
    // terminal-safe. The JSON form above keeps the exact text.
    eprintln!("error: {}", terminal_safe(&summary));
    if let Some(action) = action {
        eprintln!("       {}", terminal_safe(&action));
    }
    if let Some(refusal) = refusal {
        for line in java_refusal_lines(refusal) {
            eprintln!("  {}", terminal_safe(&line));
        }
    }
    // A failure that is not a `CoreError` has no localized message, so the
    // summary *is* its first technical line, and a technical line can also be
    // the localized sentence in its own quoting and capitalization. Printing
    // either would say the same sentence twice.
    let restated = sentence_words(&summary);
    for line in technical
        .iter()
        .filter(|line| sentence_words(line) != restated)
    {
        eprintln!("  {}", terminal_safe(line));
    }
}

/// A sentence's words, ignoring case, quoting, and final punctuation, so two
/// renderings of one sentence compare equal while any differing word does not.
fn sentence_words(text: &str) -> Vec<String> {
    text.trim_end_matches(['.', '!'])
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|character| !matches!(character, '`' | '"'))
                .flat_map(char::to_lowercase)
                .collect()
        })
        .collect()
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
    args.extend(error.message_args());

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

    #[test]
    fn a_runtime_listed_without_its_pins_reports_them_as_unknown_not_as_none() {
        let runtime = JavaRuntime {
            executable: PathBuf::from("/opt/jdk-21/bin/java"),
            home: None,
            version: ephervex_core::JavaVersion::parse("21.0.8").expect("a version"),
            version_string: "21.0.8".to_owned(),
            runtime_version: "21.0.8+9".to_owned(),
            vendor: "Eclipse Adoptium".to_owned(),
            arch: "amd64".to_owned(),
            origins: Vec::new(),
        };
        assert_eq!(
            runtime_pinned_by_json(&runtime, None),
            serde_json::Value::Null
        );
        assert_eq!(
            runtime_pinned_by_json(&runtime, Some(&JavaPinIndex::default())),
            serde_json::json!({"hosts": [], "launcher": false})
        );
    }

    #[test]
    fn a_runtime_line_names_a_vendor_or_architecture_the_runtime_did_not_report() {
        let runtime = JavaRuntimeSummary {
            version: "1.6.0_45".to_owned(),
            major: 6,
            path: PathBuf::from("/opt/jdk6/bin/java"),
            managed: false,
            vendor: String::new(),
            arch: String::new(),
            source: ephervex_core::JavaSourceKind::UserSupplied,
        };
        assert_eq!(
            runtime_summary_text(&runtime),
            "java 1.6.0_45 (vendor not reported, architecture not reported)"
        );
    }

    #[test]
    fn an_address_is_recognized_whatever_the_case_of_its_scheme() {
        for address in [
            "http://example.org/p.mrpack",
            "https://example.org/p.mrpack",
            "HTTPS://example.org/p.mrpack",
            "Http://example.org/p.mrpack",
            "  HtTpS://example.org/p.mrpack",
        ] {
            assert!(looks_like_an_address(address), "{address}");
        }
        for path in [
            "pack.mrpack",
            "./https.mrpack",
            "/tmp/https://x",
            "ftp://example.org/p.mrpack",
            "http:/example.org",
            "htt",
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}",
            "",
        ] {
            assert!(!looks_like_an_address(path), "{path}");
        }
    }

    #[test]
    fn a_question_is_written_to_the_question_sink_and_the_typed_line_is_returned() {
        let mut sink = Vec::new();
        let mut typed = std::io::Cursor::new(b"once\nignored\n".to_vec());

        let answer = ask_on(&mut sink, &mut typed, "Download? [y/N/once] ").expect("answer");

        assert_eq!(answer, "once\n");
        assert_eq!(
            String::from_utf8(sink).expect("UTF-8"),
            "Download? [y/N/once] "
        );
    }

    #[test]
    fn a_server_exit_is_described_by_its_code_then_its_signal() {
        let exit = |code, signal| ServerExit {
            code,
            signal,
            finished_starting: true,
            stop_requested: false,
        };
        assert_eq!(
            server_exit_description(&exit(Some(3), Some(9))),
            "server exited with code 3"
        );
        assert_eq!(
            server_exit_description(&exit(None, Some(9))),
            "server exited from signal 9"
        );
        assert_eq!(server_exit_description(&exit(None, None)), "server exited");
    }

    #[test]
    fn an_unverified_optional_dependency_is_not_reported_as_incompatible() {
        let unverified =
            optional_status_json(&OptionalStatus::Unverified(UnverifiedOptional::Unavailable));
        assert_eq!(unverified["compatible"], serde_json::Value::Null);
        assert_eq!(unverified["unverified"], "unavailable");
        assert_eq!(unverified["reasons"], serde_json::json!([]));

        let incompatible = optional_status_json(&OptionalStatus::Incompatible(Vec::new()));
        assert_eq!(incompatible["compatible"], false);
        assert_eq!(incompatible["unverified"], serde_json::Value::Null);

        let compatible = optional_status_json(&OptionalStatus::Compatible);
        assert_eq!(compatible["compatible"], true);
    }

    fn capture_snapshot(
        health: CaptureHealth,
        retention: ephervex_core::CaptureRetention,
        pruned_while_reading: bool,
    ) -> ephervex_core::CaptureSnapshot {
        ephervex_core::CaptureSnapshot {
            lines: Vec::new(),
            health,
            retention,
            pruned_while_reading,
        }
    }

    #[test]
    fn server_output_json_names_capture_health_like_server_status_and_keeps_the_detail_apart() {
        let id = ServerId::new();
        let unavailable = capture_snapshot(
            CaptureHealth::Unavailable {
                detail: "disk full".to_owned(),
            },
            ephervex_core::CaptureRetention::default(),
            false,
        );
        let json = server_output_json(id, &unavailable);
        assert_eq!(json["capture_health"], "unavailable");
        assert_eq!(json["capture_unavailable_detail"], "disk full");

        let available = capture_snapshot(
            CaptureHealth::Available,
            ephervex_core::CaptureRetention::default(),
            false,
        );
        let json = server_output_json(id, &available);
        assert_eq!(json["capture_health"], "available");
        assert_eq!(json["capture_unavailable_detail"], serde_json::Value::Null);
    }

    #[test]
    fn complete_available_capture_has_no_gap_notices() {
        let snapshot = capture_snapshot(
            CaptureHealth::Available,
            ephervex_core::CaptureRetention::default(),
            false,
        );
        assert!(capture_gap_notices(&snapshot).is_empty());
    }

    #[test]
    fn unavailable_capture_is_reported_with_its_detail() {
        let snapshot = capture_snapshot(
            CaptureHealth::Unavailable {
                detail: "disk full".to_owned(),
            },
            ephervex_core::CaptureRetention::default(),
            false,
        );
        let notices = capture_gap_notices(&snapshot);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("unavailable"), "{notices:?}");
        assert!(notices[0].contains("disk full"), "{notices:?}");
    }

    #[test]
    fn pruned_history_reports_the_removed_lines_and_the_earliest_kept_instant() {
        let retention: ephervex_core::CaptureRetention =
            serde_json::from_value(serde_json::json!({
                "removed_segments": 2,
                "removed_bytes": 4096,
                "removed_lines": 120,
                "earliest_retained_at": "2026-10-11T12:00:00Z",
            }))
            .expect("retention");
        let since = retention.earliest_retained_at.expect("earliest instant");
        let snapshot = capture_snapshot(CaptureHealth::Available, retention, true);
        let notices = capture_gap_notices(&snapshot);
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices[0].contains("120"), "{notices:?}");
        assert!(notices[0].contains(&since.to_string()), "{notices:?}");
        assert!(
            notices[1].contains("while it was being read"),
            "{notices:?}"
        );
    }

    #[test]
    fn pruned_history_without_a_retained_line_still_reports_the_removed_lines() {
        let retention = ephervex_core::CaptureRetention {
            removed_segments: 1,
            removed_bytes: 10,
            removed_lines: 1,
            earliest_retained_at: None,
        };
        let snapshot = capture_snapshot(CaptureHealth::Available, retention, false);
        let notices = capture_gap_notices(&snapshot);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].starts_with("1 older line"), "{notices:?}");
    }

    #[test]
    fn server_commit_warnings_print_as_kind_and_detail_entries() {
        let warnings = [
            ServerCommitWarning::IndexOutOfSync {
                detail: "index unavailable".to_owned(),
            },
            ServerCommitWarning::MetadataNotDurable {
                detail: "input/output error".to_owned(),
            },
        ];

        assert_eq!(
            server_commit_warnings_json(&warnings),
            vec![
                serde_json::json!({"kind": "index_out_of_sync", "detail": "index unavailable"}),
                serde_json::json!({"kind": "metadata_not_durable", "detail": "input/output error"}),
            ]
        );
    }

    #[test]
    fn a_launch_shows_a_linked_servers_warnings_and_nothing_else_it_emits() {
        let warnings = [
            ServerWarning::OfflineModeExposed,
            ServerWarning::PortUnavailable {
                address: "0.0.0.0".to_owned(),
                port: 25565,
                detail: "address in use".to_owned(),
            },
        ];
        for warning in warnings {
            assert_eq!(
                launch_server_warning(ServerEvent::Warning(warning.clone())),
                Some(warning)
            );
        }
        assert_eq!(launch_server_warning(ServerEvent::Started { pid: 7 }), None);
        assert_eq!(launch_server_warning(ServerEvent::Restarting), None);
    }

    #[test]
    fn an_unverified_optional_release_with_no_known_project_names_the_release() {
        let candidate = OptionalCandidate {
            project_id: None,
            title: None,
            pinned_version_id: Some("extra-v1".to_owned()),
            declared_by: "ROOT".to_owned(),
            version: None,
            status: OptionalStatus::Unverified(UnverifiedOptional::Unavailable),
        };
        let text = optional_unverified_text(&candidate).expect("an unverified offer has a note");
        assert!(text.contains("extra-v1"), "{text}");
    }

    fn settings_with(key: ephervex_core::CurseForgeKeyState) -> ApiSettings {
        ApiSettings {
            microsoft_client_id: String::new(),
            effective_microsoft_client_id: "client".to_owned(),
            microsoft_client_id_is_custom: false,
            user_agent: String::new(),
            effective_user_agent: "agent".to_owned(),
            curseforge_key: key,
            curseforge_default_key_available: false,
            credential_storage: Some(CredentialStorage::Keyring),
        }
    }

    #[test]
    fn a_key_change_under_json_reports_the_resulting_settings() {
        let output = curseforge_key_change_output(true, "stored", || {
            Ok(settings_with(ephervex_core::CurseForgeKeyState::Configured))
        })
        .expect("output");

        let document: serde_json::Value = serde_json::from_str(&output).expect("JSON output");
        assert_eq!(document["curseforge_api_key"], "configured");
        assert_eq!(document["credential_storage"], "keyring");
        assert!(!output.contains("stored"), "no sentence under --json");
    }

    #[test]
    fn a_key_change_without_json_prints_one_sentence_and_reads_no_settings() {
        let output = curseforge_key_change_output(false, "removed the key", || {
            panic!("the settings are not needed for a sentence")
        })
        .expect("output");

        assert_eq!(output, "removed the key\n");
    }

    #[test]
    fn an_unanswered_entitlement_lookup_is_reported_as_unknown_ownership() {
        let unavailable = EntitlementLookup::Unavailable;
        assert!(ownership_json(Some(&unavailable), Entitlements::owns_game).is_null());
        assert!(ownership_json(Some(&unavailable), Entitlements::has_game_pass).is_null());

        let none = EntitlementLookup::Answered(Entitlements::default());
        assert_eq!(
            ownership_json(Some(&none), Entitlements::owns_game),
            serde_json::Value::Bool(false)
        );
        let bought = EntitlementLookup::Answered(Entitlements {
            items: vec!["product_minecraft".to_owned()],
        });
        assert_eq!(
            ownership_json(Some(&bought), Entitlements::owns_game),
            serde_json::Value::Bool(true)
        );
        assert_eq!(
            ownership_json(None, Entitlements::owns_game),
            serde_json::Value::Bool(false)
        );
    }

    #[test]
    fn a_server_listing_marks_a_mod_its_catalog_does_not_confirm_runs_there() {
        let server = InstalledHost::Server(ServerId::new());
        let client = InstalledHost::Instance(InstanceId::new());
        assert_eq!(
            server_support_note(server, SideSupport::Unsupported),
            Some(ephervex_i18n::lookup("mod-search-client-only"))
        );
        assert_eq!(
            server_support_note(server, SideSupport::Unknown),
            Some(ephervex_i18n::lookup("mod-search-server-support-unstated"))
        );
        assert_eq!(server_support_note(server, SideSupport::Required), None);
        assert_eq!(server_support_note(client, SideSupport::Unsupported), None);
    }

    #[test]
    fn a_search_page_says_where_the_next_one_starts() {
        let page = ModSearchResults {
            provider: ModProvider::CurseForge,
            hits: Vec::new(),
            total_hits: 40,
            offset: 0,
            next_offset: Some(3),
        };
        assert_eq!(search_json(&page)["next_offset"], 3);
        let last = ModSearchResults {
            next_offset: None,
            ..page
        };
        assert!(search_json(&last)["next_offset"].is_null());
    }

    #[test]
    fn an_unconfirmed_server_install_says_its_next_step_on_a_line_of_its_own() {
        let refusal = unconfirmed_server_support(&["EMI".to_owned()]);
        let lines: Vec<String> = refusal.chain().map(ToString::to_string).collect();

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("EMI"), "{lines:?}");
        assert!(
            lines[1].contains("--accept-unknown-server-support"),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|line| !line.contains('\n')),
            "no line carries a newline the terminal would escape: {lines:?}"
        );
    }

    #[test]
    fn terminal_safe_escapes_control_and_bidirectional_characters_only() {
        assert_eq!(
            terminal_safe("Plain Pack 1.2 \u{e9}\u{4e2d}"),
            "Plain Pack 1.2 \u{e9}\u{4e2d}"
        );
        assert!(matches!(
            terminal_safe("plain"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(
            terminal_safe("a\u{1b}[2J\nb\u{7f}"),
            "a\\u{1b}[2J\\u{a}b\\u{7f}"
        );
        assert_eq!(terminal_safe("x\u{9b}y\u{202e}z"), "x\\u{9b}y\\u{202e}z");
    }

    fn planned_mod(project_id: &str) -> PlannedMod {
        use ephervex_core::{ModFile, ModProject, ModVersionInfo, ReleaseChannel, SideSupport};
        use ephervex_mod_platform::Distribution;
        PlannedMod {
            project: ModProject {
                provider: ModProvider::Modrinth,
                id: project_id.to_owned(),
                slug: None,
                title: "Example".to_owned(),
                description: String::new(),
                author: None,
                client_side: SideSupport::Unsupported,
                server_side: SideSupport::Required,
                icon_url: None,
            },
            version: ModVersionInfo {
                provider: ModProvider::Modrinth,
                id: "version".to_owned(),
                project_id: project_id.to_owned(),
                name: "Example 1.0".to_owned(),
                version_number: "1.0".to_owned(),
                channel: ReleaseChannel::Release,
                game_versions: Vec::new(),
                loaders: Vec::new(),
                published_at: chrono::DateTime::UNIX_EPOCH,
                file: ModFile {
                    file_name: "example.jar".to_owned(),
                    distribution: Distribution::Restricted,
                    size: None,
                    hashes: Vec::new(),
                },
                dependencies: Vec::new(),
                client_side: SideSupport::Unknown,
                server_side: SideSupport::Unknown,
            },
            origin: ephervex_model::ModOrigin::default(),
            requested_by: Vec::new(),
        }
    }

    #[test]
    fn an_installation_reports_the_same_plan_warnings_as_a_plan() {
        let warnings = vec![
            PlanWarning::ClientSideUnsupported {
                project_id: "AANobbMI".to_owned(),
            },
            PlanWarning::UncheckableConflict {
                declared_by: "AANobbMI".to_owned(),
                file_name: "other.jar".to_owned(),
            },
            PlanWarning::OptionalNotOffered {
                project_id: "P7dR8mSH".to_owned(),
            },
        ];
        let installed = InstalledMods {
            plan: ModInstallPlan {
                provider: ModProvider::Modrinth,
                root: planned_mod("AANobbMI"),
                dependencies: Vec::new(),
                optional: Vec::new(),
                external: Vec::new(),
                embedded: Vec::new(),
                already_installed: Vec::new(),
                replacements: Vec::new(),
                warnings: warnings.clone(),
            },
            installation: ManagedModInstallation::default(),
        };

        let reported = installed_mods_json(&installed);

        let expected: Vec<String> = warnings.iter().map(warning_text).collect();
        assert_eq!(expected.len(), 3);
        assert_eq!(reported["plan"]["warnings"], serde_json::json!(expected));
    }

    /// A plan whose dependencies leave their server side unstated, and whose
    /// root does too when `root_undeclared` says so.
    fn undeclared_plan(root_undeclared: bool, dependencies: &[&str]) -> ModInstallPlan {
        let undeclared = |project_id: &str| {
            let mut planned = planned_mod(project_id);
            planned.project.server_side = ephervex_core::SideSupport::Unknown;
            planned
        };
        ModInstallPlan {
            provider: ModProvider::Modrinth,
            root: if root_undeclared {
                undeclared("ROOT")
            } else {
                planned_mod("ROOT")
            },
            dependencies: dependencies.iter().map(|id| undeclared(id)).collect(),
            optional: Vec::new(),
            external: Vec::new(),
            embedded: Vec::new(),
            already_installed: Vec::new(),
            replacements: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn consent(accept: bool, use_installed: &[&str]) -> InstallConsentArgs {
        InstallConsentArgs {
            accept_unknown_server_support: accept,
            use_installed: use_installed.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    #[test]
    fn a_server_install_leaves_a_dependency_an_installed_jar_may_stand_in_for_to_the_install() {
        let confirmed = server_support_confirmed(
            &undeclared_plan(false, &["LIBX"]),
            &consent(false, &["LIBX"]),
        )
        .expect("the install decides about LIBX");

        assert!(confirmed.is_empty(), "{confirmed:?}");
    }

    #[test]
    fn a_server_install_still_needs_confirmation_for_what_no_jar_may_stand_in_for() {
        assert!(
            server_support_confirmed(
                &undeclared_plan(false, &["LIBX", "LIBY"]),
                &consent(false, &["LIBX"]),
            )
            .is_err()
        );
        assert!(
            server_support_confirmed(&undeclared_plan(true, &[]), &consent(false, &["ROOT"]))
                .is_err(),
            "nothing stands in for the mod asked for"
        );
    }

    #[test]
    fn accepting_unknown_server_support_confirms_dependencies_an_installed_jar_may_stand_in_for() {
        let confirmed = server_support_confirmed(
            &undeclared_plan(false, &["LIBX"]),
            &consent(true, &["LIBX"]),
        )
        .expect("accepted");

        assert_eq!(confirmed, ["LIBX"]);
    }

    #[test]
    fn an_installation_without_plan_warnings_reports_an_empty_list() {
        let installed = InstalledMods {
            plan: ModInstallPlan {
                provider: ModProvider::Modrinth,
                root: planned_mod("AANobbMI"),
                dependencies: Vec::new(),
                optional: Vec::new(),
                external: Vec::new(),
                embedded: Vec::new(),
                already_installed: Vec::new(),
                replacements: Vec::new(),
                warnings: Vec::new(),
            },
            installation: ManagedModInstallation::default(),
        };

        assert_eq!(
            installed_mods_json(&installed)["plan"]["warnings"],
            serde_json::json!([])
        );
    }

    #[test]
    fn a_plan_lists_a_dependency_already_installed_once_and_by_title() {
        use ephervex_mod_platform::InstalledProject;
        let mut library = planned_mod("LIBPROJ");
        library.project.title = "Library".to_owned();
        let mut fresh = planned_mod("NEWPROJ");
        fresh.project.title = "Fresh".to_owned();
        let plan = ModInstallPlan {
            provider: ModProvider::Modrinth,
            root: planned_mod("AANobbMI"),
            dependencies: vec![library, fresh],
            optional: vec![OptionalCandidate {
                project_id: Some("OPTPROJ".to_owned()),
                title: Some("Optional Extra".to_owned()),
                pinned_version_id: None,
                declared_by: "AANobbMI".to_owned(),
                version: None,
                status: OptionalStatus::Compatible,
            }],
            external: Vec::new(),
            embedded: Vec::new(),
            already_installed: vec![InstalledProject {
                project_id: "LIBPROJ".to_owned(),
                version_id: "version".to_owned(),
            }],
            replacements: Vec::new(),
            warnings: Vec::new(),
        };

        let lines = plan_lines(&plan, InstalledHost::Instance(InstanceId::new()));

        let mentioning =
            |id: &str| -> Vec<&String> { lines.iter().filter(|line| line.contains(id)).collect() };
        assert_eq!(
            mentioning("LIBPROJ"),
            ["  = LIBPROJ  Library  (already installed)"],
            "{lines:#?}"
        );
        assert_eq!(
            mentioning("NEWPROJ"),
            ["  + NEWPROJ  Fresh  (1.0)"],
            "{lines:#?}"
        );
        assert!(
            lines.contains(&"  [ ] Optional Extra (optional)".to_owned()),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_technical_line_restating_the_localized_failure_is_recognized_as_the_same_sentence() {
        let error = CoreError::ModClientOnly {
            project: "Sodium".to_owned(),
        };
        let (summary, _) = localized_failure(&error);
        assert_eq!(
            sentence_words(&summary),
            sentence_words(&error.to_string()),
            "{summary:?} and {:?} say the same thing",
            error.to_string()
        );
        assert_ne!(
            sentence_words("Sodium is declared client-only."),
            sentence_words("Sodium is declared client-only: version 0.5.8."),
            "a line carrying more detail is not a restatement"
        );
    }

    #[test]
    fn refusals_count_mods_in_a_sentence() {
        assert_eq!(
            counted(1, "1 mod needs this one.", "mods need this one."),
            "1 mod needs this one."
        );
        assert_eq!(
            counted(3, "1 mod needs this one.", "mods need this one."),
            "3 mods need this one."
        );
    }

    #[test]
    fn a_client_only_search_result_is_reported_incompatible_with_its_reason() {
        let value = compatibility_json(&Compatibility::Incompatible(vec![
            IncompatibilityReason::ClientOnly,
        ]));
        assert_eq!(value["compatible"], false);
        assert_eq!(value["reasons"][0]["reason"], "client_only");
    }

    #[test]
    fn a_reinstall_that_keeps_a_changed_jar_names_it_instead_of_saying_nothing_to_do() {
        let installation = ManagedModInstallation {
            modified: vec![PathBuf::from("mods/local-build.jar")],
            ..ManagedModInstallation::default()
        };

        let lines = installation_lines("Local Build", &installation);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("mods/local-build.jar"), "{lines:?}");
        assert!(lines[0].contains("left as it is"), "{lines:?}");
        assert!(!lines[0].contains("already installed"), "{lines:?}");
    }

    #[test]
    fn a_reinstall_that_only_updates_a_record_names_the_record_and_lists_no_installed_file() {
        let installation = ManagedModInstallation {
            recorded: vec![PathBuf::from("mods/lib.jar")],
            ..ManagedModInstallation::default()
        };

        let lines = installation_lines("Lib", &installation);

        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("Lib"), "{lines:?}");
        assert!(lines[1].contains("mods/lib.jar"), "{lines:?}");
        assert!(lines[1].contains("No file was written"), "{lines:?}");
    }

    #[test]
    fn a_reinstall_that_changes_nothing_says_there_was_nothing_to_do() {
        let lines = installation_lines("Lib", &ManagedModInstallation::default());

        assert_eq!(
            lines,
            vec![ephervex_i18n::lookup("mod-install-nothing-to-do")]
        );
    }

    #[test]
    fn an_install_names_the_jar_that_stood_in_for_a_dependency() {
        let installation = ManagedModInstallation {
            installed: vec![PathBuf::from("mods/root.jar")],
            stood_in: vec![ephervex_core::InstalledStandIn {
                planned: PathBuf::from("mods/libx-7.0.4.jar"),
                installed: PathBuf::from("mods/libx-dev.jar"),
                mod_ids: vec!["libx".to_owned()],
            }],
            ..ManagedModInstallation::default()
        };

        let lines = installation_lines("Root", &installation);

        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[2].contains("mods/libx-7.0.4.jar"), "{lines:?}");
        assert!(lines[2].contains("mods/libx-dev.jar"), "{lines:?}");
        assert!(lines[2].contains("libx"), "{lines:?}");
    }

    #[test]
    fn a_dependency_already_provided_is_refused_with_the_option_that_uses_the_installed_jar() {
        let refusal = dependency_already_provided(
            "LibX",
            "LIBX",
            Path::new("/instance/minecraft/mods/libx-dev.jar"),
        );
        let text = format!("{refusal:#}");

        assert!(text.contains("libx-dev.jar"), "{text}");
        assert!(text.contains("LibX"), "{text}");
        assert!(text.contains("--use-installed LIBX"), "{text}");
    }

    #[test]
    fn an_install_reports_a_kept_jar_beside_the_files_it_wrote() {
        let installation = ManagedModInstallation {
            installed: vec![PathBuf::from("mods/dep.jar")],
            modified: vec![PathBuf::from("mods/root.jar")],
            ..ManagedModInstallation::default()
        };

        let lines = installation_lines("Root", &installation);

        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[1].contains("mods/dep.jar"), "{lines:?}");
        assert!(lines[2].contains("mods/root.jar"), "{lines:?}");
    }

    /// Stop requests delivered by hand instead of by signals.
    struct ScriptedRequests(tokio::sync::mpsc::UnboundedReceiver<()>);

    impl StopRequests for ScriptedRequests {
        async fn next(&mut self) {
            if self.0.recv().await.is_none() {
                std::future::pending::<()>().await;
            }
        }
    }

    fn scripted_requests() -> (tokio::sync::mpsc::UnboundedSender<()>, ScriptedRequests) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (sender, ScriptedRequests(receiver))
    }

    fn launch_stop_signals(server_stopping: bool) -> LaunchStopSignals {
        LaunchStopSignals {
            stop: CancellationToken::new(),
            kill_server: CancellationToken::new(),
            server_stopping: Arc::new(AtomicBool::new(server_stopping)),
        }
    }

    #[tokio::test]
    async fn the_first_request_stops_the_game_and_the_next_kills_the_linked_server() {
        let (sender, requests) = scripted_requests();
        let signals = launch_stop_signals(false);
        let (stop, kill_server) = (signals.stop.clone(), signals.kill_server.clone());
        let task = tokio::spawn(end_launch_on_requests(requests, signals));

        sender.send(()).expect("first request");
        stop.cancelled().await;
        assert!(
            !kill_server.is_cancelled(),
            "one request leaves the server to save"
        );

        sender.send(()).expect("second request");
        kill_server.cancelled().await;
        task.await.expect("task");
    }

    #[tokio::test]
    async fn a_request_while_the_linked_server_is_stopping_kills_it_without_stopping_the_game() {
        let (sender, requests) = scripted_requests();
        let signals = launch_stop_signals(true);
        let (stop, kill_server) = (signals.stop.clone(), signals.kill_server.clone());
        let task = tokio::spawn(end_launch_on_requests(requests, signals));

        sender.send(()).expect("request");

        kill_server.cancelled().await;
        task.await.expect("task");
        assert!(
            !stop.is_cancelled(),
            "the game had already ended, so its exit is not a requested stop"
        );
    }

    /// A supervised child that stops only when asked, the way a server answers
    /// the supervisor's graceful stop.
    async fn stops_when_canceled(cancel: CancellationToken) -> &'static str {
        cancel.cancelled().await;
        "stopped"
    }

    #[tokio::test]
    async fn a_termination_request_stops_the_child_gracefully_instead_of_killing_it() {
        let cancel = CancellationToken::new();
        let (sender, mut requests) = scripted_requests();
        let killed = std::sync::atomic::AtomicBool::new(false);
        sender.send(()).expect("request");

        let result = supervise_foreground(
            stops_when_canceled(cancel.clone()),
            &cancel,
            &mut requests,
            || async { killed.store(true, std::sync::atomic::Ordering::SeqCst) },
        )
        .await;

        assert_eq!(result, "stopped");
        assert!(cancel.is_cancelled());
        assert!(!killed.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_second_termination_request_kills_a_child_that_will_not_stop() {
        let cancel = CancellationToken::new();
        let (sender, mut requests) = scripted_requests();
        let killed = std::sync::Arc::new(tokio::sync::Notify::new());
        sender.send(()).expect("first");
        sender.send(()).expect("second");

        let result = supervise_foreground(
            {
                let killed = std::sync::Arc::clone(&killed);
                async move {
                    killed.notified().await;
                    "killed"
                }
            },
            &cancel,
            &mut requests,
            || async { killed.notify_one() },
        )
        .await;

        assert_eq!(result, "killed");
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn a_cancel_from_elsewhere_stops_the_child_and_the_next_request_kills_it() {
        let cancel = CancellationToken::new();
        let (sender, mut requests) = scripted_requests();
        let killed = std::sync::Arc::new(tokio::sync::Notify::new());
        // What a closed standard output does.
        cancel.cancel();
        let quiet = supervise_foreground(
            stops_when_canceled(cancel.clone()),
            &cancel,
            &mut requests,
            || async { panic!("nothing asked for a kill") },
        )
        .await;
        assert_eq!(quiet, "stopped");

        sender.send(()).expect("request");
        let result = supervise_foreground(
            {
                let killed = std::sync::Arc::clone(&killed);
                async move {
                    killed.notified().await;
                    "killed"
                }
            },
            &cancel,
            &mut requests,
            || async { killed.notify_one() },
        )
        .await;
        assert_eq!(result, "killed");
    }

    #[test]
    fn a_closed_reader_cancels_the_command_without_failing_it() {
        let cancel = CancellationToken::new();
        let watch = StdoutWatch::new(cancel.clone());
        assert!(!watch.is_closed());

        watch.record(&std::io::Error::from(std::io::ErrorKind::BrokenPipe));

        assert!(cancel.is_cancelled());
        assert!(watch.is_closed());
        assert!(watch.result().is_ok());
    }

    #[test]
    fn a_terminal_that_hung_up_is_a_reader_that_went_away() {
        let cancel = CancellationToken::new();
        let watch = StdoutWatch::new(cancel.clone());

        watch.record(&std::io::Error::from_raw_os_error(TERMINAL_HUNG_UP));

        assert!(cancel.is_cancelled());
        assert!(watch.result().is_ok());
    }

    #[test]
    fn a_write_error_other_than_a_closed_reader_fails_the_command() {
        let cancel = CancellationToken::new();
        let watch = StdoutWatch::new(cancel.clone());

        watch.record(&std::io::Error::from(std::io::ErrorKind::StorageFull));
        watch.record(&std::io::Error::from(std::io::ErrorKind::BrokenPipe));

        assert!(cancel.is_cancelled());
        let error = watch.result().expect_err("a real write failure");
        assert!(error.to_string().contains("standard output"), "{error}");
    }

    const INSTANCE_ID: &str = "11111111-1111-4111-8111-111111111111";
    const SNAPSHOT_ID: &str = "22222222-2222-4222-8222-222222222222";

    fn import_args(argv: &[&str]) -> Result<ImportArgs, clap::Error> {
        let mut full = vec!["ephervex", "import"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full)?.command {
            Command::Import(parsed) => Ok(parsed),
            other => panic!("expected an Import command, got {other:?}"),
        }
    }

    #[test]
    fn a_redirect_to_another_site_is_refused_without_a_terminal_and_names_where_it_went() {
        let error =
            resolve_redirect_consent("https://cdn.example.net/pack", "cdn.example.net", true)
                .expect_err("--json never prompts");

        let message = error.to_string();
        assert!(
            message.contains("https://cdn.example.net/pack"),
            "{message}"
        );
        assert!(message.contains("not been agreed to"), "{message}");
    }

    fn server_exit(
        code: Option<i32>,
        signal: Option<i32>,
        finished_starting: bool,
        stop_requested: bool,
    ) -> ServerExit {
        ServerExit {
            code,
            signal,
            finished_starting,
            stop_requested,
        }
    }

    #[test]
    fn a_requested_server_stop_succeeds_whatever_status_the_server_left_with() {
        for exit in [
            server_exit(Some(0), None, true, true),
            server_exit(Some(143), None, true, true),
            server_exit(None, Some(9), false, true),
        ] {
            server_exit_result(&exit).expect("a requested stop is not a failure");
        }
    }

    #[test]
    fn a_server_that_finished_starting_and_exited_cleanly_succeeds() {
        server_exit_result(&server_exit(Some(0), None, true, false)).expect("clean exit");
    }

    #[test]
    fn a_server_that_exited_before_finishing_startup_fails_even_with_status_zero() {
        let error = server_exit_result(&server_exit(Some(0), None, false, false))
            .expect_err("startup never finished");
        assert!(error.to_string().contains("before it finished starting"));
    }

    #[test]
    fn an_unrequested_server_crash_or_signal_fails() {
        for exit in [
            server_exit(Some(1), None, true, false),
            server_exit(None, Some(11), true, false),
        ] {
            assert!(server_exit_result(&exit).is_err(), "{exit:?}");
        }
    }

    #[test]
    fn a_refused_or_failed_server_control_request_fails_the_command() {
        let refused = control_response_result(&ControlResponse::Refused(
            "server input is unavailable".to_owned(),
        ))
        .expect_err("a refusal performed nothing");
        assert!(refused.to_string().contains("server input is unavailable"));

        let failed =
            control_response_result(&ControlResponse::StartFailed("no such file".to_owned()))
                .expect_err("a failed restart leaves the server stopped");
        assert!(failed.to_string().contains("no such file"));
    }

    #[test]
    fn a_server_left_temporary_by_a_persist_fails_the_command() {
        let warnings = ["alpha", "beta"].map(|name| PromotionWarning {
            server: ServerId::new(),
            name: name.to_owned(),
            error: CoreError::StorageFull { path: None },
        });
        let error = unmoved_servers_result(&warnings)
            .expect_err("a requested server stayed in temporary storage");
        assert!(error.to_string().contains("alpha, beta"), "{error}");
    }

    #[test]
    fn a_persist_that_moved_every_requested_server_succeeds() {
        unmoved_servers_result(&[]).expect("nothing was left behind");
    }

    #[test]
    fn a_refresh_that_could_not_reach_a_catalog_fails_the_command() {
        let report = DependencyRefreshReport {
            recorded: 3,
            providers_unavailable: vec![ModProvider::Modrinth, ModProvider::CurseForge],
            ..DependencyRefreshReport::default()
        };
        let error = unreachable_catalogs_result(&report)
            .expect_err("mods of an unreachable catalog stayed unrecorded");
        assert!(
            error.to_string().contains("Modrinth, CurseForge"),
            "{error}"
        );
    }

    #[test]
    fn a_refresh_that_reached_every_catalog_succeeds() {
        let report = DependencyRefreshReport {
            recorded: 3,
            unidentified: 1,
            ..DependencyRefreshReport::default()
        };
        unreachable_catalogs_result(&report).expect("every catalog answered");
    }

    #[test]
    fn a_performed_server_control_request_succeeds() {
        for response in [
            ControlResponse::Running,
            ControlResponse::Accepted,
            ControlResponse::Exited(server_exit(Some(0), None, true, true)),
            ControlResponse::Restarted { pid: 7 },
        ] {
            control_response_result(&response).expect("performed");
        }
    }

    #[test]
    fn a_game_that_crashed_or_was_signaled_fails_the_launch() {
        for exit in [
            GameExit {
                code: Some(1),
                signal: None,
            },
            GameExit {
                code: None,
                signal: Some(11),
            },
        ] {
            assert!(game_exit_result(Some(exit), false).is_err(), "{exit:?}");
        }
    }

    #[test]
    fn a_clean_game_exit_a_dry_run_and_a_requested_stop_succeed() {
        game_exit_result(
            Some(GameExit {
                code: Some(0),
                signal: None,
            }),
            false,
        )
        .expect("clean exit");
        game_exit_result(None, false).expect("a dry run never ran the game");
        game_exit_result(
            Some(GameExit {
                code: None,
                signal: Some(15),
            }),
            true,
        )
        .expect("a requested stop");
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

    const SERVER_ID: &str = "22222222-2222-4222-8222-222222222222";

    #[test]
    fn every_catalog_command_accepts_a_client_or_a_server_and_refuses_neither_or_both() {
        for command in ["search", "show", "plan", "install"] {
            let argument = if command == "search" {
                "jei"
            } else {
                "jei-project"
            };
            for flag in [("--instance", INSTANCE_ID), ("--server", SERVER_ID)] {
                Cli::try_parse_from(["ephervex", "mod", command, flag.0, flag.1, argument])
                    .unwrap_or_else(|error| panic!("{command} {}: {error}", flag.0));
            }
            assert!(
                Cli::try_parse_from(["ephervex", "mod", command, argument]).is_err(),
                "{command} needs a host"
            );
            assert!(
                Cli::try_parse_from([
                    "ephervex",
                    "mod",
                    command,
                    "--instance",
                    INSTANCE_ID,
                    "--server",
                    SERVER_ID,
                    argument,
                ])
                .is_err(),
                "{command} takes one host"
            );
        }
    }

    #[test]
    fn an_install_onto_a_server_reads_the_unknown_support_confirmation() {
        let cli = Cli::try_parse_from([
            "ephervex",
            "mod",
            "install",
            "--server",
            SERVER_ID,
            "--accept-unknown-server-support",
            "jei",
        ])
        .expect("valid install");
        let Command::Mod {
            command: ModCommand::Install { host, consent, .. },
        } = cli.command
        else {
            panic!("expected mod install");
        };
        assert!(matches!(host.host(), Ok(InstalledHost::Server(_))));
        assert!(consent.accept_unknown_server_support);
    }

    #[test]
    fn a_mod_install_takes_every_dependency_an_installed_jar_may_stand_in_for() {
        let cli = Cli::try_parse_from([
            "ephervex",
            "mod",
            "install",
            "--instance",
            INSTANCE_ID,
            "--use-installed",
            "LIBX",
            "--use-installed",
            "LIBY",
            "jei",
        ])
        .expect("valid install");
        let Command::Mod {
            command: ModCommand::Install { consent, .. },
        } = cli.command
        else {
            panic!("expected mod install");
        };
        assert_eq!(consent.use_installed, ["LIBX", "LIBY"]);
    }

    #[test]
    fn a_client_only_dependency_warning_names_the_dependency_and_the_mod_that_needs_it() {
        let text = warning_text(&PlanWarning::ServerSideUnsupported {
            project_id: "CLIENTLIB".to_owned(),
            required_by: vec!["ROOTPROJ".to_owned(), "OTHERPROJ".to_owned()],
        });
        assert!(text.contains("CLIENTLIB"), "{text}");
        assert!(text.contains("ROOTPROJ, OTHERPROJ"), "{text}");
    }

    #[test]
    fn snapshot_restore_parses_the_host_and_explicit_safety_override() {
        let cli = Cli::try_parse_from([
            "ephervex",
            "snapshot",
            "restore",
            "--instance",
            INSTANCE_ID,
            "--no-safety-snapshot",
            "--yes",
            SNAPSHOT_ID,
        ])
        .expect("valid snapshot restore");

        let Command::Snapshot {
            command:
                SnapshotCommand::Restore {
                    host,
                    id,
                    no_safety_snapshot,
                    yes,
                },
        } = cli.command
        else {
            panic!("expected snapshot restore");
        };
        assert!(matches!(host.host(), Ok(InstalledHost::Instance(_))));
        assert_eq!(id.to_string(), SNAPSHOT_ID);
        assert!(no_safety_snapshot);
        assert!(yes);
    }

    #[test]
    fn snapshot_commands_require_exactly_one_host() {
        let missing =
            Cli::try_parse_from(["ephervex", "snapshot", "list"]).expect_err("a host is required");
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let conflicting = Cli::try_parse_from([
            "ephervex",
            "snapshot",
            "list",
            "--instance",
            INSTANCE_ID,
            "--server",
            "33333333-3333-4333-8333-333333333333",
        ])
        .expect_err("two hosts conflict");
        assert_eq!(conflicting.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hangup_is_a_request_to_stop_the_game() {
        // A process started under `nohup` keeps ignoring hangups by design.
        let hangup_ignored = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("SigIgn:"))
                    .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            })
            .is_some_and(|mask| mask & 1 == 1);
        if hangup_ignored {
            return;
        }

        // The signals are registered once this returns, so the hangup below
        // cannot arrive before the handler exists.
        let mut requests = TerminationRequests::watch().expect("signals are watched");
        let waiting = tokio::spawn(async move { requests.next().await });
        let status = tokio::process::Command::new("kill")
            .args(["-HUP", &std::process::id().to_string()])
            .status()
            .await
            .expect("kill runs");
        assert!(status.success());

        // Only a failsafe: the signal is already pending, so this does not
        // race the handler on a loaded machine.
        tokio::time::timeout(std::time::Duration::from_secs(60), waiting)
            .await
            .expect("the hangup is noticed")
            .expect("task");
    }

    fn microsoft_summary(credential: CredentialPresence) -> AccountSummary {
        AccountSummary {
            id: "00000000-0000-0000-0000-000000000000"
                .parse()
                .expect("uuid"),
            name: "Steve".to_owned(),
            kind: ephervex_model::AccountKind::Microsoft,
            is_default: true,
            added_at: String::new(),
            last_authenticated_at: String::new(),
            credential,
        }
    }

    #[test]
    fn an_unknown_credential_is_listed_as_unknown_and_not_as_not_saved() {
        let account = microsoft_summary(CredentialPresence::Unknown);

        let label = account_storage_label(&account, None);

        assert_eq!(label, ephervex_i18n::lookup("auth-storage-unknown"));
        assert_ne!(label, ephervex_i18n::lookup("auth-storage-not-saved"));
    }

    #[test]
    fn a_missing_credential_is_still_listed_as_not_saved() {
        let account = microsoft_summary(CredentialPresence::Missing);

        assert_eq!(
            account_storage_label(&account, None),
            ephervex_i18n::lookup("auth-storage-not-saved")
        );
    }

    fn status_with(kind: ephervex_model::AccountKind) -> AccountStatus {
        let mut account = microsoft_summary(CredentialPresence::NotApplicable);
        account.kind = kind;
        AccountStatus {
            active: Some(account.clone()),
            accounts: vec![account],
            session_active: false,
            credential_storage: None,
        }
    }

    #[test]
    fn the_status_json_does_not_call_an_offline_default_account_signed_in() {
        let json = account_status_json(&status_with(ephervex_model::AccountKind::Offline));

        assert_eq!(json["signed_in"], false);
        assert_eq!(json["default"]["kind"], "offline");
        assert_eq!(json["default"]["authenticated"], false);
    }

    #[test]
    fn the_status_json_calls_a_microsoft_default_account_signed_in() {
        let json = account_status_json(&status_with(ephervex_model::AccountKind::Microsoft));

        assert_eq!(json["signed_in"], true);
        assert_eq!(json["default"]["kind"], "microsoft");
    }

    #[test]
    fn the_status_json_reports_no_default_account_as_not_signed_in() {
        let json = account_status_json(&AccountStatus {
            active: None,
            accounts: Vec::new(),
            session_active: false,
            credential_storage: None,
        });

        assert_eq!(json["signed_in"], false);
        assert!(json["default"].is_null(), "{json}");
    }

    #[test]
    fn the_account_json_reports_an_unknown_credential_as_null() {
        let json = |credential| account_json(&microsoft_summary(credential));

        assert!(json(CredentialPresence::Unknown)["has_stored_credential"].is_null());
        assert_eq!(json(CredentialPresence::Unknown)["credential"], "unknown");
        assert_eq!(
            json(CredentialPresence::Stored)["has_stored_credential"],
            true
        );
        assert_eq!(
            json(CredentialPresence::Missing)["has_stored_credential"],
            false
        );
    }
}
