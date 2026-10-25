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
    LiveLogChange, LiveViewerState, LogSelection, SelectionPoint, build_log_presentation,
    diagnostic_rows, expand_tabs, parse_log_level_name, send, sync_log_chunks, to_i32,
};
use crate::backend::{
    self, Command, DiagnosticContent, DiagnosticData, LiveLogGeneration, LiveLogSource,
    LiveLogUpdate,
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
    /// Sources whose run has been started but has written nothing yet. Their
    /// retained output belongs to the run before, so a viewer opened on one
    /// reads none of it and takes the run's lines as they are produced.
    starting: HashSet<LiveLogSource>,
    /// Sources a viewer was opened on while they were starting, whose live log
    /// is opened once the wait ends.
    deferred: HashSet<LiveLogSource>,
    /// The reader the backend was last asked to run for each source in use.
    /// Viewers opened on a source that has one subscribe to it instead of
    /// asking for another, and an update from any other reader is stale.
    readers: HashMap<LiveLogSource, LiveLogGeneration>,
    /// The generation the next requested reader is given.
    next_generation: u64,
}

impl LogViews {
    pub(super) fn new(commands: Sender<Command>) -> Self {
        Self {
            slots: Vec::new(),
            model: Rc::new(VecModel::default()),
            commands,
            starting: HashSet::new(),
            deferred: HashSet::new(),
            readers: HashMap::new(),
            next_generation: 0,
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
        self.close_unread(&before);
    }

    /// Every source some viewer reads.
    fn sources(&self) -> HashSet<LiveLogSource> {
        self.slots
            .iter()
            .flatten()
            .filter_map(|viewer| viewer.live.active.clone())
            .collect()
    }

    /// Points a viewer at its host's live log and shows all of it.
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
        if self.subscribe(slot, &source) {
            return;
        }
        self.render(slot);
        if self.starting.contains(&source) {
            self.deferred.insert(source);
        } else {
            self.request_reader(source);
        }
    }

    /// Gives a viewer the content of the reader its source already has, instead
    /// of restarting that reader for every viewer on it. Says whether it could:
    /// without another viewer holding the content there is nothing to copy.
    fn subscribe(&mut self, slot: usize, source: &LiveLogSource) -> bool {
        if !self.readers.contains_key(source) {
            return false;
        }
        let Some(holder) = self
            .slots
            .iter()
            .enumerate()
            .filter(|&(other, _)| other != slot)
            .find_map(|(_, viewer)| {
                viewer
                    .as_ref()
                    .filter(|viewer| viewer.live.active.as_ref() == Some(source))
            })
        else {
            return false;
        };
        let history = holder.live.history.clone();
        let truncated = holder.live.source_truncated;
        if let Some(viewer) = self.slots[slot].as_mut() {
            viewer.live.history = history;
            viewer.live.source_truncated = truncated;
            viewer.live.pending_lines = 0;
        }
        self.render(slot);
        true
    }

    /// Asks the backend to read `source` from the start, replacing any reader
    /// it had. The new generation is what makes the replaced one's late
    /// updates recognizable.
    fn request_reader(&mut self, source: LiveLogSource) {
        self.next_generation += 1;
        let generation = LiveLogGeneration::from_raw(self.next_generation);
        self.readers.insert(source.clone(), generation);
        send(&self.commands, Command::OpenLiveLog { source, generation });
    }

    /// Opens the live log of a source whose viewers were opened while it was
    /// starting, now that its capture holds the run.
    fn open_deferred(&mut self, source: &LiveLogSource) {
        if self.deferred.remove(source)
            && self
                .slots
                .iter()
                .flatten()
                .any(|viewer| viewer.live.active.as_ref() == Some(source))
        {
            self.request_reader(source.clone());
        }
    }

    /// Starts every viewer on `source` over, as a server that is beginning a
    /// new run begins a new console. Nothing is read back: the run has not
    /// written anything yet, so its lines reach the viewers as they are
    /// produced, including a viewer opened before the run settles.
    pub(super) fn reopen(&mut self, source: &LiveLogSource) {
        self.starting.insert(source.clone());
        for slot in 0..self.slots.len() {
            let Some(viewer) = self.slots[slot].as_mut() else {
                continue;
            };
            if viewer.live.active.as_ref() == Some(source) {
                viewer.live.deactivate();
                viewer.live.activate(source.clone());
                viewer.follow = true;
                self.render(slot);
            }
        }
    }

