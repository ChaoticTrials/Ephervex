//! What each Logs or Console pane is reading.
//!
//! A viewer belongs to a pane, not to a host: two panes on one server can read
//! its console and one of its saved logs, and two panes on two hosts can
//! follow both live logs. The logs themselves stay the backend's; a source two
//! viewers read is streamed once to both.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use ephervex_core::live_diagnostics::LogLevel;
use ephervex_core::{DiagnosticScope, InstalledHost};
use ephervex_i18n::{Arg, lookup_args};
use slint::{ComponentHandle as _, Model as _, ModelRc, SharedString, VecModel};

use super::{
    LiveLogChange, LiveViewerState, build_log_presentation, diagnostic_rows, expand_tabs,
    parse_log_level_name, send, sync_log_chunks, to_i32,
};
use crate::backend::{
    self, Command, DiagnosticContent, DiagnosticData, LiveLogSource, LiveLogUpdate,
};
use crate::ui::{AppWindow, LogView};
use crate::workspace_layout::{PaneId, Surface};

/// What a viewer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogMode {
    /// The log being written: a client's `latest.log`, or a server's output.
    Live,
    /// The host's saved log files, one opened at a time.
    Files,
    /// The host's crash reports, one opened at a time.
    Crashes,
}

impl LogMode {
    const fn id(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Files => "files",
            Self::Crashes => "crashes",
        }
    }

    fn parse(id: &str) -> Option<Self> {
        [Self::Live, Self::Files, Self::Crashes]
            .into_iter()
            .find(|mode| mode.id() == id)
    }

    /// The diagnostic kind the mode lists, as the backend names it.
    const fn listing(self) -> Option<&'static str> {
        match self {
            Self::Live => None,
            Self::Files => Some("log"),
            Self::Crashes => Some("crash-report"),
        }
    }
}

struct Viewer {
    pane: PaneId,
    host: InstalledHost,
    surface: Surface,
    mode: LogMode,
    /// The saved file open in [`LogMode::Files`] or [`LogMode::Crashes`].
    file: String,
    files: ModelRc<crate::ui::DiagnosticRow>,
    /// The crash report open in [`LogMode::Crashes`], read as a plain document.
    report: String,
    live: LiveViewerState,
    wrap: bool,
    follow: bool,
}

impl Viewer {
    fn new(pane: PaneId, host: InstalledHost, surface: Surface) -> Self {
        Self {
            pane,
            host,
            surface,
            mode: LogMode::Live,
            file: String::new(),
            files: ModelRc::default(),
            report: String::new(),
            live: LiveViewerState::default(),
            wrap: false,
            follow: true,
        }
    }

    fn live_source(&self) -> LiveLogSource {
        match self.host {
            InstalledHost::Instance(instance) => LiveLogSource::Client(instance),
            InstalledHost::Server(server) => LiveLogSource::Server(server),
        }
    }

    fn scope(&self) -> DiagnosticScope {
        match self.host {
            InstalledHost::Instance(instance) => DiagnosticScope::Client(instance),
            InstalledHost::Server(server) => DiagnosticScope::Server(server),
        }
    }
}

/// Every pane's viewer, and the window rows they render into.
pub(super) struct LogViews {
    /// Indexed like the window's log views; a free slot is `None`.
    slots: Vec<Option<Viewer>>,
    model: Rc<VecModel<LogView>>,
    commands: Sender<Command>,
}

impl LogViews {
    pub(super) fn new(commands: Sender<Command>) -> Self {
        Self {
            slots: Vec::new(),
            model: Rc::new(VecModel::default()),
            commands,
        }
    }

    pub(super) fn attach(&self, ui: &AppWindow) {
        ui.set_log_views(ModelRc::from(Rc::clone(&self.model)));
    }

    /// The window row a pane's viewer renders into, or -1 for a pane without one.
    pub(super) fn slot_of(&self, pane: PaneId) -> i32 {
        self.slot(pane).map_or(-1, to_i32)
    }

