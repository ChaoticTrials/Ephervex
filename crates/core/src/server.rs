//! Dedicated-server application services composed through `ephervex-server`.

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use chrono::Utc;
use ephervex_download::{CacheLayout, DownloadManager, DownloaderConfig, OperationReporter};
use ephervex_model::{
    AttentionReason, ImportSourceKind, JavaOverride, JvmSettings, OperationId, ServerId,
    ServerInstallation, ServerLaunch, ServerRelativePath,
};
use ephervex_pack::{
    OptionalSelection, PackError, PackRecipe, ServerMaterializeRequest, SourceStore,
};
use ephervex_server::{
    CaptureHealth, CaptureRetention, CapturedLine, ControlAction, ControlResponse,
    EulaConsentRecord, EulaStatus, NetworkProperties, PropertiesDocument, RuntimeClassification,
    ServerBind, ServerCommand, ServerObserver, ServerSupervisor, ServerWarning, accepted_record,
    classify_runtime, clear_stale_runtime, materialize_eula_file, revoked_record, send_control,
};
use ephervex_storage::{
    FileDisposition, MetadataError, NetworkAccess, NewServer, RegistryError, ServerCommit,
    ServerNetworkSettings, ServerRecord,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app::{App, JavaPlanRequest, JavaTarget};
use crate::{CoreError, CoreResult};

const SERVER_PROPERTIES_FILE: &str = "server.properties";
const EULA_FILE: &str = "eula.txt";

#[derive(Debug, Clone, Default)]
pub struct ServerCreateOptions {
    pub java_path: Option<PathBuf>,
    pub install_java_if_missing: bool,
    /// Recipe entries whose server applicability is undeclared but which the
    /// caller explicitly confirms for this creation.
    pub confirmed_unknown: OptionalSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerPersistenceHealth {
    Healthy,
    IndexOutOfSync,
    MissingRoot,
    MetadataMissing,
    MetadataCorrupt,
    MetadataUnsupported,
    IdentityConflict,
}

impl ServerPersistenceHealth {
    #[must_use]
    pub const fn permits_launch_and_mutation(&self) -> bool {
        matches!(self, Self::Healthy | Self::IndexOutOfSync)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub record: ServerRecord,
    pub persistence: ServerPersistenceHealth,
    pub runtime: RuntimeClassification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSnapshot {
    pub lines: Vec<CapturedLine>,
    pub health: CaptureHealth,
    pub retention: CaptureRetention,
}

/// A fully validated server start whose process ownership can move onto the
/// application's supervision task.
pub(super) struct PreparedServerStart {
    command: ServerCommand,
    root: PathBuf,
    capture: PathBuf,
    /// The id every launcher log line for this start carries, kept so the
    /// supervision that follows the preparation logs under the same one.
    operation: OperationId,
}

impl App {
    /// Builds and installs a complete server in a hidden sibling before the
    /// final root is published.
    pub async fn create_server(
        &self,
        request: NewServer,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let mut record = self.registry_for_services().prepare_server(request)?;
        let linked_recipe = self.linked_server_recipe(&mut record, &options.confirmed_unknown)?;
        let staging = staging_root(&record.root);
        if staging.exists() {
            return Err(storage_io(
                &staging,
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "server staging root already exists",
                ),
            ));
        }
        std::fs::create_dir_all(&staging).map_err(|source| storage_io(&staging, source))?;
        let cleanup = StagingCleanup::new(staging.clone());
        let target = ephervex_storage::metadata::server_game_dir(&staging);

        let java = self
            .plan_java_for_unindexed_server(
                &record.minecraft_version,
                &record.loader,
                &JavaPlanRequest {
                    target: JavaTarget::VersionOnly,
                    explicit_path: options.java_path.clone(),
                    install_if_missing: options.install_java_if_missing,
                },
            )
            .await?;
        let installed = self
            .install_server_version(
                &record.minecraft_version,
                &record.loader,
                &java.runtime.executable,
                &target,
                cancel,
            )
            .await?;
        record.installation = ServerInstallation::Installed {
            launch: normalize_launch(&target, &installed.launch)?,
        };
        if let Some((client_source, recipe)) = linked_recipe {
            let server_source = ephervex_storage::metadata::source_dir(&staging);
            let unconfirmed_unknown = self
                .materialize_linked_server_recipe(
                    &recipe,
                    &client_source,
                    &target,
                    &server_source,
                    options,
                    cancel,
                )
                .await?;
            add_unknown_attention(&mut record, unconfirmed_unknown);
        }
        merge_network_file(&target.join(SERVER_PROPERTIES_FILE), &record.network)?;
        let committed = self
            .registry_for_services()
            .publish_staged_server(&record, &staging)?;
        cleanup.disarm();
        Ok(committed)
    }

    /// Installs a previously scaffolded record. Unknown files are never merged.
    pub async fn install_server(
        &self,
        id: ServerId,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let mut record = self.canonical_server_for_mutation(id)?;
        refuse_if_running(&record)?;
        if record.installation.is_installed() {
            return Ok(ServerCommit::clean(record));
        }
        let target = ephervex_storage::metadata::server_game_dir(&record.root);
        ensure_empty_directory(&target)?;
        let linked_recipe = self.linked_server_recipe(&mut record, &options.confirmed_unknown)?;
        let server_source = ephervex_storage::metadata::source_dir(&record.root);
        if linked_recipe.is_some() {
            ensure_empty_directory(&server_source)?;
        }
        let cleanup = InstallationCleanup::new(
            target.clone(),
            linked_recipe.as_ref().map(|_| server_source.clone()),
        );
        let java = self
            .plan_java(
                &record.minecraft_version,
                &JavaPlanRequest {
                    target: JavaTarget::Server(id),
                    explicit_path: options.java_path.clone(),
                    install_if_missing: options.install_java_if_missing,
                },
            )
            .await?;
        let installed = self
            .install_server_version(
                &record.minecraft_version,
                &record.loader,
                &java.runtime.executable,
                &target,
                cancel,
            )
            .await?;
        record.installation = ServerInstallation::Installed {
            launch: normalize_launch(&target, &installed.launch)?,
        };
        if let Some((client_source, recipe)) = linked_recipe {
            let unconfirmed_unknown = self
                .materialize_linked_server_recipe(
                    &recipe,
                    &client_source,
                    &target,
                    &server_source,
                    options,
                    cancel,
                )
                .await?;
            add_unknown_attention(&mut record, unconfirmed_unknown);
        }
        merge_network_file(&target.join(SERVER_PROPERTIES_FILE), &record.network)?;
        let committed = self
            .registry_for_services()
            .update_server_committed(&mut record)?;
        cleanup.disarm();
        Ok(committed)
    }

    fn linked_server_recipe(
        &self,
        record: &mut ServerRecord,
        confirmed_unknown: &OptionalSelection,
    ) -> CoreResult<Option<(PathBuf, PackRecipe)>> {
        let Some(client_id) = record.linked_client_id else {
            return Ok(None);
        };
        let client = self.client(client_id)?;
        let source = ephervex_storage::metadata::source_dir(&client.root);
        let store = SourceStore::new(&source);
        let recipe = if store.has_recipe() {
            store.read_recipe()?
        } else if client.source.kind == ImportSourceKind::Manual {
            PackRecipe::for_manual_instance(
                client.name.clone(),
                client.minecraft_version.clone(),
                client.loader.clone(),
            )
        } else {
            return Err(PackError::RecipeMissing {
                path: store.recipe_path(),
            }
            .into());
        };
        if recipe.minecraft_version != client.minecraft_version || recipe.loader != client.loader {
            return Err(PackError::RecipeTargetMismatch {
                client_minecraft: client.minecraft_version,
                recipe_minecraft: recipe.minecraft_version,
                client_loader: client.loader.to_string(),
                recipe_loader: recipe.loader.to_string(),
            }
            .into());
        }
        record.source = client.source;

        // Validate confirmations and retention before Java probing or loader
        // installation can do expensive work.
        let plan = recipe.server_reconstruction_plan(confirmed_unknown)?;
        if let Some(layer) = plan.layers.iter().find(|layer| !layer.is_retained()) {
            return Err(PackError::RetainedLayerUnavailable {
                scope: layer.scope.as_str(),
                path: source,
            }
            .into());
        }
        Ok(Some((store.dir().to_path_buf(), recipe)))
    }

    async fn materialize_linked_server_recipe(
        &self,
        recipe: &PackRecipe,
        client_source: &Path,
        target: &Path,
        server_source: &Path,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<Vec<PathBuf>> {
        let manager = DownloadManager::new(DownloaderConfig {
            http: self.http_config(),
            ..DownloaderConfig::default()
        })
        .map_err(|source| PackError::DownloadFailed {
            path: recipe.original_name.clone(),
            source: Box::new(source),
        })?;
        let materialized = ephervex_pack::materialize_server_recipe(ServerMaterializeRequest {
            recipe,
            confirmed_unknown: &options.confirmed_unknown,
            game_dir: target,
            client_source_dir: client_source,
            server_source_dir: server_source,
            manager: &manager,
            cache: &CacheLayout::new(self.paths().cache_dir()),
            reporter: &OperationReporter::silent("server-content"),
            cancel,
        })
        .await?;
        Ok(materialized.unconfirmed_unknown)
    }

    pub fn server_status(&self, id: ServerId) -> CoreResult<ServerStatus> {
        let indexed = self.server(id)?;
        let persistence = persistence_health(self, &indexed);
        let runtime = match classify_runtime(&indexed.root) {
            Ok(runtime) => runtime,
            Err(_) => RuntimeClassification::UnknownRunning {
                supervisor: synthetic_unverifiable_evidence(),
                child: None,
            },
        };
        Ok(ServerStatus {
            record: indexed,
            persistence,
            runtime,
        })
    }

    pub fn delete_server(&self, id: ServerId, files: FileDisposition) -> CoreResult<()> {
        let record = self.canonical_server_for_mutation(id)?;
        refuse_if_running(&record)?;
        Ok(self.registry_for_services().delete_server(id, files)?)
    }

    pub fn server_properties(&self, id: ServerId) -> CoreResult<PropertiesDocument> {
        let record = self.canonical_server_for_mutation(id)?;
        read_properties(&server_properties_path(&record))
    }

    pub fn set_server_property(&self, id: ServerId, key: &str, value: &str) -> CoreResult<()> {
        let record = self.canonical_server_for_mutation(id)?;
        refuse_if_running(&record)?;
        let path = server_properties_path(&record);
        let mut document = read_properties(&path)?;
        document.set(key, value)?;
        atomic_write(&path, document.render().as_bytes())
    }

    pub fn set_server_network(
        &self,
        id: ServerId,
        network: ServerNetworkSettings,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let mut record = self.canonical_server_for_mutation(id)?;
        refuse_if_running(&record)?;
        let typed = network_properties(&network)?;
        typed.validate()?;
        merge_network_file(&server_properties_path(&record), &network)?;
        record.network = network;
        Ok(self
            .registry_for_services()
            .update_server_committed(&mut record)?)
    }

    /// Replaces the server-specific Java and JVM overrides as one canonical
    /// metadata mutation. `None` restores the corresponding global setting.
    pub fn set_server_runtime_settings(
        &self,
        id: ServerId,
        java_override: Option<JavaOverride>,
        jvm_override: Option<JvmSettings>,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let mut record = self.canonical_server_for_mutation(id)?;
        refuse_if_running(&record)?;
        record.java_override = java_override;
        record.jvm_override = jvm_override;
        Ok(self
            .registry_for_services()
            .update_server_committed(&mut record)?)
    }

    #[must_use]
    pub fn server_eula_status(&self) -> EulaStatus {
        EulaStatus::from_record(&EulaConsentRecord {
            accepted: self.config().eula.accepted,
            accepted_at: self.config().eula.accepted_at.clone(),
            reference: self.config().eula.reference.clone(),
        })
    }

    pub fn accept_server_eula(&mut self) -> CoreResult<EulaStatus> {
        let accepted = accepted_record(Utc::now());
        self.mutate_config(|config| {
            config.eula.accepted = accepted.accepted;
            config.eula.accepted_at = accepted.accepted_at;
            config.eula.reference = accepted.reference;
        })?;
        Ok(self.server_eula_status())
    }

    pub fn revoke_server_eula(&mut self) -> CoreResult<()> {
        let revoked = revoked_record();
        self.mutate_config(|config| {
            config.eula.accepted = revoked.accepted;
            config.eula.accepted_at = revoked.accepted_at;
            config.eula.reference = revoked.reference;
        })
    }

    /// Starts in the foreground and retains ownership of stdin and signals.
    pub async fn start_server(
        &self,
        id: ServerId,
        explicit_java: Option<PathBuf>,
        observer: &ServerObserver,
    ) -> CoreResult<ephervex_server::ServerExit> {
        let prepared = self
            .prepare_server_start(id, explicit_java, observer)
            .await?;
        Self::run_prepared_server(prepared, observer).await
    }

    pub(super) async fn prepare_server_start(
        &self,
        id: ServerId,
        explicit_java: Option<PathBuf>,
        observer: &ServerObserver,
    ) -> CoreResult<PreparedServerStart> {
        // Silent because a server start reports through `ServerObserver` rather
        // than through a progress bar; the reporter is here for its id, which
        // is the workspace's one way of naming an operation. Created before the
        // span so every line the preparation logs carries it.
        let operation = OperationReporter::silent("operation-starting-server").id();
        self.prepare_server_start_operation(id, explicit_java, observer, operation)
            .await
    }

    #[tracing::instrument(
        name = "prepare_server_start",
        skip(self, explicit_java, observer),
        fields(server = %id, operation = %operation)
    )]
    async fn prepare_server_start_operation(
        &self,
        id: ServerId,
        explicit_java: Option<PathBuf>,
        observer: &ServerObserver,
        operation: OperationId,
    ) -> CoreResult<PreparedServerStart> {
        observer.emit(ephervex_server::ServerEvent::Preparing { operation });

        let mut record = self.canonical_server_for_mutation(id)?;
        refuse_if_running(&record)?;
        let launch = record
            .installation
            .launch()
            .ok_or(ephervex_server::ServerError::NotInstalled)?
            .clone();
        let eula = self.server_eula_status();
        eula.require_valid()?;
        let server_dir = ephervex_storage::metadata::server_game_dir(&record.root);
        merge_network_file(&server_dir.join(SERVER_PROPERTIES_FILE), &record.network)?;
        materialize_eula_file(&server_dir.join(EULA_FILE), &eula)?;
        emit_network_advisories(&record.network, observer)?;
        let java = self
            .plan_java(
                &record.minecraft_version,
                &JavaPlanRequest {
                    target: JavaTarget::Server(id),
                    explicit_path: explicit_java,
                    install_if_missing: true,
                },
            )
            .await?;
        let command = build_server_command(&server_dir, &launch, &java);
        record.last_started_at = Some(Utc::now());
        let _commit = self
            .registry_for_services()
            .update_server_committed(&mut record)?;
        Ok(PreparedServerStart {
            command,
            root: record.root,
            capture: self.paths().server_capture_dir(id),
            operation,
        })
    }

    pub(super) async fn run_prepared_server(
        prepared: PreparedServerStart,
        observer: &ServerObserver,
    ) -> CoreResult<ephervex_server::ServerExit> {
        let operation = prepared.operation;
        let exit = ServerSupervisor::run(
            &prepared.command,
            &prepared.root,
            &prepared.capture,
            observer,
        )
        .await;
        match &exit {
            Ok(exit) => {
                tracing::info!(%operation, code = ?exit.code, signal = ?exit.signal, "server exited");
            }
            Err(error) => tracing::warn!(%operation, %error, "server supervision failed"),
        }
        Ok(exit?)
    }

    pub async fn stop_server(&self, id: ServerId) -> CoreResult<ControlResponse> {
        let record = self.server(id)?;
        Ok(send_control(&record.root, &ControlAction::Stop).await?)
    }

    pub async fn restart_server(&self, id: ServerId) -> CoreResult<ControlResponse> {
        let record = self.server(id)?;
        Ok(send_control(&record.root, &ControlAction::Restart).await?)
    }

    pub async fn kill_server(&self, id: ServerId) -> CoreResult<ControlResponse> {
        let record = self.server(id)?;
        Ok(send_control(&record.root, &ControlAction::Kill).await?)
    }

    pub async fn send_server_console(
        &self,
        id: ServerId,
        command: String,
    ) -> CoreResult<ControlResponse> {
        let record = self.server(id)?;
        Ok(send_control(&record.root, &ControlAction::Console(command)).await?)
    }

    pub fn recover_server_runtime(&self, id: ServerId) -> CoreResult<()> {
        let record = self.server(id)?;
        Ok(clear_stale_runtime(&record.root)?)
    }

    pub fn server_output(&self, id: ServerId, limit: usize) -> CoreResult<CaptureSnapshot> {
        let record = self.server(id)?;
        read_capture_snapshot(&self.paths().server_capture_dir(record.id), limit)
    }

    pub(super) fn canonical_server_for_mutation(&self, id: ServerId) -> CoreResult<ServerRecord> {
        let indexed = self.server(id)?;
        if duplicate_server_roots(self, id).len() > 1 {
            return Err(ephervex_server::ServerError::PersistenceUnhealthy {
                reason: "several roots claim this server id",
            }
            .into());
        }
        if !indexed.root.is_dir() {
            return Err(RegistryError::RootMissing { path: indexed.root }.into());
        }
        let mut canonical = ephervex_storage::metadata::read_server_metadata(&indexed.root)
            .map_err(metadata_core_error)?;
        if canonical.id != id {
            return Err(RegistryError::ServerUnknown { id }.into());
        }
        canonical.root = indexed.root;
        Ok(canonical)
    }

    pub(super) fn server_connection_target(
        record: &ServerRecord,
    ) -> CoreResult<ephervex_model::ConnectionTarget> {
        Ok(network_properties(&record.network)?.connection_target()?)
    }
}

fn normalize_launch(
    server_dir: &Path,
    launch: &ephervex_loaders::ServerLaunch,
) -> CoreResult<ServerLaunch> {
    let path = match launch {
        ephervex_loaders::ServerLaunch::Jar { jar } => jar,
        ephervex_loaders::ServerLaunch::ArgFile { file } => file,
    };
    let relative = path.strip_prefix(server_dir).map_err(|_| {
        CoreError::Server(ephervex_server::ServerError::Io {
            path: path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "loader returned a launch path outside the server directory",
            ),
        })
    })?;
    let path = ServerRelativePath::new(relative.to_path_buf()).map_err(|error| {
        let detail = error.to_string();
        CoreError::Server(ephervex_server::ServerError::Io {
            path: error.path,
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, detail),
        })
    })?;
    Ok(match launch {
        ephervex_loaders::ServerLaunch::Jar { .. } => ServerLaunch::Jar { path },
        ephervex_loaders::ServerLaunch::ArgFile { .. } => ServerLaunch::ArgFile { path },
    })
}

