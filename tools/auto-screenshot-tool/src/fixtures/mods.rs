//! Mod management dialogs and the isolation surface of the CaveStone workspace.

use ephervex_gui::ui::{
    AppWindow, HostView, ImpactRow, IsolationCandidateRow, LinkedTargetRow, ModVersionRow,
    RelationRow, WorkflowActionRow, WorkspacePaneRow, WorkspaceSurfaceRow,
};
use ephervex_gui::workspace_layout::Surface;
use ephervex_i18n::{Arg, lookup, lookup_args};
use slint::{Model, SharedString};

use super::recorded::{self, Manage, jar};
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

fn jar_strings(titles: &[&str]) -> slint::ModelRc<SharedString> {
    rows(
        titles
            .iter()
            .map(|title| SharedString::from(jar(title)))
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
        browser_submitted: recorded::data().mod_search.query.clone().into(),
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

fn version_rows(manage: &Manage) -> Vec<ModVersionRow> {
    manage
        .versions
        .iter()
        .map(|release| ModVersionRow {
            version_id: release.id.clone().into(),
            name: release.name.clone().into(),
            number: release.number.clone().into(),
            channel: release.channel.clone().into(),
            published: release.published.clone().into(),
            installed: release.id == manage.installed_id,
            newest: release.id == manage.newest_id,
            restricted: false,
        })
        .collect()
}

fn relation_rows(relations: &[recorded::Relation]) -> Vec<RelationRow> {
    relations
        .iter()
        .map(|relation| RelationRow {
            name: relation.name.clone().into(),
            detail: SharedString::default(),
            kind: lookup(if relation.kind == "required" {
                "gui-mod-relation-required"
            } else {
                "gui-mod-relation-optional"
            })
            .into(),
            state: if relation.present {
                SharedString::default()
            } else {
                lookup("gui-mod-relation-absent").into()
            },
            attention: !relation.present,
        })
        .collect()
}

fn impact(name: &str, path: &str, distance: i32) -> ImpactRow {
    ImpactRow {
        name: name.into(),
        logical_path: path.into(),
        distance,
    }
}

/// Fills the Manage Mod dialog of `host` for a mod with a newer release available.
pub(crate) fn manage_dialog(
    ui: &AppWindow,
    (kind, host): (&str, &str),
    manage: &Manage,
    pack_owned: bool,
) {
    ui.set_mod_action_host_kind(kind.into());
    ui.set_mod_action_host(host.into());
    ui.set_mod_action_path(manage.path.clone().into());
    ui.set_mod_action_name(manage.title.clone().into());
    ui.set_mod_action_project_url(format!("https://modrinth.com/mod/{}", manage.project).into());
    ui.set_mod_action_installed(manage.installed_id.clone().into());
    ui.set_mod_action_notice(SharedString::default());
    ui.set_mod_action_loading(false);
    ui.set_mod_action_pending(false);
    ui.set_mod_action_can_change(true);
    ui.set_mod_action_can_update(true);
    ui.set_mod_action_can_remove(true);
    ui.set_mod_action_up_to_date(false);
    ui.set_mod_action_is_pack_owned(pack_owned);
    // The mod is still on the release it came with, which offers no reset.
    ui.set_mod_action_pack_original_version(SharedString::default());
    ui.set_mod_action_selected(manage.newest_id.clone().into());
    ui.set_mod_versions(rows(version_rows(manage)));
    ui.set_mod_action_requires(rows(relation_rows(&manage.requires)));
    ui.set_mod_action_required_by(rows(relation_rows(&manage.required_by)));
    ui.set_mod_action_requires_note(SharedString::default());
    ui.set_mod_action_relations_refreshable(false);
    ui.set_modal("mod-manage".into());
}

/// Manage Mod for a pack-owned mod with a newer release available.
pub(crate) fn mod_manager(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |_| {});
    manage_dialog(
        ui,
        ("client", "cavestone"),
        &recorded::dialogs().manage,
        true,
    );
}

/// Manage Mod for an added mod whose catalog has a newer release, offering Update.
pub(crate) fn mod_update_available(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |_| {});
    manage_dialog(
        ui,
        ("client", "cavestone"),
        &recorded::dialogs().update,
        false,
    );
}

/// A pack mod whose jar was deleted outside Ephervex, offering Restore.
pub(crate) fn mod_missing_file(ui: &AppWindow) {
    cavestone_workspace(ui, Surface::Mods, |view| {
        let mods = client_mod_rows();
        let shown = mods
            .iter()
            .filter(|row| row.deleted || matches!(row.title.as_str(), "Balm" | "Create"))
            .cloned()
            .collect::<Vec<_>>();
        view.mods = rows(shown);
    });
}

