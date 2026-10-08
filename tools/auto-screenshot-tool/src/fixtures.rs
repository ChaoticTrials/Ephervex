//! Synthetic presentation rows. No launcher storage or service is opened.

use ephervex_gui::ui::{
    AccountRow, AppWindow, HostView, InstanceRow, ModRow, ServerRow, WorkspacePaneRow,
    WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::lookup;
use slint::{Model, ModelRc, VecModel};

fn rows<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn instance(id: &str, name: &str, minecraft: &str, storage: &str) -> InstanceRow {
    let temporary = storage == "Temporary";
    InstanceRow {
        id: id.into(),
        name: name.into(),
        minecraft: minecraft.into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: storage.into(),
        storage_kind: if temporary { "temporary" } else { "persistent" }.into(),
        source: "Modrinth pack 1.1.0".into(),
        path: format!("/demo/instances/{id}").into(),
        updated: "Today".into(),
        mods: 54,
        state: "Ready".into(),
        state_kind: "ready".into(),
        can_launch: true,
        ..InstanceRow::default()
    }
}

fn server() -> ServerRow {
    ServerRow {
        id: "cavestone-server".into(),
        name: "CaveStone Server".into(),
        minecraft: "1.20.1".into(),
        loader: "Forge 47.4.10".into(),
        loader_kind: "forge".into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        relationship: "Standalone".into(),
        state: "Stopped".into(),
        state_kind: "stopped".into(),
        state_detail: lookup("gui-state-ready-to-start").into(),
        address: "127.0.0.1:25565".into(),
        path: "/demo/servers/cavestone-server".into(),
        installed: true,
        can_start: true,
        ..ServerRow::default()
    }
}

fn account(name: &str, is_default: bool) -> AccountRow {
    AccountRow {
        id: name.to_ascii_lowercase().into(),
        name: name.into(),
        kind: "Offline".into(),
        is_default,
        credential_state: "No credential".into(),
    }
}

fn mod_row(title: &str, version: &str, file_name: &str) -> ModRow {
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
        enabled: true,
        pack_owned: true,
        ..ModRow::default()
    }
}

pub(crate) fn install_base(ui: &AppWindow) {
    ui.set_loading(false);
    ui.set_app_version("0.10.0".into());
    ui.set_status_message(lookup("gui-ready").into());
    ui.set_application_text_scale(100);
    ui.set_accounts(rows(vec![
        account("MelanX", true),
        account("_jeb", false),
        account("Dinnerbone", false),
    ]));
    ui.set_instances(rows(vec![
        instance("cavestone", "CaveStone", "1.20.1", "Temporary"),
        InstanceRow {
            loader: "Vanilla".into(),
            loader_kind: "vanilla".into(),
            source: "Created in Ephervex".into(),
            mods: 0,
            ..instance("vanilla-test", "Vanilla Test", "1.21.1", "Persistent")
        },
    ]));
    ui.set_global_servers(rows(vec![server()]));
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
        mods: rows(vec![
            mod_row("Create", "0.5.1.j", "create-1.20.1-0.5.1.j.jar"),
            mod_row("Jade", "11.13.2", "Jade-1.20.1-Forge-11.13.2.jar"),
            mod_row("JEI", "15.20.0.130", "jei-1.20.1-forge-15.20.0.130.jar"),
        ]),
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
    use slint::SharedString;

    #[test]
    fn sample_accounts_have_no_credentials() {
        let accounts = [account("MelanX", true), account("_jeb", false)];
        assert!(accounts.iter().all(|account| account.kind == "Offline"));
        assert!(
            accounts
                .iter()
                .all(|account| account.credential_state == "No credential")
        );
        assert!(accounts.iter().all(|account| !account.name.contains(' ')));
    }

    #[test]
    fn sample_paths_are_synthetic() {
        let client = instance("cavestone", "CaveStone", "1.20.1", "Temporary");
        assert_eq!(client.path, SharedString::from("/demo/instances/cavestone"));
        assert!(server().path.starts_with("/demo/"));
        assert_eq!(rows(vec![client]).row_count(), 1);
        assert!(!server().state_detail.is_empty());
    }
}
