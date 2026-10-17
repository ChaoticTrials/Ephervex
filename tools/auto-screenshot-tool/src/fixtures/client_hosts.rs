//! The CaveStone client workspace on its overview, settings, snapshot, and log surfaces.

use ephervex_gui::ui::{
    AppWindow, DiagnosticRow, HostView, InstanceRow, LogChunk, LogView, PendingEntityRow,
    ServerRow, SnapshotRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ComponentHandle, Model, SharedString};

use super::java;
use super::{
    FORGE_1_20, expand_tabs, log_level, recorded, recorded_server_row, rows, running_server,
    size_text,
};

/// The states this module adds to the catalog.
#[cfg(test)]
const STATE_NAMES: [&str; 11] = [
    "client-overview-linked-server",
    "client-settings",
    "client-java-runtime",
    "client-java-problem",
    "client-java-review",
    "client-java-install",
    "snapshots",
    "snapshot-create-dialog",
    "snapshot-restore-dialog",
    "client-logs-live",
    "client-crash-reports",
];

/// Opens the first client's workspace on one surface of its single pane.
fn open_surface(
    ui: &AppWindow,
    surface: Surface,
    configure: impl FnOnce(&mut HostView),
    log: Option<LogView>,
) {
    open_instance_surface(ui, "cavestone", surface, configure, log);
}

/// Opens the workspace of the instance with `key` on `surface`.
fn open_instance_surface(
    ui: &AppWindow,
    key: &str,
    surface: Surface,
    configure: impl FnOnce(&mut HostView),
    log: Option<LogView>,
) {
    let client = ui
        .get_instances()
        .iter()
        .find(|row| row.id == key)
        .unwrap_or_default();
    let name = client.name.clone();
    let surfaces = Surface::client()
        .iter()
        .map(|offered| WorkspaceSurfaceRow {
            id: offered.id().into(),
            label: lookup(offered.label()).into(),
        })
        .collect::<Vec<_>>();
    let choice = Surface::client()
        .iter()
        .position(|offered| *offered == surface)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or_default();
    let mut view = HostView {
        kind: "client".into(),
        id: client.id.clone(),
        available: true,
        client,
        surfaces: rows(surfaces),
        allow_pack_mod_changes: true,
        ..HostView::default()
    };
    configure(&mut view);
    ui.set_host_views(rows(vec![view]));
    let log_index = if let Some(log) = log {
        ui.set_log_views(rows(vec![log]));
        0
    } else {
        -1
    };
    ui.set_workspace_panes(rows(vec![WorkspacePaneRow {
        id: 1,
        host: 0,
        log: log_index,
        surface: surface.id().into(),
        label: format!("{name}, {}", lookup(surface.label())).into(),
        surface_choice: choice,
        can_split_beside: true,
        ..WorkspacePaneRow::default()
    }]));
    ui.set_workspace_back_label(lookup("gui-instances").into());
    ui.set_page(0);
    ui.set_workspace_open(true);
}

fn target(minecraft: &str, loader: &str) -> SharedString {
    lookup_args(
        "gui-target-summary",
        [
            ("first", Arg::from(minecraft)),
            ("second", Arg::from(loader)),
        ],
    )
    .into()
}

/// A client paired with a running linked server, and a second server still being created.
pub(crate) fn client_overview_linked_server(ui: &AppWindow) {
    let linked = ServerRow {
        can_connect: true,
        ..running_server(recorded_server_row(recorded::server("cavestone-server")))
    };
    let pending = PendingEntityRow {
        id: "creating-cavestone-test-server".into(),
        name: "CaveStone Test Server".into(),
        target: target("1.20.1", FORGE_1_20.display),
        loader_kind: FORGE_1_20.kind.into(),
        storage: "Temporary".into(),
        storage_kind: "temporary".into(),
        state: lookup("gui-state-creating").into(),
        server: true,
        linked_client_id: "cavestone".into(),
        detail: lookup("operation-stage-downloading-files").into(),
        progress: 0.4,
        indeterminate: false,
    };
    open_surface(
        ui,
        Surface::Overview,
        |view| {
            view.client = InstanceRow {
                linked_servers: 1,
                stoppable_linked_servers: 1,
                connect_server_id: linked.id.clone(),
                connect_server_name: linked.name.clone(),
                can_connect: true,
                paired_server_id: linked.id.clone(),
                paired_server_name: linked.name.clone(),
                pair_start_together: true,
                ..view.client.clone()
            };
            view.pair_state = linked.state.clone();
            view.pair_state_kind = linked.state_kind.clone();
            view.pair_room = true;
            view.linked_servers = rows(vec![linked]);
            view.pending_servers = rows(vec![pending]);
        },
        None,
    );
}

