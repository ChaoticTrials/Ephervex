//! Logs and crash reports as an application service.
//!
//! # What a frontend gets, and what it must not have
//!
//! A log browser is a list, a text pane, a search box, and two buttons. None of
//! those needs to know that a client's logs are in `<root>/minecraft/logs`, that
//! a server's are in `<root>/server/logs`, or that the launcher's are under the
//! XDG state root — and a frontend that *did* know would be a frontend that
//! constructs filesystem paths, which is the one thing the boundary exists to
//! prevent (see [`crate`]).
//!
//! So the surface here is deliberately handle-based:
//!
//! * [`App::diagnostics`] lists what exists, newest first, for a
//!   [`DiagnosticScope`] named by id.
//! * Everything afterwards — open, refresh, search, copy the path, open the
//!   containing folder — takes that scope plus the entry's *name*. A name is
//!   resolved against the launcher-owned directory and refused if it is
//!   anything other than one file inside it.
//!
//! Refreshing is deliberately the same call as opening: a log that rotated,
//! shrank, or grew between two looks must produce a new window rather than a
//! patch applied to a stale one.
//!
//! # What this layer adds over `ephervex-storage`
//!
//! The storage crate owns the reading rules — bounded windows, path
//! containment, best-effort summaries — because it owns the layout those rules
//! protect. This layer answers the questions storage cannot: *which* directory
//! belongs to this instance, whether the folder action for it should be offered
//! at all, and which folder-opening path is shared with the rest of the
//! application. The folder actions are the ones instance management already
//! defines rather than a second set that could disagree about when `logs/` may
//! be created.

use std::path::PathBuf;

use ephervex_model::{InstanceId, ServerId};
use ephervex_storage::diagnostics::{
    DEFAULT_VIEW_BYTES, DiagnosticEntry, DiagnosticKind, OwnedDirectory, Refusal, SearchOptions,
    SearchOutcome, TextAnchor, TextView,
};
use ephervex_storage::{ClientFolder, MissingFolder, ServerFolder};
use tracing::instrument;

use crate::app::App;
use crate::error::CoreResult;

/// Whose diagnostics are being inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticScope {
    /// One client instance: the game's `logs/` and `crash-reports/`.
    Client(InstanceId),
    /// One server: the server's own `logs/` and `crash-reports/`.
    ///
    /// The launcher's capture of the server *process* output is a different
    /// thing and stays where it is, behind `App::server_output`: it is a live
    /// console history, not a file the server wrote.
    Server(ServerId),
    /// The launcher itself.
    Launcher,
}

impl DiagnosticScope {
    /// Which kinds of file this scope keeps.
    ///
    /// The launcher writes logs and does not write Minecraft crash reports, so
    /// asking it for one is answered with an empty section rather than with a
    /// directory that would never have anything in it.
    #[must_use]
    pub const fn kinds(self) -> &'static [DiagnosticKind] {
        match self {
            Self::Client(_) | Self::Server(_) => DiagnosticKind::ALL,
            Self::Launcher => &[DiagnosticKind::Log],
        }
    }

    /// Whether this scope keeps files of the given kind.
    #[must_use]
    pub fn keeps(self, kind: DiagnosticKind) -> bool {
        self.kinds().contains(&kind)
    }
}

/// One kind of diagnostic file for one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSection {
    pub kind: DiagnosticKind,
    /// The folder holding these files, or `None` when the folder action is
    /// unavailable — the directory does not exist and creating it would claim
    /// the game produced something it has not.
    pub folder: Option<PathBuf>,
    /// Newest first, with the file name as a stable tie-break.
    pub entries: Vec<DiagnosticEntry>,
}

/// Everything one scope currently offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticCatalog {
    pub scope: DiagnosticScope,
    pub sections: Vec<DiagnosticSection>,
}

impl DiagnosticCatalog {
    /// The section for one kind, if the scope keeps that kind.
    #[must_use]
    pub fn section(&self, kind: DiagnosticKind) -> Option<&DiagnosticSection> {
        self.sections.iter().find(|section| section.kind == kind)
    }
}