fn add_unknown_attention(record: &mut ServerRecord, paths: Vec<PathBuf>) {
    for path in paths {
        record.add_attention(AttentionReason::UnknownServerApplicability(
            path.to_string_lossy().into_owned(),
        ));
    }
}

fn network_properties(settings: &ServerNetworkSettings) -> CoreResult<NetworkProperties> {
    let bind = match settings.access {
        NetworkAccess::Localhost => ServerBind::Localhost,
        NetworkAccess::Lan => ServerBind::AllInterfaces,
        NetworkAccess::Custom => {
            let address = settings.bind_address.as_deref().ok_or_else(|| {
                ephervex_server::ServerError::InvalidProperty {
                    key: "server-ip".to_owned(),
                    reason: "a custom bind requires an IP address",
                }
            })?;
            if address.contains(['\0', '\n', '\r']) {
                return Err(ephervex_server::ServerError::InvalidProperty {
                    key: "server-ip".to_owned(),
                    reason: "addresses must be single-line",
                }
                .into());
            }
            ServerBind::Custom(address.parse::<IpAddr>().map_err(|_| {
                ephervex_server::ServerError::InvalidProperty {
                    key: "server-ip".to_owned(),
                    reason: "custom bind address is not a valid IP address",
                }
            })?)
        }
    };
    Ok(NetworkProperties {
        online_mode: settings.online_mode,
        bind,
        port: settings.port,
    })
}

