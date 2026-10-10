//! Confirmations and prompts that start from an instance card or an `ephervex://` link.

use ephervex_gui::ui::{AppWindow, InstanceRow, PromoteServerRow};
use slint::{Model, SharedString};

use super::rows;

pub(crate) fn instances_page(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
    ui.set_modal_error(SharedString::default());
}

fn client(ui: &AppWindow) -> InstanceRow {
    ui.get_instances().row_data(0).unwrap_or_default()
}

/// Moving a temporary instance to persistent storage, with its linked server offered.
pub(crate) fn instance_make_persistent_dialog(ui: &AppWindow) {
    instances_page(ui);
    let client = client(ui);
    ui.set_pending_action_id(client.id);
    ui.set_promote_instance_name(client.name);
    ui.set_promote_destination("/demo/ephervex/instances/cavestone".into());
    ui.set_promote_servers(rows(vec![PromoteServerRow {
        id: "cavestone-server".into(),
        display: "CaveStone Server".into(),
        selected: true,
    }]));
    ui.set_modal("make-persistent".into());
}

/// Removing an instance that has a running linked server.
pub(crate) fn instance_delete_confirmation(ui: &AppWindow) {
    instances_page(ui);
    let client = client(ui);
    ui.set_pending_action_id(client.id);
    ui.set_pending_action_name(client.name);
    ui.set_pending_action_linked_servers(1);
    ui.set_pending_action_stoppable_linked_servers(1);
    ui.set_pending_action_unstoppable_linked_servers(0);
    ui.set_pending_snapshot_deletion_note(
        ephervex_i18n::lookup_args(
            "gui-delete-host-snapshots",
            [("count", ephervex_i18n::Arg::Count(4))],
        )
        .into(),
    );
    ui.set_modal("delete-instance".into());
}

/// Choosing another build of the same loader for a pack instance.
pub(crate) fn instance_change_loader_dialog(ui: &AppWindow) {
    instances_page(ui);
    ui.set_pending_action_id(client(ui).id);
    ui.set_loader_change_current("47.4.10".into());
    ui.set_loader_change_versions(rows(
        ["47.4.10", "47.4.9", "47.4.8", "47.3.12", "47.2.20"]
            .map(SharedString::from)
            .to_vec(),
    ));
    ui.set_loader_change_selected("47.4.9".into());
    ui.set_loader_change_loading(false);
    ui.set_loader_change_unverifiable(2);
    ui.set_modal("loader-change".into());
}

/// The approval asked for before a link downloads from a host not yet trusted.
pub(crate) fn import_link_trust_prompt(ui: &AppWindow) {
    instances_page(ui);
    ui.set_pack_link_url("https://packs.example.invalid/creeper-containment-lab-2.3.0.zip".into());
    ui.set_pack_link_display(
        "https://packs.example.invalid/creeper-containment-lab-2.3.0.zip".into(),
    );
    ui.set_pack_link_host("packs.example.invalid".into());
    ui.set_pack_link_redirected(false);
    ui.set_pack_link_remember(false);
    ui.set_pack_link_downloading(false);
    ui.set_modal("open-pack-link".into());
}

/// The instance list with the selected client's account choice for one launch.
pub(crate) fn account_launch_picker(ui: &AppWindow) {
    instances_page(ui);
}