/// A window of a file and what a query found in it.
///
/// Both together because a frontend showing search results also shows the text
/// they are offsets into, and reading the file twice would let the two disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSearch {
    pub view: TextView,
    pub outcome: SearchOutcome,
}

/// How much of a file one read returns, when the caller has no opinion.
pub const DEFAULT_DIAGNOSTIC_BYTES: u64 = DEFAULT_VIEW_BYTES;

/// Which file to read, and how much of it.
///
/// One value rather than five arguments because opening, refreshing, and
/// searching all name the same thing: a frontend holds the request its list
/// selection produced and re-submits it, instead of threading the same
/// parameters through three call sites and risking a refresh that reads a
/// different window than the open did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRequest<'a> {
    pub scope: DiagnosticScope,
    pub kind: DiagnosticKind,
    /// The entry name a listing gave out. Never a path.
    pub name: &'a str,
    /// Which end of the file to read, or `None` for the kind's natural end —
    /// the end of a log, the beginning of a crash report.
    pub anchor: Option<TextAnchor>,
    /// How many bytes to read, bounded by `MAX_VIEW_BYTES` however large it is.
    pub budget: u64,
}

impl<'a> DiagnosticRequest<'a> {
    /// A request for one entry, with the defaults a frontend would otherwise
    /// have to invent.
    #[must_use]
    pub const fn new(scope: DiagnosticScope, kind: DiagnosticKind, name: &'a str) -> Self {
        Self {
            scope,
            kind,
            name,
            anchor: None,
            budget: DEFAULT_DIAGNOSTIC_BYTES,
        }
    }

    /// Reads from a specific end of the file.
    #[must_use]
    pub const fn with_anchor(mut self, anchor: TextAnchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    /// Reads a specific number of bytes.
    #[must_use]
    pub const fn with_budget(mut self, budget: u64) -> Self {
        self.budget = budget;
        self
    }
}

impl App {
    /// Lists the logs and crash reports one scope currently has.
    #[instrument(skip(self))]
    pub fn diagnostics(&self, scope: DiagnosticScope) -> CoreResult<DiagnosticCatalog> {
        let sections = scope
            .kinds()
            .iter()
            .map(|kind| self.diagnostic_section(scope, *kind))
            .collect::<CoreResult<Vec<_>>>()?;
        Ok(DiagnosticCatalog { scope, sections })
    }

    /// The entries of one kind, newest first.
    pub fn diagnostic_entries(
        &self,
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    ) -> CoreResult<Vec<DiagnosticEntry>> {
        Ok(self.diagnostic_section(scope, kind)?.entries)
    }

    /// The full path of one entry, for display and for "copy path".
    ///
    /// Resolved rather than joined: a name that is not one file inside the
    /// owned directory has no path to give out.
    pub fn diagnostic_path(
        &self,
        scope: DiagnosticScope,
        kind: DiagnosticKind,
        name: &str,
    ) -> CoreResult<PathBuf> {
        Ok(self.diagnostic_directory(scope, kind)?.resolve(name)?)
    }

    /// Reads a bounded window of one entry.
    ///
    /// Also the refresh: calling it again takes a new window of whatever the
    /// file now contains, including nothing at all if it rotated away.
    ///
    /// `anchor` defaults to the end of a log and the beginning of a crash
    /// report, which is where each one keeps the part worth reading first.
    #[instrument(skip(self))]
    pub fn read_diagnostic(&self, request: &DiagnosticRequest<'_>) -> CoreResult<TextView> {
        let path = self.diagnostic_path(request.scope, request.kind, request.name)?;
        Ok(ephervex_storage::diagnostics::read_view(
            &path,
            request
                .anchor
                .unwrap_or_else(|| request.kind.natural_anchor()),
            request.budget,
        )?)
    }

