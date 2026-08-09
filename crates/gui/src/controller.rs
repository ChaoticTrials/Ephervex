//! Slint callback registration and UI model updates.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use copypasta::{ClipboardContext, ClipboardProvider as _};
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ComponentHandle as _, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::backend::{
    self, AccountData, ActivityData, Command, DiagnosticContent, DiagnosticData, Event, JavaData,
    ManagedModData, ModSearchData, PreferenceData, ServerWorkspaceData, Snapshot,
};
use crate::presentation::{parse_network, validate_ip, validate_port};
use crate::ui::{
    AccountRow, ActivityRow, AppWindow, DiagnosticRow, GuiI18n, InstanceRow, JavaRow, ModRow,
    ModSearchRow, PreferenceRow, ServerRow,
};

pub struct Controller {
    _poll_timer: Timer,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Controller").finish_non_exhaustive()
    }
}

pub fn attach(ui: &AppWindow) -> Controller {
    let backend = backend::spawn();
    let receiver = Rc::new(RefCell::new(backend.events));
    let activities = Rc::new(RefCell::new(Vec::<ActivityData>::new()));
    register_i18n(ui);
    register_callbacks(ui, &backend.commands);

    let weak = ui.as_weak();
    let poll_receiver = Rc::clone(&receiver);
    let poll_activities = Rc::clone(&activities);
    let poll_timer = Timer::default();
    poll_timer.start(TimerMode::Repeated, Duration::from_millis(80), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let window = ui.window();
        let logical_width = f64::from(window.size().width) / f64::from(window.scale_factor());
        let show_inspector = logical_width >= 1180.0;
        if ui.get_show_inspector() != show_inspector {
            ui.set_show_inspector(show_inspector);
        }
        for event in poll_receiver.borrow_mut().try_iter() {
            apply_event(&ui, event, &poll_activities);
        }
    });

    Controller {
        _poll_timer: poll_timer,
    }
}

fn register_i18n(ui: &AppWindow) {
    let i18n = ui.global::<GuiI18n>();
    i18n.on_text(|id| lookup(id.as_str()).into());
    i18n.on_count(|id, count| lookup_args(id.as_str(), [("count", Arg::from(count))]).into());
    i18n.on_value(|id, value| {
        lookup_args(id.as_str(), [("value", Arg::from(value.as_str()))]).into()
    });
    i18n.on_values(|id, first, second| {
        lookup_args(
            id.as_str(),
            [
                ("first", Arg::from(first.as_str())),
                ("second", Arg::from(second.as_str())),
            ],
        )
        .into()
    });
}

