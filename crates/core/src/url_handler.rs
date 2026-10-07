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
//! quietly on first run (ADR 0027). Asking means the command line - the desktop
//! application offers no control over this, because registering is part of
//! installing rather than a preference anybody revisits (ADR 0028).
//!
//! # Why `$APPIMAGE` rather than the running executable
//!
//! Inside an AppImage, `current_exe()` points into `/tmp/.mount_XXXXXX/`, a
//! mount that is gone the moment the process exits. An entry built from it is
//! dead on the next boot and names a path that may later belong to something
//! else. `$APPIMAGE` is the artifact's own address and is what survives.
//! `current_exe()` is the fallback, which is the development and
//! distribution-package case, where it is exactly right.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
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

/// Who put the link handler in place, if anybody did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlHandlerSource {
    /// Nothing handles the scheme.
    None,
    /// Ephervex wrote the entry into this user's own applications directory,
    /// which is also the only one it may remove.
    User,
    /// A package manager installed it system-wide. Turning it off means
    /// removing the package, not deleting somebody else's file.
    System,
}

/// Whether Ephervex currently handles its own links, and what it would run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlHandlerStatus {
    /// Where the entry in force came from.
    pub source: UrlHandlerSource,
    /// Whether anything at all handles the scheme.
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
    /// Whether this registration is one Ephervex may remove.
    ///
    /// A frontend shows the switch either way, because what it reports is
    /// whether links work. Only a user-owned entry may be turned off from
    /// inside the application.
    #[must_use]
    pub const fn removable(&self) -> bool {
        matches!(self.source, UrlHandlerSource::User)
    }

    /// Whether the entry names something that is still on disk.
    ///
    /// A registration that points at a moved or deleted artifact is worse than
    /// none: the link resolves, the desktop tries to run it, and nothing
    /// happens.
    #[must_use]
    pub fn command_exists(&self) -> bool {
        self.command.as_deref().is_some_and(command_is_reachable)
    }
}

/// Whether a desktop entry's `Exec` still names something that can run.
fn command_is_reachable(command: &str) -> bool {
    command_is_reachable_in(command, std::env::var_os("PATH").as_deref())
}

/// The same question against an explicit `$PATH`.
///
/// Resolved the way a desktop resolves it: a bare command name is looked up on
/// the search path, which is how the packaged entry is written, and only a path
/// is checked as a path. Treating a bare name as a path made it relative to
/// whatever directory the process started in, which reported every ordinary
/// installation as broken.
///
/// Split from the environment for the reason `system_entry_in` is: the variable
/// is process-wide and, since Rust 1.80, unsafe to set. This workspace denies
/// unsafe code and that rule is not worth bending for a test.
fn command_is_reachable_in(command: &str, search_path: Option<&OsStr>) -> bool {
    let candidate = Path::new(command);
    if candidate.is_absolute() || candidate.components().count() > 1 {
        return candidate.exists();
    }
    search_path.is_some_and(|value| {
        std::env::split_paths(value).any(|directory| directory.join(command).is_file())
    })
}

/// The program a desktop entry runs, without its arguments or URL placeholder.
fn read_exec(entry: &Path) -> Option<String> {
    exec_program(&std::fs::read_to_string(entry).ok()?)
}

/// The program named by the `Exec` key of desktop entry text.
///
/// Reads the value the way a desktop does, so a quoted path is the real path.
fn exec_program(contents: &str) -> Option<String> {
    let value = contents
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))?;
    first_exec_argument(&unescape_desktop_string(value))
}

/// Undoes the string escapes of the Desktop Entry Specification, "Basic format".
fn unescape_desktop_string(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            text.push(character);
            continue;
        }
        match chars.next() {
            Some('s') => text.push(' '),
            Some('n') => text.push('\n'),
            Some('t') => text.push('\t'),
            Some('r') => text.push('\r'),
            Some('\\') | None => text.push('\\'),
            Some(other) => {
                text.push('\\');
                text.push(other);
            }
        }
    }
    text
}

