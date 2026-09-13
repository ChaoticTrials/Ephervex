//! Registering Ephervex as the handler for its own URL scheme.
//!
//! # Why the application registers itself
//!
//! The shipped desktop entry cannot do this. An AppImage is not installed, so
//! nothing puts its `.desktop` file where `xdg-mime` looks unless a separate
//! integration daemon happens to be running. And the shipped entry deliberately
//! declares no `MimeType` and takes no arguments, which is right for the entry
//! that launches the application and wrong for one that handles a link.
//!
//! So Ephervex writes a second entry, of its own, when asked. Only when asked:
//! claiming a system-wide URL scheme is not something a launcher should do
//! quietly on first run (ADR 0027).
//!
//! # Why `$APPIMAGE` rather than the running executable
//!
//! Inside an AppImage, `current_exe()` points into `/tmp/.mount_XXXXXX/`, a
//! mount that is gone the moment the process exits. An entry built from it is
//! dead on the next boot and names a path that may later belong to something
//! else. `$APPIMAGE` is the artifact's own address and is what survives.
//! `current_exe()` is the fallback, which is the development and
//! distribution-package case, where it is exactly right.

use std::path::PathBuf;
use std::process::Command;

use tracing::{info, warn};

use crate::error::{CoreError, CoreResult};
use crate::pack_url::LAUNCHER_URL_SCHEME;

/// The desktop entry Ephervex writes for itself.
///
/// A name of its own rather than the shipped `ephervex.desktop`, so that
/// registering never overwrites a file a package manager owns and
/// unregistering never deletes one.
pub const URL_HANDLER_ENTRY: &str = "ephervex-url-handler.desktop";

/// The MIME type a URL scheme is claimed through.
#[must_use]
pub fn scheme_mime_type() -> String {
    format!("x-scheme-handler/{LAUNCHER_URL_SCHEME}")
}

/// Whether Ephervex currently handles its own links, and what it would run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlHandlerStatus {
    /// Whether an entry written by Ephervex is in place.
    pub registered: bool,
    /// The entry's path, whether or not it exists.
    pub entry: PathBuf,
    /// What the installed entry runs, when there is one.
    ///
    /// Worth showing rather than assuming: an entry written by an AppImage that
    /// has since moved names a path that is no longer there, and that is the
    /// failure this is most likely to have.
    pub command: Option<String>,
}

impl UrlHandlerStatus {
    /// Whether the entry names something that is still on disk.
    ///
    /// A registration that points at a moved or deleted artifact is worse than
    /// none: the link resolves, the desktop tries to run it, and nothing
    /// happens.
    #[must_use]
    pub fn command_exists(&self) -> bool {
        self.command
            .as_deref()
            .map(PathBuf::from)
            .is_some_and(|path| path.exists())
    }
}

/// Where the launcher is, for a desktop entry that has to outlive this process.
fn handler_command() -> CoreResult<PathBuf> {
    let running = std::env::current_exe().map_err(|source| CoreError::UrlHandlerUnavailable {
        detail: format!("the running program's own path is unreadable: {source}"),
    })?;
    Ok(choose_handler_command(
        std::env::var_os("APPIMAGE").map(PathBuf::from),
        running,
    ))
}

/// The desktop application beside a given binary, if there is one.
///
/// A link opens the window, and `ephervex url-handler register` is usually run
/// from the command-line binary, so the running executable is the wrong one to
/// name. They are installed side by side, which is what makes this findable
/// without asking where the launcher was installed.
fn desktop_binary_beside(binary: &std::path::Path) -> Option<PathBuf> {
    let sibling = binary.parent()?.join(DESKTOP_BINARY);
    sibling.is_file().then_some(sibling)
}

/// The desktop application's file name, as both packaging paths install it.
const DESKTOP_BINARY: &str = "ephervex-gui";

/// Which of the two candidate paths a desktop entry should name.
///
/// Split from the environment so the decision can be tested for what it is.
/// Inside an AppImage the running executable is under a `/tmp/.mount_XXXXXX/`
/// path that vanishes with the process, so `$APPIMAGE` wins whenever it names
/// an absolute path. A relative one is not something a desktop entry can use,
/// and falling back is better than writing an entry that cannot resolve.
fn choose_handler_command(appimage: Option<PathBuf>, running: PathBuf) -> PathBuf {
    match appimage {
        // An AppImage dispatches on the name it was invoked under, and the
        // artifact's own name is not `ephervex`, so this already lands in the
        // desktop application. Nothing beside it to look for either: there is
        // one file.
        Some(path) if path.is_absolute() => path,
        Some(path) => {
            warn!(
                path = %path.display(),
                "$APPIMAGE is not an absolute path; falling back to the running executable"
            );
            desktop_binary_beside(&path).unwrap_or(running)
        }
        None => desktop_binary_beside(&running).unwrap_or(running),
    }
}