    fn slot(&self, pane: PaneId) -> Option<usize> {
        self.slots
            .iter()
            .position(|viewer| viewer.as_ref().is_some_and(|viewer| viewer.pane == pane))
    }

    fn viewer_mut(&mut self, pane: PaneId) -> Option<(usize, &mut Viewer)> {
        let slot = self.slot(pane)?;
        self.slots[slot].as_mut().map(|viewer| (slot, viewer))
    }

    /// Gives every pane on Logs or Console a viewer on its own host, and lets
    /// every other viewer go.
    ///
    /// A pane that keeps its host and surface keeps its viewer untouched.
    pub(super) fn sync(&mut self, panes: &[(PaneId, InstalledHost, Surface)]) {
        let before = self.sources();
        for slot in 0..self.slots.len() {
            let keep = self.slots[slot].as_ref().is_some_and(|viewer| {
                panes.iter().any(|&(pane, host, surface)| {
                    pane == viewer.pane && host == viewer.host && surface == viewer.surface
                })
            });
            if !keep && self.slots[slot].take().is_some() {
                self.model.set_row_data(slot, LogView::default());
            }
        }
        for &(pane, host, surface) in panes {
            if !surface.reads_log() || self.slot(pane).is_some() {
                continue;
            }
            let viewer = Viewer::new(pane, host, surface);
            let slot = if let Some(free) = self.slots.iter().position(Option::is_none) {
                self.slots[free] = Some(viewer);
                free
            } else {
                self.slots.push(Some(viewer));
                self.model.push(LogView::default());
                self.slots.len() - 1
            };
            self.open_live(slot);
        }
        let after = self.sources();
        for source in before.difference(&after) {
            send(&self.commands, Command::CloseLiveLog(source.clone()));
        }
    }

    /// Every source some viewer reads.
    fn sources(&self) -> HashSet<LiveLogSource> {
        self.slots
            .iter()
            .flatten()
            .filter_map(|viewer| viewer.live.active.clone())
            .collect()
    }

    /// Points a viewer at its host's live log and asks for all of it.
    fn open_live(&mut self, slot: usize) {
        let Some(viewer) = self.slots[slot].as_mut() else {
            return;
        };
        viewer.mode = LogMode::Live;
        viewer.file.clear();
        viewer.report.clear();
        viewer.follow = true;
        let source = viewer.live_source();
        viewer.live.activate(source.clone());
        self.render(slot);
        send(&self.commands, Command::OpenLiveLog(source));
    }

    /// Starts every viewer on `source` over, as a server that was just started
    /// begins a new console.
    pub(super) fn reopen(&mut self, source: &LiveLogSource) {
        let mut reopened = false;
        for slot in 0..self.slots.len() {
            let Some(viewer) = self.slots[slot].as_mut() else {
                continue;
            };
            if viewer.live.active.as_ref() == Some(source) {
                viewer.live.deactivate();
                viewer.live.activate(source.clone());
                viewer.follow = true;
                self.render(slot);
                reopened = true;
            }
        }
        if reopened {
            send(&self.commands, Command::OpenLiveLog(source.clone()));
        }
    }

    /// Takes one update for every viewer on its source.
    pub(super) fn apply(
        &mut self,
        update: &LiveLogUpdate,
        changes: &mut HashMap<usize, LiveLogChange>,
    ) {
        for (slot, viewer) in self.slots.iter_mut().enumerate() {
            let Some(viewer) = viewer else { continue };
            if viewer.live.active.as_ref() != Some(&update.source) {
                continue;
            }
            let change = viewer.live.apply(update.clone());
            let entry = changes.entry(slot).or_insert(LiveLogChange::Nothing);
            *entry = entry.max(change);
        }
    }

    /// Draws what one poll's updates did to each viewer.
    pub(super) fn settle(&mut self, changes: HashMap<usize, LiveLogChange>) {
        for (slot, change) in changes {
            let Some(viewer) = self.slots.get(slot).and_then(Option::as_ref) else {
                continue;
            };
            match change {
                // A saved log opens with following off, because a finished log
                // has nothing to follow; its content is drawn all the same.
                LiveLogChange::Reset => self.render(slot),
                LiveLogChange::Grew if viewer.follow => self.render(slot),
                LiveLogChange::Grew => {
                    let pending = to_i32(viewer.live.pending_lines);
                    self.update_row(slot, |row| row.pending_lines = pending);
                }
                LiveLogChange::Nothing => {}
            }
        }
    }

