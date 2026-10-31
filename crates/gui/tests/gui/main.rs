//! The desktop interface's integration tests, in one binary so the generated
//! interface is linked once rather than once per file.

// The assertions in these tests are `expect` calls on purpose, and the shared
// support module makes them too.
#![allow(clippy::expect_used)]

mod support;

mod about_links;
mod account_actions;
mod binding_editor_keyboard_capture;
mod button_faces;
mod card_menus_stay_with_their_row;
mod card_pointer_targets;
mod case_surface;
mod combo_selection;
mod create_instance_dialog;
mod create_server_dialog;
mod disabled_control_reasons;
mod disk_usage_view;
mod dropdown_entries_show_their_full_text;
mod favorite_versions;
mod home_card_columns;
mod home_running_section;
mod home_sections;
mod home_side_column;
mod host_column;
mod host_diagnostics;
mod host_list;
mod host_list_states;
mod host_page_header;
mod host_picker_selection;
mod host_restart;
mod hover_notes_stay_inside_window;
mod import_issue_link;
mod instance_deletion_dialog;
mod instance_organization;
mod instance_promotion_dialog;
mod issue_link;
mod java_runtime_removal_dialog;
mod keyboard_activation;
mod launcher_log_files;
mod left_column;
mod linked_server_creation_row;
mod linked_target_selection;
mod log_views;
mod long_names_stay_in_their_row;
mod microsoft_sign_in_dialog;
mod minecraft_default_rows_split_columns;
mod minecraft_default_slider;
mod mod_bin_action;
mod mod_browser_row;
mod mod_dependency_impact;
mod mod_isolation_workflow;
mod mod_relations_panel;
mod mod_toggle_state;
mod mod_version_badges;
mod outlined_edges;
mod pack_content_surface;
mod pack_link_dialog;
mod popup_menus;
mod quick_view;
mod responsive_shell;
mod restricted_release_acquisition;
mod scroll_view_shadows;
mod seasonal_theme_switch;
mod segmented_choice_bounds;
mod server_crash_review;
mod server_port_default;
mod settings_card_rows;
mod settings_dependency_warning;
mod settings_links_from_panes;
mod settings_save_buttons;
mod settings_value_lists;
mod split_workspace;
mod storage_maintenance_view;
mod surface_primitives;
mod text_field_long_values;
mod theme_schemes;
mod third_party_license_modal;
mod toast_panels;
mod workspace_actions;

/// A test file with no `mod` line above is never compiled, so it would pass
/// by never running.
#[test]
fn every_test_file_in_this_directory_is_declared_above() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/gui");
    let declared = include_str!("main.rs");
    for entry in std::fs::read_dir(&root).expect("test directory") {
        let path = entry.expect("directory entry").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("file name");
            if name != "main" {
                assert!(
                    declared.contains(&format!("\nmod {name};")),
                    "{name}.rs is not declared in main.rs"
                );
            }
        }
    }
}