/// The XDG applications directory a desktop entry belongs in.
///
/// Resolved by `ephervex-storage`, so a test pointed at throwaway roots writes
/// into them rather than into the developer's own desktop.
fn entry_path(paths: &ephervex_storage::Paths) -> PathBuf {
    paths.desktop_entries_dir().join(URL_HANDLER_ENTRY)
}

impl crate::app::App {
    /// Whether Ephervex is currently registered to handle its own links.
    #[must_use]
    pub fn url_handler_status(&self) -> UrlHandlerStatus {
        let entry = entry_path(self.paths());
        let command = std::fs::read_to_string(&entry).ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("Exec="))
                .map(|exec| exec.trim_end_matches(" %u").to_owned())
        });
        UrlHandlerStatus {
            registered: entry.is_file(),
            entry,
            command,
        }
    }

    /// Makes Ephervex the handler for `ephervex://` links.
    ///
    /// Idempotent: registering twice rewrites one entry rather than leaving
    /// two, and re-registering after the artifact moved is how a stale entry is
    /// repaired.
    pub fn register_url_handler(&self) -> CoreResult<UrlHandlerStatus> {
        let command = handler_command()?;
        let directory = self.paths().desktop_entries_dir();
        std::fs::create_dir_all(&directory).map_err(|source| CoreError::UrlHandlerUnavailable {
            detail: format!("{} could not be created: {source}", directory.display()),
        })?;

        let entry = entry_path(self.paths());
        // `NoDisplay` because this entry exists to answer a link, not to be a
        // second Ephervex in the application menu. `%u` is the single URL the
        // desktop passes, which is what `startup_from` in the desktop binary
        // reads.
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={} link handler\n\
             Exec={} %u\n\
             Icon={LAUNCHER_URL_SCHEME}\n\
             Terminal=false\n\
             NoDisplay=true\n\
             MimeType={};\n",
            ephervex_model::APP_NAME,
            command.display(),
            scheme_mime_type(),
        );
        std::fs::write(&entry, contents).map_err(|source| CoreError::UrlHandlerUnavailable {
            detail: format!("{} could not be written: {source}", entry.display()),
        })?;

        refresh_desktop_database(&directory);
        set_default_handler();

        info!(entry = %entry.display(), command = %command.display(), "registered the link handler");
        Ok(self.url_handler_status())
    }

    /// Stops Ephervex handling `ephervex://` links.
    ///
    /// Returns whether anything was registered. Removing the entry is the whole
    /// operation: a scheme whose handler entry is gone is one the desktop no
    /// longer resolves, and leaving a stale entry claiming a scheme nothing
    /// answers is the state this exists to avoid.
    pub fn unregister_url_handler(&self) -> CoreResult<bool> {
        let entry = entry_path(self.paths());
        match std::fs::remove_file(&entry) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => {
                return Err(CoreError::UrlHandlerUnavailable {
                    detail: format!("{} could not be removed: {source}", entry.display()),
                });
            }
        }
        refresh_desktop_database(&self.paths().desktop_entries_dir());
        info!(entry = %entry.display(), "removed the link handler");
        Ok(true)
    }
}

/// Tells the desktop that the applications directory changed.
///
/// Best effort on purpose. The tool is not present on every system, and its
/// absence costs a cache refresh rather than the registration: the entry is
/// written either way, and most desktops pick it up without being told.
fn refresh_desktop_database(directory: &std::path::Path) {
    run_quietly("update-desktop-database", &[directory.as_os_str()]);
}

/// Asks the desktop to prefer this entry for the scheme.
fn set_default_handler() {
    run_quietly(
        "xdg-mime",
        &[
            std::ffi::OsStr::new("default"),
            std::ffi::OsStr::new(URL_HANDLER_ENTRY),
            std::ffi::OsStr::new(&scheme_mime_type()),
        ],
    );
}