    fn update_row(&self, slot: usize, change: impl FnOnce(&mut LogView)) {
        if let Some(mut row) = self.model.row_data(slot) {
            change(&mut row);
            self.model.set_row_data(slot, row);
        }
    }

    /// Writes one viewer into its row.
    fn render(&mut self, slot: usize) {
        let Some(viewer) = self.slots.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        let state = &mut viewer.live;
        let presentation = build_log_presentation(state);
        let status = if state.search_query.is_empty() {
            lookup_args(
                "gui-retained-log-records",
                [("count", Arg::from(state.history.len()))],
            )
        } else {
            lookup_args(
                "gui-live-log-search-matches",
                [("count", Arg::from(presentation.match_count))],
            )
        };
        let chunks_changed = sync_log_chunks(&state.chunks, presentation.chunks);
        if chunks_changed {
            // Rows are addressed by index, and the indices just moved. Keeping
            // the highlight would mean highlighting different text than was
            // selected.
            state.selection = None;
        }
        state.pending_lines = 0;
        let mut row = self.model.row_data(slot).unwrap_or_default();
        row.mode = viewer.mode.id().into();
        row.file = viewer.file.as_str().into();
        row.files = viewer.files.clone();
        row.report = viewer.report.as_str().into();
        row.chunks = ModelRc::from(Rc::clone(&state.chunks));
        row.has_content = !state.history.is_empty();
        row.status = status.into();
        row.history_truncated = state.source_truncated || state.history.history_truncated();
        row.pending_lines = 0;
        row.content_columns = to_i32(presentation.content_columns.min(100_000));
        row.query = state.search_query.as_str().into();
        row.wrap_lines = viewer.wrap;
        row.follow = viewer.follow;
        row.trace_enabled = state.filter.is_enabled(LogLevel::Trace);
        row.debug_enabled = state.filter.is_enabled(LogLevel::Debug);
        row.info_enabled = state.filter.is_enabled(LogLevel::Info);
        row.warn_enabled = state.filter.is_enabled(LogLevel::Warn);
        row.error_enabled = state.filter.is_enabled(LogLevel::Error);
        row.fatal_enabled = state.filter.is_enabled(LogLevel::Fatal);
        row.other_enabled = state.filter.is_enabled(LogLevel::Other);
        if chunks_changed {
            row.revision = row.revision.saturating_add(1);
        }
        write_selection(state, &mut row);
        self.model.set_row_data(slot, row);
    }

    /// Puts one viewer's selection on screen, in the reading order the rows expect.
    fn render_selection(&self, slot: usize) {
        let Some(viewer) = self.slots.get(slot).and_then(Option::as_ref) else {
            return;
        };
        self.update_row(slot, |row| write_selection(&viewer.live, row));
    }

    /// Switches a pane's viewer between the live log, saved logs, and crash reports.
    pub(super) fn show(&mut self, pane: PaneId, mode: LogMode) {
        let Some(slot) = self.slot(pane) else {
            return;
        };
        if mode == LogMode::Live {
            self.open_live(slot);
            return;
        }
        let before = self.sources();
        let Some(viewer) = self.slots[slot].as_mut() else {
            return;
        };
        viewer.mode = mode;
        viewer.file.clear();
        viewer.report.clear();
        viewer.files = ModelRc::default();
        viewer.live.deactivate();
        viewer.live.history.clear();
        viewer.live.search_query.clear();
        viewer.live.selection = None;
        let scope = viewer.scope();
        self.render(slot);
        self.close_unread(&before);
        if let Some(kind) = mode.listing().and_then(backend::parse_diagnostic_kind) {
            send(&self.commands, Command::LoadDiagnostics { scope, kind });
        }
    }

