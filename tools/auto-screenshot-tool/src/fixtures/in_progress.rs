//! Work under way: imports, launches, running servers, and the Activity and Running pages.

use ephervex_gui::ui::{ActivityRow, AppWindow, PendingEntityRow, RunningRow};
use ephervex_i18n::{Arg, lookup, lookup_args, lookup_attribute};
use slint::SharedString;

use super::{instance_rows, rows, server_rows};

const IMPORT_FILES_DONE: i64 = 31;
const IMPORT_FILES_TOTAL: i64 = 54;

/// The line under an operation title, composed as the Activity page composes it.
fn stage_detail(stage: &str, current: i64, total: i64) -> String {
    let count = lookup_args(
        "gui-operation-item-count",
        [
            ("current", Arg::Count(current)),
            ("total", Arg::Count(total)),
        ],
    );
    format!("{} | {count}", lookup(stage))
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn import_progress() -> f32 {
    (IMPORT_FILES_DONE as f64 / IMPORT_FILES_TOTAL as f64) as f32
}

/// Moves the row with this id to the front so the list shows it without scrolling.
fn bring_to_front<T>(mut items: Vec<T>, is_target: impl Fn(&T) -> bool) -> Vec<T> {
    if let Some(position) = items.iter().position(is_target) {
        let item = items.remove(position);
        items.insert(0, item);
    }
    items
}

pub(crate) fn import_modpack_running(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
    ui.set_pending_instances(rows(vec![PendingEntityRow {
        id: "creation-1".into(),
        name: "CaveStone-1.1.0-modrinth.mrpack".into(),
        target: lookup("gui-import-modpack").into(),
        loader_kind: "vanilla".into(),
        storage: lookup("gui-temporary").into(),
        storage_kind: "temporary".into(),
        state: lookup("gui-state-creating").into(),
        detail: stage_detail(
            "operation-stage-downloading-files",
            IMPORT_FILES_DONE,
            IMPORT_FILES_TOTAL,
        )
        .into(),
        progress: import_progress(),
        ..PendingEntityRow::default()
    }]));
    ui.set_active_operations(1);
    ui.set_status_message(lookup("operation-importing-pack").into());
}

pub(crate) fn activity_import_in_progress(ui: &AppWindow) {
    let failure = {
        let message = lookup("error-pack-manifest-invalid");
        match lookup_attribute("error-pack-manifest-invalid", "action") {
            Some(action) => format!("{message}\n{action}"),
            None => message,
        }
    };
    let activity =
        |id: &str, title: &str, entity: &str, status: &str, detail: String, time: &str| {
            ActivityRow {
                id: id.into(),
                title: lookup(title).into(),
                entity: entity.into(),
                status: lookup(status).into(),
                detail: detail.into(),
                time: time.into(),
                indeterminate: true,
                ..ActivityRow::default()
            }
        };
    ui.set_activities(rows(vec![
        ActivityRow {
            progress: import_progress(),
            indeterminate: false,
            active: true,
            cancellable: true,
            ..activity(
                "operation-14",
                "operation-importing-pack",
                "CaveStone",
                "gui-state-running",
                stage_detail(
                    "operation-stage-downloading-files",
                    IMPORT_FILES_DONE,
                    IMPORT_FILES_TOTAL,
                ),
                "14:32:08",
            )
        },
        ActivityRow {
            active: true,
            cancellable: true,
            ..activity(
                "operation-15",
                "operation-installing-mods",
                "Utility Mods",
                "gui-state-queued",
                String::new(),
                "14:32:11",
            )
        },
        ActivityRow {
            indeterminate: false,
            progress: 1.0,
            ..activity(
                "operation-13",
                "operation-creating-instance",
                "Vanilla Test",
                "gui-state-completed",
                lookup("operation-stage-publishing"),
                "14:29:46",
            )
        },
        ActivityRow {
            failed: true,
            ..activity(
                "operation-12",
                "operation-importing-pack",
                "Create+",
                "gui-state-failed",
                failure,
                "14:21:03",
            )
        },
        activity(
            "operation-11",
            "operation-creating-snapshot",
            "CaveStone",
            "gui-state-canceled",
            String::new(),
            "14:17:52",
        ),
    ]));
    ui.set_active_operations(2);
    ui.set_status_message(lookup("operation-importing-pack").into());
    ui.set_workspace_open(false);
    ui.set_page(3);
}

pub(crate) fn instance_launching(ui: &AppWindow) {
    let launching = bring_to_front(instance_rows(), |row| row.state_kind == "starting");
    ui.set_selected_instance_id(launching[0].id.clone());
    ui.set_selected_instance_index(0);
    ui.set_instances(rows(launching));
    ui.set_active_operations(1);
    ui.set_status_message(lookup("operation-launching-minecraft").into());
    ui.set_workspace_open(false);
    ui.set_page(0);
}

fn servers_page_with(ui: &AppWindow, state_kind: &str) {
    let servers = bring_to_front(server_rows(), |row| row.state_kind == state_kind);
    ui.set_selected_global_server_id(servers[0].id.clone());
    ui.set_selected_global_server_index(0);
    ui.set_global_servers(rows(servers));
    ui.set_workspace_open(false);
    ui.set_page(1);
}

pub(crate) fn server_starting(ui: &AppWindow) {
    servers_page_with(ui, "starting");
    ui.set_active_operations(1);
    ui.set_status_message(lookup("operation-starting-server").into());
}

pub(crate) fn server_running(ui: &AppWindow) {
    servers_page_with(ui, "running");
}

fn uptime_minutes(minutes: i64) -> SharedString {
    lookup_args("gui-uptime-minutes", [("count", Arg::Count(minutes))]).into()
}

fn usage(cpu: &str, memory: &str) -> SharedString {
    lookup_args(
        "gui-process-usage",
        [("cpu", Arg::from(cpu)), ("memory", Arg::from(memory))],
    )
    .into()
}

pub(crate) fn running_page(ui: &AppWindow) {
    ui.set_running(rows(vec![
        RunningRow {
            kind: "client".into(),
            id: "photon".into(),
            name: "Photon".into(),
            pid: "48213".into(),
            uptime: uptime_minutes(12),
            usage: usage("214", "6.1 GiB"),
            last_line: "[14Oct2026 19:24:54.763] [Render thread/INFO] [mezz.jei.core.util.LoggedTimer/]: Starting JEI took 3.518 s".into(),
            can_stop: true,
            ..RunningRow::default()
        },
        RunningRow {
            kind: "server".into(),
            id: "photon-server".into(),
            name: "Photon Server".into(),
            pid: "48999".into(),
            uptime: lookup_args(
                "gui-uptime-hours",
                [("hours", Arg::from(2)), ("minutes", Arg::from(1))],
            )
            .into(),
            usage: usage("61", "2.4 GiB"),
            last_line: "[14Oct2026 19:18:14.960] [Server thread/INFO] [net.minecraft.server.dedicated.DedicatedServer/]: Done (7.039s)! For help, type \"help\""
                .into(),
            controllable: true,
            can_stop: true,
            ..RunningRow::default()
        },
    ]));
    ui.set_workspace_open(false);
    ui.set_page(2);
}