    /// Searches a window of one entry.
    ///
    /// Reads the file and searches the text it returned; nothing is written,
    /// and the file the user is looking at is not modified in any way.
    pub fn search_diagnostic(
        &self,
        request: &DiagnosticRequest<'_>,
        query: &str,
        options: &SearchOptions,
    ) -> CoreResult<DiagnosticSearch> {
        let view = self.read_diagnostic(request)?;
        let outcome = ephervex_storage::diagnostics::search(&view.text, query, options);
        Ok(DiagnosticSearch { view, outcome })
    }

    /// Where the folder holding one kind of file is, or `None` when the action
    /// is unavailable.
    pub fn diagnostic_folder(
        &self,
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    ) -> CoreResult<Option<PathBuf>> {
        match scope {
            DiagnosticScope::Client(instance) => self.client_folder(instance, client_folder(kind)),
            DiagnosticScope::Server(server) => self.server_folder(server, server_folder(kind)),
            DiagnosticScope::Launcher => {
                if !scope.keeps(kind) {
                    return Ok(None);
                }
                // The launcher owns this directory and creates it at startup,
                // so an absent one is worth creating rather than disabling.
                Ok(ephervex_storage::folders::resolve(
                    self.paths().launcher_logs_dir(),
                    MissingFolder::Create,
                )?)
            }
        }
    }

    /// Hands the folder holding one kind of file to the desktop's file manager.
    ///
    /// The same action instance management already offers, reached from the log
    /// browser so a user reading a crash report can get to its directory
    /// without going back to the instance screen.
    #[instrument(skip(self))]
    pub fn open_diagnostic_folder(
        &self,
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    ) -> CoreResult<Option<PathBuf>> {
        let Some(path) = self.diagnostic_folder(scope, kind)? else {
            return Ok(None);
        };
        crate::instance::open_folder(&path);
        Ok(Some(path))
    }

    fn diagnostic_section(
        &self,
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    ) -> CoreResult<DiagnosticSection> {
        if !scope.keeps(kind) {
            return Ok(DiagnosticSection {
                kind,
                folder: None,
                entries: Vec::new(),
            });
        }
        let entries = self.diagnostic_directory(scope, kind)?.entries()?;
        Ok(DiagnosticSection {
            kind,
            folder: self.diagnostic_folder(scope, kind)?,
            entries,
        })
    }

    /// The launcher-owned directory one scope keeps a kind of file in.
    fn diagnostic_directory(
        &self,
        scope: DiagnosticScope,
        kind: DiagnosticKind,
    ) -> CoreResult<OwnedDirectory> {
        if !scope.keeps(kind) {
            return Err(ephervex_storage::DiagnosticError::Refused {
                path: self.scope_root(scope)?,
                reason: Refusal::NotOfferedHere,
            }
            .into());
        }
        let root = self.scope_root(scope)?;
        let directory = match scope {
            DiagnosticScope::Client(_) => client_folder(kind).path(&root),
            DiagnosticScope::Server(_) => server_folder(kind).path(&root),
            DiagnosticScope::Launcher => self.paths().launcher_logs_dir(),
        };
        Ok(OwnedDirectory::new(root, directory, kind))
    }

