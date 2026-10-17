//! Diagnostics, failed-run notices, and server workspace surfaces.

use ephervex_gui::ui::{
    AppWindow, BundlePartRow, DiagnosticCheckRow, DiagnosticFindingRow, DiagnosticRow,
    FixChangeRow, HostView, InstanceRow, LogChunk, LogView, ServerRow, WorkspacePaneRow,
    WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{Model, ModelRc, SharedString, VecModel};

use super::java::{JAVA_17, JAVA_21, Runtime};
use super::recorded::{self, Diagnostics, Search};
use super::{
    cavestone_server_mod_rows, client_mod_results, client_mod_rows, expand_tabs, log_level, rows,
    running_instance, running_server, server_mod_results, size_text,
};

fn strings(values: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| SharedString::from(*value))
            .collect::<Vec<_>>(),
    ))
}

fn surface_rows(surfaces: &[Surface]) -> ModelRc<WorkspaceSurfaceRow> {
    rows(
        surfaces
            .iter()
            .map(|surface| WorkspaceSurfaceRow {
                id: surface.id().into(),
                label: lookup(surface.label()).into(),
            })
            .collect(),
    )
}

fn client_view(client: InstanceRow) -> HostView {
    HostView {
        kind: "client".into(),
        id: client.id.clone(),
        available: true,
        client,
        surfaces: surface_rows(Surface::client()),
        ..HostView::default()
    }
}

fn server_view(server: ServerRow) -> HostView {
    HostView {
        kind: "server".into(),
        id: server.id.clone(),
        available: true,
        server,
        surfaces: surface_rows(Surface::server()),
        ..HostView::default()
    }
}

/// A pane on `surface`, whose position in the host's surface list is the choice shown.
fn pane(id: i32, host: i32, surface: &str, label: &str, server: bool) -> WorkspacePaneRow {
    let offered = if server {
        Surface::server()
    } else {
        Surface::client()
    };
    let choice = offered
        .iter()
        .position(|offered| offered.id() == surface)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or_default();
    WorkspacePaneRow {
        id,
        host,
        log: -1,
        surface: surface.into(),
        label: label.into(),
        surface_choice: choice,
        can_split_beside: true,
        ..WorkspacePaneRow::default()
    }
}

fn cavestone(ui: &AppWindow) -> InstanceRow {
    ui.get_instances().row_data(0).unwrap_or_default()
}

fn named_server(ui: &AppWindow, id: &str) -> ServerRow {
    ui.get_global_servers()
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_default()
}

fn open_client(ui: &AppWindow, view: HostView, surface: &str) {
    let label = format!("{}, {surface}", view.client.name);
    ui.set_host_views(rows(vec![view]));
    ui.set_workspace_panes(rows(vec![pane(1, 0, surface, &label, false)]));
    ui.set_workspace_back_label(lookup("gui-instances").into());
    ui.set_page(0);
    ui.set_workspace_open(true);
}

fn open_server(ui: &AppWindow, view: HostView, surface: &str, log: Option<LogView>) {
    let label = format!("{}, {surface}", view.server.name);
    let mut pane = pane(1, 0, surface, &label, true);
    if let Some(log) = log {
        ui.set_log_views(rows(vec![log]));
        pane.log = 0;
    }
    ui.set_host_views(rows(vec![view]));
    ui.set_workspace_panes(rows(vec![pane]));
    ui.set_workspace_back_label(lookup("gui-servers").into());
    ui.set_page(1);
    ui.set_workspace_open(true);
}

fn finding(
    key: &str,
    problem: bool,
    title: &str,
    detail: &str,
    chain: &[&str],
    files: &[&str],
) -> DiagnosticFindingRow {
    DiagnosticFindingRow {
        key: key.into(),
        problem,
        title: title.into(),
        detail: detail.into(),
        chain: strings(chain),
        files: strings(files),
        ..DiagnosticFindingRow::default()
    }
}

fn with_fix(row: DiagnosticFindingRow) -> DiagnosticFindingRow {
    DiagnosticFindingRow {
        fix_label: lookup("gui-diagnosis-review-fix").into(),
        ..row
    }
}