/// What a host's runtime form says it follows when left empty: the memory
/// range and the automatic Java choice of the global settings.
pub(crate) fn global_runtime() -> SharedString {
    lookup_args(
        "gui-instance-runtime-global",
        [
            ("first", Arg::from("1024 - 6144 MiB")),
            ("second", Arg::from(lookup("gui-java-auto-selected"))),
        ],
    )
    .into()
}

/// The runtime form of a client that sets its own memory and arguments.
pub(crate) fn client_settings(ui: &AppWindow) {
    open_surface(
        ui,
        Surface::Settings,
        |view| {
            view.runtime_memory_min = "2048".into();
            view.runtime_memory_max = "6144".into();
            view.runtime_jvm_args = "-XX:+UseG1GC -XX:MaxGCPauseMillis=100".into();
            view.runtime_pinned = true;
            view.runtime_global = global_runtime();
            view.runtime_revision = 1;
            view.allow_pack_mod_changes = true;
        },
        None,
    );
}

/// The Settings of an instance whose pinned Java cannot run it, after the
/// launch it refused.
pub(crate) fn client_java_problem(ui: &AppWindow) {
    open_instance_surface(ui, "fog", Surface::Settings, refused_launch, None);
}

/// The same instance reviewing a switch back to automatic selection.
pub(crate) fn client_java_review(ui: &AppWindow) {
    open_instance_surface(
        ui,
        "fog",
        Surface::Settings,
        |view| {
            refused_launch(view);
            java::review_automatic(view);
        },
        None,
    );
}

/// Legacy Forge, whose Java 8 ceiling no installed runtime meets, offering
/// to install one.
pub(crate) fn client_java_install(ui: &AppWindow) {
    open_instance_surface(ui, "super-tiny", Surface::Settings, java::fill_host, None);
}

fn refused_launch(view: &mut HostView) {
    java::fill_host(view);
    view.run_failure_title = lookup("gui-run-failed-client").into();
    view.run_failure = java::refusal(view).unwrap_or_default().into();
    view.run_likely_cause = lookup_args(
        "gui-run-likely-cause",
        [(
            "cause",
            Arg::from(lookup("gui-diagnosis-java-incompatible")),
        )],
    )
    .into();
}

fn snapshot(
    id: &str,
    label: &str,
    captured: &str,
    size: &str,
    mods: (u32, u32),
    origin: &str,
    full_copy: bool,
) -> SnapshotRow {
    SnapshotRow {
        id: id.into(),
        label: label.into(),
        captured: captured.into(),
        size: size.into(),
        mods: lookup_args(
            "gui-snapshot-mod-counts",
            [
                ("enabled", Arg::from(mods.0)),
                ("disabled", Arg::from(mods.1)),
            ],
        )
        .into(),
        origin: lookup(origin).into(),
        copy_method: lookup(if full_copy {
            "gui-snapshot-copy"
        } else {
            "gui-snapshot-reflink"
        })
        .into(),
        uses_full_space: full_copy,
    }
}

fn snapshot_rows() -> Vec<SnapshotRow> {
    recorded::data()
        .snapshots
        .iter()
        .map(|record| {
            snapshot(
                &record.id,
                &record.label,
                &record.captured,
                &size_text(record.size_bytes),
                (record.enabled, record.disabled),
                match record.origin.as_str() {
                    "before_restore" => "gui-snapshot-origin-before-restore",
                    "before_isolation" => "gui-snapshot-origin-before-isolation",
                    _ => "gui-snapshot-origin-manual",
                },
                record.copy_method == "copy",
            )
        })
        .collect()
}

fn show_snapshots(ui: &AppWindow) {
    open_surface(
        ui,
        Surface::Snapshots,
        |view| {
            view.snapshots = rows(snapshot_rows());
            view.snapshot_total = size_text(
                recorded::data()
                    .snapshots
                    .iter()
                    .map(|record| record.size_bytes)
                    .sum(),
            )
            .into();
            view.snapshots_have_plain_copies = true;
        },
        None,
    );
}

/// The client's snapshots, one of them a full copy.
pub(crate) fn snapshots(ui: &AppWindow) {
    show_snapshots(ui);
}

/// The label prompt for a new snapshot, with the suggested label as its placeholder.
pub(crate) fn snapshot_create_dialog(ui: &AppWindow) {
    show_snapshots(ui);
    ui.set_snapshot_host_kind("client".into());
    ui.set_snapshot_host_id("cavestone".into());
    ui.set_snapshot_action_label(SharedString::default());
    ui.set_snapshot_label_suggestion("CaveStone 2026-10-14 19:58".into());
    ui.set_modal_error(SharedString::default());
    ui.set_modal("snapshot-create".into());
}