/// The confirmation shown before turning off a mod other mods need.
pub(crate) fn mod_impact(ui: &AppWindow) {
    mods_surface(ui);
    let recorded = &recorded::dialogs().impact;
    ui.set_mod_impact_kind("client".into());
    ui.set_mod_impact_host("cavestone".into());
    ui.set_mod_impact_path(recorded.path.clone().into());
    ui.set_mod_impact_name(recorded.name.clone().into());
    ui.set_mod_impact_deleting(false);
    ui.set_mod_impact_enabling(false);
    ui.set_mod_impact_breaks(rows(
        recorded
            .breaks
            .iter()
            .map(|broken| impact(&broken.name, &broken.path, broken.distance))
            .collect(),
    ));
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
    let plan = &recorded::dialogs().plan;
    let installs = 1 + plan.dependencies.saturating_sub(plan.existing);
    let count = |value: usize| i32::try_from(value).unwrap_or_default();
    ui.set_mod_plan_instance("cavestone".into());
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
    ui.set_mod_action_pending(false);
    ui.set_linked_support(lookup("gui-linked-support-required").into());
    let will_install = || lookup_args("gui-linked-installs", [("count", Arg::from(installs))]);
    ui.set_linked_targets(rows(vec![
        linked_target(
            "cavestone-server",
            "CaveStone Server",
            "linked-target-will-install",
            will_install(),
            "ok",
            true,
            false,
        ),
        linked_target(
            "cavestone-staging",
            "CaveStone Staging",
            "linked-target-will-install",
            will_install(),
            "ok",
            true,
            false,
        ),
    ]));
    ui.set_modal_error(SharedString::default());
    ui.set_modal("mod-plan".into());
}

fn candidate(title: &str, pinned: bool) -> IsolationCandidateRow {
    let file = jar(title);
    IsolationCandidateRow {
        path: format!("mods/{file}").into(),
        display_path: file.into(),
        name: title.into(),
        selected: pinned,
    }
}

/// The mods an isolation session reduces, by title. The list is a slice of
/// the recorded CaveStone mods, in the launcher's order.
const ISOLATION_POOL: [&str; 12] = [
    "AIOT Botania",
    "Balm",
    "Botania",
    "Cooking for Blockheads",
    "Crafting Tweaks",
    "Curios API",
    "Extra Disks",
    "Just Enough Items (JEI)",
    "KleeSlabs",
    "Patchouli",
    "Refined Storage",
    "Waystones",
];

/// The isolation setup with candidates, warnings, and Start enabled.
pub(crate) fn mod_isolation_setup(ui: &AppWindow) {
    let pinned = ["Balm", "Curios API", "Just Enough Items (JEI)"];
    let candidates = ISOLATION_POOL
        .iter()
        .map(|title| candidate(title, pinned.contains(title)))
        .collect::<Vec<_>>();
    let summary = lookup_args(
        "gui-isolation-setup-summary",
        [("count", Arg::from(candidates.len()))],
    );
    let hand_placed = recorded::data()
        .cavestone_mods
        .iter()
        .filter(|row| row.origin == "hand-placed")
        .map(|row| SharedString::from(row.file_name.as_str()))
        .collect::<Vec<_>>();
    let overrides = recorded::data()
        .cavestone_mods
        .iter()
        .filter(|row| row.origin == "override")
        .take(2)
        .map(|row| SharedString::from(row.file_name.as_str()))
        .collect::<Vec<_>>();
    cavestone_workspace(ui, Surface::Isolation, |view| {
        view.isolation_phase = "setup".into();
        view.isolation_summary = summary.into();
        view.isolation_candidates = rows(candidates);
        view.isolation_unrecorded = rows(hand_placed);
        view.isolation_unidentifiable = rows(overrides);
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
        view.isolation_enabled = jar_strings(&[
            "AIOT Botania",
            "Balm",
            "Botania",
            "Cooking for Blockheads",
            "Curios API",
            "Just Enough Items (JEI)",
            "Patchouli",
        ]);
        view.isolation_disabled = jar_strings(&[
            "Crafting Tweaks",
            "Extra Disks",
            "KleeSlabs",
            "Refined Storage",
            "Waystones",
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
        view.isolation_remaining_candidates = jar_strings(&["Waystones"]);
        view.isolation_pinned = jar_strings(&["Balm", "Just Enough Items (JEI)"]);
        view.isolation_required_dependencies = jar_strings(&["Curios API"]);
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
    view.isolation_recovery_detail = format!(
        "The jar mods/{} is missing from the mods folder.",
        jar("Just Enough Items (JEI)")
    )
    .into();
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
        assert_eq!(
            files.len(),
            states
                .iter()
                .map(|state| state.regions.len())
                .sum::<usize>()
        );
        assert!(files.iter().all(|file| file.is_file()));
    }
}