fn with_route(row: DiagnosticFindingRow, route: &str, label: &str) -> DiagnosticFindingRow {
    DiagnosticFindingRow {
        route: route.into(),
        route_label: lookup(label).into(),
        ..row
    }
}

fn check(label: &str, value: String, state: &str, state_label: &str) -> DiagnosticCheckRow {
    DiagnosticCheckRow {
        label: label.into(),
        value: value.into(),
        state: state.into(),
        state_label: lookup(state_label).into(),
    }
}

/// The files the verification found missing or damaged, as the findings list them.
fn damaged_files(diagnostics: &Diagnostics) -> Vec<String> {
    diagnostics
        .files
        .problems
        .iter()
        .map(|problem| format!("{} ({})", problem.path, problem.state))
        .collect()
}

fn client_problems() -> Vec<DiagnosticFindingRow> {
    let diagnostics = recorded::diagnostics();
    let files = damaged_files(diagnostics);
    let listed = files.iter().map(String::as_str).collect::<Vec<_>>();
    let count = Arg::from(files.len());
    vec![with_fix(finding(
        "p0",
        true,
        &lookup_args("gui-diagnosis-files-damaged", [("count", count.clone())]),
        &lookup_args("gui-diagnosis-files-restorable", [("count", count)]),
        &[],
        &listed,
    ))]
}

fn client_customizations() -> Vec<DiagnosticFindingRow> {
    let java = &JAVA_17;
    vec![with_route(
        finding(
            "c0",
            false,
            &lookup_args(
                "gui-diagnosis-java-custom",
                [("version", Arg::from(recorded::diagnostics().java.as_str()))],
            ),
            &[
                java.line(),
                java.path.to_owned(),
                lookup_args(
                    "gui-diagnosis-java-satisfies",
                    [
                        ("minecraft", Arg::from("1.20.1")),
                        ("major", Arg::from("17")),
                    ],
                ),
                lookup("gui-diagnosis-java-pinned-host"),
            ]
            .join("\n"),
            &[],
            &[],
        ),
        "settings",
        "gui-diagnosis-open-settings",
    )]
}

/// The Java line of a diagnosis: the runtime, how it was chosen, and the
/// major the host requires.
fn java_check(
    runtime: &Runtime,
    origin: &str,
    state: &str,
    state_label: &str,
) -> DiagnosticCheckRow {
    check(
        &lookup("gui-diagnosis-check-java"),
        lookup_args(
            "gui-diagnosis-check-java-runtime",
            [
                ("runtime", Arg::from(runtime.line())),
                ("origin", Arg::from(lookup(origin))),
                ("major", Arg::from(runtime.major.to_string())),
            ],
        ),
        state,
        state_label,
    )
}

fn client_checks() -> Vec<DiagnosticCheckRow> {
    let diagnostics = recorded::diagnostics();
    let files = &diagnostics.files;
    let mods = &diagnostics.mods;
    vec![
        java_check(
            &JAVA_17,
            "gui-java-origin-host-managed",
            "customized",
            "gui-diagnosis-check-customized",
        ),
        check(
            &lookup("gui-diagnosis-check-mods"),
            lookup_args(
                "gui-diagnosis-check-mods-value",
                [
                    ("enabled", Arg::from(mods.enabled)),
                    ("disabled", Arg::from(mods.disabled)),
                    ("answered", Arg::from(mods.answered)),
                    ("unanswered", Arg::from(mods.unanswered)),
                ],
            ),
            "ok",
            "gui-diagnosis-check-ok",
        ),
        check(
            &lookup("gui-diagnosis-check-log"),
            lookup("gui-diagnosis-check-log-none"),
            "skipped",
            "gui-diagnosis-check-skipped",
        ),
        check(
            &lookup("gui-diagnosis-check-crash"),
            lookup("gui-diagnosis-check-crash-none"),
            "ok",
            "gui-diagnosis-check-ok",
        ),
        check(
            &lookup("gui-diagnosis-check-files"),
            lookup_args(
                "gui-diagnosis-check-files-value",
                [
                    ("count", Arg::from(files.total)),
                    ("intact", Arg::from(files.intact)),
                    ("problems", Arg::from(files.problems.len())),
                    ("unverifiable", Arg::from(files.unverifiable)),
                ],
            ),
            "problem",
            "gui-diagnosis-check-problem",
        ),
    ]
}

