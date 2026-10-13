//! Application service for launching a client into one of its linked servers.

use std::time::Duration;

use ephervex_minecraft::LaunchObserver;
use ephervex_model::{ConnectionTarget, InstanceId, ServerId};
use ephervex_server::{
    ControlAction, ControlResponse, RuntimeClassification, ServerError, ServerExit, ServerObserver,
    classify_runtime, send_control, wait_for_server_readiness,
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

/// What happens to a server this operation started once the operation returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StartedServerLifetime {
    /// Supervision continues on the caller's runtime after the client exits.
    /// The caller must keep that runtime alive for as long as the server
    /// should run.
    #[default]
    Detach,
    /// The server is stopped before the operation returns, whether the client
    /// launch succeeded or not. For a caller whose runtime ends with the
    /// operation.
    StopOnReturn,
}

/// Options for one linked launch. Client options retain their normal Java,
/// account, feature, and dry-run behavior. A dry run starts no server.
#[derive(Debug, Clone)]
pub struct LaunchAndConnectOptions {
    pub launch: LaunchOptions,
    pub readiness_timeout: Duration,
    pub started_server: StartedServerLifetime,
    /// Canceling this while a started server is being stopped on return kills
    /// it instead of waiting out its save. Never canceled by default.
    pub started_server_kill: CancellationToken,
}

impl Default for LaunchAndConnectOptions {
    fn default() -> Self {
        Self {
            launch: LaunchOptions::default(),
            readiness_timeout: DEFAULT_SERVER_READINESS_TIMEOUT,
            started_server: StartedServerLifetime::default(),
            started_server_kill: CancellationToken::new(),
        }
    }
}