    fn close_unread(&self, before: &HashSet<LiveLogSource>) {
        let after = self.sources();
        for source in before.difference(&after) {
            send(&self.commands, Command::CloseLiveLog(source.clone()));
        }
    }

    /// Opens one saved file in a pane's viewer: a log through the filtered
    /// view the live log uses, a crash report as a plain document.
    pub(super) fn open_file(&mut self, pane: PaneId, name: &str) {
        let before = self.sources();
        let Some((slot, viewer)) = self.viewer_mut(pane) else {
            return;
        };
        let scope = viewer.scope();
        name.clone_into(&mut viewer.file);
        match viewer.mode {
            LogMode::Live => {}
            LogMode::Files => {
                let source = LiveLogSource::File {
                    scope,
                    name: name.to_owned(),
                };
                viewer.live.activate(source.clone());
                // A saved log does not grow, so there is nothing to follow.
                // The view still opens at the end, which is where a log is
                // read from.
                viewer.follow = false;
                self.render(slot);
                self.close_unread(&before);
                send(&self.commands, Command::OpenLiveLog(source));
            }
            LogMode::Crashes => {
                viewer.report.clear();
                self.render(slot);
                if let Some(kind) = backend::parse_diagnostic_kind("crash-report") {
                    send(
                        &self.commands,
                        Command::OpenDiagnostic {
                            scope,
                            kind,
                            name: name.to_owned(),
                        },
                    );
                }
            }
        }
    }

    /// Searches the crash report a pane has open.
    pub(super) fn search_report(&self, pane: PaneId, query: &str) {
        let Some(viewer) = self.slot(pane).and_then(|slot| self.slots[slot].as_ref()) else {
            return;
        };
        if viewer.mode != LogMode::Crashes || viewer.file.is_empty() {
            return;
        }
        let scope = viewer.scope();
        if let Some(kind) = backend::parse_diagnostic_kind("crash-report") {
            send(
                &self.commands,
                Command::SearchDiagnostic {
                    scope,
                    kind,
                    name: viewer.file.clone(),
                    query: query.to_owned(),
                },
            );
        }
    }

    /// Opens the folder the pane's current kind of file lives in.
    pub(super) fn open_folder(&self, pane: PaneId) {
        let Some(viewer) = self.slot(pane).and_then(|slot| self.slots[slot].as_ref()) else {
            return;
        };
        let scope = viewer.scope();
        if let Some(kind) = backend::parse_diagnostic_kind(viewer.mode.listing().unwrap_or("log")) {
            send(
                &self.commands,
                Command::OpenDiagnosticFolder { scope, kind },
            );
        }
    }

    /// A host's file listing, for every viewer showing that host's files of that kind.
    pub(super) fn apply_listing(
        &mut self,
        rows: &[DiagnosticData],
        scope: &str,
        scope_id: &str,
        kind: &str,
    ) {
        let listed = ModelRc::new(VecModel::from(diagnostic_rows(rows.to_vec())));
        for slot in 0..self.slots.len() {
            let Some(viewer) = self.slots[slot].as_mut() else {
                continue;
            };
            if viewer.host.kind() != scope
                || viewer.host.id() != scope_id
                || viewer.mode.listing() != Some(kind)
            {
                continue;
            }
            viewer.files = listed.clone();
            self.render(slot);
        }
    }

    /// A crash report's text, for every viewer that has that report open.
    pub(super) fn apply_report(&mut self, content: &DiagnosticContent) {
        let text = expand_tabs(&content.text);
        self.apply_report_text(content, &text);
    }

    /// A report that could not be read, shown where its text would be.
    pub(super) fn apply_report_unavailable(
        &mut self,
        scope: DiagnosticScope,
        name: &str,
        message: &str,
    ) {
        self.show_report(scope, name, message);
    }

    fn apply_report_text(&mut self, content: &DiagnosticContent, text: &str) {
        self.show_report(content.scope, &content.name, text);
    }