fn run_quietly(program: &str, arguments: &[&std::ffi::OsStr]) {
    match Command::new(program).args(arguments).output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => warn!(
            program,
            status = ?output.status,
            "a desktop integration tool reported a failure"
        ),
        Err(error) => {
            info!(program, %error, "a desktop integration tool is not installed");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::app::App;
    use ephervex_storage::Paths;

    /// Pointed at throwaway roots, so a test writes into them rather than into
    /// the developer's own desktop.
    fn app_rooted_at(base: &std::path::Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    #[test]
    fn nothing_handles_the_scheme_until_it_is_asked_for() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let status = app.url_handler_status();

        assert!(
            !status.registered,
            "claiming a system-wide scheme is not something a first run does"
        );
        assert_eq!(status.command, None);
    }

    #[test]
    fn registering_writes_an_entry_that_claims_the_scheme_and_runs_the_launcher() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let status = app.register_url_handler().expect("register");

        assert!(status.registered);
        let text = std::fs::read_to_string(&status.entry).expect("the entry is readable");
        assert!(
            text.contains(&format!("MimeType={};", scheme_mime_type())),
            "the entry has to claim the scheme: {text}"
        );
        assert!(
            text.contains(" %u\n"),
            "the entry has to be handed the link: {text}"
        );
        assert!(
            text.contains("NoDisplay=true"),
            "a link handler is not a second Ephervex in the menu: {text}"
        );
        assert!(
            status.command_exists(),
            "the entry names {:?}, which is not on disk",
            status.command
        );
    }

    /// An AppImage's `current_exe()` points into a mount that disappears when
    /// the process exits, so an entry built from it is dead on the next boot
    /// and may later name a path belonging to something else.
    #[test]
    fn an_appimage_entry_names_the_artifact_rather_than_the_mount_it_runs_from() {
        let mounted = PathBuf::from("/tmp/.mount_ephervAbc123/usr/bin/ephervex-gui");
        let artifact = PathBuf::from("/home/melanx/AppImages/ephervex.appimage");

        assert_eq!(
            choose_handler_command(Some(artifact.clone()), mounted.clone()),
            artifact,
            "an entry built from the mount point is dead on the next boot"
        );
    }

    /// The development and distribution-package case. A link opens a window,
    /// and `url-handler register` is normally run from the command-line
    /// binary, so the running executable is the wrong one to name.
    #[test]
    fn without_an_appimage_the_entry_names_the_desktop_binary_beside_the_running_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let bin = base.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let cli = bin.join("ephervex");
        let gui = bin.join(DESKTOP_BINARY);
        std::fs::write(&cli, b"cli").expect("cli");
        std::fs::write(&gui, b"gui").expect("gui");

        assert_eq!(
            choose_handler_command(None, cli.clone()),
            gui,
            "a link has to open the desktop application, not the command line"
        );

        // Nothing beside it is the only honest answer left.
        std::fs::remove_file(&gui).expect("remove");
        assert_eq!(choose_handler_command(None, cli.clone()), cli);
        assert_eq!(
            choose_handler_command(Some(PathBuf::from("relative/path")), cli.clone()),
            cli,
            "a relative path is not something a desktop entry can resolve"
        );
    }

    /// The entry has to land where the desktop reads, which is a sibling of
    /// Ephervex's own data directory rather than a child of it.
    #[test]
    fn the_entry_is_written_where_the_desktop_looks_for_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let status = app.register_url_handler().expect("register");

        assert_eq!(
            status.entry.parent().expect("parent"),
            base.path().join("applications"),
            "an entry under the application's own data directory is never found"
        );
        assert!(
            !status.entry.starts_with(app.paths().data_dir()),
            "{} is inside Ephervex's data directory",
            status.entry.display()
        );
    }

    #[test]
    fn registering_twice_leaves_one_entry_and_removing_it_leaves_none() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        app.register_url_handler().expect("register");
        let status = app.register_url_handler().expect("register again");
        let directory = status.entry.parent().expect("applications directory");
        // Desktop entries only: `update-desktop-database` leaves a
        // `mimeinfo.cache` of its own beside them, which is its business.
        let mut entries: Vec<String> = std::fs::read_dir(directory)
            .expect("read")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".desktop"))
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![URL_HANDLER_ENTRY.to_owned()],
            "registering twice must not leave two entries claiming one scheme"
        );

        assert!(app.unregister_url_handler().expect("unregister"));
        assert!(!app.url_handler_status().registered);
        assert!(
            !app.unregister_url_handler().expect("unregister again"),
            "removing what is not there is not an error, and is not a removal either"
        );
    }
}
