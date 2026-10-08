//! The CaveStone client workspace on its overview, settings, snapshot, and log surfaces.

use ephervex_gui::ui::{
    AppWindow, DiagnosticRow, HostView, InstanceRow, LogChunk, LogView, PendingEntityRow,
    ServerRow, SnapshotRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{ComponentHandle, Model, SharedString};

use super::{FORGE_1_20, rows, running_server, server};

/// The states this module adds to the catalog.
#[cfg(test)]
const STATE_NAMES: [&str; 6] = [
    "client-overview-linked-server",
    "client-settings",
    "snapshots",
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
    let client = ui.get_instances().row_data(0).unwrap_or_default();
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
        label: format!("CaveStone, {}", lookup(surface.label())).into(),
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
        linked_client_id: "cavestone".into(),
        relationship: "Linked".into(),
        can_connect: true,
        ..running_server(server(
            "cavestone-server",
            "CaveStone Server",
            "1.20.1",
            &FORGE_1_20,
            25565,
        ))
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

/// The runtime form of a client that pins its own Java, memory, and arguments.
pub(crate) fn client_settings(ui: &AppWindow) {
    open_surface(
        ui,
        Surface::Settings,
        |view| {
            view.runtime_java = "/demo/java/temurin-17/bin/java".into();
            view.runtime_memory_min = "2048".into();
            view.runtime_memory_max = "6144".into();
            view.runtime_jvm_args = "-XX:+UseG1GC -XX:MaxGCPauseMillis=100".into();
            view.runtime_pinned = true;
            view.runtime_global = lookup_args(
                "gui-instance-runtime-global",
                [
                    ("first", Arg::from("1024 - 4096 MiB")),
                    ("second", Arg::from(lookup("gui-java-auto-selected"))),
                ],
            )
            .into();
            view.runtime_revision = 1;
            view.allow_pack_mod_changes = true;
        },
        None,
    );
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
    vec![
        snapshot(
            "0192f3c8-5d4a-7b21-9e63-4a7c1d2b8f05",
            "Before the Wither",
            "12 min ago",
            "2.3 GiB",
            (54, 0),
            "gui-snapshot-origin-manual",
            false,
        ),
        snapshot(
            "0192f3a1-0c77-7e94-8b12-d3f5a60e4c18",
            "Pre-Ender-Dragon",
            "3 h ago",
            "2.4 GiB",
            (54, 2),
            "gui-snapshot-origin-manual",
            false,
        ),
        snapshot(
            "0192e9b4-71ad-7c05-a8e0-5b2917c3d6f4",
            "Before restoring Nether Roof Test",
            "2026-10-06 19:41",
            "2.4 GiB",
            (52, 3),
            "gui-snapshot-origin-before-restore",
            true,
        ),
        snapshot(
            "0192e7d0-3b62-7a18-b4c9-0e8f12a7d935",
            "Before isolating Creeper Confetti",
            "2026-10-05 08:17",
            "2.4 GiB",
            (54, 0),
            "gui-snapshot-origin-before-isolation",
            false,
        ),
    ]
}

fn show_snapshots(ui: &AppWindow) {
    open_surface(
        ui,
        Surface::Snapshots,
        |view| {
            view.snapshots = rows(snapshot_rows());
            view.snapshot_total = "9.5 GiB".into();
            view.snapshots_have_plain_copies = true;
        },
        None,
    );
}

/// The client's snapshots, one of them a full copy.
pub(crate) fn snapshots(ui: &AppWindow) {
    show_snapshots(ui);
}

/// The confirmation to restore a snapshot, with the safety copy on.
pub(crate) fn snapshot_restore_dialog(ui: &AppWindow) {
    show_snapshots(ui);
    ui.set_snapshot_host_kind("client".into());
    ui.set_snapshot_host_id("cavestone".into());
    ui.set_snapshot_action_id("0192f3a1-0c77-7e94-8b12-d3f5a60e4c18".into());
    ui.set_snapshot_action_label("Pre-Ender-Dragon".into());
    ui.set_snapshot_safety_copy(true);
    ui.set_modal_error(SharedString::default());
    ui.set_modal("snapshot-restore".into());
}

/// Forge boot output of the client: (timestamp, thread, level, logger, message).
const BOOT_LOG: &[(&str, &str, &str, &str, &str)] = &[
    (
        "14:02:07.412",
        "main",
        "INFO",
        "cp.mo.mo.Launcher/MODLAUNCHER",
        "ModLauncher running: args [--username, MelanX, --version, 1.20.1-forge-47.4.10, --gameDir, /demo/instances/cavestone/minecraft]",
    ),
    (
        "14:02:07.415",
        "main",
        "INFO",
        "cp.mo.mo.Launcher/MODLAUNCHER",
        "ModLauncher 10.0.9+10.0.9+main.dcd20f30 starting: java version 17.0.12 by Eclipse Adoptium; OS Linux arch amd64 version 6.8.0",
    ),
    (
        "14:02:08.021",
        "main",
        "INFO",
        "mixin/",
        "SpongePowered MIXIN Subsystem Version=0.8.5 Source=file:/demo/shared/libraries/org/spongepowered/mixin/0.8.5/mixin-0.8.5.jar Service=ModLauncher Env=CLIENT",
    ),
    (
        "14:02:08.634",
        "main",
        "INFO",
        "ne.mi.fm.lo.mo.ModDiscoverer/SCAN",
        "Found mod file create-1.20.1-0.5.1.j.jar of type MOD with provider {mods folder locator at /demo/instances/cavestone/minecraft/mods}",
    ),
    (
        "14:02:08.635",
        "main",
        "INFO",
        "ne.mi.fm.lo.mo.ModDiscoverer/SCAN",
        "Found mod file Jade-1.20.1-Forge-11.13.2.jar of type MOD with provider {mods folder locator at /demo/instances/cavestone/minecraft/mods}",
    ),
    (
        "14:02:08.636",
        "main",
        "INFO",
        "ne.mi.fm.lo.mo.ModDiscoverer/SCAN",
        "Found mod file jei-1.20.1-forge-15.20.0.130.jar of type MOD with provider {mods folder locator at /demo/instances/cavestone/minecraft/mods}",
    ),
    (
        "14:02:08.637",
        "main",
        "INFO",
        "ne.mi.fm.lo.mo.ModDiscoverer/SCAN",
        "Found mod file creeper-confetti-1.20.1-forge-0.3.2.jar of type MOD with provider {mods folder locator at /demo/instances/cavestone/minecraft/mods}",
    ),
    (
        "14:02:09.102",
        "main",
        "INFO",
        "ne.mi.fm.lo.mo.JarInJarDependencyLocator/",
        "Found 3 dependencies adding them to mods collection",
    ),
    (
        "14:02:09.488",
        "main",
        "WARN",
        "mixin/",
        "Reference map 'creeper_confetti.refmap.json' for creeper_confetti.mixins.json could not be read. If this is a development environment you can ignore this message",
    ),
    (
        "14:02:09.771",
        "main",
        "DEBUG",
        "cp.mo.mo.LaunchServiceHandler/MODLAUNCHER",
        "Launching target 'forgeclient' with arguments [--version, 1.20.1-forge-47.4.10]",
    ),
    (
        "14:02:10.208",
        "main",
        "INFO",
        "ne.mi.co.ForgeMod/FORGEMOD",
        "Forge mod loading, version 47.4.10, for MC 1.20.1 with MCP 20230612.114412",
    ),
    (
        "14:02:10.209",
        "main",
        "INFO",
        "ne.mi.co.MinecraftForge/FORGE",
        "MinecraftForge v47.4.10 Initialized",
    ),
    (
        "14:02:10.944",
        "Render thread",
        "INFO",
        "net.minecraft.client.Minecraft/",
        "Setting user: MelanX",
    ),
    (
        "14:02:11.350",
        "Render thread",
        "INFO",
        "net.minecraft.client.Minecraft/",
        "Backend library: LWJGL version 3.3.1 build 7",
    ),
    (
        "14:02:11.902",
        "Render thread",
        "INFO",
        "ne.mi.fm.lo.ModLoader/LOADING",
        "Loading 54 mods",
    ),
    (
        "14:02:11.903",
        "Render thread",
        "DEBUG",
        "ne.mi.fm.lo.ModLoader/LOADING",
        "Sorted mod list: create jade jei creeper_confetti redstone_rabbit_holes ender_pearl_pager",
    ),
    (
        "14:02:12.516",
        "Render thread",
        "INFO",
        "ne.mi.fm.ja.FMLModContainer/LOADING",
        "Loading configuration for mod creeper_confetti from /demo/instances/cavestone/minecraft/config/creeper_confetti-common.toml",
    ),
    (
        "14:02:12.730",
        "Render thread",
        "DEBUG",
        "ne.mi.re.GameData/REGISTRIES",
        "Applying holder lookups: minecraft:block",
    ),
    (
        "14:02:12.744",
        "Render thread",
        "DEBUG",
        "ne.mi.re.GameData/REGISTRIES",
        "Applying holder lookups: minecraft:item",
    ),
    (
        "14:02:12.801",
        "Render thread",
        "INFO",
        "create/",
        "Create 0.5.1.j is loading with 34 registered blocks and 212 registered items",
    ),
    (
        "14:02:13.077",
        "Render thread",
        "INFO",
        "jei/",
        "Starting JEI with the Forge platform helpers",
    ),
    (
        "14:02:13.412",
        "Render thread",
        "INFO",
        "ne.mi.re.GameData/REGISTRIES",
        "Freezing registries",
    ),
    (
        "14:02:13.690",
        "Render thread",
        "INFO",
        "ne.mi.co.ForgeMod/FORGEMOD",
        "Registered 4 custom particle types for creeper_confetti",
    ),
    (
        "14:02:14.019",
        "Render thread",
        "ERROR",
        "creeper_confetti/",
        "Missing texture creeper_confetti:textures/particle/confetti_3.png, using the fallback texture",
    ),
    (
        "14:02:14.356",
        "Render thread",
        "INFO",
        "net.minecraft.client.resources.model.ModelBakery/",
        "Baking 4126 block states and 1893 item models",
    ),
    (
        "14:02:15.882",
        "Worker-Main-2",
        "DEBUG",
        "ne.mi.cl.model.ModelLoader/",
        "Baked 36 model variants for create:cogwheel in 22 ms",
    ),
    (
        "14:02:16.204",
        "Render thread",
        "INFO",
        "net.minecraft.client.sounds.SoundEngine/SOUNDS",
        "Sound engine started",
    ),
    (
        "14:02:16.597",
        "Render thread",
        "INFO",
        "net.minecraft.client.renderer.texture.TextureAtlas/",
        "Created: 1024x1024x4 minecraft:textures/atlas/blocks.png-atlas",
    ),
    (
        "14:02:16.640",
        "Render thread",
        "INFO",
        "net.minecraft.client.renderer.texture.TextureAtlas/",
        "Created: 256x256x0 minecraft:textures/atlas/signs.png-atlas",
    ),
    (
        "14:02:17.113",
        "Render thread",
        "WARN",
        "net.minecraft.client.resources.language.LanguageManager/",
        "Language file 'en_us' of ender_pearl_pager is missing 2 keys; falling back to the key names",
    ),
    (
        "14:02:17.388",
        "Render thread",
        "INFO",
        "ne.mi.cl.ClientModLoader/",
        "Finished loading client resources in 5481 ms",
    ),
    (
        "14:02:18.002",
        "Render thread",
        "INFO",
        "jade/",
        "Loaded 37 Jade plugins",
    ),
    (
        "14:02:18.540",
        "Render thread",
        "INFO",
        "net.minecraft.client.Minecraft/",
        "Loading single-player world 'Reproduction Flat'",
    ),
    (
        "14:02:19.275",
        "Server thread",
        "INFO",
        "net.minecraft.server.integrated.IntegratedServer/",
        "Starting integrated minecraft server version 1.20.1",
    ),
    (
        "14:02:19.276",
        "Server thread",
        "INFO",
        "net.minecraft.server.integrated.IntegratedServer/",
        "Generating keypair",
    ),
    (
        "14:02:19.801",
        "Server thread",
        "DEBUG",
        "ne.mi.se.ServerLifecycleHooks/",
        "Preparing level 'Reproduction Flat'",
    ),
    (
        "14:02:20.150",
        "Server thread",
        "INFO",
        "net.minecraft.server.level.ChunkProgressListener/",
        "Preparing spawn area: 0%",
    ),
    (
        "14:02:20.644",
        "Server thread",
        "INFO",
        "net.minecraft.server.level.ChunkProgressListener/",
        "Preparing spawn area: 46%",
    ),
    (
        "14:02:21.012",
        "Server thread",
        "INFO",
        "net.minecraft.server.level.ChunkProgressListener/",
        "Preparing spawn area: 100%",
    ),
    (
        "14:02:21.130",
        "Server thread",
        "WARN",
        "net.minecraft.world.entity.npc.Villager/",
        "Villager 'Cartographer' at 12.5, 64.0, -310.5 has no path to its job site, ignoring",
    ),
    (
        "14:02:21.318",
        "Server thread",
        "INFO",
        "net.minecraft.server.players.PlayerList/",
        "MelanX[local:E:6f1a2c9d] logged in with entity id 214 at (8.5, 65.0, -300.5)",
    ),
    (
        "14:02:21.320",
        "Server thread",
        "INFO",
        "net.minecraft.server.MinecraftServer/",
        "MelanX joined the game",
    ),
    (
        "14:02:21.904",
        "Render thread",
        "DEBUG",
        "ne.mi.cl.ForgeHooksClient/",
        "Reloading shader programs for 11 render types",
    ),
    (
        "14:02:22.481",
        "Render thread",
        "INFO",
        "create/",
        "Create contraption sync ready for 0 active contraptions",
    ),
    (
        "14:02:23.007",
        "Server thread",
        "DEBUG",
        "ne.mi.ev.EventBus/",
        "Dispatched 1840 events during the first server tick",
    ),
    (
        "14:02:24.620",
        "Server thread",
        "INFO",
        "net.minecraft.server.MinecraftServer/",
        "Saving and pausing game...",
    ),
    (
        "14:02:25.133",
        "Render thread",
        "INFO",
        "net.minecraft.client.Minecraft/",
        "[CHAT] Welcome to the CaveStone reproduction world",
    ),
    (
        "14:02:26.870",
        "Server thread",
        "INFO",
        "net.minecraft.server.level.ServerLevel/",
        "Saving chunks for level 'ServerLevel[Reproduction Flat]'/minecraft:overworld",
    ),
    (
        "14:02:28.402",
        "Render thread",
        "INFO",
        "jei/",
        "Registered 1527 recipes in 143 ms",
    ),
    (
        "14:02:31.215",
        "Server thread",
        "INFO",
        "net.minecraft.server.MinecraftServer/",
        "Auto-saved 12 chunks",
    ),
];

fn log_chunks() -> Vec<LogChunk> {
    BOOT_LOG
        .iter()
        .map(|(time, thread, level, logger, message)| LogChunk {
            text: format!("[08Oct2026 {time}] [{thread}/{level}] [{logger}]: {message}").into(),
            level: level.to_ascii_lowercase().into(),
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

const CRASH_TEXT: &str = "---- Minecraft Crash Report ----
// The confetti was a lie.

Time: 2026-10-08 14:31:07
Description: Ticking entity

java.lang.IllegalStateException: Creeper Confetti tried to explode confetti twice
\tat demo.creeperconfetti.entity.ConfettiCreeper.explodeConfetti(ConfettiCreeper.java:142) ~[creeper-confetti-1.20.1-forge-0.3.2.jar%23183!/:0.3.2] {re:classloading}
\tat demo.creeperconfetti.entity.ConfettiCreeper.m_8119_(ConfettiCreeper.java:97) ~[creeper-confetti-1.20.1-forge-0.3.2.jar%23183!/:0.3.2] {re:classloading}
\tat net.minecraft.world.level.Level.m_46653_(Level.java:482) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat net.minecraft.server.level.ServerLevel.m_8647_(ServerLevel.java:733) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat net.minecraft.world.level.Level.m_46653_(Level.java:482) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat net.minecraft.server.level.ServerLevel.m_184063_(ServerLevel.java:365) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat net.minecraft.server.MinecraftServer.m_5703_(MinecraftServer.java:951) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat net.minecraft.client.server.IntegratedServer.m_5703_(IntegratedServer.java:96) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat net.minecraft.server.MinecraftServer.m_130012_(MinecraftServer.java:820) ~[client-1.20.1-20230612.114412-srg.jar%23182!/:?] {re:classloading}
\tat java.lang.Thread.run(Thread.java:840) ~[?:?] {}

A detailed walkthrough of the error, its code path and all known details is as follows:
---------------------------------------------------------------------------------------

-- Head --
Thread: Server thread
Stacktrace:
\tat demo.creeperconfetti.entity.ConfettiCreeper.explodeConfetti(ConfettiCreeper.java:142) ~[creeper-confetti-1.20.1-forge-0.3.2.jar%23183!/:0.3.2] {re:classloading}
\tat demo.creeperconfetti.entity.ConfettiCreeper.m_8119_(ConfettiCreeper.java:97) ~[creeper-confetti-1.20.1-forge-0.3.2.jar%23183!/:0.3.2] {re:classloading}

-- Entity being ticked --
Details:
\tEntity Type: creeper_confetti:confetti_creeper (demo.creeperconfetti.entity.ConfettiCreeper)
\tEntity ID: 4821
\tEntity Name: Confetti Creeper
\tEntity's Exact location: 41.50, 64.00, -318.50
\tEntity's Block location: World: (41,64,-319), Section: (at 9,0,1 in 2,4,-20)
\tEntity's Momentum: 0.00, -0.08, 0.00

-- System Details --
Details:
\tMinecraft Version: 1.20.1
\tMinecraft Version ID: 1.20.1
\tOperating System: Linux (amd64) version 6.8.0
\tJava Version: 17.0.12, Eclipse Adoptium
\tJava VM Version: OpenJDK 64-Bit Server VM (mixed mode, sharing), Eclipse Adoptium
\tMemory: 1914502144 bytes (1825 MiB) / 3221225472 bytes (3072 MiB) up to 6442450944 bytes (6144 MiB)
\tModLauncher: 10.0.9+10.0.9+main.dcd20f30
\tFML Language Providers:
\t\tminecraft@1.0
\t\tjavafml@47.4
\tMod List:
\t\tcreate-1.20.1-0.5.1.j.jar                         |Create                        |create                        |0.5.1.j             |DONE      |Manifest: NOSIGNATURE
\t\tJade-1.20.1-Forge-11.13.2.jar                     |Jade                          |jade                          |11.13.2             |DONE      |Manifest: NOSIGNATURE
\t\tcreeper-confetti-1.20.1-forge-0.3.2.jar           |Creeper Confetti              |creeper_confetti              |0.3.2               |DONE      |Manifest: NOSIGNATURE
\tPlayer Count: 1 / 8; [ServerPlayer['MelanX'/214, l='ServerLevel[Reproduction Flat]', x=8.50, y=65.00, z=-300.50]]
\tData Packs: vanilla, mod:creeper_confetti, mod:create
\tType: Integrated Server (map_client.txt)
\tIs Modded: Definitely; Client brand changed to 'forge'; Server brand changed to 'forge'
";

/// Tabs become spaces to the next multiple of four columns, as the app shows reports.
fn expand_tabs(text: &str) -> String {
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

/// The crash reports of a client, with the newest one open.
pub(crate) fn client_crash_reports(ui: &AppWindow) {
    let newest = "crash-2026-10-08_14.31.07-client.txt";
    let files = vec![
        crash_file(newest, "Today", "5.8 KiB", "Ticking entity"),
        crash_file(
            "crash-2026-10-07_21.12.44-client.txt",
            "Yesterday",
            "7.2 KiB",
            "Rendering overlay",
        ),
        crash_file(
            "crash-2026-10-05_09.40.18-client.txt",
            "2026-10-05 09:40",
            "6.4 KiB",
            "Initializing game",
        ),
    ];
    open_surface(
        ui,
        Surface::Logs,
        |_| {},
        Some(LogView {
            mode: "crashes".into(),
            file: newest.into(),
            files: rows(files),
            report: expand_tabs(CRASH_TEXT).into(),
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
    fn the_boot_log_mixes_levels_and_stays_synthetic() {
        let chunks = log_chunks();
        assert!((40..=60).contains(&chunks.len()));
        for level in ["info", "warn", "error", "debug"] {
            assert!(chunks.iter().any(|chunk| chunk.level == level));
        }
        assert!(chunks.iter().all(|chunk| !chunk.text.contains("http")));
    }
}
