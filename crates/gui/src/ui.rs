//! The desktop interface's generated Slint bindings.
//!
//! The markup lives in standalone `.slint` modules under `crates/gui/ui`, with
//! `app.slint` as the root. `build.rs` compiles it, and this module brings the
//! generated window, globals, and row structs into the crate.

slint::include_modules!();