/// The CaveStone client after a diagnosis that found problems.
fn diagnosed_client(ui: &AppWindow) -> HostView {
    let problems = client_problems();
    let customized = client_customizations();
    HostView {
        diagnosis_state: "result".into(),
        diagnosis_summary: lookup_args(
            "gui-diagnosis-problems",
            [("count", Arg::from(problems.len()))],
        )
        .into(),
        diagnosis_detail: lookup_args(
            "gui-diagnosis-summary",
            [
                ("time", Arg::from("14:32")),
                ("count", Arg::from(customized.len())),
            ],
        )
        .into(),
        diagnosis_problems: rows(problems),
        diagnosis_customized: rows(customized),
        diagnosis_checks: rows(client_checks()),
        ..client_view(cavestone(ui))
    }
}

/// CaveStone's Diagnostics surface after a diagnosis with problems.
pub(crate) fn client_diagnostics_result(ui: &AppWindow) {
    let view = diagnosed_client(ui);
    open_client(ui, view, "Diagnostics");
}

/// The same surface with the fix for damaged files open for review.
pub(crate) fn client_diagnostics_fix_preview(ui: &AppWindow) {
    let problems = &recorded::diagnostics().files.problems;
    let view = HostView {
        fix_state: "ready".into(),
        fix_finding: "p0".into(),
        fix_title: lookup_args(
            "gui-diagnosis-files-damaged",
            [("count", Arg::from(problems.len()))],
        )
        .into(),
        fix_changes: rows(
            problems
                .iter()
                .map(|problem| FixChangeRow {
                    action: lookup("gui-fix-restore").into(),
                    name: problem.path.clone().into(),
                    detail: SharedString::default(),
                })
                .collect(),
        ),
        fix_notes: strings(&[&lookup("gui-fix-restore-note")]),
        ..diagnosed_client(ui)
    };
    open_client(ui, view, "Diagnostics");
}

/// A diagnosis in progress, with the stage it reports.
pub(crate) fn diagnostics_working(ui: &AppWindow) {
    let view = HostView {
        diagnosis_state: "working".into(),
        diagnosis_progress: lookup("operation-stage-checking-mods").into(),
        ..client_view(cavestone(ui))
    };
    open_client(ui, view, "Diagnostics");
}

/// The notice above a client's other surfaces after its game crashed.
pub(crate) fn client_run_failure_notice(ui: &AppWindow) {
    let view = HostView {
        mods: rows(client_mod_rows()),
        allow_pack_mod_changes: true,
        browser_provider: "modrinth".into(),
        browser_submitted: recorded::data().mod_search.query.clone().into(),
        browser_ran: true,
        browser_results: rows(client_mod_results()),
        browser_next_offset: -1,
        run_failure_title: lookup("gui-run-crashed").into(),
        run_failure: lookup("gui-run-exited-with-error").into(),
        run_likely_cause: lookup_args(
            "gui-run-likely-cause",
            [(
                "cause",
                Arg::from(lookup_args(
                    "gui-diagnosis-files-damaged",
                    [(
                        "count",
                        Arg::from(recorded::diagnostics().files.problems.len()),
                    )],
                )),
            )],
        )
        .into(),
        run_crash_report: recorded::diagnostics().client_crash.report.clone().into(),
        ..client_view(cavestone(ui))
    };
    open_client(ui, view, "Mods");
}

fn log_chunks(text: &str) -> ModelRc<LogChunk> {
    rows(
        text.lines()
            .map(|line| LogChunk {
                text: line.into(),
                level: log_level(line).into(),
            })
            .collect(),
    )
}

/// A live view that follows its recorded log with every level shown.
fn live_log(text: &str) -> LogView {
    let lines = text.lines().collect::<Vec<_>>();
    let columns = lines
        .iter()
        .map(|text| text.chars().count())
        .max()
        .unwrap_or(0)
        .max(80);
    LogView {
        mode: "live".into(),
        chunks: log_chunks(text),
        has_content: !lines.is_empty(),
        status: lookup_args(
            "gui-retained-log-records",
            [("count", Arg::from(lines.len()))],
        )
        .into(),
        content_columns: i32::try_from(columns).unwrap_or(80),
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
    }
}

