//! Ephervex desktop GUI.
//!
//! The frontend owns presentation and event routing only. All launcher work is
//! delegated to the application services in `ephervex-core`.

use slint::ComponentHandle as _;

/// What this process was started for, beyond showing a window.
///
/// The desktop application takes no options and never has. The one argument it
/// accepts is a link a scheme handler delivered, and anything else is a
/// mistake worth naming rather than ignoring: a launcher that silently
/// discarded its command line is what made `ephervex-gui pack.mrpack` look
/// like it did nothing.
enum Startup {
    /// Open the window and wait.
    Window,
    /// Open the window and act on this link.
    Link(ephervex_core::LauncherUrl),
    /// Refuse, with something to print.
    Refused(String),
}

fn startup_from(arguments: &[String]) -> Startup {
    match arguments {
        [] => Startup::Window,
        [argument] if ephervex_core::LauncherUrl::looks_like_one(argument) => {
            match ephervex_core::LauncherUrl::parse(argument) {
                Ok(link) => Startup::Link(link),
                Err(error) => Startup::Refused(ephervex_i18n::lookup(error.message_id())),
            }
        }
        _ => Startup::Refused(ephervex_i18n::lookup("gui-takes-no-arguments")),
    }
}

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

    // Read before the window exists: a refusal should cost a line on stderr
    // rather than a window that opens and then explains itself.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let startup = startup_from(&arguments);
    if let Startup::Refused(message) = &startup {
        eprintln!("{message}");
        std::process::exit(2);
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
    if let Startup::Link(ephervex_core::LauncherUrl::Import { url }) = &startup {
        tracing::info!("opening a modpack a link named");
        ephervex_gui::controller::open_pack_link(&controller, url);
    }

    tracing::info!(
        "starting {} {} desktop interface",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION
    );
    ui.run()
}
