//! The UI states and regions used in documentation.

use ephervex_gui::ui::AppWindow;

use crate::fixtures;

#[derive(Clone, Copy)]
pub(crate) struct Region {
    pub(crate) name: &'static str,
    pub(crate) accessible_label: Option<&'static str>,
    pub(crate) padding: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct State {
    pub(crate) name: &'static str,
    pub(crate) show: fn(&AppWindow),
    pub(crate) regions: &'static [Region],
    /// A 1080p image may be resized from 1440p only after a state is known to
    /// preserve the same layout at both window sizes.
    pub(crate) same_layout_at_both_sizes: bool,
}

const INSTANCE_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: 0.0,
    },
    Region {
        name: "client-card",
        accessible_label: Some("CaveStone"),
        padding: 12.0,
    },
];
const SERVER_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: 0.0,
    },
    Region {
        name: "server-card",
        accessible_label: Some("CaveStone Server"),
        padding: 12.0,
    },
];
const ACCOUNT_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: 0.0,
    },
    Region {
        name: "offline-profile",
        accessible_label: Some("MelanX"),
        padding: 12.0,
    },
];
const FULL_ONLY: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: 0.0,
}];

const MANUAL_DOWNLOAD_REGIONS: &[Region] = &[
    Region {
        name: "full",
        accessible_label: None,
        padding: 0.0,
    },
    Region {
        name: "blocked-file",
        accessible_label: Some("Obsidian Mirror"),
        padding: 0.0,
    },
];

const CLIENT_HOSTS_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: 0.0,
}];

const SETTINGS_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: 0.0,
}];

const MODS_FULL_WINDOW_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: 0.0,
}];

const DIAGNOSTICS_SERVERS_REGIONS: &[Region] = &[Region {
    name: "full",
    accessible_label: None,
    padding: 0.0,
}];

pub(crate) const STATES: &[State] = &[
    State {
        name: "instances-overview",
        show: fixtures::instances_overview,
        regions: INSTANCE_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "servers-overview",
        show: fixtures::servers_overview,
        regions: SERVER_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "accounts-settings",
        show: fixtures::accounts_settings,
        regions: ACCOUNT_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-mods",
        show: fixtures::client_mods,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "instances-empty",
        show: fixtures::getting_started::instances_empty,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "create-instance-dialog",
        show: fixtures::getting_started::create_instance_dialog,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "import-modpack-ready",
        show: fixtures::getting_started::import_modpack_ready,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "import-modpack-duplicates",
        show: fixtures::getting_started::import_modpack_duplicates,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "import-modpack-manual-download",
        show: fixtures::getting_started::import_modpack_manual_download,
        regions: MANUAL_DOWNLOAD_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "import-modpack-credential-missing",
        show: fixtures::getting_started::import_modpack_credential_missing,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "import-modpack-resolving",
        show: fixtures::getting_started::import_modpack_resolving,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "import-modpack-running",
        show: fixtures::in_progress::import_modpack_running,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "activity-import-in-progress",
        show: fixtures::in_progress::activity_import_in_progress,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "instance-launching",
        show: fixtures::in_progress::instance_launching,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "server-starting",
        show: fixtures::in_progress::server_starting,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "server-running",
        show: fixtures::in_progress::server_running,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "running-page",
        show: fixtures::in_progress::running_page,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "toasts-success-and-error",
        show: fixtures::notices::toasts_success_and_error,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "accounts-device-code",
        show: fixtures::notices::accounts_device_code,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "file-drop-overlay",
        show: fixtures::getting_started::file_drop_overlay,
        regions: FULL_ONLY,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-overview-linked-server",
        show: fixtures::client_hosts::client_overview_linked_server,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-settings",
        show: fixtures::client_hosts::client_settings,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "snapshots",
        show: fixtures::client_hosts::snapshots,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "snapshot-restore-dialog",
        show: fixtures::client_hosts::snapshot_restore_dialog,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-logs-live",
        show: fixtures::client_hosts::client_logs_live,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-crash-reports",
        show: fixtures::client_hosts::client_crash_reports,
        regions: CLIENT_HOSTS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "settings-general",
        show: fixtures::settings::general,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "settings-minecraft-defaults",
        show: fixtures::settings::minecraft_defaults,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "settings-java",
        show: fixtures::settings::java,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "settings-providers",
        show: fixtures::settings::providers,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "settings-storage",
        show: fixtures::settings::storage,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "settings-disk-usage",
        show: fixtures::settings::disk_usage,
        regions: SETTINGS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "mod-manager",
        show: fixtures::mods::mod_manager,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "mod-impact",
        show: fixtures::mods::mod_impact,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "mod-install-plan",
        show: fixtures::mods::mod_install_plan,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "mod-isolation-setup",
        show: fixtures::mods::mod_isolation_setup,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "mod-isolation-round",
        show: fixtures::mods::mod_isolation_round,
        regions: MODS_FULL_WINDOW_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-diagnostics-result",
        show: fixtures::diagnostics_servers::client_diagnostics_result,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-diagnostics-fix-preview",
        show: fixtures::diagnostics_servers::client_diagnostics_fix_preview,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "diagnostics-working",
        show: fixtures::diagnostics_servers::diagnostics_working,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "client-run-failure-notice",
        show: fixtures::diagnostics_servers::client_run_failure_notice,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "server-console",
        show: fixtures::diagnostics_servers::server_console,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "server-properties",
        show: fixtures::diagnostics_servers::server_properties,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "server-crash-notice",
        show: fixtures::diagnostics_servers::server_crash_notice,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
    State {
        name: "split-workspace",
        show: fixtures::diagnostics_servers::split_workspace,
        regions: DIAGNOSTICS_SERVERS_REGIONS,
        same_layout_at_both_sizes: false,
    },
];

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

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