/// What the combined operation produced after the client exited or a dry run
/// completed. Unless the options ask for [`StartedServerLifetime::StopOnReturn`],
/// a server started by this operation remains supervised by the application and
/// is not coupled to the client process lifetime.
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
    StartingServer {
        server: ServerId,
    },
    ReusingServer {
        server: ServerId,
    },
    WaitingForServer {
        target: ConnectionTarget,
    },
    ServerReady {
        target: ConnectionTarget,
    },
    /// A server this operation started is being stopped, which can take as
    /// long as the world needs to save.
    StoppingServer {
        server: ServerId,
    },
    /// The stop is being cut short because the caller asked to kill the server.
    KillingServer {
        server: ServerId,
    },
    /// The stopped server's supervisor has ended and released the server.
    ServerStopped {
        server: ServerId,
    },
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
        let client = self.current_client(client_id)?;
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
        // The handle reaches a lock file name only after the server is up, so
        // an unusable one is refused here instead of after a start and stop.
        if let Some(handle) = options.launch.additional.as_deref() {
            ephervex_storage::validate_launch_id(handle)?;
        }
        let availability = self.launch_and_connect_availability(client_id, server_id)?;
        // This early read avoids starting a stopped server for a client that
        // is already known to be unavailable. The ordinary launch lock remains
        // authoritative and closes a race with another launcher after this
        // check without introducing a second lock path here. An additional
        // launch is only unavailable when its own handle is already live.
        let blocking = self
            .running_launches(client_id)?
            .into_iter()
            .find(|running| match options.launch.additional.as_deref() {
                None => true,
                Some(handle) => running.id.as_deref() == Some(handle),
            });
        if let Some(running) = blocking {
            return Err(ephervex_storage::LockError::AlreadyRunning {
                pid: running.state.holder_pid(),
                since: running.state.since,
            }
            .into());
        }
        // Read before anything starts, so stopping what was started cannot be
        // skipped by a lookup that fails afterwards.
        let root = self.server(server_id)?.root;
        let mut started = None;
        let result = self
            .connect_and_launch(
                client_id,
                server_id,
                &availability,
                options,
                (observer, server_observer, launch_observer),
                cancel,
                &mut started,
            )
            .await;
        // A dropped JoinHandle detaches its task. The supervisor then remains
        // the sole owner of the server and is not coupled to the client
        // process or to a successful client spawn.
        if let Some(supervisor) = started
            && options.started_server == StartedServerLifetime::StopOnReturn
        {
            stop_and_report_started_server(
                server_id,
                &root,
                supervisor,
                &options.started_server_kill,
                observer,
            )
            .await;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_and_launch(
        &mut self,
        client_id: InstanceId,
        server_id: ServerId,
        availability: &LaunchAndConnectAvailability,
        options: &LaunchAndConnectOptions,
        (observer, server_observer, launch_observer): (
            &LaunchAndConnectObserver,
            &ServerObserver,
            &LaunchObserver,
        ),
        cancel: &CancellationToken,
        started: &mut Option<JoinHandle<CoreResult<ServerExit>>>,
    ) -> CoreResult<LaunchAndConnectOutcome> {
        // A dry run only reports the command, so it never starts a server and
        // has nothing to wait for.
        let starts_server =
            availability.server_preparation == ServerPreparation::Start && !options.launch.dry_run;
        if starts_server {
            observer.emit(LaunchAndConnectEvent::StartingServer { server: server_id });
            let prepared = self
                .prepare_server_start(server_id, None, server_observer, cancel)
                .await?;
            // Preparation can take long enough to be interrupted, and a server
            // started after that would only be killed again.
            if cancel.is_cancelled() {
                server_observer.emit(ephervex_server::ServerEvent::Finished { exit: None });
                return Err(ServerError::ReadinessCanceled.into());
            }
            let server_observer = server_observer.clone();
            *started = Some(tokio::spawn(async move {
                App::run_prepared_server(prepared, &server_observer).await
            }));
        } else if availability.server_preparation == ServerPreparation::Reuse {
            observer.emit(LaunchAndConnectEvent::ReusingServer { server: server_id });
        }
        if starts_server || availability.server_preparation == ServerPreparation::Reuse {
            observer.emit(LaunchAndConnectEvent::WaitingForServer {
                target: availability.target,
            });
            wait_for_managed_server(
                server_id,
                &self.server(server_id)?.root,
                availability.target,
                options.readiness_timeout,
                cancel,
                started,
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
        }

        let launch_options = connected_launch_options(&options.launch, availability.target);
        let launch = self
            .launch(client_id, &launch_options, launch_observer, cancel)
            .await?;
        Ok(LaunchAndConnectOutcome {
            client: client_id,
            server: server_id,
            target: availability.target,
            server_started: starts_server,
            launch,
        })
    }
}

/// Stops a server this operation started and says when the stop begins and
/// when the server is gone, because the wait can last minutes.
async fn stop_and_report_started_server(
    server: ServerId,
    root: &std::path::Path,
    supervisor: JoinHandle<CoreResult<ServerExit>>,
    kill: &CancellationToken,
    observer: &LaunchAndConnectObserver,
) {
    // A supervisor that already ended left nothing of ours to stop, and the
    // server under the same id may be a replacement that is still running.
    let running = !supervisor.is_finished();
    if running {
        observer.emit(if kill.is_cancelled() {
            LaunchAndConnectEvent::KillingServer { server }
        } else {
            LaunchAndConnectEvent::StoppingServer { server }
        });
    }
    let stopping = stop_started_server(root, supervisor, kill);
    tokio::pin!(stopping);
    let ended = tokio::select! {
        ended = &mut stopping => ended,
        () = kill.cancelled(), if running && !kill.is_cancelled() => {
            observer.emit(LaunchAndConnectEvent::KillingServer { server });
            stopping.await
        }
    };
    if running && ended.is_some() {
        observer.emit(LaunchAndConnectEvent::ServerStopped { server });
    }
}

/// Stops a server this operation started and waits for its supervisor to end,
/// so the world is saved and the runtime lock is released. Only the supervisor
/// this operation spawned is addressed: once it has ended, whatever owns the
/// root now is not ours to stop.
///
/// A stop the supervisor has accepted is waited out, because killing a server
/// mid-save is what this exists to avoid. A server that never answers, or whose
/// control channel is not published yet, is retried briefly and then killed.
/// Canceling `kill` ends the wait at once and kills the server.
///
/// Returns how the supervisor finished, or `None` when it had to be abandoned.
pub(super) async fn stop_started_server(
    root: &std::path::Path,
    mut supervisor: JoinHandle<CoreResult<ServerExit>>,
    kill: &CancellationToken,
) -> Option<Result<CoreResult<ServerExit>, tokio::task::JoinError>> {
    // Checked before `kill` is looked at: a canceled token must not reach a
    // server that replaced the one this operation started.
    if supervisor.is_finished() {
        return Some((&mut supervisor).await);
    }
    let graceful = tokio::select! {
        () = kill.cancelled() => None,
        ended = stop_gracefully(root, &mut supervisor) => ended,
    };
    if graceful.is_some() {
        return graceful;
    }
    tracing::warn!(root = %root.display(), "the server did not stop in time and was killed");
    let _ = send_control(root, &ControlAction::Kill).await;
    if let Ok(ended) = tokio::time::timeout(STARTED_SERVER_SETTLE, &mut supervisor).await {
        return Some(ended);
    }
    supervisor.abort();
    None
}

/// Asks the server to stop and waits for its supervisor to end, giving up
/// with `None` when the server does not stop in time.
async fn stop_gracefully(
    root: &std::path::Path,
    supervisor: &mut JoinHandle<CoreResult<ServerExit>>,
) -> Option<Result<CoreResult<ServerExit>, tokio::task::JoinError>> {
    let publication_deadline = tokio::time::Instant::now() + STARTED_SERVER_SETTLE;
    let mut delivered = false;
    while !supervisor.is_finished() {
        match send_control(root, &ControlAction::Stop).await {
            Ok(ControlResponse::Exited(_)) => break,
            // The supervisor wrote `stop` and the server is still saving.
            Err(ServerError::ControlUnavailable { source, .. })
                if source.kind() == std::io::ErrorKind::TimedOut =>
            {
                delivered = true;
                break;
            }
            _ if tokio::time::Instant::now() < publication_deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ => break,
        }
    }
    let grace = if delivered {
        STARTED_SERVER_STOP_GRACE
    } else {
        STARTED_SERVER_SETTLE
    };
    tokio::time::timeout(grace, supervisor).await.ok()
}

/// How long a stopped server's supervisor may take to release its runtime, and
/// how long a supervisor may take to publish its control channel.
const STARTED_SERVER_SETTLE: Duration = Duration::from_secs(10);

/// How long a server that accepted `stop` may take to save and exit before it
/// is killed.
const STARTED_SERVER_STOP_GRACE: Duration = Duration::from_secs(300);

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
    started: &mut Option<JoinHandle<CoreResult<ServerExit>>>,
) -> CoreResult<()> {
    let readiness = wait_for_server_readiness(root, target, timeout, cancel);
    tokio::pin!(readiness);
    if let Some(supervisor) = started.as_mut() {
        let ended = tokio::select! {
            result = &mut readiness => return Ok(result?),
            result = supervisor => result,
        };
        // A finished handle must not be awaited again when the caller stops
        // whatever it started.
        *started = None;
        match ended {
            Ok(Ok(_)) => Err(ConnectError::ServerStoppedBeforeReady { server }.into()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(ConnectError::SupervisorLost { server }.into()),
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
    let mut uncertain_since = None;
    loop {
        interval.tick().await;
        match classify_runtime(root) {
            Ok(RuntimeClassification::Controlled { .. }) => uncertain_since = None,
            Ok(RuntimeClassification::Stopped | RuntimeClassification::ProvenStale { .. }) => {
                return ConnectError::ServerStoppedBeforeReady { server }.into();
            }
            Ok(RuntimeClassification::UnknownRunning { .. }) => {
                let since = *uncertain_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= crate::server::RUNTIME_SETTLE {
                    return ConnectError::RuntimeUncertain { server }.into();
                }
            }
            Ok(RuntimeClassification::Orphaned { .. }) => {
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
    fn availability_compares_the_committed_client_target_while_the_index_row_lags() {
        let directory = tempfile::tempdir().expect("directory");
        let app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
        app.commit_client_without_index_for_test(client.id, |committed| {
            committed.loader = LoaderSelection::exact(LoaderKind::Forge, "47.4.5");
        })
        .expect("commit");
        assert_eq!(
            app.client(client.id)
                .expect("row")
                .loader
                .version
                .as_deref(),
            Some("47.4.0")
        );

        assert!(matches!(
            app.launch_and_connect_availability(client.id, server.id),
            Err(CoreError::Connect(ConnectError::TargetMismatch { .. }))
        ));
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

    fn alongside_options(handle: &str, dry_run: bool) -> LaunchAndConnectOptions {
        LaunchAndConnectOptions {
            launch: LaunchOptions {
                dry_run,
                additional: Some(handle.to_owned()),
                ..LaunchOptions::default()
            },
            ..LaunchAndConnectOptions::default()
        }
    }

    #[tokio::test]
    async fn an_additional_launch_connects_while_the_client_is_already_running() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
        let held = LaunchLock::acquire(&client.root).expect("client launch lock");

        let error = app
            .launch_and_connect(
                client.id,
                server.id,
                &alongside_options("second", true),
                &LaunchAndConnectObserver::silent(),
                &ServerObserver::silent(),
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("no account still means no launch");
        assert!(
            error.requires_interactive_login(),
            "the running client refused an additional launch: {error}"
        );
        drop(held);
    }

    #[tokio::test]
    async fn an_additional_launch_handle_that_is_already_live_is_refused_before_server_preparation()
    {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
        let held = LaunchLock::acquire_additional(&client.root, "second").expect("additional lock");
        let (observer, mut events) = LaunchAndConnectObserver::channel();

        let error = app
            .launch_and_connect(
                client.id,
                server.id,
                &alongside_options("second", false),
                &observer,
                &ServerObserver::silent(),
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a live handle is unavailable");
        assert!(matches!(
            error,
            CoreError::Storage(ephervex_storage::StorageError::Lock(
                ephervex_storage::LockError::AlreadyRunning { .. }
            ))
        ));
        assert!(events.try_recv().is_err(), "the server was not prepared");
        assert!(!server.root.join("server/eula.txt").exists());
        drop(held);
    }

    #[tokio::test]
    async fn an_unusable_additional_launch_handle_is_refused_before_server_preparation() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
        for handle in ["", "bad id", "../escape", "a/b", "a.lock"] {
            let (observer, mut events) = LaunchAndConnectObserver::channel();

            let error = app
                .launch_and_connect(
                    client.id,
                    server.id,
                    &alongside_options(handle, false),
                    &observer,
                    &ServerObserver::silent(),
                    &LaunchObserver::silent(),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("an unusable handle is refused");
            assert!(
                matches!(
                    error,
                    CoreError::Storage(ephervex_storage::StorageError::Lock(
                        ephervex_storage::LockError::InvalidLaunchId
                    ))
                ),
                "{handle:?} gave {error}"
            );
            assert!(events.try_recv().is_err(), "the server was not prepared");
            assert!(matches!(
                classify_runtime(&server.root).expect("runtime"),
                RuntimeClassification::Stopped
            ));
            assert!(!server.root.join("server/eula.txt").exists());
        }
    }

    #[tokio::test]
    async fn a_controlled_server_is_reused_and_client_authentication_stays_on_the_normal_path() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, mut server) = installed_linked_server(&app, ServerNetworkSettings::default());
        let properties = server.root.join("server/server.properties");
        std::fs::write(&properties, b"motd=unchanged\n").expect("properties");
        std::fs::write(server.root.join("server").join(STAND_IN_MARKER), b"").expect("marker");

        let root = server.root.clone();
        let capture = directory.path().join("capture");
        let task = tokio::spawn(async move {
            ServerSupervisor::run(
                &ServerCommand {
                    java: std::env::current_exe().expect("test executable"),
                    arguments: [
                        "--exact",
                        "connection::tests::a_supervised_stand_in_listens_until_told_to_stop",
                        "--ignored",
                        "--test-threads=1",
                    ]
                    .map(str::to_owned)
                    .to_vec(),
                    working_directory: root.join("server"),
                },
                &root,
                &capture,
                &ServerObserver::silent(),
            )
            .await
        });
        wait_until_controlled(&server.root).await;
        let Some(port) = stand_in_port(&server.root.join("server")).await else {
            let _ = send_control(&server.root, &ControlAction::Stop).await;
            panic!("the stand-in server never published its port");
        };
        server.network.port = port;
        app.registry_for_services()
            .update_server(&mut server)
            .expect("server port");
        let before = std::fs::read(&properties).expect("before");
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
                    // A server this operation did not start is left alone even
                    // when it is told to stop what it started.
                    started_server: StartedServerLifetime::StopOnReturn,
                    ..LaunchAndConnectOptions::default()
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
    async fn a_dry_run_starts_no_server_and_still_builds_the_connected_command() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = app_rooted_at(directory.path());
        let (client, server) = installed_linked_server(&app, ServerNetworkSettings::default());
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
                    ..LaunchAndConnectOptions::default()
                },
                &connect_observer,
                &server_observer,
                &LaunchObserver::silent(),
                &CancellationToken::new(),
            )
            .await
            .expect_err("no account still means no launch");
        assert!(
            error.requires_interactive_login(),
            "the client launch was reached without a server: {error}"
        );
        assert!(connect_events.try_recv().is_err(), "no server step ran");
        assert!(server_events.try_recv().is_err(), "no server was prepared");
        assert!(!server.root.join("server/eula.txt").exists());
        assert!(matches!(
            classify_runtime(&server.root).expect("runtime"),
            RuntimeClassification::Stopped
        ));
    }

    fn spawn_stand_in(root: &Path, capture: PathBuf) -> JoinHandle<CoreResult<ServerExit>> {
        spawn_stand_in_process(
            root,
            capture,
            "connection::tests::a_supervised_stand_in_listens_until_told_to_stop",
        )
    }

    fn spawn_stand_in_process(
        root: &Path,
        capture: PathBuf,
        test: &'static str,
    ) -> JoinHandle<CoreResult<ServerExit>> {
        let root = root.to_owned();
        tokio::spawn(async move {
            Ok(ServerSupervisor::run(
                &ServerCommand {
                    java: std::env::current_exe().expect("test executable"),
                    arguments: ["--exact", test, "--ignored", "--test-threads=1"]
                        .map(str::to_owned)
                        .to_vec(),
                    working_directory: root.join("server"),
                },
                &root,
                &capture,
                &ServerObserver::silent(),
            )
            .await?)
        })
    }

    fn stand_in_root(base: &Path) -> PathBuf {
        let root = base.join("root");
        std::fs::create_dir_all(root.join("server")).expect("server directory");
        std::fs::write(root.join("server").join(STAND_IN_MARKER), b"").expect("marker");
        root
    }

    #[tokio::test]
    async fn a_server_started_by_the_operation_is_stopped_gracefully_when_it_returns() {
        let directory = tempfile::tempdir().expect("directory");
        let root = stand_in_root(directory.path());
        let working_directory = root.join("server");
        let supervisor = spawn_stand_in(&root, directory.path().join("capture"));
        wait_until_controlled(&root).await;
        assert!(
            stand_in_port(&working_directory).await.is_some(),
            "the stand-in server never published its port"
        );

        stop_started_server(&root, supervisor, &CancellationToken::new()).await;

        assert!(
            working_directory.join(STAND_IN_STOPPED_FILE).exists(),
            "the server was asked to stop rather than killed"
        );
        assert!(matches!(
            classify_runtime(&root).expect("runtime"),
            RuntimeClassification::Stopped
        ));
    }

    #[tokio::test]
    async fn canceling_the_kill_token_ends_the_wait_for_a_server_that_is_still_saving() {
        let directory = tempfile::tempdir().expect("directory");
        let root = stand_in_root(directory.path());
        let working_directory = root.join("server");
        let supervisor = spawn_stand_in_process(
            &root,
            directory.path().join("capture"),
            "connection::tests::a_supervised_stand_in_ignores_stop",
        );
        wait_until_controlled(&root).await;
        let kill = CancellationToken::new();
        let stopping = tokio::spawn({
            let root = root.clone();
            let kill = kill.clone();
            async move { stop_started_server(&root, supervisor, &kill).await }
        });
        tokio::time::timeout(Duration::from_secs(30), async {
            while !working_directory.join(STAND_IN_STOP_RECEIVED_FILE).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the server received the stop");
        assert!(!stopping.is_finished(), "the server is still saving");

        kill.cancel();

        let ended = tokio::time::timeout(Duration::from_secs(30), stopping)
            .await
            .expect("the kill ended the wait without the save grace")
            .expect("stop task");
        assert!(ended.is_some(), "the killed supervisor ended");
        assert!(matches!(
            classify_runtime(&root).expect("runtime"),
            RuntimeClassification::Stopped
        ));
    }

    #[tokio::test]
    async fn stopping_a_started_server_reports_when_the_stop_begins_and_when_it_ends() {
        let directory = tempfile::tempdir().expect("directory");
        let root = stand_in_root(directory.path());
        let supervisor = spawn_stand_in(&root, directory.path().join("capture"));
        wait_until_controlled(&root).await;
        let server = ServerId::new();
        let (observer, mut events) = LaunchAndConnectObserver::channel();

        stop_and_report_started_server(
            server,
            &root,
            supervisor,
            &CancellationToken::new(),
            &observer,
        )
        .await;

        assert_eq!(
            events.try_recv().expect("stopping"),
            LaunchAndConnectEvent::StoppingServer { server }
        );
        assert_eq!(
            events.try_recv().expect("stopped"),
            LaunchAndConnectEvent::ServerStopped { server }
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_kill_requested_while_a_started_server_saves_is_reported_before_it_stops() {
        let directory = tempfile::tempdir().expect("directory");
        let root = stand_in_root(directory.path());
        let working_directory = root.join("server");
        let supervisor = spawn_stand_in_process(
            &root,
            directory.path().join("capture"),
            "connection::tests::a_supervised_stand_in_ignores_stop",
        );
        wait_until_controlled(&root).await;
        let server = ServerId::new();
        let kill = CancellationToken::new();
        let (observer, mut events) = LaunchAndConnectObserver::channel();
        let stopping = tokio::spawn({
            let root = root.clone();
            let kill = kill.clone();
            async move {
                stop_and_report_started_server(server, &root, supervisor, &kill, &observer).await;
            }
        });
        tokio::time::timeout(Duration::from_secs(30), async {
            while !working_directory.join(STAND_IN_STOP_RECEIVED_FILE).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the server received the stop");

        kill.cancel();

        tokio::time::timeout(Duration::from_secs(30), stopping)
            .await
            .expect("the kill ended the wait")
            .expect("stop task");
        let mut reported = Vec::new();
        while let Ok(event) = events.try_recv() {
            reported.push(event);
        }
        assert_eq!(
            reported,
            [
                LaunchAndConnectEvent::StoppingServer { server },
                LaunchAndConnectEvent::KillingServer { server },
                LaunchAndConnectEvent::ServerStopped { server },
            ]
        );
    }

    #[tokio::test]
    async fn a_server_that_replaced_the_one_this_operation_started_is_left_running() {
        assert_replacement_left_running(CancellationToken::new()).await;
    }

    #[tokio::test]
    async fn a_canceled_kill_token_does_not_reach_a_server_that_replaced_the_one_this_operation_started()
     {
        let kill = CancellationToken::new();
        kill.cancel();
        assert_replacement_left_running(kill).await;
    }

    #[tokio::test]
    async fn no_stop_is_reported_for_a_started_server_that_had_already_ended() {
        let directory = tempfile::tempdir().expect("directory");
        let root = stand_in_root(directory.path());
        let replacement = spawn_stand_in(&root, directory.path().join("capture"));
        wait_until_controlled(&root).await;
        let ended = finished_supervisor().await;
        let (observer, mut events) = LaunchAndConnectObserver::channel();

        stop_and_report_started_server(
            ServerId::new(),
            &root,
            ended,
            &CancellationToken::new(),
            &observer,
        )
        .await;

        assert!(
            events.try_recv().is_err(),
            "the replacement was not stopped"
        );
        let _ = send_control(&root, &ControlAction::Stop).await;
        let _ = replacement.await;
    }

    async fn finished_supervisor() -> JoinHandle<CoreResult<ServerExit>> {
        let ended = tokio::spawn(async {
            Ok(ServerExit {
                code: Some(0),
                signal: None,
                finished_starting: true,
                stop_requested: true,
            })
        });
        while !ended.is_finished() {
            tokio::task::yield_now().await;
        }
        ended
    }

    async fn assert_replacement_left_running(kill: CancellationToken) {
        let directory = tempfile::tempdir().expect("directory");
        let root = stand_in_root(directory.path());
        let working_directory = root.join("server");
        let replacement = spawn_stand_in(&root, directory.path().join("capture"));
        wait_until_controlled(&root).await;
        assert!(
            stand_in_port(&working_directory).await.is_some(),
            "the stand-in server never published its port"
        );
        let ended = finished_supervisor().await;

        stop_started_server(&root, ended, &kill).await;

        assert!(
            !working_directory.join(STAND_IN_STOPPED_FILE).exists(),
            "a server this operation did not start was not told to stop"
        );
        assert!(matches!(
            classify_runtime(&root).expect("runtime"),
            RuntimeClassification::Controlled { .. }
        ));
        let _ = send_control(&root, &ControlAction::Stop).await;
        let _ = replacement.await;
    }

    #[tokio::test]
    async fn readiness_propagates_start_failure_exit_and_timeout_before_client_launch() {
        let directory = tempfile::tempdir().expect("directory");
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let mut evidence =
            ephervex_server::RuntimeEvidence::for_current_supervisor().expect("evidence");
        evidence.record_child(std::process::id()).expect("child");
        ephervex_server::write_runtime_evidence(directory.path(), &evidence).expect("evidence");
        let address = listener.local_addr().expect("address");
        let target = local_target(address.port());
        let mut running = Some(tokio::spawn(
            std::future::pending::<CoreResult<ServerExit>>(),
        ));
        wait_for_managed_server(
            ServerId::new(),
            directory.path(),
            target,
            Duration::from_secs(1),
            &CancellationToken::new(),
            &mut running,
        )
        .await
        .expect("listening target is ready");
        running.expect("still running").abort();
        drop(listener);

        // Bound but never listening, so no other test's listener can take the
        // port. Held until the test ends.
        let reserved = tokio::net::TcpSocket::new_v4().expect("socket");
        reserved
            .bind((Ipv4Addr::LOCALHOST, 0).into())
            .expect("reserve a loopback port");
        let target = local_target(reserved.local_addr().expect("address").port());
        let server = ServerId::new();
        let mut exited = Some(tokio::spawn(async {
            Ok(ServerExit {
                code: Some(1),
                signal: None,
                finished_starting: false,
                stop_requested: false,
            })
        }));
        assert!(matches!(
            wait_for_managed_server(
                server,
                directory.path(),
                target,
                Duration::from_secs(1),
                &CancellationToken::new(),
                &mut exited,
            )
            .await,
            Err(CoreError::Connect(
                ConnectError::ServerStoppedBeforeReady { .. }
            ))
        ));

        let executable = PathBuf::from("/missing/server-java");
        let mut failed = Some(tokio::spawn(async move {
            Err(ServerError::ProcessStart {
                executable,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            }
            .into())
        }));
        assert!(matches!(
            wait_for_managed_server(
                server,
                directory.path(),
                target,
                Duration::from_secs(1),
                &CancellationToken::new(),
                &mut failed,
            )
            .await,
            Err(CoreError::Server(ServerError::ProcessStart { .. }))
        ));

        let mut pending = Some(tokio::spawn(
            std::future::pending::<CoreResult<ServerExit>>(),
        ));
        assert!(matches!(
            wait_for_managed_server(
                server,
                directory.path(),
                target,
                Duration::from_millis(25),
                &CancellationToken::new(),
                &mut pending,
            )
            .await,
            Err(CoreError::Server(ServerError::ReadinessTimeout { .. }))
        ));
        pending.expect("still running").abort();
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

    /// Present in a stand-in server's working directory, so running ignored
    /// tests by hand never starts one.
    const STAND_IN_MARKER: &str = "listening-stand-in";
    const STAND_IN_PORT_FILE: &str = "bound-port";
    /// Written by the stand-in only after it reads `stop` or loses its input.
    const STAND_IN_STOPPED_FILE: &str = "stopped-by-request";

    /// The supervised server process for the tests that need a listening
    /// server: it holds a loopback listener, publishes its port, and exits on
    /// `stop`.
    #[test]
    #[ignore = "started as a server process by the reuse test"]
    fn a_supervised_stand_in_listens_until_told_to_stop() {
        if !Path::new(STAND_IN_MARKER).exists() {
            return;
        }
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let port = listener.local_addr().expect("address").port();
        let staged = format!("{STAND_IN_PORT_FILE}.tmp");
        std::fs::write(&staged, port.to_string()).expect("port");
        std::fs::rename(&staged, STAND_IN_PORT_FILE).expect("publish port");
        for line in std::io::stdin().lines() {
            if line.is_err() || line.is_ok_and(|line| line == "stop") {
                break;
            }
        }
        drop(listener);
        std::fs::write(STAND_IN_STOPPED_FILE, b"").expect("record the stop");
    }

    /// Written by the slow stand-in when it reads `stop`, which it then ignores.
    const STAND_IN_STOP_RECEIVED_FILE: &str = "stop-received";

    /// A supervised server that accepts `stop` but never finishes saving, so
    /// only a kill ends it.
    #[test]
    #[ignore = "started as a server process by the stop escalation test"]
    fn a_supervised_stand_in_ignores_stop() {
        if !Path::new(STAND_IN_MARKER).exists() {
            return;
        }
        for line in std::io::stdin().lines() {
            if line.is_err() {
                break;
            }
            std::fs::write(STAND_IN_STOP_RECEIVED_FILE, b"").expect("record the stop");
        }
    }

    async fn stand_in_port(directory: &Path) -> Option<u16> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(port) = std::fs::read_to_string(directory.join(STAND_IN_PORT_FILE)) {
                    return port.parse().expect("port");
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .ok()
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
