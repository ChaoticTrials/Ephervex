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

    // Before any install: a session that starts this from a desktop entry gives
    // it a far lower limit than the same session's terminal does.
    match ephervex_core::open_files::raise_limit() {
        Ok(limit) => tracing::debug!(limit, "open file limit"),
        Err(error) => tracing::warn!(%error, "could not raise the open file limit"),
    }

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
    // Before anything is drawn, because the console and every other technical
    // view asks for this family on its first frame.
    if let Some(family) = ephervex_gui::monospace::resolve_monospace_family() {
        tracing::debug!(%family, "fixed-width family");
        ui.global::<ephervex_gui::ui::Theme>()
            .set_mono_family(family.into());
    } else {
        // The declared default stands. It resolves to nothing, so technical
        // text renders in the fallback face rather than not at all.
        tracing::info!("no fixed-width font family was found on this system");
    }
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
