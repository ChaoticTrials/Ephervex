//! Synthetic presentation rows. No launcher storage or service is opened.

use ephervex_gui::ui::{
    AccountRow, AppWindow, HostView, InstanceRow, ModRow, ModSearchRow, ServerRow,
    WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::lookup;
use slint::{Model, ModelRc, VecModel};

pub(crate) mod client_hosts;
pub(crate) mod diagnostics_servers;
pub(crate) mod getting_started;
pub(crate) mod in_progress;
pub(crate) mod instance_dialogs;
pub(crate) mod mods;
pub(crate) mod notices;
pub(crate) mod settings;

pub(crate) fn rows<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

pub(crate) struct Build {
    kind: &'static str,
    display: &'static str,
}

pub(crate) const FORGE_1_20: Build = Build {
    kind: "forge",
    display: "Forge 47.4.10",
};
pub(crate) const FORGE_1_16: Build = Build {
    kind: "forge",
    display: "Forge 36.2.39",
};
pub(crate) const FORGE_1_12: Build = Build {
    kind: "forge",
    display: "Forge 14.23.5.2860",
};
pub(crate) const NEOFORGE_1_21: Build = Build {
    kind: "neoforge",
    display: "NeoForge 21.1.249",
};
pub(crate) const NEOFORGE_26: Build = Build {
    kind: "neoforge",
    display: "NeoForge 26.2.0.59",
};
pub(crate) const VANILLA: Build = Build {
    kind: "vanilla",
    display: "Vanilla",
};

pub(crate) fn instance(
    id: &str,
    name: &str,
    minecraft: &str,
    build: &Build,
    storage: &str,
    source: &str,
) -> InstanceRow {
    InstanceRow {
        id: id.into(),
        name: name.into(),
        minecraft: minecraft.into(),
        loader: build.display.into(),
        loader_kind: build.kind.into(),
        storage: storage.into(),
        storage_kind: if storage == "Temporary" {
            "temporary"
        } else {
            "persistent"
        }
        .into(),
        source: source.into(),
        path: format!("/demo/instances/{id}").into(),
        updated: "Today".into(),
        state: lookup("gui-state-ready").into(),
        state_kind: "ready".into(),
        can_launch: true,
        ..InstanceRow::default()
    }
}

/// An instance whose game is running under this launcher.
pub(crate) fn running_instance(row: InstanceRow) -> InstanceRow {
    InstanceRow {
        state: lookup("gui-state-running").into(),
        state_kind: "running".into(),
        can_launch: false,
        can_kill: true,
        can_restart: true,
        ..row
    }
}

/// An instance whose game is still starting.
pub(crate) fn starting_instance(row: InstanceRow) -> InstanceRow {
    InstanceRow {
        state: lookup("gui-state-starting").into(),
        state_kind: "starting".into(),
        can_launch: false,
        can_kill: true,
        ..row
    }
}

pub(crate) fn server(id: &str, name: &str, minecraft: &str, build: &Build, port: u16) -> ServerRow {
    ServerRow {
        id: id.into(),
        name: name.into(),
        minecraft: minecraft.into(),
        loader: build.display.into(),
        loader_kind: build.kind.into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        state: lookup("gui-state-stopped").into(),
        state_kind: "stopped".into(),
        state_detail: lookup("gui-state-ready-to-start").into(),
        address: format!("127.0.0.1:{port}").into(),
        path: format!("/demo/servers/{id}").into(),
        installed: true,
        can_start: true,
        ..ServerRow::default()
    }
}

/// A server that a supervisor is running and can control.
pub(crate) fn running_server(row: ServerRow) -> ServerRow {
    ServerRow {
        state: lookup("gui-state-running").into(),
        state_kind: "running".into(),
        state_detail: lookup("gui-state-controlled").into(),
        can_start: false,
        can_stop: true,
        can_restart: true,
        can_kill: true,
        can_send_command: true,
        ..row
    }
}

/// A server whose process is still starting.
pub(crate) fn starting_server(row: ServerRow) -> ServerRow {
    ServerRow {
        state: lookup("gui-state-starting").into(),
        state_kind: "starting".into(),
        state_detail: lookup("gui-state-starting-detail").into(),
        can_start: false,
        ..row
    }
}

fn cavestone_server() -> ServerRow {
    server(
        "cavestone-server",
        "CaveStone Server",
        "1.20.1",
        &FORGE_1_20,
        25565,
    )
}

pub(crate) fn account(name: &str, is_default: bool) -> AccountRow {
    AccountRow {
        id: name.to_ascii_lowercase().into(),
        name: name.into(),
        kind: "Offline".into(),
        is_default,
        credential_state: lookup("gui-no-credential").into(),
    }
}

/// A Microsoft account row. `stored` is whether a refresh token is kept.
pub(crate) fn microsoft_account(name: &str, stored: bool) -> AccountRow {
    AccountRow {
        kind: "Microsoft".into(),
        credential_state: lookup(if stored {
            "gui-credential-stored"
        } else {
            "gui-signin-required"
        })
        .into(),
        ..account(name, false)
    }
}

/// A mod installed from a catalog as part of a pack.
pub(crate) fn mod_row(title: &str, version: &str, file_name: &str) -> ModRow {
    ModRow {
        project_id: title.to_ascii_lowercase().replace(' ', "-").into(),
        logical_path: format!("mods/{file_name}").into(),
        title: title.into(),
        file_name: file_name.into(),
        version: version.into(),
        version_known: true,
        provider: "Modrinth".into(),
        provider_kind: "modrinth".into(),
        origin: "From pack".into(),
        origin_kind: "pack".into(),
        compatibility: String::new().into(),
        enabled: true,
        pack_owned: true,
        can_manage: true,
        can_change_version: true,
        can_update: true,
        can_remove: true,
        ..ModRow::default()
    }
}

/// A mod the user added from a catalog.
pub(crate) fn added_mod(row: ModRow) -> ModRow {
    ModRow {
        origin: "Added".into(),
        origin_kind: "user".into(),
        pack_owned: false,
        ..row
    }
}

/// A mod installed only because another mod requires it.
pub(crate) fn dependency_mod(row: ModRow) -> ModRow {
    ModRow {
        origin: "Dependency".into(),
        origin_kind: "dependency".into(),
        pack_owned: false,
        ..row
    }
}

/// A jar the user placed in the mods folder, with no catalog identity.
pub(crate) fn hand_placed_mod(title: &str, version: &str, file_name: &str) -> ModRow {
    ModRow {
        provider: "No provider".into(),
        provider_kind: String::new().into(),
        origin: "Added by hand".into(),
        origin_kind: "hand-placed".into(),
        pack_owned: false,
        can_change_version: false,
        can_update: false,
        can_remove: false,
        can_delete_file: true,
        ..mod_row(title, version, file_name)
    }
}

pub(crate) fn instance_rows() -> Vec<InstanceRow> {
    vec![
        InstanceRow {
            mods: 54,
            ..instance(
                "cavestone",
                "CaveStone",
                "1.20.1",
                &FORGE_1_20,
                "Temporary",
                "Modrinth",
            )
        },
        InstanceRow {
            mods: 0,
            ..instance(
                "vanilla-test",
                "Vanilla Test",
                "1.21.1",
                &VANILLA,
                "Persistent",
                "Manual",
            )
        },
        InstanceRow {
            mods: 31,
            ..running_instance(instance(
                "nether-roof-highway",
                "Nether Roof Highway",
                "1.21.1",
                &NEOFORGE_1_21,
                "Persistent",
                "Modrinth",
            ))
        },
        InstanceRow {
            mods: 38,
            ..instance(
                "creeper-containment-lab",
                "Creeper Containment Lab",
                "1.12.2",
                &FORGE_1_12,
                "Temporary",
                "CurseForge",
            )
        },
        InstanceRow {
            mods: 12,
            ..starting_instance(instance(
                "redstone-clock-debugging",
                "Redstone Clock Debugging",
                "26.2",
                &NEOFORGE_26,
                "Temporary",
                "Manual",
            ))
        },
        InstanceRow {
            mods: 9,
            needs_attention: true,
            can_launch: false,
            ..instance(
                "herobrine-sighting",
                "Herobrine Sighting #1337",
                "1.16.5",
                &FORGE_1_16,
                "Temporary",
                "CurseForge",
            )
        },
        InstanceRow {
            mods: 24,
            creating_server_detail: lookup("operation-stage-downloading-files").into(),
            creating_server_progress: 0.4,
            ..instance(
                "silverfish-nest",
                "Silverfish Nest Reproduction",
                "1.20.1",
                &FORGE_1_20,
                "Persistent",
                "Modrinth",
            )
        },
        InstanceRow {
            mods: 18,
            ..instance(
                "spawn-chunks-workshop",
                "Spawn Chunks Workshop",
                "1.21.1",
                &NEOFORGE_1_21,
                "Persistent",
                "Server pack",
            )
        },
    ]
}

pub(crate) fn server_rows() -> Vec<ServerRow> {
    vec![
        cavestone_server(),
        running_server(server(
            "totem-of-undying-qa",
            "Totem of Undying QA",
            "1.21.1",
            &NEOFORGE_1_21,
            25566,
        )),
        starting_server(server(
            "spawn-chunks-server",
            "Spawn Chunks Server",
            "1.21.1",
            &VANILLA,
            25567,
        )),
        ServerRow {
            state: lookup("gui-state-failed").into(),
            state_kind: "failed".into(),
            state_detail: lookup("gui-server-failed-detail").into(),
            ..server(
                "cow-clicker-test",
                "Cow Clicker Test Server",
                "1.20.1",
                &FORGE_1_20,
                25568,
            )
        },
    ]
}

pub(crate) fn account_rows() -> Vec<AccountRow> {
    vec![
        account("MelanX", true),
        account("Steve", false),
        account("Herobrine", false),
        microsoft_account("Alex_Builds", true),
        microsoft_account("Enderman_QA", false),
    ]
}

pub(crate) fn install_base(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_app_version("0.10.0".into());
    ui.set_status_message(lookup("gui-ready").into());
    ui.set_storage_root("/demo/ephervex".into());
    ui.set_application_text_scale(100);
    ui.set_accounts(rows(account_rows()));
    ui.set_instances(rows(instance_rows()));
    ui.set_global_servers(rows(server_rows()));
    ui.set_selected_instance_id("cavestone".into());
    ui.set_selected_instance_index(0);
    ui.set_selected_global_server_id("cavestone-server".into());
    ui.set_selected_global_server_index(0);
}

pub(crate) fn instances_overview(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
}

pub(crate) fn servers_overview(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(1);
}

pub(crate) fn accounts_settings(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(4);
    ui.set_settings_tab("Accounts".into());
}

/// The mods of CaveStone. The first three come from the documented pack; the
/// rest are synthetic and exist to show every kind of row.
pub(crate) fn client_mod_rows() -> Vec<ModRow> {
    vec![
        mod_row("Create", "0.5.1.j", "create-1.20.1-0.5.1.j.jar"),
        mod_row("Jade", "11.13.2", "Jade-1.20.1-Forge-11.13.2.jar"),
        mod_row("JEI", "15.20.0.130", "jei-1.20.1-forge-15.20.0.130.jar"),
        ModRow {
            suspect_reason: lookup("crash-reason-named").into(),
            suspect_confidence: "high".into(),
            ..mod_row(
                "Creeper Confetti",
                "0.3.2",
                "creeper-confetti-1.20.1-forge-0.3.2.jar",
            )
        },
        added_mod(mod_row(
            "Redstone Rabbit Holes",
            "1.4.0",
            "redstone-rabbit-holes-1.20.1-1.4.0.jar",
        )),
        dependency_mod(mod_row(
            "Ender Pearl Pager",
            "2.1.7",
            "ender-pearl-pager-forge-2.1.7.jar",
        )),
        ModRow {
            enabled: false,
            ..mod_row(
                "Obsidian Mirror",
                "0.9.1",
                "obsidian-mirror-1.20.1-0.9.1.jar.disabled",
            )
        },
        ModRow {
            deleted: true,
            ..mod_row(
                "Pocket Piglin",
                "1.2.0",
                "pocket-piglin-1.20.1-forge-1.2.0.jar",
            )
        },
        ModRow {
            version_known: false,
            version: String::new().into(),
            ..hand_placed_mod("Nether Fridge", "", "nether-fridge-custom.jar")
        },
    ]
}

pub(crate) fn client_mod_results() -> Vec<ModSearchRow> {
    let note = lookup("gui-compatible");
    let result = |id: &str, title: &str, description: &str, author: &str| ModSearchRow {
        project_id: id.into(),
        title: title.into(),
        description: description.into(),
        author: author.into(),
        note: note.clone().into(),
        ..ModSearchRow::default()
    };
    vec![
        ModSearchRow {
            installed: true,
            ..result(
                "redstone-rabbit-holes",
                "Redstone Rabbit Holes",
                "Burrows that carry redstone signals under the grass.",
                "demo_author",
            )
        },
        result(
            "villager-notice-board",
            "Villager Notice Board",
            "A bulletin board villagers use to trade rumors.",
            "demo_author",
        ),
        ModSearchRow {
            queued: true,
            ..result(
                "silent-minecart-rails",
                "Silent Minecart Rails",
                "Quiet rails for late-night redstone work.",
                "demo_studio",
            )
        },
        result(
            "cow-clicker",
            "Cow Clicker",
            "Cows that keep count of how often they were visited.",
            "demo_studio",
        ),
    ]
}

pub(crate) fn client_mods(ui: &AppWindow) {
    let client = ui.get_instances().row_data(0).unwrap_or_default();
    let surfaces = Surface::client()
        .iter()
        .map(|surface| WorkspaceSurfaceRow {
            id: surface.id().into(),
            label: lookup(surface.label()).into(),
        })
        .collect::<Vec<_>>();
    let view = HostView {
        kind: "client".into(),
        id: client.id.clone(),
        available: true,
        client,
        surfaces: rows(surfaces),
        mods: rows(client_mod_rows()),
        allow_pack_mod_changes: true,
        browser_provider: "modrinth".into(),
        browser_submitted: "redstone".into(),
        browser_ran: true,
        browser_results: rows(client_mod_results()),
        browser_next_offset: -1,
        ..HostView::default()
    };
    ui.set_host_views(rows(vec![view]));
    ui.set_workspace_panes(rows(vec![WorkspacePaneRow {
        id: 1,
        host: 0,
        log: -1,
        surface: "Mods".into(),
        label: "CaveStone, Mods".into(),
        surface_choice: 1,
        can_split_beside: true,
        ..WorkspacePaneRow::default()
    }]));
    ui.set_workspace_back_label(lookup("gui-instances").into());
    ui.set_page(0);
    ui.set_workspace_open(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_accounts_are_synthetic_and_valid_offline_names() {
        let accounts = account_rows();
        assert_eq!(
            accounts.iter().filter(|account| account.is_default).count(),
            1
        );
        assert!(accounts.iter().all(|account| {
            let name = account.name.as_str();
            (3..=16).contains(&name.len())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }));
        assert!(
            accounts
                .iter()
                .filter(|account| account.kind == "Offline")
                .all(|account| account.credential_state == lookup("gui-no-credential").as_str())
        );
    }

    #[test]
    fn sample_instances_use_the_source_names_the_app_displays() {
        let allowed = ["Manual", "Modrinth", "CurseForge", "Prism", "Server pack"];
        assert!(
            instance_rows()
                .iter()
                .all(|row| allowed.contains(&row.source.as_str()))
        );
    }

    #[test]
    fn sample_hosts_stay_within_supported_loaders_and_synthetic_paths() {
        assert!(instance_rows().iter().all(|row| {
            ["vanilla", "forge", "neoforge"].contains(&row.loader_kind.as_str())
                && row.path.starts_with("/demo/")
        }));
        assert!(
            server_rows()
                .iter()
                .all(|row| row.path.starts_with("/demo/")
                    && !row.state_detail.is_empty()
                    && ["vanilla", "forge", "neoforge"].contains(&row.loader_kind.as_str()))
        );
    }

    #[test]
    fn running_hosts_offer_kill_and_not_launch() {
        assert!(
            instance_rows()
                .iter()
                .any(|row| row.state_kind == "running" && row.can_kill && !row.can_launch)
        );
    }

    #[test]
    fn the_mod_list_shows_each_kind_of_row() {
        let mods = client_mod_rows();
        let kinds = ["pack", "user", "dependency", "hand-placed"];
        assert!(
            kinds
                .iter()
                .all(|kind| mods.iter().any(|row| row.origin_kind == *kind))
        );
        assert!(mods.iter().any(|row| !row.enabled));
        assert!(mods.iter().any(|row| row.deleted));
        assert!(mods.iter().any(|row| !row.suspect_reason.is_empty()));
        assert!(mods.iter().any(|row| !row.version_known));
    }
}
