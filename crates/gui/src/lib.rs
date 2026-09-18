//! Slint desktop frontend and presentation models.

pub mod backend;
pub mod controller;
pub mod file_drop;
pub mod minecraft_defaults;
pub mod monospace;
pub mod presentation;
// Slint's generated bindings contain framework-owned unsafe blocks. Keep the
// exception on that generated module; handwritten GUI Rust stays under the
// workspace-wide unsafe-code denial.
#[allow(unsafe_code)]
pub mod ui;
pub mod window_aspect;
pub mod window_events;

#[cfg(test)]
mod test_support;
