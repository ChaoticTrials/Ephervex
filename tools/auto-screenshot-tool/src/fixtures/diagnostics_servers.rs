//! Diagnostics, failed-run notices, and server workspace surfaces.

use ephervex_gui::ui::{
    AppWindow, DiagnosticCheckRow, DiagnosticFindingRow, FixChangeRow, HostView, InstanceRow,
    LogChunk, LogView, ServerRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{Model, ModelRc, SharedString, VecModel};

use super::{client_mod_results, client_mod_rows, rows, running_instance, running_server};

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

fn client_problems() -> Vec<DiagnosticFindingRow> {
    vec![
        with_fix(finding(
            "p0",
            true,
            &lookup_args(
                "gui-diagnosis-files-damaged",
                [("count", Arg::from(2_usize))],
            ),
            &lookup_args(
                "gui-diagnosis-files-restorable",
                [("count", Arg::from(2_usize))],
            ),
            &[],
            &[
                "mods/jei-1.20.1-forge-15.20.0.130.jar (Missing)",
                "mods/jade-1.20.1-forge-11.13.2.jar (Damaged)",
            ],
        )),
        DiagnosticFindingRow {
            install_label: lookup("gui-diagnosis-install-latest").into(),
            install_provider: "modrinth".into(),
            install_project: "villager-notice-board".into(),
            ..with_route(
                finding(
                    "p1",
                    true,
                    &lookup_args(
                        "gui-diagnosis-dependency-missing",
                        [("name", Arg::from("Villager Notice Board"))],
                    ),
                    &lookup_args(
                        "gui-diagnosis-required-by",
                        [("name", Arg::from("Creeper Confetti"))],
                    ),
                    &[
                        "Redstone Rabbit Holes",
                        "Creeper Confetti",
                        "Villager Notice Board",
                    ],
                    &[],
                ),
                "mods",
                "gui-diagnosis-open-mods",
            )
        },
    ]
}

fn client_customizations() -> Vec<DiagnosticFindingRow> {
    vec![with_route(
        finding(
            "c0",
            false,
            &lookup_args(
                "gui-diagnosis-java-custom",
                [("version", Arg::from("17.0.12"))],
            ),
            &lookup_args(
                "gui-diagnosis-java-satisfies",
                [
                    ("minecraft", Arg::from("1.20.1")),
                    ("major", Arg::from("17")),
                ],
            ),
            &[],
            &[],
        ),
        "java-settings",
        "gui-diagnosis-open-java-settings",
    )]
}

fn client_checks() -> Vec<DiagnosticCheckRow> {
    vec![
        check(
            &lookup("gui-diagnosis-check-java"),
            lookup_args(
                "gui-diagnosis-check-java-pinned",
                [
                    ("version", Arg::from("17.0.12")),
                    ("major", Arg::from("17")),
                ],
            ),
            "customized",
            "gui-diagnosis-check-customized",
        ),
        check(
            &lookup("gui-diagnosis-check-mods"),
            lookup_args(
                "gui-diagnosis-check-mods-value",
                [
                    ("enabled", Arg::from(53_usize)),
                    ("disabled", Arg::from(1_usize)),
                    ("answered", Arg::from(54_usize)),
                    ("unanswered", Arg::from(0_usize)),
                ],
            ),
            "problem",
            "gui-diagnosis-check-problem",
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
                    ("count", Arg::from(54_usize)),
                    ("intact", Arg::from(52_usize)),
                    ("problems", Arg::from(2_usize)),
                    ("unverifiable", Arg::from(0_usize)),
                ],
            ),
            "problem",
            "gui-diagnosis-check-problem",
        ),
        check(
            &lookup("gui-integrity-shared-title"),
            lookup_args(
                "gui-integrity-shared-sound",
                [("count", Arg::from(212_usize))],
            ),
            "ok",
            "gui-diagnosis-check-ok",
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
    let view = HostView {
        fix_state: "ready".into(),
        fix_finding: "p0".into(),
        fix_title: lookup_args(
            "gui-diagnosis-files-damaged",
            [("count", Arg::from(2_usize))],
        )
        .into(),
        fix_changes: rows(vec![
            FixChangeRow {
                action: lookup("gui-fix-restore").into(),
                name: "mods/jei-1.20.1-forge-15.20.0.130.jar".into(),
                detail: SharedString::default(),
            },
            FixChangeRow {
                action: lookup("gui-fix-restore").into(),
                name: "mods/jade-1.20.1-forge-11.13.2.jar".into(),
                detail: SharedString::default(),
            },
        ]),
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
        browser_submitted: "redstone".into(),
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
                    "gui-diagnosis-dependency-missing",
                    [("name", Arg::from("Villager Notice Board"))],
                )),
            )],
        )
        .into(),
        run_crash_report: "crash-2026-10-07_21.14.32-client.txt".into(),
        ..client_view(cavestone(ui))
    };
    open_client(ui, view, "Mods");
}

