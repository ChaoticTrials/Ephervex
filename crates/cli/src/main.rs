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

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ephervex_core::{
    App, ArgumentForm, Artifact, AssetIndexRef, ClientRecord, Config, CoreError, FileDisposition,
    Freshness, NewClient, NewServer, Paths, ReconciliationReport, ReleaseListing, ServerRecord,
    VersionMetadata, VersionRecord,
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
    /// Show the effective configuration.
    Config,
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
    /// Reconcile the index against what is actually on disk.
    Reconcile,
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
    ephervex_core::init_tracing("info");

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
        Command::Config => {
            let paths = Paths::from_environment()?;
            show_config(&Config::load_or_default(&paths)?, &paths, cli.json)
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
            let record = app.create_client(
                NewClient::new(&args.name, &args.minecraft, loader)
                    .with_storage(args.storage.class()),
            )?;
            print_instance(app, &record, json)?;
        }
        InstanceCommand::Show { id } => {
            let record = app.client(*id)?;
            print_instance(app, &record, json)?;
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
        for id in app.linked_server_ids(record.id)? {
            println!("server:    {id}");
        }
        for reason in &record.attention {
            println!("attention: {reason:?}");
        }
    }
    Ok(())
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
    for line in &technical {
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

    // Two shapes of next step. One names the file to look at, and can only be
    // rendered when the error carries a path — otherwise the user would be
    // shown a literal `{$path}`. The other, typically a network failure, needs
    // no argument at all and must still be shown .
    let action = match error.failing_path() {
        Some(path) => ephervex_i18n::lookup_attribute_args(
            id,
            "action",
            [("path", MessageArg::from(path.as_path()))],
        ),
        None => {
            ephervex_i18n::lookup_attribute(id, "action").filter(|action| !action.contains("{$"))
        }
    };

    (summary, action)
}
