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

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ephervex_core::{
    AccountStatus, AccountSummary, ApiSettings, App, ArgumentForm, Artifact, AssetIndexRef,
    AuthEvent, AuthObserver, CancellationToken, ClientFolder, ClientRecord, Config, CoreError,
    CreatedClient, CredentialStorage, DuplicateClient, Entitlements, FileDisposition, Freshness,
    JavaPathChoice, JavaPlan, JavaPlanRequest, JavaRuntime, LaunchEvent, LaunchFeatures,
    LaunchObserver, LaunchOptions, LaunchOutcome, LaunchState, LoginFlow, LoginOptions,
    LoginRequest, ManagedInstallation, NewClient, NewServer, OutputLine, Paths,
    PreferenceApplication, PreferenceKey, ReconciliationReport, ReleaseListing, RequirementBasis,
    RuntimeInventory, SecretString, ServerFolder, ServerRecord, Session, VersionMetadata,
    VersionRecord,
};
use ephervex_i18n::Arg as MessageArg;
use ephervex_model::{InstanceId, LoaderKind, LoaderSelection, ServerId, StorageClass};

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
    /// Show build information.
    Info,
    /// Show the resolved application directories.
    Paths,
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
    /// Inspect and change the provider API settings .
    Api {
        #[command(subcommand)]
        command: ApiCommand,
    },
    /// Install and launch a client instance, streaming its log to the terminal.
    Launch(LaunchArgs),
    /// Reconcile the index against what is actually on disk.
    Reconcile,
}

