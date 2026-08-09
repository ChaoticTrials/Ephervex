//! Ephervex desktop GUI.
//!
//! This binary defines the frontend boundary and does not provide a functional
//! interface. It depends on `ephervex-core` and nothing below it, and it must
//! never depend on `ephervex-cli`.
//!
//! Slint is a dependency so the real graph, including its native backend
//! requirements, is exercised by every build. See ADR 0001.

fn main() {
    // Resolved before anything else so the launcher's own log file exists from
    // the first event. A frontend that cannot resolve its state directory still
    // starts and logs to stderr.
    let launcher_logs = ephervex_core::Paths::from_environment()
        .ok()
        .map(|paths| paths.launcher_logs_dir());
    ephervex_core::init_tracing(
        ephervex_core::DEFAULT_LOG_DIRECTIVES,
        launcher_logs.as_deref(),
    );
    // Touching the Slint runtime keeps the dependency load-bearing so toolchain
    // and backend compatibility remain part of normal builds.
    let _ui_platform_ready = slint::SharedString::from(ephervex_model::APP_NAME);
    tracing::info!(
        "{} {} — GUI is provided by the GUI",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION
    );
    println!("The Ephervex GUI is unavailable in this build. Use the `ephervex` CLI.");
}