    /// The root a scope's diagnostics must stay inside.
    fn scope_root(&self, scope: DiagnosticScope) -> CoreResult<PathBuf> {
        Ok(match scope {
            DiagnosticScope::Client(instance) => self.client(instance)?.root,
            DiagnosticScope::Server(server) => self.server(server)?.root,
            // The state root rather than the log directory itself: the launcher
            // log directory is the thing being checked, and a directory cannot
            // vouch for itself.
            DiagnosticScope::Launcher => self.paths().state_dir().to_path_buf(),
        })
    }
}

/// The instance folder action a kind of file corresponds to.
const fn client_folder(kind: DiagnosticKind) -> ClientFolder {
    match kind {
        DiagnosticKind::Log => ClientFolder::Logs,
        DiagnosticKind::CrashReport => ClientFolder::CrashReports,
    }
}

/// The server folder action a kind of file corresponds to.
const fn server_folder(kind: DiagnosticKind) -> ServerFolder {
    match kind {
        DiagnosticKind::Log => ServerFolder::Logs,
        DiagnosticKind::CrashReport => ServerFolder::CrashReports,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use std::path::Path;

    use ephervex_model::LoaderSelection;
    use ephervex_storage::{NewClient, Paths};

    use super::*;

    const CRASH_REPORT: &str = "---- Minecraft Crash Report ----\n\
         // I let you down. Sorry :(\n\
         \n\
         Time: 2026-08-09 10:30:00\n\
         Description: Ticking entity\n\
         \n\
         java.lang.NullPointerException: Ticking entity\n\
         \tat net.minecraft.world.entity.Entity.tick(Entity.java:100)\n";

    fn app_rooted_at(base: &Path) -> App {
        App::open_at(Paths::rooted_at(base)).expect("app opens")
    }

    fn client(app: &App) -> ephervex_storage::ClientRecord {
        app.create_client(NewClient::new(
            "Repro",
            "1.20.1",
            LoaderSelection::vanilla(),
        ))
        .expect("create")
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn a_client_with_no_logs_lists_nothing_and_offers_no_folder() {
        // An instance that has never been launched is a normal state to render.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = client(&app);

        let catalog = app
            .diagnostics(DiagnosticScope::Client(record.id))
            .expect("catalog");
        assert_eq!(catalog.sections.len(), 2);
        for section in &catalog.sections {
            assert!(section.entries.is_empty());
            assert_eq!(
                section.folder, None,
                "creating {} would claim the game had run",
                section.kind
            );
        }
    }

    #[test]
    fn client_logs_and_crash_reports_are_discovered_where_minecraft_writes_them() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = client(&app);
        let game = ephervex_storage::metadata::client_game_dir(&record.root);

        write(&game.join("logs").join("latest.log"), "[main] hello\n");
        write(&game.join("logs").join("debug.log"), "[debug] hello\n");
        write(
            &game
                .join("crash-reports")
                .join("crash-2026-08-09-client.txt"),
            CRASH_REPORT,
        );

        let scope = DiagnosticScope::Client(record.id);
        let catalog = app.diagnostics(scope).expect("catalog");

        let logs = catalog.section(DiagnosticKind::Log).expect("a log section");
        let names: Vec<&str> = logs
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(names.contains(&"latest.log"), "{names:?}");
        assert!(names.contains(&"debug.log"), "{names:?}");
        assert!(logs.folder.is_some(), "the folder action is available now");

        let crashes = catalog
            .section(DiagnosticKind::CrashReport)
            .expect("a crash section");
        let report = crashes.entries.first().expect("a crash report");
        let summary = report.summary.as_ref().expect("a summary");
        assert_eq!(summary.description.as_deref(), Some("Ticking entity"));
    }

    #[test]
    fn a_log_is_read_refreshed_and_searched_through_its_name_alone() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = client(&app);
        let scope = DiagnosticScope::Client(record.id);
        let path = ephervex_storage::metadata::client_game_dir(&record.root)
            .join("logs")
            .join("latest.log");
        write(&path, "[main] starting\n");

        let request = DiagnosticRequest::new(scope, DiagnosticKind::Log, "latest.log");
        let view = app.read_diagnostic(&request).expect("read");
        assert_eq!(view.text, "[main] starting\n");
        assert_eq!(view.anchor, TextAnchor::Tail, "a log is read from its end");

        // The game keeps writing; a refresh is the same call again.
        write(&path, "[main] starting\n[main] Exception in tick\n");
        let refreshed = app.read_diagnostic(&request).expect("refresh");
        assert!(refreshed.text.contains("Exception"));

        let found = app
            .search_diagnostic(&request, "exception", &SearchOptions::default())
            .expect("search");
        assert_eq!(found.outcome.matches.len(), 1);
        assert_eq!(found.outcome.matches[0].line_number, 2);

        // The path is available for "copy path" without the caller building it.
        assert_eq!(
            app.diagnostic_path(scope, DiagnosticKind::Log, "latest.log")
                .expect("path")
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some("latest.log")
        );

        // Searching does not modify the file it searched.
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[main] starting\n[main] Exception in tick\n"
        );
    }

