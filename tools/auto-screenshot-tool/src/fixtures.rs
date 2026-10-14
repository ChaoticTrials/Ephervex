//! Synthetic presentation rows. No launcher storage or service is opened.

use ephervex_gui::ui::{
    AccountRow, AppWindow, HostView, InstanceRow, ModRow, ModSearchRow, ServerRow,
    WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::lookup;
use slint::{Model, ModelRc, SharedString, VecModel};

use crate::artwork;

pub(crate) mod client_hosts;
pub(crate) mod diagnostics_servers;
pub(crate) mod getting_started;
pub(crate) mod in_progress;
pub(crate) mod instance_dialogs;
pub(crate) mod mods;
pub(crate) mod notices;
pub(crate) mod recorded;
pub(crate) mod settings;

/// A byte count the way the launcher prints sizes: KiB below a MiB, then MiB or GiB
/// with one decimal.
pub(crate) fn size_text(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    let tenths = |unit: u64| {
        let value = (bytes.saturating_mul(10) + unit / 2) / unit;
        format!("{}.{}", value / 10, value % 10)
    };
    if bytes >= GIB {
        format!("{} GiB", tenths(GIB))
    } else if bytes >= MIB {
        format!("{} MiB", tenths(MIB))
    } else {
        format!("{} KiB", bytes.div_ceil(KIB))
    }
}

/// The level of one recorded log line, from its `[thread/LEVEL]` group, or
/// `other` for a line that is not a record.
pub(crate) fn log_level(line: &str) -> String {
    line.split("] [")
        .nth(1)
        .and_then(|group| group.rsplit('/').next())
        .map(|level| level.trim_end_matches(']').to_ascii_lowercase())
        .filter(|level| {
            ["trace", "debug", "info", "warn", "error", "fatal"].contains(&level.as_str())
        })
        .unwrap_or_else(|| "other".to_owned())
}

/// Tabs become spaces to the next multiple of four columns, as the app shows reports.
pub(crate) fn expand_tabs(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut column = 0;
    for character in text.chars() {
        match character {
            '\t' => {
                let width = 4 - column % 4;
                expanded.extend(std::iter::repeat_n(' ', width));
                column += width;
            }
            '\n' => {
                expanded.push('\n');
                column = 0;
            }
            other => {
                expanded.push(other);
                column += 1;
            }
        }
    }
    expanded
}

pub(crate) fn rows<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

pub(crate) struct Build {
    pub(crate) kind: &'static str,
    pub(crate) display: &'static str,
}

/// The Forge build of CaveStone and the other 1.20.1 packs.
pub(crate) const FORGE_1_20: Build = Build {
    kind: "forge",
    display: "Forge 47.4.10",
};
/// The Forge build of the 1.12.2 pack.
pub(crate) const FORGE_1_12: Build = Build {
    kind: "forge",
    display: "Forge 14.23.5.2860",
};

pub(crate) fn instance(
    id: &str,
    name: &str,
    minecraft: &str,
    loader: (&str, &str),
    storage: &str,
    source: &str,
) -> InstanceRow {
    InstanceRow {
        id: id.into(),
        name: name.into(),
        minecraft: minecraft.into(),
        loader: loader.1.into(),
        loader_kind: loader.0.into(),
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

pub(crate) fn server(
    id: &str,
    name: &str,
    minecraft: &str,
    loader: (&str, &str),
    port: u16,
) -> ServerRow {
    ServerRow {
        id: id.into(),
        name: name.into(),
        minecraft: minecraft.into(),
        loader: loader.1.into(),
        loader_kind: loader.0.into(),
        storage: "Persistent".into(),
        storage_kind: "persistent".into(),
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

/// One mod of the recorded CaveStone list, shown the way the launcher shows it.
///
/// Titles, versions, and compatibility are what the catalog reports for the
/// installed release. The author column stays empty, as it does for a mod
/// known only by its project. A jar the launcher cannot identify is listed by
/// its file name, with no provider and no version, and offers only deleting
/// the file.
fn recorded_mod_row(record: &recorded::Mod) -> ModRow {
    let identified = !record.project_id.is_empty();
    let file_name = record.file_name.as_str();
    let (origin_label, origin_kind) = match record.origin.as_str() {
        "pack" => (lookup("mod-origin-pack-declared"), "pack"),
        "override" => (lookup("mod-origin-pack-override"), "pack"),
        "user" => (lookup("mod-origin-user-selected"), "user"),
        "dependency" => (lookup("mod-origin-required-dependency"), "dependency"),
        _ => (lookup("mod-origin-hand-placed"), "hand-placed"),
    };
    let restriction = match record.origin.as_str() {
        "dependency" => lookup("mod-restriction-dependency"),
        "hand-placed" => lookup("mod-restriction-hand-placed"),
        "override" => lookup("mod-restriction-pack-owned"),
        _ => String::new(),
    };
    // Pack mods are shown with independent mod changes enabled, so a
    // pack-declared mod with a known release can be moved and removed.
    let movable = identified && matches!(record.origin.as_str(), "pack" | "user");
    ModRow {
        project_id: record.project_id.clone().into(),
        logical_path: format!("mods/{file_name}").into(),
        title: record.title.clone().into(),
        file_name: if record.title == file_name {
            SharedString::default()
        } else {
            file_name.into()
        },
        version: if record.version.is_empty() {
            lookup("gui-mod-version-unknown").into()
        } else {
            record.version.clone().into()
        },
        version_known: !record.version.is_empty(),
        provider: if identified {
            "Modrinth".into()
        } else {
            lookup("gui-mod-provider-unknown").into()
        },
        provider_kind: if identified { "modrinth" } else { "" }.into(),
        origin: origin_label.into(),
        origin_kind: origin_kind.into(),
        compatibility: record.compatibility.clone().into(),
        icon: artwork::logo(&record.icon.url),
        enabled: record.enabled,
        deleted: record.deleted,
        pack_owned: origin_kind == "pack",
        can_manage: identified,
        can_change_version: movable,
        can_update: movable,
        can_remove: movable || record.origin == "user",
        can_delete_file: !movable,
        restriction: restriction.into(),
        ..ModRow::default()
    }
}

/// The instances the screenshots show, recorded from imported packs and
/// instances built by hand. Which of them is running, starting, or needs
/// attention is a state chosen for the capture, not a recorded fact.
pub(crate) fn instance_rows() -> Vec<InstanceRow> {
    recorded::data()
        .instances
        .iter()
        .map(|record| {
            let row = InstanceRow {
                mods: record.mods,
                ..instance(
                    &record.key,
                    &record.name,
                    &record.minecraft,
                    (&record.loader_kind, &record.loader),
                    &record.storage,
                    &record.source,
                )
            };
            match record.key.as_str() {
                "photon" => running_instance(row),
                "cobblemon" => starting_instance(row),
                "fog" => InstanceRow {
                    needs_attention: true,
                    can_launch: false,
                    ..row
                },
                "skyblock-tools" => InstanceRow {
                    creating_server_detail: lookup("operation-stage-downloading-files").into(),
                    creating_server_progress: 0.4,
                    ..row
                },
                _ => row,
            }
        })
        .collect()
}

/// A server recorded from a real install, as a row in its stopped state.
pub(crate) fn recorded_server_row(record: &recorded::Server) -> ServerRow {
    ServerRow {
        storage: record.storage.clone().into(),
        storage_kind: record.storage.to_lowercase().into(),
        relationship: "Linked".into(),
        linked_client_id: record.linked_client.clone().into(),
        ..server(
            &record.key,
            &record.name,
            &record.minecraft,
            (&record.loader_kind, &record.loader),
            record.port,
        )
    }
}

/// The servers the screenshots show. A server that stopped during startup is a
/// recorded outcome; running and starting are states chosen for the capture.
pub(crate) fn server_rows() -> Vec<ServerRow> {
    recorded::data()
        .servers
        .iter()
        .map(|record| {
            let row = recorded_server_row(record);
            match record.key.as_str() {
                "photon-server" => running_server(row),
                "cobblemon-server" => starting_server(row),
                _ if record.failed => ServerRow {
                    state: lookup("gui-state-failed").into(),
                    state_kind: "failed".into(),
                    state_detail: lookup("gui-server-failed-detail").into(),
                    ..row
                },
                _ => row,
            }
        })
        .collect()
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

/// The mods of the recorded CaveStone instance, in the order the launcher
/// lists them: by title, ignoring case.
pub(crate) fn client_mod_rows() -> Vec<ModRow> {
    recorded::data()
        .cavestone_mods
        .iter()
        .map(recorded_mod_row)
        .collect()
}

/// The results of the recorded catalog search, as the mod browser shows them.
pub(crate) fn client_mod_results() -> Vec<ModSearchRow> {
    let note = lookup("gui-compatible");
    recorded::data()
        .mod_search
        .results
        .iter()
        .map(|hit| ModSearchRow {
            project_id: hit.project_id.clone().into(),
            title: hit.title.clone().into(),
            description: hit.description.clone().into(),
            author: hit.author.clone().into(),
            icon: artwork::logo(&hit.icon.url),
            note: note.clone().into(),
            installed: hit.installed,
            ..ModSearchRow::default()
        })
        .collect()
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
        browser_submitted: recorded::data().mod_search.query.clone().into(),
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
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn sizes_print_like_the_launcher_does() {
        assert_eq!(size_text(522_680), "511 KiB");
        assert_eq!(size_text(101_287_669), "96.6 MiB");
        assert_eq!(size_text(2_500_000_000), "2.3 GiB");
    }

    #[test]
    fn sample_accounts_are_valid_offline_names() {
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
    fn sample_hosts_stay_within_supported_loaders_and_anonymous_paths() {
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
    fn every_server_is_linked_to_a_listed_client() {
        let clients = instance_rows();
        assert!(server_rows().iter().all(|server| {
            server.relationship == "Linked"
                && clients
                    .iter()
                    .any(|client| client.id == server.linked_client_id)
        }));
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
        assert!(mods.iter().any(|row| !row.version_known));
    }

    #[test]
    fn the_mod_list_keeps_the_launchers_order() {
        let titles = client_mod_rows()
            .iter()
            .map(|row| row.title.to_lowercase())
            .collect::<Vec<_>>();
        let mut sorted = titles.clone();
        sorted.sort();
        assert_eq!(titles, sorted);
    }

    #[test]
    fn an_unidentified_jar_offers_only_deleting_its_file() {
        let mods = client_mod_rows();
        let jar = mods
            .iter()
            .find(|row| row.origin_kind == "hand-placed")
            .expect("a hand-placed jar is recorded");
        assert!(jar.can_delete_file && !jar.can_remove && !jar.can_update);
        assert!(
            jar.file_name.is_empty(),
            "the title already is the file name"
        );
    }
}
