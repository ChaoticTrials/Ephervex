//! The UI states and regions used in documentation.

use ephervex_gui::ui::AppWindow;

use crate::fixtures;

#[derive(Clone, Copy)]
pub(crate) struct Region {
    pub(crate) name: &'static str,
    pub(crate) accessible_label: Option<&'static str>,
    /// Horizontal and vertical crop padding in source pixels.
    pub(crate) padding: (f32, f32),
    pub(crate) bounds: Option<RegionBounds>,
}

#[derive(Clone, Copy)]
pub(crate) enum RegionBounds {
    Centered {
        left: f32,
        top: f32,
        width: f32,
        height: f32,
    },
    RightAligned {
        right: f32,
        top: f32,
        width: f32,
        height: f32,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct State {
    pub(crate) name: &'static str,
    pub(crate) show: fn(&AppWindow),
    pub(crate) regions: &'static [Region],
    /// A 1080p image may be resized from 1440p only after a state is known to
    /// preserve the same layout at both window sizes.
    pub(crate) same_layout_at_both_sizes: bool,
    /// Opens a menu or dropdown once the window is shown, before the capture.
    pub(crate) interaction: Option<Interaction>,
}

/// A pointer press on the element with this accessible label, or a wheel
/// scroll that brings it near the top of its scroll area.
#[derive(Clone, Copy)]
pub(crate) enum Interaction {
    Click(&'static str),
    RightClick(&'static str),
    ScrollTo(&'static str),
}

const INSTANCE_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "client-card",
        accessible_label: Some("CaveStone"),
        padding: (12.0, 12.0),
        bounds: None,
    },
];
const SERVER_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "server-card",
        accessible_label: Some("CaveStone Server"),
        padding: (12.0, 12.0),
        bounds: None,
    },
];
const ACCOUNT_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "offline-profile",
        accessible_label: Some("MelanX"),
        padding: (12.0, 12.0),
        bounds: None,
    },
];
const FULL_ONLY: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: (0.0, 0.0),
    bounds: None,
}];

const MANUAL_DOWNLOAD_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "blocked-file",
        accessible_label: Some("Obsidian Mirror"),
        padding: (0.0, 0.0),
        bounds: None,
    },
];

const CREATE_INSTANCE_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "dialog",
        accessible_label: Some("New Instance"),
        padding: (4.0, 4.0),
        bounds: None,
    },
];

const ISOLATION_ROUND_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "answer-controls",
        accessible_label: Some("Still failing"),
        padding: (140.0, 22.0),
        bounds: None,
    },
];

const CLIENT_HOSTS_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: (0.0, 0.0),
    bounds: None,
}];

const SETTINGS_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: (0.0, 0.0),
    bounds: None,
}];

const MODS_FULL_WINDOW_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: (0.0, 0.0),
    bounds: None,
}];

const DIAGNOSTICS_SERVERS_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: (0.0, 0.0),
    bounds: None,
}];

const DIAGNOSTIC_RESULT_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "missing-files",
        accessible_label: Some("Problem: 3 recorded files are missing or damaged"),
        padding: (4.0, 4.0),
        bounds: None,
    },
];

const SERVER_PROPERTIES_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "access",
        accessible_label: Some("Access"),
        padding: (12.0, 22.0),
        bounds: None,
    },
    Region {
        name: "online-mode",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: Some(RegionBounds::Centered {
            left: -453.0,
            top: 344.0,
            width: 660.0,
            height: 56.0,
        }),
    },
    Region {
        name: "eula",
        accessible_label: Some("Minecraft Server EULA"),
        padding: (4.0, 4.0),
        bounds: None,
    },
];

const INSTANCE_MENU_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "actions",
        accessible_label: None,
        padding: (0.0, 0.0),
        // The native context menu is centered over the instance list at both sizes.
        bounds: Some(RegionBounds::Centered {
            left: -84.0,
            top: 288.0,
            width: 304.0,
            height: 404.0,
        }),
    },
];

const INSTANCE_LIST_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: (0.0, 0.0),
    bounds: None,
}];

const INSTANCE_ORGANIZATION_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "inspector",
        accessible_label: None,
        padding: (0.0, 0.0),
        // The inspector is top-aligned against the right edge at both sizes.
        bounds: Some(RegionBounds::RightAligned {
            right: 0.0,
            top: 78.0,
            width: 360.0,
            height: 478.0,
        }),
    },
];

const ACCOUNT_LAUNCH_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: (0.0, 0.0),
        bounds: None,
    },
    Region {
        name: "account-menu",
        accessible_label: None,
        padding: (0.0, 0.0),
        // The native account menu stays against the right edge at both sizes.
        bounds: Some(RegionBounds::RightAligned {
            right: 0.0,
            top: 538.0,
            width: 300.0,
            height: 272.0,
        }),
    },
];