/// The Console surface of a running server.
pub(crate) fn server_console(ui: &AppWindow) {
    let server = running_server(named_server(ui, "photon-server"));
    open_server(
        ui,
        server_view(server),
        "Console",
        Some(live_log(recorded::PHOTON_SERVER_LOG)),
    );
}

/// The Properties surface of a stopped server with LAN access awaiting EULA consent.
pub(crate) fn server_properties(ui: &AppWindow) {
    let view = HostView {
        property_motd: "CaveStone reproduction server".into(),
        property_max_players: "8".into(),
        property_difficulty: "normal".into(),
        property_gamemode: "survival".into(),
        property_allow_flight: false,
        property_pvp: true,
        property_allowlist: false,
        property_online_mode: false,
        property_access: "LAN".into(),
        property_address: SharedString::default(),
        property_port: "25565".into(),
        properties_revision: 1,
        ..server_view(named_server(ui, "cavestone-server"))
    };
    ui.set_eula_accepted(false);
    ui.set_eula_status(lookup("gui-eula-not-accepted").into());
    open_server(ui, view, "Properties", None);
}

fn servers_page(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(1);
    ui.set_modal_error(SharedString::default());
}

fn release_versions(ui: &AppWindow) {
    ui.set_minecraft_releases(rows(
        [
            "26.2", "1.21.1", "1.20.1", "1.19.4", "1.18.2", "1.16.5", "1.12.2",
        ]
        .map(SharedString::from)
        .to_vec(),
    ));
}

/// The New Server dialog with its own Minecraft release and loader build to choose.
pub(crate) fn create_server_standalone(ui: &AppWindow) {
    servers_page(ui);
    release_versions(ui);
    ui.set_create_server_client_id(SharedString::default());
    ui.set_create_server_name(SharedString::default());
    ui.set_create_server_name_suggestion("Minecraft 1.21.1 NeoForge Server".into());
    ui.set_create_server_minecraft("1.21.1".into());
    ui.set_create_server_loader("NeoForge".into());
    ui.set_create_server_loader_versions(strings(&["21.1.249", "21.1.248", "21.1.246"]));
    ui.set_create_server_loader_version("21.1.249".into());
    ui.set_create_server_persistent(false);
    ui.set_create_server_automatic_port(true);
    ui.set_create_server_pack(SharedString::default());
    ui.set_create_server_pack_name(SharedString::default());
    ui.set_modal("create-server".into());
}

/// The New Linked Server dialog, which takes its version and loader from the client.
pub(crate) fn create_server_linked(ui: &AppWindow) {
    ui.set_workspace_open(false);
    ui.set_page(0);
    ui.set_modal_error(SharedString::default());
    release_versions(ui);
    ui.set_create_server_client_id("cavestone".into());
    ui.set_create_server_name(SharedString::default());
    ui.set_create_server_name_suggestion("CaveStone Server".into());
    ui.set_create_server_persistent(false);
    ui.set_create_server_automatic_port(true);
    ui.set_create_server_pack(SharedString::default());
    ui.set_create_server_pack_name(SharedString::default());
    ui.set_modal("create-server".into());
}

/// The EULA review a server needs before its first start.
pub(crate) fn server_eula_review(ui: &AppWindow) {
    let view = HostView {
        property_motd: "CaveStone reproduction server".into(),
        property_max_players: "8".into(),
        property_difficulty: "normal".into(),
        property_gamemode: "survival".into(),
        property_pvp: true,
        property_access: "LAN".into(),
        property_port: "25565".into(),
        properties_revision: 1,
        ..server_view(named_server(ui, "cavestone-server"))
    };
    ui.set_eula_accepted(false);
    ui.set_eula_status(lookup("gui-eula-not-accepted").into());
    open_server(ui, view, "Properties", None);
    ui.set_modal("eula".into());
}

/// A server whose recorded processes have exited, which blocks Start until it is repaired.
pub(crate) fn server_needs_recovery(ui: &AppWindow) {
    let server = ServerRow {
        state: lookup("gui-state-needs-recovery").into(),
        state_kind: "needs-recovery".into(),
        state_detail: lookup("gui-state-needs-recovery-detail").into(),
        can_start: false,
        ..named_server(ui, "cavestone-server")
    };
    open_server(ui, server_view(server), "Overview", None);
}