fn log_chunks(lines: &[(&str, &str)]) -> ModelRc<LogChunk> {
    rows(
        lines
            .iter()
            .map(|(level, text)| LogChunk {
                text: (*text).into(),
                level: (*level).into(),
            })
            .collect(),
    )
}

/// A live view that follows its log with every level shown.
fn live_log(lines: &[(&str, &str)]) -> LogView {
    let columns = lines
        .iter()
        .map(|(_, text)| text.chars().count())
        .max()
        .unwrap_or(0)
        .max(80);
    LogView {
        mode: "live".into(),
        chunks: log_chunks(lines),
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

const TOTEM_CONSOLE: &[(&str, &str)] = &[
    (
        "info",
        "[21:04:10] [main/INFO] [cpw.mods.modlauncher.Launcher/MODLAUNCHER]: ModLauncher running: args [--launchTarget, neoforgeserver]",
    ),
    (
        "info",
        "[21:04:10] [main/INFO] [cpw.mods.modlauncher.Launcher/MODLAUNCHER]: JVM identified as OpenJDK 64-Bit Server VM 21.0.8",
    ),
    (
        "info",
        "[21:04:11] [main/INFO] [net.neoforged.fml.loading.moddiscovery.ModDiscoverer/SCAN]: Found 7 mod jars in /demo/servers/totem-of-undying-qa/mods",
    ),
    (
        "info",
        "[21:04:12] [main/INFO] [net.neoforged.fml.loading.FMLLoader/LOADING]: Loading NeoForge 21.1.249 for Minecraft 1.21.1",
    ),
    (
        "info",
        "[21:04:14] [Server thread/INFO] [net.minecraft.server.dedicated.DedicatedServer/]: Starting minecraft server version 1.21.1",
    ),
    (
        "info",
        "[21:04:14] [Server thread/INFO] [net.minecraft.server.dedicated.DedicatedServer/]: Loading properties",
    ),
    (
        "info",
        "[21:04:14] [Server thread/INFO] [net.minecraft.server.dedicated.DedicatedServer/]: Default game type: SURVIVAL",
    ),
    (
        "info",
        "[21:04:14] [Server thread/INFO] [net.minecraft.server.dedicated.DedicatedServer/]: Starting Minecraft server on 127.0.0.1:25566",
    ),
    (
        "warn",
        "[21:04:15] [Server thread/WARN] [net.minecraft.server.dedicated.DedicatedServer/]: **** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!",
    ),
    (
        "info",
        "[21:04:16] [Server thread/INFO] [net.minecraft.server.level.ServerLevel/]: Preparing level \"world\"",
    ),
    (
        "info",
        "[21:04:17] [Server thread/INFO] [totemqa/]: Totem of Undying QA harness ready, 3 scenarios loaded",
    ),
    (
        "info",
        "[21:04:18] [Server thread/INFO] [net.minecraft.server.dedicated.DedicatedServer/]: Done (4.2s)! For help, type \"help\"",
    ),
    (
        "info",
        "[21:06:41] [Server thread/INFO] [net.minecraft.server.players.PlayerList/]: MelanX[/127.0.0.1:51734] logged in with entity id 214 at (12.5, 64.0, -30.5)",
    ),
    (
        "info",
        "[21:06:41] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: MelanX joined the game",
    ),
    (
        "info",
        "[21:07:03] [Server thread/INFO] [net.minecraft.server.players.PlayerList/]: Steve[/127.0.0.1:51802] logged in with entity id 231 at (14.5, 64.0, -28.5)",
    ),
    (
        "info",
        "[21:07:03] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: Steve joined the game",
    ),
    (
        "info",
        "[21:07:19] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: <MelanX> testing the totem again",
    ),
    (
        "info",
        "[21:07:44] [Server thread/INFO] [totemqa/]: Scenario 1 passed: totem consumed on lethal damage",
    ),
    (
        "warn",
        "[21:08:02] [Server thread/WARN] [net.minecraft.server.MinecraftServer/]: Can't keep up! Is the server overloaded? Running 2312ms or 46 ticks behind",
    ),
    (
        "info",
        "[21:08:31] [Server thread/INFO] [totemqa/]: Scenario 2 passed: totem popped in the Nether",
    ),
    (
        "info",
        "[21:08:55] [Server thread/INFO] [totemqa/]: Scenario 3 passed: only one totem is consumed per death",
    ),
    (
        "info",
        "[21:09:12] [Server thread/INFO] [net.minecraft.server.network.ServerGamePacketListenerImpl/]: Steve lost connection: Disconnected",
    ),
    (
        "info",
        "[21:09:12] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: Steve left the game",
    ),
    (
        "info",
        "[21:09:40] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: <MelanX> that was the last scenario",
    ),
    (
        "info",
        "[21:10:05] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: There are 1 of a max of 20 players online: MelanX",
    ),
];

/// The Console surface of a running server.
pub(crate) fn server_console(ui: &AppWindow) {
    let server = running_server(named_server(ui, "totem-of-undying-qa"));
    open_server(
        ui,
        server_view(server),
        "Console",
        Some(live_log(TOTEM_CONSOLE)),
    );
}

/// The Properties surface of a stopped server with LAN access and its EULA accepted.
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
    ui.set_eula_accepted(true);
    ui.set_eula_status(
        lookup_args(
            "gui-eula-accepted-at",
            [("value", Arg::from("2026-10-02 18:20 UTC"))],
        )
        .into(),
    );
    open_server(ui, view, "Properties", None);
}

/// The Overview of a server that failed to start, with its crash review.
pub(crate) fn server_crash_notice(ui: &AppWindow) {
    let view = HostView {
        crash_report: "crash-2026-10-08_09.41.17-server.txt".into(),
        crash_failure: "java.lang.NoClassDefFoundError: net/minecraft/client/gui/screens/Screen"
            .into(),
        crash_client_code_on_server: true,
        crash_suggested: 1,
        ..server_view(named_server(ui, "cow-clicker-test"))
    };
    open_server(ui, view, "Overview", None);
}

const CAVESTONE_CLIENT_LOG: &[(&str, &str)] = &[
    (
        "info",
        "[21:12:03] [main/INFO] [FMLLoader/]: Forge mod loading, version 47.4.10, for MC 1.20.1",
    ),
    (
        "info",
        "[21:12:04] [main/INFO] [ModDiscoverer/SCAN]: Found 54 mod jars in /demo/instances/cavestone/mods",
    ),
    (
        "info",
        "[21:12:06] [main/INFO] [ModDiscoverer/SCAN]: Found 3 dependency jars",
    ),
    (
        "info",
        "[21:12:08] [main/INFO] [Minecraft/]: Setting user: MelanX",
    ),
    (
        "info",
        "[21:12:09] [Render thread/INFO] [Minecraft/]: Backend library: LWJGL version 3.3.1 build 7",
    ),
    (
        "info",
        "[21:12:11] [Render thread/INFO] [mixin/]: Initialized 38 mixin configs",
    ),
    (
        "info",
        "[21:12:14] [modloading-worker-0/INFO] [create/]: Create 0.5.1.j is starting up",
    ),
    (
        "info",
        "[21:12:15] [modloading-worker-1/INFO] [jei/]: Starting JEI...",
    ),
    (
        "info",
        "[21:12:17] [modloading-worker-2/INFO] [jade/]: Jade 11.13.2 is loading",
    ),
    (
        "info",
        "[21:12:19] [Render thread/INFO] [Minecraft/]: Reloading ResourceManager: vanilla, mod_resources",
    ),
    (
        "warn",
        "[21:12:21] [Worker-Main-3/WARN] [Minecraft/]: Missing sound for event: creeperconfetti:pop",
    ),
    (
        "info",
        "[21:12:24] [Render thread/INFO] [Minecraft/]: OpenAL initialized on device default",
    ),
    (
        "info",
        "[21:12:26] [Render thread/INFO] [Minecraft/]: Sound engine started",
    ),
    (
        "info",
        "[21:12:30] [Render thread/INFO] [Minecraft/]: Narrator library for x86-64 successfully loaded",
    ),
    (
        "info",
        "[21:12:33] [Render thread/INFO] [jei/]: Registering recipe catalysts",
    ),
    (
        "info",
        "[21:12:41] [Render thread/INFO] [Minecraft/]: Connecting to 127.0.0.1, 25565",
    ),
    (
        "info",
        "[21:12:42] [Render thread/INFO] [Minecraft/]: [CHAT] MelanX joined the game",
    ),
    (
        "info",
        "[21:13:05] [Render thread/INFO] [Minecraft/]: [CHAT] Steve joined the game",
    ),
    (
        "info",
        "[21:13:27] [Render thread/INFO] [Minecraft/]: [CHAT] <MelanX> chunks look correct here",
    ),
    (
        "info",
        "[21:13:52] [Render thread/INFO] [Minecraft/]: [CHAT] <Steve> the pearls land in the right chunk",
    ),
];

const CAVESTONE_SERVER_CONSOLE: &[(&str, &str)] = &[
    (
        "info",
        "[21:11:46] [main/INFO] [ModLauncher/MODLAUNCHER]: ModLauncher running: args [--launchTarget, forgeserver]",
    ),
    (
        "info",
        "[21:11:47] [main/INFO] [FMLLoader/]: Forge mod loading, version 47.4.10, for MC 1.20.1",
    ),
    (
        "info",
        "[21:11:49] [main/INFO] [ModDiscoverer/SCAN]: Found 32 mod jars in /demo/servers/cavestone-server/mods",
    ),
    (
        "info",
        "[21:11:53] [Server thread/INFO] [DedicatedServer/]: Starting minecraft server version 1.20.1",
    ),
    (
        "info",
        "[21:11:53] [Server thread/INFO] [DedicatedServer/]: Loading properties",
    ),
    (
        "info",
        "[21:11:54] [Server thread/INFO] [DedicatedServer/]: Default game type: SURVIVAL",
    ),
    (
        "info",
        "[21:11:54] [Server thread/INFO] [DedicatedServer/]: Starting Minecraft server on 127.0.0.1:25565",
    ),
    (
        "warn",
        "[21:11:55] [Server thread/WARN] [DedicatedServer/]: **** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!",
    ),
    (
        "info",
        "[21:11:57] [Server thread/INFO] [ServerLevel/]: Preparing level \"world\"",
    ),
    (
        "info",
        "[21:11:59] [Server thread/INFO] [DedicatedServer/]: Done (3.8s)! For help, type \"help\"",
    ),
    (
        "info",
        "[21:12:42] [Server thread/INFO] [MinecraftServer/]: MelanX joined the game",
    ),
    (
        "info",
        "[21:13:05] [Server thread/INFO] [MinecraftServer/]: Steve joined the game",
    ),
    (
        "warn",
        "[21:13:19] [Server thread/WARN] [MinecraftServer/]: Can't keep up! Running 2012ms or 40 ticks behind",
    ),
    (
        "info",
        "[21:13:27] [Server thread/INFO] [MinecraftServer/]: <MelanX> chunks look correct here",
    ),
    (
        "info",
        "[21:13:52] [Server thread/INFO] [MinecraftServer/]: <Steve> the pearls land in the right chunk",
    ),
    (
        "info",
        "[21:14:20] [Server thread/INFO] [MinecraftServer/]: There are 2 of a max of 8 players online: MelanX, Steve",
    ),
];

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
        live_log(CAVESTONE_CLIENT_LOG),
        live_log(CAVESTONE_SERVER_CONSOLE),
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

    const NAMES: &[&str] = &[
        "client-diagnostics-result",
        "client-diagnostics-fix-preview",
        "diagnostics-working",
        "client-run-failure-notice",
        "server-console",
        "server-properties",
        "server-crash-notice",
        "split-workspace",
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
        assert_eq!(files.len(), NAMES.len());
        assert!(files.iter().all(|path| path.is_file()));
    }
}