    #[test]
    fn a_name_that_leaves_the_owned_directory_is_refused() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = client(&app);
        let scope = DiagnosticScope::Client(record.id);
        write(
            &ephervex_storage::metadata::client_game_dir(&record.root)
                .join("logs")
                .join("latest.log"),
            "[main] hello\n",
        );
        let secret = base.path().join("secrets.log");
        write(&secret, "a refresh token\n");

        for name in ["../../../secrets.log", "..", "/etc/passwd"] {
            let error = app
                .read_diagnostic(&DiagnosticRequest::new(scope, DiagnosticKind::Log, name))
                .expect_err("must be refused");
            assert_eq!(error.message_id(), "error-diagnostics-file-refused");
        }
    }

    #[test]
    fn the_launcher_keeps_logs_and_never_claims_to_keep_crash_reports() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        write(
            &app.paths().launcher_logs_dir().join("launcher-1.log"),
            "started\n",
        );

        let catalog = app.diagnostics(DiagnosticScope::Launcher).expect("catalog");
        assert_eq!(catalog.sections.len(), 1);
        let logs = catalog.section(DiagnosticKind::Log).expect("a log section");
        assert_eq!(logs.entries.len(), 1);
        assert!(logs.folder.is_some(), "the launcher owns this directory");

        assert!(!DiagnosticScope::Launcher.keeps(DiagnosticKind::CrashReport));
        let error = app
            .read_diagnostic(&DiagnosticRequest::new(
                DiagnosticScope::Launcher,
                DiagnosticKind::CrashReport,
                "launcher-1.log",
            ))
            .expect_err("the launcher writes no crash reports");
        assert_eq!(error.message_id(), "error-diagnostics-file-refused");
    }

    #[test]
    fn a_servers_own_files_and_its_captured_process_output_are_both_reachable() {
        // Two different things that must not be confused: the files the server
        // wrote into its own directory, and the launcher's capture of what the
        // process printed. The capture keeps the home it already had.
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let record = app
            .registry_for_services()
            .create_server(ephervex_storage::NewServer::new(
                "Repro server",
                "1.20.1",
                LoaderSelection::vanilla(),
            ))
            .expect("scaffold");
        let server = ephervex_storage::metadata::server_game_dir(&record.root);

        write(
            &server.join("logs").join("latest.log"),
            "[Server thread/INFO]: Done (5.0s)!\n",
        );
        write(
            &server.join("crash-reports").join("crash-server.txt"),
            CRASH_REPORT,
        );

        let catalog = app
            .diagnostics(DiagnosticScope::Server(record.id))
            .expect("catalog");
        assert_eq!(
            catalog
                .section(DiagnosticKind::Log)
                .expect("logs")
                .entries
                .len(),
            1
        );
        assert_eq!(
            catalog
                .section(DiagnosticKind::CrashReport)
                .expect("crash reports")
                .entries
                .len(),
            1
        );

        // The process capture is untouched by any of this and still readable.
        let captured = ephervex_server::CapturedLine {
            timestamp: chrono::Utc::now(),
            stream: ephervex_server::CaptureStream::Stdout,
            text: "[Server thread/INFO]: Done (5.0s)!".to_owned(),
            truncated: false,
            omitted_bytes: 0,
        };
        write(
            &app.paths()
                .server_capture_dir(record.id)
                .join("capture-0001.jsonl"),
            &format!("{}\n", serde_json::to_string(&captured).expect("serialize")),
        );
        let snapshot = app.server_output(record.id, 100).expect("output");
        assert_eq!(snapshot.lines.len(), 1);
        assert_eq!(snapshot.lines[0].text, captured.text);
    }

    #[test]
    fn diagnostics_of_an_unknown_instance_report_the_instance_rather_than_a_path() {
        let base = tempfile::tempdir().expect("temp dir");
        let app = app_rooted_at(base.path());
        let error = app
            .diagnostics(DiagnosticScope::Client(InstanceId::new()))
            .expect_err("no such instance");
        assert_eq!(error.message_id(), "error-registry-instance-unknown");
    }
}