#[allow(clippy::too_many_lines)]
fn register_callbacks(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_refresh(move || send(&sender, Command::Refresh));

    let sender = commands.clone();
    ui.on_request_version_catalog(move || send(&sender, Command::LoadMinecraftReleases));

    let sender = commands.clone();
    ui.on_request_loader_catalog(move |form, loader, minecraft| {
        let loader = match loader.as_str() {
            "Forge" => Some(ephervex_model::LoaderKind::Forge),
            "NeoForge" => Some(ephervex_model::LoaderKind::NeoForge),
            _ => None,
        };
        if let Some(loader) = loader {
            send(
                &sender,
                Command::LoadLoaderVersions {
                    form: form.into(),
                    loader,
                    minecraft: minecraft.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_submit_create_instance(move |name, minecraft, loader, loader_version, persistent| {
        send(
            &sender,
            Command::CreateClient {
                name: name.into(),
                minecraft: minecraft.into(),
                loader: loader.into(),
                loader_version: loader_version.into(),
                persistent,
            },
        );
    });

    let sender = commands.clone();
    ui.on_choose_import_file(move || send(&sender, Command::PickImport));

    let sender = commands.clone();
    ui.on_submit_import(move |archive, name, persistent| {
        send(
            &sender,
            Command::ImportPack {
                archive: PathBuf::from(archive.as_str()),
                name: name.into(),
                persistent,
            },
        );
    });

    let sender = commands.clone();
    ui.on_select_instance(move |_| send(&sender, Command::Refresh));

    let sender = commands.clone();
    ui.on_launch_client(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::LaunchClient(id));
        }
    });

    let sender = commands.clone();
    ui.on_launch_and_connect(move |client, server| {
        if let (Some(client), Some(server)) = (
            backend::parse_instance_id(&client),
            backend::parse_server_id(&server),
        ) {
            send(&sender, Command::LaunchAndConnect { client, server });
        }
    });

    let sender = commands.clone();
    ui.on_open_client_folder(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::OpenClientFolder(id));
        }
    });

    let sender = commands.clone();
    ui.on_duplicate_client(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::DuplicateClient(id));
        }
    });

    let sender = commands.clone();
    ui.on_delete_client(move |id, files| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::DeleteClient { id, files });
        }
    });

    let sender = commands.clone();
    ui.on_delete_server(move |id, files| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::DeleteServer { id, files });
        }
    });

    let sender = commands.clone();
    ui.on_load_instance_workspace(move |id| {
        if let Some(id) = backend::parse_instance_id(&id) {
            send(&sender, Command::LoadInstanceWorkspace(id));
        }
    });

    let sender = commands.clone();
    ui.on_search_mods(move |instance, query| {
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::SearchMods {
                    instance,
                    query: query.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_review_mod(move |instance, project| {
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::PlanMod {
                    instance,
                    project: project.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_install_mod(move |instance, project| {
        if let Some(instance) = backend::parse_instance_id(&instance) {
            send(
                &sender,
                Command::InstallMod {
                    instance,
                    project: project.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_submit_create_server(
        move |name, client, minecraft, loader, loader_version, persistent| {
            let client = if client.is_empty() {
                None
            } else {
                backend::parse_instance_id(&client)
            };
            send(
                &sender,
                Command::CreateServer {
                    name: name.into(),
                    client,
                    minecraft: minecraft.into(),
                    loader: loader.into(),
                    loader_version: loader_version.into(),
                    persistent,
                },
            );
        },
    );

    let sender = commands.clone();
    ui.on_select_server(move |_| send(&sender, Command::Refresh));

    register_server_actions(ui, commands);
    register_diagnostics(ui, commands);
    register_settings(ui, commands);

    let weak = ui.as_weak();
    ui.on_copy_text(move |text| {
        let copied = ClipboardContext::new()
            .and_then(|mut clipboard| clipboard.set_contents(text.into()))
            .is_ok();
        if let Some(ui) = weak.upgrade() {
            ui.set_toast_error(!copied);
            ui.set_toast_text(
                if copied {
                    lookup("gui-clipboard-copied")
                } else {
                    lookup("gui-clipboard-unavailable")
                }
                .into(),
            );
        }
    });

    let sender = commands.clone();
    ui.on_cancel_operation(move |id| send(&sender, Command::CancelOperation(id.into())));
}

#[allow(clippy::too_many_lines)]
fn register_server_actions(ui: &AppWindow, commands: &Sender<Command>) {
    let weak = ui.as_weak();
    ui.on_validate_server_properties(move |access, address, port, max_players| {
        if let Some(ui) = weak.upgrade() {
            let port = validate_port(&port);
            let address = validate_ip(&address, access == "Custom");
            let players = validate_player_count(&max_players);
            ui.set_property_port_error(port.message.into());
            ui.set_property_address_error(address.message.into());
            ui.set_property_max_players_error(players.into());
        }
    });

    let sender = commands.clone();
    ui.on_open_server_folder(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::OpenServerFolder(id));
        }
    });

    let sender = commands.clone();
    ui.on_load_server_workspace(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::LoadServerWorkspace(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_start(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::StartServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_stop(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::StopServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_restart(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::RestartServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_kill(move |id| {
        if let Some(id) = backend::parse_server_id(&id) {
            send(&sender, Command::KillServer(id));
        }
    });

    let sender = commands.clone();
    ui.on_server_console(move |id, command| {
        if let Some(server) = backend::parse_server_id(&id)
            && !command.trim().is_empty()
        {
            send(
                &sender,
                Command::SendConsole {
                    server,
                    command: command.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_server_properties(
        move |id,
              motd,
              max_players,
              difficulty,
              gamemode,
              allow_flight,
              pvp,
              allowlist,
              online_mode,
              access,
              address,
              port| {
            let Some(server) = backend::parse_server_id(&id) else {
                return;
            };
            let port_result = validate_port(&port);
            let address_result = validate_ip(&address, access == "Custom");
            let max_players_error = validate_player_count(&max_players);
            if let Some(ui) = weak.upgrade() {
                ui.set_property_port_error(port_result.message.clone().into());
                ui.set_property_address_error(address_result.message.clone().into());
                ui.set_property_max_players_error(max_players_error.clone().into());
            }
            let max_players_valid = max_players_error.is_empty();
            let difficulty_valid =
                ["peaceful", "easy", "normal", "hard"].contains(&difficulty.as_str());
            let gamemode_valid =
                ["survival", "creative", "adventure", "spectator"].contains(&gamemode.as_str());
            let Ok(network) = parse_network(&access, &address, online_mode, &port) else {
                return;
            };
            if !max_players_valid || !difficulty_valid || !gamemode_valid {
                if let Some(ui) = weak.upgrade() {
                    ui.set_toast_error(true);
                    ui.set_toast_text(lookup("gui-server-property-values-invalid").into());
                }
                return;
            }
            send(
                &sender,
                Command::SaveServerProperties {
                    server,
                    network,
                    values: vec![
                        ("motd".to_owned(), motd.into()),
                        ("max-players".to_owned(), max_players.into()),
                        ("difficulty".to_owned(), difficulty.into()),
                        ("gamemode".to_owned(), gamemode.into()),
                        ("allow-flight".to_owned(), allow_flight.to_string()),
                        ("pvp".to_owned(), pvp.to_string()),
                        ("white-list".to_owned(), allowlist.to_string()),
                    ],
                },
            );
        },
    );

    let sender = commands.clone();
    ui.on_accept_eula(move || send(&sender, Command::AcceptEula));
    let sender = commands.clone();
    ui.on_revoke_eula(move || send(&sender, Command::RevokeEula));
    let sender = commands.clone();
    ui.on_open_eula_reference(move || send(&sender, Command::OpenEulaReference));
}

fn validate_player_count(value: &str) -> String {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return lookup("gui-digits-only");
    }
    if value.parse::<u32>().is_ok_and(|players| players > 0) {
        String::new()
    } else {
        lookup("gui-player-count-minimum")
    }
}

fn register_diagnostics(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_load_diagnostics(move |scope, id, kind| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(&sender, Command::LoadDiagnostics { scope, kind });
        }
    });

    let sender = commands.clone();
    ui.on_open_diagnostic(move |scope, id, kind, name| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(
                &sender,
                Command::OpenDiagnostic {
                    scope,
                    kind,
                    name: name.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_refresh_diagnostic(move |scope, id, kind, name| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(
                &sender,
                Command::OpenDiagnostic {
                    scope,
                    kind,
                    name: name.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_search_diagnostic(move |scope, id, kind, name, query| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(
                &sender,
                Command::SearchDiagnostic {
                    scope,
                    kind,
                    name: name.into(),
                    query: query.into(),
                },
            );
        }
    });

    let sender = commands.clone();
    ui.on_open_diagnostic_folder(move |scope, id, kind| {
        if let (Some(scope), Some(kind)) = (
            backend::parse_scope(&scope, &id),
            backend::parse_diagnostic_kind(&kind),
        ) {
            send(&sender, Command::OpenDiagnosticFolder { scope, kind });
        }
    });
}

fn register_settings(ui: &AppWindow, commands: &Sender<Command>) {
    let sender = commands.clone();
    ui.on_add_offline_account(move |name| {
        if !name.trim().is_empty() {
            send(&sender, Command::AddOfflineAccount(name.into()));
        }
    });

    let sender = commands.clone();
    ui.on_set_default_account(move |id| send(&sender, Command::SetDefaultAccount(id.into())));
    let sender = commands.clone();
    ui.on_remove_account(move |id| send(&sender, Command::RemoveAccount(id.into())));
    let sender = commands.clone();
    ui.on_begin_microsoft_login(move || send(&sender, Command::MicrosoftLogin));

    let sender = commands.clone();
    ui.on_save_api_settings(move |client_id, user_agent| {
        send(
            &sender,
            Command::SaveApiSettings {
                client_id: client_id.into(),
                user_agent: user_agent.into(),
            },
        );
    });

    let sender = commands.clone();
    ui.on_save_storage_settings(move |root| {
        send(&sender, Command::SaveStorageRoot(root.into()));
    });

    let sender = commands.clone();
    ui.on_refresh_java(move || send(&sender, Command::RefreshJava));

    let sender = commands.clone();
    let weak = ui.as_weak();
    ui.on_save_java_settings(
        move |auto_detect, auto_download, minimum, maximum, preferred_path| {
            let (minimum_mib, maximum_mib) = match parse_java_memory(&minimum, &maximum) {
                Ok(memory) => memory,
                Err(message) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_toast_error(true);
                        ui.set_toast_text(message.into());
                    }
                    return;
                }
            };
            send(
                &sender,
                Command::SaveJavaSettings {
                    auto_detect,
                    auto_download,
                    minimum_mib,
                    maximum_mib,
                    preferred_path: preferred_path.into(),
                },
            );
        },
    );

    let sender = commands.clone();
    ui.on_save_preference(move |key, value, enabled| {
        if let Some(key) = ephervex_core::PreferenceKey::parse(&key) {
            send(
                &sender,
                Command::SavePreference {
                    key,
                    value: value.into(),
                    enabled,
                },
            );
        }
    });
}

fn parse_java_memory(minimum: &str, maximum: &str) -> Result<(u32, u32), String> {
    let parsed = minimum
        .parse::<u32>()
        .ok()
        .zip(maximum.parse::<u32>().ok())
        .ok_or_else(|| lookup("gui-java-memory-digits-only"))?;
    if parsed.0 == 0 || parsed.1 == 0 || parsed.0 > parsed.1 {
        Err(lookup("gui-java-memory-range-invalid"))
    } else {
        Ok(parsed)
    }
}

fn apply_event(ui: &AppWindow, event: Event, activities: &Rc<RefCell<Vec<ActivityData>>>) {
    match event {
        Event::Snapshot(snapshot) => apply_snapshot(ui, snapshot),
        Event::MinecraftReleases(versions) => {
            if !versions.is_empty() {
                let create_current = ui.get_create_minecraft();
                let server_current = ui.get_create_server_minecraft();
                if !versions
                    .iter()
                    .any(|version| version == create_current.as_str())
                {
                    ui.set_create_minecraft(versions[0].clone().into());
                }
                if !versions
                    .iter()
                    .any(|version| version == server_current.as_str())
                {
                    ui.set_create_server_minecraft(versions[0].clone().into());
                }
            }
            ui.set_minecraft_releases(model(
                versions.into_iter().map(SharedString::from).collect(),
            ));
        }
        Event::LoaderVersions {
            form,
            loader,
            minecraft,
            versions,
        } => {
            let label = loader.to_string();
            let current_matches = if form == "server" {
                ui.get_create_server_loader() == label
                    && ui.get_create_server_minecraft() == minecraft
            } else {
                ui.get_create_loader() == label && ui.get_create_minecraft() == minecraft
            };
            if !current_matches {
                return;
            }
            let rows = model(versions.iter().cloned().map(SharedString::from).collect());
            if form == "server" {
                ui.set_create_server_loader_versions(rows);
                ui.set_create_server_loader_version(
                    versions.first().cloned().unwrap_or_default().into(),
                );
            } else {
                ui.set_create_loader_versions(rows);
                ui.set_create_loader_version(versions.first().cloned().unwrap_or_default().into());
            }
        }
        Event::ManagedMods(rows) => ui.set_managed_mods(model(managed_mod_rows(rows))),
        Event::ModResults(rows) => ui.set_mod_results(model(mod_search_rows(rows))),
        Event::ModPlan(plan) => {
            ui.set_mod_plan_instance(plan.instance.into());
            ui.set_mod_plan_project(plan.project.into());
            ui.set_mod_plan_title(plan.title.into());
            ui.set_mod_plan_version(plan.version.into());
            ui.set_mod_plan_install_count(to_i32(plan.install_count));
            ui.set_mod_plan_dependency_count(to_i32(plan.dependency_count));
            ui.set_mod_plan_optional_count(to_i32(plan.optional_count));
            ui.set_mod_plan_existing_count(to_i32(plan.already_installed_count));
            ui.set_mod_plan_warning_count(to_i32(plan.warning_count));
            ui.set_modal("mod-plan".into());
            ui.set_modal_error(SharedString::default());
        }
        Event::ServerWorkspace(workspace) => apply_server_workspace(ui, workspace),
        Event::Diagnostics {
            rows,
            scope,
            scope_id,
            kind,
        } => {
            ui.set_diagnostics(model(diagnostic_rows(rows)));
            ui.set_selected_diagnostic_scope(scope.into());
            ui.set_selected_diagnostic_id(scope_id.into());
            ui.set_selected_diagnostic_kind(kind.into());
            ui.set_selected_diagnostic_name(SharedString::default());
            ui.set_diagnostic_content(SharedString::default());
            ui.set_diagnostic_path(SharedString::default());
            ui.set_diagnostic_title(SharedString::default());
        }
        Event::DiagnosticContent(content) => apply_diagnostic_content(ui, content),
        Event::DiagnosticUnavailable(message) => {
            ui.set_diagnostic_title(lookup("gui-diagnostic-unavailable").into());
            ui.set_diagnostic_path(SharedString::default());
            ui.set_diagnostic_content(message.into());
            ui.set_status_message(lookup("gui-diagnostic-disappeared").into());
        }
        Event::JavaRuntimes(rows) => ui.set_java_runtimes(model(java_rows(rows))),
        Event::Activity(activity) => {
            upsert_activity(&mut activities.borrow_mut(), activity);
            apply_activities(ui, &activities.borrow());
        }
        Event::Toast { message, error } => {
            ui.set_toast_text(message.into());
            ui.set_toast_error(error);
        }
        Event::ModalError(message) => ui.set_modal_error(message.into()),
        Event::CloseModal => {
            ui.set_modal(SharedString::default());
            ui.set_modal_error(SharedString::default());
        }
        Event::ImportAnalysis {
            path,
            suggested_name,
            summary,
        } => {
            ui.set_import_path(path.display().to_string().into());
            if ui.get_import_name().is_empty() {
                ui.set_import_name(suggested_name.into());
            }
            ui.set_import_analysis(summary.into());
            ui.set_modal_error(SharedString::default());
        }
    }
}

#[allow(clippy::too_many_lines)]
fn apply_snapshot(ui: &AppWindow, snapshot: Snapshot) {
    let previous_instance = ui.get_selected_instance_id();
    let previous_server = ui.get_selected_server_id();
    let mut instance_rows = snapshot
        .instances
        .into_iter()
        .map(|item| InstanceRow {
            id: item.id.into(),
            name: item.name.into(),
            minecraft: item.minecraft.into(),
            loader: item.loader.into(),
            loader_kind: item.loader_kind.into(),
            storage: item.storage.into(),
            storage_kind: item.storage_kind.into(),
            source: item.source.into(),
            path: item.path.into(),
            updated: lookup("gui-available").into(),
            linked_servers: to_i32(item.linked_servers),
            mods: to_i32(item.managed_mods),
            needs_attention: item.needs_attention,
            can_launch: item.can_launch,
            connect_server_id: item
                .launch_and_connect
                .first()
                .map_or_else(SharedString::default, |connect| {
                    connect.server_id.clone().into()
                }),
            connect_server_name: item
                .launch_and_connect
                .first()
                .map_or_else(SharedString::default, |connect| {
                    connect.server_name.clone().into()
                }),
            can_connect: item
                .launch_and_connect
                .first()
                .is_some_and(|connect| connect.enabled),
        })
        .collect::<Vec<_>>();
    let storage_filter = ui.get_instance_filter();
    let query = ui.get_instance_query().to_ascii_lowercase();
    instance_rows.retain(|row| {
        (storage_filter == "all" || row.storage_kind == storage_filter)
            && row_matches(
                &query,
                [&row.name, &row.minecraft, &row.loader, &row.source],
            )
    });
    let instance_index = selected_index(&instance_rows, &previous_instance, |row| &row.id);
    if let Some(index) = instance_index {
        ui.set_selected_instance_id(instance_rows[index].id.clone());
        ui.set_selected_instance_index(to_i32(index));
    } else {
        ui.set_selected_instance_id(SharedString::default());
        ui.set_selected_instance_index(-1);
    }
    ui.set_instances(model(instance_rows));

    let mut server_rows = snapshot
        .servers
        .into_iter()
        .map(|item| ServerRow {
            id: item.id.into(),
            name: item.name.into(),
            minecraft: item.minecraft.into(),
            loader: item.loader.into(),
            loader_kind: item.loader_kind.into(),
            storage: item.storage.into(),
            storage_kind: item.storage_kind.into(),
            relationship: item.relationship.into(),
            linked_client_id: item.linked_client_id.into(),
            state: item.state.into(),
            state_kind: item.state_kind.into(),
            state_detail: item.state_detail.into(),
            address: item.address.into(),
            path: item.path.into(),
            installed: item.installed,
            needs_attention: item.needs_attention,
            can_start: item.can_start,
            can_stop: item.can_stop,
            can_restart: item.can_restart,
            can_kill: item.can_kill,
            can_send_command: item.can_send_command,
        })
        .collect::<Vec<_>>();
    let query = ui.get_server_query().to_ascii_lowercase();
    server_rows.retain(|row| {
        row_matches(
            &query,
            [
                &row.name,
                &row.minecraft,
                &row.loader,
                &row.relationship,
                &row.address,
            ],
        )
    });
    let server_index = selected_index(&server_rows, &previous_server, |row| &row.id);
    if let Some(index) = server_index {
        ui.set_selected_server_id(server_rows[index].id.clone());
        ui.set_selected_server_index(to_i32(index));
    } else {
        ui.set_selected_server_id(SharedString::default());
        ui.set_selected_server_index(-1);
    }
    ui.set_servers(model(server_rows));
    ui.set_accounts(model(account_rows(snapshot.accounts)));
    ui.set_preferences(model(preference_rows(snapshot.preferences)));
    ui.set_storage_root(snapshot.settings.storage_root.into());
    ui.set_persistent_root(snapshot.settings.persistent_root.into());
    ui.set_microsoft_client_id(snapshot.settings.microsoft_client_id.into());
    ui.set_user_agent(snapshot.settings.user_agent.into());
    ui.set_java_auto_detect(snapshot.settings.java_auto_detect);
    ui.set_java_auto_download(snapshot.settings.java_auto_download);
    ui.set_java_memory_min(snapshot.settings.java_memory_min.to_string().into());
    ui.set_java_memory_max(snapshot.settings.java_memory_max.to_string().into());
    ui.set_java_preferred_path(snapshot.settings.java_preferred_path.into());
    ui.set_eula_accepted(snapshot.eula.accepted);
    ui.set_eula_status(snapshot.eula.status.into());
    ui.set_loading(false);
    ui.set_status_message(lookup("gui-ready").into());
}

fn apply_server_workspace(ui: &AppWindow, workspace: ServerWorkspaceData) {
    ui.set_console_text(workspace.console_lines.join("\n").into());
    ui.set_console_lines(model(
        workspace
            .console_lines
            .into_iter()
            .map(SharedString::from)
            .collect(),
    ));
    ui.set_property_motd(workspace.motd.into());
    ui.set_property_max_players(workspace.max_players.into());
    ui.set_property_difficulty(workspace.difficulty.into());
    ui.set_property_gamemode(workspace.gamemode.into());
    ui.set_property_allow_flight(workspace.allow_flight);
    ui.set_property_pvp(workspace.pvp);
    ui.set_property_allowlist(workspace.allowlist);
    ui.set_property_online_mode(workspace.network.online_mode);
    ui.set_property_access(
        match workspace.network.access {
            ephervex_core::NetworkAccess::Localhost => "Localhost",
            ephervex_core::NetworkAccess::Lan => "LAN",
            ephervex_core::NetworkAccess::Custom => "Custom",
        }
        .into(),
    );
    ui.set_property_address(workspace.network.bind_address.unwrap_or_default().into());
    ui.set_property_port(workspace.network.port.to_string().into());
    ui.set_property_port_error(SharedString::default());
    ui.set_property_address_error(SharedString::default());
}

fn apply_diagnostic_content(ui: &AppWindow, content: DiagnosticContent) {
    ui.set_diagnostic_title(content.title.into());
    ui.set_diagnostic_path(content.path.into());
    ui.set_diagnostic_content(content.text.into());
    let status = match (content.truncated, content.lossy, content.matches) {
        (_, _, Some(matches)) => lookup_args(
            "gui-diagnostic-search-matches",
            [("count", Arg::from(matches))],
        ),
        (true, true, None) => lookup("gui-diagnostic-window-lossy"),
        (true, false, None) => lookup("gui-diagnostic-window-bounded"),
        (false, true, None) => lookup("gui-diagnostic-file-lossy"),
        (false, false, None) => lookup("gui-diagnostic-loaded"),
    };
    ui.set_status_message(status.into());
}

fn upsert_activity(activities: &mut Vec<ActivityData>, activity: ActivityData) {
    if let Some(existing) = activities
        .iter_mut()
        .find(|existing| existing.item.id == activity.item.id)
    {
        if !activity.entity.is_empty() {
            existing.entity = activity.entity;
        }
        existing.item = activity.item;
        existing.time = activity.time;
    } else {
        activities.insert(0, activity);
        activities.truncate(100);
    }
}

fn apply_activities(ui: &AppWindow, activities: &[ActivityData]) {
    let rows = activities
        .iter()
        .map(|activity| ActivityRow {
            id: activity.item.id.clone().into(),
            title: activity.item.title.clone().into(),
            entity: activity.entity.clone().into(),
            status: activity.item.status.clone().into(),
            detail: activity.item.detail.clone().into(),
            time: activity.time.clone().into(),
            progress: activity.item.progress,
            indeterminate: activity.item.indeterminate,
            active: matches!(activity.item.status_kind, "queued" | "running"),
            failed: activity.item.failed,
            cancellable: activity.item.cancellable,
        })
        .collect::<Vec<_>>();
    ui.set_active_operations(to_i32(rows.iter().filter(|row| row.active).count()));
    ui.set_activities(model(rows));
}

fn account_rows(rows: Vec<AccountData>) -> Vec<AccountRow> {
    rows.into_iter()
        .map(|row| AccountRow {
            id: row.id.into(),
            name: row.name.into(),
            kind: row.kind.into(),
            is_default: row.is_default,
            credential_state: row.credential_state.into(),
        })
        .collect()
}

fn preference_rows(rows: Vec<PreferenceData>) -> Vec<PreferenceRow> {
    rows.into_iter()
        .map(|row| {
            let numeric_value = row.value.parse::<f32>().unwrap_or_default();
            PreferenceRow {
                key: row.key.into(),
                label: row.label.into(),
                value: row.value.into(),
                enabled: row.enabled,
                kind: row.kind.into(),
                syntax: row.syntax.into(),
                numeric_value,
            }
        })
        .collect()
}

fn managed_mod_rows(rows: Vec<ManagedModData>) -> Vec<ModRow> {
    rows.into_iter()
        .map(|row| ModRow {
            project_id: row.project_id.into(),
            title: row.title.into(),
            version: row.version.into(),
            provider: row.provider.into(),
            origin: row.origin.into(),
        })
        .collect()
}

fn mod_search_rows(rows: Vec<ModSearchData>) -> Vec<ModSearchRow> {
    rows.into_iter()
        .map(|row| ModSearchRow {
            project_id: row.project_id.into(),
            title: row.title.into(),
            description: row.description.into(),
            author: row.author.into(),
            downloads: row.downloads.into(),
        })
        .collect()
}

fn diagnostic_rows(rows: Vec<DiagnosticData>) -> Vec<DiagnosticRow> {
    rows.into_iter()
        .map(|row| DiagnosticRow {
            scope_kind: row.scope_kind.into(),
            scope_id: row.scope_id.into(),
            entity: row.entity.into(),
            kind: row.kind.into(),
            name: row.name.into(),
            path: row.path.into(),
            modified: row.modified.into(),
            size: row.size.into(),
            summary: row.summary.into(),
        })
        .collect()
}

fn java_rows(rows: Vec<JavaData>) -> Vec<JavaRow> {
    rows.into_iter()
        .map(|row| JavaRow {
            path: row.path.into(),
            version: row.version.into(),
            vendor: row.vendor.into(),
            origin: row.origin.into(),
            compatible: row.compatible.into(),
        })
        .collect()
}

fn selected_index<T>(
    rows: &[T],
    previous: &SharedString,
    id: impl Fn(&T) -> &SharedString,
) -> Option<usize> {
    if previous.is_empty() {
        (!rows.is_empty()).then_some(0)
    } else {
        rows.iter()
            .position(|row| id(row) == previous)
            .or_else(|| (!rows.is_empty()).then_some(0))
    }
}

fn row_matches<'a>(query: &str, values: impl IntoIterator<Item = &'a SharedString>) -> bool {
    query.is_empty()
        || values
            .into_iter()
            .any(|value| value.to_ascii_lowercase().contains(query))
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn send(sender: &Sender<Command>, command: Command) {
    if sender.send(command).is_err() {
        tracing::error!("the GUI core worker stopped unexpectedly");
    }
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use crate::presentation::ActivityPresentation;
    use ephervex_i18n::has;

    use super::*;

    fn activity(id: &str, status_kind: &'static str) -> ActivityData {
        ActivityData {
            item: ActivityPresentation {
                id: id.to_owned(),
                title: "Import pack".to_owned(),
                status: status_kind.to_owned(),
                status_kind,
                detail: String::new(),
                progress: 0.0,
                indeterminate: true,
                cancellable: false,
                failed: false,
            },
            entity: "Pack".to_owned(),
            time: "12:00:00".to_owned(),
        }
    }

    #[test]
    fn operation_updates_replace_the_same_activity_instead_of_fabricating_history() {
        let mut rows = vec![activity("operation-id", "running")];
        upsert_activity(&mut rows, activity("operation-id", "completed"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].item.status_kind, "completed");
    }

    #[test]
    fn in_memory_activity_is_bounded() {
        let mut rows = Vec::new();
        for index in 0..120 {
            upsert_activity(&mut rows, activity(&index.to_string(), "completed"));
        }
        assert_eq!(rows.len(), 100);
    }

    #[test]
    fn row_filtering_is_case_insensitive_and_searches_exact_metadata() {
        let name = SharedString::from("Vanilla Repro");
        let loader = SharedString::from("Minecraft 1.21.1 | Vanilla");
        assert!(row_matches("repro", [&name, &loader]));
        assert!(row_matches("1.21.1", [&name, &loader]));
        assert!(!row_matches("forge", [&name, &loader]));
    }

    #[test]
    fn java_memory_input_requires_digits_and_an_ordered_nonzero_range() {
        assert_eq!(parse_java_memory("512", "4096"), Ok((512, 4096)));
        assert!(parse_java_memory("512 MiB", "4096").is_err());
        assert!(parse_java_memory("0", "4096").is_err());
        assert!(parse_java_memory("4096", "512").is_err());
    }

    #[test]
    fn every_gui_message_id_exists_in_the_fluent_catalog() {
        for source in [
            include_str!("ui.rs"),
            include_str!("controller.rs"),
            include_str!("backend.rs"),
            include_str!("presentation.rs"),
        ] {
            let mut remaining = source;
            while let Some(start) = remaining.find("\"gui-") {
                let candidate = &remaining[start + 1..];
                let Some(end) = candidate.find('"') else {
                    assert_eq!(candidate, "", "unterminated GUI localization id");
                    break;
                };
                let message_id = &candidate[..end];
                if message_id != "gui-" {
                    assert!(has(message_id), "missing GUI localization: {message_id}");
                }
                remaining = &candidate[end + 1..];
            }
        }
    }

    #[test]
    fn modrinth_search_does_not_reuse_the_server_filter() {
        let source = include_str!("ui.rs");
        assert!(source.contains("text <=> root.mod-query"));
        assert!(source.contains("text <=> root.server-query"));
        assert!(!source.contains("search-mods(root.selected-instance-id, root.server-query)"));
    }

    #[test]
    fn player_count_accepts_positive_digits_only() {
        assert!(validate_player_count("20").is_empty());
        assert!(!validate_player_count("0").is_empty());
        assert!(!validate_player_count("2.5").is_empty());
        assert!(!validate_player_count("many").is_empty());
    }

    #[test]
    fn creation_forms_use_catalog_models_for_exact_versions() {
        let source = include_str!("ui.rs");
        assert!(source.contains("model: root.minecraft-releases"));
        assert!(source.contains("model: root.create-loader-versions"));
        assert!(source.contains("model: root.create-server-loader-versions"));
        assert!(!source.contains("text <=> root.create-minecraft"));
        assert!(!source.contains("text <=> root.create-loader-version"));
    }

    #[test]
    fn fov_presentation_keeps_core_degree_semantics() {
        let rows = preference_rows(vec![PreferenceData {
            key: "fov".to_owned(),
            label: "Field of View".to_owned(),
            value: "70".to_owned(),
            enabled: true,
            kind: "slider".to_owned(),
            syntax: "field of view in degrees, 30 to 110".to_owned(),
        }]);

        assert_eq!(rows[0].numeric_value, 70.0);
        assert_eq!(rows[0].value, "70");
    }
}