/// The CaveStone Server's Mods surface with `search` from `provider` beside its
/// list. Its pack mods are shown changeable, as independent mod changes allow.
fn cavestone_server_mods(
    server: ServerRow,
    provider: &str,
    search: &Search,
    include_incompatible: bool,
) -> HostView {
    HostView {
        mods: rows(cavestone_server_mod_rows()),
        browser_provider: provider.into(),
        browser_submitted: search.query.clone().into(),
        browser_ran: true,
        browser_results: rows(server_mod_results(search)),
        browser_next_offset: -1,
        browser_include_incompatible: include_incompatible,
        allow_pack_mod_changes: true,
        mods_are_the_users: true,
        ..server_view(server)
    }
}

/// The Settings of a server that sets its own memory, with independent mod
/// changes on as its Mods surface shows them.
pub(crate) fn server_settings(ui: &AppWindow) {
    let view = HostView {
        runtime_memory_min: "2048".into(),
        runtime_memory_max: "4096".into(),
        runtime_pinned: true,
        runtime_global: super::client_hosts::global_runtime(),
        runtime_revision: 1,
        allow_pack_mod_changes: true,
        ..server_view(named_server(ui, "cavestone-server"))
    };
    open_server(ui, view, "Settings", None);
}

/// Manage Mod for a pack mod of the server, opened from its Mods list.
pub(crate) fn server_mod_manager(ui: &AppWindow) {
    let server = named_server(ui, "cavestone-server");
    let search = &recorded::server_mods().modrinth_search;
    let view = cavestone_server_mods(server, "modrinth", search, false);
    open_server(ui, view, "Mods", None);
    super::mods::manage_dialog(
        ui,
        ("server", "cavestone-server"),
        &recorded::dialogs().manage,
        true,
    );
}

/// A server's Modrinth search, where one result does not state server support.
pub(crate) fn server_mods(ui: &AppWindow) {
    let server = named_server(ui, "cavestone-server");
    let search = &recorded::server_mods().modrinth_search;
    let view = cavestone_server_mods(server, "modrinth", search, false);
    open_server(ui, view, "Mods", None);
}

/// A server's CurseForge search that also lists the mods a server cannot take.
pub(crate) fn server_mods_curseforge(ui: &AppWindow) {
    let server = named_server(ui, "cavestone-server");
    let search = &recorded::server_mods().curseforge_search;
    let view = cavestone_server_mods(server, "curseforge", search, true);
    open_server(ui, view, "Mods", None);
}

/// The confirmation a running server's install opens for a mod that does not
/// state server support.
pub(crate) fn server_mod_install_confirm(ui: &AppWindow) {
    let server = running_server(named_server(ui, "cavestone-server"));
    let recorded = recorded::server_mods();
    let view = cavestone_server_mods(server, "modrinth", &recorded.modrinth_search, false);
    open_server(ui, view, "Mods", None);
    let server_plan = &recorded.plan;
    let plan = &server_plan.plan;
    let installs = 1 + plan.dependencies.saturating_sub(plan.existing);
    let count = |value: usize| i32::try_from(value).unwrap_or_default();
    ui.set_mod_plan_instance("cavestone-server".into());
    ui.set_mod_plan_kind("server".into());
    ui.set_mod_plan_provider("modrinth".into());
    ui.set_mod_plan_project(plan.project.clone().into());
    ui.set_mod_plan_version_id(plan.version_id.clone().into());
    ui.set_mod_plan_title(plan.title.clone().into());
    ui.set_mod_plan_version(plan.version.clone().into());
    ui.set_mod_plan_install_count(count(installs));
    ui.set_mod_plan_dependency_count(count(plan.dependencies));
    ui.set_mod_plan_existing_count(count(plan.existing));
    ui.set_mod_plan_optional_count(count(plan.optional));
    ui.set_mod_plan_warning_count(count(plan.warnings));
    let unstated = server_plan
        .unstated
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    ui.set_mod_plan_unstated(strings(&unstated));
    ui.set_mod_plan_unstated_ids(strings(&[plan.project.as_str()]));
    ui.set_mod_plan_client_only(strings(&[]));
    ui.set_mod_plan_restart_note(lookup("gui-server-mod-plan-restart").into());
    ui.set_mod_action_pending(false);
    ui.set_linked_targets(rows(Vec::new()));
    ui.set_modal_error(SharedString::default());
    ui.set_modal("mod-plan".into());
}

