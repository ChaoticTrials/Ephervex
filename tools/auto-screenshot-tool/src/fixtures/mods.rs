//! Mod management dialogs and the isolation surface of the CaveStone workspace.

use ephervex_gui::ui::{
    AppWindow, HostView, ImpactRow, IsolationCandidateRow, LinkedTargetRow, ModVersionRow,
    RelationRow, WorkflowActionRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{Model, SharedString};

use super::{FORGE_1_20, client_mod_results, client_mod_rows, rows};

/// The workflow id the isolation session registers on its host.
const ISOLATION_WORKFLOW: &str = "mod-isolation";

fn strings(values: &[&str]) -> slint::ModelRc<SharedString> {
    rows(
        values
            .iter()
            .map(|value| SharedString::from(*value))
            .collect(),
    )
}

/// Opens the CaveStone workspace on `surface`, letting `edit` fill the host.
fn cavestone_workspace(ui: &AppWindow, surface: Surface, edit: impl FnOnce(&mut HostView)) {
    let client = ui.get_instances().row_data(0).unwrap_or_default();
    let offered = Surface::client();
    let choice = offered
        .iter()
        .position(|offered| *offered == surface)
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or_default();
    let surfaces = offered
        .iter()
        .map(|surface| WorkspaceSurfaceRow {
            id: surface.id().into(),
            label: lookup(surface.label()).into(),
        })
        .collect::<Vec<_>>();
    let mut view = HostView {
        kind: "client".into(),
        id: client.id.clone(),
        available: true,
        client,
        surfaces: rows(surfaces),
        mods: rows(client_mod_rows()),
        allow_pack_mod_changes: true,
        browser_provider: "modrinth".into(),
        browser_submitted: "villager".into(),
        browser_ran: true,
        browser_results: rows(client_mod_results()),
        browser_next_offset: -1,
        ..HostView::default()
    };
    edit(&mut view);
    ui.set_host_views(rows(vec![view]));
    ui.set_workspace_panes(rows(vec![WorkspacePaneRow {
        id: 1,
        host: 0,
        log: -1,
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

fn mods_surface(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |_| {});
}

fn version(
    id: &str,
    number: &str,
    channel: &str,
    published: &str,
    installed: bool,
    newest: bool,
    restricted: bool,
) -> ModVersionRow {
    ModVersionRow {
        version_id: id.into(),
        name: format!("Create {number}").into(),
        number: number.into(),
        channel: channel.into(),
        published: published.into(),
        installed,
        newest,
        restricted,
    }
}

fn relation(name: &str, detail: &str, kind: &str, state: &str, attention: bool) -> RelationRow {
    RelationRow {
        name: name.into(),
        detail: detail.into(),
        kind: lookup(kind).into(),
        state: if state.is_empty() {
            SharedString::default()
        } else {
            lookup(state).into()
        },
        attention,
    }
}

fn impact(name: &str, distance: i32) -> ImpactRow {
    ImpactRow {
        name: name.into(),
        logical_path: format!("mods/{name}").into(),
        distance,
    }
}

/// Manage Mod for a pack-owned mod with a newer release available.
pub(crate) fn mod_manager(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |view| {
        let mut mods = client_mod_rows();
        mods[0].version = "6.0.4".into();
        mods[0].file_name = "create-1.20.1-6.0.4.jar".into();
        mods[0].logical_path = "mods/create-1.20.1-6.0.4.jar".into();
        view.mods = rows(mods);
    });
    ui.set_mod_action_host("cavestone".into());
    ui.set_mod_action_path("mods/create-1.20.1-6.0.4.jar".into());
    ui.set_mod_action_name("Create".into());
    ui.set_mod_action_project_url("https://example.invalid/mod/create".into());
    ui.set_mod_action_installed("create-6-0-4".into());
    ui.set_mod_action_notice(SharedString::default());
    ui.set_mod_action_loading(false);
    ui.set_mod_action_pending(false);
    ui.set_mod_action_can_change(true);
    ui.set_mod_action_can_update(false);
    ui.set_mod_action_can_remove(true);
    ui.set_mod_action_up_to_date(true);
    ui.set_mod_action_is_pack_owned(true);
    ui.set_mod_action_pack_original_version("create-0-5-1-j".into());
    ui.set_mod_action_selected("create-0-5-1-j".into());
    ui.set_mod_versions(rows(vec![
        version(
            "create-6-0-4",
            "6.0.4",
            "release",
            "2026-08-19",
            true,
            true,
            false,
        ),
        version(
            "create-6-0-3",
            "6.0.3",
            "release",
            "2026-07-02",
            false,
            false,
            true,
        ),
        version(
            "create-0-5-1-j",
            "0.5.1.j",
            "release",
            "2024-04-10",
            false,
            false,
            false,
        ),
        version(
            "create-0-5-1-i",
            "0.5.1.i",
            "beta",
            "2024-02-27",
            false,
            false,
            false,
        ),
        version(
            "create-0-5-1-h",
            "0.5.1.h",
            "release",
            "2024-01-15",
            false,
            false,
            false,
        ),
    ]));
    mod_manager_relations(ui);
    ui.set_modal("mod-manage".into());
}

/// Manage Mod for an added mod whose catalog has a newer release, offering Update.
pub(crate) fn mod_update_available(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |_| {});
    ui.set_mod_action_host("cavestone".into());
    ui.set_mod_action_path("mods/redstone-rabbit-holes-1.20.1-1.4.0.jar".into());
    ui.set_mod_action_name("Redstone Rabbit Holes".into());
    ui.set_mod_action_project_url("https://example.invalid/mod/redstone-rabbit-holes".into());
    ui.set_mod_action_installed("rrh-1-4-0".into());
    ui.set_mod_action_notice(SharedString::default());
    ui.set_mod_action_loading(false);
    ui.set_mod_action_pending(false);
    ui.set_mod_action_can_change(true);
    ui.set_mod_action_can_update(true);
    ui.set_mod_action_can_remove(true);
    ui.set_mod_action_up_to_date(false);
    ui.set_mod_action_is_pack_owned(false);
    ui.set_mod_action_pack_original_version(SharedString::default());
    ui.set_mod_action_selected("rrh-1-5-0".into());
    let release =
        |id: &str, number: &str, published: &str, installed: bool, newest: bool| ModVersionRow {
            name: format!("Redstone Rabbit Holes {number}").into(),
            ..version(id, number, "release", published, installed, newest, false)
        };
    ui.set_mod_versions(rows(vec![
        release("rrh-1-5-0", "1.5.0", "2026-09-14", false, true),
        release("rrh-1-4-1", "1.4.1", "2026-08-02", false, false),
        release("rrh-1-4-0", "1.4.0", "2026-06-21", true, false),
        release("rrh-1-3-2", "1.3.2", "2026-04-09", false, false),
    ]));
    ui.set_mod_action_requires(rows(vec![relation(
        "Ender Pearl Pager",
        "ender-pearl-pager",
        "gui-mod-relation-required",
        "",
        false,
    )]));
    ui.set_mod_action_required_by(rows(Vec::new()));
    ui.set_mod_action_requires_note(SharedString::default());
    ui.set_mod_action_relations_refreshable(false);
    ui.set_modal("mod-manage".into());
}

/// A pack mod whose jar was deleted outside Ephervex, offering Restore.
pub(crate) fn mod_missing_file(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |view| {
        let mods = client_mod_rows();
        let missing = mods
            .iter()
            .filter(|row| matches!(row.title.as_str(), "Create" | "Pocket Piglin" | "Jade"))
            .cloned()
            .collect::<Vec<_>>();
        view.mods = rows(missing);
    });
}

fn mod_manager_relations(ui: &AppWindow) {
    ui.set_mod_action_requires(rows(vec![
        relation(
            "Ender Pearl Pager",
            "ender-pearl-pager",
            "gui-mod-relation-required",
            "",
            false,
        ),
        relation(
            "Cow Clicker",
            "cow-clicker",
            "gui-mod-relation-optional",
            "gui-mod-relation-absent",
            true,
        ),
    ]));
    ui.set_mod_action_required_by(rows(vec![
        relation(
            "Redstone Rabbit Holes",
            "redstone-rabbit-holes",
            "gui-mod-relation-required",
            "",
            false,
        ),
        relation(
            "Creeper Confetti",
            "creeper-confetti",
            "gui-mod-relation-optional",
            "",
            false,
        ),
    ]));
    ui.set_mod_action_requires_note(SharedString::default());
    ui.set_mod_action_relations_refreshable(false);
}

/// The confirmation shown before turning off a mod other mods need.
pub(crate) fn mod_impact(ui: &AppWindow) {
    mods_surface(ui);
    ui.set_mod_impact_kind("client".into());
    ui.set_mod_impact_host("cavestone".into());
    ui.set_mod_impact_path("mods/ender-pearl-pager-forge-2.1.7.jar".into());
    ui.set_mod_impact_name("Ender Pearl Pager".into());
    ui.set_mod_impact_deleting(false);
    ui.set_mod_impact_enabling(false);
    ui.set_mod_impact_breaks(rows(vec![
        impact("Create", 1),
        impact("Redstone Rabbit Holes", 1),
        impact("Villager Notice Board", 1),
        impact("Silent Minecart Rails", 2),
    ]));
    ui.set_mod_impact_degrades(rows(Vec::new()));
    ui.set_mod_impact_unknown(rows(Vec::new()));
    ui.set_mod_impact_unidentifiable(rows(Vec::new()));
    ui.set_mod_requires_disabled(rows(Vec::new()));
    ui.set_mod_requires_absent(rows(Vec::new()));
    ui.set_mod_requires_incompatible(rows(Vec::new()));
    ui.set_mod_impact_cascade(true);
    ui.set_mod_impact_remember(false);
    ui.set_modal_error(SharedString::default());
    ui.set_modal("mod-impact".into());
}

fn linked_target(
    id: &str,
    name: &str,
    state: &str,
    explanation: String,
    tone: &str,
    actionable: bool,
    blocked: bool,
) -> LinkedTargetRow {
    LinkedTargetRow {
        id: id.into(),
        name: name.into(),
        detail: format!("1.20.1 | {}", FORGE_1_20.display).into(),
        state: lookup(state).into(),
        explanation: explanation.into(),
        tone: tone.into(),
        actionable,
        blocked,
        selected: actionable,
    }
}

/// The install confirmation with the linked servers that can receive the mod.
pub(crate) fn mod_install_plan(ui: &AppWindow) {
    mods_surface(ui);
    ui.set_mod_plan_instance("cavestone".into());
    ui.set_mod_plan_provider("modrinth".into());
    ui.set_mod_plan_project("villager-notice-board".into());
    ui.set_mod_plan_version_id("villager-notice-board-1-3-0".into());
    ui.set_mod_plan_title("Villager Notice Board".into());
    ui.set_mod_plan_version("1.3.0".into());
    ui.set_mod_plan_install_count(3);
    ui.set_mod_plan_dependency_count(2);
    ui.set_mod_plan_existing_count(1);
    ui.set_mod_plan_optional_count(2);
    ui.set_mod_plan_warning_count(1);
    ui.set_mod_action_pending(false);
    ui.set_linked_support(lookup("gui-linked-support-required").into());
    ui.set_linked_targets(rows(vec![
        linked_target(
            "cavestone-server",
            "CaveStone Server",
            "linked-target-will-install",
            lookup_args("gui-linked-installs", [("count", Arg::from(3_usize))]),
            "ok",
            true,
            false,
        ),
        linked_target(
            "cavestone-staging",
            "CaveStone Staging",
            "linked-target-will-install",
            lookup_args("gui-linked-installs", [("count", Arg::from(3_usize))]),
            "ok",
            true,
            false,
        ),
        linked_target(
            "cavestone-archive",
            "CaveStone Archive",
            "gui-linked-unavailable",
            lookup("linked-target-server-not-installed"),
            "warn",
            false,
            true,
        ),
    ]));
    ui.set_modal_error(SharedString::default());
    ui.set_modal("mod-plan".into());
}

fn candidate(name: &str, file: &str, pinned: bool) -> IsolationCandidateRow {
    IsolationCandidateRow {
        path: format!("mods/{file}").into(),
        display_path: file.into(),
        name: name.into(),
        selected: pinned,
    }
}

/// The isolation setup with candidates, warnings, and Start enabled.
pub(crate) fn mod_isolation_setup(ui: &AppWindow) {
    let candidates = vec![
        candidate("Create", "create-1.20.1-0.5.1.j.jar", true),
        candidate(
            "Creeper Confetti",
            "creeper-confetti-1.20.1-forge-0.3.2.jar",
            false,
        ),
        candidate(
            "Ender Pearl Pager",
            "ender-pearl-pager-forge-2.1.7.jar",
            true,
        ),
        candidate("Jade", "Jade-1.20.1-Forge-11.13.2.jar", false),
        candidate("JEI", "jei-1.20.1-forge-15.20.0.130.jar", true),
        candidate("Lantern Lattice", "lantern-lattice-1.20.1-2.0.3.jar", false),
        candidate("Nether Fridge", "nether-fridge-custom.jar", false),
        candidate(
            "Pocket Piglin",
            "pocket-piglin-1.20.1-forge-1.2.0.jar",
            false,
        ),
        candidate(
            "Redstone Rabbit Holes",
            "redstone-rabbit-holes-1.20.1-1.4.0.jar",
            false,
        ),
        candidate(
            "Silent Minecart Rails",
            "silent-minecart-rails-1.20.1-0.8.2.jar",
            false,
        ),
        candidate(
            "Spawn Chunk Surveyor",
            "spawn-chunk-surveyor-forge-1.1.5.jar",
            false,
        ),
        candidate(
            "Villager Notice Board",
            "villager-notice-board-1.20.1-1.3.0.jar",
            false,
        ),
    ];
    let summary = lookup_args(
        "gui-isolation-setup-summary",
        [("count", Arg::from(candidates.len()))],
    );
    cavestone_workspace(ui, Surface::Isolation, |view| {
        view.isolation_phase = "setup".into();
        view.isolation_summary = summary.into();
        view.isolation_candidates = rows(candidates);
        view.isolation_unrecorded = strings(&[
            "lantern-lattice-1.20.1-2.0.3.jar",
            "villager-notice-board-1.20.1-1.3.0.jar",
        ]);
        view.isolation_unidentifiable = strings(&[
            "nether-fridge-custom.jar",
            "spawn-chunk-surveyor-forge-1.1.5.jar",
        ]);
        view.isolation_linked_servers = strings(&[
            "CaveStone Server (cavestone-server)",
            "CaveStone Staging (cavestone-staging)",
        ]);
        view.isolation_accept_unrecorded = true;
    });
}

fn history_entry(round: i32, answer: &str, kind: &str, count: usize) -> String {
    lookup_args(
        "gui-isolation-history-entry",
        [
            ("round", Arg::from(round)),
            ("answer", Arg::from(lookup(answer))),
            ("kind", Arg::from(kind)),
            ("count", Arg::from(count)),
        ],
    )
}

fn workflow_action(
    id: &str,
    message: &str,
    tone: &str,
    requires_stopped: bool,
) -> WorkflowActionRow {
    WorkflowActionRow {
        id: id.into(),
        label: lookup(message).into(),
        tone: tone.into(),
        enabled: true,
        requires_stopped,
    }
}

/// A session waiting for the answer to round 3, with its banner above the tabs.
pub(crate) fn mod_isolation_round(ui: &AppWindow) {
    let round = 3;
    let summary = lookup_args("gui-isolation-round-summary", [("round", Arg::from(round))]);
    cavestone_workspace(ui, Surface::Isolation, |view| {
        view.isolation_phase = "round".into();
        view.isolation_active = true;
        view.isolation_round = round;
        view.isolation_granularity = 4;
        view.isolation_summary = summary.clone().into();
        view.isolation_enabled = strings(&[
            "create-1.20.1-0.5.1.j.jar",
            "creeper-confetti-1.20.1-forge-0.3.2.jar",
            "ender-pearl-pager-forge-2.1.7.jar",
            "Jade-1.20.1-Forge-11.13.2.jar",
            "jei-1.20.1-forge-15.20.0.130.jar",
            "lantern-lattice-1.20.1-2.0.3.jar",
            "redstone-rabbit-holes-1.20.1-1.4.0.jar",
        ]);
        view.isolation_disabled = strings(&[
            "nether-fridge-custom.jar",
            "pocket-piglin-1.20.1-forge-1.2.0.jar",
            "silent-minecart-rails-1.20.1-0.8.2.jar",
            "spawn-chunk-surveyor-forge-1.1.5.jar",
            "villager-notice-board-1.20.1-1.3.0.jar",
        ]);
        view.isolation_history = rows(
            [
                history_entry(2, "gui-isolation-issue-present", "complement", 7),
                history_entry(1, "gui-isolation-issue-gone", "subset", 6),
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        );
        view.isolation_reduced_confidence = true;
        view.workflow_id = ISOLATION_WORKFLOW.into();
        view.workflow_surface = Surface::Isolation.id().into();
        view.workflow_title = lookup("gui-isolation-active").into();
        view.workflow_summary = summary.into();
        view.workflow_actions = rows(vec![
            workflow_action("details", "gui-isolation-detailed-view", "secondary", false),
            workflow_action("gone", "gui-isolation-issue-gone", "secondary", true),
            workflow_action("present", "gui-isolation-issue-present", "primary", true),
            workflow_action("undo", "gui-isolation-undo", "secondary", true),
        ]);
    });
}

/// The isolation result waiting to be finished or canceled.
pub(crate) fn mod_isolation_result(ui: &AppWindow) {
    let summary = lookup("gui-isolation-ready-summary");
    cavestone_workspace(ui, Surface::Isolation, |view| {
        view.isolation_phase = "ready".into();
        view.isolation_active = true;
        view.isolation_round = 5;
        view.isolation_confidence = "one_minimal".into();
        view.isolation_summary = summary.clone().into();
        view.isolation_remaining_candidates = strings(&["creeper-confetti-1.20.1-forge-0.3.2.jar"]);
        view.isolation_pinned = strings(&[
            "create-1.20.1-0.5.1.j.jar",
            "jei-1.20.1-forge-15.20.0.130.jar",
        ]);
        view.isolation_required_dependencies = strings(&["ender-pearl-pager-forge-2.1.7.jar"]);
        view.isolation_history = rows(
            [
                history_entry(5, "gui-isolation-issue-present", "subset", 1),
                history_entry(4, "gui-isolation-issue-gone", "complement", 3),
                history_entry(3, "gui-isolation-issue-present", "complement", 7),
            ]
            .into_iter()
            .map(SharedString::from)
            .collect(),
        );
        view.workflow_id = ISOLATION_WORKFLOW.into();
        view.workflow_surface = Surface::Isolation.id().into();
        view.workflow_title = lookup("gui-isolation-active").into();
        view.workflow_summary = summary.into();
        view.workflow_actions = rows(vec![
            workflow_action("details", "gui-isolation-detailed-view", "secondary", false),
            workflow_action("undo", "gui-isolation-undo", "secondary", true),
            workflow_action("finish", "gui-isolation-finish", "primary", true),
        ]);
    });
}

/// A round whose mod setup could not be reapplied, so it cannot be answered yet.
pub(crate) fn mod_isolation_recovery(ui: &AppWindow) {
    mod_isolation_round(ui);
    let mut view = ui.get_host_views().row_data(0).unwrap_or_default();
    view.isolation_recovery = "failed".into();
    view.isolation_recovery_detail =
        "The jar mods/jei-1.20.1-forge-15.20.0.130.jar is missing from the mods folder.".into();
    view.workflow_summary = lookup("gui-isolation-recovery-summary").into();
    view.workflow_actions = rows(vec![workflow_action(
        "details",
        "gui-isolation-detailed-view",
        "secondary",
        false,
    )]);
    ui.get_host_views().set_row_data(0, view);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use crate::capture::{CaptureOptions, Dimensions, ThemeName, generate};
    use crate::catalog::STATES;

    #[test]
    fn every_mod_dialog_and_isolation_state_renders() {
        let names = [
            "mod-manager",
            "mod-impact",
            "mod-install-plan",
            "mod-isolation-setup",
            "mod-isolation-round",
            "mod-update-available",
            "mod-missing-file",
            "mod-isolation-result",
            "mod-isolation-recovery",
        ];
        let states: Vec<_> = STATES
            .iter()
            .filter(|state| names.contains(&state.name))
            .copied()
            .collect();
        assert_eq!(states.len(), names.len());
        let _session = crate::capture::render_session();
        let output = tempfile::tempdir().expect("temporary output");
        let files = generate(&CaptureOptions {
            states: &states,
            themes: &[ThemeName::Light],
            sizes: &[Dimensions::HD],
            output: output.path(),
            derive_1080: false,
        })
        .expect("render the states");
        assert_eq!(files.len(), names.len());
        assert!(files.iter().all(|file| file.is_file()));
    }
}