    /// Ends the wait for a started run's first line: from here on its
    /// capture holds the run, so a viewer opened later may read it.
    ///
    /// A wait that ends before any line arrived leaves the viewers on the
    /// source empty, so the capture is read for all of them.
    pub(super) fn settle_start(&mut self, source: &LiveLogSource) {
        if !self.starting.remove(source) {
            return;
        }
        self.deferred.remove(source);
        if self.sources().contains(source) {
            self.request_reader(source.clone());
        }
    }

    /// Takes one update for every viewer on its source.
    pub(super) fn apply(
        &mut self,
        update: &LiveLogUpdate,
        changes: &mut HashMap<usize, LiveLogChange>,
    ) {
        // A reader that was replaced or closed can still deliver one last
        // update; it must not reach a viewer of the reader that took its place.
        if update
            .generation
            .is_some_and(|generation| self.readers.get(&update.source) != Some(&generation))
        {
            return;
        }
        if self.starting.contains(&update.source) {
            // A whole-source update can only be a snapshot taken before the
            // run began, which is the output being left behind.
            if update.reset {
                return;
            }
            if !update.lines.is_empty() {
                self.starting.remove(&update.source);
                self.open_deferred(&update.source);
            }
        }
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
            if self.slots.get(slot).and_then(Option::as_ref).is_none() {
                continue;
            }
            // New records are drawn whether or not the viewer follows: following
            // decides only where the view scrolls to, and a viewer that is not
            // following counts what arrived below its position as unseen.
            if change != LiveLogChange::Nothing {
                self.draw(slot, change == LiveLogChange::Grew);
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
        self.draw(slot, false);
    }

    /// Writes one viewer into its row. `grown` says the source grew, so a viewer
    /// that is not following counts the rows that appeared as unseen.
    fn draw(&mut self, slot: usize, grown: bool) {
        let Some(viewer) = self.slots.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        let follow = viewer.follow;
        let state = &mut viewer.live;
        if follow {
            // A following view is always at the newest output.
            state.pending_lines = 0;
        }
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
        let change = sync_log_chunks(&state.chunks, presentation.chunks);
        let chunks_changed = change.is_some();
        if let Some(change) = change {
            if grown && !follow {
                // Counted from the rows, because that is what the pane shows:
                // a filter or search hides lines, and a stack trace is one
                // record of many rows.
                state.pending_lines = state.pending_lines.saturating_add(change.new_rows());
            }
            // Rows are addressed by index. A selection among the rows that
            // were kept moves up with them; one that touches a row that was
            // dropped or changed would highlight different text than was
            // selected, so it goes.
            state.selection = state.selection.and_then(|selection| {
                let (start, end) = selection.ordered();
                let kept = start.row >= change.dropped_front
                    && end.row - change.dropped_front < change.first_changed;
                kept.then(|| LogSelection {
                    anchor: SelectionPoint {
                        row: selection.anchor.row - change.dropped_front,
                        ..selection.anchor
                    },
                    focus: SelectionPoint {
                        row: selection.focus.row - change.dropped_front,
                        ..selection.focus
                    },
                })
            });
        }
        let mut row = self.model.row_data(slot).unwrap_or_default();
        row.mode = viewer.mode.id().into();
        row.file = viewer.file.as_str().into();
        row.files = viewer.files.clone();
        row.report = viewer.report.as_str().into();
        row.chunks = ModelRc::from(Rc::clone(&state.chunks));
        row.has_content = !state.history.is_empty();
        row.status = status.into();
        row.history_truncated = state.source_truncated || state.history.history_truncated();
        row.pending_lines = to_i32(state.pending_lines);
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
        let mut counts = [0_usize; 7];
        for record in state.history.records() {
            counts[level_slot(record.level)] += 1;
        }
        let [trace, debug, info, warn, error, fatal, other] = counts.map(to_i32);
        row.trace_count = trace;
        row.debug_count = debug;
        row.info_count = info;
        row.warn_count = warn;
        row.error_count = error;
        row.fatal_count = fatal;
        row.other_count = other;
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

    /// Stops reading every source that was in `before` and no viewer reads now.
    fn close_unread(&mut self, before: &HashSet<LiveLogSource>) {
        let after = self.sources();
        for source in before.difference(&after) {
            self.readers.remove(source);
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
                // A saved file is read afresh on every open, because the file
                // may have grown since another pane read it.
                self.render(slot);
                self.close_unread(&before);
                self.request_reader(source);
            }
            LogMode::Crashes => {
                viewer.report.clear();
                self.render(slot);
                if let Some(kind) = backend::parse_diagnostic_kind("crash-report") {
                    send(
                        &self.commands,
                        Command::OpenDiagnostic {
                            pane,
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
                    pane,
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

    /// A crash report's text, for the pane that asked for it. Another pane
    /// with the same report open keeps its own document and search.
    pub(super) fn apply_report(&mut self, content: &DiagnosticContent) {
        let text = expand_tabs(&content.text);
        self.show_report(content.pane, content.scope, &content.name, &text);
    }

    /// A report that could not be read, shown where its text would be in the
    /// pane that asked.
    pub(super) fn apply_report_unavailable(
        &mut self,
        pane: PaneId,
        scope: DiagnosticScope,
        name: &str,
        message: &str,
    ) {
        self.show_report(pane, scope, name, message);
    }

    /// Drops a reply for a pane that has since moved to another file or mode.
    fn show_report(&mut self, pane: PaneId, scope: DiagnosticScope, name: &str, text: &str) {
        let Some((slot, viewer)) = self.viewer_mut(pane) else {
            return;
        };
        if viewer.mode == LogMode::Crashes && viewer.file == name && viewer.scope() == scope {
            text.clone_into(&mut viewer.report);
            self.render(slot);
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
            // The rows shown changed, so the count of unseen ones no longer
            // refers to them.
            viewer.live.pending_lines = 0;
        });
    }

    pub(super) fn search(&mut self, pane: PaneId, query: &str) {
        self.with_viewer(pane, |viewer| {
            query.clone_into(&mut viewer.live.search_query);
            viewer.live.pending_lines = 0;
        });
    }

    pub(super) fn set_wrap(&mut self, pane: PaneId, wrap: bool) {
        self.with_viewer(pane, |viewer| viewer.wrap = wrap);
    }

    /// Following controls only where the view scrolls to: new records are
    /// drawn either way. Resuming it takes the view to the newest output and
    /// counts everything as seen.
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

/// Where a level's count sits among the seven.
const fn level_slot(level: LogLevel) -> usize {
    match level {
        LogLevel::Trace => 0,
        LogLevel::Debug => 1,
        LogLevel::Info => 2,
        LogLevel::Warn => 3,
        LogLevel::Error => 4,
        LogLevel::Fatal => 5,
        LogLevel::Other => 6,
    }
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
            generation: None,
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
                Command::OpenLiveLog { source, .. } => format!("open {}", source.kind()),
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
    fn a_console_opened_for_a_starting_run_reads_nothing_until_the_run_settles() {
        let (mut views, commands) = views();
        let server = ServerId::new();
        let host = InstalledHost::Server(server);
        let source = LiveLogSource::Server(server);

        views.reopen(&source);
        views.sync(&[(A, host, Surface::Console)]);
        assert!(sent(&commands).is_empty(), "no snapshot of the run before");

        views.settle_start(&source);
        assert_eq!(
            sent(&commands),
            ["open server"],
            "a console opened meanwhile follows the capture once the run settles"
        );
        views.sync(&[(A, host, Surface::Console), (B, host, Surface::Logs)]);
        assert!(
            sent(&commands).is_empty(),
            "a second viewer reads the console that is already open"
        );
    }

    #[test]
    fn a_console_closed_before_its_run_settles_opens_nothing() {
        let (mut views, commands) = views();
        let server = ServerId::new();
        let host = InstalledHost::Server(server);
        let source = LiveLogSource::Server(server);

        views.reopen(&source);
        views.sync(&[(A, host, Surface::Console)]);
        views.sync(&[(A, InstalledHost::Server(ServerId::new()), Surface::Console)]);
        sent(&commands);

        views.settle_start(&source);
        assert!(sent(&commands).is_empty());
    }

    #[test]
    fn a_snapshot_of_the_run_before_is_dropped_while_the_next_run_starts() {
        let (mut views, _commands) = views();
        let server = ServerId::new();
        let host = InstalledHost::Server(server);
        let source = LiveLogSource::Server(server);
        views.sync(&[(A, host, Surface::Console)]);
        views.reopen(&source);
        let update = crate::backend::LiveLogUpdate {
            source: source.clone(),
            generation: None,
            reset: true,
            missing: false,
            source_truncated: false,
            lines: vec![LiveLogLine::Server(
                ephervex_server::CapturedLine::from_bytes(
                    ephervex_server::CaptureStream::Stdout,
                    b"earlier run",
                ),
            )],
        };
        let mut changes = HashMap::new();
        views.apply(&update, &mut changes);
        views.settle(changes);

        assert!(lines(&views, A).is_empty());
        assert!(views.starting.contains(&source), "the wait goes on");
    }

    #[test]
    fn the_first_line_of_a_starting_run_ends_the_wait_for_its_capture() {
        let (mut views, commands) = views();
        let server = ServerId::new();
        let host = InstalledHost::Server(server);
        let source = LiveLogSource::Server(server);
        views.reopen(&source);
        views.sync(&[(A, host, Surface::Console)]);
        let update = crate::backend::LiveLogUpdate {
            source,
            generation: None,
            reset: false,
            missing: false,
            source_truncated: false,
            lines: vec![LiveLogLine::Server(
                ephervex_server::CapturedLine::from_bytes(
                    ephervex_server::CaptureStream::Stdout,
                    b"ready",
                ),
            )],
        };
        let mut changes = HashMap::new();
        views.apply(&update, &mut changes);
        assert_eq!(sent(&commands), ["open server"]);

        views.sync(&[(A, host, Surface::Console), (B, host, Surface::Logs)]);
        assert!(sent(&commands).is_empty());
    }

    #[test]
    fn a_client_log_stops_only_when_no_pane_reads_it() {
        let (mut views, commands) = views();
        let host = InstalledHost::Instance(InstanceId::new());
        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Logs)]);
        assert_eq!(
            sent(&commands),
            ["open client"],
            "the second pane reads the log the first one opened"
        );

        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Overview)]);
        assert!(sent(&commands).is_empty(), "A still reads it");
        assert_eq!(views.slot_of(B), -1);

        views.sync(&[(A, host, Surface::Mods)]);
        assert_eq!(sent(&commands), ["close client"]);
    }

    fn log_line(level: &str, text: &str) -> String {
        format!("[12:00:00] [main/{level}]: {text}")
    }

    /// An update from the reader the views last asked for, as the backend
    /// tags it.
    fn from_reader(views: &LogViews, source: &LiveLogSource, lines: &[String]) -> LiveLogUpdate {
        LiveLogUpdate {
            source: source.clone(),
            generation: views.readers.get(source).copied(),
            reset: false,
            missing: false,
            source_truncated: false,
            lines: lines
                .iter()
                .map(|text| {
                    LiveLogLine::Client(TailedLine {
                        text: text.clone(),
                        truncation: None,
                    })
                })
                .collect(),
        }
    }

    #[test]
    fn a_pane_opened_on_a_log_that_is_already_read_shows_it_whole_without_a_new_reader() {
        let (mut views, commands) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        let mut snapshot = from_reader(
            &views,
            &source,
            &[log_line("INFO", "first"), log_line("INFO", "second")],
        );
        snapshot.reset = true;
        deliver(&mut views, &snapshot);
        let reader = views.readers.get(&source).copied();
        sent(&commands);

        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);

        assert!(sent(&commands).is_empty(), "the reader is not restarted");
        assert_eq!(views.readers.get(&source).copied(), reader);
        assert_eq!(lines(&views, B), lines(&views, A));
        assert_eq!(lines(&views, B).len(), 2);

        let next = from_reader(&views, &source, &[log_line("INFO", "third")]);
        deliver(&mut views, &next);
        assert_eq!(lines(&views, A).len(), 3);
        assert_eq!(lines(&views, B).len(), 3, "every pane receives new records");
    }

