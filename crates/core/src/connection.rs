//! Application service for launching a client into one of its linked servers.

use std::time::Duration;

use ephervex_minecraft::LaunchObserver;
use ephervex_model::{ConnectionTarget, InstanceId, ServerId};
use ephervex_server::{
    RuntimeClassification, ServerExit, ServerObserver, classify_runtime, wait_for_server_readiness,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::launch::{LaunchOptions, LaunchOutcome};
use crate::{App, CoreError, CoreResult};

/// The longest an automatic server start waits for a listening Minecraft
/// endpoint before the client launch is refused.
pub const DEFAULT_SERVER_READINESS_TIMEOUT: Duration = Duration::from_secs(120);

/// Whether the operation will start the server or reuse its verified owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerPreparation {
    Start,
    Reuse,
}

/// Current, validated Launch & Connect availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAndConnectAvailability {
    pub client: InstanceId,
    pub server: ServerId,
    pub target: ConnectionTarget,
    pub server_preparation: ServerPreparation,
    /// A running client is reported without changing the normal launch lock
    /// rule. A frontend can disable the action, and a racing launch is still
    /// refused by the owning launch service.
    pub client_running: bool,
}

/// Options for one linked launch. Client options retain their normal Java,
/// account, feature, and dry-run behavior.
#[derive(Debug, Clone)]
pub struct LaunchAndConnectOptions {
    pub launch: LaunchOptions,
    pub readiness_timeout: Duration,
}

impl Default for LaunchAndConnectOptions {
    fn default() -> Self {
        Self {
            launch: LaunchOptions::default(),
            readiness_timeout: DEFAULT_SERVER_READINESS_TIMEOUT,
        }
    }
}

/// What the combined operation produced after the client exited or a dry run
/// completed. A server started by this operation remains supervised by the
/// application and is not coupled to the client process lifetime.
#[derive(Debug)]
pub struct LaunchAndConnectOutcome {
    pub client: InstanceId,
    pub server: ServerId,
    pub target: ConnectionTarget,
    pub server_started: bool,
    pub launch: LaunchOutcome,
}

/// Progress specific to preparing the linked server. Server and client output
/// continue through their existing observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LaunchAndConnectEvent {
    StartingServer { server: ServerId },
    ReusingServer { server: ServerId },
    WaitingForServer { target: ConnectionTarget },
    ServerReady { target: ConnectionTarget },
}

#[derive(Debug, Clone)]
pub struct LaunchAndConnectObserver {
    sender: Option<mpsc::UnboundedSender<LaunchAndConnectEvent>>,
}

impl LaunchAndConnectObserver {
    #[must_use]
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<LaunchAndConnectEvent>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                sender: Some(sender),
            },
            receiver,
        )
    }

    #[must_use]
    pub const fn silent() -> Self {
        Self { sender: None }
    }

    pub fn emit(&self, event: LaunchAndConnectEvent) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
    }
}

/// Relationship and runtime failures owned by the combined application flow.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    #[error("server {server} is not linked to a client")]
    ServerNotLinked { server: ServerId },
    #[error("server {server} is linked to client {linked_client}, not {requested_client}")]
    RelationshipMismatch {
        server: ServerId,
        requested_client: InstanceId,
        linked_client: InstanceId,
    },
    #[error("the linked client and server do not have the same exact Minecraft and loader target")]
    TargetMismatch {
        client_minecraft: String,
        server_minecraft: String,
        client_loader: String,
        server_loader: String,
    },
    #[error("server {server} has a runtime Ephervex cannot safely reuse or replace")]
    RuntimeUncertain { server: ServerId },
    #[error("server {server} stopped before it accepted connections")]
    ServerStoppedBeforeReady { server: ServerId },
    #[error("supervision for server {server} ended unexpectedly")]
    SupervisorLost { server: ServerId },
}