fn merge_network_file(path: &Path, network: &ServerNetworkSettings) -> CoreResult<()> {
    let mut document = read_properties(path)?;
    document.merge_network(&network_properties(network)?)?;
    atomic_write(path, document.render().as_bytes())
}

fn emit_network_advisories(
    network: &ServerNetworkSettings,
    observer: &ServerObserver,
) -> CoreResult<()> {
    emit_network_advisories_with(network, observer, |address| {
        TcpListener::bind(address).map(drop)
    })
}

fn emit_network_advisories_with(
    network: &ServerNetworkSettings,
    observer: &ServerObserver,
    check_port: impl FnOnce(SocketAddr) -> std::io::Result<()>,
) -> CoreResult<()> {
    let properties = network_properties(network)?;
    if properties.warns_about_offline_exposure() {
        observer.emit(ephervex_server::ServerEvent::Warning(
            ServerWarning::OfflineModeExposed,
        ));
    }
    let address = match properties.bind {
        ServerBind::Localhost => IpAddr::V4(Ipv4Addr::LOCALHOST),
        ServerBind::AllInterfaces => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ServerBind::Custom(address) => address,
    };
    if let Err(error) = check_port(SocketAddr::new(address, properties.port)) {
        observer.emit(ephervex_server::ServerEvent::Warning(
            ServerWarning::PortUnavailable {
                address: address.to_string(),
                port: properties.port,
                detail: error.to_string(),
            },
        ));
    }
    Ok(())
}

