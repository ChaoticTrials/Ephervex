//! Non-blocking bridge from Slint callbacks to `ephervex-core` services.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use chrono::{DateTime, Utc};
use ephervex_core::{
    App, AuthEvent, AuthObserver, CancellationToken, ClientFolder, CoreError, DiagnosticKind,
    DiagnosticRequest, DiagnosticScope, EulaStatus, GlobalMinecraftPreferences, ImportRequest,
    LaunchAndConnectObserver, LaunchAndConnectOptions, LaunchEvent, LaunchObserver, LaunchOptions,
    LoginOptions, ModInstallRequest, ModSearchQuery, NewClient, NewServer, OperationReporter,
    Paths, PreferenceKey, ProgressSink, SearchOptions, ServerCreateOptions, ServerEvent,
    ServerFolder, ServerNetworkSettings, ServerObserver,
};
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute, lookup_attribute_args};
use ephervex_model::{
    InstanceId, LoaderKind, LoaderSelection, Operation, OperationStatus, ServerId, StorageClass,
};

use crate::presentation::{
    ActivityPresentation, InstancePresentation, ServerPresentation, diagnostic_kind_label,
    diagnostic_scope_label,
};

type CancellationRegistry = Arc<Mutex<HashMap<String, CancellationToken>>>;

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
    CancelOperation(String),
    PickImport,
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
    },
    LaunchClient(InstanceId),
    LaunchAndConnect {
        client: InstanceId,
        server: ServerId,
    },
    OpenClientFolder(InstanceId),
    OpenServerFolder(ServerId),
    DuplicateClient(InstanceId),
    DeleteClient {
        id: InstanceId,
        files: bool,
    },
    DeleteServer {
        id: ServerId,
        files: bool,
    },
    LoadInstanceWorkspace(InstanceId),
    SearchMods {
        instance: InstanceId,
        query: String,
    },
    PlanMod {
        instance: InstanceId,
        project: String,
    },
    InstallMod {
        instance: InstanceId,
        project: String,
    },
    CreateServer {
        request_id: String,
        name: String,
        client: Option<InstanceId>,
        minecraft: String,
        loader: String,
        loader_version: String,
        persistent: bool,
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
    SaveStorageRoot(String),
    RefreshJava,
    SaveJavaSettings {
        auto_detect: bool,
        auto_download: bool,
        minimum_mib: u32,
        maximum_mib: u32,
        preferred_path: String,
    },
    SetPreference {
        key: PreferenceKey,
        value: String,
    },
    ResetPreference(PreferenceKey),
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub instances: Vec<InstancePresentation>,
    pub servers: Vec<ServerPresentation>,
    pub accounts: Vec<AccountData>,
    pub preferences: Vec<PreferenceData>,
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

#[derive(Debug, Clone)]
pub struct PreferenceData {
    pub key: String,
    pub label: String,
    pub value: String,
    pub customized: bool,
    pub kind: String,
    pub syntax: String,
}

#[derive(Debug, Clone, Default)]
pub struct SettingsData {
    pub storage_root: String,
    pub persistent_root: String,
    pub microsoft_client_id: String,
    pub user_agent: String,
    pub java_auto_detect: bool,
    pub java_auto_download: bool,
    pub java_memory_min: u32,
    pub java_memory_max: u32,
    pub java_preferred_path: String,
}

#[derive(Debug, Clone)]
pub struct EulaData {
    pub accepted: bool,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct ManagedModData {
    pub project_id: String,
    pub title: String,
    pub version: String,
    pub provider: String,
    pub origin: String,
}

#[derive(Debug, Clone)]
pub struct ModSearchData {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: String,
}

#[derive(Debug, Clone)]
pub struct ModPlanData {
    pub instance: String,
    pub project: String,
    pub title: String,
    pub version: String,
    pub install_count: usize,
    pub dependency_count: usize,
    pub optional_count: usize,
    pub already_installed_count: usize,
    pub warning_count: usize,
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
    pub console_lines: Vec<String>,
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
    Snapshot(Snapshot),
    MinecraftReleases(Vec<String>),
    LoaderVersions {
        form: String,
        loader: LoaderKind,
        minecraft: String,
        versions: Vec<String>,
    },
    ManagedMods(Vec<ManagedModData>),
    ModResults(Vec<ModSearchData>),
    ModPlan(ModPlanData),
    ServerWorkspace(ServerWorkspaceData),
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
    ImportAnalysis {
        path: PathBuf,
        suggested_name: String,
        summary: String,
    },
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
            Command::CancelOperation(id) => cancel_operation(&cancellations, &events, &id),
            Command::PickImport => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(lookup("gui-choose-modpack-archive"))
                    .add_filter(lookup("gui-modpack-archives"), &["mrpack", "zip"])
                    .pick_file()
                {
                    match app.analyze_pack(&path) {
                        Ok(analysis) => {
                            let summary = lookup_args(
                                "gui-import-analysis",
                                [
                                    ("first", Arg::from(analysis.format.to_string())),
                                    ("second", Arg::from(analysis.minecraft_version())),
                                    ("third", Arg::from(analysis.loader().to_string())),
                                    ("count", Arg::from(analysis.recipe.files.len())),
                                ],
                            );
                            let _ = events.send(Event::ImportAnalysis {
                                path,
                                suggested_name: analysis.name().to_owned(),
                                summary,
                            });
                        }
                        Err(error) => send_modal_error(&events, &error),
                    }
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
            } => {
                let paths = app.paths().clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    "operation-importing-pack",
                    archive.display().to_string(),
                    move |app, reporter, cancel| async move {
                        let mut request =
                            ImportRequest::new(archive).with_storage(storage_class(persistent));
                        if !name.trim().is_empty() {
                            request = request.named(name);
                        }
                        app.import_pack(&request, &reporter, &cancel)
                            .await
                            .map(|pack| localized_value("gui-imported-pack", &pack.record.name))
                    },
                );
            }
            Command::LaunchClient(instance) => {
                let paths = app.paths().clone();
                let entity = app
                    .client(instance)
                    .map_or_else(|_| instance.to_string(), |record| record.name);
                spawn_launch(paths, &events, instance, entity);
            }
            Command::LaunchAndConnect { client, server } => {
                let paths = app.paths().clone();
                let entity = app
                    .client(client)
                    .map_or_else(|_| format!("{client} -> {server}"), |record| record.name);
                spawn_connect(paths, &events, client, server, entity);
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
            Command::DuplicateClient(instance) => {
                match app.duplicate_client(instance, &ephervex_core::DuplicateClient::default()) {
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
            Command::LoadInstanceWorkspace(instance) => {
                let mods = app.managed_mods(instance).map(|mods| {
                    mods.into_iter()
                        .map(|item| ManagedModData {
                            project_id: item.project_id,
                            title: item.file_name,
                            version: item.version_id,
                            provider: item.provider.to_string(),
                            origin: item.origin.as_str().replace('_', " "),
                        })
                        .collect()
                });
                match mods {
                    Ok(mods) => {
                        let _ = events.send(Event::ManagedMods(mods));
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::SearchMods { instance, query } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let result =
                        runtime.block_on(app.search_mods(instance, &ModSearchQuery::new(query)));
                    match result {
                        Ok(results) => {
                            let rows = results
                                .hits
                                .into_iter()
                                .map(|hit| ModSearchData {
                                    project_id: hit.project.id,
                                    title: hit.project.title,
                                    description: hit.project.description,
                                    author: hit
                                        .project
                                        .author
                                        .unwrap_or_else(|| lookup("gui-unknown-author")),
                                    downloads: lookup("gui-compatible"),
                                })
                                .collect();
                            let _ = events.send(Event::ModResults(rows));
                        }
                        Err(error) => send_error(&events, &error),
                    }
                });
            }
            Command::PlanMod { instance, project } => {
                let paths = app.paths().clone();
                spawn_detached(paths, events.clone(), move |app, events, runtime| {
                    let request = ModInstallRequest::new(project.clone());
                    match runtime.block_on(app.plan_mod_installation(instance, &request)) {
                        Ok(plan) => {
                            let _ = events.send(Event::ModPlan(ModPlanData {
                                instance: instance.to_string(),
                                project,
                                title: plan.root.project.title.clone(),
                                version: plan.root.version.version_number.clone(),
                                install_count: plan.to_install().len(),
                                dependency_count: plan.dependencies.len(),
                                optional_count: plan.optional.len(),
                                already_installed_count: plan.already_installed.len(),
                                warning_count: plan.warnings.len(),
                            }));
                        }
                        Err(error) => send_error(&events, &error),
                    }
                });
            }
            Command::InstallMod { instance, project } => {
                let paths = app.paths().clone();
                spawn_async_operation(
                    paths,
                    &events,
                    Arc::clone(&cancellations),
                    "operation-installing-mods",
                    project.clone(),
                    move |app, reporter, cancel| async move {
                        let request = ModInstallRequest::new(project);
                        let plan = app.plan_mod_installation(instance, &request).await?;
                        let count = plan.all_mods().len();
                        app.install_mods(instance, &request, &reporter, &cancel)
                            .await?;
                        Ok(localized_count("gui-installed-managed-mods", count))
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
                    Ok(request) => {
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
            Command::LoadServerWorkspace(server) => send_server_workspace(&app, server, &events),
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
            Command::SetPreference { key, value } => {
                let result = app.set_minecraft_preference(key, &value).map(|_| ());
                match result {
                    Ok(()) => {
                        let _ = events.send(Event::Toast {
                            message: lookup("gui-minecraft-default-saved"),
                            error: false,
                        });
                        send_snapshot(&app, &events);
                    }
                    Err(error) => send_error(&events, &error),
                }
            }
            Command::ResetPreference(key) => match app.reset_minecraft_preference(key) {
                Ok(()) => {
                    let _ = events.send(Event::Toast {
                        message: lookup("gui-minecraft-default-reset"),
                        error: false,
                    });
                    send_snapshot(&app, &events);
                }
                Err(error) => send_error(&events, &error),
            },
        }
    }
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
            let _ = events.send(Event::Snapshot(snapshot));
        }
        Err(error) => send_error(events, &error),
    }
}

fn snapshot(app: &App) -> Result<Snapshot, CoreError> {
    let servers = app.list_servers()?;
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
        server_rows.push(ServerPresentation::from_status(&status));
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
    let loaded = app.minecraft_preferences();
    let inherited = GlobalMinecraftPreferences::default();
    let preferences = PreferenceKey::ALL
        .iter()
        .map(|key| preference_data(*key, &loaded.preferences, &inherited))
        .collect();
    if !loaded.rejected.is_empty() {
        tracing::warn!(
            count = loaded.rejected.len(),
            "some configured Minecraft preferences are invalid"
        );
    }
    let api = app.api_settings()?;
    let java = app.java_settings();
    Ok(Snapshot {
        instances: instance_rows,
        servers: server_rows,
        accounts,
        preferences,
        settings: SettingsData {
            storage_root: app.paths().persistent_instances_dir().display().to_string(),
            persistent_root: app.config().persistent_instance_root.clone(),
            microsoft_client_id: api.microsoft_client_id,
            user_agent: api.user_agent,
            java_auto_detect: java.auto_detect,
            java_auto_download: java.auto_download,
            java_memory_min: java.min_memory_mib,
            java_memory_max: java.max_memory_mib,
            java_preferred_path: java
                .preferred_path
                .map_or_else(String::new, |path| path.display().to_string()),
        },
        eula: eula_data(&app.server_eula_status()),
    })
}

fn preference_data(
    key: PreferenceKey,
    preferences: &GlobalMinecraftPreferences,
    inherited: &GlobalMinecraftPreferences,
) -> PreferenceData {
    let customized = preferences.is_managed(key);
    PreferenceData {
        key: key.as_str().to_owned(),
        label: preference_label(key),
        value: if customized {
            preferences.value(key)
        } else {
            inherited.value(key)
        }
        .to_setting_value(),
        customized,
        kind: preference_kind(key).to_owned(),
        syntax: key.value_syntax().to_owned(),
    }
}

fn send_server_workspace(app: &App, server: ServerId, events: &Sender<Event>) {
    let result = (|| {
        let record = app.server(server)?;
        let properties = app.server_properties(server)?;
        let capture = app.server_output(server, 500)?;
        Ok::<_, CoreError>(ServerWorkspaceData {
            console_lines: capture
                .lines
                .into_iter()
                .map(|line| {
                    format!(
                        "{} [{:?}] {}{}",
                        line.timestamp.format("%H:%M:%S"),
                        line.stream,
                        line.text,
                        if line.truncated {
                            " ... [truncated]"
                        } else {
                            ""
                        }
                    )
                })
                .collect(),
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

fn spawn_launch(paths: Paths, events: &Sender<Event>, instance: InstanceId, entity: String) {
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |mut app, events, runtime| {
            let (observer, mut receiver) = LaunchObserver::channel();
            let cancel = CancellationToken::new();
            let event_sender = events.clone();
            let entity_for_events = entity.clone();
            let paths_for_events = paths.clone();
            let observed_operation = Arc::new(Mutex::new(None::<String>));
            let forwarded_operation = Arc::clone(&observed_operation);
            runtime.block_on(async move {
                let forward = tokio::spawn(async move {
                    while let Some(event) = receiver.recv().await {
                        forward_launch_event(
                            &event_sender,
                            &entity_for_events,
                            &forwarded_operation,
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
                        );
                        send_error(&events, &error);
                    }
                }
                if let Ok(app) = App::open_at(paths) {
                    send_snapshot(&app, &events);
                }
            });
        },
    );
}

fn forward_launch_event(
    events: &Sender<Event>,
    entity: &str,
    observed_operation: &Mutex<Option<String>>,
    paths: &Paths,
    event: LaunchEvent,
) {
    match event {
        LaunchEvent::Preparing { operation } => {
            if let Ok(mut observed) = observed_operation.lock() {
                *observed = Some(operation.to_string());
            }
            send_activity_state(
                events,
                operation.to_string(),
                &lookup("gui-activity-launching-minecraft"),
                entity,
                &lookup("gui-activity-preparing-environment"),
                "running",
                true,
                false,
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
                        match event {
                            ServerEvent::Preparing { operation } => {
                                if let Ok(mut observed) = forwarded_operation.lock() {
                                    *observed = Some(operation.to_string());
                                }
                                send_activity_state(
                                    &event_sender,
                                    operation.to_string(),
                                    &lookup("gui-activity-starting-server"),
                                    &entity_for_events,
                                    &lookup("gui-activity-validating-server"),
                                    "running",
                                    true,
                                    false,
                                );
                                refresh_snapshot(&paths_for_events, &event_sender);
                            }
                            ServerEvent::Started { .. } => {
                                let _ = event_sender.send(Event::Toast {
                                    message: localized_value(
                                        "gui-server-running",
                                        &entity_for_events,
                                    ),
                                    error: false,
                                });
                                refresh_snapshot(&paths_for_events, &event_sender);
                            }
                            ServerEvent::Warning(warning) => {
                                let _ = event_sender.send(Event::Toast {
                                    message: server_warning_message(&warning),
                                    error: false,
                                });
                            }
                            _ => {}
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

fn spawn_connect(
    paths: Paths,
    events: &Sender<Event>,
    client: InstanceId,
    server: ServerId,
    entity: String,
) {
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |mut app, events, runtime| {
            runtime.block_on(async move {
                let (connect_observer, _connect_events) = LaunchAndConnectObserver::channel();
                let (server_observer, _server_events) = ServerObserver::channel();
                let (launch_observer, _launch_events) = LaunchObserver::channel();
                let cancel = CancellationToken::new();
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
                    ),
                    Err(error) => send_error(&events, &error),
                }
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

fn refresh_snapshot(paths: &Paths, events: &Sender<Event>) {
    if let Ok(app) = App::open_at(paths.clone()) {
        send_snapshot(&app, events);
    }
}

fn spawn_async_operation<F, Fut>(
    paths: Paths,
    events: &Sender<Event>,
    cancellations: CancellationRegistry,
    title: &'static str,
    entity: String,
    operation: F,
) where
    F: FnOnce(App, OperationReporter, CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<String, CoreError>> + 'static,
{
    spawn_detached(
        paths.clone(),
        events.clone(),
        move |app, events, runtime| {
            let sink = Arc::new(UiProgressSink {
                events: events.clone(),
                entity,
                allow_cancel: true,
            });
            let reporter = OperationReporter::new(title, true, sink);
            reporter.begin(None);
            let cancel = CancellationToken::new();
            register_cancellation(&cancellations, &reporter, &cancel);
            let result = runtime.block_on(operation(app, reporter.clone(), cancel));
            remove_cancellation(&cancellations, &reporter);
            match result {
                Ok(message) => {
                    reporter.finish(OperationStatus::Completed);
                    let _ = events.send(Event::CloseModal);
                    let _ = events.send(Event::Toast {
                        message,
                        error: false,
                    });
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
    if let Ok(mut entries) = cancellations.lock() {
        entries.insert(reporter.id().to_string(), token.clone());
    }
}

fn remove_cancellation(cancellations: &CancellationRegistry, reporter: &OperationReporter) {
    if let Ok(mut entries) = cancellations.lock() {
        entries.remove(&reporter.id().to_string());
    }
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

#[derive(Debug)]
struct UiProgressSink {
    events: Sender<Event>,
    entity: String,
    allow_cancel: bool,
}

impl ProgressSink for UiProgressSink {
    fn operation_changed(&self, operation: &Operation) {
        send_operation(
            &self.events,
            operation,
            self.entity.clone(),
            self.allow_cancel,
        );
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
            cancellable: false,
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

fn localized_value(message_id: &str, value: &str) -> String {
    lookup_args(message_id, [("value", Arg::from(value))])
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

fn send_error(events: &Sender<Event>, error: &CoreError) {
    tracing::warn!(message_id = error.message_id(), detail = %error, "GUI operation failed");
    let _ = events.send(Event::Toast {
        message: localized_error(error),
        error: true,
    });
}

fn send_modal_error(events: &Sender<Event>, error: &CoreError) {
    tracing::warn!(message_id = error.message_id(), detail = %error, "GUI form submission failed");
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

fn preference_label(key: PreferenceKey) -> String {
    match key {
        PreferenceKey::GuiScale => lookup("gui-preference-gui-scale"),
        PreferenceKey::Language => lookup("gui-preference-language"),
        PreferenceKey::Fullscreen => lookup("gui-preference-fullscreen"),
        PreferenceKey::WindowSize => lookup("gui-preference-window-size"),
        PreferenceKey::Fov => lookup("gui-preference-fov"),
        PreferenceKey::RenderDistance => lookup("gui-preference-render-distance"),
        PreferenceKey::SimulationDistance => lookup("gui-preference-simulation-distance"),
    }
}

fn preference_kind(key: PreferenceKey) -> &'static str {
    match key {
        PreferenceKey::Fullscreen => "boolean",
        PreferenceKey::GuiScale => "choice",
        PreferenceKey::Fov => "slider",
        PreferenceKey::RenderDistance | PreferenceKey::SimulationDistance => "integer",
        PreferenceKey::Language | PreferenceKey::WindowSize => "text",
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
    fn unmanaged_remembered_values_are_presented_as_inherited_defaults() {
        let loaded = GlobalMinecraftPreferences::from_settings([("render_distance", false, "16")]);
        let inherited = GlobalMinecraftPreferences::default();

        let row = preference_data(
            PreferenceKey::RenderDistance,
            &loaded.preferences,
            &inherited,
        );

        assert!(!row.customized);
        assert_eq!(row.value, "12");
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
}