#[derive(Debug, Args)]
struct LaunchArgs {
    /// The client instance to launch.
    id: InstanceId,
    /// Use this Java runtime instead of the selected one.
    #[arg(long)]
    java_path: Option<PathBuf>,
    /// Download a managed runtime if nothing installed is compatible.
    #[arg(long)]
    install_java: bool,
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
    /// Remove a client instance from the index.
    Delete {
        id: InstanceId,
        /// Leave the instance directory on disk.
        #[arg(long)]
        keep_files: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ServersCommand {
    /// List every indexed server.
    List,
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Create a server.
    Create(CreateServerArgs),
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
    /// Remove a server from the index.
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
    name: String,
    #[arg(long)]
    minecraft: String,
    #[arg(long, value_enum, default_value_t = LoaderArg::Vanilla)]
    loader: LoaderArg,
    #[arg(long)]
    loader_version: Option<String>,
    /// Link this server to a client instance.
    #[arg(long)]
    client: Option<InstanceId>,
    #[command(flatten)]
    storage: StorageArgs,
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
    ephervex_core::init_tracing(ephervex_core::DEFAULT_LOG_DIRECTIVES);

    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_failure(&error, cli.json);
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Info => {
            show_info(cli.json);
            Ok(())
        }
        Command::Paths => {
            // Resolved without opening storage, so this still works when the
            // database or the configuration is the thing that is broken.
            let paths = Paths::from_environment()?;
            let config = Config::load_or_default(&paths)?;
            show_paths(&config.apply_to(paths), cli.json);
            Ok(())
        }
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
            run_instance_command(&app, command, cli.json)
        }
        Command::Servers { command } => {
            let app = open_app()?;
            match command {
                ServersCommand::List => list_servers(&app, cli.json)?,
            }
            Ok(())
        }
        Command::Server { command } => {
            let app = open_app()?;
            run_server_command(&app, command, cli.json)
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
        Command::Launch(args) => {
            let mut app = open_app()?;
            // Downloading, spawning, and streaming all need an executor, for
            // the same reason the Minecraft commands do.
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(run_launch(&mut app, args, cli.json))
        }
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

fn run_instance_command(app: &App, command: &InstanceCommand, json: bool) -> anyhow::Result<()> {
    match command {
        InstanceCommand::Create(args) => {
            let loader = build_loader(args.loader, args.loader_version.as_deref())?;
            let created = app.create_client_reporting(
                NewClient::new(&args.name, &args.minecraft, loader)
                    .with_storage(args.storage.class()),
            )?;
            print_created_instance(app, &created, json)?;
        }
        InstanceCommand::Show { id } => {
            let record = app.client(*id)?;
            print_instance(app, &record, json)?;
        }
        InstanceCommand::Duplicate { id, name, storage } => {
            // Both fields default to "the same as the original", which is what
            // `ephervex instance duplicate <id>` with no flags should mean.
            let request = DuplicateClient {
                name: name.clone(),
                storage: storage.class(),
            };
            let copy = app.duplicate_client(*id, &request)?;
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
        InstanceCommand::Open { id, folder, print } => {
            let path = if *print {
                app.client_folder(*id, *folder)?
            } else {
                app.open_client_folder(*id, *folder)?
            };
            print_folder(path.as_deref(), folder.as_str(), json)?;
        }
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

fn run_server_command(app: &App, command: &ServerCommand, json: bool) -> anyhow::Result<()> {
    match command {
        ServerCommand::Create(args) => {
            let loader = build_loader(args.loader, args.loader_version.as_deref())?;
            let mut request = NewServer::new(&args.name, &args.minecraft, loader)
                .with_storage(args.storage.class());
            if let Some(client) = args.client {
                // Fail before creating anything if the link is bogus.
                app.client(client)?;
                request = request.linked_to(client);
            }
            let record = app.create_server(request)?;
            print_server(&record, json);
        }
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
            println!("deleted {id}");
        }
    }
    Ok(())
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

    let outcome = app.launch(args.id, &options, &observer, &cancel).await;
    // The printer ends when every sender is gone, so this drop is what lets it
    // finish rather than hang.
    drop(observer);
    let _ = printer.await;

    let outcome = outcome?;
    report_launch(&outcome, json);
    Ok(())
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
            LaunchEvent::Preparing => eprintln!("{}", ephervex_i18n::lookup("launch-preparing")),
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
        let values: Vec<_> = records.iter().map(server_json).collect();
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
                        instance: *instance,
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
            let key = key.trim();
            if key.is_empty() {
                anyhow::bail!("no API key was supplied on standard input");
            }
            app.set_curseforge_api_key(Some(&SecretString::new(key)))?;
            // Never echo it back, not even truncated.
            println!("stored the CurseForge API key in the system keyring");
        }
        ApiCommand::ClearCurseforgeKey => {
            app.set_curseforge_api_key(None)?;
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
}

// -------------------------------------------------------------------- output

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
    // A preference this release has no setting for is skipped, and silence
    // would look like a bug.
    for key in &applied.unsupported {
        eprintln!(
            "{}",
            ephervex_i18n::lookup_args(
                "preferences-unsupported-for-version",
                [
                    ("preference", MessageArg::from(key.as_str())),
                    (
                        "version",
                        MessageArg::from(created.record.minecraft_version.as_str())
                    ),
                ]
            )
        );
    }
    Ok(())
}

fn preferences_applied_json(applied: Option<&PreferenceApplication>) -> serde_json::Value {
    applied.map_or(serde_json::Value::Null, |applied| {
        serde_json::json!({
                    "adapter": applied.adapter,
                    "created_file": applied.created_file,
                    "applied": applied
        .applied
        .iter()
        .map(|key| key.as_str())
        .collect::<Vec<_>>(),
                    "unsupported": applied
        .unsupported
        .iter()
        .map(|key| key.as_str())
        .collect::<Vec<_>>(),
                })
    })
}

/// Prints the outcome of an open-folder action.
///
/// An unavailable folder is a failure with an exit code, not a silent success:
/// a script that asked for the crash reports of an instance that has never
/// crashed should be able to tell.
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
        println!("eula:      {}", record.eula_accepted);
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
        "eula_accepted": record.eula_accepted,
        "created_at": record.created_at,
    })
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

    let (summary, action) = error.downcast_ref::<CoreError>().map_or_else(
        || (technical.first().cloned().unwrap_or_default(), None),
        localized_failure,
    );

    if json {
        let value = serde_json::json!({
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
    let summary = ephervex_i18n::lookup(id);

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
