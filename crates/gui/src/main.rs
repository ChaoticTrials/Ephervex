//! Ephervex desktop GUI.
//!
//! The frontend owns presentation and event routing only. All launcher work is
//! delegated to the application services in `ephervex-core`.

use slint::ComponentHandle as _;

fn main() -> Result<(), slint::PlatformError> {
    let launcher_logs = ephervex_core::Paths::from_environment()
        .ok()
        .map(|paths| paths.launcher_logs_dir());
    ephervex_core::init_tracing(
        ephervex_core::DEFAULT_LOG_DIRECTIVES,
        launcher_logs.as_deref(),
    );

    let ui = ephervex_gui::ui::AppWindow::new()?;
    ui.set_app_version(ephervex_model::APP_VERSION.into());
    ui.set_license_text(include_str!("../../../LICENSE").into());
    let _controller = ephervex_gui::controller::attach(&ui);

    tracing::info!(
        "starting {} {} desktop interface",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION
    );
    ui.run()
}
