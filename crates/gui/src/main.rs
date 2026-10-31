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

/// The text to hand an already-running launcher, if this launch names a link
/// for it to act on.
///
/// Deliberately the *original* command-line argument, not the address
/// [`Startup::Link`] decoded out of it: the primary instance re-parses
/// whatever arrives on the handoff socket with `LauncherUrl::parse`, which
/// expects a full `ephervex://...` link, not the bare address inside one.
fn handoff_argument<'a>(startup: &Startup, arguments: &'a [String]) -> Option<&'a str> {
    match startup {
        Startup::Link(_) => arguments.first().map(String::as_str),
        _ => None,
    }
}

/// A termination request ends the event loop, so the code after `run` runs for
/// it as it does for a closed window.
fn quit_event_loop_on_termination_request() {
    if let Err(error) = ephervex_gui::termination::quit_on_termination_request(|| {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    }) {
        tracing::warn!("termination requests are not watched: {error}");
    }
}

/// Runs as a runner and exits, when this process was started as one.
///
/// A runner is this executable started to supervise a game or server. It opens
/// no window and reads none of the arguments the application accepts.
fn exit_if_runner() {
    if let Some(status) = ephervex_core::runner::run_if_requested(std::env::args_os()) {
        std::process::exit(i32::from(status));
    }
}

fn main() -> Result<(), slint::PlatformError> {
    exit_if_runner();
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
    let keys = ephervex_gui::binding_capture::BindingCapture::default();
    let main_window = ephervex_gui::window_events::MainWindow::default();
    if let Err(error) = slint::BackendSelector::new()
        .backend_name("winit".to_owned())
        .with_winit_custom_application_handler(
            ephervex_gui::window_events::WindowEventHandlers::new(main_window.clone())
                // First, because it takes keyboard events away while a binding
                // editor is open, and a file drop is not one.
                .with(ephervex_gui::binding_capture::BindingCaptureReader::new(
                    keys.clone(),
                ))
                .with(ephervex_gui::file_drop::FileDropHandler::new(drops.clone())),
        )
        .select()
    {
        tracing::info!(
            %error,
            "dropping a modpack onto the window, and binding a key by pressing it, \
             are unavailable here"
        );
    }

    // Read before the window exists: a refusal should cost a line on stderr
    // rather than a window that opens and then explains itself.
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let startup = startup_from(&arguments);
    if let Startup::Refused(message) = &startup {
        eprintln!("{message}");
        std::process::exit(2);
    }

    // Before the window, because there may be no window to open: a launcher is
    // already running, this process hands it the link, and exits. A scheme
    // handler starts a fresh process for every click, so without this a second
    // link means a second launcher sharing one set of instances.
    let link = match &startup {
        Startup::Link(ephervex_core::LauncherUrl::Import { url, issue_link }) => {
            Some((url.as_str(), issue_link.as_deref()))
        }
        _ => None,
    };
    let primary =
        match ephervex_core::single_instance::start(handoff_argument(&startup, &arguments)) {
            Ok(ephervex_core::SingleInstanceStartup::HandedOff) => {
                tracing::info!("handed over to the launcher that is already running");
                return Ok(());
            }
            Ok(ephervex_core::SingleInstanceStartup::Primary(primary)) => Some(primary),
            // Not fatal. Failing to reach a socket is a reason to be a second
            // window, not a reason to refuse to start: the user asked for a
            // launcher and should get one.
            Err(error) => {
                tracing::warn!(%error, "starting without single-instance handover");
                None
            }
        };

    let ui = ephervex_gui::ui::AppWindow::new()?;
    main_window.attach(&ui);
    ui.set_app_version(ephervex_model::APP_VERSION.into());
    ui.set_license_text(include_str!("../../../LICENSE").into());
    // Build data like the two above: compiled in, the same for every run, and
    // needed before anything a controller does.
    ephervex_gui::controller::show_third_party_licenses(&ui);
    let controller = ephervex_gui::controller::attach(&ui, &keys);
    ephervex_gui::controller::accept_file_drops(&ui, &drops, &controller);
    if let Some((url, issue_link)) = link {
        tracing::info!("opening a modpack a link named");
        ephervex_gui::controller::open_pack_link(&ui, url, issue_link);
    }
    let launcher_name = primary.and_then(|primary| {
        // Delivered onto the event loop: the listener runs on a thread of its
        // own, and a frontend does not touch widgets from one.
        let weak = ui.as_weak();
        primary.listen(move |handover| {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ephervex_gui::controller::raise(&ui);
                    if let ephervex_core::single_instance::Handover::Link(message) = &handover
                        && let Ok(ephervex_core::LauncherUrl::Import { url, issue_link }) =
                            ephervex_core::LauncherUrl::parse(message)
                    {
                        ephervex_gui::controller::open_pack_link(&ui, &url, issue_link.as_deref());
                    }
                }
            });
        })
    });

    quit_event_loop_on_termination_request();

    tracing::info!(
        "starting {} {} desktop interface",
        ephervex_model::APP_NAME,
        ephervex_model::APP_VERSION
    );
    let outcome = ui.run();
    // Stopping servers takes a while with no window, and a launch in that time
    // has to open one rather than hand over to this process.
    if let Some(launcher_name) = launcher_name {
        launcher_name.release();
    }
    // Closing the window and a termination request are both quitting. Servers
    // run under runners of their own, so they are left as they are.
    outcome
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    #[test]
    fn handoff_carries_the_raw_argument_a_running_launcher_can_reparse() {
        let argument = "ephervex://import?url=https%3A%2F%2Fexample.test%2Fpack.mrpack".to_owned();
        let arguments = vec![argument.clone()];
        let startup = startup_from(&arguments);

        let handoff =
            handoff_argument(&startup, &arguments).expect("an import link hands off something");
        assert_eq!(handoff, argument);
        assert_eq!(
            ephervex_core::LauncherUrl::parse(handoff).expect("the handoff round-trips"),
            ephervex_core::LauncherUrl::Import {
                url: "https://example.test/pack.mrpack".to_owned(),
                issue_link: None,
            }
        );
    }

    /// The running launcher re-parses the argument it is handed, so what a link
    /// carries beside the pack has to survive the handoff the same way.
    #[test]
    fn handoff_carries_the_issue_a_link_names() {
        let argument = "ephervex://import?url=https%3A%2F%2Fexample.test%2Fpack.mrpack&issue=https%3A%2F%2Fgithub.com%2Fexample%2Fcavestone%2Fissues%2F42".to_owned();
        let arguments = vec![argument.clone()];
        let startup = startup_from(&arguments);

        let handoff =
            handoff_argument(&startup, &arguments).expect("an import link hands off something");
        assert_eq!(handoff, argument);
        assert_eq!(
            ephervex_core::LauncherUrl::parse(handoff).expect("the handoff round-trips"),
            ephervex_core::LauncherUrl::Import {
                url: "https://example.test/pack.mrpack".to_owned(),
                issue_link: Some("https://github.com/example/cavestone/issues/42".to_owned()),
            }
        );
    }

    #[test]
    fn a_cold_start_with_no_arguments_hands_off_nothing() {
        let arguments: Vec<String> = Vec::new();
        let startup = startup_from(&arguments);
        assert!(handoff_argument(&startup, &arguments).is_none());
    }

    #[test]
    fn a_refused_argument_hands_off_nothing() {
        let arguments = vec!["not-a-launcher-link".to_owned()];
        let startup = startup_from(&arguments);
        assert!(handoff_argument(&startup, &arguments).is_none());
    }
}