/// What the Utility Mods Server's failed start shows: the loader's refusal, the
/// crash it wrote, and the missing dependency the catalog can install, in the
/// order the launcher lists them.
fn utility_mods_problems() -> Vec<DiagnosticFindingRow> {
    let missing = &recorded::server_mods().missing_dependency;
    let crash = &recorded::diagnostics().server_crashes.utility_mods;
    let suspects = crash
        .suspects
        .iter()
        .map(|suspect| suspect.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let loader = with_route(
        finding(
            "p0",
            true,
            &lookup_args(
                "gui-diagnosis-loader-missing",
                [("dependency", Arg::from(missing.loader_id.as_str()))],
            ),
            &[
                lookup_args(
                    "gui-diagnosis-loader-range",
                    [("range", Arg::from(missing.loader_range.as_str()))],
                ),
                lookup_args(
                    "gui-diagnosis-loader-asked-by",
                    [("mods", Arg::from(suspects.as_str()))],
                ),
                lookup("gui-diagnosis-loader-detail"),
            ]
            .join("\n"),
            &[],
            &[],
        ),
        "log",
        "gui-diagnosis-open-log",
    );
    let crashed = with_route(
        finding(
            "p1",
            true,
            &lookup("gui-diagnosis-crash"),
            &[
                crash.failure.clone(),
                lookup_args(
                    "gui-diagnosis-crash-names",
                    [("names", Arg::from(suspects.as_str()))],
                ),
            ]
            .join("\n"),
            &[],
            &[],
        ),
        "crash-report",
        "gui-diagnosis-open-crash-report",
    );
    let dependency = DiagnosticFindingRow {
        install_label: lookup("gui-diagnosis-install-latest").into(),
        install_provider: "modrinth".into(),
        install_project: missing.project.clone().into(),
        ..with_route(
            finding(
                "p2",
                true,
                &lookup_args(
                    "gui-diagnosis-dependency-missing",
                    [("name", Arg::from(missing.name.as_str()))],
                ),
                &[
                    lookup_args(
                        "gui-diagnosis-required-by",
                        [("name", Arg::from(missing.required_by.as_str()))],
                    ),
                    lookup("gui-diagnosis-dependency-install-detail"),
                ]
                .join("\n"),
                &[missing.required_by.as_str(), missing.name.as_str()],
                &[],
            ),
            "mods",
            "gui-diagnosis-open-mods",
        )
    };
    vec![loader, crashed, dependency]
}

fn utility_mods_checks() -> Vec<DiagnosticCheckRow> {
    let crash = &recorded::diagnostics().server_crashes.utility_mods;
    vec![
        java_check(
            &JAVA_21,
            "gui-java-origin-automatic",
            "ok",
            "gui-diagnosis-check-ok",
        ),
        check(
            &lookup("gui-diagnosis-check-mods"),
            lookup_args(
                "gui-diagnosis-check-mods-value",
                [
                    ("enabled", Arg::from(7)),
                    ("disabled", Arg::from(0)),
                    ("answered", Arg::from(7)),
                    ("unanswered", Arg::from(0)),
                ],
            ),
            "problem",
            "gui-diagnosis-check-problem",
        ),
        check(
            &lookup("gui-diagnosis-check-log"),
            lookup("gui-diagnosis-check-log-refused"),
            "problem",
            "gui-diagnosis-check-problem",
        ),
        check(
            &lookup("gui-diagnosis-check-crash"),
            lookup_args(
                "gui-diagnosis-check-crash-found",
                [("report", Arg::from(crash.report.as_str()))],
            ),
            "problem",
            "gui-diagnosis-check-problem",
        ),
        check(
            &lookup("gui-diagnosis-check-files"),
            lookup("gui-diagnosis-check-files-skipped"),
            "skipped",
            "gui-diagnosis-check-skipped",
        ),
    ]
}

/// The quick diagnosis of the Utility Mods Server after its failed start.
pub(crate) fn server_diagnostics_install(ui: &AppWindow) {
    let missing = &recorded::server_mods().missing_dependency;
    let crash = &recorded::diagnostics().server_crashes.utility_mods;
    let problems = utility_mods_problems();
    let view = HostView {
        diagnosis_state: "result".into(),
        diagnosis_summary: lookup_args(
            "gui-diagnosis-problems",
            [("count", Arg::from(problems.len()))],
        )
        .into(),
        diagnosis_detail: lookup_args(
            "gui-diagnosis-summary-quick",
            [("time", Arg::from("19:18"))],
        )
        .into(),
        diagnosis_failure: lookup("gui-run-exited-with-error").into(),
        diagnosis_crash_report: crash.report.clone().into(),
        diagnosis_problems: rows(problems),
        diagnosis_checks: rows(utility_mods_checks()),
        ..server_view(named_server(ui, &missing.server))
    };
    open_server(ui, view, "Diagnostics", None);
}

fn bundle_part(name: &str, kind: &str, size: &str, selected: bool) -> BundlePartRow {
    BundlePartRow {
        name: name.into(),
        kind: lookup(kind).into(),
        size: size.into(),
        selected,
    }
}

/// The bundle panel opened on a client with a crash report. The parts are what
/// the launcher offers for the instance; the large debug log stays unchecked.
pub(crate) fn diagnostic_bundle_selection(ui: &AppWindow) {
    let offered = &recorded::diagnostics().bundle;
    let selected = |part: &recorded::BundlePart| !part.name.ends_with("debug.log");
    let parts = offered
        .iter()
        .map(|part| {
            let kind = match part.kind.as_str() {
                "game_log" => "gui-bundle-kind-game-log",
                "crash_report" => "gui-bundle-kind-crash-report",
                "recipe" => "gui-bundle-kind-recipe",
                _ => "gui-bundle-kind-record",
            };
            let name = part.name.rsplit('/').next().unwrap_or(&part.name);
            bundle_part(name, kind, &size_text(part.size_bytes), selected(part))
        })
        .collect::<Vec<_>>();
    let chosen = offered.iter().filter(|part| selected(part));
    let count = chosen.clone().count();
    let total = chosen.map(|part| part.size_bytes).sum::<u64>();
    let view = HostView {
        bundle_selection: lookup_args(
            "gui-bundle-selection",
            [
                ("count", Arg::from(count)),
                ("size", Arg::from(size_text(total))),
            ],
        )
        .into(),
        bundle_selected_count: i32::try_from(count).unwrap_or_default(),
        bundle_parts: rows(parts),
        ..client_view(cavestone(ui))
    };
    open_client(ui, view, "Diagnostics");
}

/// The Overview of a server that failed to start, with its crash review.
pub(crate) fn server_crash_notice(ui: &AppWindow) {
    let crash = &recorded::diagnostics().server_crashes.fog;
    let view = HostView {
        crash_report: crash.report.clone().into(),
        crash_failure: crash.failure.clone().into(),
        crash_client_code_on_server: crash.client_code_on_server,
        crash_suggested: i32::try_from(crash.suggested).unwrap_or_default(),
        ..server_view(named_server(ui, "fog-server"))
    };
    open_server(ui, view, "Overview", None);
}

/// The `Description:` line of a crash report, which the crash list shows as its summary.
fn crash_description(report: &str) -> &str {
    report
        .lines()
        .find_map(|line| line.strip_prefix("Description: "))
        .unwrap_or_default()
}

/// The Logs surface of a server that stopped while loading mods, with its
/// crash report open.
pub(crate) fn server_crash_reports(ui: &AppWindow) {
    let crash = &recorded::diagnostics().server_crashes.utility_mods;
    let server = named_server(ui, "utility-mods-server");
    let summary = crash_description(recorded::UTILITY_SERVER_CRASH);
    let file = DiagnosticRow {
        scope_kind: "server".into(),
        scope_id: server.id.clone(),
        entity: server.name.clone(),
        kind: "crash-report".into(),
        name: crash.report.clone().into(),
        path: format!(
            "/demo/servers/{}/server/crash-reports/{}",
            server.id, crash.report
        )
        .into(),
        modified: "Today".into(),
        size: size_text(recorded::UTILITY_SERVER_CRASH.len() as u64).into(),
        summary: summary.into(),
    };
    let log = LogView {
        mode: "crashes".into(),
        file: crash.report.clone().into(),
        files: rows(vec![file]),
        report: expand_tabs(recorded::UTILITY_SERVER_CRASH).into(),
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
    };
    open_server(ui, server_view(server), "Logs", Some(log));
}

/// Two panes: the client's live log beside its linked server's console.
pub(crate) fn split_workspace(ui: &AppWindow) {
    let pair_state = lookup("gui-state-running");
    let server = ServerRow {
        relationship: "Linked".into(),
        linked_client_id: "cavestone".into(),
        paired_client_id: "cavestone".into(),
        paired_client_name: "CaveStone".into(),
        ..running_server(named_server(ui, "cavestone-server"))
    };
    let client = InstanceRow {
        linked_servers: 1,
        paired_server_id: server.id.clone(),
        paired_server_name: server.name.clone(),
        ..running_instance(cavestone(ui))
    };
    let client_view = HostView {
        linked_servers: rows(vec![server.clone()]),
        pair_shown: true,
        pair_state: pair_state.clone().into(),
        pair_state_kind: "running".into(),
        ..client_view(client)
    };
    let server_view = HostView {
        pair_shown: true,
        pair_state: pair_state.into(),
        pair_state_kind: "running".into(),
        ..server_view(server)
    };
    ui.set_log_views(rows(vec![
        live_log(recorded::CLIENT_LOG),
        live_log(recorded::CAVESTONE_SERVER_LOG),
    ]));
    ui.set_host_views(rows(vec![client_view, server_view]));
    ui.set_workspace_panes(rows(vec![
        WorkspacePaneRow {
            log: 0,
            column: 0,
            row: 0,
            stacked: false,
            can_split_beside: false,
            can_split_below: true,
            can_close: true,
            ..pane(1, 0, "Logs", "Left, CaveStone, Logs", false)
        },
        WorkspacePaneRow {
            log: 1,
            column: 1,
            row: 0,
            stacked: false,
            can_split_beside: false,
            can_split_below: true,
            can_close: true,
            ..pane(2, 1, "Console", "Right, CaveStone Server, Console", true)
        },
    ]));
    ui.set_workspace_back_label(lookup("gui-instances").into());
    ui.set_page(0);
    ui.set_workspace_open(true);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use crate::capture::{CaptureOptions, Dimensions, ThemeName, generate};
    use crate::catalog::STATES;

    #[test]
    fn the_recorded_server_crash_carries_a_description() {
        assert!(
            !super::crash_description(crate::fixtures::recorded::UTILITY_SERVER_CRASH).is_empty()
        );
    }

    const NAMES: &[&str] = &[
        "client-diagnostics-result",
        "client-diagnostics-fix-preview",
        "diagnostics-working",
        "client-run-failure-notice",
        "server-console",
        "server-properties",
        "server-crash-notice",
        "server-crash-reports",
        "split-workspace",
        "create-server-standalone",
        "create-server-linked",
        "server-eula-review",
        "server-needs-recovery",
        "diagnostic-bundle-selection",
    ];

    #[test]
    fn every_diagnostics_and_server_state_renders() {
        let _session = crate::capture::render_session();
        let output = tempfile::tempdir().expect("temporary output");
        let states = NAMES
            .iter()
            .map(|name| {
                *STATES
                    .iter()
                    .find(|state| state.name == *name)
                    .expect("state is cataloged")
            })
            .collect::<Vec<_>>();
        let files = generate(&CaptureOptions {
            states: &states,
            themes: &[ThemeName::Light],
            sizes: &[Dimensions::HD],
            output: output.path(),
            derive_1080: false,
        })
        .expect("render the states");
        assert_eq!(
            files.len(),
            states
                .iter()
                .map(|state| state.regions.len())
                .sum::<usize>()
        );
        assert!(files.iter().all(|path| path.is_file()));
    }
}