impl ConnectError {
    #[must_use]
    pub const fn message_id(&self) -> &'static str {
        match self {
            Self::ServerNotLinked { .. } => "error-connect-server-not-linked",
            Self::RelationshipMismatch { .. } => "error-connect-relationship-mismatch",
            Self::TargetMismatch { .. } => "error-connect-target-mismatch",
            Self::RuntimeUncertain { .. } => "error-connect-runtime-uncertain",
            Self::ServerStoppedBeforeReady { .. } => "error-connect-server-stopped-before-ready",
            Self::SupervisorLost { .. } => "error-connect-supervisor-lost",
        }
    }

    #[must_use]
    pub fn message_args(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::ServerNotLinked { server }
            | Self::RuntimeUncertain { server }
            | Self::ServerStoppedBeforeReady { server }
            | Self::SupervisorLost { server } => vec![("server", server.to_string())],
            Self::RelationshipMismatch {
                server,
                requested_client,
                linked_client,
            } => vec![
                ("server", server.to_string()),
                ("requested_client", requested_client.to_string()),
                ("linked_client", linked_client.to_string()),
            ],
            Self::TargetMismatch {
                client_minecraft,
                server_minecraft,
                client_loader,
                server_loader,
            } => vec![
                ("client_minecraft", client_minecraft.clone()),
                ("server_minecraft", server_minecraft.clone()),
                ("client_loader", client_loader.clone()),
                ("server_loader", server_loader.clone()),
            ],
        }
    }
}

impl App {
    /// Validates the exact relationship and reports the target and lifecycle
    /// action without changing either entity or starting a process.
    pub fn launch_and_connect_availability(
        &self,
        client_id: InstanceId,
        server_id: ServerId,
    ) -> CoreResult<LaunchAndConnectAvailability> {
        let client = self.client(client_id)?;
        let server = self.canonical_server_for_mutation(server_id)?;
        match server.linked_client_id {
            None => return Err(ConnectError::ServerNotLinked { server: server_id }.into()),
            Some(linked_client) if linked_client != client_id => {
                return Err(ConnectError::RelationshipMismatch {
                    server: server_id,
                    requested_client: client_id,
                    linked_client,
                }
                .into());
            }
            Some(_) => {}
        }
        if client.minecraft_version != server.minecraft_version || client.loader != server.loader {
            return Err(ConnectError::TargetMismatch {
                client_minecraft: client.minecraft_version,
                server_minecraft: server.minecraft_version,
                client_loader: client.loader.to_string(),
                server_loader: server.loader.to_string(),
            }
            .into());
        }
        if !server.installation.is_installed() {
            return Err(ephervex_server::ServerError::NotInstalled.into());
        }
        let target = Self::server_connection_target(&server)?;
        let server_preparation = match classify_runtime(&server.root)? {
            RuntimeClassification::Stopped | RuntimeClassification::ProvenStale { .. } => {
                ServerPreparation::Start
            }
            RuntimeClassification::Controlled { .. } => ServerPreparation::Reuse,
            RuntimeClassification::UnknownRunning { .. }
            | RuntimeClassification::Orphaned { .. } => {
                return Err(ConnectError::RuntimeUncertain { server: server_id }.into());
            }
        };
        Ok(LaunchAndConnectAvailability {
            client: client_id,
            server: server_id,
            target,
            server_preparation,
            client_running: self.launch_state(client_id)?.is_some(),
        })
    }

