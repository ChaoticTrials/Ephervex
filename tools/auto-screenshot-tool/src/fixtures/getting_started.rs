//! First-run screens: the empty list, the creation dialog, and the import dialog.

use ephervex_gui::ui::{AppWindow, BlockedFileRow, ImportDuplicateRow, ImportOptionRow};
use ephervex_i18n::{Arg, lookup_args, lookup_attribute_args};
use slint::SharedString;

use super::{FORGE_1_12, FORGE_1_20, rows};

fn strings(values: &[&str]) -> Vec<SharedString> {
    values
        .iter()
        .map(|value| SharedString::from(*value))
        .collect()
}

/// A pack as the import dialog describes it before anything is installed.
struct PackFacts {
    format: &'static str,
    name: &'static str,
    version: &'static str,
    author: &'static str,
    minecraft: &'static str,
    loader: &'static str,
    required: usize,
    optional: usize,
    archive: &'static str,
}

/// The documented CaveStone Modrinth pack. Modrinth manifests name no author.
const CAVESTONE: PackFacts = PackFacts {
    format: "Modrinth",
    name: "CaveStone",
    version: "1.1.0",
    author: "",
    minecraft: "1.20.1",
    loader: FORGE_1_20.display,
    required: 50,
    optional: 2,
    archive: "/demo/downloads/CaveStone-1.1.0-modrinth.mrpack",
};

/// A synthetic CurseForge export, the format whose files can be withheld.
const CONTAINMENT_LAB: PackFacts = PackFacts {
    format: "CurseForge",
    name: "Creeper Containment Lab",
    version: "2.3.0",
    author: "demo_author",
    minecraft: "1.12.2",
    loader: FORGE_1_12.display,
    required: 38,
    optional: 0,
    archive: "/demo/downloads/Creeper-Containment-Lab-2.3.0.zip",
};

fn open_import(ui: &AppWindow, pack: &PackFacts, status: &str) {
    ui.set_modal("import".into());
    ui.set_import_path(pack.archive.into());
    ui.set_import_name(pack.name.into());
    ui.set_import_status_kind(status.into());
    ui.set_import_format(pack.format.into());
    ui.set_import_pack_header(
        lookup_args(
            "gui-import-pack-header",
            [
                ("first", Arg::from(pack.format)),
                (
                    "second",
                    Arg::from(format!("{} {}", pack.name, pack.version)),
                ),
            ],
        )
        .into(),
    );
    ui.set_import_author_line(if pack.author.is_empty() {
        SharedString::default()
    } else {
        lookup_args("gui-import-author", [("value", Arg::from(pack.author))]).into()
    });
    ui.set_import_minecraft_line(
        lookup_args(
            "gui-minecraft-version-label",
            [("value", Arg::from(pack.minecraft))],
        )
        .into(),
    );
    ui.set_import_loader_line(pack.loader.into());
    ui.set_import_required_line(
        lookup_args(
            "gui-import-required-count",
            [("count", Arg::from(pack.required))],
        )
        .into(),
    );
    ui.set_import_optional_line(
        lookup_args(
            "gui-import-optional-count",
            [("count", Arg::from(pack.optional))],
        )
        .into(),
    );
}

fn optional_item(path: &str, selected: bool) -> ImportOptionRow {
    ImportOptionRow {
        logical_path: path.into(),
        display: path.into(),
        selected,
    }
}

fn blocked_file(project: &str, file: &str, id: u32) -> BlockedFileRow {
    BlockedFileRow {
        identity: format!("curseforge:{id}").into(),
        project_line: project.into(),
        file_line: file.into(),
        browser_url: format!("https://example.invalid/files/{id}").into(),
    }
}

pub(crate) fn instances_empty(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
    ui.set_instances(rows(Vec::new()));
    ui.set_pending_instances(rows(Vec::new()));
    ui.set_selected_instance_id(SharedString::default());
    ui.set_selected_instance_index(-1);
}

pub(crate) fn create_instance_dialog(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
    ui.set_minecraft_releases(rows(strings(&[
        "26.2", "1.21.1", "1.20.1", "1.19.4", "1.18.2", "1.16.5", "1.12.2",
    ])));
    ui.set_favorite_minecraft_versions(rows(strings(&["1.20.1", "1.21.1"])));
    ui.set_create_name("Redstone Lag Repro".into());
    ui.set_create_name_suggestion("Minecraft 1.20.1 Forge".into());
    ui.set_create_minecraft("1.20.1".into());
    ui.set_create_minecraft_favorite(true);
    ui.set_create_loader("Forge".into());
    ui.set_create_loader_versions(rows(strings(&["47.4.10", "47.4.9", "47.4.8", "47.3.12"])));
    ui.set_create_loader_version("47.4.10".into());
    ui.set_create_persistent(false);
    ui.set_modal("create-instance".into());
}

pub(crate) fn import_modpack_ready(ui: &AppWindow) {
    open_import(ui, &CAVESTONE, "ready");
    ui.set_import_ready(true);
    ui.set_import_optional_items(rows(vec![
        optional_item("mods/redstone-rabbit-holes-1.20.1-1.4.0.jar", true),
        optional_item("mods/villager-notice-board-1.20.1-0.8.2.jar", false),
    ]));
}

pub(crate) fn import_modpack_duplicates(ui: &AppWindow) {
    open_import(ui, &CONTAINMENT_LAB, "ready");
    ui.set_import_ready(true);
    ui.set_import_duplicate_items(rows(vec![ImportDuplicateRow {
        project_line: "Ender Pearl Pager".into(),
        kept_line: "ender-pearl-pager-forge-2.1.7.jar".into(),
        discarded_line: "ender-pearl-pager-forge-2.0.9.jar".into(),
        matched_minecraft_version: true,
    }]));
}

pub(crate) fn import_modpack_manual_download(ui: &AppWindow) {
    open_import(ui, &CONTAINMENT_LAB, "ready");
    ui.set_import_ready(false);
    ui.set_import_blocked_items(rows(vec![blocked_file(
        "Obsidian Mirror",
        "obsidian-mirror-1.12.2-0.9.1.jar",
        1_048_301,
    )]));
}

pub(crate) fn import_modpack_credential_missing(ui: &AppWindow) {
    ui.set_modal("import".into());
    ui.set_import_path(CONTAINMENT_LAB.archive.into());
    ui.set_import_status_kind("failed".into());
    ui.set_import_error_kind("missing-credential".into());
    let provider = [("provider", Arg::from("CurseForge"))];
    let action = lookup_attribute_args("error-mod-credential-missing", "action", provider.clone())
        .unwrap_or_default();
    ui.set_modal_error(
        format!(
            "{}\n{action}",
            lookup_args("error-mod-credential-missing", provider)
        )
        .into(),
    );
}

pub(crate) fn import_modpack_resolving(ui: &AppWindow) {
    open_import(ui, &CONTAINMENT_LAB, "resolving");
    ui.set_import_ready(false);
}

pub(crate) fn file_drop_overlay(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
    ui.set_file_drop_kind("import".into());
    ui.set_file_drop_active(true);
}