/// The first argument of an unescaped `Exec` value, with quoting and `%%` undone.
///
/// `None` for an unterminated quote or an empty program.
fn first_exec_argument(exec: &str) -> Option<String> {
    let mut argument = String::new();
    let mut quoted = false;
    let mut chars = exec.trim_start_matches([' ', '\t']).chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' => quoted = !quoted,
            '\\' if quoted => match chars.peek() {
                Some(&escaped @ ('"' | '`' | '$' | '\\')) => {
                    argument.push(escaped);
                    chars.next();
                }
                _ => argument.push('\\'),
            },
            '%' if chars.peek() == Some(&'%') => {
                argument.push('%');
                chars.next();
            }
            ' ' | '\t' if !quoted => break,
            _ => argument.push(character),
        }
    }
    (!quoted && !argument.is_empty()).then_some(argument)
}

/// Characters an `Exec` argument may not contain unquoted.
const EXEC_RESERVED: &[char] = &[
    ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')',
    '`',
];

/// One `Exec` argument, quoted when it holds a reserved character.
fn exec_argument(argument: &str) -> String {
    let argument = argument.replace('%', "%%");
    if !argument.contains(EXEC_RESERVED) {
        return argument;
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    for character in argument.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// Applies the string escapes a desktop entry value needs.
///
/// A raw newline would end the line and let a path inject further keys.
fn escape_desktop_string(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\t' => text.push_str("\\t"),
            '\r' => text.push_str("\\r"),
            _ => text.push(character),
        }
    }
    text
}

/// The text of the entry that runs `command` with the link the desktop passes.
///
/// A desktop entry is UTF-8, so a path that is not is refused rather than
/// written lossily into an entry that would run something else.
fn entry_contents(command: &Path) -> CoreResult<String> {
    let program = command
        .to_str()
        .ok_or_else(|| CoreError::UrlHandlerUnavailable {
            detail: format!(
                "{} is not valid UTF-8, which a desktop entry cannot hold; move the launcher to a path that is",
                command.display()
            ),
        })?;
    let exec = escape_desktop_string(&format!("{} %u", exec_argument(program)));
    // `NoDisplay` because this entry answers a link rather than being a second
    // Ephervex in the menu. `%u` is the one URL `startup_from` reads.
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={} link handler\n\
         Exec={exec}\n\
         Icon={LAUNCHER_URL_SCHEME}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         MimeType={};\n",
        ephervex_model::APP_NAME,
        scheme_mime_type(),
    ))
}

/// Which registration is in force, given what was found.
///
/// The user's own entry wins, because that is what `xdg-mime default` records
/// and what a desktop then prefers. Only when there is none does a
/// package-installed one decide whether links work.
///
/// Separate from the lookups so the decision can be tested for what it is: the
/// alternative is mutating `$XDG_DATA_DIRS`, which is process-wide and, since
/// Rust 1.80, unsafe. This workspace denies unsafe code and that rule is not
/// worth bending for a test.
fn status_from(user: PathBuf, system: Option<(PathBuf, String)>) -> UrlHandlerStatus {
    if let Some(command) = read_exec(&user) {
        return UrlHandlerStatus {
            source: UrlHandlerSource::User,
            registered: true,
            entry: user,
            command: Some(command),
        };
    }
    if let Some((entry, command)) = system {
        return UrlHandlerStatus {
            source: UrlHandlerSource::System,
            registered: true,
            entry,
            command: Some(command),
        };
    }
    UrlHandlerStatus {
        source: UrlHandlerSource::None,
        registered: false,
        entry: user,
        command: None,
    }
}

/// A package-installed link handler, if one is present.
fn system_entry() -> Option<(PathBuf, String)> {
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    system_entry_in(&dirs)
}