    #[test]
    fn a_pane_that_joins_before_the_first_snapshot_arrives_receives_it() {
        let (mut views, commands) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        assert_eq!(sent(&commands), ["open client"]);

        let mut snapshot = from_reader(&views, &source, &[log_line("INFO", "opened")]);
        snapshot.reset = true;
        deliver(&mut views, &snapshot);

        assert_eq!(lines(&views, A).len(), 1);
        assert_eq!(lines(&views, B).len(), 1);
    }

    #[test]
    fn leaving_a_log_with_a_pane_still_on_it_keeps_the_reader_until_the_last_pane_leaves() {
        let (mut views, commands) = views();
        let instance = InstanceId::new();
        let host = InstalledHost::Instance(instance);
        let source = LiveLogSource::Client(instance);
        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Logs)]);
        sent(&commands);

        views.sync(&[(B, host, Surface::Logs)]);
        assert!(sent(&commands).is_empty());
        assert!(views.readers.contains_key(&source));

        views.sync(&[]);
        assert_eq!(sent(&commands), ["close client"]);
        assert!(!views.readers.contains_key(&source));
    }

    #[test]
    fn an_update_from_a_reader_that_was_closed_never_reaches_the_one_that_replaced_it() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        let pane = [(A, InstalledHost::Instance(host), Surface::Logs)];
        views.sync(&pane);
        let retired = from_reader(&views, &source, &[log_line("INFO", "from the old reader")]);

        views.sync(&[]);
        views.sync(&pane);
        assert_ne!(
            views.readers.get(&source).copied(),
            retired.generation,
            "a reader asked for again is a new one"
        );
        deliver(&mut views, &retired);
        assert!(lines(&views, A).is_empty(), "the stale update is dropped");

        let current = from_reader(&views, &source, &[log_line("INFO", "from the new reader")]);
        deliver(&mut views, &current);
        assert_eq!(lines(&views, A), [log_line("INFO", "from the new reader")]);
    }

    #[test]
    fn an_update_from_a_reader_that_was_closed_is_dropped_while_nothing_reads_the_source() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        let retired = from_reader(&views, &source, &[log_line("INFO", "late")]);
        views.sync(&[]);

        let mut changes = HashMap::new();
        views.apply(&retired, &mut changes);

        assert!(changes.is_empty());
    }

    #[test]
    fn new_records_are_drawn_at_once_when_a_pane_is_not_following() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        let mut snapshot = from_reader(&views, &source, &[log_line("INFO", "before")]);
        snapshot.reset = true;
        deliver(&mut views, &snapshot);
        assert_eq!(row(&views, A).pending_lines, 0);

        views.set_follow(A, false);
        let revision = row(&views, A).revision;
        let next = from_reader(
            &views,
            &source,
            &[log_line("INFO", "after"), log_line("WARN", "later")],
        );
        deliver(&mut views, &next);

        assert_eq!(
            lines(&views, A),
            [
                log_line("INFO", "before"),
                log_line("INFO", "after"),
                log_line("WARN", "later"),
            ],
            "the records are in the viewer before it follows again"
        );
        assert!(!row(&views, A).follow);
        assert_eq!(row(&views, A).pending_lines, 2);
        assert!(row(&views, A).revision > revision);

        let more = from_reader(&views, &source, &[log_line("INFO", "and more")]);
        deliver(&mut views, &more);
        assert_eq!(row(&views, A).pending_lines, 3, "the count keeps growing");

        views.set_follow(A, true);
        assert_eq!(row(&views, A).pending_lines, 0);
        assert_eq!(lines(&views, A).len(), 4);
    }

    #[test]
    fn each_level_is_counted_whatever_the_filters_show() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        views.set_level(A, "info", false);

        let next = from_reader(
            &views,
            &source,
            &[
                log_line("INFO", "one"),
                log_line("INFO", "two"),
                log_line("WARN", "careful"),
                log_line("ERROR", "broken"),
            ],
        );
        deliver(&mut views, &next);

        let shown = row(&views, A);
        assert_eq!(
            (shown.info_count, shown.warn_count, shown.error_count),
            (2, 1, 1),
            "a hidden level still has its count"
        );
        assert_eq!(shown.debug_count, 0);
    }

    #[test]
    fn only_records_the_pane_shows_count_as_new_lines() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        views.set_level(A, "info", false);
        views.set_follow(A, false);

        let next = from_reader(
            &views,
            &source,
            &[log_line("INFO", "hidden"), log_line("ERROR", "shown")],
        );
        deliver(&mut views, &next);

        assert_eq!(lines(&views, A), [log_line("ERROR", "shown")]);
        assert_eq!(row(&views, A).pending_lines, 1);
    }

    #[test]
    fn one_pane_following_does_not_change_what_another_pane_counts_as_new() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        views.set_follow(A, false);

        let next = from_reader(&views, &source, &[log_line("INFO", "arrived")]);
        deliver(&mut views, &next);

        assert_eq!(row(&views, A).pending_lines, 1);
        assert_eq!(row(&views, B).pending_lines, 0);
        assert!(row(&views, B).follow);
        assert_eq!(lines(&views, A), lines(&views, B));
    }

    #[test]
    fn a_selection_above_new_records_survives_their_arrival() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        let mut snapshot = from_reader(
            &views,
            &source,
            &[log_line("INFO", "one"), log_line("INFO", "two")],
        );
        snapshot.reset = true;
        deliver(&mut views, &snapshot);
        views.set_follow(A, false);
        views.select_at(A, 0, 0, false);
        views.select_at(A, 1, 4, true);
        assert!(row(&views, A).has_selection);

        let next = from_reader(&views, &source, &[log_line("INFO", "three")]);
        deliver(&mut views, &next);

        assert!(
            row(&views, A).has_selection,
            "records appended below do not move the selected text"
        );
        assert_eq!(lines(&views, A).len(), 3);
    }

    #[test]
    fn a_selection_survives_new_records_pushing_old_ones_out_of_a_full_log() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        let slot = views.slot(A).expect("viewer");
        // A log at its retention cap drops its oldest record with every new one.
        views.slots[slot].as_mut().expect("viewer").live.history =
            ephervex_core::live_diagnostics::RetainedLog::new(3, usize::MAX);
        let mut snapshot = from_reader(
            &views,
            &source,
            &[
                log_line("INFO", "one"),
                log_line("INFO", "two"),
                log_line("INFO", "three"),
            ],
        );
        snapshot.reset = true;
        deliver(&mut views, &snapshot);
        views.set_follow(A, false);
        views.select_at(A, 1, 0, false);
        views.select_at(A, 1, 100, true);
        let selected = views.selected_text(A);
        assert_eq!(selected.as_deref(), Some(log_line("INFO", "two").as_str()));

        let next = from_reader(&views, &source, &[log_line("INFO", "four")]);
        deliver(&mut views, &next);

        assert_eq!(lines(&views, A).len(), 3);
        assert_eq!(
            views.selected_text(A),
            selected,
            "the selection moved up with its text"
        );
        assert_eq!(row(&views, A).pending_lines, 1);

        let more = from_reader(&views, &source, &[log_line("INFO", "five")]);
        deliver(&mut views, &more);
        assert_eq!(
            views.selected_text(A),
            None,
            "the selected record was pushed out"
        );
    }

    #[test]
    fn the_new_line_count_follows_the_rows_a_filtered_stack_trace_adds() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        let source = LiveLogSource::Client(host);
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        views.set_level(A, "all", false);
        views.set_level(A, "error", true);
        views.set_follow(A, false);

        let trace = from_reader(
            &views,
            &source,
            &[
                log_line("ERROR", "boom"),
                "\tat example.Main.run(Main.java:1)".to_owned(),
                log_line("INFO", "unrelated"),
            ],
        );
        deliver(&mut views, &trace);

        assert_eq!(lines(&views, A).len(), 2, "the error and its trace line");
        assert_eq!(row(&views, A).pending_lines, 2);
    }

    #[test]
    fn a_saved_file_opened_in_a_second_pane_is_read_again() {
        let (mut views, commands) = views();
        let host = InstanceId::new();
        let host = InstalledHost::Instance(host);
        views.sync(&[(A, host, Surface::Logs), (B, host, Surface::Logs)]);
        for pane in [A, B] {
            views.show(pane, LogMode::Files);
        }
        sent(&commands);

        views.open_file(A, "latest.log");
        views.open_file(B, "latest.log");

        assert_eq!(
            sent(&commands),
            ["open file", "open file"],
            "the file may have grown since the other pane read it"
        );
    }

    #[test]
    fn a_console_left_empty_by_an_abandoned_start_reads_the_capture_when_the_wait_ends() {
        let (mut views, commands) = views();
        let server = ServerId::new();
        let host = InstalledHost::Server(server);
        let source = LiveLogSource::Server(server);
        views.sync(&[(A, host, Surface::Console)]);
        views.reopen(&source);
        sent(&commands);

        views.settle_start(&source);
        assert_eq!(sent(&commands), ["open server"]);

        views.sync(&[(A, host, Surface::Console), (B, host, Surface::Logs)]);
        assert!(
            sent(&commands).is_empty(),
            "the second pane takes what the first one now holds"
        );
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

    fn report_for(pane: PaneId, host: InstanceId, text: &str) -> DiagnosticContent {
        DiagnosticContent {
            pane,
            scope: DiagnosticScope::Client(host),
            kind: backend::parse_diagnostic_kind("crash-report").expect("kind"),
            name: "crash.txt".to_owned(),
            title: String::new(),
            path: String::new(),
            text: text.to_owned(),
            truncated: false,
            lossy: false,
            matches: None,
        }
    }

    fn open_report_in(views: &mut LogViews, panes: &[PaneId]) {
        for &pane in panes {
            views.show(pane, LogMode::Crashes);
            views.open_file(pane, "crash.txt");
        }
    }

    #[test]
    fn a_crash_report_reaches_only_the_pane_that_asked_for_it() {
        let (mut views, _) = views();
        let (own, other) = (InstanceId::new(), InstanceId::new());
        views.sync(&[
            (A, InstalledHost::Instance(own), Surface::Logs),
            (B, InstalledHost::Instance(other), Surface::Logs),
        ]);
        open_report_in(&mut views, &[A, B]);

        views.apply_report(&report_for(A, own, "java.lang.Throwable"));

        assert_eq!(row(&views, A).report, "java.lang.Throwable");
        assert_eq!(
            row(&views, B).report,
            "",
            "the same file name on another host is another file"
        );
    }

    #[test]
    fn a_search_in_one_pane_leaves_another_pane_on_the_same_report_alone() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        open_report_in(&mut views, &[A, B]);
        views.apply_report(&report_for(A, host, "whole report"));
        views.apply_report(&report_for(B, host, "whole report"));

        views.apply_report(&report_for(A, host, "[1 match]\n\nexcerpt"));

        assert_eq!(row(&views, A).report, "[1 match]\n\nexcerpt");
        assert_eq!(row(&views, B).report, "whole report");
    }

    #[test]
    fn opening_a_report_in_one_pane_keeps_the_search_result_in_another() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        open_report_in(&mut views, &[A, B]);
        views.apply_report(&report_for(A, host, "[1 match]\n\nexcerpt"));

        views.apply_report(&report_for(B, host, "whole report"));

        assert_eq!(row(&views, A).report, "[1 match]\n\nexcerpt");
        assert_eq!(row(&views, B).report, "whole report");
    }

    #[test]
    fn an_unreadable_report_is_shown_only_in_the_pane_that_asked() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        open_report_in(&mut views, &[A, B]);
        views.apply_report(&report_for(B, host, "whole report"));

        views.apply_report_unavailable(A, DiagnosticScope::Client(host), "crash.txt", "gone");

        assert_eq!(row(&views, A).report, "gone");
        assert_eq!(row(&views, B).report, "whole report");
    }

    #[test]
    fn a_reply_for_a_file_the_pane_no_longer_has_open_is_dropped() {
        let (mut views, _) = views();
        let host = InstanceId::new();
        views.sync(&[(A, InstalledHost::Instance(host), Surface::Logs)]);
        open_report_in(&mut views, &[A]);
        views.open_file(A, "other.txt");

        views.apply_report(&report_for(A, host, "late reply for crash.txt"));

        assert_eq!(row(&views, A).report, "");
    }

    #[test]
    fn a_report_search_names_the_pane_that_searched() {
        let (mut views, commands) = views();
        let host = InstanceId::new();
        views.sync(&[
            (A, InstalledHost::Instance(host), Surface::Logs),
            (B, InstalledHost::Instance(host), Surface::Logs),
        ]);
        open_report_in(&mut views, &[A, B]);
        commands.try_iter().for_each(drop);

        views.search_report(B, "needle");

        let panes: Vec<PaneId> = commands
            .try_iter()
            .filter_map(|command| match command {
                Command::SearchDiagnostic { pane, .. } => Some(pane),
                _ => None,
            })
            .collect();
        assert_eq!(panes, [B]);
    }
}