fn read_properties(path: &Path) -> CoreResult<PropertiesDocument> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(PropertiesDocument::parse(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PropertiesDocument::default())
        }
        Err(source) => Err(ephervex_server::ServerError::Io {
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

fn atomic_write(path: &Path, body: &[u8]) -> CoreResult<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|source| ephervex_server::ServerError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| ephervex_server::ServerError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(body)
        .and_then(|()| file.sync_all())
        .map_err(|source| ephervex_server::ServerError::Io {
            path: temporary.clone(),
            source,
        })?;
    std::fs::rename(&temporary, path).map_err(|source| ephervex_server::ServerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn ensure_empty_directory(path: &Path) -> CoreResult<()> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(RegistryError::RootOccupied {
                    path: path.to_path_buf(),
                }
                .into())
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(storage_io(path, source)),
    }
}

fn refuse_if_running(record: &ServerRecord) -> CoreResult<()> {
    if classify_runtime(&record.root)?.may_be_running() {
        return Err(ephervex_server::ServerError::AlreadyRunning.into());
    }
    Ok(())
}

fn build_server_command(
    server_dir: &Path,
    launch: &ServerLaunch,
    java: &crate::JavaPlan,
) -> ServerCommand {
    let mut arguments = vec![
        format!("-Xms{}M", java.jvm.min_memory_mib),
        format!("-Xmx{}M", java.jvm.max_memory_mib),
    ];
    arguments.extend(java.jvm.jvm_args.clone());
    match launch {
        ServerLaunch::Jar { path } => {
            arguments.push("-jar".to_owned());
            arguments.push(path.to_string());
        }
        ServerLaunch::ArgFile { path } => {
            arguments.push(format!("@{path}"));
        }
    }
    arguments.push("nogui".to_owned());
    ServerCommand {
        java: java.runtime.executable.clone(),
        arguments,
        working_directory: server_dir.to_path_buf(),
    }
}

fn server_properties_path(record: &ServerRecord) -> PathBuf {
    ephervex_storage::metadata::server_game_dir(&record.root).join(SERVER_PROPERTIES_FILE)
}

fn staging_root(final_root: &Path) -> PathBuf {
    final_root.with_file_name(format!(
        ".{}.staging-{}",
        final_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("server"),
        Uuid::new_v4()
    ))
}

fn persistence_health(app: &App, record: &ServerRecord) -> ServerPersistenceHealth {
    if duplicate_server_roots(app, record.id).len() > 1 {
        return ServerPersistenceHealth::IdentityConflict;
    }
    if !record.root.is_dir() {
        return ServerPersistenceHealth::MissingRoot;
    }
    match ephervex_storage::metadata::read_server_metadata(&record.root) {
        Ok(canonical) if canonical.id != record.id => ServerPersistenceHealth::IdentityConflict,
        Ok(canonical) if canonical != *record => ServerPersistenceHealth::IndexOutOfSync,
        Ok(_) => ServerPersistenceHealth::Healthy,
        Err(MetadataError::Unreadable { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            ServerPersistenceHealth::MetadataMissing
        }
        Err(MetadataError::UnsupportedVersion { .. }) => {
            ServerPersistenceHealth::MetadataUnsupported
        }
        Err(_) => ServerPersistenceHealth::MetadataCorrupt,
    }
}

fn duplicate_server_roots(app: &App, id: ServerId) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let directory = app.paths().persistent_servers_dir();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return roots;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if ephervex_storage::metadata::read_server_metadata(&path)
            .is_ok_and(|record| record.id == id)
        {
            roots.push(path);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

fn read_capture_snapshot(directory: &Path, limit: usize) -> CoreResult<CaptureSnapshot> {
    let mut files = Vec::new();
    if directory.is_dir() {
        for entry in
            std::fs::read_dir(directory).map_err(|source| ephervex_server::ServerError::Io {
                path: directory.to_path_buf(),
                source,
            })?
        {
            let path = entry
                .map_err(|source| ephervex_server::ServerError::Io {
                    path: directory.to_path_buf(),
                    source,
                })?
                .path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("capture-"))
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut lines = Vec::new();
    for path in files {
        let body = std::fs::read(&path).map_err(|source| ephervex_server::ServerError::Io {
            path: path.clone(),
            source,
        })?;
        for line in body
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if let Ok(captured) = serde_json::from_slice::<CapturedLine>(line) {
                lines.push(captured);
            }
        }
    }
    if lines.len() > limit {
        lines.drain(..lines.len() - limit);
    }
    let retention_path = directory.join(ephervex_server::CAPTURE_INDEX_FILE);
    let retention = std::fs::read(&retention_path)
        .ok()
        .and_then(|body| serde_json::from_slice(&body).ok())
        .unwrap_or_default();
    Ok(CaptureSnapshot {
        lines,
        health: CaptureHealth::Available,
        retention,
    })
}

fn metadata_core_error(error: MetadataError) -> CoreError {
    RegistryError::Metadata(error).into()
}

fn storage_io(path: &Path, source: std::io::Error) -> CoreError {
    RegistryError::Io {
        path: path.to_path_buf(),
        source,
    }
    .into()
}

// Runtime classification has no "unreadable evidence" payload because a
// frontend should only act on the conservative result. This synthetic value is
// never persisted and makes that unknown-running result displayable.
fn synthetic_unverifiable_evidence() -> ephervex_server::ProcessEvidence {
    ephervex_server::ProcessEvidence {
        role: ephervex_server::ProcessRole::Supervisor,
        pid: 0,
        boot_id: String::new(),
        start_ticks: 0,
        executable: PathBuf::new(),
        executable_device: None,
        executable_inode: None,
        started_at: Utc::now(),
    }
}

struct StagingCleanup {
    path: PathBuf,
    armed: std::cell::Cell<bool>,
}

struct InstallationCleanup {
    game_dir: PathBuf,
    source_dir: Option<PathBuf>,
    armed: std::cell::Cell<bool>,
}

impl InstallationCleanup {
    fn new(game_dir: PathBuf, source_dir: Option<PathBuf>) -> Self {
        Self {
            game_dir,
            source_dir,
            armed: std::cell::Cell::new(true),
        }
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for InstallationCleanup {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ = std::fs::remove_dir_all(&self.game_dir);
            if let Some(source_dir) = &self.source_dir {
                let _ = std::fs::remove_dir_all(source_dir);
            }
        }
    }
}

impl StagingCleanup {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            armed: std::cell::Cell::new(true),
        }
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use ephervex_model::LoaderSelection;
    use ephervex_storage::Paths;

    use super::*;

    fn test_app(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("open app")
    }

    #[tokio::test]
    async fn start_without_global_consent_never_writes_eula_true() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let mut server = app
            .registry_for_services()
            .create_server(NewServer::new("test", "1.20.1", LoaderSelection::vanilla()))
            .expect("scaffold");
        server.installation = ServerInstallation::Installed {
            launch: ServerLaunch::Jar {
                path: ServerRelativePath::new("server.jar").expect("relative"),
            },
        };
        app.registry_for_services()
            .update_server(&mut server)
            .expect("update");

        let error = app
            .start_server(server.id, None, &ServerObserver::silent())
            .await
            .expect_err("consent required before Java planning or spawn");
        assert!(matches!(
            error,
            CoreError::Server(ephervex_server::ServerError::EulaNotAccepted)
        ));
        assert!(!server.root.join("server/eula.txt").exists());
    }

    #[test]
    fn accepting_and_revoking_eula_is_global_and_durable() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        assert_eq!(app.server_eula_status(), EulaStatus::NotAccepted);
        assert!(matches!(
            app.accept_server_eula().expect("accept"),
            EulaStatus::Accepted { .. }
        ));
        assert!(matches!(
            test_app(directory.path()).server_eula_status(),
            EulaStatus::Accepted { .. }
        ));
        app.revoke_server_eula().expect("revoke");
        assert_eq!(app.server_eula_status(), EulaStatus::NotAccepted);
    }

    #[tokio::test]
    async fn installation_refuses_to_merge_into_an_occupied_target() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = app
            .registry_for_services()
            .create_server(NewServer::new(
                "occupied",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("scaffold");
        std::fs::write(server.root.join("server/unknown.txt"), "unknown").expect("seed");

        let error = app
            .install_server(
                server.id,
                &ServerCreateOptions::default(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("occupied target must fail before network access");
        assert!(matches!(
            error,
            CoreError::Storage(ephervex_storage::StorageError::Registry(
                RegistryError::RootOccupied { .. }
            ))
        ));
    }

    #[test]
    fn runtime_settings_are_written_to_metadata_before_the_index_copy() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = app
            .registry_for_services()
            .create_server(NewServer::new(
                "runtime settings",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("scaffold");
        let java_override = JavaOverride::new("/opt/jdk/bin/java");
        let jvm_override = JvmSettings {
            min_memory_mib: Some(768),
            max_memory_mib: Some(2048),
            jvm_args: vec!["-Dexample=true".to_owned()],
        };

        let committed = app
            .set_server_runtime_settings(
                server.id,
                Some(java_override.clone()),
                Some(jvm_override.clone()),
            )
            .expect("update runtime settings");
        assert_eq!(committed.record.java_override, Some(java_override.clone()));
        assert_eq!(committed.record.jvm_override, Some(jvm_override.clone()));

        let canonical = ephervex_storage::metadata::read_server_metadata(&server.root)
            .expect("canonical metadata");
        assert_eq!(canonical.java_override, Some(java_override.clone()));
        assert_eq!(canonical.jvm_override, Some(jvm_override.clone()));
        let indexed = app.server(server.id).expect("indexed recovery copy");
        assert_eq!(indexed.java_override, Some(java_override));
        assert_eq!(indexed.jvm_override, Some(jvm_override));
    }

    #[test]
    fn network_advisories_report_exposure_and_a_busy_port() {
        let port = 25_565;
        let (observer, mut events) = ServerObserver::channel();
        let network = ServerNetworkSettings {
            online_mode: false,
            access: NetworkAccess::Custom,
            port,
            bind_address: Some(Ipv4Addr::LOCALHOST.to_string()),
        };

        emit_network_advisories_with(&network, &observer, |address| {
            assert_eq!(address, SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
            Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "reserved by another process",
            ))
        })
        .expect("advisory checks");
        assert!(matches!(
            events.try_recv().expect("exposure warning"),
            ephervex_server::ServerEvent::Warning(ServerWarning::OfflineModeExposed)
        ));
        assert!(matches!(
            events.try_recv().expect("port warning"),
            ephervex_server::ServerEvent::Warning(ServerWarning::PortUnavailable { port: warned, .. })
                if warned == port
        ));
    }

    #[test]
    fn an_imported_client_without_its_retained_recipe_is_refused_before_publication() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let client = app
            .registry_for_services()
            .create_client(
                ephervex_storage::NewClient::new("imported", "1.20.1", LoaderSelection::vanilla())
                    .with_source(ephervex_storage::ImportSource::imported(
                        ImportSourceKind::ModrinthPack,
                        "deleted.mrpack",
                    )),
            )
            .expect("client");
        let mut server = app
            .registry_for_services()
            .prepare_server(NewServer::linked("linked", client.id))
            .expect("prepare");

        let error = app
            .linked_server_recipe(&mut server, &OptionalSelection::none())
            .expect_err("missing desired state is not an empty pack");
        assert!(matches!(
            error,
            CoreError::Pack(PackError::RecipeMissing { .. })
        ));
        assert!(app.list_servers().expect("servers").is_empty());
        assert!(!server.root.exists());
    }

    #[test]
    fn a_failed_staged_creation_rolls_back_every_materialization_stage() {
        let directory = tempfile::tempdir().expect("temp dir");
        for (index, artifact) in [
            "server/minecraft_server.jar",
            "server/mods/managed.jar",
            "server/config/from-server-override.toml",
            ".launcher/source/recipe.json",
        ]
        .into_iter()
        .enumerate()
        {
            let final_root = directory.path().join(format!("server-{index}"));
            let staging = staging_root(&final_root);
            let cleanup = StagingCleanup::new(staging.clone());
            let path = staging.join(artifact);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, b"partial").expect("write partial stage");

            drop(cleanup);
            assert!(
                !staging.exists(),
                "staging survived failure after {artifact}"
            );
            assert!(!final_root.exists(), "partial server was published");
        }
    }

    #[test]
    fn a_failed_install_of_a_scaffolded_linked_server_restores_empty_state() {
        let directory = tempfile::tempdir().expect("temp dir");
        let game = directory.path().join("root/server");
        let source = directory.path().join("root/.launcher/source");
        let cleanup = InstallationCleanup::new(game.clone(), Some(source.clone()));
        std::fs::create_dir_all(&game).expect("game");
        std::fs::create_dir_all(&source).expect("source");
        std::fs::write(game.join("base.jar"), b"base").expect("base");
        std::fs::write(source.join("recipe.json"), b"partial").expect("recipe");

        drop(cleanup);
        assert!(!game.exists());
        assert!(!source.exists());
    }
}
