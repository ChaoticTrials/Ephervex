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

    // Chosen before the window exists, because that is when a backend can still
    // be given a handler. A display server that cannot deliver file drops, or a
    // build with no windowing backend to configure, simply keeps the file
    // dialog as the only way to name an archive.
    let drops = ephervex_gui::file_drop::FileDrops::default();
    if let Err(error) = slint::BackendSelector::new()
        .backend_name("winit".to_owned())
        .with_winit_custom_application_handler(ephervex_gui::file_drop::FileDropHandler::new(
            drops.clone(),
        ))
        .select()
    {
        tracing::info!(%error, "dropping a modpack onto the window is unavailable here");
    }

    let ui = ephervex_gui::ui::AppWindow::new()?;
    ui.set_app_version(ephervex_model::APP_VERSION.into());
    ui.set_license_text(include_str!("../../../LICENSE").into());
    let controller = ephervex_gui::controller::attach(&ui);
    ephervex_gui::controller::accept_file_drops(&ui, &drops, &controller);

    tracing::info!(
        "starting {} {} desktop interface",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION
    );
    ui.run()
}