    /// Starts or reuses the linked server, waits for its bounded TCP readiness,
    /// then invokes the ordinary client launch with direct-connect features.
    #[allow(clippy::too_many_arguments)]
    // The two halves of this flow each open their own operation; naming both
    // records here is what lets a launcher log show which client was waiting on
    // which server between them.
    #[tracing::instrument(skip_all, fields(instance = %client_id, server = %server_id))]
    pub async fn launch_and_connect(
        &mut self,
        client_id: InstanceId,
        server_id: ServerId,
        options: &LaunchAndConnectOptions,
        observer: &LaunchAndConnectObserver,
        server_observer: &ServerObserver,
        launch_observer: &LaunchObserver,
        cancel: &CancellationToken,
    ) -> CoreResult<LaunchAndConnectOutcome> {
        let availability = self.launch_and_connect_availability(client_id, server_id)?;
        // This early read avoids starting a stopped server for a client that
        // is already known to be unavailable. The ordinary launch lock remains
        // authoritative and closes a race with another launcher after this
        // check without introducing a second lock path here.
        if let Some(running) = self.launch_state(client_id)? {
            return Err(ephervex_storage::LockError::AlreadyRunning {
                pid: running.holder_pid(),
                since: running.since,
            }
            .into());
        }
        let mut started = None;
        match availability.server_preparation {
            ServerPreparation::Start => {
                observer.emit(LaunchAndConnectEvent::StartingServer { server: server_id });
                let prepared = self
                    .prepare_server_start(server_id, None, server_observer)
                    .await?;
                let server_observer = server_observer.clone();
                started = Some(tokio::spawn(async move {
                    App::run_prepared_server(prepared, &server_observer).await
                }));
            }
            ServerPreparation::Reuse => {
                observer.emit(LaunchAndConnectEvent::ReusingServer { server: server_id });
            }
        }
        observer.emit(LaunchAndConnectEvent::WaitingForServer {
            target: availability.target,
        });
        wait_for_managed_server(
            server_id,
            &self.server(server_id)?.root,
            availability.target,
            options.readiness_timeout,
            cancel,
            started.as_mut(),
        )
        .await?;
        match classify_runtime(&self.server(server_id)?.root)? {
            RuntimeClassification::Controlled { .. } => {}
            RuntimeClassification::Stopped | RuntimeClassification::ProvenStale { .. } => {
                return Err(ConnectError::ServerStoppedBeforeReady { server: server_id }.into());
            }
            RuntimeClassification::UnknownRunning { .. }
            | RuntimeClassification::Orphaned { .. } => {
                return Err(ConnectError::RuntimeUncertain { server: server_id }.into());
            }
        }
        observer.emit(LaunchAndConnectEvent::ServerReady {
            target: availability.target,
        });

        // Dropping a JoinHandle detaches its task. The existing supervisor
        // remains the sole owner, and the server is not coupled to the client
        // process or to a successful client spawn.
        drop(started);
        let launch_options = connected_launch_options(&options.launch, availability.target);
        let launch = self
            .launch(client_id, &launch_options, launch_observer, cancel)
            .await?;
        Ok(LaunchAndConnectOutcome {
            client: client_id,
            server: server_id,
            target: availability.target,
            server_started: availability.server_preparation == ServerPreparation::Start,
            launch,
        })
    }
}

fn connected_launch_options(options: &LaunchOptions, target: ConnectionTarget) -> LaunchOptions {
    let mut connected = options.clone();
    connected.features = connected.features.direct_connect(target);
    connected
}

async fn wait_for_managed_server(
    server: ServerId,
    root: &std::path::Path,
    target: ConnectionTarget,
    timeout: Duration,
    cancel: &CancellationToken,
    started: Option<&mut JoinHandle<CoreResult<ServerExit>>>,
) -> CoreResult<()> {
    let readiness = wait_for_server_readiness(target, timeout, cancel);
    tokio::pin!(readiness);
    if let Some(started) = started {
        tokio::select! {
            result = &mut readiness => Ok(result?),
            result = started => match result {
                Ok(Ok(_)) => Err(ConnectError::ServerStoppedBeforeReady { server }.into()),
                Ok(Err(error)) => Err(error),
                Err(_) => Err(ConnectError::SupervisorLost { server }.into()),
            },
        }
    } else {
        let stopped = wait_for_controlled_server_to_stop(server, root);
        tokio::pin!(stopped);
        tokio::select! {
            result = &mut readiness => Ok(result?),
            error = &mut stopped => Err(error),
        }
    }
}

