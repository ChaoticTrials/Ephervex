//! Slint desktop frontend and presentation models.

pub mod backend;
pub mod binding_capture;
mod branding;
pub mod controller;
pub mod diagnosis_view;
pub mod file_drop;
pub mod java_view;
mod jvm_arguments;
mod memory_input;
pub mod minecraft_defaults;
mod mod_path_key;
pub mod monospace;
mod palette;
pub mod presentation;
mod server_property_form;
pub mod settings_drafts;
pub mod termination;
pub mod theme_files;
// Slint's generated bindings contain framework-owned unsafe blocks, unwraps,
// and undocumented public items. Keep the exceptions on that generated module;
// handwritten GUI Rust stays under the workspace-wide lints.
#[allow(
    unsafe_code,
    unreachable_pub,
    missing_debug_implementations,
    clippy::unwrap_used,
    clippy::panic,
    clippy::todo
)]
pub mod ui;
pub mod window_aspect;
pub mod window_events;
pub mod workspace_layout;

#[cfg(test)]
mod disabled_control_reasons;
#[cfg(test)]
mod test_support;