/// The first link handler found across a `$XDG_DATA_DIRS`-shaped list.
///
/// The default is the one the XDG base directory specification names, so an
/// unset variable is a normal system rather than a reason to report nothing.
fn system_entry_in(dirs: &str) -> Option<(PathBuf, String)> {
    dirs.split(':')
        .filter(|directory| !directory.is_empty())
        .map(|directory| {
            PathBuf::from(directory)
                .join("applications")
                .join(URL_HANDLER_ENTRY)
        })
        .find_map(|entry| read_exec(&entry).map(|command| (entry, command)))
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
        status_from(entry_path(self.paths()), system_entry())
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
        let contents = entry_contents(&command)?;
        std::fs::write(&entry, contents).map_err(|source| CoreError::UrlHandlerUnavailable {
            detail: format!("{} could not be written: {source}", entry.display()),
        })?;

        refresh_desktop_database(self.paths(), &directory);
        set_default_handler(self.paths());

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
        // Only this user's own entry. A package manager's file is not
        // Ephervex's to delete, and removing it would leave the package
        // believing it still owns a file that is gone.
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
        refresh_desktop_database(self.paths(), &self.paths().desktop_entries_dir());
        info!(entry = %entry.display(), "removed the link handler");
        Ok(true)
    }
}

/// Tells the desktop that the applications directory changed.
///
/// Best effort on purpose. The tool is not present on every system, and its
/// absence costs a cache refresh rather than the registration: the entry is
/// written either way, and most desktops pick it up without being told.
fn refresh_desktop_database(paths: &ephervex_storage::Paths, directory: &std::path::Path) {
    run_quietly(paths, "update-desktop-database", &[directory.as_os_str()]);
}

/// Asks the desktop to prefer this entry for the scheme.
fn set_default_handler(paths: &ephervex_storage::Paths) {
    run_quietly(
        paths,
        "xdg-mime",
        &[
            std::ffi::OsStr::new("default"),
            std::ffi::OsStr::new(URL_HANDLER_ENTRY),
            std::ffi::OsStr::new(&scheme_mime_type()),
        ],
    );
}