    fn show_report(&mut self, scope: DiagnosticScope, name: &str, text: &str) {
        for slot in 0..self.slots.len() {
            let Some(viewer) = self.slots[slot].as_mut() else {
                continue;
            };
            if viewer.mode == LogMode::Crashes && viewer.file == name && viewer.scope() == scope {
                text.clone_into(&mut viewer.report);
                self.render(slot);
            }
        }
    }

    fn with_viewer(&mut self, pane: PaneId, change: impl FnOnce(&mut Viewer)) {
        if let Some((slot, viewer)) = self.viewer_mut(pane) {
            change(viewer);
            self.render(slot);
        }
    }

    pub(super) fn set_level(&mut self, pane: PaneId, level: &str, enabled: bool) {
        self.with_viewer(pane, |viewer| {
            if level == "all" {
                viewer.live.filter.set_all(enabled);
            } else if let Some(level) = parse_log_level_name(level) {
                viewer.live.filter.set(level, enabled);
            }
        });
    }

    pub(super) fn search(&mut self, pane: PaneId, query: &str) {
        self.with_viewer(pane, |viewer| {
            query.clone_into(&mut viewer.live.search_query);
        });
    }

    pub(super) fn set_wrap(&mut self, pane: PaneId, wrap: bool) {
        self.with_viewer(pane, |viewer| viewer.wrap = wrap);
    }

    /// Following resumes by drawing what arrived while it was off; turning it
    /// off only stops counting what is already on screen as new.
    pub(super) fn set_follow(&mut self, pane: PaneId, follow: bool) {
        self.with_viewer(pane, |viewer| viewer.follow = follow);
    }

    pub(super) fn text(&self, pane: PaneId) -> Option<String> {
        let slot = self.slot(pane)?;
        self.slots[slot]
            .as_ref()
            .map(|viewer| viewer.live.history.text())
    }

    pub(super) fn selected_text(&self, pane: PaneId) -> Option<String> {
        let slot = self.slot(pane)?;
        self.slots[slot].as_ref()?.live.selected_text()
    }

    pub(super) fn select_at(&mut self, pane: PaneId, row: usize, column: usize, extend: bool) {
        if let Some((slot, viewer)) = self.viewer_mut(pane) {
            viewer.live.select_at(row, column, extend);
            self.render_selection(slot);
        }
    }

    pub(super) fn select_word_at(&mut self, pane: PaneId, row: usize, column: usize) {
        if let Some((slot, viewer)) = self.viewer_mut(pane) {
            viewer.live.select_word_at(row, column);
            self.render_selection(slot);
        }
    }

    pub(super) fn select_all(&mut self, pane: PaneId) {
        if let Some((slot, viewer)) = self.viewer_mut(pane) {
            viewer.live.select_all();
            self.render_selection(slot);
        }
    }
}

/// Writes a selection in reading order, bumping the revision rows react to:
/// a selection is a call on a text item rather than a binding.
fn write_selection(state: &LiveViewerState, row: &mut LogView) {
    if let Some(selection) = state.selection.filter(|selection| !selection.is_empty()) {
        let (start, end) = selection.ordered();
        row.selection_start_row = to_i32(start.row);
        row.selection_start_offset = to_i32(start.offset);
        row.selection_end_row = to_i32(end.row);
        row.selection_end_offset = to_i32(end.offset);
        row.has_selection = true;
    } else {
        // A press with no drag is not a selection, and must clear the one it
        // replaced rather than leave it highlighted.
        row.selection_start_row = -1;
        row.selection_end_row = -1;
        row.has_selection = false;
    }
    row.selection_revision = row.selection_revision.saturating_add(1);
}

