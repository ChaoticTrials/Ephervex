//! Ephervex desktop GUI.
//!
//! The Slint UI is built in the GUI. Until then this binary holds the frontend
//! boundary open: it depends on `ephervex-core` and nothing below it, and it
//! must never depend on `ephervex-cli`.
//!
//! Slint is already a dependency so the real graph — including its native
//! backend requirements — is exercised by every build rather than discovered in
//! the GUI. See ADR 0001.

fn main() {
    ephervex_core::init_tracing("info");
    // Touching the Slint runtime keeps the dependency load-bearing, so a
    // toolchain or backend regression surfaces here and not in the GUI.
    let _ui_platform_ready = slint::SharedString::from(ephervex_model::APP_NAME);
    tracing::info!(
        "{} {} — GUI is provided by the GUI",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION
    );
    println!("The Ephervex GUI is not implemented yet. Use the `ephervex` CLI.");
}