/// Runs a desktop tool in the desktop session `paths` belongs to.
fn run_quietly(paths: &ephervex_storage::Paths, program: &str, arguments: &[&std::ffi::OsStr]) {
    let output = Command::new(program)
        .args(arguments)
        .envs(paths.desktop_tool_environment())
        .output();
    match output {
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

    /// This user's own half of the answer, with no package-installed entry.
    ///
    /// `App::url_handler_status` reads the real `$XDG_DATA_DIRS`, so on a
    /// machine where Ephervex is installed it reports that machine's handler
    /// and a test about a throwaway root would be answering a different
    /// question. What the system half does is covered on its own, below.
    fn user_status_of(app: &App) -> UrlHandlerStatus {
        status_from(entry_path(app.paths()), None)
    }

    /// The tools rewrite the session's `mimeapps.list`, so a test's registration
    /// must run them inside its own root.
    #[test]
    fn desktop_tools_run_inside_throwaway_roots() {
        let base = tempfile::tempdir().expect("temp dir");
        let paths = Paths::rooted_at(base.path());
        let seen = base.path().join("seen");
        let script = format!(
            "printf '%s\\n%s\\n' \"$HOME\" \"$XDG_CONFIG_HOME\" > '{}'",
            seen.display()
        );
        run_quietly(
            &paths,
            "sh",
            &[std::ffi::OsStr::new("-c"), std::ffi::OsStr::new(&script)],
        );
        let seen = std::fs::read_to_string(seen).expect("the tool ran");
        let lines: Vec<&str> = seen.lines().collect();
        assert_eq!(lines.len(), 2, "{seen:?}");
        for line in lines {
            assert!(
                std::path::Path::new(line).starts_with(base.path()),
                "{line}"
            );
        }
    }

    #[test]
    fn nothing_handles_the_scheme_until_it_is_asked_for() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());

        let status = user_status_of(&app);

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

    /// The Debian package installs its own entry, so links work without anybody
    /// asking. The switch has to report that truthfully, and must not offer to
    /// delete a file a package manager owns.
    #[test]
    fn a_package_installed_handler_is_reported_but_is_not_ours_to_remove() {
        let base = tempfile::tempdir().expect("temp dir");
        let share = base.path().join("usr/share");
        let applications = share.join("applications");
        std::fs::create_dir_all(&applications).expect("mkdir");
        std::fs::write(
            applications.join(URL_HANDLER_ENTRY),
            "[Desktop Entry]\nExec=/usr/bin/ephervex-gui %u\n",
        )
        .expect("entry");

        let found = system_entry_in(&share.to_string_lossy()).expect("the package entry is found");
        let status = status_from(base.path().join("never-written.desktop"), Some(found));

        assert!(status.registered, "links work, so the switch says so");
        assert_eq!(status.source, UrlHandlerSource::System);
        assert!(
            !status.removable(),
            "a package manager's file is not Ephervex's to delete"
        );
        assert_eq!(status.command.as_deref(), Some("/usr/bin/ephervex-gui"));
    }

    /// Registering for this user takes precedence, because that is what
    /// `xdg-mime default` records and what the desktop then prefers.
    #[test]
    fn a_users_own_registration_wins_over_a_package_installed_one() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let user = app.register_url_handler().expect("register").entry;

        let status = status_from(
            user,
            Some((
                PathBuf::from("/usr/share/applications").join(URL_HANDLER_ENTRY),
                "/usr/bin/ephervex-gui".to_owned(),
            )),
        );

        assert_eq!(status.source, UrlHandlerSource::User);
        assert!(status.removable());
    }

    /// The packaged entry names a bare `ephervex-gui`, which a desktop resolves
    /// on `PATH`. Checked as a path it would be read relative to whatever
    /// directory the process happened to start in, and every ordinary
    /// installation would report itself as broken.
    #[test]
    fn a_bare_command_name_is_looked_for_on_the_search_path() {
        let base = tempfile::tempdir().expect("temp dir");
        let bin = base.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join(DESKTOP_BINARY), b"gui").expect("gui");
        let search_path = bin.as_os_str();

        assert!(
            command_is_reachable_in(DESKTOP_BINARY, Some(search_path)),
            "a command on the search path is one the desktop can run"
        );
        assert!(
            !command_is_reachable_in("ephervex-not-installed", Some(search_path)),
            "and one that is nowhere on it is still reported missing"
        );
        assert!(
            !command_is_reachable_in(DESKTOP_BINARY, None),
            "with no search path there is nowhere for a bare name to be"
        );
    }

    /// A path is still checked as a path, which is what an AppImage's entry
    /// records and the case the warning was written for.
    #[test]
    fn a_recorded_path_is_checked_where_it_points() {
        let base = tempfile::tempdir().expect("temp dir");
        let artifact = base.path().join("ephervex.appimage");
        std::fs::write(&artifact, b"appimage").expect("artifact");

        assert!(command_is_reachable_in(&artifact.to_string_lossy(), None));
        std::fs::remove_file(&artifact).expect("remove");
        assert!(
            !command_is_reachable_in(&artifact.to_string_lossy(), None),
            "a moved or deleted artifact is the failure this reports"
        );
    }

    /// The program an entry written for `path` makes the status report.
    fn program_written_for(path: &Path) -> Option<String> {
        exec_program(&entry_contents(path).expect("entry text"))
    }

    #[test]
    fn a_path_with_reserved_characters_is_written_quoted_and_read_back_unchanged() {
        let paths = [
            "/opt/Ephervex/ephervex-gui",
            "/home/me/My Apps/Ephervex.AppImage",
            "/home/me/it's/\"quoted\"/ephervex",
            "/home/me/back\\slash/ephervex",
            "/home/me/$HOME/`id`/ephervex",
            "/home/me/100%/%u/ephervex",
            "/home/me/a;b&c|d>e<f~g*h?i#j(k)/ephervex",
            "/home/me/tab\there/ephervex",
            "/home/me/line\nbreak/ephervex",
            "/home/me/carriage\rreturn/ephervex",
            "/home/me/Ephervex \u{e4}\u{f6}\u{fc}/ephervex",
        ];
        for path in paths {
            assert_eq!(
                program_written_for(Path::new(path)).as_deref(),
                Some(path),
                "{path:?} did not survive being written to a desktop entry"
            );
        }
    }

    #[test]
    fn the_exec_value_follows_the_desktop_entry_quoting_rules() {
        let written = |path: &str| entry_contents(Path::new(path)).expect("entry text");

        assert!(written("/opt/e/ephervex-gui").contains("\nExec=/opt/e/ephervex-gui %u\n"));
        assert!(
            written("/home/me/My Apps/e.AppImage")
                .contains("\nExec=\"/home/me/My Apps/e.AppImage\" %u\n")
        );
        // A literal `%` is `%%`; in a quoted argument `"` and `$` take a
        // backslash, and the string level doubles every backslash again.
        assert!(written("/a/100%/e").contains("\nExec=/a/100%%/e %u\n"));
        assert!(written("/a b/\"$/e").contains("\nExec=\"/a b/\\\\\"\\\\$/e\" %u\n"));
    }

    #[test]
    fn a_path_cannot_add_keys_to_the_entry() {
        let text =
            entry_contents(Path::new("/tmp/x\nExec=/bin/evil\nTerminal=true")).expect("text");

        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Exec="))
                .count(),
            1
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("Terminal="))
                .count(),
            1
        );
        assert!(text.lines().any(|line| line == "Terminal=false"));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused_rather_than_written_lossily() {
        use std::os::unix::ffi::OsStrExt;
        let path = PathBuf::from(OsStr::from_bytes(b"/home/me/\xff/ephervex"));

        let error = entry_contents(&path).expect_err("refused");

        assert!(
            matches!(error, CoreError::UrlHandlerUnavailable { .. }),
            "{error:?}"
        );
    }

    /// Entries other tools wrote are not in Ephervex's own shape.
    #[test]
    fn packaged_and_foreign_exec_values_name_their_program() {
        let program = |exec: &str| exec_program(&format!("[Desktop Entry]\nExec={exec}\n"));

        assert_eq!(program("ephervex-gui %u").as_deref(), Some("ephervex-gui"));
        assert_eq!(
            program("/usr/bin/ephervex-gui").as_deref(),
            Some("/usr/bin/ephervex-gui")
        );
        assert_eq!(
            program("\"/opt/My Apps/e\" --flag %u").as_deref(),
            Some("/opt/My Apps/e")
        );
        assert_eq!(program("env FOO=1 ephervex-gui %u").as_deref(), Some("env"));
        assert_eq!(exec_program("[Desktop Entry]\nName=x\n"), None);
        assert_eq!(program("\"/opt/unterminated %u"), None);
        assert_eq!(program(""), None);
    }

    /// The failure this exists for: the status check has to look at the real
    /// file, not at the quoted text.
    #[test]
    fn the_status_of_an_entry_with_a_spaced_path_reports_the_real_path() {
        let base = tempfile::tempdir().expect("temp dir");
        let artifact = base.path().join("My Apps").join("Ephervex.AppImage");
        std::fs::create_dir_all(artifact.parent().expect("parent")).expect("mkdir");
        std::fs::write(&artifact, b"appimage").expect("artifact");
        let user = base.path().join(URL_HANDLER_ENTRY);
        std::fs::write(&user, entry_contents(&artifact).expect("text")).expect("entry");

        let status = status_from(user, None);

        assert_eq!(
            status.command.as_deref(),
            artifact.to_str(),
            "the status must show the path the desktop would run"
        );
        assert!(status.command_exists());
        std::fs::remove_file(&artifact).expect("remove");
        assert!(
            !status.command_exists(),
            "a moved artifact is still reported"
        );
    }

    #[test]
    fn nothing_installed_anywhere_is_reported_as_nothing() {
        let base = tempfile::tempdir().expect("temp dir");
        let status = status_from(base.path().join("never-written.desktop"), None);

        assert!(!status.registered);
        assert_eq!(status.source, UrlHandlerSource::None);
        assert!(!status.removable());
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
        assert!(!user_status_of(&app).registered);
        assert!(
            !app.unregister_url_handler().expect("unregister again"),
            "removing what is not there is not an error, and is not a removal either"
        );
    }
}