async fn wait_for_controlled_server_to_stop(server: ServerId, root: &std::path::Path) -> CoreError {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        match classify_runtime(root) {
            Ok(RuntimeClassification::Controlled { .. }) => {}
            Ok(RuntimeClassification::Stopped | RuntimeClassification::ProvenStale { .. }) => {
                return ConnectError::ServerStoppedBeforeReady { server }.into();
            }
            Ok(
                RuntimeClassification::UnknownRunning { .. }
                | RuntimeClassification::Orphaned { .. },
            ) => {
                return ConnectError::RuntimeUncertain { server }.into();
            }
            Err(error) => return error.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::{IpAddr, Ipv4Addr};
    use std::num::NonZeroU16;
    use std::path::{Path, PathBuf};

    use ephervex_minecraft::LaunchFeatures;
    use ephervex_model::{LoaderKind, LoaderSelection, ServerInstallation, ServerRelativePath};
    use ephervex_server::{
        ControlAction, ServerCommand, ServerError, ServerSupervisor, send_control,
    };
    use ephervex_storage::{
        FileDisposition, LaunchLock, NetworkAccess, NewClient, NewServer, Paths,
        ServerNetworkSettings,
    };

    use super::*;

    fn app_rooted_at(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app")
    }

    fn installed_linked_server(
        app: &App,
        network: ServerNetworkSettings,
    ) -> (
        ephervex_storage::ClientRecord,
        ephervex_storage::ServerRecord,
    ) {
        let loader = LoaderSelection::exact(LoaderKind::Forge, "47.4.0");
        let client = app
            .create_client_for_test(NewClient::new("client", "1.20.1", loader.clone()))
            .expect("client");
        let mut server = app
            .registry_for_services()
            .create_server(NewServer::linked("server", client.id).with_network(network))
            .expect("server");
        server.installation = ServerInstallation::Installed {
            launch: ephervex_model::ServerLaunch::Jar {
                path: ServerRelativePath::new("server.jar").expect("launch path"),
            },
        };
        std::fs::write(server.root.join("server/server.jar"), b"fixture").expect("server jar");
        app.registry_for_services()
            .update_server(&mut server)
            .expect("update server");
        (client, server)
    }

    fn local_target(port: u16) -> ConnectionTarget {
        ConnectionTarget {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: NonZeroU16::new(port).expect("port"),
        }
    }

    #[test]
    fn a_valid_exact_link_reports_a_stopped_server_and_its_effective_target() {
        let directory = tempfile::tempdir().expect("directory");
        let app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(
            &app,
            ServerNetworkSettings {
                port: 25_567,
                ..ServerNetworkSettings::default()
            },
        );

        let availability = app
            .launch_and_connect_availability(client.id, server.id)
            .expect("available");
        assert_eq!(availability.client, client.id);
        assert_eq!(availability.server, server.id);
        assert_eq!(availability.server_preparation, ServerPreparation::Start);
        assert_eq!(availability.target, local_target(25_567));
        assert_eq!(client.minecraft_version, server.minecraft_version);
        assert_eq!(client.loader, server.loader);

        let held = LaunchLock::acquire(&client.root).expect("client launch lock");
        assert!(
            app.launch_and_connect_availability(client.id, server.id)
                .expect("relationship remains valid")
                .client_running,
            "availability exposes the existing launch without bypassing its lock"
        );
        drop(held);
    }

    #[test]
    fn missing_unlinked_mismatched_and_incompatible_relationships_are_typed() {
        let directory = tempfile::tempdir().expect("directory");
        let app = app_rooted_at(directory.path());
        let (client, mut linked) = installed_linked_server(&app, ServerNetworkSettings::default());
        let other = app
            .create_client_for_test(NewClient::new(
                "other",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.4.0"),
            ))
            .expect("other client");
        let standalone = app
            .registry_for_services()
            .create_server(NewServer::new(
                "standalone",
                "1.20.1",
                LoaderSelection::exact(LoaderKind::Forge, "47.4.0"),
            ))
            .expect("standalone");

        assert!(matches!(
            app.launch_and_connect_availability(client.id, standalone.id),
            Err(CoreError::Connect(ConnectError::ServerNotLinked { .. }))
        ));
        assert!(matches!(
            app.launch_and_connect_availability(other.id, linked.id),
            Err(CoreError::Connect(
                ConnectError::RelationshipMismatch { .. }
            ))
        ));

        linked.minecraft_version = "1.20.2".to_owned();
        app.registry_for_services()
            .update_server(&mut linked)
            .expect("mismatch metadata");
        assert!(matches!(
            app.launch_and_connect_availability(client.id, linked.id),
            Err(CoreError::Connect(ConnectError::TargetMismatch { .. }))
        ));

        assert!(matches!(
            app.launch_and_connect_availability(client.id, ServerId::new()),
            Err(CoreError::Storage(_))
        ));
        app.registry_for_services()
            .delete_client(client.id, FileDisposition::Keep)
            .expect("delete client");
        assert!(matches!(
            app.launch_and_connect_availability(client.id, linked.id),
            Err(CoreError::Storage(_))
        ));
    }

    #[test]
    fn invalid_connection_settings_fail_without_touching_server_properties() {
        let directory = tempfile::tempdir().expect("directory");
        let app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(
            &app,
            ServerNetworkSettings {
                access: NetworkAccess::Custom,
                port: 0,
                bind_address: Some("not-an-address".to_owned()),
                ..ServerNetworkSettings::default()
            },
        );
        let properties = server.root.join("server/server.properties");
        std::fs::write(&properties, b"motd=unchanged\n").expect("properties");
        let before = std::fs::read(&properties).expect("before");

        assert!(matches!(
            app.launch_and_connect_availability(client.id, server.id),
            Err(CoreError::Server(ServerError::InvalidProperty { .. }))
        ));
        assert_eq!(std::fs::read(properties).expect("after"), before);
    }

    #[tokio::test]
    async fn a_stopped_server_requires_existing_eula_consent_before_any_start_or_client_launch() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
        let properties = server.root.join("server/server.properties");
        std::fs::write(&properties, b"motd=unchanged\n").expect("properties");
        let before = std::fs::read(&properties).expect("before");

        let error = app
            .launch_and_connect(
                client.id,
                server.id,
                &LaunchAndConnectOptions {
                    readiness_timeout: Duration::from_millis(25),
                    ..LaunchAndConnectOptions::default()
                },
                &LaunchAndConnectObserver::silent(),
                &ServerObserver::silent(),
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("consent is required by the existing start service");
        assert!(matches!(
            error,
            CoreError::Server(ServerError::EulaNotAccepted)
        ));
        assert!(!server.root.join("server/eula.txt").exists());
        assert_eq!(std::fs::read(properties).expect("after"), before);
        assert!(matches!(
            classify_runtime(&server.root).expect("runtime"),
            RuntimeClassification::Stopped
        ));
    }

    #[tokio::test]
    async fn an_already_launching_client_is_refused_before_server_preparation() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
        let held = LaunchLock::acquire(&client.root).expect("client launch lock");
        let (observer, mut events) = LaunchAndConnectObserver::channel();

        let error = app
            .launch_and_connect(
                client.id,
                server.id,
                &LaunchAndConnectOptions::default(),
                &observer,
                &ServerObserver::silent(),
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a running client is unavailable");
        assert!(matches!(
            error,
            CoreError::Storage(ephervex_storage::StorageError::Lock(
                ephervex_storage::LockError::AlreadyRunning { .. }
            ))
        ));
        assert!(events.try_recv().is_err(), "the server was not prepared");
        assert!(matches!(
            classify_runtime(&server.root).expect("runtime"),
            RuntimeClassification::Stopped
        ));
        assert!(!server.root.join("server/eula.txt").exists());
        drop(held);
    }

    #[tokio::test]
    async fn a_controlled_server_is_reused_and_client_authentication_stays_on_the_normal_path() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let port = listener.local_addr().expect("address").port();
        let (client, server) = installed_linked_server(
            &app,
            ServerNetworkSettings {
                port,
                ..ServerNetworkSettings::default()
            },
        );
        let properties = server.root.join("server/server.properties");
        std::fs::write(&properties, b"motd=unchanged\n").expect("properties");
        let before = std::fs::read(&properties).expect("before");

        let root = server.root.clone();
        let capture = directory.path().join("capture");
        let task = tokio::spawn(async move {
            ServerSupervisor::run(
                &ServerCommand {
                    java: PathBuf::from("/bin/sh"),
                    arguments: vec![
                        "-c".to_owned(),
                        "while IFS= read -r line; do [ \"$line\" = stop ] && exit 0; done"
                            .to_owned(),
                    ],
                    working_directory: root.join("server"),
                },
                &root,
                &capture,
                &ServerObserver::silent(),
            )
            .await
        });
        wait_until_controlled(&server.root).await;
        let (connect_observer, mut connect_events) = LaunchAndConnectObserver::channel();
        let (server_observer, mut server_events) = ServerObserver::channel();

        let error = app
            .launch_and_connect(
                client.id,
                server.id,
                &LaunchAndConnectOptions {
                    launch: LaunchOptions {
                        dry_run: true,
                        ..LaunchOptions::default()
                    },
                    readiness_timeout: Duration::from_secs(1),
                },
                &connect_observer,
                &server_observer,
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("no account still means no launch");
        assert!(error.requires_interactive_login());
        let events: Vec<_> = std::iter::from_fn(|| connect_events.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            LaunchAndConnectEvent::ReusingServer { server: id } if *id == server.id
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            LaunchAndConnectEvent::ServerReady { target } if target.port.get() == port
        )));
        assert!(
            server_events.try_recv().is_err(),
            "no duplicate start occurred"
        );
        assert!(matches!(
            classify_runtime(&server.root).expect("runtime"),
            RuntimeClassification::Controlled { .. }
        ));
        assert_eq!(std::fs::read(properties).expect("after"), before);

        assert!(matches!(
            send_control(&server.root, &ControlAction::Stop)
                .await
                .expect("stop"),
            ephervex_server::ControlResponse::Exited(_)
        ));
        assert_eq!(task.await.expect("join").expect("server").code, Some(0));
    }

    #[tokio::test]
    async fn readiness_propagates_start_failure_exit_and_timeout_before_client_launch() {
        let directory = tempfile::tempdir().expect("directory");
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let target = local_target(address.port());
        let mut running = tokio::spawn(std::future::pending::<CoreResult<ServerExit>>());
        wait_for_managed_server(
            ServerId::new(),
            directory.path(),
            target,
            Duration::from_secs(1),
            &CancellationToken::new(),
            Some(&mut running),
        )
        .await
        .expect("listening target is ready");
        running.abort();
        drop(listener);

        let closed = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve");
        let target = local_target(closed.local_addr().expect("address").port());
        drop(closed);
        let server = ServerId::new();
        let mut exited = tokio::spawn(async {
            Ok(ServerExit {
                code: Some(1),
                signal: None,
                finished_starting: false,
                stop_requested: false,
            })
        });
        assert!(matches!(
            wait_for_managed_server(
                server,
                directory.path(),
                target,
                Duration::from_secs(1),
                &CancellationToken::new(),
                Some(&mut exited),
            )
            .await,
            Err(CoreError::Connect(
                ConnectError::ServerStoppedBeforeReady { .. }
            ))
        ));

        let executable = PathBuf::from("/missing/server-java");
        let mut failed = tokio::spawn(async move {
            Err(ServerError::ProcessStart {
                executable,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            }
            .into())
        });
        assert!(matches!(
            wait_for_managed_server(
                server,
                directory.path(),
                target,
                Duration::from_secs(1),
                &CancellationToken::new(),
                Some(&mut failed),
            )
            .await,
            Err(CoreError::Server(ServerError::ProcessStart { .. }))
        ));

        let mut pending = tokio::spawn(std::future::pending::<CoreResult<ServerExit>>());
        assert!(matches!(
            wait_for_managed_server(
                server,
                directory.path(),
                target,
                Duration::from_millis(25),
                &CancellationToken::new(),
                Some(&mut pending),
            )
            .await,
            Err(CoreError::Server(ServerError::ReadinessTimeout { .. }))
        ));
        pending.abort();
    }

    #[test]
    fn direct_connection_keeps_every_existing_client_launch_option() {
        let target = local_target(25_568);
        let options = LaunchOptions {
            features: LaunchFeatures::none().demo().custom_resolution(1280, 720),
            java_path: Some(PathBuf::from("/exact/client-java")),
            install_java_if_missing: true,
            account: Some("Alice".to_owned()),
            additional: None,
            dry_run: true,
        };
        let connected = connected_launch_options(&options, target);
        assert_eq!(connected.java_path, options.java_path);
        assert_eq!(
            connected.account, options.account,
            "connecting to a server must not silently change who is playing"
        );
        assert_eq!(
            connected.install_java_if_missing,
            options.install_java_if_missing
        );
        assert_eq!(connected.dry_run, options.dry_run);
        assert!(connected.features.is_enabled("is_demo_user"));
        assert!(connected.features.is_enabled("is_quick_play_multiplayer"));
    }

    async fn wait_until_controlled(root: &Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            loop {
                interval.tick().await;
                if matches!(
                    classify_runtime(root),
                    Ok(RuntimeClassification::Controlled { .. })
                ) {
                    return;
                }
            }
        })
        .await
        .expect("controlled runtime");
    }
}