/// The pane callbacks of every log viewer.
pub(super) fn register(ui: &AppWindow, views: &Rc<std::cell::RefCell<LogViews>>) {
    views.borrow().attach(ui);
    let pane = |value: i32| u32::try_from(value).ok().map(PaneId::from_raw);

    let state = Rc::clone(views);
    ui.on_log_show(move |id, mode| {
        if let (Some(id), Some(mode)) = (pane(id), LogMode::parse(&mode)) {
            state.borrow_mut().show(id, mode);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_open_file(move |id, name| {
        if let Some(id) = pane(id) {
            state.borrow_mut().open_file(id, &name);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_search_report(move |id, query| {
        if let Some(id) = pane(id) {
            state.borrow().search_report(id, &query);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_open_folder(move |id| {
        if let Some(id) = pane(id) {
            state.borrow().open_folder(id);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_set_level(move |id, level, enabled| {
        if let Some(id) = pane(id) {
            state.borrow_mut().set_level(id, &level, enabled);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_search(move |id, query| {
        if let Some(id) = pane(id) {
            state.borrow_mut().search(id, &query);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_wrap(move |id, wrap| {
        if let Some(id) = pane(id) {
            state.borrow_mut().set_wrap(id, wrap);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_follow(move |id, follow| {
        if let Some(id) = pane(id) {
            state.borrow_mut().set_follow(id, follow);
        }
    });
    let state = Rc::clone(views);
    let weak = ui.as_weak();
    ui.on_log_copy(move |id| {
        let text = pane(id).and_then(|id| state.borrow().text(id));
        if let (Some(ui), Some(text)) = (weak.upgrade(), text) {
            super::copy_to_clipboard(&ui, text);
        }
    });
    let state = Rc::clone(views);
    let weak = ui.as_weak();
    ui.on_log_copy_selection(move |id| {
        let text = pane(id).and_then(|id| state.borrow().selected_text(id));
        if let (Some(ui), Some(text)) = (weak.upgrade(), text) {
            ui.invoke_copy_text(SharedString::from(text));
        }
    });
    let state = Rc::clone(views);
    ui.on_log_select_at(move |id, row, column, extend| {
        if let (Some(id), Ok(row), Ok(column)) =
            (pane(id), usize::try_from(row), usize::try_from(column))
        {
            state.borrow_mut().select_at(id, row, column, extend);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_select_word_at(move |id, row, column| {
        if let (Some(id), Ok(row), Ok(column)) =
            (pane(id), usize::try_from(row), usize::try_from(column))
        {
            state.borrow_mut().select_word_at(id, row, column);
        }
    });
    let state = Rc::clone(views);
    ui.on_log_select_all(move |id| {
        if let Some(id) = pane(id) {
            state.borrow_mut().select_all(id);
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::mpsc::Receiver;

    use ephervex_core::live_diagnostics::TailedLine;
    use ephervex_model::{InstanceId, ServerId};

    use super::*;
    use crate::backend::LiveLogLine;

    fn views() -> (LogViews, Receiver<Command>) {
        let (sender, commands) = std::sync::mpsc::channel();
        (LogViews::new(sender), commands)
    }

    fn row(views: &LogViews, pane: PaneId) -> LogView {
        let slot = views.slot(pane).expect("the pane has a viewer");
        views.model.row_data(slot).expect("row")
    }

    fn lines(views: &LogViews, pane: PaneId) -> Vec<String> {
        row(views, pane)
            .chunks
            .iter()
            .map(|chunk| chunk.text.to_string())
            .collect()
    }

    fn client_update(instance: InstanceId, text: &str) -> LiveLogUpdate {
        LiveLogUpdate {
            source: LiveLogSource::Client(instance),
            reset: false,
            missing: false,
            source_truncated: false,
            lines: vec![LiveLogLine::Client(TailedLine {
                text: text.to_owned(),
                truncation: None,
            })],
        }
    }

    fn deliver(views: &mut LogViews, update: &LiveLogUpdate) {
        let mut changes = HashMap::new();
        views.apply(update, &mut changes);
        views.settle(changes);
    }

    fn sent(commands: &Receiver<Command>) -> Vec<String> {
        commands
            .try_iter()
            .map(|command| match command {
                Command::OpenLiveLog(source) => format!("open {}", source.kind()),
                Command::CloseLiveLog(source) => format!("close {}", source.kind()),
                Command::LoadDiagnostics { .. } => "list".to_owned(),
                _ => "other".to_owned(),
            })
            .collect()
    }

    const A: PaneId = PaneId::from_raw(1);
    const B: PaneId = PaneId::from_raw(2);

    #[test]
    fn two_panes_follow_two_hosts_and_each_line_lands_only_in_its_own() {
        let (mut views, _) = views();
        let (first, second) = (InstanceId::new(), InstanceId::new());
        views.sync(&[
            (A, InstalledHost::Instance(first), Surface::Logs),
            (B, InstalledHost::Instance(second), Surface::Logs),
        ]);

        deliver(&mut views, &client_update(first, "first client"));
        deliver(&mut views, &client_update(second, "second client"));

        assert_eq!(lines(&views, A), ["first client"]);
        assert_eq!(lines(&views, B), ["second client"]);
    }

    #[test]
    fn two_panes_on_one_host_read_one_stream_with_their_own_filters() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        deliver(
            &mut views,
            &client_update(host, "[12:00:00] [main/INFO]: ready"),
        );
        deliver(
            &mut views,
            &client_update(host, "[12:00:01] [main/ERROR]: broke"),
        );

        views.set_level(A, "info", false);
        views.set_wrap(A, true);
        views.search(B, "ready");

        assert_eq!(lines(&views, A).len(), 1, "A hides INFO");
        assert!(row(&views, A).wrap_lines);
        assert_eq!(lines(&views, B), ["[12:00:00] [main/INFO]: ready"]);
        assert!(row(&views, B).info_enabled, "B keeps its own levels");
        assert!(!row(&views, B).wrap_lines);
        assert_eq!(row(&views, A).query, "", "A keeps its own search");
    }

    #[test]
    fn a_pane_switching_to_saved_files_leaves_the_other_panes_console_live() {
        let (mut views, commands) = views();
        let server = InstalledHost::Server(ServerId::new());
        views.sync(&[(A, server, Surface::Console), (B, server, Surface::Logs)]);
        sent(&commands);

        views.show(B, LogMode::Files);

        assert_eq!(row(&views, A).mode, "live");
        assert_eq!(row(&views, B).mode, "files");
        assert_eq!(
            sent(&commands),
            ["list"],
            "the console still reads the server, so nothing is closed"
        );
    }

    #[test]
    fn a_client_log_stops_only_when_no_pane_reads_it() {
        let (mut views, commands) = views();
        let host = InstalledHost::Instance(InstanceId::new());
        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Logs)]);
        assert_eq!(sent(&commands), ["open client", "open client"]);

        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Overview)]);
        assert!(sent(&commands).is_empty(), "A still reads it");
        assert_eq!(views.slot_of(B), -1);

        views.sync(&[(A, host, Surface::Mods)]);
        assert_eq!(sent(&commands), ["close client"]);
    }

    #[test]
    fn a_pane_that_keeps_its_host_and_surface_keeps_its_viewer() {
        let (mut views, commands) = views();
        let host = InstalledHost::Instance(InstanceId::new());
        views.sync(&[(A, host, Surface::Logs)]);
        views.search(A, "kept");
        sent(&commands);

        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Overview)]);

        assert_eq!(row(&views, A).query, "kept");
        assert!(sent(&commands).is_empty(), "nothing is reopened");
    }

    #[test]
    fn a_crash_report_reaches_only_the_viewers_that_have_it_open() {
        let (mut views, _) = views();
        let (own, other) = (InstanceId::new(), InstanceId::new());
        views.sync(&[
            (A, InstalledHost::Instance(own), Surface::Logs),
            (B, InstalledHost::Instance(other), Surface::Logs),
        ]);
        for pane in [A, B] {
            views.show(pane, LogMode::Crashes);
            views.open_file(pane, "crash.txt");
        }

        views.apply_report(&DiagnosticContent {
            scope: DiagnosticScope::Client(own),
            kind: backend::parse_diagnostic_kind("crash-report").expect("kind"),
            name: "crash.txt".to_owned(),
            title: String::new(),
            path: String::new(),
            text: "java.lang.Throwable".to_owned(),
            truncated: false,
            lossy: false,
            matches: None,
        });

        assert_eq!(row(&views, A).report, "java.lang.Throwable");
        assert_eq!(
            row(&views, B).report,
            "",
            "the same file name on another host is another file"
        );
    }
}