pub(crate) const STATES: &[State] = &[
    State {
        name: "instances-overview",
        show: fixtures::instances_overview,
        regions: INSTANCE_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "servers-overview",
        show: fixtures::servers_overview,
        regions: SERVER_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "accounts-settings",
        show: fixtures::accounts_settings,
        regions: ACCOUNT_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-mods",
        show: fixtures::client_mods,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instances-empty",
        show: fixtures::getting_started::instances_empty,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "create-instance-dialog",
        show: fixtures::getting_started::create_instance_dialog,
        regions: CREATE_INSTANCE_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-modpack-ready",
        show: fixtures::getting_started::import_modpack_ready,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-modpack-duplicates",
        show: fixtures::getting_started::import_modpack_duplicates,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-modpack-manual-download",
        show: fixtures::getting_started::import_modpack_manual_download,
        regions: MANUAL_DOWNLOAD_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-modpack-credential-missing",
        show: fixtures::getting_started::import_modpack_credential_missing,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-modpack-resolving",
        show: fixtures::getting_started::import_modpack_resolving,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-modpack-running",
        show: fixtures::in_progress::import_modpack_running,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "activity-import-in-progress",
        show: fixtures::in_progress::activity_import_in_progress,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instance-launching",
        show: fixtures::in_progress::instance_launching,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-starting",
        show: fixtures::in_progress::server_starting,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-running",
        show: fixtures::in_progress::server_running,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "running-page",
        show: fixtures::in_progress::running_page,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "toasts-success-and-error",
        show: fixtures::notices::toasts_success_and_error,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "accounts-device-code",
        show: fixtures::notices::accounts_device_code,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "file-drop-overlay",
        show: fixtures::getting_started::file_drop_overlay,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-overview-linked-server",
        show: fixtures::client_hosts::client_overview_linked_server,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-settings",
        show: fixtures::client_hosts::client_settings,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "snapshots",
        show: fixtures::client_hosts::snapshots,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "snapshot-restore-dialog",
        show: fixtures::client_hosts::snapshot_restore_dialog,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-logs-live",
        show: fixtures::client_hosts::client_logs_live,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-crash-reports",
        show: fixtures::client_hosts::client_crash_reports,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-general",
        show: fixtures::settings::general,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-minecraft-defaults",
        show: fixtures::settings::minecraft_defaults,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-java",
        show: fixtures::settings::java,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-java-runtime",
        show: fixtures::client_hosts::client_settings,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::ScrollTo("Java runtime")),
    },
    State {
        name: "client-java-problem",
        show: fixtures::client_hosts::client_java_problem,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::ScrollTo("Java runtime")),
    },
    State {
        name: "client-java-review",
        show: fixtures::client_hosts::client_java_review,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::ScrollTo("Proposed changes")),
    },
    State {
        name: "client-java-install",
        show: fixtures::client_hosts::client_java_install,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::ScrollTo("Java runtime")),
    },
    State {
        name: "settings-java-remove-pinned",
        show: fixtures::settings::java_remove_pinned,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-providers",
        show: fixtures::settings::providers,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-storage",
        show: fixtures::settings::storage,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-disk-usage",
        show: fixtures::settings::disk_usage,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-manager",
        show: fixtures::mods::mod_manager,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-stand-in",
        show: fixtures::mods::mod_stand_in,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-impact",
        show: fixtures::mods::mod_impact,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-install-plan",
        show: fixtures::mods::mod_install_plan,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-isolation-setup",
        show: fixtures::mods::mod_isolation_setup,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-isolation-round",
        show: fixtures::mods::mod_isolation_round,
        regions: ISOLATION_ROUND_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-diagnostics-result",
        show: fixtures::diagnostics_servers::client_diagnostics_result,
        regions: DIAGNOSTIC_RESULT_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-diagnostics-fix-preview",
        show: fixtures::diagnostics_servers::client_diagnostics_fix_preview,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "diagnostics-working",
        show: fixtures::diagnostics_servers::diagnostics_working,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "client-run-failure-notice",
        show: fixtures::diagnostics_servers::client_run_failure_notice,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-console",
        show: fixtures::diagnostics_servers::server_console,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-settings",
        show: fixtures::diagnostics_servers::server_settings,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-mod-manager",
        show: fixtures::diagnostics_servers::server_mod_manager,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-properties-running",
        show: fixtures::diagnostics_servers::server_properties_running,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-eula-refused",
        show: fixtures::diagnostics_servers::server_eula_refused,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-mods",
        show: fixtures::diagnostics_servers::server_mods,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-mods-curseforge",
        show: fixtures::diagnostics_servers::server_mods_curseforge,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-mod-install-confirm",
        show: fixtures::diagnostics_servers::server_mod_install_confirm,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-diagnostics-install",
        show: fixtures::diagnostics_servers::server_diagnostics_install,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-properties",
        show: fixtures::diagnostics_servers::server_properties,
        regions: SERVER_PROPERTIES_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-crash-notice",
        show: fixtures::diagnostics_servers::server_crash_notice,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-crash-reports",
        show: fixtures::diagnostics_servers::server_crash_reports,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "split-workspace",
        show: fixtures::diagnostics_servers::split_workspace,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instance-make-persistent-dialog",
        show: fixtures::instance_dialogs::instance_make_persistent_dialog,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instance-delete-confirmation",
        show: fixtures::instance_dialogs::instance_delete_confirmation,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instance-change-loader-dialog",
        show: fixtures::instance_dialogs::instance_change_loader_dialog,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "import-link-trust-prompt",
        show: fixtures::instance_dialogs::import_link_trust_prompt,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instance-actions-menu",
        show: fixtures::instance_dialogs::instances_page,
        regions: INSTANCE_MENU_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::RightClick("CaveStone")),
    },
    State {
        name: "snapshot-create-dialog",
        show: fixtures::client_hosts::snapshot_create_dialog,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-update-available",
        show: fixtures::mods::mod_update_available,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-missing-file",
        show: fixtures::mods::mod_missing_file,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-isolation-result",
        show: fixtures::mods::mod_isolation_result,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "mod-isolation-recovery",
        show: fixtures::mods::mod_isolation_recovery,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "create-server-standalone",
        show: fixtures::diagnostics_servers::create_server_standalone,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "create-server-linked",
        show: fixtures::diagnostics_servers::create_server_linked,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-eula-review",
        show: fixtures::diagnostics_servers::server_eula_review,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "server-needs-recovery",
        show: fixtures::diagnostics_servers::server_needs_recovery,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "diagnostic-bundle-selection",
        show: fixtures::diagnostics_servers::diagnostic_bundle_selection,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::Click("Prepare diagnostic bundle")),
    },
    State {
        name: "account-launch-picker",
        show: fixtures::instance_dialogs::account_launch_picker,
        regions: ACCOUNT_LAUNCH_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::Click("Launch as")),
    },
    State {
        name: "instance-organization",
        show: fixtures::organization::instance_organization,
        regions: INSTANCE_ORGANIZATION_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instances-filters",
        show: fixtures::organization::instances_filters,
        regions: INSTANCE_LIST_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "instance-groups-dialog",
        show: fixtures::organization::instance_groups_dialog,
        regions: INSTANCE_LIST_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "settings-server-defaults",
        show: fixtures::settings::settings_server_defaults,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "theme-selector-open",
        show: fixtures::settings::theme_selector_open,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: Some(Interaction::Click("Interface theme")),
    },
    State {
        name: "theme-load-error",
        show: fixtures::settings::theme_load_error,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "theme-problems",
        show: fixtures::settings::theme_problems,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
    State {
        name: "theme-branding",
        show: fixtures::settings::theme_branding,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
        interaction: None,
    },
];

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_diagnosis_region_names_the_recorded_problem_count() {
        let count = crate::fixtures::recorded::diagnostics()
            .files
            .problems
            .len();
        let expected = format!("Problem: {count} recorded files are missing or damaged");
        assert!(STATES.iter().any(|state| {
            state
                .regions
                .iter()
                .any(|region| region.accessible_label == Some(expected.as_str()))
        }));
    }

    #[test]
    fn every_state_renders_a_full_window() {
        let _session = crate::capture::render_session();
        let output = tempfile::tempdir().expect("temporary output");
        let files = crate::capture::generate(&crate::capture::CaptureOptions {
            states: STATES,
            themes: &[crate::capture::ThemeName::Light],
            sizes: &[crate::capture::Dimensions::HD],
            output: output.path(),
            derive_1080: false,
        })
        .expect("render every documented state");
        assert!(files.len() >= STATES.len());
        assert!(files.iter().all(|path| path.is_file()));
    }

    #[test]
    fn every_state_and_region_has_a_unique_safe_name() {
        let mut names = std::collections::HashSet::new();
        for state in STATES {
            assert!(names.insert(state.name));
            assert!(
                state
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            );
            let mut regions = std::collections::HashSet::new();
            for region in state.regions {
                assert!(regions.insert(region.name));
                assert!(
                    region
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
                );
            }
            assert!(regions.contains("full"));
        }
    }
}
