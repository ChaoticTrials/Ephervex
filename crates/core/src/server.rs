//! Dedicated-server application services composed through `ephervex-server`.

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use chrono::Utc;
use ephervex_download::{CacheLayout, OperationReporter, ProgressSink};
use ephervex_model::{
    AccountKind, AttentionReason, ImportSourceKind, InstanceId, JavaOverride, JvmSettings,
    OperationId, ServerId, ServerInstallation, ServerLaunch, ServerRelativePath,
};
use ephervex_pack::{
    ArtifactFetch, PackError, PackRecipe, ServerMaterializeRequest, SourceStore, UnknownServerFiles,
};
use ephervex_server::{
    CaptureHealth, CaptureRetention, CapturedLine, ControlAction, ControlResponse,
    EulaConsentRecord, EulaStatus, NetworkProperties, OperatorsDocument, PropertiesDocument,
    RuntimeClassification, ServerBind, ServerCommand, ServerObserver, ServerSupervisor,
    ServerWarning, accepted_record, classify_runtime, clear_stale_runtime, materialize_eula_file,
    revoked_record, send_control,
};
use ephervex_storage::{
    FileDisposition, MetadataError, NetworkAccess, NewServer, RegistryError, ServerCommit,
    ServerNetworkSettings, ServerOperatorIdentity, ServerOperatorProfile, ServerOperatorSetup,
    ServerPortSelection, ServerRecord,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::app::{App, JavaPlanRequest, JavaTarget};
use crate::instance::validated_entity_name;
use crate::server_pack::{
    ServerArchiveInstall, ServerContentSource, archive_import_source, launch_reserved,
};
use crate::{CoreError, CoreResult};

const SERVER_PROPERTIES_FILE: &str = "server.properties";
const EULA_FILE: &str = "eula.txt";
const OPERATORS_FILE: &str = "ops.json";
const DEFAULT_SERVER_PORT: u16 = 25_565;
const AUTOMATIC_PORT_CANDIDATES: usize = 100;

#[derive(Debug, Clone, Default)]
pub struct ServerCreateOptions {
    pub java_path: Option<PathBuf>,
    pub install_java_if_missing: bool,
    /// What to do with recipe entries whose server applicability the pack
    /// never declared. Reconstructing them is the default; see
    /// [`UnknownServerFiles`].
    ///
    /// Meaningless when [`Self::content`] names an archive: there is no
    /// reconstruction to make a decision about.
    pub unknown_server_files: UnknownServerFiles,
    /// Where the server's content comes from.
    ///
    /// Reconstruction from the linked client's recipe by default. An archive
    /// published as a pack's server files replaces that entirely, for a linked
    /// and a standalone server alike.
    pub content: ServerContentSource,
    /// Where to report the loader install and the content reconstruction, for
    /// a caller that is showing this work to someone.
    ///
    /// An option rather than a parameter because most callers do not watch:
    /// creating a server from a command line, or from a test, has nobody to
    /// tell. Without one, both stages report into a silent reporter as they
    /// always have.
    pub reporter: Option<OperationReporter>,
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
    /// The server's claim, kept from the moment it was found stopped until its
    /// supervisor owns the runtime lock.
    claim: Option<ephervex_storage::HostClaim>,
}

/// Forwards the downloads a server start makes into the server event stream, so
/// a frontend watches one stream for the whole start.
#[derive(Debug)]
struct ServerDownloadProgress {
    observer: ServerObserver,
}

impl ProgressSink for ServerDownloadProgress {
    fn operation_changed(&self, operation: &ephervex_model::Operation) {
        self.observer
            .emit(ephervex_server::ServerEvent::Downloading(Box::new(
                operation.clone(),
            )));
    }
}

impl App {
    fn resolve_creation_network(&self, request: &mut NewServer) -> CoreResult<()> {
        match request.port_selection {
            ServerPortSelection::Explicit(port) => {
                request.network.port = port;
                let _ = network_properties(&request.network)?;
            }
            ServerPortSelection::Automatic => {
                let defaults = &self.config().server_defaults;
                request.network = ServerNetworkSettings {
                    online_mode: defaults.online_mode,
                    access: defaults.network_access,
                    bind_address: None,
                    port: DEFAULT_SERVER_PORT,
                };
                let properties = network_properties(&request.network)?;
                let address = bind_address(&properties.bind);
                let assigned = self
                    .list_servers()?
                    .into_iter()
                    .map(|server| server.network.port)
                    .collect::<std::collections::HashSet<_>>();
                request.network.port = select_available_port_with(
                    address,
                    DEFAULT_SERVER_PORT,
                    AUTOMATIC_PORT_CANDIDATES,
                    |port| assigned.contains(&port),
                    |socket| TcpListener::bind(socket).map(drop),
                )?;
            }
        }
        Ok(())
    }

    fn default_operator_setup(
        &self,
        network: &ServerNetworkSettings,
    ) -> Option<ServerOperatorSetup> {
        let defaults = &self.config().server_defaults;
        defaults
            .make_default_account_operator
            .then(|| ServerOperatorSetup {
                permission_level: defaults.operator_permission_level,
                profile: self.default_operator_profile(network),
            })
    }

    fn default_operator_profile(
        &self,
        network: &ServerNetworkSettings,
    ) -> Option<ServerOperatorProfile> {
        let (id, account) = self.config().active_account_config()?;
        let (id, identity) = if network.online_mode {
            if account.kind != AccountKind::Microsoft {
                return None;
            }
            (Uuid::parse_str(id).ok()?, ServerOperatorIdentity::Online)
        } else {
            (
                ephervex_auth::offline_uuid(&account.name),
                ServerOperatorIdentity::Offline,
            )
        };
        Some(ServerOperatorProfile {
            id,
            name: account.name.clone(),
            identity,
        })
    }

    /// Builds and installs a complete server in a hidden sibling before the
    /// final root is published.
    pub async fn create_server(
        &self,
        mut request: NewServer,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        // Before the artifact cache is taken, and before any staging root exists.
        request.name = validated_entity_name(&request.name)?;
        self.with_artifact_cache(Box::pin(
            self.create_server_holding_the_cache(request, options, cancel),
        ))
        .await
    }

    async fn create_server_holding_the_cache(
        &self,
        mut request: NewServer,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        self.resolve_creation_network(&mut request)?;
        let mut record = self.registry_for_services().prepare_server(request)?;
        record.operator_setup = self.default_operator_setup(&record.network);
        let linked_recipe = match options.content.archive() {
            // An archive is the server's whole content, so the client's recipe
            // is never read and nothing is reconstructed from it. The exact
            // Minecraft and loader selection still came from the client, by way
            // of `prepare_server`.
            Some(archive) => {
                validate_server_archive(archive)?;
                record.source = archive_import_source(archive);
                None
            }
            None => self.linked_server_recipe(&mut record, &options.unknown_server_files)?,
        };
        let client_source = self.linked_client_source(&record);
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
                    cancel: cancel.clone(),
                    reporter: options.reporter.clone(),
                },
            )
            .await?;
        if let Some(reporter) = options.reporter.as_ref() {
            reporter.set_stage(Some("operation-stage-installing-server".to_owned()));
        }
        let installed = self
            .install_server_version_reporting(
                &record.minecraft_version,
                &record.loader,
                &java.runtime.executable,
                &target,
                options.reporter.as_ref(),
                cancel,
            )
            .await?;
        record.installation = ServerInstallation::Installed {
            launch: normalize_launch(&target, &installed.launch)?,
        };
        let server_source = ephervex_storage::metadata::source_dir(&staging);
        if let Some(archive) = options.content.archive() {
            self.install_server_archive(ServerArchiveInstall {
                archive,
                name: &record.name,
                minecraft_version: &record.minecraft_version,
                loader: &record.loader,
                launch: record.installation.launch(),
                game_dir: &target,
                server_source: &server_source,
                client_source: client_source.as_deref(),
                reporter: options.reporter.as_ref(),
                cancel,
            })
            .await?;
        } else if let Some((client_root, recipe)) = linked_recipe {
            let unconfirmed_unknown = self
                .materialize_linked_server_recipe(
                    &recipe,
                    &client_root,
                    &target,
                    &server_source,
                    record.installation.launch(),
                    options,
                    cancel,
                )
                .await?;
            add_unknown_attention(&mut record, unconfirmed_unknown);
        }
        merge_network_file(&target.join(SERVER_PROPERTIES_FILE), &record.network)?;
        materialize_operator_setup(&target, &record.network, record.operator_setup.as_ref())?;
        let committed = self
            .registry_for_services()
            .publish_staged_server(&record, &staging)?;
        cleanup.disarm();
        self.sync_linked_server_list(&committed.record, None);
        Ok(committed)
    }

    /// Installs a previously scaffolded record.
    pub async fn install_server(
        &self,
        id: ServerId,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        self.with_artifact_cache(Box::pin(
            self.install_server_holding_the_cache(id, options, cancel),
        ))
        .await
    }

    async fn install_server_holding_the_cache(
        &self,
        id: ServerId,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let (_claim, mut record) = self.claim_stopped_server(id)?;
        if record.installation.is_installed() {
            return Ok(ServerCommit::clean(record));
        }
        let target = ephervex_storage::metadata::server_game_dir(&record.root);
        ensure_empty_directory(&target)?;
        let linked_recipe = match options.content.archive() {
            Some(archive) => {
                validate_server_archive(archive)?;
                record.source = archive_import_source(archive);
                None
            }
            None => self.linked_server_recipe(&mut record, &options.unknown_server_files)?,
        };
        let client_source = self.linked_client_source(&record);
        let server_source = ephervex_storage::metadata::source_dir(&record.root);
        let owns_source = linked_recipe.is_some() || options.content.archive().is_some();
        if owns_source {
            ensure_empty_directory(&server_source)?;
        }
        let cleanup =
            InstallationCleanup::new(target.clone(), owns_source.then(|| server_source.clone()));
        let java = self
            .plan_java(
                &record.minecraft_version,
                &JavaPlanRequest {
                    target: JavaTarget::Server(id),
                    explicit_path: options.java_path.clone(),
                    install_if_missing: options.install_java_if_missing,
                    cancel: cancel.clone(),
                    reporter: options.reporter.clone(),
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
        if let Some(archive) = options.content.archive() {
            self.install_server_archive(ServerArchiveInstall {
                archive,
                name: &record.name,
                minecraft_version: &record.minecraft_version,
                loader: &record.loader,
                launch: record.installation.launch(),
                game_dir: &target,
                server_source: &server_source,
                client_source: client_source.as_deref(),
                reporter: options.reporter.as_ref(),
                cancel,
            })
            .await?;
        } else if let Some((client_root, recipe)) = linked_recipe {
            let unconfirmed_unknown = self
                .materialize_linked_server_recipe(
                    &recipe,
                    &client_root,
                    &target,
                    &server_source,
                    record.installation.launch(),
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

    /// Where a linked server's client keeps its retained source.
    ///
    /// `None` for a standalone server, and for a client that has gone missing:
    /// both mean there is no local record to recognize an archive's jars
    /// against, which costs identification and nothing else.
    fn linked_client_source(&self, record: &ServerRecord) -> Option<PathBuf> {
        let client = self.client(record.linked_client_id?).ok()?;
        Some(ephervex_storage::metadata::source_dir(&client.root))
    }

    /// The client's retained recipe, and the client instance's own root.
    ///
    /// The root rather than the retained source directory, because
    /// reconstruction reads both that and the client's game directory: a jar
    /// the user placed in the client themselves is recorded with no address to
    /// fetch it from, so the server takes its copy from the client.
    fn linked_server_recipe(
        &self,
        record: &mut ServerRecord,
        unknown_server_files: &UnknownServerFiles,
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
        let plan = recipe.server_reconstruction_plan(unknown_server_files)?;
        if let Some(layer) = plan.layers.iter().find(|layer| !layer.is_retained()) {
            return Err(PackError::RetainedLayerUnavailable {
                scope: layer.scope.as_str(),
                path: source,
            }
            .into());
        }
        Ok(Some((client.root.clone(), recipe)))
    }

    /// Lays a linked server's content down from the client's retained recipe.
    ///
    /// Crate-visible because it is the seam a server creation is built from:
    /// everything before it is Java and loader installation, and everything the
    /// recipe itself decides happens here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn materialize_linked_server_recipe(
        &self,
        recipe: &PackRecipe,
        client_root: &Path,
        target: &Path,
        server_source: &Path,
        launch: Option<&ServerLaunch>,
        options: &ServerCreateOptions,
        cancel: &CancellationToken,
    ) -> CoreResult<Vec<PathBuf>> {
        let manager = self.artifact_manager()?;
        let reserved = launch_reserved(launch);
        let silent_reporter = OperationReporter::silent("server-content");
        let reporter = options.reporter.as_ref().unwrap_or(&silent_reporter);
        let materialized = ephervex_pack::materialize_server_recipe(ServerMaterializeRequest {
            recipe,
            unknown_server_files: &options.unknown_server_files,
            game_dir: target,
            client_game_dir: &ephervex_storage::metadata::client_game_dir(client_root),
            client_source_dir: &ephervex_storage::metadata::source_dir(client_root),
            server_source_dir: server_source,
            reserved: &reserved,
            fetch: ArtifactFetch {
                manager: &manager,
                cache: &CacheLayout::new(self.paths().cache_dir()),
                // Reconstruction installs from the addresses and digests the
                // recipe already records, and never asks a provider what this
                // instance contains. Whether the *bytes* still need fetching is
                // a separate question: an artifact whose verified copy is in the
                // shared cache is copied straight out of it, and one that has
                // been collected out of it has to come back from its source,
                // which for some providers means authenticating. So a credential
                // is available here and is asked for per file, only when a fetch
                // is unavoidable.
                credentials: &self.provider_credentials(),
                reporter,
                cancel,
            },
        })
        .await;
        let materialized = match materialized {
            Ok(materialized) => materialized,
            Err(source) => {
                let missing = match &source {
                    PackError::FileDownloadsMissing { path } => recipe
                        .files
                        .iter()
                        .find(|file| file.display_name() == *path)
                        .map(|file| file.logical_path.clone()),
                    _ => None,
                };
                let error = CoreError::from(source);
                let Some(logical_path) = missing else {
                    return Err(error);
                };
                return Err(self
                    .as_recorded_manual_acquisition(error, recipe, &logical_path, reporter, cancel)
                    .await);
            }
        };
        // A cancel that lands after the last pack step must still stop the
        // server from being published.
        if cancel.is_cancelled() {
            return Err(PackError::Canceled.into());
        }
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
        let (_claim, record) = self.claim_stopped_server(id)?;
        self.delete_claimed_server(&record, files)
    }

    /// The deletion itself, for a caller that holds the server's claim and has
    /// found it stopped.
    pub(crate) fn delete_claimed_server(
        &self,
        record: &ServerRecord,
        files: FileDisposition,
    ) -> CoreResult<()> {
        self.forget_linked_server_list_entry(record);
        Ok(self
            .registry_for_services()
            .delete_server(record.id, files)?)
    }

    /// Stops every running linked server of a client that is about to be deleted.
    ///
    /// A server whose files are kept is stopped cleanly where it allows it, and
    /// killed otherwise. A server that cannot be stopped through its supervisor
    /// refuses before any server is stopped. A stop that fails refuses too, and
    /// the other servers stay stopped.
    pub(crate) async fn stop_linked_servers_for_deletion(
        &self,
        client: InstanceId,
        files: FileDisposition,
    ) -> CoreResult<()> {
        let mut running = Vec::new();
        for id in self.linked_server_ids(client)? {
            let record = self.canonical_server_for_mutation(id)?;
            let runtime = settled_runtime(&record.root)?;
            if stoppable_for_deletion(&runtime) {
                running.push(record);
            } else if runtime.may_be_running() {
                return Err(CoreError::LinkedServerUnstoppable { name: record.name });
            }
        }
        // Concurrent, and every stop runs to its end, so a failure leaves none half done.
        futures_util::future::join_all(
            running
                .iter()
                .map(|record| stop_then_kill(record, files == FileDisposition::Keep)),
        )
        .await
        .into_iter()
        .collect()
    }

    pub fn server_properties(&self, id: ServerId) -> CoreResult<PropertiesDocument> {
        let record = self.canonical_server_for_mutation(id)?;
        read_properties(&server_properties_path(&record))
    }

    pub fn set_server_property(&self, id: ServerId, key: &str, value: &str) -> CoreResult<()> {
        let (_claim, record) = self.claim_stopped_server(id)?;
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
        let (_claim, mut record) = self.claim_stopped_server(id)?;
        let typed = network_properties(&network)?;
        typed.validate()?;
        merge_network_file(&server_properties_path(&record), &network)?;
        let previous = record.clone();
        record.network = network;
        let committed = self
            .registry_for_services()
            .update_server_committed(&mut record)?;
        self.sync_linked_server_list(&committed.record, Some(&previous));
        Ok(committed)
    }

    /// Applies only the given network options to the stored settings. Options
    /// left unset keep their stored values.
    pub fn update_server_network(
        &self,
        id: ServerId,
        change: &ServerNetworkChange,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let (_claim, mut record) = self.claim_stopped_server(id)?;
        let network = change.applied_to(&record.network)?;
        network_properties(&network)?.validate()?;
        merge_network_file(&server_properties_path(&record), &network)?;
        let previous = record.clone();
        record.network = network;
        let committed = self
            .registry_for_services()
            .update_server_committed(&mut record)?;
        self.sync_linked_server_list(&committed.record, Some(&previous));
        Ok(committed)
    }

    /// Replaces the server-specific Java and JVM overrides as one canonical
    /// metadata mutation. `None` restores the corresponding global setting.
    pub fn set_server_runtime_settings(
        &self,
        id: ServerId,
        java_override: Option<JavaOverride>,
        jvm_override: Option<JvmSettings>,
    ) -> CoreResult<ServerCommit<ServerRecord>> {
        let (_claim, mut record) = self.claim_stopped_server(id)?;
        self.validate_jvm_override(jvm_override.as_ref())?;
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
            .prepare_server_start(id, explicit_java, observer, &CancellationToken::new())
            .await?;
        Self::run_prepared_server(prepared, observer).await
    }

    /// Starts in the foreground like [`App::start_server`], and ends the start
    /// when `cancel` fires.
    ///
    /// Canceled while preparing, nothing was started and the start fails with a
    /// cancellation. Canceled once the server runs, the server is stopped
    /// gracefully (killed only if it will not stop) and the result describes how
    /// it ended, with `stop_requested` set.
    pub async fn start_server_until_canceled(
        &self,
        id: ServerId,
        explicit_java: Option<PathBuf>,
        observer: &ServerObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<ephervex_server::ServerExit> {
        let prepared = Self::unless_canceled(
            self.prepare_server_start(id, explicit_java, observer, cancel),
            observer,
            cancel,
        )
        .await?;
        Self::supervise_until_canceled(id, prepared, observer, cancel).await
    }

    /// Drops an unfinished preparation when `cancel` fires, so nothing starts.
    async fn unless_canceled(
        preparation: impl std::future::Future<Output = CoreResult<PreparedServerStart>>,
        observer: &ServerObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<PreparedServerStart> {
        tokio::select! {
            prepared = preparation => prepared,
            () = cancel.cancelled() => {
                observer.emit(ephervex_server::ServerEvent::Finished { exit: None });
                Err(ephervex_server::ServerError::StartCanceled.into())
            }
        }
    }

    async fn supervise_until_canceled(
        id: ServerId,
        prepared: PreparedServerStart,
        observer: &ServerObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<ephervex_server::ServerExit> {
        let root = prepared.root.clone();
        let supervised_observer = observer.clone();
        let mut supervisor =
            tokio::spawn(
                async move { Self::run_prepared_server(prepared, &supervised_observer).await },
            );
        let ended = tokio::select! {
            ended = &mut supervisor => Some(ended),
            () = cancel.cancelled() => None,
        };
        let ended = match ended {
            Some(ended) => Some(ended),
            None => crate::connection::stop_started_server(&root, supervisor).await,
        };
        match ended {
            Some(Ok(exit)) => exit,
            Some(Err(_)) | None => {
                Err(crate::connection::ConnectError::SupervisorLost { server: id }.into())
            }
        }
    }

    pub(super) async fn prepare_server_start(
        &self,
        id: ServerId,
        explicit_java: Option<PathBuf>,
        observer: &ServerObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<PreparedServerStart> {
        // Created before the span so every line the preparation logs carries
        // its id. Downloads the preparation makes report through the observer.
        let reporter = OperationReporter::new(
            "operation-starting-server",
            true,
            std::sync::Arc::new(ServerDownloadProgress {
                observer: observer.clone(),
            }),
        );
        let result = self
            .prepare_server_start_operation(id, explicit_java, observer, cancel, &reporter)
            .await;
        if result.is_err() {
            observer.emit(ephervex_server::ServerEvent::Finished { exit: None });
        }
        result
    }

    #[tracing::instrument(
        name = "prepare_server_start",
        skip(self, explicit_java, observer, cancel, reporter),
        fields(server = %id, operation = %reporter.id())
    )]
    async fn prepare_server_start_operation(
        &self,
        id: ServerId,
        explicit_java: Option<PathBuf>,
        observer: &ServerObserver,
        cancel: &CancellationToken,
        reporter: &OperationReporter,
    ) -> CoreResult<PreparedServerStart> {
        let operation = reporter.id();
        observer.emit(ephervex_server::ServerEvent::Preparing { operation });
        // Begun so downloads reported under it read as running, not queued.
        reporter.begin_stage("operation-stage-preparing-environment".to_owned());

        // Held until the supervisor owns the runtime lock, which is the first
        // thing that keeps another change out once the files below are written.
        let (claim, mut record) = self.claim_stopped_server(id)?;
        let launch = record
            .installation
            .launch()
            .ok_or(ephervex_server::ServerError::NotInstalled)?
            .clone();
        let eula = self.server_eula_status();
        eula.require_valid()?;
        let server_dir = ephervex_storage::metadata::server_game_dir(&record.root);
        merge_network_file(&server_dir.join(SERVER_PROPERTIES_FILE), &record.network)?;
        let incompatible_profile = record.operator_setup.as_ref().is_some_and(|setup| {
            setup.profile.as_ref().is_some_and(|profile| {
                !operator_profile_matches_network(profile.identity, &record.network)
            })
        });
        let needs_profile = record
            .operator_setup
            .as_ref()
            .is_some_and(|setup| setup.profile.is_none() || incompatible_profile);
        if needs_profile {
            let profile = self.default_operator_profile(&record.network);
            let should_persist = profile.is_some() || incompatible_profile;
            if let Some(setup) = record.operator_setup.as_mut() {
                setup.profile = profile;
            }
            if should_persist {
                let _commit = self
                    .registry_for_services()
                    .update_server_committed(&mut record)?;
            }
        }
        materialize_operator_setup(&server_dir, &record.network, record.operator_setup.as_ref())?;
        materialize_eula_file(&server_dir.join(EULA_FILE), &eula)?;
        emit_network_advisories(&record.network, observer)?;
        let java = self
            .plan_java(
                &record.minecraft_version,
                &JavaPlanRequest {
                    target: JavaTarget::Server(id),
                    explicit_path: explicit_java,
                    install_if_missing: true,
                    cancel: cancel.clone(),
                    reporter: Some(reporter.clone()),
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
            claim: Some(claim),
        })
    }

    pub(super) async fn run_prepared_server(
        prepared: PreparedServerStart,
        observer: &ServerObserver,
    ) -> CoreResult<ephervex_server::ServerExit> {
        let operation = prepared.operation;
        let exit = ServerSupervisor::run_holding(
            prepared.claim,
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
        observer.emit(ephervex_server::ServerEvent::Finished {
            exit: exit.as_ref().ok().copied(),
        });
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

    /// Stops a server through its supervisor and kills it when it will not
    /// stop, for a caller whose own runtime is about to end.
    pub async fn stop_server_or_kill(&self, id: ServerId) -> CoreResult<()> {
        let record = self.server(id)?;
        stop_then_kill(&record, true).await
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

    /// The newest `limit` captured lines across every retained run.
    pub fn server_output(&self, id: ServerId, limit: usize) -> CoreResult<CaptureSnapshot> {
        let record = self.server(id)?;
        read_capture_snapshot(
            &self.paths().server_capture_dir(record.id),
            limit,
            CaptureScope::Retained,
        )
    }

    /// The newest `limit` captured lines since the server was last started or
    /// restarted, leaving out the runs before it.
    pub fn server_latest_run_output(
        &self,
        id: ServerId,
        limit: usize,
    ) -> CoreResult<CaptureSnapshot> {
        let record = self.server(id)?;
        read_capture_snapshot(
            &self.paths().server_capture_dir(record.id),
            limit,
            CaptureScope::LatestRun,
        )
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

    /// Records `reason` on a server's canonical metadata.
    ///
    /// Callers sit on recovery paths and decide what a refusal means for the
    /// result they already hold, so this reports one rather than replacing the
    /// canonical file with the index copy.
    pub(super) fn add_server_attention(
        &self,
        id: ServerId,
        reason: AttentionReason,
    ) -> CoreResult<()> {
        let mut record = self.canonical_server_for_mutation(id)?;
        record.add_attention(reason);
        self.registry_for_services()
            .update_server_committed(&mut record)?;
        Ok(())
    }

    pub(super) fn server_connection_target(
        record: &ServerRecord,
    ) -> CoreResult<ephervex_model::ConnectionTarget> {
        Ok(network_properties(&record.network)?.connection_target()?)
    }

    /// Adds or updates `record`'s entry in its linked client's
    /// `servers.dat`, so the client's own Multiplayer screen already names
    /// this exact server without anyone typing its address in by hand.
    ///
    /// `previous` is the record before the change being synced, so an entry
    /// the game has rewritten without its tag is still found by the name and
    /// address it showed. If the change moved the server to another client, or
    /// unlinked it, the entry is removed from the client it left.
    ///
    /// A no-op for a standalone server, or one whose client cannot currently
    /// be read: there is nothing to update against. Never fails the caller;
    /// see [`crate::linked_server_list`].
    pub(super) fn sync_linked_server_list(
        &self,
        record: &ServerRecord,
        previous: Option<&ServerRecord>,
    ) {
        if let Some(previous) = previous
            && previous.linked_client_id.is_some()
            && previous.linked_client_id != record.linked_client_id
        {
            self.forget_linked_server_list_entry(previous);
        }
        let Some(client_id) = record.linked_client_id else {
            return;
        };
        let Ok(client) = self.client(client_id) else {
            return;
        };
        let Some(current) = Self::listed_server(record) else {
            return;
        };
        let previous = previous
            .filter(|previous| previous.linked_client_id == record.linked_client_id)
            .and_then(Self::listed_server);
        let game_dir = ephervex_storage::metadata::client_game_dir(&client.root);
        crate::linked_server_list::sync_entry(&game_dir, record.id, &current, previous.as_ref());
    }

    /// Re-syncs the entry of every server linked to `client`, for a client
    /// whose `servers.dat` was replaced wholesale.
    pub(super) fn sync_linked_server_lists_of_client(&self, client: InstanceId) {
        let Ok(servers) = self.list_servers() else {
            return;
        };
        for server in servers
            .iter()
            .filter(|server| server.linked_client_id == Some(client))
        {
            self.sync_linked_server_list(server, None);
        }
    }

    /// Removes `record`'s entry from its linked client's `servers.dat`, the
    /// counterpart to [`Self::sync_linked_server_list`] called when the
    /// server itself goes away.
    pub(super) fn forget_linked_server_list_entry(&self, record: &ServerRecord) {
        let Some(client_id) = record.linked_client_id else {
            return;
        };
        let Ok(client) = self.client(client_id) else {
            return;
        };
        let Some(listed) = Self::listed_server(record) else {
            return;
        };
        let game_dir = ephervex_storage::metadata::client_game_dir(&client.root);
        crate::linked_server_list::remove_entry(&game_dir, record.id, &listed);
    }

    fn listed_server(record: &ServerRecord) -> Option<crate::linked_server_list::ListedServer> {
        let target = Self::server_connection_target(record).ok()?;
        Some(crate::linked_server_list::ListedServer {
            name: record.name.clone(),
            address: target.to_string(),
        })
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

/// Refuses an archive the install would refuse before any expensive work starts.
///
/// Java probing and a loader installation take minutes and megabytes. Checking
/// the archive first means "that file is not a readable ZIP" or "an entry name
/// is unsafe" is answered in milliseconds, which is the same reason
/// `linked_server_recipe` validates a recipe before this point.
fn validate_server_archive(archive: &Path) -> CoreResult<()> {
    ephervex_pack::validate_server_pack(archive, ephervex_pack::ExtractionLimits::default())?;
    Ok(())
}

fn add_unknown_attention(record: &mut ServerRecord, paths: Vec<PathBuf>) {
    for path in paths {
        record.add_attention(AttentionReason::UnknownServerApplicability(
            path.to_string_lossy().into_owned(),
        ));
    }
}

/// A partial change to a server's network settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerNetworkChange {
    pub online_mode: Option<bool>,
    pub access: Option<NetworkAccess>,
    pub port: Option<u16>,
    /// Requires the resulting access to be [`NetworkAccess::Custom`].
    pub bind_address: Option<String>,
}

impl ServerNetworkChange {
    fn applied_to(&self, current: &ServerNetworkSettings) -> CoreResult<ServerNetworkSettings> {
        let access = self.access.unwrap_or(current.access);
        let bind_address = match (access, &self.bind_address) {
            (NetworkAccess::Custom, Some(address)) => Some(address.clone()),
            (NetworkAccess::Custom, None) => current.bind_address.clone(),
            (_, Some(_)) => {
                return Err(ephervex_server::ServerError::InvalidProperty {
                    key: "server-ip".to_owned(),
                    reason: "a bind address requires custom access",
                }
                .into());
            }
            (_, None) => None,
        };
        Ok(ServerNetworkSettings {
            online_mode: self.online_mode.unwrap_or(current.online_mode),
            access,
            port: self.port.unwrap_or(current.port),
            bind_address,
        })
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

const fn bind_address(bind: &ServerBind) -> IpAddr {
    match bind {
        ServerBind::Localhost => IpAddr::V4(Ipv4Addr::LOCALHOST),
        ServerBind::AllInterfaces => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ServerBind::Custom(address) => *address,
    }
}

fn select_available_port_with(
    address: IpAddr,
    start: u16,
    candidates: usize,
    is_assigned: impl Fn(u16) -> bool,
    mut try_bind: impl FnMut(SocketAddr) -> std::io::Result<()>,
) -> CoreResult<u16> {
    for offset in 0..candidates {
        let Some(port) = u16::try_from(offset)
            .ok()
            .and_then(|offset| start.checked_add(offset))
        else {
            break;
        };
        if port == 0 || is_assigned(port) {
            continue;
        }
        // The listener is deliberately dropped by the caller's bind check.
        // Another process can claim the port before Minecraft starts, so the
        // normal start-time advisory remains authoritative for that race.
        if try_bind(SocketAddr::new(address, port)).is_ok() {
            return Ok(port);
        }
    }
    Err(ephervex_server::ServerError::InvalidProperty {
        key: "server-port".to_owned(),
        reason: "no available port was found in the automatic search range",
    }
    .into())
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
    let address = bind_address(&properties.bind);
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

fn materialize_operator_setup(
    server_dir: &Path,
    network: &ServerNetworkSettings,
    setup: Option<&ServerOperatorSetup>,
) -> CoreResult<()> {
    let Some(setup) = setup else {
        return Ok(());
    };
    let Some(profile) = setup.profile.as_ref() else {
        return Ok(());
    };
    if !operator_profile_matches_network(profile.identity, network) {
        return Ok(());
    }
    let path = server_dir.join(OPERATORS_FILE);
    let mut document = match std::fs::read_to_string(&path) {
        Ok(text) => {
            OperatorsDocument::parse(&text).map_err(|source| ephervex_server::ServerError::Io {
                path: path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => OperatorsDocument::default(),
        Err(source) => {
            return Err(ephervex_server::ServerError::Io {
                path: path.clone(),
                source,
            }
            .into());
        }
    };
    document.upsert(profile.id, &profile.name, setup.permission_level);
    let rendered = document
        .render()
        .map_err(|source| ephervex_server::ServerError::Io {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
        })?;
    atomic_write(&path, rendered.as_bytes())
}

const fn operator_profile_matches_network(
    identity: ServerOperatorIdentity,
    network: &ServerNetworkSettings,
) -> bool {
    matches!(
        (identity, network.online_mode),
        (ServerOperatorIdentity::Offline, false) | (ServerOperatorIdentity::Online, true)
    )
}

fn read_properties(path: &Path) -> CoreResult<PropertiesDocument> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            PropertiesDocument::parse(&text).map_err(|malformed| malformed.in_file(path).into())
        }
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

/// How long a runtime may stay uncertain before it is believed. A server holds
/// its lock for a moment while it starts and while it stops.
pub(crate) const RUNTIME_SETTLE: std::time::Duration = std::time::Duration::from_secs(1);

/// The runtime once a starting or stopping server has had a moment to settle.
pub(crate) fn settled_runtime(root: &Path) -> CoreResult<RuntimeClassification> {
    let deadline = std::time::Instant::now() + RUNTIME_SETTLE;
    loop {
        let runtime = classify_runtime(root)?;
        if !matches!(runtime, RuntimeClassification::UnknownRunning { .. })
            || std::time::Instant::now() >= deadline
        {
            return Ok(runtime);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Stops one server that has to go away. A clean stop is tried when `graceful`,
/// and one that is refused or runs out of time becomes a kill.
async fn stop_then_kill(record: &ServerRecord, graceful: bool) -> CoreResult<()> {
    if graceful
        && matches!(
            send_control(&record.root, &ControlAction::Stop).await,
            Ok(ControlResponse::Exited(_))
        )
    {
        return Ok(());
    }
    let outcome = send_control(&record.root, &ControlAction::Kill).await;
    if matches!(outcome, Ok(ControlResponse::Exited(_)))
        || !settled_runtime(&record.root)?.may_be_running()
    {
        return Ok(());
    }
    Err(CoreError::LinkedServerNotStopped {
        name: record.name.clone(),
        detail: match outcome {
            Ok(ControlResponse::Refused(reason) | ControlResponse::StartFailed(reason)) => reason,
            Ok(_) => "the server answered without stopping".to_owned(),
            Err(error) => error.to_string(),
        },
    })
}

/// Whether deleting a server's client may stop it: only through a supervisor
/// that answers. A server that may be running otherwise blocks the deletion.
#[must_use]
pub const fn stoppable_for_deletion(runtime: &RuntimeClassification) -> bool {
    runtime.is_controlled()
}

pub(crate) fn refuse_if_running(record: &ServerRecord) -> CoreResult<()> {
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

/// Which retained capture a snapshot reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureScope {
    Retained,
    LatestRun,
}

fn read_capture_snapshot(
    directory: &Path,
    limit: usize,
    scope: CaptureScope,
) -> CoreResult<CaptureSnapshot> {
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
    // Retention removes the oldest segments first, so a run whose first
    // segment is still here lost nothing, whatever earlier runs lost.
    let mut latest_run_is_whole = false;
    if scope == CaptureScope::LatestRun
        && let Some(start) = files.iter().rposition(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(ephervex_server::segment_continues_run)
        })
    {
        files.drain(..start);
        latest_run_is_whole = true;
    }
    let mut lines = Vec::with_capacity(limit.min(2_000));
    'segments: for path in files.into_iter().rev() {
        if lines.len() == limit {
            break;
        }
        let body = std::fs::read(&path).map_err(|source| ephervex_server::ServerError::Io {
            path: path.clone(),
            source,
        })?;
        for line in body
            .rsplit(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            if let Ok(captured) = serde_json::from_slice::<CapturedLine>(line) {
                lines.push(captured);
                if lines.len() == limit {
                    break 'segments;
                }
            }
        }
    }
    lines.reverse();
    let retention_path = directory.join(ephervex_server::CAPTURE_INDEX_FILE);
    let retention = if latest_run_is_whole {
        CaptureRetention::default()
    } else {
        std::fs::read(&retention_path)
            .ok()
            .and_then(|body| serde_json::from_slice(&body).ok())
            .unwrap_or_default()
    };
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
    use fastnbt::Value;

    use super::*;

    fn server_archive(path: &Path, entries: &[&str]) {
        use std::io::Write as _;

        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).expect("create"));
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for name in entries {
            writer.start_file(*name, options).expect("start entry");
            writer.write_all(b"x").expect("write entry");
        }
        writer.finish().expect("finish");
    }

    #[test]
    fn a_server_archive_with_an_entry_name_extraction_refuses_is_refused_up_front() {
        let directory = tempfile::tempdir().expect("temp dir");
        let archive = directory.path().join("server.zip");
        server_archive(&archive, &["mods/one.jar", "docs\\readme.txt"]);

        let error = validate_server_archive(&archive).expect_err("unsafe entry name");

        assert!(matches!(
            error,
            CoreError::Pack(ephervex_pack::PackError::EntryPathUnsafe { .. })
        ));
    }

    #[test]
    fn a_well_formed_server_archive_passes_validation() {
        let directory = tempfile::tempdir().expect("temp dir");
        let archive = directory.path().join("server.zip");
        server_archive(&archive, &["server/mods/one.jar", "server/config/a.toml"]);

        validate_server_archive(&archive).expect("valid");
    }

    #[test]
    fn a_properties_file_java_cannot_load_is_refused_and_left_untouched() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(SERVER_PROPERTIES_FILE);
        std::fs::write(&path, b"motd=ok\nlevel-name=bad\\u12G4\n").expect("seed");

        let merge = merge_network_file(&path, &ServerNetworkSettings::default());
        assert!(matches!(
            merge,
            Err(CoreError::Server(
                ephervex_server::ServerError::MalformedPropertiesEscape { line: 2, .. }
            ))
        ));
        assert!(matches!(
            read_properties(&path),
            Err(CoreError::Server(
                ephervex_server::ServerError::MalformedPropertiesEscape { line: 2, .. }
            ))
        ));
        let error = read_properties(&path).expect_err("malformed escape");
        assert_eq!(error.failing_path().as_deref(), Some(path.as_path()));
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"motd=ok\nlevel-name=bad\\u12G4\n"
        );
    }

    #[tokio::test]
    async fn server_observers_receive_one_terminal_result_for_the_complete_run() {
        for code in [0, 7] {
            let root = tempfile::tempdir().expect("root");
            let (observer, mut events) = ServerObserver::channel();
            let prepared = PreparedServerStart {
                command: ephervex_server::ServerCommand {
                    java: PathBuf::from("/bin/sh"),
                    arguments: vec!["-c".to_owned(), format!("exit {code}")],
                    working_directory: root.path().join("server"),
                },
                root: root.path().to_path_buf(),
                capture: root.path().join("capture"),
                operation: OperationReporter::silent("test-server-run").id(),
                claim: None,
            };
            let exit = App::run_prepared_server(prepared, &observer)
                .await
                .expect("run");
            assert_eq!(exit.code, Some(code));
            let events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
            assert!(
                matches!(events.last(), Some(ephervex_server::ServerEvent::Finished { exit: Some(exit) }) if exit.code == Some(code))
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, ephervex_server::ServerEvent::Finished { .. }))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn downloads_made_while_preparing_a_start_reach_the_observer_under_the_start_operation() {
        let (observer, mut events) = ServerObserver::channel();
        let reporter = OperationReporter::new(
            "operation-starting-server",
            true,
            std::sync::Arc::new(ServerDownloadProgress { observer }),
        );

        reporter.begin_stage("operation-stage-preparing-environment".to_owned());

        let Ok(ephervex_server::ServerEvent::Downloading(snapshot)) = events.try_recv() else {
            panic!("the download was not reported through the observer");
        };
        assert_eq!(snapshot.id, reporter.id());
        assert_eq!(
            snapshot.stage.as_deref(),
            Some("operation-stage-preparing-environment")
        );
    }

    #[tokio::test]
    async fn a_start_reports_its_operation_as_running_with_a_stage_before_any_download() {
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
        let (observer, mut events) = ServerObserver::channel();

        app.prepare_server_start(server.id, None, &observer, &CancellationToken::new())
            .await
            .err()
            .expect("consent is required, so the start stops before Java planning");

        let events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        let Some(ephervex_server::ServerEvent::Preparing { operation }) = events.first() else {
            panic!("the start did not announce itself first: {events:?}");
        };
        let Some(ephervex_server::ServerEvent::Downloading(snapshot)) = events.get(1) else {
            panic!("the operation was never begun: {events:?}");
        };
        assert_eq!(snapshot.id, *operation);
        assert_eq!(snapshot.status, ephervex_model::OperationStatus::Running);
        assert_eq!(
            snapshot.stage.as_deref(),
            Some("operation-stage-preparing-environment")
        );
    }

    fn stand_in_prepared(root: &Path, script: &str) -> PreparedServerStart {
        PreparedServerStart {
            command: ephervex_server::ServerCommand {
                java: PathBuf::from("/bin/sh"),
                arguments: vec!["-c".to_owned(), script.to_owned()],
                working_directory: root.join("server"),
            },
            root: root.to_path_buf(),
            capture: root.join("capture"),
            operation: OperationReporter::silent("test-server-cancel").id(),
            claim: None,
        }
    }

    #[tokio::test]
    async fn canceling_a_preparation_that_has_not_finished_starts_nothing() {
        let (observer, mut events) = ServerObserver::channel();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = App::unless_canceled(std::future::pending(), &observer, &cancel).await;

        let error = result.err().expect("canceled");
        assert!(error.is_cancellation(), "{error}");
        assert!(matches!(
            events.try_recv(),
            Ok(ephervex_server::ServerEvent::Finished { exit: None })
        ));
    }

    #[tokio::test]
    async fn canceling_a_running_server_start_stops_it_gracefully_and_reports_a_requested_stop() {
        let root = tempfile::tempdir().expect("root");
        let (observer, mut events) = ServerObserver::channel();
        let cancel = CancellationToken::new();
        let prepared = stand_in_prepared(
            root.path(),
            "echo 'Done (1s)!'; while read -r line; do [ \"$line\" = stop ] && exit 0; done",
        );
        let run = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                App::supervise_until_canceled(ServerId::new(), prepared, &observer, &cancel).await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                if matches!(event, ephervex_server::ServerEvent::Started { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("started");

        cancel.cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(20), run)
            .await
            .expect("the stop was answered without waiting for the control timeout")
            .expect("join")
            .expect("exit");

        assert!(exit.stop_requested);
        assert_eq!(exit.code, Some(0));
    }

    #[tokio::test]
    async fn a_server_start_that_ends_on_its_own_reports_its_own_exit() {
        let root = tempfile::tempdir().expect("root");
        let (observer, _events) = ServerObserver::channel();
        let prepared = stand_in_prepared(root.path(), "exit 3");

        let exit = App::supervise_until_canceled(
            ServerId::new(),
            prepared,
            &observer,
            &CancellationToken::new(),
        )
        .await
        .expect("exit");

        assert_eq!(exit.code, Some(3));
        assert!(!exit.stop_requested);
        assert!(!exit.finished_starting);
    }

    #[tokio::test]
    async fn restarting_a_server_emits_no_terminal_result_until_the_replacement_stops() {
        let root = tempfile::tempdir().expect("root");
        let server_root = root.path().to_path_buf();
        let (observer, mut events) = ServerObserver::channel();
        let prepared = PreparedServerStart {
            command: ephervex_server::ServerCommand {
                java: PathBuf::from("/bin/sh"),
                arguments: vec![
                    "-c".to_owned(),
                    "while read -r line; do [ \"$line\" = stop ] && exit 0; done".to_owned(),
                ],
                working_directory: root.path().join("server"),
            },
            root: server_root.clone(),
            capture: root.path().join("capture"),
            operation: OperationReporter::silent("test-server-restart").id(),
            claim: None,
        };
        let run = tokio::spawn(async move { App::run_prepared_server(prepared, &observer).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                if matches!(event, ephervex_server::ServerEvent::Started { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("started");
        assert!(matches!(
            send_control(&server_root, &ControlAction::Restart)
                .await
                .expect("restart"),
            ControlResponse::Restarted { .. }
        ));
        let before_stop: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(
            !before_stop
                .iter()
                .any(|event| matches!(event, ephervex_server::ServerEvent::Finished { .. }))
        );
        assert!(matches!(
            send_control(&server_root, &ControlAction::Stop)
                .await
                .expect("stop"),
            ControlResponse::Exited(_)
        ));
        run.await.expect("join").expect("run");
        let after_stop: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(
            after_stop
                .iter()
                .filter(|event| matches!(event, ephervex_server::ServerEvent::Finished { .. }))
                .count(),
            1
        );
    }

    fn claimed_by_another_operation(
        app: &App,
        server: &ServerRecord,
    ) -> ephervex_storage::HostClaim {
        app.registry_for_services()
            .claim_host(ephervex_storage::RecordRef::Server(server.id))
            .expect("claim")
    }

    #[test]
    fn changes_to_a_server_are_refused_while_another_operation_holds_its_claim() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let _start = claimed_by_another_operation(&app, &server);

        let settings = app
            .set_server_runtime_settings(server.id, None, None)
            .expect_err("settings");
        let property = app
            .set_server_property(server.id, "motd", "changed")
            .expect_err("property");
        let deletion = app
            .delete_server(server.id, FileDisposition::Remove)
            .expect_err("deletion");

        for error in [settings, property, deletion] {
            assert_eq!(error.message_id(), "error-registry-host-busy", "{error}");
        }
        assert!(server.root.is_dir(), "the refused deletion removed nothing");
    }

    #[tokio::test]
    async fn a_server_start_is_refused_while_another_operation_holds_its_claim() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let _deletion = claimed_by_another_operation(&app, &server);

        let error = app
            .start_server(server.id, None, &ServerObserver::silent())
            .await
            .expect_err("a server being changed is not started");

        assert_eq!(error.message_id(), "error-registry-host-busy", "{error}");
    }

    #[tokio::test]
    async fn a_prepared_start_keeps_its_claim_until_the_supervisor_owns_the_runtime_lock() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        std::fs::create_dir_all(server.root.join("server")).expect("server directory");
        let mut prepared = stand_in_prepared(
            &server.root,
            "while read -r line; do [ \"$line\" = stop ] && exit 0; done",
        );
        prepared.claim = Some(claimed_by_another_operation(&app, &server));
        assert!(
            app.registry_for_services()
                .claim_host(ephervex_storage::RecordRef::Server(server.id))
                .is_err(),
            "the prepared start holds the server"
        );
        let (observer, mut events) = ServerObserver::channel();
        let run = tokio::spawn(async move { App::run_prepared_server(prepared, &observer).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                if matches!(event, ephervex_server::ServerEvent::Started { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("started");

        drop(
            app.registry_for_services()
                .claim_host(ephervex_storage::RecordRef::Server(server.id))
                .expect("the runtime lock keeps changes out now, not the claim"),
        );
        let error = app
            .set_server_property(server.id, "motd", "changed")
            .expect_err("a running server is not changed");
        assert_eq!(
            error.message_id(),
            "error-server-already-running",
            "{error}"
        );

        send_control(&server.root, &ControlAction::Stop)
            .await
            .expect("stop");
        run.await.expect("join").expect("run");
    }

    #[tokio::test]
    async fn a_server_preparation_failure_finishes_its_observer() {
        let root = tempfile::tempdir().expect("root");
        let app = App::open_at(ephervex_storage::Paths::rooted_at(root.path())).expect("app");
        let (observer, mut events) = ServerObserver::channel();
        assert!(
            app.start_server(ServerId::new(), None, &observer)
                .await
                .is_err()
        );
        let events: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert!(matches!(
            events.last(),
            Some(ephervex_server::ServerEvent::Finished { exit: None })
        ));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ephervex_server::ServerEvent::Finished { .. }))
                .count(),
            1
        );
    }

    fn test_app(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("open app")
    }

    fn remember_active_account(app: &mut App, id: &str, name: &str, kind: AccountKind) {
        app.mutate_config(|config| {
            config.remember_account(id, name, kind, "2026-01-01T00:00:00Z");
            assert!(config.set_active_account(id));
        })
        .expect("config");
    }

    #[test]
    fn automatic_port_prefers_start_and_advances_past_unavailable_candidates() {
        let selected = select_available_port_with(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            25_565,
            8,
            |port| port == 25_565,
            |address| {
                if address.port() == 25_566 {
                    Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "busy"))
                } else {
                    Ok(())
                }
            },
        )
        .expect("candidate");
        assert_eq!(selected, 25_567);

        let preferred = select_available_port_with(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            25_565,
            1,
            |_| false,
            |_| Ok(()),
        )
        .expect("preferred");
        assert_eq!(preferred, 25_565);
    }

    #[test]
    fn automatic_port_search_is_bounded_without_wrapping() {
        let attempts = std::cell::Cell::new(0usize);
        let error = select_available_port_with(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            u16::MAX - 1,
            100,
            |_| false,
            |_| {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "busy"))
            },
        )
        .expect_err("bounded exhaustion");
        assert!(matches!(error, CoreError::Server(_)));
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn automatic_port_uses_a_real_loopback_bind_check() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind fixture");
        let port = occupied.local_addr().expect("address").port();
        if port > u16::MAX - 100 {
            return;
        }
        let selected = select_available_port_with(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
            100,
            |_| false,
            |address| TcpListener::bind(address).map(drop),
        )
        .expect("next port");
        assert!(selected > port);
    }

    #[test]
    fn microsoft_account_uses_its_authenticated_uuid_for_online_mode() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        let microsoft_id = Uuid::new_v4();
        remember_active_account(
            &mut app,
            &microsoft_id.to_string(),
            "OnlineDeveloper",
            AccountKind::Microsoft,
        );
        let profile = app
            .default_operator_profile(&ServerNetworkSettings {
                online_mode: true,
                ..ServerNetworkSettings::default()
            })
            .expect("authenticated profile");

        assert_eq!(profile.id, microsoft_id);
        assert_eq!(profile.identity, ServerOperatorIdentity::Online);
    }

    #[test]
    fn microsoft_account_uses_its_name_derived_uuid_for_offline_mode() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        let microsoft_id = Uuid::new_v4();
        remember_active_account(
            &mut app,
            &microsoft_id.to_string(),
            "OnlineDeveloper",
            AccountKind::Microsoft,
        );
        let profile = app
            .default_operator_profile(&ServerNetworkSettings::default())
            .expect("offline identity");

        assert_eq!(profile.id, ephervex_auth::offline_uuid("OnlineDeveloper"));
        assert_ne!(profile.id, microsoft_id);
        assert_eq!(profile.identity, ServerOperatorIdentity::Offline);
    }

    #[test]
    fn offline_account_resolves_only_for_offline_mode() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        let offline_id = ephervex_auth::offline_uuid("Developer");
        remember_active_account(
            &mut app,
            &offline_id.to_string(),
            "Developer",
            AccountKind::Offline,
        );
        assert!(
            app.default_operator_profile(&ServerNetworkSettings {
                online_mode: true,
                ..ServerNetworkSettings::default()
            })
            .is_none()
        );
        let profile = app
            .default_operator_profile(&ServerNetworkSettings::default())
            .expect("offline profile");
        assert_eq!(profile.id, offline_id);
        assert_eq!(profile.identity, ServerOperatorIdentity::Offline);
    }

    #[test]
    fn malformed_or_missing_online_uuid_is_never_derived_from_the_name() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        let online = ServerNetworkSettings {
            online_mode: true,
            ..ServerNetworkSettings::default()
        };
        assert!(app.default_operator_profile(&online).is_none());

        remember_active_account(&mut app, "not-a-uuid", "Unsafe", AccountKind::Microsoft);
        assert!(app.default_operator_profile(&online).is_none());
    }

    #[test]
    fn default_server_configuration_resolves_a_microsoft_account_for_operator_setup() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        let microsoft_id = Uuid::new_v4();
        remember_active_account(
            &mut app,
            &microsoft_id.to_string(),
            "DefaultDeveloper",
            AccountKind::Microsoft,
        );
        assert!(!app.config().server_defaults.online_mode);
        assert!(app.config().server_defaults.make_default_account_operator);

        let setup = app
            .default_operator_setup(&ServerNetworkSettings::default())
            .expect("enabled default intent");
        let profile = setup.profile.expect("usable default profile");
        assert_eq!(profile.id, ephervex_auth::offline_uuid("DefaultDeveloper"));
        assert_eq!(profile.identity, ServerOperatorIdentity::Offline);
    }

    #[test]
    fn operator_setup_preserves_existing_entries_and_server_property_semantics() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = test_app(directory.path());
        let microsoft_id = Uuid::new_v4();
        remember_active_account(
            &mut app,
            &microsoft_id.to_string(),
            "OnlineDeveloper",
            AccountKind::Microsoft,
        );
        let network = ServerNetworkSettings::default();
        let profile = app
            .default_operator_profile(&network)
            .expect("offline identity from Microsoft account");
        let unrelated = Uuid::new_v4();
        let selected = profile.id;
        assert_ne!(selected, microsoft_id);
        std::fs::write(
            directory.path().join(OPERATORS_FILE),
            format!(
                r#"[{{"uuid":"{unrelated}","name":"Existing","level":2,"bypassesPlayerLimit":true,"extension":"keep"}}]"#
            ),
        )
        .expect("fixture");
        let properties = directory.path().join(SERVER_PROPERTIES_FILE);
        std::fs::write(&properties, "op-permission-level=2\nmotd=keep\n")
            .expect("properties fixture");
        materialize_operator_setup(
            directory.path(),
            &network,
            Some(&ServerOperatorSetup {
                permission_level: ephervex_model::OperatorPermissionLevel::from_value(4)
                    .expect("level"),
                profile: Some(profile),
            }),
        )
        .expect("materialize");
        merge_network_file(&properties, &network).expect("network merge");

        let operators = OperatorsDocument::parse(
            &std::fs::read_to_string(directory.path().join(OPERATORS_FILE)).expect("ops"),
        )
        .expect("parse");
        assert_eq!(operators.entries().count(), 2);
        assert!(operators.entries().any(|entry| entry.uuid == unrelated));
        assert!(operators.entries().any(|entry| {
            entry.uuid == selected && entry.level() == Some(4) && entry.name == "OnlineDeveloper"
        }));
        let operator_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join(OPERATORS_FILE)).expect("ops"),
        )
        .expect("JSON");
        assert_eq!(operator_json[0]["extension"], "keep");
        let properties = std::fs::read_to_string(properties).expect("properties");
        assert!(properties.contains("op-permission-level=2"));
        assert!(properties.contains("motd=keep"));
    }

    #[test]
    fn operator_setup_keeps_entries_minecraft_accepts_with_missing_or_zero_levels() {
        let directory = tempfile::tempdir().expect("temp dir");
        let zero = Uuid::new_v4();
        let no_level = Uuid::new_v4();
        let no_bypass = Uuid::new_v4();
        let selected = Uuid::new_v4();
        std::fs::write(
            directory.path().join(OPERATORS_FILE),
            format!(
                r#"[{{"uuid":"{zero}","name":"Zero","level":0,"bypassesPlayerLimit":false}},{{"uuid":"{no_level}","name":"NoLevel","bypassesPlayerLimit":true}},{{"uuid":"{no_bypass}","name":"NoBypass","level":3}}]"#
            ),
        )
        .expect("fixture");

        materialize_operator_setup(
            directory.path(),
            &ServerNetworkSettings::default(),
            Some(&ServerOperatorSetup {
                permission_level: ephervex_model::OperatorPermissionLevel::default(),
                profile: Some(ServerOperatorProfile {
                    id: selected,
                    name: "Developer".to_owned(),
                    identity: ServerOperatorIdentity::Offline,
                }),
            }),
        )
        .expect("tolerant entries must not block the start");

        let operators: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join(OPERATORS_FILE)).expect("ops"),
        )
        .expect("JSON");
        assert_eq!(operators[0]["level"], 0);
        assert!(operators[1].get("level").is_none());
        assert_eq!(operators[1]["bypassesPlayerLimit"], true);
        assert_eq!(operators[2]["level"], 3);
        assert!(operators[2].get("bypassesPlayerLimit").is_none());
        assert_eq!(operators[3]["uuid"], selected.to_string());
        assert_eq!(operators[3]["level"], 4);
    }

    #[test]
    fn operator_setup_keeps_elements_minecraft_skips_and_still_adds_the_operator() {
        let directory = tempfile::tempdir().expect("temp dir");
        let selected = Uuid::new_v4();
        std::fs::write(
            directory.path().join(OPERATORS_FILE),
            r#"[{"name":"NoUuid"},{"uuid":"not-a-uuid","name":"BadUuid"},{"uuid":"44444444-4444-4444-8444-444444444444"}]"#,
        )
        .expect("fixture");

        materialize_operator_setup(
            directory.path(),
            &ServerNetworkSettings::default(),
            Some(&ServerOperatorSetup {
                permission_level: ephervex_model::OperatorPermissionLevel::default(),
                profile: Some(ServerOperatorProfile {
                    id: selected,
                    name: "Developer".to_owned(),
                    identity: ServerOperatorIdentity::Offline,
                }),
            }),
        )
        .expect("elements Minecraft skips must not block the start");

        let operators: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(directory.path().join(OPERATORS_FILE)).expect("ops"),
        )
        .expect("JSON");
        assert_eq!(operators[0], serde_json::json!({"name": "NoUuid"}));
        assert_eq!(
            operators[1],
            serde_json::json!({"uuid": "not-a-uuid", "name": "BadUuid"})
        );
        assert_eq!(operators[3]["uuid"], selected.to_string());
        assert_eq!(operators[3]["level"], 4);
    }

    #[test]
    fn operator_setup_waits_when_the_profile_does_not_match_the_authentication_mode() {
        let directory = tempfile::tempdir().expect("temp dir");
        let setup = ServerOperatorSetup {
            permission_level: ephervex_model::OperatorPermissionLevel::default(),
            profile: Some(ServerOperatorProfile {
                id: Uuid::new_v4(),
                name: "OnlineDeveloper".to_owned(),
                identity: ServerOperatorIdentity::Online,
            }),
        };

        materialize_operator_setup(
            directory.path(),
            &ServerNetworkSettings::default(),
            Some(&setup),
        )
        .expect("incompatible intent remains pending");

        assert!(!directory.path().join(OPERATORS_FILE).exists());
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
    async fn a_server_name_a_rename_would_refuse_is_refused_before_a_root_is_made() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let too_long = "n".repeat(crate::instance::MAX_ENTITY_NAME_CHARS + 1);

        for name in ["", "two\nlines", too_long.as_str()] {
            let error = app
                .create_server(
                    NewServer::new(name, "1.20.1", LoaderSelection::vanilla()),
                    &ServerCreateOptions::default(),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("the name is refused");
            assert!(
                matches!(error, CoreError::EntityNameInvalid { .. }),
                "{name:?}: {error:?}"
            );
        }

        assert!(app.list_servers().expect("list").is_empty());
        for class in [
            ephervex_model::StorageClass::Persistent,
            ephervex_model::StorageClass::Temporary,
        ] {
            let root = app.paths().servers_dir(class);
            let leftovers = std::fs::read_dir(&root).map_or(0, Iterator::count);
            assert_eq!(leftovers, 0, "{}", root.display());
        }
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
    fn server_runtime_settings_whose_layered_heap_range_is_invalid_are_refused() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = app
            .registry_for_services()
            .create_server(NewServer::new(
                "invalid heap",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("scaffold");

        // The default global range is 512..4096 MiB.
        let error = app
            .set_server_runtime_settings(
                server.id,
                None,
                Some(JvmSettings {
                    max_memory_mib: Some(256),
                    ..JvmSettings::default()
                }),
            )
            .expect_err("a maximum below the global minimum cannot launch");

        assert!(
            matches!(
                error,
                CoreError::JavaSettingsInvalid {
                    minimum_mib: 512,
                    maximum_mib: 256,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(
            app.server(server.id)
                .expect("server")
                .jvm_override
                .is_none()
        );
    }

    #[test]
    fn partial_network_change_keeps_unspecified_settings() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = app
            .registry_for_services()
            .create_server(NewServer::new(
                "partial network",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("scaffold");
        app.set_server_network(
            server.id,
            ServerNetworkSettings {
                online_mode: true,
                access: NetworkAccess::Lan,
                port: 25_570,
                bind_address: None,
            },
        )
        .expect("initial network");

        let committed = app
            .update_server_network(
                server.id,
                &ServerNetworkChange {
                    port: Some(25_571),
                    ..ServerNetworkChange::default()
                },
            )
            .expect("port-only change");
        assert_eq!(
            committed.record.network,
            ServerNetworkSettings {
                online_mode: true,
                access: NetworkAccess::Lan,
                port: 25_571,
                bind_address: None,
            }
        );
        let properties = std::fs::read_to_string(server_properties_path(&committed.record))
            .expect("properties file");
        assert!(properties.contains("online-mode=true"));
        assert!(properties.contains("server-port=25571"));

        let committed = app
            .update_server_network(
                server.id,
                &ServerNetworkChange {
                    online_mode: Some(false),
                    ..ServerNetworkChange::default()
                },
            )
            .expect("online mode off");
        assert!(!committed.record.network.online_mode);
        assert_eq!(committed.record.network.access, NetworkAccess::Lan);
        assert_eq!(committed.record.network.port, 25_571);
    }

    #[test]
    fn partial_network_change_resolves_the_custom_bind_address() {
        let custom = ServerNetworkSettings {
            online_mode: true,
            access: NetworkAccess::Custom,
            port: 25_565,
            bind_address: Some("192.168.1.5".to_owned()),
        };

        let port_only = ServerNetworkChange {
            port: Some(25_600),
            ..ServerNetworkChange::default()
        };
        assert_eq!(
            port_only.applied_to(&custom).expect("port").bind_address,
            custom.bind_address
        );

        let leave_custom = ServerNetworkChange {
            access: Some(NetworkAccess::Localhost),
            ..ServerNetworkChange::default()
        };
        assert_eq!(
            leave_custom
                .applied_to(&custom)
                .expect("localhost")
                .bind_address,
            None
        );

        let address_without_custom = ServerNetworkChange {
            bind_address: Some("10.0.0.1".to_owned()),
            ..ServerNetworkChange::default()
        };
        assert!(
            address_without_custom
                .applied_to(&ServerNetworkSettings::default())
                .is_err()
        );

        let custom_without_address = ServerNetworkChange {
            access: Some(NetworkAccess::Custom),
            ..ServerNetworkChange::default()
        };
        let applied = custom_without_address
            .applied_to(&ServerNetworkSettings::default())
            .expect("applies");
        assert!(network_properties(&applied).is_err());
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
    fn capture_snapshot_reads_newest_records_without_reordering_them() {
        let directory = tempfile::tempdir().expect("temp dir");
        let captured = |text: &str| CapturedLine {
            timestamp: Utc::now(),
            stream: ephervex_server::CaptureStream::Stdout,
            text: text.to_owned(),
            truncated: false,
            omitted_bytes: 0,
        };
        let segment = |name: &str, lines: &[CapturedLine]| {
            let mut body = Vec::new();
            for line in lines {
                serde_json::to_writer(&mut body, line).expect("serialize capture");
                body.push(b'\n');
            }
            std::fs::write(directory.path().join(name), body).expect("write capture segment");
        };
        segment(
            "capture-20260813T120000-old.jsonl",
            &[captured("old one"), captured("old two")],
        );
        segment(
            "capture-20260813T130000-new.jsonl",
            &[
                captured("new one"),
                captured("new two"),
                captured("new three"),
            ],
        );

        let snapshot =
            read_capture_snapshot(directory.path(), 2, CaptureScope::Retained).expect("snapshot");
        assert_eq!(
            snapshot
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["new two", "new three"]
        );
        assert!(
            read_capture_snapshot(directory.path(), 0, CaptureScope::Retained)
                .expect("metadata-only snapshot")
                .lines
                .is_empty()
        );
    }

    fn write_capture_segment(directory: &Path, name: &str, texts: &[&str]) {
        let mut body = Vec::new();
        for text in texts {
            let line = CapturedLine {
                timestamp: Utc::now(),
                stream: ephervex_server::CaptureStream::Stdout,
                text: (*text).to_owned(),
                truncated: false,
                omitted_bytes: 0,
            };
            serde_json::to_writer(&mut body, &line).expect("serialize capture");
            body.push(b'\n');
        }
        std::fs::write(directory.join(name), body).expect("write capture segment");
    }

    fn texts(snapshot: &CaptureSnapshot) -> Vec<&str> {
        snapshot
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }

    #[test]
    fn the_latest_run_snapshot_leaves_out_earlier_runs_and_spans_its_own_rotated_segments() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_capture_segment(
            directory.path(),
            "capture-20260813T120000.000Z-a.jsonl",
            &["old one", "old two"],
        );
        write_capture_segment(
            directory.path(),
            "capture-20260813T130000.000Z-b.jsonl",
            &["new one", "new two"],
        );
        write_capture_segment(
            directory.path(),
            "capture-20260813T130500.000Z-c.continued.jsonl",
            &["new three"],
        );

        let latest = read_capture_snapshot(directory.path(), 100, CaptureScope::LatestRun)
            .expect("snapshot");
        assert_eq!(texts(&latest), ["new one", "new two", "new three"]);
        let retained =
            read_capture_snapshot(directory.path(), 100, CaptureScope::Retained).expect("snapshot");
        assert_eq!(
            texts(&retained),
            ["old one", "old two", "new one", "new two", "new three"]
        );
    }

    #[test]
    fn a_run_that_has_written_nothing_yet_has_an_empty_latest_run_snapshot() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_capture_segment(
            directory.path(),
            "capture-20260813T120000.000Z-a.jsonl",
            &["old one"],
        );
        write_capture_segment(
            directory.path(),
            "capture-20260813T130000.000Z-b.jsonl",
            &[],
        );

        let latest = read_capture_snapshot(directory.path(), 100, CaptureScope::LatestRun)
            .expect("snapshot");
        assert!(latest.lines.is_empty());
    }

    #[test]
    fn a_latest_run_that_lost_no_segment_reports_no_pruned_output_from_earlier_runs() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_capture_segment(
            directory.path(),
            "capture-20260813T120000.000Z-a.jsonl",
            &["only run"],
        );
        let retention = ephervex_server::CaptureRetention {
            removed_segments: 3,
            removed_bytes: 10,
            removed_lines: 9,
            earliest_retained_at: None,
        };
        std::fs::write(
            directory.path().join(ephervex_server::CAPTURE_INDEX_FILE),
            serde_json::to_vec(&retention).expect("index"),
        )
        .expect("write index");

        let latest = read_capture_snapshot(directory.path(), 100, CaptureScope::LatestRun)
            .expect("snapshot");
        assert_eq!(latest.retention.removed_lines, 0);
        let retained =
            read_capture_snapshot(directory.path(), 100, CaptureScope::Retained).expect("snapshot");
        assert_eq!(retained.retention.removed_lines, 9);
    }

    #[test]
    fn a_latest_run_whose_first_segment_was_pruned_keeps_the_retention_report() {
        let directory = tempfile::tempdir().expect("temp dir");
        write_capture_segment(
            directory.path(),
            "capture-20260813T130500.000Z-c.continued.jsonl",
            &["tail"],
        );
        let retention = ephervex_server::CaptureRetention {
            removed_segments: 1,
            removed_bytes: 10,
            removed_lines: 4,
            earliest_retained_at: None,
        };
        std::fs::write(
            directory.path().join(ephervex_server::CAPTURE_INDEX_FILE),
            serde_json::to_vec(&retention).expect("index"),
        )
        .expect("write index");

        let latest = read_capture_snapshot(directory.path(), 100, CaptureScope::LatestRun)
            .expect("snapshot");
        assert_eq!(texts(&latest), ["tail"]);
        assert_eq!(latest.retention.removed_lines, 4);
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
        app.registry_for_services()
            .publish_client(&client)
            .expect("publish");
        let mut server = app
            .registry_for_services()
            .prepare_server(NewServer::linked("linked", client.id))
            .expect("prepare");

        let error = app
            .linked_server_recipe(&mut server, &UnknownServerFiles::default())
            .expect_err("missing desired state is not an empty pack");
        assert!(matches!(
            error,
            CoreError::Pack(PackError::RecipeMissing { .. })
        ));
        assert!(app.list_servers().expect("servers").is_empty());
        assert!(!server.root.exists());
    }

    fn scaffolded_server(app: &App) -> ServerRecord {
        app.registry_for_services()
            .create_server(NewServer::new(
                "Canonical Server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("scaffold")
    }

    #[test]
    fn rename_keeps_canonical_settings_the_index_copy_lags_behind() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let mut canonical = ephervex_storage::metadata::read_server_metadata(&server.root)
            .expect("canonical metadata");
        canonical.network.port = 25_599;
        canonical.java_override = Some(JavaOverride::new("/opt/java/bin/java"));
        ephervex_storage::metadata::write_server_metadata(&server.root, &canonical)
            .expect("write canonical metadata only");

        app.rename_server(server.id, "Renamed").expect("rename");

        let after = ephervex_storage::metadata::read_server_metadata(&server.root)
            .expect("canonical metadata");
        assert_eq!(after.name, "Renamed");
        assert_eq!(after.network.port, 25_599);
        assert_eq!(after.java_override, canonical.java_override);
        assert_eq!(app.server(server.id).expect("index").network.port, 25_599);
    }

    #[test]
    fn rename_refuses_corrupt_canonical_metadata_and_leaves_it_untouched() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let path = ephervex_storage::metadata::server_metadata_path(&server.root);
        std::fs::write(&path, b"{ not json").expect("corrupt the metadata");

        app.rename_server(server.id, "Renamed")
            .expect_err("corrupt metadata blocks the rename");

        assert_eq!(std::fs::read(&path).expect("read"), b"{ not json");
        assert_eq!(
            app.server(server.id).expect("index").name,
            "Canonical Server"
        );
    }

    #[test]
    fn rename_refuses_missing_canonical_metadata_without_recreating_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let path = ephervex_storage::metadata::server_metadata_path(&server.root);
        std::fs::remove_file(&path).expect("remove the metadata");

        app.rename_server(server.id, "Renamed")
            .expect_err("missing metadata blocks the rename");

        assert!(!path.exists());
    }

    #[test]
    fn recording_attention_refuses_corrupt_canonical_metadata_and_leaves_it_untouched() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let path = ephervex_storage::metadata::server_metadata_path(&server.root);
        std::fs::write(&path, b"{ not json").expect("corrupt the metadata");

        app.add_server_attention(server.id, AttentionReason::InterruptedMutation)
            .expect_err("corrupt metadata blocks the write");

        assert_eq!(std::fs::read(&path).expect("read"), b"{ not json");
    }

    #[test]
    fn recording_attention_keeps_canonical_settings_the_index_copy_lags_behind() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let server = scaffolded_server(&app);
        let mut canonical = ephervex_storage::metadata::read_server_metadata(&server.root)
            .expect("canonical metadata");
        canonical.network.port = 25_599;
        ephervex_storage::metadata::write_server_metadata(&server.root, &canonical)
            .expect("write canonical metadata only");

        app.add_server_attention(server.id, AttentionReason::InterruptedMutation)
            .expect("record attention");

        let after = ephervex_storage::metadata::read_server_metadata(&server.root)
            .expect("canonical metadata");
        assert_eq!(after.network.port, 25_599);
        assert!(
            after
                .attention
                .contains(&AttentionReason::InterruptedMutation)
        );
    }

    #[test]
    fn a_linked_servers_entry_follows_it_through_a_rename_and_a_deletion() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let client = app
            .registry_for_services()
            .create_client(ephervex_storage::NewClient::new(
                "Repro Client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("client");
        app.registry_for_services()
            .publish_client(&client)
            .expect("publish");
        let server = app
            .registry_for_services()
            .create_server(NewServer::linked("Repro Server", client.id))
            .expect("create");

        let servers_dat =
            ephervex_storage::metadata::client_game_dir(&client.root).join("servers.dat");
        assert!(
            !servers_dat.exists(),
            "creating the server alone does not touch the client yet"
        );

        let renamed = app
            .rename_server(server.id, "Renamed Repro Server")
            .expect("rename")
            .record;
        let entry = read_managed_entry(&servers_dat, renamed.id);
        assert_eq!(
            entry.get("name"),
            Some(&Value::String("Renamed Repro Server".to_owned()))
        );
        assert_eq!(
            entry.get("ip"),
            Some(&Value::String("127.0.0.1:25565".to_owned()))
        );

        app.delete_server(server.id, FileDisposition::Remove)
            .expect("delete");
        assert!(
            find_managed_entry(&servers_dat, server.id).is_none(),
            "deleting the server removes its entry, not just the record"
        );
    }

    /// A client with a linked server, the way a user would have them before
    /// the game ever saved its server list.
    fn linked_server_in_client_with_entry(app: &App) -> (PathBuf, ServerRecord) {
        let client = app
            .registry_for_services()
            .create_client(ephervex_storage::NewClient::new(
                "Repro Client",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("client");
        app.registry_for_services()
            .publish_client(&client)
            .expect("publish");
        let server = app
            .registry_for_services()
            .create_server(NewServer::linked("Repro Server", client.id))
            .expect("create");
        app.sync_linked_server_list(&server, None);
        let servers_dat =
            ephervex_storage::metadata::client_game_dir(&client.root).join("servers.dat");
        (servers_dat, server)
    }

    /// Rewrites `servers.dat` the way Minecraft saves its server list: every
    /// entry keeps only its own fields, so the hidden id is lost.
    fn strip_managed_ids(path: &Path) {
        let bytes = std::fs::read(path).expect("read servers.dat");
        let mut root: std::collections::HashMap<String, Value> =
            fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(list)) = root.get_mut("servers") else {
            panic!("no servers list");
        };
        for item in list.iter_mut() {
            if let Value::Compound(fields) = item {
                fields.remove("ephervexLinkedServerId");
            }
        }
        std::fs::write(path, fastnbt::to_bytes(&root).expect("encode")).expect("write servers.dat");
    }

    fn listed_entries(path: &Path) -> Vec<(String, String)> {
        let bytes = std::fs::read(path).expect("read servers.dat");
        let root: std::collections::HashMap<String, Value> =
            fastnbt::from_bytes(&bytes).expect("valid nbt");
        let Some(Value::List(list)) = root.get("servers") else {
            panic!("no servers list");
        };
        list.iter()
            .map(|item| {
                let Value::Compound(fields) = item else {
                    panic!("not a compound");
                };
                let text = |key: &str| match fields.get(key) {
                    Some(Value::String(value)) => value.clone(),
                    _ => panic!("missing {key}"),
                };
                (text("name"), text("ip"))
            })
            .collect()
    }

    #[test]
    fn a_rename_updates_the_one_entry_after_the_game_dropped_its_hidden_id() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let (servers_dat, server) = linked_server_in_client_with_entry(&app);
        strip_managed_ids(&servers_dat);

        app.rename_server(server.id, "Renamed Repro Server")
            .expect("rename");

        assert_eq!(
            listed_entries(&servers_dat),
            vec![(
                "Renamed Repro Server".to_owned(),
                "127.0.0.1:25565".to_owned()
            )]
        );
    }

    #[test]
    fn a_port_change_updates_the_one_entry_after_the_game_dropped_its_hidden_id() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let (servers_dat, server) = linked_server_in_client_with_entry(&app);
        strip_managed_ids(&servers_dat);

        app.update_server_network(
            server.id,
            &ServerNetworkChange {
                port: Some(25_571),
                ..ServerNetworkChange::default()
            },
        )
        .expect("port change");

        assert_eq!(
            listed_entries(&servers_dat),
            vec![("Repro Server".to_owned(), "127.0.0.1:25571".to_owned())]
        );
    }

    #[test]
    fn deleting_a_server_removes_its_entry_after_the_game_dropped_its_hidden_id() {
        let directory = tempfile::tempdir().expect("temp dir");
        let app = test_app(directory.path());
        let (servers_dat, server) = linked_server_in_client_with_entry(&app);
        strip_managed_ids(&servers_dat);

        app.delete_server(server.id, FileDisposition::Remove)
            .expect("delete");

        assert!(listed_entries(&servers_dat).is_empty());
    }

    /// The one entry `sync_entry`/`remove_entry` manage in a client's
    /// `servers.dat`, found by the hidden id they tag it with.
    fn find_managed_entry(
        path: &Path,
        server: ServerId,
    ) -> Option<std::collections::HashMap<String, Value>> {
        let bytes = std::fs::read(path).ok()?;
        let root: std::collections::HashMap<String, Value> = fastnbt::from_bytes(&bytes).ok()?;
        let Value::List(list) = root.get("servers")? else {
            return None;
        };
        list.iter().find_map(|item| {
            let Value::Compound(fields) = item else {
                return None;
            };
            (fields.get("ephervexLinkedServerId") == Some(&Value::String(server.to_string())))
                .then(|| fields.clone())
        })
    }

    fn read_managed_entry(
        path: &Path,
        server: ServerId,
    ) -> std::collections::HashMap<String, Value> {
        find_managed_entry(path, server).expect("the managed entry is present")
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