/// The confirmation to restore a snapshot, with the safety copy on.
pub(crate) fn snapshot_restore_dialog(ui: &AppWindow) {
    show_snapshots(ui);
    ui.set_snapshot_host_kind("client".into());
    ui.set_snapshot_host_id("cavestone".into());
    let snapshot = recorded::data()
        .snapshots
        .iter()
        .find(|record| record.label == "Pre-Ender-Dragon");
    ui.set_snapshot_action_id(
        snapshot.map_or_else(SharedString::default, |it| it.id.as_str().into()),
    );
    ui.set_snapshot_action_label("Pre-Ender-Dragon".into());
    ui.set_snapshot_safety_copy(true);
    ui.set_modal_error(SharedString::default());
    ui.set_modal("snapshot-restore".into());
}

/// The recorded output of the CaveStone client, one chunk per line.
fn log_chunks() -> Vec<LogChunk> {
    recorded::CLIENT_LOG
        .lines()
        .map(|line| LogChunk {
            text: line.into(),
            level: log_level(line).into(),
        })
        .collect()
}

/// The live log of a running client, with every level shown and following on.
pub(crate) fn client_logs_live(ui: &AppWindow) {
    let chunks = log_chunks();
    let columns = chunks
        .iter()
        .map(|chunk| chunk.text.chars().count())
        .max()
        .unwrap_or(80);
    let records = lookup_args(
        "gui-retained-log-records",
        [("count", Arg::from(chunks.len()))],
    );
    // A followed log sits at its newest line, which the viewer does once a
    // new line arrives after its first layout.
    let weak = ui.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(ui) = weak.upgrade() {
            let views = ui.get_log_views();
            if let Some(mut view) = views.row_data(0) {
                view.revision += 1;
                views.set_row_data(0, view);
            }
        }
    });
    open_surface(
        ui,
        Surface::Logs,
        |_| {},
        Some(LogView {
            mode: "live".into(),
            has_content: true,
            status: records.into(),
            content_columns: i32::try_from(columns).unwrap_or(80),
            chunks: rows(chunks),
            follow: true,
            trace_enabled: true,
            debug_enabled: true,
            info_enabled: true,
            warn_enabled: true,
            error_enabled: true,
            fatal_enabled: true,
            other_enabled: true,
            revision: 1,
            selection_start_row: -1,
            selection_end_row: -1,
            ..LogView::default()
        }),
    );
}

fn crash_file(name: &str, modified: &str, size: &str, description: &str) -> DiagnosticRow {
    DiagnosticRow {
        scope_kind: "client".into(),
        scope_id: "cavestone".into(),
        entity: "CaveStone".into(),
        kind: "crash-report".into(),
        name: name.into(),
        path: format!("/demo/instances/cavestone/minecraft/crash-reports/{name}").into(),
        modified: modified.into(),
        size: size.into(),
        summary: description.into(),
    }
}

/// The crash reports of a client, with the newest one open.
pub(crate) fn client_crash_reports(ui: &AppWindow) {
    let crash = &recorded::diagnostics().client_crash;
    let newest = crash.report.as_str();
    let files = vec![crash_file(
        newest,
        "Today",
        &size_text(crash.size_bytes),
        &crash.summary,
    )];
    open_surface(
        ui,
        Surface::Logs,
        |_| {},
        Some(LogView {
            mode: "crashes".into(),
            file: newest.into(),
            files: rows(files),
            report: expand_tabs(recorded::CLIENT_CRASH).into(),
            follow: true,
            trace_enabled: true,
            debug_enabled: true,
            info_enabled: true,
            warn_enabled: true,
            error_enabled: true,
            fatal_enabled: true,
            other_enabled: true,
            selection_start_row: -1,
            selection_end_row: -1,
            ..LogView::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::capture::{CaptureOptions, Dimensions, ThemeName, generate};

    #[test]
    fn every_client_host_state_renders_in_a_full_window() {
        let _session = crate::capture::render_session();
        let states = crate::catalog::STATES
            .iter()
            .filter(|state| STATE_NAMES.contains(&state.name))
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(states.len(), STATE_NAMES.len());
        let output = tempfile::tempdir().expect("temporary output");
        let files = generate(&CaptureOptions {
            states: &states,
            themes: &[ThemeName::Light],
            sizes: &[Dimensions::HD],
            output: output.path(),
            derive_1080: false,
        })
        .expect("capture the client host states");
        assert_eq!(files.len(), STATE_NAMES.len());
        assert!(files.iter().all(|path| path.is_file()));
    }

    #[test]
    fn the_snapshot_list_warns_about_a_plain_copy_it_contains() {
        let rows = snapshot_rows();
        assert!(rows.iter().any(|row| row.uses_full_space));
        assert!(rows.iter().all(|row| !row.origin.is_empty()));
    }

    #[test]
    fn the_recorded_log_mixes_levels_and_keeps_every_line_a_record() {
        let chunks = log_chunks();
        assert!((100..=200).contains(&chunks.len()));
        for level in ["info", "warn", "error", "fatal"] {
            assert!(chunks.iter().any(|chunk| chunk.level == level));
        }
        assert!(chunks.iter().all(|chunk| chunk.level != "other"));
    }
}
